use serde_json::{Value, json};

use crate::{Channel, Claim, Error, Event, protocol, read_tools};

pub(crate) const ISSUE_TOOLS: &[&str] = &[
    "issue_read",
    "issue_create",
    "issue_comment",
    "issue_ready_to_close",
    "issue_blocked_upstream",
    "issue_hold",
    "issue_close_not_planned",
];

const DEFAULT_RECENT_COMMENTS: usize = 10;
const MAX_RECENT_COMMENTS: usize = 50;
const MAX_TITLE_CHARS: usize = 256;

/// Which update operation an AIRC issue tool requested (#526).
pub enum IssueUpdateKind<'a> {
    Comment,
    ReadyToClose,
    /// Mark the issue `blocked-upstream` waiting for a PR or release checked daily.
    BlockedUpstream {
        upstream: &'a str,
    },
    /// Record a visible person hold on the Work view with the action that releases it.
    Hold {
        release_action: &'a str,
    },
    /// Close an orphaned issue as `not_planned` citing the commit that removed its spec section.
    CloseNotPlanned {
        spec_section: &'a str,
        removed_commit: &'a str,
    },
}

pub struct IssueUpdate<'a> {
    pub claim: &'a Claim,
    pub issue: u32,
    pub body: &'a str,
    pub kind: IssueUpdateKind<'a>,
    /// The issues this one waits on, as `#12` or `owner/name#12`. They become GitHub
    /// issue dependencies, which hold the issue until they close (#511).
    pub blocked_by: &'a [String],
}

/// Request to read one issue from the project's mirror repository (#535).
pub struct IssueRead {
    pub issue: u32,
    pub offset: usize,
    pub comments: usize,
}

/// Request to file a new issue on the project's mirror repository (#535).
pub struct IssueCreate<'a> {
    pub member: &'a str,
    pub nick: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub labels: &'a [String],
    pub blocked_by: &'a [String],
}

/// All GitHub issue operations handled by the AIRC MCP server.
pub enum IssueRequest<'a> {
    Read(IssueRead),
    Create(IssueCreate<'a>),
    Update(IssueUpdate<'a>),
}

pub(crate) type IssueHandler<'a> = dyn Fn(IssueRequest<'_>) -> Result<Value, Error> + 'a;

/// A tool's optional `blocked_by` list. A bare number is read as that issue.
pub(crate) fn blocked_by(arguments: &Value) -> Result<Vec<String>, Error> {
    let invalid = || Error::Invalid("blocked_by must be a list of issues such as #12".into());
    match arguments.get("blocked_by") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(names)) => names
            .iter()
            .map(|name| match name {
                Value::String(name) => Ok(name.clone()),
                Value::Number(number) => Ok(number.to_string()),
                _ => Err(invalid()),
            })
            .collect(),
        Some(_) => Err(invalid()),
    }
}

pub(crate) fn call(
    channel: &Channel,
    member: &str,
    nick: &str,
    name: &str,
    arguments: &Value,
    handler: Option<&IssueHandler<'_>>,
) -> Result<Value, Error> {
    let handler = handler.ok_or_else(|| Error::Invalid("No GitHub issue transport".into()))?;
    match name {
        "issue_read" => call_read(arguments, handler),
        "issue_create" => call_create(channel, member, nick, arguments, handler),
        _ => call_update(channel, member, name, arguments, handler),
    }
}

fn call_read(arguments: &Value, handler: &IssueHandler<'_>) -> Result<Value, Error> {
    let issue = positive_issue(arguments)?;
    let offset = read_tools::counter(arguments, "offset")?;
    let comments = match arguments.get("comments") {
        None | Some(Value::Null) => DEFAULT_RECENT_COMMENTS,
        Some(_) => {
            let count = read_tools::counter(arguments, "comments")?;
            if count > MAX_RECENT_COMMENTS {
                return Err(Error::Invalid(format!(
                    "comments must be between 0 and {MAX_RECENT_COMMENTS}"
                )));
            }
            count
        }
    };
    handler(IssueRequest::Read(IssueRead {
        issue,
        offset,
        comments,
    }))
}

fn call_create(
    channel: &Channel,
    member: &str,
    nick: &str,
    arguments: &Value,
    handler: &IssueHandler<'_>,
) -> Result<Value, Error> {
    let title = protocol::text(arguments, "title")?.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS || title.contains(['\r', '\n']) {
        return Err(Error::Invalid(
            "title must be one nonempty line, at most 256 characters".into(),
        ));
    }
    let body = protocol::text(arguments, "body")?.trim();
    if body.is_empty() {
        return Err(Error::Invalid("An issue needs a body".into()));
    }
    let labels = labels(arguments)?;
    let blockers = blocked_by(arguments)?;
    let answer = handler(IssueRequest::Create(IssueCreate {
        member,
        nick,
        title,
        body,
        labels: &labels,
        blocked_by: &blockers,
    }))?;
    if answer.get("outcome").and_then(Value::as_str) == Some("filed")
        && let Some(number) = answer
            .get("number")
            .and_then(Value::as_u64)
            .and_then(|number| u32::try_from(number).ok())
    {
        let filed_title = answer.get("title").and_then(Value::as_str).unwrap_or(title);
        if let Ok(event) = Event::issue_filed(nick, number, filed_title) {
            let _ = channel.post_event(event);
        }
    }
    Ok(answer)
}

fn call_update(
    channel: &Channel,
    member: &str,
    name: &str,
    arguments: &Value,
    handler: &IssueHandler<'_>,
) -> Result<Value, Error> {
    let issue = positive_issue(arguments)?;
    let body_key = if name == "issue_hold" {
        "reason"
    } else {
        "body"
    };
    let body = protocol::text(arguments, body_key)?.trim();
    if body.is_empty() {
        return Err(Error::Invalid(format!(
            "An issue update needs a nonempty {body_key}"
        )));
    }
    let blockers = blocked_by(arguments)?;
    let kind = parse_update_kind(name, arguments, &blockers)?;
    let key = format!("issue:{issue}");
    let claim = channel
        .claims()?
        .into_iter()
        .find(|claim| claim.member == member && claim.key() == key)
        .ok_or_else(|| Error::Invalid(format!("You do not hold the claim on issue #{issue}")))?;
    handler(IssueRequest::Update(IssueUpdate {
        claim: &claim,
        issue,
        body,
        kind,
        blocked_by: &blockers,
    }))
}

fn parse_update_kind<'a>(
    name: &str,
    arguments: &'a Value,
    blockers: &[String],
) -> Result<IssueUpdateKind<'a>, Error> {
    match name {
        "issue_ready_to_close" => {
            if !blockers.is_empty() {
                return Err(Error::Invalid(
                    "An issue that still waits on others is not ready to close".into(),
                ));
            }
            Ok(IssueUpdateKind::ReadyToClose)
        }
        "issue_blocked_upstream" => Ok(IssueUpdateKind::BlockedUpstream {
            upstream: protocol::text(arguments, "upstream")?.trim(),
        }),
        "issue_hold" => Ok(IssueUpdateKind::Hold {
            release_action: protocol::text(arguments, "release_action")?.trim(),
        }),
        "issue_close_not_planned" => {
            if !blockers.is_empty() {
                return Err(Error::Invalid(
                    "An orphaned spec issue being closed waits on no other issue".into(),
                ));
            }
            Ok(IssueUpdateKind::CloseNotPlanned {
                spec_section: protocol::text(arguments, "spec_section")?.trim(),
                removed_commit: protocol::text(arguments, "removed_commit")?.trim(),
            })
        }
        _ => Ok(IssueUpdateKind::Comment),
    }
}

fn positive_issue(arguments: &Value) -> Result<u32, Error> {
    arguments["issue"]
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .filter(|number| *number > 0)
        .ok_or_else(|| Error::Invalid("issue must be a positive issue number".into()))
}

fn labels(arguments: &Value) -> Result<Vec<String>, Error> {
    let invalid = || Error::Invalid("labels must be a list of nonempty label names".into());
    match arguments.get("labels") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(invalid)
            })
            .collect(),
        Some(_) => Err(invalid()),
    }
}

pub(crate) fn tools() -> [Value; 7] {
    let blockers = json!({"type":"array","items":{"type":"string"},
        "description":"Issues this one waits on, as #12 for this repository or owner/name#12 for another. They are linked as GitHub issue dependencies."});
    let fields =
        json!({"issue":{"type":"integer","minimum":1},"body":{"type":"string","minLength":1}});
    let mut waiting = fields.clone();
    waiting["blocked_by"] = blockers.clone();
    [
        protocol::tool(
            "issue_read",
            "Read a GitHub issue from this project's repository: its title, body, labels, state, blocked_by dependencies and recent comments. Pass next_offset as offset to read the rest of a long body.",
            json!({
                "issue":{"type":"integer","minimum":1,"description":"GitHub issue number in this project's repository"},
                "offset":{"type":"integer","minimum":0,"description":"Byte offset from next_offset to continue reading a long issue body"},
                "comments":{"type":"integer","minimum":0,"maximum":MAX_RECENT_COMMENTS,"default":DEFAULT_RECENT_COMMENTS,
                    "description":"How many most recent comments to include (0 to omit comments)"}
            }),
            &["issue"],
        ),
        protocol::tool(
            "issue_create",
            "File a new GitHub issue on this project's repository (for spec questions, contradictions, bugs or findings). Runs the duplicate check against open issues before filing and returns the matching issue instead of opening a duplicate. When the new issue waits on other issues, name them in blocked_by so they are linked as GitHub issue dependencies. Retired silent labels (needs-owner, on-hold) are refused; use tracking, blocked-upstream or blocked_by instead.",
            json!({
                "title":{"type":"string","minLength":1,"maxLength":MAX_TITLE_CHARS,"description":"One-line issue title"},
                "body":{"type":"string","minLength":1,"description":"Issue description and evidence"},
                "labels":{"type":"array","items":{"type":"string","minLength":1},
                    "description":"Labels from this repository, such as spec-question, tracking or blocked-upstream. Optional."},
                "blocked_by":blockers
            }),
            &["title", "body"],
        ),
        protocol::tool(
            "issue_comment",
            "Post a progress comment on a GitHub issue you hold in AIRC. Use it when starting, when a finding changes the plan, or when blocked. When the issue waits on other issues, name them in blocked_by and end your turn: they are recorded as GitHub issue dependencies and dispatch starts the issue again when they close. Never hold an issue with a label for that.",
            waiting.clone(),
            &["issue", "body"],
        ),
        protocol::tool(
            "issue_ready_to_close",
            "Post your completion evidence and add the ready-to-close label on a GitHub issue you hold in AIRC. This leaves the issue open for independent follow-through.",
            fields,
            &["issue", "body"],
        ),
        protocol::tool(
            "issue_blocked_upstream",
            "Mark a GitHub issue you hold in AIRC as blocked-upstream, recording the upstream PR or release it waits for (e.g. owner/repo#123, owner/repo@v1.2.3, or owner/repo#123 -> release:owner/repo). Continuous dispatch checks it daily and returns the issue to work automatically when it ships, then end your turn.",
            json!({
                "issue":{"type":"integer","minimum":1},
                "upstream":{"type":"string","minLength":1,"description":"Upstream PR or release target: owner/repo#123, owner/repo@tag, or owner/repo#123 -> release:owner2/repo2"},
                "body":{"type":"string","minLength":1,"description":"Why this issue waits on the upstream PR or release"},
                "blocked_by":blockers
            }),
            &["issue", "upstream", "body"],
        ),
        protocol::tool(
            "issue_hold",
            "Place a visible person hold on a GitHub issue you hold in AIRC ONLY when a real person action is required (physical hardware or an explicit scope hold). Never use for other issues (use blocked_by), upstream PRs/releases (use issue_blocked_upstream), or decisions covered by spec defaults (SPEC §1 Rule 1). The hold shows as one line on the project's Work view with its release action.",
            json!({
                "issue":{"type":"integer","minimum":1},
                "reason":{"type":"string","minLength":1,"description":"Concise reason a person is required (e.g. physical hardware or scope hold)"},
                "release_action":{"type":"string","minLength":1,"description":"The concrete action that releases this hold on the Work view"},
                "blocked_by":blockers
            }),
            &["issue", "reason", "release_action"],
        ),
        protocol::tool(
            "issue_close_not_planned",
            "Close a GitHub issue you hold in AIRC as not planned when the spec section it refers to has been removed, citing the commit SHA that removed the section. End your turn afterwards.",
            json!({
                "issue":{"type":"integer","minimum":1},
                "spec_section":{"type":"string","minLength":1,"description":"Name of the spec section that was removed"},
                "removed_commit":{"type":"string","minLength":7,"maxLength":40,"description":"Commit SHA that removed the spec section"},
                "body":{"type":"string","minLength":1,"description":"Verification showing the section was removed and the issue is orphaned"}
            }),
            &["issue", "spec_section", "removed_commit", "body"],
        ),
    ]
}
