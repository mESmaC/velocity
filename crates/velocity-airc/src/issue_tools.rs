use serde_json::{Value, json};

use crate::{Channel, Claim, Error, protocol};

pub struct IssueUpdate<'a> {
    pub claim: &'a Claim,
    pub issue: u32,
    pub body: &'a str,
    pub ready: bool,
}

pub(crate) type IssueHandler<'a> = dyn Fn(IssueUpdate<'_>) -> Result<String, Error> + 'a;

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
        ready: name == "issue_ready_to_close",
    })?;
    Ok(json!({"url": url}))
}

pub(crate) fn tools() -> [Value; 2] {
    let fields =
        json!({"issue":{"type":"integer","minimum":1},"body":{"type":"string","minLength":1}});
    [
        protocol::tool(
            "issue_comment",
            "Post a progress comment on a GitHub issue you hold in AIRC. Use it when starting, when a finding changes the plan, or when blocked.",
            fields.clone(),
            &["issue", "body"],
        ),
        protocol::tool(
            "issue_ready_to_close",
            "Post your completion evidence and add the ready-to-close label on a GitHub issue you hold in AIRC. This leaves the issue open for independent follow-through.",
            fields,
            &["issue", "body"],
        ),
    ]
}
