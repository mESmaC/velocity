//! The `workflow_run` tool (#508, #591), served by the same stdio subprocess as `queue_run`.
//! On GitHub projects, dispatches owner-allowed GitHub Actions workflows. On Collapse /
//! GitHub-free projects, asks Rosen to run or verify a named check set on a platform.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::queued_run_tool::{Request, Work, submit};

pub const WORKFLOW_RUN_TOOL: &str = "workflow_run";
/// The session reads the project's configuration from Rosen before it answers.
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(20);

const DESCRIPTION: &str = "Ask Velocity to run an owner-allowed CI check in this project. \
    On a project with Issues on Collapse (or without a GitHub mirror), pass `check_set` \
    (or `workflow`) naming a configured check set from the base's rosen.toml such as \
    `pre-land` or `post-land`, and an optional `platform` such as `windows`, `macos` or \
    `linux`; Velocity runs the check set on Rosen (locally or on a connected platform checker) \
    without calling GitHub Actions. On a GitHub project, pass the workflow file name under \
    `.github/workflows` such as `ci.yml`; only workflows listed under `[ci] dispatchable` in \
    the base's rosen.toml run. After a request is accepted, end your turn; Velocity sends the \
    result, output excerpt and saved log path in your next prompt.";

/// A workflow or Rosen check-set run, as the tool hands it to the session that owns the queue.
#[derive(Deserialize, Serialize)]
pub(super) struct WorkflowRequest {
    #[serde(default)]
    pub workflow: String,
    #[serde(default)]
    pub check_set: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
}

/// The tool's arguments. Input values are strings to GitHub, but a model often writes a
/// boolean or a number for an input declared as one.
#[derive(Deserialize)]
struct Arguments {
    #[serde(default)]
    workflow: Option<String>,
    #[serde(default)]
    check_set: Option<String>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default, rename = "ref")]
    reference: Option<String>,
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
}

pub fn workflow_run_tool() -> Value {
    json!({
        "name": WORKFLOW_RUN_TOOL,
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "workflow": {"type": "string", "description": "GitHub Actions workflow file (e.g. ci.yml) or Rosen check set name (e.g. post-land)"},
                "check_set": {"type": "string", "description": "Named Rosen check set from rosen.toml (e.g. pre-land or post-land) for projects on Collapse"},
                "platform": {"type": "string", "description": "Target platform for a Rosen check set (e.g. windows, macos, linux, macos-arm64)"},
                "ref": {"type": "string", "description": "Branch, tag or commit to run at. Defaults to the default branch or base tip"},
                "inputs": {"type": "object", "description": "GitHub workflow_dispatch inputs", "additionalProperties": {"type": ["string", "number", "boolean"]}}
            }
        }
    })
}

pub fn request_workflow_run(port: u16, token: &str, arguments: Value) -> Result<Value, String> {
    let arguments: Arguments =
        serde_json::from_value(arguments).map_err(|error| error.to_string())?;
    let workflow = arguments
        .workflow
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or_default()
        .to_owned();
    let check_set = arguments
        .check_set
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    if workflow.is_empty() && check_set.is_none() {
        return Err("Provide `workflow` (GitHub Actions) or `check_set` (Rosen check set)".into());
    }
    let inputs = arguments
        .inputs
        .into_iter()
        .map(|(name, value)| input_text(&name, value).map(|text| (name, text)))
        .collect::<Result<_, _>>()?;
    let request = Request {
        token: token.to_owned(),
        work: Work::Workflow(WorkflowRequest {
            workflow,
            check_set,
            platform: arguments.platform.filter(|name| !name.trim().is_empty()),
            reference: arguments.reference.filter(|name| !name.trim().is_empty()),
            inputs,
        }),
    };
    submit::<String>(port, &request, ADMISSION_TIMEOUT)
        .map_err(|error| format!("Could not request the run: {error}"))?
        .map(|run| json!({"queued": true, "run": run, "next": "End your turn. Velocity will send the run's result when it finishes."}))
}

fn input_text(name: &str, value: Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text),
        Value::Bool(_) | Value::Number(_) => Ok(value.to_string()),
        _ => Err(format!(
            "Input `{name}` must be a string, a number or a boolean"
        )),
    }
}
