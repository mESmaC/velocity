//! The `workflow_run` tool (#508), served by the same stdio subprocess as `queue_run`.
//! Agents have no `gh`, so a workflow an owner allows is requested here and its result
//! arrives the way a queued command's does.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::queued_run_tool::{Request, Work, submit};

pub const WORKFLOW_RUN_TOOL: &str = "workflow_run";
/// The session reads the project's configuration from Rosen before it answers.
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(20);

const DESCRIPTION: &str = "Ask Velocity to run a GitHub Actions workflow in this project's \
    repository, since agents have no `gh`. Pass the workflow's file name, such as `ci.yml`. \
    Only workflows the project owner lists under `[ci] dispatchable` in the base's \
    rosen.toml run; any other request is refused with the reason. After a request is \
    accepted, end your turn. Velocity waits for the run and sends its conclusion, each \
    job's result, failed steps' output and the artifacts, with small text artifacts in \
    full, in your next prompt. GitHub has no API for step summaries, so a workflow uploads \
    a table it wants read as a text artifact.";

/// A workflow to run, as the tool hands it to the session that owns the queue.
#[derive(Deserialize, Serialize)]
pub(super) struct WorkflowRequest {
    pub workflow: String,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
}

/// The tool's arguments. Input values are strings to GitHub, but a model often writes a
/// boolean or a number for an input declared as one.
#[derive(Deserialize)]
struct Arguments {
    workflow: String,
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
                "workflow": {"type": "string", "description": "Workflow file under .github/workflows, e.g. ci.yml"},
                "ref": {"type": "string", "description": "Branch or tag to run at. Defaults to the repository's default branch"},
                "inputs": {"type": "object", "description": "The workflow's workflow_dispatch inputs", "additionalProperties": {"type": ["string", "number", "boolean"]}}
            },
            "required": ["workflow"]
        }
    })
}

pub fn request_workflow_run(port: u16, token: &str, arguments: Value) -> Result<Value, String> {
    let arguments: Arguments =
        serde_json::from_value(arguments).map_err(|error| error.to_string())?;
    let inputs = arguments
        .inputs
        .into_iter()
        .map(|(name, value)| input_text(&name, value).map(|text| (name, text)))
        .collect::<Result<_, _>>()?;
    let request = Request {
        token: token.to_owned(),
        work: Work::Workflow(WorkflowRequest {
            workflow: arguments.workflow,
            reference: arguments.reference.filter(|name| !name.trim().is_empty()),
            inputs,
        }),
    };
    submit::<String>(port, &request, ADMISSION_TIMEOUT)
        .map_err(|error| format!("Could not request the workflow: {error}"))?
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
