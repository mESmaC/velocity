//! Post-land verification state read from Collapse (Collapse C12, collapse.sh#186; #580).
//!
//! Reads the repository's `green` checkpoint and recorded post-land failures from Collapse's
//! REST endpoint when available, falling back to Collapse's read-only MCP endpoint
//! (`https://api.collapse.sh/mcp`) when REST returns 404. Only `green` post-land checkpoints
//! count as green; build-only (`built`) checkpoints are ignored.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use reqwest::{Client, Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::credential::CollapseCredentials;
use crate::provision::{authorized, failed, success};
use crate::url_text::encode;
use crate::{CollapseError, MCP_URL};

const READ_TIMEOUT: Duration = Duration::from_secs(20);
const SHORT_COMMIT: usize = 7;

/// Post-land verification state Collapse reports for one repository (Collapse C12, #580).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryPostLand {
    /// Newest commit covered by Rosen's `green` post-land checkpoint, if any.
    pub green_commit: Option<String>,
    /// UTC RFC 3339 timestamp when `green_commit` was recorded, if Collapse reported it.
    pub green_at: Option<String>,
    /// Individual `green` events reported by Collapse (`lowercase commit -> UTC RFC 3339`).
    pub green_events: BTreeMap<String, String>,
    /// Commits with a recorded post-land failure, active bisection failure or revert.
    pub failed_commits: BTreeSet<String>,
}

/// Reads Rosen's post-land checkpoint and recorded failures for `owner/name` from Collapse.
/// Returns `Ok(None)` when Collapse reports the repository or its post-land endpoint absent.
pub async fn repository_post_land(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<Option<RepositoryPostLand>, CollapseError> {
    let client = Client::builder()
        .timeout(READ_TIMEOUT)
        .build()
        .map_err(failed)?;
    let encoded_owner = encode(owner);
    let encoded_name = encode(name);
    for path in [
        format!("/api/repositories/{encoded_owner}/{encoded_name}/post-land"),
        format!("/api/repositories/{encoded_owner}/{encoded_name}/rosen/post-land"),
    ] {
        if let Some(state) = read_rest_post_land(&client, credentials, &path).await? {
            return Ok(Some(state));
        }
    }
    read_mcp_post_land(&client, credentials, owner, name).await
}

async fn read_rest_post_land(
    client: &Client,
    credentials: &CollapseCredentials,
    path: &str,
) -> Result<Option<RepositoryPostLand>, CollapseError> {
    let response = authorized(client, Method::GET, path, &credentials.mcp_token)
        .send()
        .await
        .map_err(failed)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body: Value = success(response, "read post-land state")
        .await?
        .json()
        .await
        .map_err(failed)?;
    Ok(Some(parse_post_land_value(&body)))
}

async fn read_mcp_post_land(
    client: &Client,
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<Option<RepositoryPostLand>, CollapseError> {
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "velocity", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    let (session, init_reply) = mcp_request(client, credentials, None, &init).await?;
    if init_reply.is_none() {
        return Ok(None);
    }
    let list_request = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} });
    let (_, Some(list_reply)) =
        mcp_request(client, credentials, session.as_deref(), &list_request).await?
    else {
        return Ok(None);
    };
    let Some(tool) = select_post_land_tool(&list_reply) else {
        return Ok(None);
    };
    let call_request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": tool.name,
            "arguments": tool_arguments(&tool.properties, owner, name)
        }
    });
    let (_, Some(call_reply)) =
        mcp_request(client, credentials, session.as_deref(), &call_request).await?
    else {
        return Ok(None);
    };
    Ok(extract_mcp_result(&call_reply).map(|value| parse_post_land_value(&value)))
}

async fn mcp_request(
    client: &Client,
    credentials: &CollapseCredentials,
    session: Option<&str>,
    body: &Value,
) -> Result<(Option<String>, Option<Value>), CollapseError> {
    let mut request = client
        .post(MCP_URL)
        .bearer_auth(&credentials.mcp_token)
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .json(body);
    if let Some(session_id) = session {
        request = request.header("Mcp-Session-Id", session_id);
    }
    let response = request.send().await.map_err(failed)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok((None, None));
    }
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let response = success(response, "read post-land state through Collapse MCP").await?;
    let text = response.text().await.map_err(failed)?;
    Ok((session_id, parse_mcp_envelope(&text)))
}

struct PostLandTool {
    name: String,
    properties: Vec<String>,
}

fn select_post_land_tool(list_reply: &Value) -> Option<PostLandTool> {
    let tools = list_reply.get("result")?.get("tools")?.as_array()?;
    let tool = tools.iter().find(|tool| {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            return false;
        };
        let lowered = name.to_ascii_lowercase();
        (lowered.contains("post_land") || lowered.contains("post-land"))
            && !lowered.contains("submit")
            && !lowered.contains("trigger")
            && !lowered.contains("retry")
    })?;
    let name = tool.get("name")?.as_str()?.to_owned();
    let properties = tool
        .get("inputSchema")
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    Some(PostLandTool { name, properties })
}

fn tool_arguments(properties: &[String], owner: &str, name: &str) -> Value {
    let mut args = Map::new();
    let full = format!("{owner}/{name}");
    for key in properties {
        match key.as_str() {
            "owner" | "namespace" => {
                args.insert(key.clone(), Value::String(owner.to_owned()));
            }
            "repo" | "name" => {
                args.insert(key.clone(), Value::String(name.to_owned()));
            }
            "repository" => {
                let value = if properties.iter().any(|field| field == "owner") {
                    name.to_owned()
                } else {
                    full.clone()
                };
                args.insert(key.clone(), Value::String(value));
            }
            _ => {}
        }
    }
    if args.is_empty() {
        args.insert("owner".into(), Value::String(owner.to_owned()));
        args.insert("repo".into(), Value::String(name.to_owned()));
    }
    Value::Object(args)
}

fn parse_mcp_envelope(body: &str) -> Option<Value> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(value);
    }
    trimmed.lines().rev().find_map(|line| {
        let data = line.trim().strip_prefix("data:")?.trim();
        (!data.is_empty()).then(|| serde_json::from_str::<Value>(data).ok())?
    })
}

fn extract_mcp_result(reply: &Value) -> Option<Value> {
    let result = reply.get("result")?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if let Some(structured) = result.get("structuredContent")
        && !structured.is_null()
    {
        return Some(structured.clone());
    }
    result.get("content")?.as_array()?.iter().find_map(|item| {
        let text = item.get("text")?.as_str()?.trim();
        serde_json::from_str::<Value>(text).ok()
    })
}

/// Parses Collapse's post-land JSON payload into [`RepositoryPostLand`].
/// Handles both REST and MCP shapes while ignoring build-only (`built`) checkpoints.
pub fn parse_post_land_value(value: &Value) -> RepositoryPostLand {
    let root = value
        .get("post_land")
        .or_else(|| value.get("state"))
        .filter(|nested| nested.is_object())
        .unwrap_or(value);
    let mut state = RepositoryPostLand::default();
    let Some(map) = root.as_object() else {
        return state;
    };
    read_green_checkpoint(map, &mut state);
    read_failures(map, &mut state);
    read_events(map, &mut state);
    state
}

fn read_green_checkpoint(map: &Map<String, Value>, state: &mut RepositoryPostLand) {
    let checkpoint = ["last_green", "green", "post_land_green"]
        .into_iter()
        .find_map(|key| map.get(key))
        .or_else(|| map.get("checkpoints").and_then(|value| value.get("green")));
    let fallback_time = first_timestamp(map, &["last_green_at", "green_at"]);
    if let Some(checkpoint) = checkpoint {
        let (commit, time) = parse_checkpoint(checkpoint);
        if let Some(commit) = commit {
            state.green_commit = Some(commit);
            state.green_at = time.or(fallback_time);
        }
    }
}

fn parse_checkpoint(value: &Value) -> (Option<String>, Option<String>) {
    match value {
        Value::String(text) => (commit_sha(text), None),
        Value::Object(map) => (
            first_commit(map, &["commit", "commit_id", "sha"]),
            first_timestamp(
                map,
                &["at", "time", "recorded_at", "created_at", "timestamp"],
            ),
        ),
        _ => (None, None),
    }
}

fn read_failures(map: &Map<String, Value>, state: &mut RepositoryPostLand) {
    for key in [
        "failures",
        "post_land_failures",
        "culprits",
        "reverts",
        "auto_reverts",
    ] {
        let Some(items) = map.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            if let Some(commit) = failure_commit(item) {
                state.failed_commits.insert(commit);
            }
        }
    }
    if let Some(culprit) = map.get("culprit").and_then(failure_commit) {
        state.failed_commits.insert(culprit);
    }
    for key in ["bisection", "active_bisection"] {
        if let Some(bisection) = map.get(key) {
            record_bisection_failure(bisection, state);
        }
    }
    if let Some(bisections) = map.get("bisections").and_then(Value::as_array) {
        for bisection in bisections {
            record_bisection_failure(bisection, state);
        }
    }
}

fn record_bisection_failure(value: &Value, state: &mut RepositoryPostLand) {
    let Some(map) = value.as_object() else {
        return;
    };
    let done = map
        .get("phase")
        .and_then(Value::as_str)
        .is_some_and(|phase| phase.eq_ignore_ascii_case("done"));
    if !done && let Some(commit) = first_commit(map, &["failed", "commit", "commit_id"]) {
        state.failed_commits.insert(commit);
    }
}

fn failure_commit(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => commit_sha(text),
        Value::Object(map) => first_commit(map, &["culprit", "commit", "commit_id", "sha"]),
        _ => None,
    }
}

fn read_events(map: &Map<String, Value>, state: &mut RepositoryPostLand) {
    let Some(events) = map.get("events").and_then(Value::as_array) else {
        return;
    };
    for event in events {
        let Some(event_map) = event.as_object() else {
            continue;
        };
        let Some(kind) = event_map
            .get("kind")
            .or_else(|| event_map.get("type"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let detail = event_map.get("detail").and_then(Value::as_object);
        match kind.to_ascii_lowercase().as_str() {
            "green" => record_green_event(event_map, detail, state),
            "post_land_failure" | "reverted" => {
                let commit =
                    first_commit(event_map, &["culprit", "commit", "commit_id"]).or_else(|| {
                        detail.and_then(|detail| {
                            first_commit(detail, &["culprit", "commit", "commit_id"])
                        })
                    });
                if let Some(commit) = commit {
                    state.failed_commits.insert(commit);
                }
            }
            _ => {}
        }
    }
}

fn record_green_event(
    event_map: &Map<String, Value>,
    detail: Option<&Map<String, Value>>,
    state: &mut RepositoryPostLand,
) {
    let commit = first_commit(event_map, &["commit", "commit_id", "sha"]).or_else(|| {
        detail.and_then(|detail| first_commit(detail, &["commit", "commit_id", "sha"]))
    });
    let time = first_timestamp(
        event_map,
        &["at", "time", "recorded_at", "created_at", "timestamp"],
    )
    .or_else(|| {
        detail.and_then(|detail| {
            first_timestamp(
                detail,
                &["at", "time", "recorded_at", "created_at", "timestamp"],
            )
        })
    });
    let Some(commit) = commit else {
        return;
    };
    if let Some(time) = time.clone() {
        state.green_events.entry(commit.clone()).or_insert(time);
    }
    if state.green_commit.is_none() {
        state.green_commit = Some(commit);
        state.green_at = time;
    }
}

fn first_commit(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_str).and_then(commit_sha))
}

fn commit_sha(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (trimmed.len() >= SHORT_COMMIT
        && trimmed.len() <= 64
        && trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then(|| trimmed.to_ascii_lowercase())
}

fn first_timestamp(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(parse_timestamp))
}

fn parse_timestamp(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        }
        Value::Number(number) => {
            let raw = number.as_u64()?;
            let seconds = if raw >= 10_000_000_000 {
                raw / 1_000
            } else {
                raw
            };
            Some(format_utc_seconds(seconds))
        }
        _ => None,
    }
}

fn format_utc_seconds(unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let remaining = unix_seconds % 86_400;
    let hour = remaining / 3_600;
    let minute = (remaining % 3_600) / 60;
    let second = remaining % 60;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year_base = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = if month <= 2 { year_base + 1 } else { year_base };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#580): Collapse's post-land read must recognize `green` checkpoints and failures
    // across REST and MCP shapes while never treating build-only (`built`) checkpoints as green.
    #[test]
    fn parses_collapse_post_land_checkpoints_failures_and_mcp_envelopes()
    -> Result<(), Box<dyn std::error::Error>> {
        let green = "a".repeat(40);
        let built = "b".repeat(40);
        let culprit = "c".repeat(40);
        let bisect = "d".repeat(40);
        let payload = json!({
            "last_built": { "commit": built, "at": "2026-11-21T19:45:00Z" },
            "last_green": { "commit": green, "at": "2026-11-21T19:40:00Z" },
            "failures": [{ "commit": culprit, "kind": "test" }],
            "active_bisection": { "phase": "search", "failed": bisect },
            "events": [
                { "kind": "built", "detail": { "commit": built }, "created_at": 1795290300000u64 },
                { "kind": "green", "detail": { "commit": green }, "created_at": 1795290000000u64 }
            ]
        });
        let parsed = parse_post_land_value(&payload);
        assert_eq!(parsed.green_commit.as_deref(), Some(green.as_str()));
        assert_eq!(parsed.green_at.as_deref(), Some("2026-11-21T19:40:00Z"));
        assert!(!parsed.green_events.contains_key(&built));
        assert!(parsed.failed_commits.contains(&culprit));
        assert!(parsed.failed_commits.contains(&bisect));

        let built_only = json!({ "last_built": { "commit": built, "at": "2026-11-21T19:45:00Z" } });
        assert_eq!(parse_post_land_value(&built_only).green_commit, None);

        let sse = format!(
            "event: message\ndata: {}\n\n",
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {
                    "content": [{ "type": "text", "text": payload.to_string() }]
                }
            })
        );
        let envelope = parse_mcp_envelope(&sse).ok_or("expected SSE JSON envelope")?;
        let extracted = extract_mcp_result(&envelope).ok_or("expected MCP tool result")?;
        assert_eq!(
            parse_post_land_value(&extracted).green_commit.as_deref(),
            Some(green.as_str())
        );

        let list = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "tools": [
                    {
                        "name": "get_post_land",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "owner": {}, "name": {} }
                        }
                    }
                ]
            }
        });
        let tool = select_post_land_tool(&list).ok_or("expected post-land tool")?;
        assert_eq!(tool.name, "get_post_land");
        assert_eq!(
            tool_arguments(&tool.properties, "mESmaC", "rosen"),
            json!({ "owner": "mESmaC", "name": "rosen" })
        );
        Ok(())
    }
}
