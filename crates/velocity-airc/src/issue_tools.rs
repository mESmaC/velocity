use serde_json::{Value, json};

use crate::{Channel, Claim, Error, Event, protocol, read_tools};

pub(crate) const ISSUE_TOOLS: &[&str] = &[
    "issue_read",
    "issue_create",
    "issue_comment",
    "issue_ready_to_close",
    "issue_needs_owner",
];

const DEFAULT_RECENT_COMMENTS: usize = 10;
const MAX_RECENT_COMMENTS: usize = 50;
const MAX_TITLE_CHARS: usize = 256;

pub struct IssueUpdate<'a> {
    pub claim: &'a Claim,
    pub issue: u32,
    pub body: &'a str,
    pub ready: bool,
    /// The body is the reason the work needs hardware, an upstream release or an owner
    /// decision; the issue takes `needs-owner` and leaves dispatch (#506).
    pub needs_owner: bool,
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
    let body = protocol::text(arguments, "body")?.trim();
    if body.is_empty() {
        return Err(Error::Invalid("An issue comment needs a body".into()));
    }
    let ready = name == "issue_ready_to_close";
    let blockers = blocked_by(arguments)?;
    if ready && !blockers.is_empty() {
        return Err(Error::Invalid(
            "An issue that still waits on others is not ready to close".into(),
        ));
    }
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
        ready,
        needs_owner: name == "issue_needs_owner",
        blocked_by: &blockers,
    }))
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

pub(crate) fn tools() -> [Value; 5] {
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
            "File a new GitHub issue on this project's repository (for spec questions, contradictions, bugs or findings). Runs the duplicate check against open issues before filing and returns the matching issue instead of opening a duplicate. When the new issue waits on other issues, name them in blocked_by so they are linked as GitHub issue dependencies; never use needs-owner for a dependency wait.",
            json!({
                "title":{"type":"string","minLength":1,"maxLength":MAX_TITLE_CHARS,"description":"One-line issue title"},
                "body":{"type":"string","minLength":1,"description":"Issue description and evidence"},
                "labels":{"type":"array","items":{"type":"string","minLength":1},
                    "description":"Labels from this repository, such as spec-question. Optional."},
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
            "issue_needs_owner",
            "Add the needs-owner label on a GitHub issue you hold in AIRC, with the reason as the body. Use it when the work cannot proceed without hardware, an upstream release or an owner decision, then end your turn. The issue leaves dispatch until the owner removes the label. A reason that only waits on other issues is refused: use issue_comment with blocked_by for those.",
            waiting,
            &["issue", "body"],
        ),
    ]
}
