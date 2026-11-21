//! `next_work` MCP tool: atomically claim the first unclaimed, unblocked item with its brief (#539).

use serde_json::{Value, json};

use crate::{Channel, Error, protocol};

pub(crate) const NEXT_WORK_TOOL: &str = "next_work";

/// Request from a self-selecting thread to claim the next available work item.
pub struct NextWorkRequest<'a> {
    pub member: &'a str,
    pub nick: &'a str,
    pub sources: &'a [String],
    pub branch: Option<&'a str>,
    pub files: &'a [String],
}

pub(crate) type NextWorkHandler<'a> =
    dyn Fn(&Channel, NextWorkRequest<'_>) -> Result<Value, Error> + 'a;

pub(crate) fn call(
    channel: &Channel,
    member: &str,
    nick: &str,
    arguments: &Value,
    handler: Option<&NextWorkHandler<'_>>,
) -> Result<Value, Error> {
    let handler = handler.ok_or_else(|| Error::Invalid("next_work is unavailable".into()))?;
    let sources = string_list(arguments, "sources")?;
    let files = string_list(arguments, "files")?;
    let branch = match arguments.get("branch") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.trim()),
        Some(_) => return Err(Error::Invalid("branch must be a nonempty string".into())),
    };
    handler(
        channel,
        NextWorkRequest {
            member,
            nick,
            sources: &sources,
            branch,
            files: &files,
        },
    )
}

fn string_list(arguments: &Value, key: &str) -> Result<Vec<String>, Error> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        Error::Invalid(format!("{key} must be a list of nonempty strings"))
                    })
            })
            .collect(),
        Some(_) => Err(Error::Invalid(format!("{key} must be an array of strings"))),
    }
}

pub(crate) fn tool() -> Value {
    protocol::tool(
        NEXT_WORK_TOOL,
        "Atomically claim the first unclaimed, unblocked work item (user tasks, actionable GitHub issues in critical-path and milestone order, unchecked §7 items, and agent tasks) and return its task brief with acceptance criteria, spec and §8 links, nearby open changes, crate discoveries and failures, and ownership.",
        json!({
            "sources": {
                "type": "array",
                "items": { "type": "string", "enum": ["tasks", "issues", "spec"] },
                "description": "Optional work sources to consider; defaults to all (tasks, issues, spec)."
            },
            "branch": {
                "type": "string",
                "minLength": 1,
                "description": "Optional Rosen change ID or branch name recorded on the claim; defaults to ROSEN_CHANGE or this thread's branch."
            },
            "files": {
                "type": "array",
                "items": { "type": "string", "minLength": 1 },
                "description": "Optional initial repository-relative file paths to claim alongside the piece."
            }
        }),
        &[],
    )
}
