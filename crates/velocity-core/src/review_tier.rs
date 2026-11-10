//! PR duty's review tiers (SPEC.md F7 Tiered review). Velocity tiers each PR from its diff:
//! **trivial** when it changes only docs, comments or formatting, **sensitive** when it touches
//! a path listed under "Sensitive paths" in AGENTS.md, and **normal** otherwise. Every tier
//! still needs an approving review (Rule 4); the tier only sets how deep that review goes.

use crate::review_diff::DiffSummary;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ReviewTier {
    /// Docs, comments or formatting: approved once the local checks pass and the change
    /// matches its claim. No proof needed.
    Trivial,
    /// Full review.
    Normal,
    /// Touches a sensitive path: full review, the full stress suite before merge, and a
    /// confidence proof at the strongest level the language allows (F17).
    Sensitive,
}

impl ReviewTier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Trivial => "trivial",
            Self::Normal => "normal",
            Self::Sensitive => "sensitive",
        }
    }
}

/// A PR's tier and, when it is sensitive, the files that made it so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TierAssessment {
    pub tier: ReviewTier,
    pub sensitive_files: Vec<String>,
}

/// Tiers a diff against the sensitive path patterns. Sensitive wins over trivial: a comment
/// edit in a sensitive file still gets the sensitive review.
pub fn assess_tier(diff: &DiffSummary, sensitive_patterns: &[String]) -> TierAssessment {
    let sensitive_files: Vec<String> = diff
        .paths
        .iter()
        .filter(|path| {
            sensitive_patterns
                .iter()
                .any(|pattern| path_matches(pattern, path))
        })
        .cloned()
        .collect();
    let tier = if !sensitive_files.is_empty() {
        ReviewTier::Sensitive
    } else if diff.cosmetic {
        ReviewTier::Trivial
    } else {
        ReviewTier::Normal
    };
    TierAssessment {
        tier,
        sensitive_files,
    }
}

/// The path patterns AGENTS.md lists under a "Sensitive paths" heading: every inline code
/// span on that section's bullets, e.g. "- `crates/velocity-store/`: saved state".
pub fn sensitive_paths(agents_markdown: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut section_level: Option<usize> = None;
    let mut fenced = false;
    for line in agents_markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some((level, title)) = heading(trimmed) {
            let opens = title.eq_ignore_ascii_case("sensitive paths");
            section_level = match section_level {
                Some(open) if level > open => Some(open),
                _ => opens.then_some(level),
            };
            continue;
        }
        let bullet = ["- ", "* ", "+ "]
            .iter()
            .any(|marker| trimmed.starts_with(marker));
        if section_level.is_some() && bullet {
            patterns.extend(code_spans(trimmed).filter_map(normalize_pattern));
        }
    }
    patterns
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|byte| *byte == b'#').count();
    let title = line.get(level..)?;
    ((1..=6).contains(&level) && title.starts_with(' ')).then(|| (level, title.trim()))
}

fn code_spans(line: &str) -> impl Iterator<Item = &str> {
    // Odd pieces between backticks are code; an unclosed span at the end is not.
    let pieces: Vec<&str> = line.split('`').collect();
    let closed = pieces.len().saturating_sub(1) / 2 * 2;
    pieces.into_iter().take(closed + 1).skip(1).step_by(2)
}

fn normalize_pattern(span: &str) -> Option<String> {
    let pattern = span.trim().replace('\\', "/");
    let pattern = pattern.trim_start_matches("./").trim_start_matches('/');
    (!pattern.is_empty() && !pattern.contains(char::is_whitespace)).then(|| pattern.to_owned())
}

/// Whether a repository path matches a sensitive pattern: a file, a folder (with or without a
/// trailing `/`), or a glob where `*` and `?` stay inside one folder and `**` spans folders.
/// ASCII case is ignored: Windows checkouts treat `Store.rs` and `store.rs` as one file.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    let path = path.replace('\\', "/");
    glob(pattern.as_bytes(), path.as_bytes())
        || glob(format!("{pattern}/**").as_bytes(), path.as_bytes())
}

fn glob(pattern: &[u8], path: &[u8]) -> bool {
    match pattern {
        [] => path.is_empty(),
        [b'*', b'*', rest @ ..] => {
            // `**/` also matches no folder at all, but only at a folder boundary.
            let (rest, bounded) = match rest.strip_prefix(b"/") {
                Some(rest) => (rest, true),
                None => (rest, false),
            };
            (0..=path.len())
                .filter(|&index| !bounded || index == 0 || path[index - 1] == b'/')
                .any(|index| glob(rest, &path[index..]))
        }
        [b'*', rest @ ..] => (0..=path.len())
            .take_while(|&index| index == 0 || path[index - 1] != b'/')
            .any(|index| glob(rest, &path[index..])),
        [b'?', rest @ ..] => {
            path.first().is_some_and(|byte| *byte != b'/') && glob(rest, &path[1..])
        }
        [expected, rest @ ..] => {
            path.first()
                .is_some_and(|byte| byte.eq_ignore_ascii_case(expected))
                && glob(rest, &path[1..])
        }
    }
}
