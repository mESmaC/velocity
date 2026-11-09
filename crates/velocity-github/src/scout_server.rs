//! The scouts' MCP server (F7 Scouts): the only way a sweep files an issue.
//!
//! `scout_issues` reads every open and closed issue, so the scout can compare its finding
//! against them, and remembers that review. `scout_file` then runs the existing filing path
//! ([`IssueClient::file_scout_finding`]): the reproduction must fail, its script and output
//! are attached, and an exact duplicate or a stale review is refused. The scout's nick comes
//! from the launcher, never from the tool arguments, so the attribution can't be forged.
//! Every published outcome is appended to the sweep's report file, which Velocity reads to
//! update the Duty view's activity feed and issues-filed count.

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{IssueClient, IssueReviewToken, ScoutFinding, ScoutIssueError, ScoutIssueOutcome};

/// One line of a sweep's report file: what a published `scout_file` call ended with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ScoutReport {
    Filed { number: u32, title: String },
    Duplicate { number: u32, title: String },
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
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            let response = match serde_json::from_str::<Value>(&line) {
                Ok(request) => self.respond(&request),
                Err(_) => Some(failure(Value::Null, -32700, "Parse error")),
            };
            if let Some(response) = response {
                serde_json::to_writer(&mut output, &response)?;
                writeln!(output)?;
                output.flush()?;
            }
        }
        Ok(())
    }

    fn respond(&mut self, request: &Value) -> Option<Value> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str);
        if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
            return Some(failure(id, -32600, "Invalid request"));
        }
        // A notification (no id) gets no answer.
        request.get("id")?;
        let result = match method.unwrap_or_default() {
            "initialize" => json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "velocity-scout", "version": env!("CARGO_PKG_VERSION")},
                "instructions": format!(
                    "You are {}. Call scout_issues before filing, then file each finding with scout_file.",
                    self.scout
                ),
            }),
            "ping" => json!({}),
            "tools/list" => json!({"tools": tool_list()}),
            "tools/call" => match self.call(&request["params"]) {
                Ok(value) => json!({"content": [{"type": "text", "text": value.to_string()}]}),
                Err(error) => {
                    json!({"isError": true, "content": [{"type": "text", "text": error.to_string()}]})
                }
            },
            _ => return Some(failure(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    fn call(&mut self, parameters: &Value) -> Result<Value, ScoutIssueError> {
        let arguments = parameters.get("arguments").cloned().unwrap_or(json!({}));
        match parameters.get("name").and_then(Value::as_str) {
            Some("scout_issues") => self.issues(),
            Some("scout_file") => self.file(arguments),
            _ => Err(ScoutIssueError::Invalid("Unknown scout tool".into())),
        }
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
            self.record(&outcome)?;
        }
        Ok(serde_json::to_value(&outcome)?)
    }

    fn record(&self, outcome: &ScoutIssueOutcome) -> Result<(), ScoutIssueError> {
        let report = match outcome {
            ScoutIssueOutcome::Filed { issue } => ScoutReport::Filed {
                number: issue.number,
                title: issue.title.clone(),
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
        let mut line = serde_json::to_string(&report)?;
        line.push('\n');
        // One short append per outcome; Velocity reads whole lines only.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(line.as_bytes())?;
        Ok(())
    }
}

fn tool_list() -> Vec<Value> {
    let string = json!({"type": "string"});
    let strings = json!({"type": "array", "items": {"type": "string"}});
    vec![
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
                    "reproduction": {
                        "type": "object",
                        "properties": {
                            "program": {"type": "string", "description": "The interpreter, e.g. pwsh, python, cargo."},
                            "arguments": strings,
                            "script": {"type": "string", "description": "The failing script, relative to the worktree."},
                            "script_arguments": strings,
                        },
                        "required": ["program", "script"],
                        "additionalProperties": false,
                    },
                    "publish": {"type": "boolean"},
                },
                "required": ["title", "description", "labels", "duplicate_assessment", "reproduction", "publish"],
                "additionalProperties": false,
            },
        }),
    ]
}

fn failure(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Reads the report lines appended since `offset` bytes, and the offset after the last
/// complete line, so a line the server is still writing is read next time.
pub fn read_scout_reports(
    path: &std::path::Path,
    offset: u64,
) -> std::io::Result<(Vec<ScoutReport>, u64)> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), offset));
        }
        Err(error) => return Err(error),
    };
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let unread = &bytes[start..];
    let complete = unread
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |last| last + 1);
    let reports = unread[..complete]
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        // A line this build can't read is skipped rather than stalling the feed.
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    Ok((reports, offset + complete as u64))
}
