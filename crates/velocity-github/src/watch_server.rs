//! The Main watcher's MCP server (F7 Main watcher): the only way a watch records its verdict.
//!
//! The launcher fixes the watcher's nick and the tip it watches. `watch_broken` checks that
//! the merge it names really is on the base branch at that tip, then posts the `broken` AIRC
//! event bound to that commit, so delivery (F14) tells only the agents whose branches contain
//! it. `watch_pull_request` checks that the fix or revert really is an open PR against the
//! base branch before it records it. Every outcome is appended to the watch's report file,
//! which Velocity reads to update the Duty view's activity feed.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use velocity_airc::{Channel, Event};

use crate::mcp_stdio::{ToolServer, append_report, read_reports};
use crate::{PullRequestClient, PullRequestError, PullRequestState};

/// The longest failing-output excerpt a `broken` event carries; an event is one line.
const OUTPUT_EXCERPT: usize = 200;

/// One line of a watch's report file: a verdict the watcher recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WatchReport {
    Green {
        commit: String,
    },
    Broken {
        commit: String,
        pull_request: Option<u32>,
        command: String,
    },
    Repair {
        number: u32,
        revert: bool,
        commit: String,
    },
}

/// Where one watch runs and what it watches.
pub struct WatchTarget {
    pub worktree: PathBuf,
    pub watcher: String,
    pub base_branch: String,
    pub tip: String,
}

impl WatchTarget {
    pub fn new(worktree: PathBuf, watcher: String, base_branch: String, tip: String) -> Self {
        Self {
            worktree,
            watcher,
            base_branch,
            tip,
        }
    }
}

/// One watch's connection: its checkout, the project's AIRC channel and its report file.
pub struct WatchServer {
    target: WatchTarget,
    /// The project's channel, or why it can't be opened: every tool call then answers with
    /// that reason instead of the server failing to start.
    channel: Result<Channel, String>,
    pull_requests: Result<PullRequestClient, String>,
    report: Option<PathBuf>,
}

#[derive(Deserialize)]
struct BrokenArguments {
    commit: String,
    #[serde(default)]
    pull_request: Option<u32>,
    command: String,
    output: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RepairKind {
    Fix,
    Revert,
}

#[derive(Deserialize)]
struct RepairArguments {
    number: u32,
    kind: RepairKind,
    commit: String,
}

impl WatchServer {
    pub fn new(
        target: WatchTarget,
        channel: Result<Channel, String>,
        pull_requests: Result<PullRequestClient, String>,
        report: Option<PathBuf>,
    ) -> Self {
        Self {
            target,
            channel,
            pull_requests,
            report,
        }
    }

    /// Answers JSON-RPC requests, one per line, until the agent closes the connection.
    pub fn serve(&mut self, input: impl BufRead, output: impl Write) -> std::io::Result<()> {
        crate::mcp_stdio::serve(self, input, output)
    }

    fn channel(&self) -> Result<&Channel, PullRequestError> {
        self.channel
            .as_ref()
            .map_err(|reason| PullRequestError::Invalid(reason.clone()))
    }

    fn pull_requests(&self) -> Result<&PullRequestClient, PullRequestError> {
        self.pull_requests
            .as_ref()
            .map_err(|reason| PullRequestError::Invalid(reason.clone()))
    }

    fn green(&self) -> Result<Value, PullRequestError> {
        let commit = self.target.tip.clone();
        self.record(&WatchReport::Green {
            commit: commit.clone(),
        });
        Ok(json!({"green": commit}))
    }

    fn broken(&self, arguments: Value) -> Result<Value, PullRequestError> {
        let arguments: BrokenArguments = serde_json::from_value(arguments)?;
        let command = one_line(&arguments.command, OUTPUT_EXCERPT);
        if command.is_empty() {
            return Err(PullRequestError::Invalid(
                "Name the command that fails".into(),
            ));
        }
        let commit = self.watched_commit(&arguments.commit)?;
        let pull_request = self.merged_pull_request(arguments.pull_request, &commit)?;
        let merge = match pull_request {
            Some(number) => format!("{} (#{number})", short(&commit)),
            None => short(&commit).to_owned(),
        };
        let text = format!(
            "{} broken by {merge} · `{command}` fails: {}",
            self.target.base_branch,
            one_line(&arguments.output, OUTPUT_EXCERPT)
        );
        let mut event = Event::new("broken", &self.target.watcher, &text);
        if let Some(number) = pull_request {
            event = event.for_pull_request(number, Some(&commit));
        }
        let entry = self.channel()?.post_event(event.at_commit(&commit))?;
        self.record(&WatchReport::Broken {
            commit: commit.clone(),
            pull_request,
            command,
        });
        Ok(json!({"broken": commit, "pull_request": pull_request, "event": entry.sequence}))
    }

    /// The merged PR `number` names, checked on GitHub. A project with no GitHub repository
    /// (a local project, F2) has no PRs to check: the event is bound to the commit alone.
    fn merged_pull_request(
        &self,
        number: Option<u32>,
        commit: &str,
    ) -> Result<Option<u32>, PullRequestError> {
        let (Some(number), Ok(client)) = (number, self.pull_requests.as_ref()) else {
            return Ok(None);
        };
        if client.get(number)?.state != PullRequestState::Merged {
            return Err(PullRequestError::Invalid(format!(
                "#{number} is not merged; name the PR that merged {}",
                short(commit)
            )));
        }
        Ok(Some(number))
    }

    fn repair(&self, arguments: Value) -> Result<Value, PullRequestError> {
        let arguments: RepairArguments = serde_json::from_value(arguments)?;
        let commit = self.watched_commit(&arguments.commit)?;
        let number = arguments.number;
        let pull_request = self.pull_requests()?.get(number)?;
        let base = &self.target.base_branch;
        // The locked rule: a fix or revert reaches the base branch only through a PR.
        if pull_request.state != PullRequestState::Open || pull_request.base_ref_name != *base {
            return Err(PullRequestError::Invalid(format!(
                "#{number} is not an open PR against `{base}`; open one with `gh pr create --base {base}`"
            )));
        }
        let revert = matches!(arguments.kind, RepairKind::Revert);
        let (tag, kind) = if revert {
            ("reverted", "revert")
        } else {
            ("PR", "fix")
        };
        let text = format!("opened {kind} PR #{number} for {}", short(&commit));
        let event = Event::new(tag, &self.target.watcher, &text)
            .for_pull_request(number, Some(&pull_request.head_ref_oid));
        self.channel()?.post_event(event)?;
        self.record(&WatchReport::Repair {
            number,
            revert,
            commit,
        });
        Ok(json!({"recorded": number}))
    }

    /// The full SHA `commit` names, when it is on the base branch at the watched tip.
    fn watched_commit(&self, commit: &str) -> Result<String, PullRequestError> {
        let commit = commit.trim();
        let well_formed =
            (7..=64).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !well_formed {
            return Err(PullRequestError::Invalid(format!(
                "`{commit}` is not a commit SHA"
            )));
        }
        let output = crate::command::git_in(&self.target.worktree)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{commit}^{{commit}}"))
            .output()?;
        let full = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || full.is_empty() {
            return Err(PullRequestError::Invalid(format!(
                "{commit} is not a commit in this checkout"
            )));
        }
        let on_tip = crate::command::git_in(&self.target.worktree)
            .args(["merge-base", "--is-ancestor", &full, &self.target.tip])
            .status()?;
        if !on_tip.success() {
            return Err(PullRequestError::Invalid(format!(
                "{} is not on `{}` at {}",
                short(&full),
                self.target.base_branch,
                short(&self.target.tip)
            )));
        }
        Ok(full)
    }

    /// The feed is a convenience; a report that can't be written never undoes an event.
    fn record(&self, report: &WatchReport) {
        if let Some(path) = &self.report {
            let _ = append_report(path, report);
        }
    }
}

impl ToolServer for WatchServer {
    fn name(&self) -> &'static str {
        "velocity-watch"
    }

    fn instructions(&self) -> String {
        format!(
            "You are {}, the main watcher, watching `{}` at {}. Record a green tip with watch_green, a break with watch_broken, and the fix or revert PR you open with watch_pull_request.",
            self.target.watcher,
            self.target.base_branch,
            short(&self.target.tip)
        )
    }

    fn tools(&self) -> Vec<Value> {
        tool_list(&self.target.base_branch)
    }

    fn call(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String> {
        let result = match name {
            Some("watch_green") => self.green(),
            Some("watch_broken") => self.broken(arguments),
            Some("watch_pull_request") => self.repair(arguments),
            _ => Err(PullRequestError::Invalid("Unknown watch tool".into())),
        };
        result.map_err(|error| error.to_string())
    }
}

fn tool_list(base: &str) -> Vec<Value> {
    let string = json!({"type": "string"});
    let commit = json!({"type": "string", "description": format!("The breaking merge's commit SHA on `{base}`.")});
    let empty = json!({"type": "object", "properties": {}, "additionalProperties": false});
    vec![
        json!({
            "name": "watch_green",
            "description": format!("Record that every test, stress script and check passed on `{base}` at the watched tip."),
            "inputSchema": empty,
        }),
        json!({
            "name": "watch_broken",
            "description": format!("Record the merge that broke `{base}`. Velocity checks it is on `{base}`, posts the `broken` AIRC event and tells only the agents whose branches contain it."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "commit": commit,
                    "pull_request": {"type": "integer", "description": "The merged PR that commit came from, when there is one."},
                    "command": string,
                    "output": {"type": "string", "description": "The failing command's output, most telling part first."},
                },
                "required": ["commit", "command", "output"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "watch_pull_request",
            "description": format!("Record the fix or revert PR you opened against `{base}` for a broken merge. It must be open; PR duty reviews and merges it."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "number": {"type": "integer"},
                    "kind": {"type": "string", "enum": ["fix", "revert"]},
                    "commit": commit,
                },
                "required": ["number", "kind", "commit"],
                "additionalProperties": false,
            },
        }),
    ]
}

/// `text` as one line of at most `limit` characters.
fn one_line(text: &str, limit: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= limit {
        return joined;
    }
    let mut clipped: String = joined.chars().take(limit - 1).collect();
    clipped.push('…');
    clipped
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// Reads the report lines appended since `offset` bytes, and the offset after the last
/// complete line.
pub fn read_watch_reports(
    path: &std::path::Path,
    offset: u64,
) -> std::io::Result<(Vec<WatchReport>, u64)> {
    read_reports(path, offset)
}
