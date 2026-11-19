//! Whether an agent's request to run a GitHub workflow is allowed (#508). Agents have no
//! `gh`, so Velocity dispatches for them, and only workflows the project owner lists under
//! `[ci] dispatchable` in the base's `rosen.toml`. The list names files, so a request is
//! reduced to a file name first and compared exactly.

use std::collections::BTreeMap;
use std::fmt;

/// How a requested run's row in the thread starts. The row stays running for as long as
/// GitHub takes, and the stall check reads this to know the thread is waiting, not silent.
pub const RUN_TITLE: &str = "GitHub workflow ";
/// Where GitHub reads workflow files, which a request or a list entry may spell out.
const WORKFLOW_FOLDER: &str = ".github/workflows/";
/// GitHub refuses a `workflow_dispatch` with more inputs than this.
const MAXIMUM_INPUTS: usize = 25;
/// Longer than any branch or tag GitHub accepts.
const MAXIMUM_REFERENCE_BYTES: usize = 255;

/// Why a workflow request was not started, worded for the agent that asked.
#[derive(Debug, PartialEq, Eq)]
pub enum WorkflowRefusal {
    NotWorkflowFile(String),
    NotDispatchable {
        workflow: String,
        /// The workflow files the list does allow.
        allowed: Vec<String>,
    },
    InvalidReference(String),
    TooManyInputs,
    InvalidInput(String),
}

impl fmt::Display for WorkflowRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotWorkflowFile(name) => write!(
                formatter,
                "`{name}` is not a workflow file name. Pass the file under .github/workflows, such as `ci.yml`."
            ),
            Self::NotDispatchable { workflow, allowed } => {
                let allows = if allowed.is_empty() {
                    "allows no workflows".to_owned()
                } else {
                    format!("allows only {}", allowed.join(", "))
                };
                write!(
                    formatter,
                    "`{workflow}` is not on the owner's allowlist: `[ci] dispatchable` in the base's rosen.toml {allows}. Only the project owner changes that list."
                )
            }
            Self::InvalidReference(reference) => {
                write!(formatter, "`{reference}` is not a branch or tag name")
            }
            Self::TooManyInputs => {
                write!(
                    formatter,
                    "A workflow takes at most {MAXIMUM_INPUTS} inputs"
                )
            }
            Self::InvalidInput(name) => write!(formatter, "`{name}` is not a workflow input name"),
        }
    }
}

impl std::error::Error for WorkflowRefusal {}

/// The workflow file to dispatch when `dispatchable` allows the request.
pub fn admit(
    dispatchable: &[String],
    workflow: &str,
    reference: Option<&str>,
    inputs: &BTreeMap<String, String>,
) -> Result<String, WorkflowRefusal> {
    let file = workflow_file(workflow)
        .ok_or_else(|| WorkflowRefusal::NotWorkflowFile(workflow.trim().to_owned()))?;
    let allowed: Vec<String> = dispatchable
        .iter()
        .filter_map(|entry| workflow_file(entry))
        .collect();
    if !allowed.contains(&file) {
        return Err(WorkflowRefusal::NotDispatchable {
            workflow: file,
            allowed,
        });
    }
    if let Some(reference) = reference
        && !is_reference(reference)
    {
        return Err(WorkflowRefusal::InvalidReference(reference.to_owned()));
    }
    if inputs.len() > MAXIMUM_INPUTS {
        return Err(WorkflowRefusal::TooManyInputs);
    }
    if let Some(name) = inputs.keys().find(|name| !is_input_name(name)) {
        return Err(WorkflowRefusal::InvalidInput(name.clone()));
    }
    Ok(file)
}

/// `name` as a file directly under `.github/workflows`, or `None` when it is anything
/// else: a path elsewhere, a numeric workflow ID or a display name.
fn workflow_file(name: &str) -> Option<String> {
    let name = name.trim();
    let file = name.strip_prefix(WORKFLOW_FOLDER).unwrap_or(name);
    let named = file
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
    let extension = file.ends_with(".yml") || file.ends_with(".yaml");
    (named && extension && !file.starts_with(['.', '-'])).then(|| file.to_owned())
}

/// A branch or tag by Git's naming rules, so it is never read as an option or a range.
fn is_reference(reference: &str) -> bool {
    !reference.is_empty()
        && reference.len() <= MAXIMUM_REFERENCE_BYTES
        && !reference.starts_with(['-', '/'])
        && !reference.ends_with(['/', '.'])
        && !reference.contains("..")
        && !reference.contains("@{")
        && !reference.chars().any(|character| {
            character.is_whitespace() || character.is_control() || "~^:?*[\\".contains(character)
        })
}

fn is_input_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}
