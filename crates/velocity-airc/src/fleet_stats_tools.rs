//! `fleet_stats` read-only AIRC MCP tool: returns per-hour fleet rollups, top offenders,
//! and optional per-issue timings/tokens as JSON (#619).

use serde_json::{Value, json};

use crate::{Error, protocol};

pub(crate) const FLEET_STATS_TOOL: &str = "fleet_stats";

/// Read-only query parameters for `fleet_stats`.
pub struct FleetStatsRequest<'a> {
    pub window: Option<&'a str>,
    pub project: Option<&'a str>,
    pub issue: Option<u32>,
}

pub(crate) type FleetStatsHandler<'a> = dyn Fn(FleetStatsRequest<'_>) -> Result<Value, Error> + 'a;

pub(crate) fn call(
    arguments: &Value,
    handler: Option<&FleetStatsHandler<'_>>,
) -> Result<Value, Error> {
    let handler = handler.ok_or_else(|| Error::Invalid("fleet_stats is unavailable".into()))?;
    let window = optional_string(arguments, "window")?;
    let project = optional_string(arguments, "project")?;
    let issue = optional_u32(arguments, "issue")?;
    handler(FleetStatsRequest {
        window,
        project,
        issue,
    })
}

fn optional_string<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>, Error> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            Ok((!trimmed.is_empty()).then_some(trimmed))
        }
        Some(_) => Err(Error::Invalid(format!("{key} must be a string"))),
    }
}

fn optional_u32(arguments: &Value, key: &str) -> Result<Option<u32>, Error> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .filter(|number| *number > 0)
            .map(Some)
            .ok_or_else(|| Error::Invalid(format!("{key} must be a positive integer"))),
        Some(_) => Err(Error::Invalid(format!("{key} must be a positive integer"))),
    }
}

pub(crate) fn tool() -> Value {
    protocol::tool(
        FLEET_STATS_TOOL,
        "Read-only fleet development metrics: returns per-project hourly rollups (lands, median filed->landed and dispatched->landed times, idle slot-minutes, tokens per landed change, post-land failure and revert rates, check queue wait, retries per issue), top offenders (slowest issues, most tokens, most retries), and optionally one issue's full lifecycle record.",
        json!({
            "window": {
                "type": "string",
                "description": "Time window such as \"24h\" (default) or \"7d\" (up to \"90d\")."
            },
            "project": {
                "type": "string",
                "description": "Optional project filter: project ID, project name, repository (owner/name), or \"all\". Defaults to this session's project."
            },
            "issue": {
                "type": "integer",
                "minimum": 1,
                "description": "Optional issue number to inspect in detail alongside the window rollups."
            }
        }),
        &[],
    )
}
