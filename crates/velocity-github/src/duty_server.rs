//! PR duty's MCP server (F7 PR duty, Rule 4): the only way a reviewing agent approves,
//! requests changes or merges.
//!
//! The launcher fixes the reviewer's nick and the one PR it holds the claim on, so an agent
//! can neither act on another agent's PR nor record a decision under another nick. Approve
//! and request changes go through Velocity's approval record ([`crate::ReviewRecord`]): each
//! is an AIRC event bound to the head the agent checked. Merge sends exactly the approved
//! head to GitHub. Every outcome is appended to the review's report file, which Velocity
//! reads to update the Duty view's activity feed.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::mcp_stdio::{ToolServer, append_report, read_reports};
use crate::{
    MergeMethod, NewReviewComment, PullRequestClient, PullRequestError, PullRequestOperation,
    ReviewAction,
};

/// One line of a review's report file: what a `pr_duty_review` or `pr_duty_merge` call did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PrDutyReport {
    Approved { number: u32, head: String },
    ChangesRequested { number: u32, head: String },
    Commented { number: u32 },
    Merged { number: u32, branch_kept: bool },
    MergeFailed { number: u32, reason: String },
}

/// One reviewing agent's connection: its PR, its merge method and its report file.
pub struct PrDutyServer {
    /// The PR client with the reviewer's approval record, or why there is none: every tool
    /// call then answers with that reason instead of the server failing to start.
    client: Result<PullRequestClient, String>,
    reviewer: String,
    number: u32,
    method: MergeMethod,
    report: Option<PathBuf>,
    statement_lock: Option<StatementLockPlace>,
}

/// Where an approval's statement lock is read and kept (F17): the review worktree, whose git
/// has the approved head, and the app data folder of the project's approval record.
struct StatementLockPlace {
    worktree: PathBuf,
    app_data: PathBuf,
    project: String,
}

#[derive(Deserialize)]
struct ReviewArguments {
    action: ReviewChoice,
    head: String,
    reason: String,
    #[serde(default)]
    comments: Vec<NewReviewComment>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReviewChoice {
    Approve,
    RequestChanges,
    Comment,
}

#[derive(Deserialize)]
struct MergeArguments {
    head: String,
}

impl PrDutyServer {
    pub fn new(
        client: Result<PullRequestClient, String>,
        reviewer: String,
        number: u32,
        method: MergeMethod,
        report: Option<PathBuf>,
    ) -> Self {
        Self {
            client,
            reviewer,
            number,
            method,
            report,
            statement_lock: None,
        }
    }

    /// Locks every theorem statement at the head each approval names (F17 statement lock).
    pub fn with_statement_lock(
        mut self,
        worktree: PathBuf,
        app_data: PathBuf,
        project: String,
    ) -> Self {
        self.statement_lock = Some(StatementLockPlace {
            worktree,
            app_data,
            project,
        });
        self
    }

    /// Answers JSON-RPC requests, one per line, until the agent closes the connection.
    pub fn serve(&mut self, input: impl BufRead, output: impl Write) -> std::io::Result<()> {
        crate::mcp_stdio::serve(self, input, output)
    }

    fn client(&self) -> Result<&PullRequestClient, PullRequestError> {
        self.client
            .as_ref()
            .map_err(|reason| PullRequestError::Invalid(reason.clone()))
    }

    fn pull_request(&self) -> Result<Value, PullRequestError> {
        Ok(serde_json::to_value(self.client()?.snapshot(self.number)?)?)
    }

    fn review(&self, arguments: Value) -> Result<Value, PullRequestError> {
        let arguments: ReviewArguments = serde_json::from_value(arguments)?;
        let action = match arguments.action {
            ReviewChoice::Approve => ReviewAction::Approve,
            ReviewChoice::RequestChanges => ReviewAction::RequestChanges,
            ReviewChoice::Comment => ReviewAction::Comment,
        };
        // Read before approving, so an approval never stands without its lock.
        let lock = match (arguments.action, &self.statement_lock) {
            (ReviewChoice::Approve, Some(place)) => Some(
                velocity_proofs::statements_at(&place.worktree, &arguments.head).map_err(
                    |error| {
                        PullRequestError::Invalid(format!(
                            "Could not lock the theorem statements at {}: {error}",
                            arguments.head
                        ))
                    },
                )?,
            ),
            _ => None,
        };
        let receipt = self.client()?.execute(PullRequestOperation::Review {
            number: self.number,
            expected_head: arguments.head.clone(),
            action,
            body: arguments.reason,
            comments: arguments.comments,
        })?;
        if let (Some(lock), Some(place)) = (lock, &self.statement_lock) {
            velocity_proofs::save_statement_lock(
                &place.app_data,
                &place.project,
                self.number,
                &lock,
            )
            .map_err(|error| {
                PullRequestError::Invalid(format!(
                    "Approved, but the statement lock could not be saved: {error}"
                ))
            })?;
        }
        let (number, head) = (self.number, arguments.head);
        self.record(&match arguments.action {
            ReviewChoice::Approve => PrDutyReport::Approved { number, head },
            ReviewChoice::RequestChanges => PrDutyReport::ChangesRequested { number, head },
            ReviewChoice::Comment => PrDutyReport::Commented { number },
        });
        Ok(serde_json::to_value(receipt)?)
    }

    fn merge(&self, arguments: Value) -> Result<Value, PullRequestError> {
        let arguments: MergeArguments = serde_json::from_value(arguments)?;
        let merged = self.client()?.execute(PullRequestOperation::Merge {
            number: self.number,
            expected_head: arguments.head,
            method: self.method,
        });
        let number = self.number;
        match merged {
            Ok(receipt) => {
                let branch_kept = receipt.operation == "merged_branch_kept";
                self.record(&PrDutyReport::Merged {
                    number,
                    branch_kept,
                });
                Ok(serde_json::to_value(receipt)?)
            }
            Err(error) => {
                let reason = error.to_string();
                self.record(&PrDutyReport::MergeFailed { number, reason });
                Err(error)
            }
        }
    }

    /// The feed is a convenience; a report that can't be written never undoes a decision.
    fn record(&self, report: &PrDutyReport) {
        if let Some(path) = &self.report {
            let _ = append_report(path, report);
        }
    }
}

impl ToolServer for PrDutyServer {
    fn name(&self) -> &'static str {
        "velocity-pr-duty"
    }

    fn instructions(&self) -> String {
        format!(
            "You are {}, holding the review claim on PR #{}. Read it with pr_duty_pull_request, record your decision with pr_duty_review, and merge an approved head with pr_duty_merge.",
            self.reviewer, self.number
        )
    }

    fn tools(&self) -> Vec<Value> {
        tool_list(self.number)
    }

    fn call(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String> {
        let result = match name {
            Some("pr_duty_pull_request") => self.pull_request(),
            Some("pr_duty_review") => self.review(arguments),
            Some("pr_duty_merge") => self.merge(arguments),
            _ => Err(PullRequestError::Invalid("Unknown PR duty tool".into())),
        };
        result.map_err(|error| error.to_string())
    }
}

fn tool_list(number: u32) -> Vec<Value> {
    let string = json!({"type": "string"});
    let head = json!({"type": "string", "description": "The full head commit SHA you checked. Velocity refuses it if the PR has moved."});
    vec![
        json!({
            "name": "pr_duty_pull_request",
            "description": format!("PR #{number} as GitHub has it now: title, description, head SHA, branches, CI checks (advisory), reviews, comments and review threads."),
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        }),
        json!({
            "name": "pr_duty_review",
            "description": format!("Record your decision on PR #{number}. approve and request_changes are recorded in Velocity's approval record (the approval Rule 4 requires) and mirrored on GitHub; comment only comments. A reason is required."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["approve", "request_changes", "comment"]},
                    "head": head,
                    "reason": string,
                    "comments": {
                        "type": "array",
                        "description": "Inline comments on the diff.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": string,
                                "line": {"type": "integer"},
                                "side": {"type": "string", "enum": ["LEFT", "RIGHT"]},
                                "body": string,
                            },
                            "required": ["path", "line", "side", "body"],
                            "additionalProperties": false,
                        },
                    },
                },
                "required": ["action", "head", "reason"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "pr_duty_merge",
            "description": format!("Merge PR #{number} at the head you approved, with the role's merge method. Refused unless Velocity's record holds a standing approval at exactly that head."),
            "inputSchema": {
                "type": "object",
                "properties": {"head": head},
                "required": ["head"],
                "additionalProperties": false,
            },
        }),
    ]
}

/// Reads the report lines appended since `offset` bytes, and the offset after the last
/// complete line.
pub fn read_pr_duty_reports(
    path: &std::path::Path,
    offset: u64,
) -> std::io::Result<(Vec<PrDutyReport>, u64)> {
    read_reports(path, offset)
}
