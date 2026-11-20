use serde_json::{Value, json};

use crate::{Channel, Claim, Error, protocol};

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

pub(crate) type IssueHandler<'a> = dyn Fn(IssueUpdate<'_>) -> Result<String, Error> + 'a;

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
    name: &str,
    arguments: &Value,
    handler: Option<&IssueHandler<'_>>,
) -> Result<Value, Error> {
    let handler = handler.ok_or_else(|| Error::Invalid("No GitHub issue transport".into()))?;
    let issue = arguments["issue"]
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .filter(|number| *number > 0)
        .ok_or_else(|| Error::Invalid("issue must be a positive issue number".into()))?;
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
    let url = handler(IssueUpdate {
        claim: &claim,
        issue,
        body,
        ready,
        needs_owner: name == "issue_needs_owner",
        blocked_by: &blockers,
    })?;
    Ok(json!({"url": url}))
}

pub(crate) fn tools() -> [Value; 3] {
    let fields =
        json!({"issue":{"type":"integer","minimum":1},"body":{"type":"string","minLength":1}});
    let mut waiting = fields.clone();
    waiting["blocked_by"] = json!({"type":"array","items":{"type":"string"},
        "description":"Issues this one waits on, as #12 for this repository or owner/name#12 for another. They are linked as GitHub issue dependencies."});
    [
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
