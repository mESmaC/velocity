//! F11 live lint: marks hedges, "should" and vague words in AGENTS.md rules. The lint only
//! advises (Q8); nothing here blocks typing, saving or committing.

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LintKind {
    /// Wording that makes a rule optional ("maybe", "try to", "where possible").
    Hedge,
    /// "Should", which agents read as optional.
    Modal,
    /// A word with no standard an agent can check ("clean", "good").
    Vague,
}

/// One flagged phrase inside a rule, as byte offsets into the rule's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LintMatch {
    pub start: usize,
    pub end: usize,
    pub kind: LintKind,
    pub message: &'static str,
}

const ESCAPE_HATCH: &str = "Escape hatch. Name the exception, or drop it.";
const LEAVES_IT_OPEN: &str = "Leaves the decision to the agent.";
const OPTIONAL: &str = "Hedge. Agents read this as optional.";
const VAGUE: &str = "Vague. Name the exact standard.";

/// The reference UI's lint table. Longer phrases come first so that, at the same start,
/// "should probably" wins over "should".
const PHRASES: &[(&str, LintKind, &str)] = &[
    ("should probably", LintKind::Hedge, "Hedge. Make it a rule."),
    ("probably", LintKind::Hedge, OPTIONAL),
    ("maybe", LintKind::Hedge, OPTIONAL),
    ("ideally", LintKind::Hedge, OPTIONAL),
    ("hopefully", LintKind::Hedge, OPTIONAL),
    (
        "try to",
        LintKind::Hedge,
        "\u{201c}Try to\u{201d} makes the rule optional.",
    ),
    ("where possible", LintKind::Hedge, ESCAPE_HATCH),
    ("if possible", LintKind::Hedge, ESCAPE_HATCH),
    ("when possible", LintKind::Hedge, ESCAPE_HATCH),
    ("feel free to", LintKind::Hedge, LEAVES_IT_OPEN),
    ("if you can", LintKind::Hedge, LEAVES_IT_OPEN),
    ("as needed", LintKind::Hedge, LEAVES_IT_OPEN),
    ("when appropriate", LintKind::Hedge, LEAVES_IT_OPEN),
    (
        "consider",
        LintKind::Hedge,
        "\u{201c}Consider\u{201d} isn\u{2019}t an instruction. Say what to do, and when.",
    ),
    (
        "should",
        LintKind::Modal,
        "\u{201c}Should\u{201d} reads as optional. Use must / must not.",
    ),
    ("clean", LintKind::Vague, VAGUE),
    ("good", LintKind::Vague, VAGUE),
    ("nice", LintKind::Vague, VAGUE),
    ("appropriate", LintKind::Vague, VAGUE),
    ("reasonable", LintKind::Vague, VAGUE),
    ("properly", LintKind::Vague, VAGUE),
    ("etc.", LintKind::Vague, VAGUE),
    ("etc", LintKind::Vague, VAGUE),
];

/// Every flagged phrase in `text`, in order and never overlapping. Matching ignores ASCII
/// case and needs a word boundary on both sides, so "shouldn't" and "goodbye" pass.
pub(crate) fn find_issues(text: &str) -> Vec<LintMatch> {
    let mut found = Vec::new();
    let mut end = 0;
    for (start, _) in text.char_indices() {
        if start < end || !boundary_before(text, start) {
            continue;
        }
        if let Some(issue) = match_at(text, start) {
            end = issue.end;
            found.push(issue);
        }
    }
    found
}

fn match_at(text: &str, start: usize) -> Option<LintMatch> {
    let rest = &text.as_bytes()[start..];
    PHRASES.iter().find_map(|(phrase, kind, message)| {
        let end = start + phrase.len();
        let matches = rest
            .get(..phrase.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(phrase.as_bytes()));
        // "etc." ends in punctuation, so whatever follows it is already a boundary.
        (matches && (phrase.ends_with('.') || boundary_after(text, end))).then_some(LintMatch {
            start,
            end,
            kind: *kind,
            message,
        })
    })
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn boundary_before(text: &str, start: usize) -> bool {
    !text[..start].chars().next_back().is_some_and(is_word)
}

fn boundary_after(text: &str, end: usize) -> bool {
    // A phrase that ends inside a multibyte character is no match at all.
    text.get(end..)
        .is_some_and(|rest| !rest.chars().next().is_some_and(is_word))
}

/// A rule in the side panel's Flagged list, with its first flagged phrase.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct FlaggedRule {
    pub identifier: u32,
    pub section: String,
    pub kind: LintKind,
    pub phrase: String,
    pub message: String,
}

impl crate::agents_editor::AgentsEditor {
    /// Rules that trip the lint, in file order. A rule the model is rewriting leaves the list
    /// until its answer lands, as in the reference.
    pub(crate) fn flagged_rules(&self) -> Vec<FlaggedRule> {
        use crate::agents_markdown::OutlineKind;
        let mut section = String::new();
        let mut flagged = Vec::new();
        for line in &self.lines {
            match line.kind {
                OutlineKind::Section => section = line.text.trim().to_owned(),
                OutlineKind::Rule if !self.rewriting(line.identifier) => {
                    let Some(part) = line.inline.iter().find(|part| part.mark.is_some()) else {
                        continue;
                    };
                    let Some(issue) = find_issues(&part.text).into_iter().next() else {
                        continue;
                    };
                    flagged.push(FlaggedRule {
                        identifier: line.identifier,
                        section: if section.is_empty() {
                            "Untitled".into()
                        } else {
                            section.clone()
                        },
                        kind: issue.kind,
                        phrase: part.text.clone(),
                        message: issue.message.into(),
                    });
                }
                _ => {}
            }
        }
        flagged
    }

    fn rewriting(&self, identifier: u32) -> bool {
        self.rewrites
            .get(&identifier)
            .is_some_and(|record| record.pending.is_some())
    }
}
