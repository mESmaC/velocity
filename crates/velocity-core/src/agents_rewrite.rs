//! F11 asynchronous rewrite reconciliation. Requests hold no document lock while a model works.
use crate::agents_editor::AgentsEditor;
use crate::agents_markdown::{OutlineKind, OutlineLine};

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct RewriteView {
    pub identifier: u32,
    pub original: Option<String>,
    pub pending: bool,
    pub note: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RewriteTicket {
    pub identifier: u32,
    pub revision: u64,
    pub source: String,
    pub context: String,
}

#[derive(Clone, Debug)]
pub(crate) struct RewriteRecord {
    original: Option<OutlineLine>,
    inserted: Vec<u32>,
    pub pending: Option<u64>,
    note: Option<String>,
}

impl AgentsEditor {
    pub fn begin_rewrite(&mut self, identifier: u32) -> Option<RewriteTicket> {
        let line = self.lines.get(self.index(identifier)?)?;
        if line.kind != OutlineKind::Rule
            || line.text.trim().is_empty()
            || line.text.starts_with("## ")
            || !line.continuation.is_empty()
        {
            return None;
        }
        let record = self
            .rewrites
            .entry(identifier)
            .or_insert_with(|| RewriteRecord {
                original: None,
                inserted: Vec::new(),
                pending: None,
                note: None,
            });
        if record.pending.is_some() {
            return None;
        }
        self.revision += 1;
        record.pending = Some(self.revision);
        record.note = None;
        Some(RewriteTicket {
            identifier,
            revision: self.revision,
            source: line.text.clone(),
            context: self.markdown(),
        })
    }

    pub fn finish_rewrite(&mut self, ticket: &RewriteTicket, result: Result<Vec<String>, String>) {
        let Some(index) = self.index(ticket.identifier) else {
            return;
        };
        let Some(record) = self.rewrites.get_mut(&ticket.identifier) else {
            return;
        };
        // Edits, Undo and Save invalidate outstanding answers, even when text is changed back.
        if record.pending != Some(ticket.revision) || self.lines[index].text != ticket.source {
            return;
        }
        record.pending = None;
        let rules = match result {
            Ok(rules) if !rules.is_empty() => rules,
            Ok(_) => {
                record.note = Some("The model returned no rule".into());
                return;
            }
            Err(note) => {
                record.note = Some(note);
                return;
            }
        };
        if rules.len() == 1 && rules[0] == ticket.source {
            return;
        }
        if record.original.is_none() {
            record.original = Some(self.lines[index].clone());
        }
        self.lines[index].replace(rules[0].clone());
        let nested = self.lines[index].nested;
        let mut inserted = Vec::new();
        for (offset, rule) in rules.iter().skip(1).enumerate() {
            let prefix = if nested { "  - " } else { "- " };
            inserted.push(self.insert(index + offset + 1, &format!("{prefix}{rule}")));
        }
        if let Some(record) = self.rewrites.get_mut(&ticket.identifier) {
            record.inserted.extend(inserted);
        }
    }

    pub(crate) fn undo_rewrite(&mut self, identifier: u32) {
        let Some(record) = self.rewrites.remove(&identifier) else {
            return;
        };
        if let Some(index) = self.index(identifier)
            && let Some(original) = record.original
        {
            // The copy may come from a last line without a newline. A bullet added after the
            // rewrite must not join the restored wording, so keep the line's current ending.
            let ending = std::mem::take(&mut self.lines[index].ending);
            self.lines[index] = original;
            if self.lines[index].ending.is_empty() {
                self.lines[index].ending = ending;
            }
            self.lines
                .retain(|line| !record.inserted.contains(&line.identifier));
        }
        self.revision += 1;
    }

    pub(crate) fn rewrite_views(&self) -> Vec<RewriteView> {
        self.rewrites
            .iter()
            .map(|(identifier, record)| RewriteView {
                identifier: *identifier,
                original: record.original.as_ref().map(|line| line.text.clone()),
                pending: record.pending.is_some(),
                note: record.note.clone(),
            })
            .collect()
    }
}

pub fn rewrite_prompt(ticket: &RewriteTicket) -> String {
    format!(
        "Rewrite one AGENTS.md bullet as direct, explicit instructions. Use the whole file as context. Tighten hedges, expand short notes, split independent rules when needed. Preserve meaning and commands. If vague intent cannot be inferred, return FLAG: followed by what is missing. Otherwise return only one or more single-line rules, each prefixed by '- '. Do not edit files or run commands; return the proposed text.\nWhole file (context only):\n{}\nBullet to rewrite:\n{}",
        ticket.context, ticket.source
    )
}

pub fn parse_rewrite(response: &str) -> Result<Vec<String>, String> {
    let response = response.trim();
    if let Some(note) = response.strip_prefix("FLAG:") {
        return Err(note.trim().into());
    }
    // Some adapters prepend a sentence even when asked for bullets only. Accept that
    // introduction, but require every nonblank line after the first bullet to be a rule.
    let rules: Option<Vec<String>> = response
        .lines()
        .skip_while(|line| !line.starts_with("- "))
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.strip_prefix("- ")
                .filter(|text| !text.trim().is_empty())
                .map(str::to_owned)
        })
        .collect();
    rules
        .filter(|rules| !rules.is_empty() && rules.len() <= 8)
        .ok_or_else(|| "The model returned an invalid rewrite; your wording is kept".into())
}
