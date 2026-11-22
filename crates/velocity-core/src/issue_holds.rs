//! Machine-checkable issue holds replacing `needs-owner` (#526):
//! - `tracking`: never dispatched; closes automatically when its checklist finishes.
//! - `blocked-upstream`: records the upstream PR or release it waits for; checked daily.
//! - Visible person holds (`hold`, plus any legacy hold labels until released): always
//!   shown as one line on the project's Work view with the action that releases them.
//!
//! Pure and synchronous; `velocity-github` runs the GitHub requests.

use serde::{Deserialize, Serialize};

use crate::issue_dependencies::{
    IssueReference, dependency_hold_refusal, dependency_only_hold, is_repository,
};

pub const TRACKING_LABEL: &str = "tracking";
pub const BLOCKED_UPSTREAM_LABEL: &str = "blocked-upstream";
pub const HOLD_LABEL: &str = "hold";

/// Labels whose only effect was making continuous dispatch silently skip an issue (#526).
/// Agents are refused from adding them; any existing issue still carrying one is surfaced
/// visibly in the Work view's holds strip with a Release action.
pub const RETIRED_HOLD_LABELS: &[&str] =
    &["needs-owner", "on-hold", "needs-decision", "owner-decision"];

const UPSTREAM_MARKER_PREFIX: &str = "<!-- velocity-blocked-upstream:";
const UPSTREAM_LINE_PREFIX: &str = "Blocked upstream:";
const PERSON_HOLD_MARKER_PREFIX: &str = "<!-- velocity-person-hold:";
const MARKER_SUFFIX: &str = " -->";

/// Phrases that indicate an approval/decision hold violating SPEC §1 Rule 1.
const RULE1_DECISION_PHRASES: &[&str] = &[
    "owner decision",
    "needs approval",
    "waiting for approval",
    "awaiting approval",
    "approve this",
    "decide whether",
    "needs your input",
];

/// Grounds for a genuine person hold (hardware or explicit scope hold).
const PERSON_HOLD_GROUNDS: &[&str] = &[
    "hardware",
    "physical",
    "machine",
    "device",
    "scope hold",
    "phase ",
    "manual ",
    "credential",
    "account",
];

pub fn is_retired_hold_label(label: &str) -> bool {
    let trimmed = label.trim();
    RETIRED_HOLD_LABELS
        .iter()
        .any(|retired| trimmed.eq_ignore_ascii_case(retired))
}

pub fn is_tracking_label(label: &str) -> bool {
    label.trim().eq_ignore_ascii_case(TRACKING_LABEL)
}

pub fn is_blocked_upstream_label(label: &str) -> bool {
    label.trim().eq_ignore_ascii_case(BLOCKED_UPSTREAM_LABEL)
}

pub fn is_person_hold_label(label: &str) -> bool {
    let trimmed = label.trim();
    trimmed.eq_ignore_ascii_case(HOLD_LABEL) || is_retired_hold_label(trimmed)
}

pub fn has_label(labels: &[String], expected: &str) -> bool {
    labels
        .iter()
        .any(|label| label.trim().eq_ignore_ascii_case(expected))
}

/// Refuses retired silent skip-only labels when filing or updating issues (#526).
pub fn refuse_retired_label(label: &str) -> Result<(), String> {
    if !is_retired_hold_label(label) {
        return Ok(());
    }
    Err(format!(
        "Label {label:?} is retired (#526). Use a machine-checkable state instead: \
         `tracking` for checklist tracking issues, `blocked-upstream` with an upstream PR or \
         release reference (checked daily), `blocked_by` for GitHub issue dependencies, \
         `issue_close_not_planned` when the spec section was removed, or `issue_hold` with \
         an explicit reason and release_action when a person action (hardware or scope hold) is required."
    ))
}

/// Markdown task-list checklist counts for a `tracking` issue (#526).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistProgress {
    pub checked: u32,
    pub unchecked: u32,
}

impl ChecklistProgress {
    pub fn total(&self) -> u32 {
        self.checked.saturating_add(self.unchecked)
    }

    /// Complete only when at least one checkbox exists and every checkbox is ticked.
    pub fn is_complete(&self) -> bool {
        self.checked > 0 && self.unchecked == 0
    }
}

/// Counts GitHub-flavoured task list items (`- [ ]`, `- [x]`, `* [X]`) outside code fences
/// and HTML comments.
pub fn parse_checklist(body: &str) -> ChecklistProgress {
    let mut progress = ChecklistProgress::default();
    let mut in_fence = false;
    let mut in_comment = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if in_comment {
            if trimmed.contains("-->") {
                in_comment = false;
            }
            continue;
        }
        if trimmed.starts_with("<!--") {
            if !trimmed.contains("-->") {
                in_comment = true;
            }
            continue;
        }
        if let Some(checked) = checklist_item_state(trimmed) {
            if checked {
                progress.checked = progress.checked.saturating_add(1);
            } else {
                progress.unchecked = progress.unchecked.saturating_add(1);
            }
        }
    }
    progress
}

fn checklist_item_state(trimmed: &str) -> Option<bool> {
    let rest = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))?;
    let rest = rest.trim_start();
    let (checked, after) = if let Some(after) = rest.strip_prefix("[ ]") {
        (false, after)
    } else {
        let after = rest
            .strip_prefix("[x]")
            .or_else(|| rest.strip_prefix("[X]"))?;
        (true, after)
    };
    (after.is_empty() || after.starts_with(char::is_whitespace)).then_some(checked)
}

/// Machine-checkable upstream PR or release an issue waits on (#526).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpstreamTarget {
    /// An upstream GitHub PR (unblocks when merged) or issue (unblocks when closed).
    PullRequest { repository: String, number: u32 },
    /// A specific upstream GitHub release tag (unblocks when published as non-draft).
    Release { repository: String, tag: String },
    /// An upstream PR plus a non-draft release in `release_repository` published at or
    /// after the PR merged (e.g. `tauri-apps/wry#1862 -> release:tauri-apps/tauri`).
    PullRequestRelease {
        repository: String,
        number: u32,
        release_repository: String,
    },
}

impl UpstreamTarget {
    /// Parses an explicit upstream target expression:
    /// - `owner/repo#123` or `https://github.com/owner/repo/pull/123`
    /// - `owner/repo@v1.2.3` or `https://github.com/owner/repo/releases/tag/v1.2.3`
    /// - `owner/repo#123 -> release:owner2/repo2` or `owner/repo#123 -> release`
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input
            .trim()
            .trim_matches(|character: char| matches!(character, '`' | '"' | '\'' | '(' | ')'));
        if trimmed.is_empty() {
            return None;
        }
        if let Some((left, right)) = trimmed.split_once("->") {
            return parse_pr_release(left.trim(), right.trim());
        }
        if let Some(release) = parse_release_url(trimmed).or_else(|| parse_release_at(trimmed)) {
            return Some(release);
        }
        parse_pr_ref(trimmed)
    }

    pub fn canonical(&self) -> String {
        match self {
            Self::PullRequest { repository, number } => format!("{repository}#{number}"),
            Self::Release { repository, tag } => format!("{repository}@{tag}"),
            Self::PullRequestRelease {
                repository,
                number,
                release_repository,
            } => format!("{repository}#{number} -> release:{release_repository}"),
        }
    }

    pub fn display_summary(&self) -> String {
        match self {
            Self::PullRequest { repository, number } => format!("{repository}#{number}"),
            Self::Release { repository, tag } => format!("{repository} release {tag}"),
            Self::PullRequestRelease {
                repository,
                number,
                release_repository,
            } => {
                if release_repository.eq_ignore_ascii_case(repository) {
                    format!("{repository}#{number} (release)")
                } else {
                    format!("{repository}#{number} in {release_repository} release")
                }
            }
        }
    }

    pub fn marker(&self) -> String {
        format!(
            "{UPSTREAM_MARKER_PREFIX}{}{MARKER_SUFFIX}",
            self.canonical()
        )
    }

    /// Direct GitHub URL for the upstream PR, issue, or release (#617).
    pub fn url(&self) -> String {
        match self {
            Self::PullRequest { repository, number }
            | Self::PullRequestRelease {
                repository, number, ..
            } => format!("https://github.com/{repository}/issues/{number}"),
            Self::Release { repository, tag } => {
                format!("https://github.com/{repository}/releases/tag/{tag}")
            }
        }
    }
}

fn parse_pr_release(left: &str, right: &str) -> Option<UpstreamTarget> {
    let UpstreamTarget::PullRequest { repository, number } = parse_pr_ref(left)? else {
        return None;
    };
    let rest = right
        .strip_prefix("release:")
        .or_else(|| right.strip_prefix("release"))?
        .trim();
    let release_repository = if rest.is_empty() {
        repository.clone()
    } else if is_repository(rest) {
        rest.to_owned()
    } else {
        return None;
    };
    Some(UpstreamTarget::PullRequestRelease {
        repository,
        number,
        release_repository,
    })
}

fn parse_release_url(text: &str) -> Option<UpstreamTarget> {
    let (_, after) = text.split_once("github.com/")?;
    let mut parts = after.split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if parts.next()? != "releases" || parts.next()? != "tag" {
        return None;
    }
    let tag = parts.next()?.split(['?', '#', ')']).next()?.trim();
    let repository = format!("{owner}/{name}");
    (is_repository(&repository) && valid_release_tag(tag)).then_some(UpstreamTarget::Release {
        repository,
        tag: tag.to_owned(),
    })
}

fn parse_release_at(text: &str) -> Option<UpstreamTarget> {
    let (repository, tag) = text.split_once('@')?;
    let repository = repository.trim();
    let tag = tag.trim().trim_end_matches(['.', ',', ';', ')']);
    (is_repository(repository) && valid_release_tag(tag)).then_some(UpstreamTarget::Release {
        repository: repository.to_owned(),
        tag: tag.to_owned(),
    })
}

fn parse_pr_ref(text: &str) -> Option<UpstreamTarget> {
    if let Some((_, after)) = text.split_once("github.com/") {
        let mut parts = after.split('/');
        let owner = parts.next()?;
        let name = parts.next()?;
        let kind = parts.next()?;
        if kind != "pull" && kind != "issues" {
            return None;
        }
        let number_part = parts.next()?;
        let digits = number_part.bytes().take_while(u8::is_ascii_digit).count();
        let number: u32 = number_part[..digits].parse().ok()?;
        let repository = format!("{owner}/{name}");
        return (number > 0 && is_repository(&repository))
            .then_some(UpstreamTarget::PullRequest { repository, number });
    }
    let reference = IssueReference::parse(text)?;
    if !reference.names_repository() {
        return None;
    }
    Some(UpstreamTarget::PullRequest {
        repository: reference.repository_or("").to_owned(),
        number: reference.number(),
    })
}

fn valid_release_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 128
        && !tag
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '/' | '?' | '#'))
}

/// Finds the latest machine-checkable upstream target in `text`.
/// Priority: explicit marker > `Blocked upstream:` line > cross-repo GitHub URL or `owner/repo#n`.
pub fn parse_upstream_target(text: &str, home_repository: Option<&str>) -> Option<UpstreamTarget> {
    let mut from_marker = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(spec) = trimmed
            .strip_prefix(UPSTREAM_MARKER_PREFIX)
            .and_then(|rest| rest.strip_suffix(MARKER_SUFFIX))
            && let Some(target) = UpstreamTarget::parse(spec)
        {
            from_marker = Some(target);
        }
    }
    if from_marker.is_some() {
        return from_marker;
    }
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = strip_prefix_ignore_ascii_case(trimmed, UPSTREAM_LINE_PREFIX)
            && let Some(target) = parse_upstream_in_line(rest, home_repository)
        {
            return Some(target);
        }
    }
    parse_upstream_in_text(text, home_repository)
}

fn parse_upstream_in_line(line: &str, home_repository: Option<&str>) -> Option<UpstreamTarget> {
    if let Some(target) = UpstreamTarget::parse(line) {
        return filter_upstream_home(target, home_repository);
    }
    parse_upstream_in_text(line, home_repository)
}

fn parse_upstream_in_text(text: &str, home_repository: Option<&str>) -> Option<UpstreamTarget> {
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|character: char| {
            matches!(
                character,
                '(' | ')' | '[' | ']' | '<' | '>' | '`' | '"' | '\'' | ',' | '.' | ';' | ':'
            )
        });
        if let Some(release) = parse_release_url(cleaned).or_else(|| parse_release_at(cleaned))
            && let Some(filtered) = filter_upstream_home(release, home_repository)
        {
            return Some(filtered);
        }
    }
    let mentions_release = text.to_ascii_lowercase().contains("release");
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|character: char| {
            matches!(
                character,
                '(' | ')' | '[' | ']' | '<' | '>' | '`' | '"' | '\'' | ',' | '.' | ';' | ':'
            )
        });
        if let Some(UpstreamTarget::PullRequest { repository, number }) = parse_pr_ref(cleaned) {
            if home_repository.is_some_and(|home| home.eq_ignore_ascii_case(&repository)) {
                continue;
            }
            return Some(if mentions_release {
                UpstreamTarget::PullRequestRelease {
                    release_repository: repository.clone(),
                    repository,
                    number,
                }
            } else {
                UpstreamTarget::PullRequest { repository, number }
            });
        }
    }
    None
}

fn filter_upstream_home(
    target: UpstreamTarget,
    home_repository: Option<&str>,
) -> Option<UpstreamTarget> {
    let Some(home) = home_repository else {
        return Some(target);
    };
    let same = match &target {
        UpstreamTarget::PullRequest { repository, .. }
        | UpstreamTarget::Release { repository, .. }
        | UpstreamTarget::PullRequestRelease { repository, .. } => {
            repository.eq_ignore_ascii_case(home)
        }
    };
    (!same).then_some(target)
}

/// Appends or updates the machine-readable `Blocked upstream:` footer in an issue body.
pub fn upsert_upstream_block(body: &str, target: &UpstreamTarget) -> String {
    let cleaned = strip_upstream_block(body);
    let block = format!(
        "{UPSTREAM_LINE_PREFIX} `{}`\n{}",
        target.canonical(),
        target.marker()
    );
    if cleaned.is_empty() {
        block
    } else {
        format!("{cleaned}\n\n{block}")
    }
}

/// Removes any Velocity-managed `Blocked upstream:` footer from an issue body.
pub fn strip_upstream_block(body: &str) -> String {
    let lines: Vec<&str> = body
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.starts_with(UPSTREAM_MARKER_PREFIX) && trimmed.ends_with(MARKER_SUFFIX))
                && strip_prefix_ignore_ascii_case(trimmed, UPSTREAM_LINE_PREFIX).is_none()
        })
        .collect();
    lines.join("\n").trim_end().to_owned()
}

/// A hold that genuinely needs a person (hardware or explicit scope hold), shown as one
/// line on the project's Work view with the action that releases it (#526).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonHold {
    pub reason: String,
    pub release_action: String,
}

impl PersonHold {
    pub fn marker(&self) -> String {
        format!(
            "{PERSON_HOLD_MARKER_PREFIX}{}|{}{MARKER_SUFFIX}",
            encode_marker_field(&self.reason),
            encode_marker_field(&self.release_action),
        )
    }

    fn from_marker_payload(payload: &str) -> Option<Self> {
        let (reason, release_action) = payload.split_once('|')?;
        let reason = decode_marker_field(reason)?;
        let release_action = decode_marker_field(release_action)?;
        (!reason.trim().is_empty() && !release_action.trim().is_empty()).then_some(Self {
            reason,
            release_action,
        })
    }
}

fn encode_marker_field(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if matches!(byte, b'%' | b'|' | b'-' | b'>' | b'\r' | b'\n') {
            encoded.push_str(&format!("%{byte:02X}"));
        } else {
            encoded.push(byte as char);
        }
    }
    encoded
}

fn decode_marker_field(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            let value = u8::from_str_radix(hex, 16).ok()?;
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Validates a new person hold before writing it to GitHub (#526).
pub fn validate_person_hold(reason: &str, release_action: &str) -> Result<PersonHold, String> {
    let reason = collapse_whitespace(reason);
    let release_action = collapse_whitespace(release_action);
    if reason.is_empty() {
        return Err("A hold needs a concise reason explaining why a person is required".into());
    }
    if release_action.is_empty() {
        return Err(
            "A hold must state release_action: the concrete action that releases it on the Work view"
                .into(),
        );
    }
    let blockers = dependency_only_hold(&reason);
    if !blockers.is_empty() {
        return Err(dependency_hold_refusal(&blockers));
    }
    let lower = reason.to_ascii_lowercase();
    if (lower.contains("upstream") || lower.contains("release"))
        && parse_upstream_target(&reason, None).is_some()
    {
        return Err(
            "Use `issue_blocked_upstream` (label `blocked-upstream`) for an upstream PR or \
             release wait so dispatch checks it daily and unblocks the issue automatically."
                .into(),
        );
    }
    if RULE1_DECISION_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
        && !PERSON_HOLD_GROUNDS
            .iter()
            .any(|ground| lower.contains(ground))
    {
        return Err(
            "SPEC §1 Rule 1 forbids waiting on owner decisions or approval prompts. Decide using \
             the spec's defaults, or file a `spec-question` issue with your recommendation. \
             Reserve holds for physical hardware or an explicit scope hold."
                .into(),
        );
    }
    Ok(PersonHold {
        reason,
        release_action,
    })
}

/// Extracts a visible person hold when the issue carries `hold` or a retired hold label.
/// Always returns `Some` for any held issue so no hold ever hides behind a label (#526).
pub fn parse_person_hold(body: &str, labels: &[String]) -> Option<PersonHold> {
    let explicit = parse_explicit_person_hold(body);
    let retired = labels
        .iter()
        .find(|label| is_retired_hold_label(label))
        .map(String::as_str);
    let held = has_label(labels, HOLD_LABEL) || retired.is_some();
    if !held {
        return None;
    }
    if let Some(hold) = explicit {
        return Some(hold);
    }
    let fallback_reason = first_prose_line(body).unwrap_or_else(|| match retired {
        Some(label) => format!("Held by retired `{label}` label with no reason recorded"),
        None => "Held for person action".into(),
    });
    Some(PersonHold {
        reason: fallback_reason,
        release_action: "Release hold to return to dispatch".into(),
    })
}

/// Parses an explicit Velocity person-hold marker or `Hold:` / `Release when:` lines.
pub fn parse_explicit_person_hold(text: &str) -> Option<PersonHold> {
    let mut from_marker = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(payload) = trimmed
            .strip_prefix(PERSON_HOLD_MARKER_PREFIX)
            .and_then(|rest| rest.strip_suffix(MARKER_SUFFIX))
            && let Some(hold) = PersonHold::from_marker_payload(payload)
        {
            from_marker = Some(hold);
        }
    }
    if from_marker.is_some() {
        return from_marker;
    }
    let mut reason = None;
    let mut release = None;
    for line in text.lines() {
        let plain = line.trim().replace("**", "");
        if let Some(value) = strip_prefix_ignore_ascii_case(&plain, "Hold:")
            .or_else(|| strip_prefix_ignore_ascii_case(&plain, "Hold reason:"))
        {
            let value = collapse_whitespace(value);
            if !value.is_empty() {
                reason = Some(value);
            }
        } else if let Some(value) = strip_prefix_ignore_ascii_case(&plain, "Release when:")
            .or_else(|| strip_prefix_ignore_ascii_case(&plain, "Release action:"))
            .or_else(|| strip_prefix_ignore_ascii_case(&plain, "Lift when:"))
        {
            let value = collapse_whitespace(value);
            if !value.is_empty() {
                release = Some(value);
            }
        }
    }
    match (reason, release) {
        (Some(reason), Some(release_action)) => Some(PersonHold {
            reason,
            release_action,
        }),
        _ => None,
    }
}

/// Appends or replaces the machine-readable person-hold block in an issue body.
pub fn upsert_person_hold_block(body: &str, hold: &PersonHold) -> String {
    let cleaned = strip_person_hold_block(body);
    let block = format!(
        "Hold: {}\nRelease when: {}\n{}",
        hold.reason,
        hold.release_action,
        hold.marker()
    );
    if cleaned.is_empty() {
        block
    } else {
        format!("{cleaned}\n\n{block}")
    }
}

/// Strips Velocity's person-hold footer block from an issue body when releasing a hold.
pub fn strip_person_hold_block(body: &str) -> String {
    let lines: Vec<&str> = body
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            let plain = trimmed.replace("**", "");
            !(trimmed.starts_with(PERSON_HOLD_MARKER_PREFIX) && trimmed.ends_with(MARKER_SUFFIX))
                && strip_prefix_ignore_ascii_case(&plain, "Hold:").is_none()
                && strip_prefix_ignore_ascii_case(&plain, "Release when:").is_none()
        })
        .collect();
    lines.join("\n").trim_end().to_owned()
}

/// Whether a legacy retired-hold issue (such as `needs-owner`) is actually a `tracking`
/// checklist issue that should be migrated to `tracking` (#617).
pub fn is_legacy_tracking_issue(title: &str, body: &str, labels: &[String]) -> bool {
    if has_label(labels, TRACKING_LABEL)
        || has_label(labels, BLOCKED_UPSTREAM_LABEL)
        || has_label(labels, HOLD_LABEL)
        || !labels.iter().any(|label| is_retired_hold_label(label))
    {
        return false;
    }
    let starts_tracking = title
        .trim_start()
        .get(..9)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("tracking:"));
    starts_tracking && parse_checklist(body).total() > 0
}

/// Detects an upstream PR/release wait on a legacy retired-hold issue (such as `needs-owner`)
/// from its body or comments when the text explicitly names an upstream wait (#617).
pub fn legacy_upstream_target(
    body: &str,
    comments: &[String],
    labels: &[String],
    home_repository: Option<&str>,
) -> Option<UpstreamTarget> {
    if has_label(labels, TRACKING_LABEL)
        || has_label(labels, BLOCKED_UPSTREAM_LABEL)
        || has_label(labels, HOLD_LABEL)
        || !labels.iter().any(|label| is_retired_hold_label(label))
    {
        return None;
    }
    for text in comments.iter().rev().map(String::as_str).chain([body]) {
        if !text.to_ascii_lowercase().contains("upstream") {
            continue;
        }
        if let Some(target) = parse_upstream_target(text, home_repository) {
            return Some(target);
        }
    }
    None
}

/// Validates the commit SHA cited when closing an orphaned spec issue as not planned (#526).
pub fn validate_commit_ref(commit: &str) -> Result<String, String> {
    let trimmed = commit.trim();
    if !(7..=40).contains(&trimmed.len()) || !trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(
            "removed_commit must be the 7 to 40 hex character commit SHA that removed the spec section"
                .into(),
        );
    }
    Ok(trimmed.to_ascii_lowercase())
}

fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| text[prefix.len()..].trim())
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn first_prose_line(body: &str) -> Option<String> {
    for line in body.lines() {
        let plain = line.trim().replace("**", "");
        if plain.is_empty()
            || plain.starts_with('#')
            || plain.starts_with("<!--")
            || plain.starts_with("```")
        {
            continue;
        }
        let condensed = collapse_whitespace(&plain);
        if !condensed.is_empty() {
            return Some(truncate_chars(&condensed, 140));
        }
    }
    None
}

fn truncate_chars(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", text[..end].trim_end()),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#526): checklist completion, upstream target parsing and visible hold validation
    // govern whether continuous dispatch closes, unblocks or surfaces held issues.
    #[test]
    fn checklist_counts_items_outside_fences_and_comments() {
        let body = "\
Tracking issue:\n\
- [x] First step\n\
* [X] Second step\n\
```markdown\n\
- [ ] Ignored in fence\n\
```\n\
<!-- - [ ] Ignored in comment -->\n";
        let done = parse_checklist(body);
        assert_eq!(done.checked, 2);
        assert_eq!(done.unchecked, 0);
        assert!(done.is_complete());

        let pending = parse_checklist("- [x] Done\n- [ ] Still open\n");
        assert!(!pending.is_complete());
        assert_eq!(pending.total(), 2);

        assert!(!parse_checklist("No checkboxes here").is_complete());
    }

    #[test]
    fn upstream_targets_parse_pr_release_and_pr_in_release() {
        assert_eq!(
            UpstreamTarget::parse("tauri-apps/wry#1862"),
            Some(UpstreamTarget::PullRequest {
                repository: "tauri-apps/wry".into(),
                number: 1862,
            })
        );
        assert_eq!(
            UpstreamTarget::parse("tauri-apps/tauri@tauri-v2.5.0"),
            Some(UpstreamTarget::Release {
                repository: "tauri-apps/tauri".into(),
                tag: "tauri-v2.5.0".into(),
            })
        );
        assert_eq!(
            UpstreamTarget::parse("tauri-apps/wry#1862 -> release:tauri-apps/tauri"),
            Some(UpstreamTarget::PullRequestRelease {
                repository: "tauri-apps/wry".into(),
                number: 1862,
                release_repository: "tauri-apps/tauri".into(),
            })
        );
        let fallback = parse_upstream_target(
            "Remove the label when a Tauri release ships the callback (tauri-apps/wry#1862).",
            Some("mESmaC/velocity"),
        );
        assert_eq!(
            fallback,
            Some(UpstreamTarget::PullRequestRelease {
                repository: "tauri-apps/wry".into(),
                number: 1862,
                release_repository: "tauri-apps/wry".into(),
            })
        );
    }

    #[test]
    fn person_hold_validation_blocks_silent_dead_ends() {
        assert!(refuse_retired_label("needs-owner").is_err());
        assert!(refuse_retired_label("on-hold").is_err());
        assert!(refuse_retired_label("tracking").is_ok());

        assert!(validate_person_hold("Waiting on #42", "Release when #42 closes").is_err());
        assert!(
            validate_person_hold(
                "Waiting for upstream release tauri-apps/wry#1862",
                "Release when Tauri ships"
            )
            .is_err()
        );
        assert!(validate_person_hold("Needs owner decision on colour", "Owner decides").is_err());
        assert!(
            validate_person_hold(
                "Needs three physical Windows 11 machines for §5 stress run",
                "Run §5 stress test on 3 machines and release"
            )
            .is_ok()
        );
        assert!(
            validate_person_hold(
                "Phase B scope hold on Watch and Fork buttons",
                "Lift Phase B scope hold"
            )
            .is_ok()
        );
    }

    #[test]
    fn person_hold_block_roundtrips_and_strips_cleanly() {
        let hold = PersonHold {
            reason: "Needs 3 Windows 11 machines".into(),
            release_action: "Run stress suite and release".into(),
        };
        let body = upsert_person_hold_block("Original description.", &hold);
        assert_eq!(parse_person_hold(&body, &["hold".into()]), Some(hold));
        assert_eq!(strip_person_hold_block(&body), "Original description.");
    }

    #[test]
    fn legacy_hold_migration_detects_tracking_and_blocked_upstream_issues() {
        let needs_owner = vec!["needs-owner".to_owned()];
        assert!(is_legacy_tracking_issue(
            "Tracking: Rosen-first overhaul",
            "## Issues\n- [ ] #414 Reference UI\n- [x] #419 Migration\n",
            &needs_owner,
        ));
        assert!(!is_legacy_tracking_issue(
            "Regular issue needing owner",
            "- [ ] Task 1\n",
            &needs_owner,
        ));

        let comments = vec![
            "Held for an upstream change: Wry's callback is still an open PR (tauri-apps/wry#1862). Remove the label when a Tauri release ships.".to_owned(),
        ];
        let upstream = legacy_upstream_target(
            "Expose CoreWebView2 ProcessFailed through safe Tauri bindings.",
            &comments,
            &needs_owner,
            Some("mESmaC/velocity"),
        );
        assert_eq!(
            upstream.as_ref().map(UpstreamTarget::url).as_deref(),
            Some("https://github.com/tauri-apps/wry/issues/1862")
        );
    }
}
