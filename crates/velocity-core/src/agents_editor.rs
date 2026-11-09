//! Pure F11 outliner operations. The native edge owns reading and committing AGENTS.md.

use crate::agents_markdown::{self, OutlineKind, OutlineLine};

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub enum OutlineAction {
    Edit { identifier: u32, text: String },
    Enter { identifier: u32 },
    Indent { identifier: u32, nested: bool },
    Move { identifier: u32, down: bool },
    DeleteEmpty { identifier: u32 },
    Neighbor { identifier: u32, down: bool },
    AddRule { section: Option<u32> },
    AddSection,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct OutlineFocus {
    pub identifier: u32,
    pub at_end: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct OutlineSnapshot {
    pub sections: Vec<OutlineSection>,
    pub markdown: String,
    pub dirty: bool,
    pub rule_count: u32,
    pub focus: Option<OutlineFocus>,
}

#[derive(Clone, Debug)]
pub struct AgentsEditor {
    lines: Vec<OutlineLine>,
    baseline: String,
    next_identifier: u32,
    newline: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct OutlineSection {
    pub identifier: Option<u32>,
    pub lines: Vec<OutlineLine>,
}

impl AgentsEditor {
    pub fn new(source: String) -> Self {
        let next_identifier = u32::try_from(source.lines().count()).unwrap_or(u32::MAX);
        let newline = if source.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
        .into();
        Self {
            lines: agents_markdown::parse(&source),
            baseline: source,
            next_identifier,
            newline,
        }
    }

    pub fn baseline(&self) -> &str {
        &self.baseline
    }

    pub fn saved(&mut self) {
        self.baseline = self.markdown();
    }

    pub fn markdown(&self) -> String {
        self.lines.iter().map(OutlineLine::markdown).collect()
    }

    pub fn snapshot(&self, focus: Option<OutlineFocus>) -> OutlineSnapshot {
        let markdown = self.markdown();
        OutlineSnapshot {
            dirty: markdown != self.baseline,
            markdown,
            sections: self.sections(),
            focus,
            rule_count: u32::try_from(
                self.lines
                    .iter()
                    .filter(|line| line.kind == OutlineKind::Rule)
                    .count(),
            )
            .unwrap_or(u32::MAX),
        }
    }

    fn sections(&self) -> Vec<OutlineSection> {
        let mut sections = Vec::new();
        let mut current = OutlineSection {
            identifier: None,
            lines: Vec::new(),
        };
        for line in &self.lines {
            if line.kind == OutlineKind::Section {
                if !current.lines.is_empty() {
                    sections.push(current);
                }
                current = OutlineSection {
                    identifier: Some(line.identifier),
                    lines: Vec::new(),
                };
            }
            current.lines.push(line.clone());
        }
        sections.push(current);
        sections
    }

    pub fn apply(&mut self, action: OutlineAction) -> OutlineSnapshot {
        use OutlineAction::*;
        let focus =
            match action {
                Edit { identifier, text } => {
                    if let Some(line) = self.lines.iter_mut().find(|line| {
                        line.identifier == identifier && line.kind != OutlineKind::Source
                    }) {
                        line.replace(text);
                    }
                    None
                }
                Enter { identifier } => self.enter(identifier),
                Indent { identifier, nested } => self.indent(identifier, nested),
                Move { identifier, down } => self.move_rule(identifier, down),
                DeleteEmpty { identifier } => self.delete_empty(identifier),
                Neighbor { identifier, down } => self.neighbor(identifier, down),
                AddRule { section } => self.add_rule(section),
                AddSection => self.add_section(),
            };
        self.snapshot(focus)
    }

    fn index(&self, identifier: u32) -> Option<usize> {
        self.lines
            .iter()
            .position(|line| line.identifier == identifier)
    }

    fn insert(&mut self, index: usize, source: &str) -> u32 {
        let identifier = self.next_identifier;
        self.next_identifier = self.next_identifier.saturating_add(1);
        if let Some(previous) = index
            .checked_sub(1)
            .and_then(|index| self.lines.get_mut(index))
            && previous.ending.is_empty()
        {
            previous.ending = self.newline.clone();
        }
        let line = OutlineLine::new(identifier, &format!("{source}{}", self.newline), false);
        self.lines.insert(index, line);
        identifier
    }

    fn enter(&mut self, identifier: u32) -> Option<OutlineFocus> {
        let index = self.index(identifier)?;
        let line = &self.lines[index];
        if line.kind == OutlineKind::Source {
            return None;
        }
        if matches!(line.kind, OutlineKind::Title | OutlineKind::Section) {
            let next = self.lines[index + 1..]
                .iter()
                .take_while(|line| line.kind != OutlineKind::Section)
                .find(|line| line.kind == OutlineKind::Rule);
            if let Some(next) = next {
                return Some(focus(next.identifier, true));
            }
        }
        if line.kind == OutlineKind::Rule && line.text.trim().is_empty() {
            return self.indent(identifier, false);
        }
        if let Some(title) = line
            .text
            .strip_prefix("## ")
            .filter(|_| line.kind == OutlineKind::Rule)
        {
            return self.section_from_rule(index, title.to_owned());
        }
        if self
            .lines
            .get(index + 1)
            .is_some_and(|line| line.kind == OutlineKind::Rule && line.text.is_empty())
        {
            return Some(focus(self.lines[index + 1].identifier, true));
        }
        let prefix = if self.lines[index].nested {
            "  - "
        } else {
            "- "
        };
        Some(focus(self.insert(index + 1, prefix), true))
    }

    fn section_from_rule(&mut self, index: usize, title: String) -> Option<OutlineFocus> {
        let mut heading = self.lines.remove(index);
        heading.kind = OutlineKind::Section;
        heading.prefix = "## ".into();
        heading.nested = false;
        heading.replace(title);
        let end = self.lines[index..]
            .iter()
            .position(|line| line.kind == OutlineKind::Section)
            .map_or(self.lines.len(), |offset| index + offset);
        self.lines.insert(end, heading);
        Some(focus(self.insert(end + 1, "- "), true))
    }

    fn indent(&mut self, identifier: u32, nested: bool) -> Option<OutlineFocus> {
        let index = self.index(identifier)?;
        let previous_rule = index > 0 && self.lines[index - 1].kind == OutlineKind::Rule;
        let line = &mut self.lines[index];
        if line.kind != OutlineKind::Rule {
            return None;
        }
        line.nested = nested && previous_rule;
        line.prefix = if line.nested { "  - " } else { "- " }.into();
        Some(focus(identifier, true))
    }

    fn move_rule(&mut self, identifier: u32, down: bool) -> Option<OutlineFocus> {
        let index = self.index(identifier)?;
        let other = if down {
            index.checked_add(1)?
        } else {
            index.checked_sub(1)?
        };
        if self.lines.get(other)?.kind == OutlineKind::Rule
            && self.lines[index].kind == OutlineKind::Rule
        {
            self.lines.swap(index, other);
            // A former last line without a newline must not join its new neighbour.
            let last = self.lines.len().saturating_sub(1);
            for line in self.lines.iter_mut().take(last) {
                if line.ending.is_empty() {
                    line.ending = self.newline.clone();
                }
            }
        }
        Some(focus(identifier, true))
    }

    fn delete_empty(&mut self, identifier: u32) -> Option<OutlineFocus> {
        let index = self.index(identifier)?;
        let line = &self.lines[index];
        if line.kind != OutlineKind::Rule || !line.text.is_empty() || !line.continuation.is_empty()
        {
            return None;
        }
        if line.nested {
            return self.indent(identifier, false);
        }
        let target = self
            .neighbor(identifier, false)
            .or_else(|| self.neighbor(identifier, true));
        self.lines.remove(index);
        target
    }

    fn neighbor(&self, identifier: u32, down: bool) -> Option<OutlineFocus> {
        let index = self.index(identifier)?;
        let selectable =
            |line: &&OutlineLine| matches!(line.kind, OutlineKind::Rule | OutlineKind::Section);
        let next = if down {
            self.lines[index + 1..].iter().find(selectable)
        } else {
            self.lines[..index].iter().rev().find(selectable)
        }?;
        Some(focus(next.identifier, !down))
    }

    fn add_rule(&mut self, section: Option<u32>) -> Option<OutlineFocus> {
        let start = section
            .and_then(|identifier| self.index(identifier))
            .map_or(0, |index| index + 1);
        let end = self.lines[start..]
            .iter()
            .position(|line| line.kind == OutlineKind::Section)
            .map_or(self.lines.len(), |offset| start + offset);
        Some(focus(self.insert(end, "- "), true))
    }

    fn add_section(&mut self) -> Option<OutlineFocus> {
        let end = self.lines.len();
        self.insert(end, "");
        let identifier = self.insert(end + 1, "## ");
        self.insert(end + 2, "- ");
        Some(focus(identifier, true))
    }
}

fn focus(identifier: u32, at_end: bool) -> OutlineFocus {
    OutlineFocus { identifier, at_end }
}
