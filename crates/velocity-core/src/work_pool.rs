//! F16 inventory only. Reserving or choosing work belongs to the claim lock and runner.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkKey {
    Task { identifier: u32 },
    Issue { repository: String, number: u32 },
    Spec { section: String },
}

impl WorkKey {
    /// The AIRC claim lock key for this piece (F14, F16). A goal swarm agent, a swarm
    /// member and a dispatched thread working on the same item all claim this key, so
    /// the first claim wins whichever path started it. An issue omits its repository:
    /// each project's channel belongs to one repository.
    pub fn claim_piece(&self) -> String {
        match self {
            Self::Task { identifier } => format!("task:{identifier}"),
            Self::Issue { number, .. } => format!("issue:{number}"),
            Self::Spec { section } => format!("spec:{section}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct WorkPiece {
    pub key: WorkKey,
    pub title: String,
    pub reference: String,
    pub thread_identifier: Option<u32>,
}

impl WorkPiece {
    pub fn new(key: WorkKey, title: String, reference: String, owner: Option<u32>) -> Self {
        Self {
            key,
            title,
            reference,
            thread_identifier: owner,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct WorkPool {
    pub project_identifier: u32,
    pub pieces: Vec<WorkPiece>,
    pub available_tasks: u32,
    pub available_issues: u32,
    pub available_sections: u32,
}

impl WorkPool {
    /// Keep claimed rows for the board; only unowned rows count toward launch capacity.
    pub fn new(project_identifier: u32, pieces: Vec<WorkPiece>) -> Self {
        let mut pool = Self {
            project_identifier,
            pieces: Vec::new(),
            available_tasks: 0,
            available_issues: 0,
            available_sections: 0,
        };
        let mut seen = BTreeSet::new();
        for piece in pieces {
            if !seen.insert(piece.key.clone()) {
                continue;
            }
            if piece.thread_identifier.is_none() {
                match piece.key {
                    WorkKey::Task { .. } => pool.available_tasks += 1,
                    WorkKey::Issue { .. } => pool.available_issues += 1,
                    WorkKey::Spec { .. } => pool.available_sections += 1,
                }
            }
            pool.pieces.push(piece);
        }
        pool
    }
}

/// Use the implementation plan when present: acceptance checkboxes in feature
/// descriptions are often left unchecked after landing and must not duplicate work.
/// Other specs expose unfinished checklist-bearing sections as one piece each.
pub fn spec_pieces(markdown: &str) -> Vec<WorkPiece> {
    let lines = visible_lines(markdown);
    let plan = lines.iter().position(|line| {
        line.starts_with("## ")
            && (line.contains("Implementation order") || line.contains("Implementation plan"))
    });
    if let Some(start) = plan {
        let end = lines[start + 1..]
            .iter()
            .position(|line| line.starts_with("## "))
            .map_or(lines.len(), |offset| start + 1 + offset);
        return plan_pieces(&lines[start + 1..end]);
    }
    section_pieces(&lines)
}

fn visible_lines(markdown: &str) -> Vec<&str> {
    let mut fence: Option<(u8, usize)> = None;
    markdown
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            let marker = trimmed.as_bytes().first().copied().unwrap_or_default();
            let count = trimmed.bytes().take_while(|byte| *byte == marker).count();
            if matches!(marker, b'`' | b'~') && count >= 3 {
                match fence {
                    None => fence = Some((marker, count)),
                    Some((opening, width)) if opening == marker && count >= width => fence = None,
                    _ => (),
                }
                return false;
            }
            fence.is_none()
        })
        .collect()
}

fn plan_pieces(lines: &[&str]) -> Vec<WorkPiece> {
    let mut group = String::new();
    let mut pieces = Vec::new();
    for line in lines {
        if line.starts_with("**") || line.starts_with('#') {
            group = line.trim_matches(['*', '#', ' ']).to_owned();
        }
        if let Some(title) = unchecked_title(line) {
            let section = format!("{group}/{title}");
            pieces.push(WorkPiece::new(
                WorkKey::Spec { section },
                title.into(),
                format!("SPEC.md · {group}"),
                None,
            ));
        }
    }
    pieces
}

fn section_pieces(lines: &[&str]) -> Vec<WorkPiece> {
    let mut headings: Vec<(usize, String)> = Vec::new();
    let mut sections = BTreeSet::new();
    let mut pieces = Vec::new();
    for line in lines {
        let level = line.bytes().take_while(|byte| *byte == b'#').count();
        if level >= 2 && line.as_bytes().get(level) == Some(&b' ') {
            headings.retain(|(depth, _)| *depth < level);
            headings.push((level, line[level..].trim().to_owned()));
        }
        if unchecked_title(line).is_some() && !headings.is_empty() {
            let section = headings
                .iter()
                .map(|(_, title)| title.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            if sections.insert(section.clone()) {
                pieces.push(WorkPiece::new(
                    WorkKey::Spec {
                        section: section.clone(),
                    },
                    section,
                    "SPEC.md".into(),
                    None,
                ));
            }
        }
    }
    pieces
}

fn unchecked_title(line: &str) -> Option<&str> {
    let title = line.trim_start().strip_prefix("- [ ] ")?.trim();
    (!title.is_empty()).then_some(title)
}
