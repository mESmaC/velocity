//! Issue dependencies as GitHub links (#511): the issues an agent names as blockers, the
//! retired `Held for dependencies` body note, and the `needs-owner` reasons that only wait
//! on other issues. Pure and synchronous; `velocity-github` makes the GitHub calls.

use std::fmt;
use std::ops::Range;

/// The line the retired hold wrote into an issue body beside its `needs-owner` label.
const HOLD_NOTE: &str = "held for dependencies";
/// The line a discovery carries its blockers on, from the ledger to the filed issue.
const BLOCKED_BY_LINE: &str = "Blocked by:";
/// Words that say a text waits on the issues it names.
const WAITING_WORDS: [&str; 14] = [
    HOLD_NOTE,
    "waits for",
    "wait for",
    "waiting for",
    "waits on",
    "wait on",
    "waiting on",
    "blocked by",
    "blocked on",
    "depends on",
    "depend on",
    "dependent on",
    "held until",
    "hold until",
];
/// Grounds that go beyond waiting on other project issues (F9, #526).
const NON_DEPENDENCY_GROUNDS: [&str; 4] = ["hardware", "physical", "upstream", "scope hold"];

/// An issue named in text or tool arguments: `#12` in the repository at hand, or
/// `owner/name#12` and issue URLs for any repository.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IssueReference {
    repository: Option<String>,
    number: u32,
}

impl IssueReference {
    /// `None` for issue 0 and for a repository that is not `owner/name`.
    pub fn new(repository: Option<&str>, number: u32) -> Option<Self> {
        if number == 0 || repository.is_some_and(|name| !is_repository(name)) {
            return None;
        }
        Some(Self {
            repository: repository.map(str::to_owned),
            number,
        })
    }

    /// One whole reference, as a tool argument gives it: `12`, `#12`, `owner/name#12` or
    /// an issue URL.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Ok(number) = text.parse() {
            return Self::new(None, number);
        }
        let mut found = Self::in_text(text).into_iter();
        let reference = found.next()?;
        // Anything left over, such as a second issue or a sentence, is not one reference.
        let exact = text == reference.to_string() || text.contains("github.com/");
        (exact && found.next().is_none() && !text.contains(char::is_whitespace))
            .then_some(reference)
    }

    /// Every issue a text names, in the order it names them, each once.
    pub fn in_text(text: &str) -> Vec<Self> {
        let mut found: Vec<(usize, Self)> = text
            .match_indices('#')
            .filter_map(|(position, _)| Some((position, Self::at_hash(text, position)?)))
            .chain(
                text.match_indices("github.com/")
                    .filter_map(|(position, host)| {
                        Some((position, Self::in_address(&text[position + host.len()..])?))
                    }),
            )
            .collect();
        found.sort_by_key(|(position, _)| *position);
        let mut references = Vec::new();
        for (_, reference) in found {
            if !references.contains(&reference) {
                references.push(reference);
            }
        }
        references
    }

    /// The reference whose `#` sits at `position`: digits follow it and a repository, or
    /// nothing that belongs to a word, comes before it.
    fn at_hash(text: &str, position: usize) -> Option<Self> {
        let rest = &text[position + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        // `#12ab` is a colour or an anchor, never an issue.
        let ends_word = rest[digits..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_alphanumeric() && next != '_');
        if digits == 0 || !ends_word {
            return None;
        }
        let before = &text[..position];
        let start = before
            .char_indices()
            .rev()
            .find(|(_, character)| !is_repository_character(*character))
            .map_or(0, |(index, character)| index + character.len_utf8());
        let prefix = before[start..].trim_start_matches('/');
        let prefix = prefix.strip_prefix("github.com/").unwrap_or(prefix);
        let number = rest[..digits].parse().ok()?;
        if prefix.is_empty() {
            Self::new(None, number)
        } else {
            Self::new(Some(prefix), number)
        }
    }

    /// `owner/name/issues/12`, the rest of an issue URL after its host.
    fn in_address(path: &str) -> Option<Self> {
        let mut parts = path.splitn(4, '/');
        let (owner, name) = (parts.next()?, parts.next()?);
        if parts.next()? != "issues" {
            return None;
        }
        let number = parts.next()?;
        let digits = number.bytes().take_while(u8::is_ascii_digit).count();
        Self::new(
            Some(&format!("{owner}/{name}")),
            number[..digits].parse().ok()?,
        )
    }

    pub fn number(&self) -> u32 {
        self.number
    }

    /// The repository the issue lives in, with `home` for a bare `#12`.
    pub fn repository_or<'reference>(&'reference self, home: &'reference str) -> &'reference str {
        self.repository.as_deref().unwrap_or(home)
    }

    /// Whether the reference names a repository itself, so it reads the same anywhere.
    pub fn names_repository(&self) -> bool {
        self.repository.is_some()
    }

    /// Whether this is `repository`'s issue `number` when read in `home`.
    pub fn is_issue(&self, home: &str, repository: &str, number: u32) -> bool {
        self.number == number && self.repository_or(home).eq_ignore_ascii_case(repository)
    }

    /// `#12` for an issue of `home` and `owner/name#12` for any other, the form
    /// `GitHubIssue::open_blockers` lists.
    pub fn relative_to(&self, home: &str) -> String {
        match &self.repository {
            Some(repository) if !repository.eq_ignore_ascii_case(home) => {
                format!("{repository}#{}", self.number)
            }
            _ => format!("#{}", self.number),
        }
    }
}

impl fmt::Display for IssueReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.repository {
            Some(repository) => write!(formatter, "{repository}#{}", self.number),
            None => write!(formatter, "#{}", self.number),
        }
    }
}

fn is_repository_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || "-_./".contains(character)
}

/// `owner/name` in GitHub's characters. GitHub itself decides whether it exists.
pub(crate) fn is_repository(name: &str) -> bool {
    name.split_once('/').is_some_and(|(owner, name)| {
        let named = |part: &str, allowed: &str| {
            !part.is_empty()
                && part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || allowed.contains(character)
                })
        };
        named(owner, "-") && named(name, "-_.") && name != "." && name != ".."
    })
}

/// A tool's `blocked_by` argument as references, each once.
pub fn parse_blockers(names: &[String]) -> Result<Vec<IssueReference>, String> {
    let mut blockers = Vec::new();
    for name in names {
        let blocker = IssueReference::parse(name).ok_or_else(|| {
            format!("blocked_by names issues as #12 or owner/name#12, not {name:?}")
        })?;
        if !blockers.contains(&blocker) {
            blockers.push(blocker);
        }
    }
    Ok(blockers)
}

/// `#12, owner/name#40`: blockers as an agent or an issue body lists them.
pub fn blocker_list(blockers: &[IssueReference]) -> String {
    blockers
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The retired hold in an issue body: a `Held for dependencies: waits for #N …` line that
/// went with a `needs-owner` label nothing removed (#511).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyHold {
    /// The issues the line names.
    pub blockers: Vec<IssueReference>,
    line: Range<usize>,
}

impl DependencyHold {
    /// The first hold line of `body` that names an issue. A line that names none cannot
    /// become links and is left as it is.
    pub fn find(body: &str) -> Option<Self> {
        let mut start = 0;
        for line in body.split_inclusive('\n') {
            let text = line.trim_end_matches(['\r', '\n']);
            let blockers = if text.to_lowercase().contains(HOLD_NOTE) {
                IssueReference::in_text(text)
            } else {
                Vec::new()
            };
            if !blockers.is_empty() {
                return Some(Self {
                    blockers,
                    line: start..start + text.len(),
                });
            }
            start += line.len();
        }
        None
    }

    /// `body` with the hold line replaced by what now holds the issue, so the line is
    /// converted once and never tells anyone to remove a label again.
    pub fn rewritten(&self, body: &str) -> String {
        format!(
            "{}Blocked by {}, recorded as GitHub issue dependencies. Dispatch starts this issue when they close.{}",
            &body[..self.line.start],
            blocker_list(&self.blockers),
            &body[self.line.end..]
        )
    }
}

/// The issues a hold reason waits on when waiting is all it gives (#511, #526): it
/// names issues beside waiting words and names no hardware, upstream release or scope hold.
pub fn dependency_only_hold(reason: &str) -> Vec<IssueReference> {
    let lowered = reason.to_lowercase();
    if NON_DEPENDENCY_GROUNDS
        .iter()
        .any(|ground| lowered.contains(ground))
    {
        return Vec::new();
    }
    let mut blockers = Vec::new();
    // A paragraph at a time: a long report also cites issues it does not wait on.
    for paragraph in reason.split("\n\n") {
        let paragraph_lowered = paragraph.to_lowercase();
        if !WAITING_WORDS
            .iter()
            .any(|words| paragraph_lowered.contains(words))
        {
            continue;
        }
        for blocker in IssueReference::in_text(paragraph) {
            if !blockers.contains(&blocker) {
                blockers.push(blocker);
            }
        }
    }
    blockers
}

/// What a filing or hold tool answers when a hold is requested for `blockers` alone.
pub fn dependency_hold_refusal(blockers: &[IssueReference]) -> String {
    format!(
        "This reason only waits for other issues ({}). \
         Record them as GitHub dependencies instead: pass them as blocked_by without a hold label, and dispatch starts the issue when they close. \
         For an upstream PR or release, use blocked-upstream; for hardware or an explicit scope hold, name it in the reason.",
        blocker_list(blockers)
    )
}

/// `text` with its blockers on a closing `Blocked by:` line, which travels with a
/// discovery through the ledger to the issue Velocity files.
pub fn with_declared_blockers(text: &str, blockers: &[IssueReference]) -> String {
    if blockers.is_empty() {
        return text.to_owned();
    }
    format!(
        "{}\n\n{BLOCKED_BY_LINE} {}",
        text.trim_end(),
        blocker_list(blockers)
    )
}

/// The issues named on a text's `Blocked by:` lines.
pub fn declared_blockers(text: &str) -> Vec<IssueReference> {
    let mut blockers = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let declares = line
            .get(..BLOCKED_BY_LINE.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(BLOCKED_BY_LINE));
        if !declares {
            continue;
        }
        for blocker in IssueReference::in_text(&line[BLOCKED_BY_LINE.len()..]) {
            if !blockers.contains(&blocker) {
                blockers.push(blocker);
            }
        }
    }
    blockers
}

/// Phrases introducing issues that another issue waits on (`waits for #N`, `after #N`,
/// `blocked by #N`, etc.) used to clear stale dispatch attempts when a blocker closes (#617).
const BLOCKER_REFERENCE_PHRASES: [&str; 15] = [
    "waits for",
    "wait for",
    "waiting for",
    "waits on",
    "wait on",
    "waiting on",
    "blocked by",
    "blocked on",
    "depends on",
    "depend on",
    "dependent on",
    "held until",
    "hold until",
    "after",
    HOLD_NOTE,
];

/// Maximum span expanded for inclusive issue ranges like `#421–#424` or `#421-#424`.
const MAX_ISSUE_RANGE_SPAN: u32 = 20;

/// Extracts issues referenced as blockers in free text via `waits for #N`, `after #N`,
/// `blocked by #N`, `depends on #N` (including lists and ranges such as `after #421–#424`) (#617).
pub fn referenced_blockers_in_text(text: &str) -> Vec<IssueReference> {
    let mut found = Vec::new();
    for line in text.lines() {
        let lowered = line.to_ascii_lowercase();
        for phrase in BLOCKER_REFERENCE_PHRASES {
            let mut offset = 0;
            while let Some(pos) = lowered[offset..].find(phrase) {
                let start = offset + pos;
                let end = start + phrase.len();
                let left_ok = line[..start]
                    .chars()
                    .next_back()
                    .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
                let right_ok = line[end..]
                    .chars()
                    .next()
                    .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
                if left_ok && right_ok {
                    let clause = clause_tail(&line[end..]);
                    collect_references_and_ranges(clause, &mut found);
                }
                offset = end;
            }
        }
    }
    found
}

fn clause_tail(rest: &str) -> &str {
    for (index, ch) in rest.char_indices() {
        if matches!(ch, ';' | '\n' | '\r') {
            return &rest[..index];
        }
        if ch == '.' {
            let next = rest[index + 1..].chars().next();
            if next.is_none_or(|c| c.is_whitespace()) {
                return &rest[..index];
            }
        }
    }
    rest
}

fn collect_references_and_ranges(clause: &str, out: &mut Vec<IssueReference>) {
    for reference in IssueReference::in_text(clause) {
        if !out.contains(&reference) {
            out.push(reference);
        }
    }
    // Expand inclusive ranges like `#421–#424`, `#421-#424`, or `#421..#424`.
    for (hash_pos, _) in clause.match_indices('#') {
        let Some(first) = IssueReference::at_hash(clause, hash_pos) else {
            continue;
        };
        let after_hash = &clause[hash_pos + 1..];
        let digits = after_hash.bytes().take_while(u8::is_ascii_digit).count();
        let tail = after_hash[digits..].trim_start();
        let Some(after_sep) = tail
            .strip_prefix("–")
            .or_else(|| tail.strip_prefix("—"))
            .or_else(|| tail.strip_prefix("..="))
            .or_else(|| tail.strip_prefix(".."))
            .or_else(|| tail.strip_prefix('-'))
        else {
            continue;
        };
        let after_sep = after_sep.trim_start();
        let second_digits_str = after_sep.strip_prefix('#').unwrap_or(after_sep);
        let second_len = second_digits_str
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if second_len == 0 {
            continue;
        }
        let Ok(end_num) = second_digits_str[..second_len].parse::<u32>() else {
            continue;
        };
        let start_num = first.number();
        if end_num > start_num && end_num - start_num <= MAX_ISSUE_RANGE_SPAN {
            let repo = first.repository.as_deref();
            for number in start_num..=end_num {
                if let Some(reference) = IssueReference::new(repo, number)
                    && !out.contains(&reference)
                {
                    out.push(reference);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referenced_blockers_extracts_waits_after_blocked_by_and_ranges() {
        let body = "\
This issue waits for #419 before starting.\n\
Phased after #421–#424 and blocked by mESmaC/rosen#42.\n\
Unrelated mention of #999 in background prose.\n";
        let refs: Vec<String> = referenced_blockers_in_text(body)
            .into_iter()
            .map(|r| r.to_string())
            .collect();
        assert!(refs.contains(&"#419".to_owned()));
        assert!(refs.contains(&"#421".to_owned()));
        assert!(refs.contains(&"#422".to_owned()));
        assert!(refs.contains(&"#423".to_owned()));
        assert!(refs.contains(&"#424".to_owned()));
        assert!(refs.contains(&"mESmaC/rosen#42".to_owned()));
        assert!(!refs.contains(&"#999".to_owned()));
    }
}
