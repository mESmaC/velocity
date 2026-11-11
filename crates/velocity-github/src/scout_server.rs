//! The scouts' MCP server (F7 Scouts): the only way a sweep files an issue.
//!
//! `scout_issues` reads every open and closed issue, so the scout can compare its finding
//! against them, and remembers that review. `scout_file` then runs the existing filing path
//! ([`IssueClient::file_scout_finding`]): the reproduction must fail, its script and output
//! are attached, and an exact duplicate or a stale review is refused. The scout's nick comes
//! from the launcher, never from the tool arguments, so the attribution can't be forged.
//! Every published outcome is appended to the sweep's report file, which Velocity reads to
//! update the Duty view's activity feed and issues-filed count.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{IssueClient, IssueReviewToken, ScoutFinding, ScoutIssueError, ScoutIssueOutcome};

/// One line of a sweep's report file: what a published `scout_file` call ended with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ScoutReport {
    Filed {
        number: u32,
        title: String,
        /// The failing script attached as evidence, relative to the scout's checkout, which
        /// Velocity posts as a `stress` event (F14, F17).
        #[serde(default)]
        evidence: String,
    },
    Duplicate {
        number: u32,
        title: String,
    },
}

/// What a filed issue's feed line says. The evidence path follows the colon, so the event
/// log can name the failing script.
pub fn filed_message(number: u32, title: &str, evidence: &str) -> String {
    if evidence.is_empty() {
        format!("Filed #{number} {title} · failing evidence attached")
    } else {
        format!("Filed #{number} {title} · failing evidence attached: {evidence}")
    }
}

/// One scout's connection: its checkout's issues, its nick and its report file.
pub struct ScoutServer {
    /// The checkout's issues, or why it has none (no GitHub remote): every tool call then
    /// answers with that reason instead of the server failing to start.
    client: Result<IssueClient, String>,
    scout: String,
    report: Option<PathBuf>,
    reviewed: Option<IssueReviewToken>,
}

impl ScoutServer {
    pub fn new(
        client: Result<IssueClient, String>,
        scout: String,
        report: Option<PathBuf>,
    ) -> Self {
        Self {
            client,
            scout,
            report,
            reviewed: None,
        }
    }

    /// Answers JSON-RPC requests, one per line, until the agent closes the connection.
    pub fn serve(&mut self, input: impl BufRead, output: impl Write) -> std::io::Result<()> {
        crate::mcp_stdio::serve(self, input, output)
    }

    fn client(&self) -> Result<&IssueClient, ScoutIssueError> {
        self.client
            .as_ref()
            .map_err(|reason| ScoutIssueError::Invalid(reason.clone()))
    }

    fn issues(&mut self) -> Result<Value, ScoutIssueError> {
        let client = self.client()?;
        let inventory = client.inventory()?;
        let labels: Vec<String> = client.label_identifiers()?.into_keys().collect();
        let issues: Vec<Value> = inventory
            .issues
            .iter()
            .map(|issue| {
                json!({
                    "number": issue.number,
                    "state": issue.state,
                    "title": issue.title,
                    "labels": issue.labels,
                    "body": issue.body,
                })
            })
            .collect();
        self.reviewed = Some(inventory.review_token);
        Ok(json!({
            "repository": inventory.repository,
            "labels": labels,
            "issues": issues,
        }))
    }

    /// The arguments are a [`ScoutFinding`] without the scout and the review, which the
    /// server supplies, plus `publish`.
    fn file(&mut self, arguments: Value) -> Result<Value, ScoutIssueError> {
        let reviewed = self.reviewed.clone().ok_or_else(|| {
            ScoutIssueError::Invalid(
                "Call scout_issues first and compare the finding with every issue".into(),
            )
        })?;
        let Value::Object(mut fields) = arguments else {
            return Err(ScoutIssueError::Invalid(
                "Arguments must be an object".into(),
            ));
        };
        let publish = fields
            .remove("publish")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        // Identity and review come from this connection, whatever the agent sent.
        fields.insert("scout".into(), Value::String(self.scout.clone()));
        fields.insert("reviewed".into(), serde_json::to_value(reviewed)?);
        let finding: ScoutFinding = serde_json::from_value(Value::Object(fields))?;
        let outcome = self
            .client()?
            .file_scout_finding(&finding, publish)
            .map_err(|error| match error {
                ScoutIssueError::StaleReview => ScoutIssueError::Invalid(
                    "Issues changed since scout_issues; call it again and re-check duplicates"
                        .into(),
                ),
                other => other,
            })?;
        if publish {
            let evidence = finding
                .reproduction
                .script
                .to_string_lossy()
                .replace('\\', "/");
            self.record(&outcome, &evidence)?;
        }
        Ok(serde_json::to_value(&outcome)?)
    }

    fn compare(&self, arguments: Value) -> Result<Value, ScoutIssueError> {
        let comparison: crate::ReferenceComparison = serde_json::from_value(arguments)?;
        let result = comparison.run(&self.client()?.directory)?;
        Ok(serde_json::to_value(result)?)
    }

    fn record(&self, outcome: &ScoutIssueOutcome, evidence: &str) -> Result<(), ScoutIssueError> {
        let report = match outcome {
            ScoutIssueOutcome::Filed { issue } => ScoutReport::Filed {
                number: issue.number,
                title: issue.title.clone(),
                evidence: evidence.to_owned(),
            },
            ScoutIssueOutcome::Duplicate { issue, .. } => ScoutReport::Duplicate {
                number: issue.number,
                title: issue.title.clone(),
            },
            ScoutIssueOutcome::Prepared { .. } => return Ok(()),
        };
        let Some(path) = &self.report else {
            return Ok(());
        };
        crate::mcp_stdio::append_report(path, &report)?;
        Ok(())
    }
}

fn comparison_tool() -> Value {
    json!({
        "name": "scout_compare",
        "description": "Run an implementation driver and a compiled Lean reference driver on identical UTF-8 stdin. Both must exit successfully; stdout is compared byte for byte. Use adapters to normalize output. On disagreement, preserve the input and write a failing script for scout_file; this tool does not file issues.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "implementation": driver_schema(),
                "reference": driver_schema(),
                "input": {"type": "string", "maxLength": 48000}
            },
            "required": ["implementation", "reference", "input"],
            "additionalProperties": false
        }
    })
}

fn driver_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "program": {"type": "string"},
            "arguments": {"type": "array", "items": {"type": "string"}},
            "script": {"type": "string", "description": "Driver path relative to the scout worktree"},
            "script_arguments": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["program", "script"],
        "additionalProperties": false
    })
}

fn tool_list() -> Vec<Value> {
    let string = json!({"type": "string"});
    let strings = json!({"type": "array", "items": {"type": "string"}});
    vec![
        comparison_tool(),
        json!({
            "name": "scout_issues",
            "description": "Every open and closed issue in this repository, with full bodies, plus the repository's labels. Call it before filing and compare each finding with every issue.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        }),
        json!({
            "name": "scout_file",
            "description": "File one finding. Velocity runs the reproduction (it must exit with a failing status), attaches the script and its output, and refuses duplicates of open or closed issues. publish false previews the issue; publish true files it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": string,
                    "description": string,
                    "labels": strings,
                    "duplicate_assessment": {"type": "string", "description": "How this differs from every similar open or closed issue."},
                    "duplicate_of": {"type": "integer", "description": "The existing issue this duplicates, if any; nothing is filed."},
                    "reproduction": driver_schema(),
                    "publish": {"type": "boolean"},
                },
                "required": ["title", "description", "labels", "duplicate_assessment", "reproduction", "publish"],
                "additionalProperties": false,
            },
        }),
    ]
}

/// Reads the report lines appended since `offset` bytes, and the offset after the last
/// complete line, so a line the server is still writing is read next time.
pub fn read_scout_reports(
    path: &std::path::Path,
    offset: u64,
) -> std::io::Result<(Vec<ScoutReport>, u64)> {
    crate::mcp_stdio::read_reports(path, offset)
}

impl crate::mcp_stdio::ToolServer for ScoutServer {
    fn name(&self) -> &'static str {
        "velocity-scout"
    }

    fn instructions(&self) -> String {
        format!(
            "You are {}. Call scout_issues before filing, then file each finding with scout_file.",
            self.scout
        )
    }

    fn tools(&self) -> Vec<Value> {
        tool_list()
    }

    fn call(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String> {
        let result = match name {
            Some("scout_issues") => self.issues(),
            Some("scout_file") => self.file(arguments),
            Some("scout_compare") => self.compare(arguments),
            _ => Err(ScoutIssueError::Invalid("Unknown scout tool".into())),
        };
        result.map_err(|error| error.to_string())
    }
}
