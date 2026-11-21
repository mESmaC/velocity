//! Short per-thread session card injected once at session start (#534).
//! Static rules live in AGENTS.md; this card holds only values that change per thread.

use std::fmt::Write;
use std::path::PathBuf;

/// Dynamic per-thread values rendered once ahead of a fresh session's opening prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionCard {
    /// Agent nick / member identity (e.g. `issue-534-5 (thread-269)`).
    pub identity: Option<String>,
    /// Workspace path on disk.
    pub workspace: Option<PathBuf>,
    /// Work item or issue label (e.g. `mESmaC/velocity#534 (name snapshot with \`(#534)\`)`).
    pub work_item: Option<String>,
    /// Previous sequential dispatch item when continuing a handoff.
    pub previous_item: Option<String>,
    /// Rosen change ID, or git branch name for legacy git worktrees.
    pub change: Option<String>,
    /// True for a Rosen workspace; false for a git worktree.
    pub is_rosen: bool,
    /// Current base tip commit SHA (rendered shortened to 12 hex characters).
    pub base_tip: Option<String>,
    /// Held AIRC / Rosen claims and open dependency waits.
    pub held_claims: Vec<String>,
    /// Overlapping active Rosen changes (`change (owner): paths`).
    pub overlapping_changes: Vec<String>,
    /// Current machine-wide queue depth (light slots + run queue).
    pub queue_depth: Option<String>,
    /// Expected session tools that are unavailable (e.g. `velocity-run`, `velocity-graph`).
    pub unavailable_tools: Vec<String>,
    /// Provider-specific runtime notes that do not belong in project AGENTS.md.
    pub notes: Vec<String>,
}

impl SessionCard {
    pub fn push_claim(&mut self, claim: impl Into<String>) {
        let claim = claim.into();
        let trimmed = claim.trim();
        if !trimmed.is_empty() && !self.held_claims.iter().any(|existing| existing == trimmed) {
            self.held_claims.push(trimmed.to_owned());
        }
    }

    pub fn push_overlap(&mut self, overlap: impl Into<String>) {
        let overlap = overlap.into();
        let trimmed = overlap.trim();
        if !trimmed.is_empty()
            && !self
                .overlapping_changes
                .iter()
                .any(|existing| existing == trimmed)
        {
            self.overlapping_changes.push(trimmed.to_owned());
        }
    }

    pub fn push_unavailable_tool(&mut self, tool: impl Into<String>) {
        let tool = tool.into();
        let trimmed = tool.trim();
        if !trimmed.is_empty()
            && !self
                .unavailable_tools
                .iter()
                .any(|existing| existing == trimmed)
        {
            self.unavailable_tools.push(trimmed.to_owned());
        }
    }

    pub fn push_note(&mut self, note: impl Into<String>) {
        let note = note.into();
        let trimmed = note.trim();
        if !trimmed.is_empty() && !self.notes.iter().any(|existing| existing == trimmed) {
            self.notes.push(trimmed.to_owned());
        }
    }

    /// Renders the card as a compact block holding only dynamic per-thread values.
    pub fn render(&self) -> String {
        let mut out = String::from("[Velocity session]");
        if let Some(line) = self.identity_line() {
            let _ = write!(out, "\nIdentity: {line}");
        }
        if let Some(item) = non_empty(self.work_item.as_deref()) {
            let _ = write!(out, "\nWork item: {item}");
        }
        if let Some(previous) = non_empty(self.previous_item.as_deref()) {
            let _ = write!(out, "\nPrevious item: {previous}");
        }
        if let Some(line) = self.change_line() {
            let _ = write!(out, "\nChange: {line}");
        }
        let claims = list_or_none(&self.held_claims);
        let _ = write!(out, "\nHeld claims: {claims}");
        let overlaps = list_or_none(&self.overlapping_changes);
        let _ = write!(out, "\nOverlapping changes: {overlaps}");
        if let Some(queue) = non_empty(self.queue_depth.as_deref()) {
            let _ = write!(out, "\nQueue depth: {queue}");
        }
        let unavailable = list_or_none(&self.unavailable_tools);
        let _ = write!(out, "\nUnavailable tools: {unavailable}");
        for note in &self.notes {
            let _ = write!(out, "\nNote: {note}");
        }
        out
    }

    fn identity_line(&self) -> Option<String> {
        match (
            non_empty(self.identity.as_deref()),
            self.workspace.as_deref(),
        ) {
            (Some(identity), Some(workspace)) => {
                Some(format!("{identity} · workspace {}", workspace.display()))
            }
            (Some(identity), None) => Some(identity.to_owned()),
            (None, Some(workspace)) => Some(format!("workspace {}", workspace.display())),
            (None, None) => None,
        }
    }

    fn change_line(&self) -> Option<String> {
        let base = non_empty(self.base_tip.as_deref()).map(short_commit);
        match (non_empty(self.change.as_deref()), base) {
            (Some(change), Some(tip)) => {
                let kind = if self.is_rosen { "Rosen" } else { "git" };
                Some(format!("{change} ({kind}) · base {tip}"))
            }
            (Some(change), None) => {
                let kind = if self.is_rosen { "Rosen" } else { "git" };
                Some(format!("{change} ({kind})"))
            }
            (None, Some(tip)) => Some(format!("base {tip}")),
            (None, None) => None,
        }
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join("; ")
    }
}

fn short_commit(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

/// Joins adjacent text content blocks with a blank line so ACP adapters that concatenate
/// consecutive text blocks never glue sentences together ("next prompt.Use the workspace's") (#534).
pub(super) fn join_adjacent_text_blocks(
    blocks: Vec<agent_client_protocol::v1::schema::ContentBlock>,
) -> Vec<agent_client_protocol::v1::schema::ContentBlock> {
    use agent_client_protocol::v1::schema::ContentBlock;

    let mut out: Vec<ContentBlock> = Vec::with_capacity(blocks.len());
    for block in blocks {
        match (out.last_mut(), block) {
            (Some(ContentBlock::Text(previous)), ContentBlock::Text(next)) => {
                let left = previous.text.trim_end_matches(['\r', '\n']);
                let right = next.text.trim_start_matches(['\r', '\n']);
                if right.is_empty() {
                    continue;
                }
                if left.is_empty() {
                    previous.text = right.to_owned();
                } else {
                    previous.text = format!("{left}\n\n{right}");
                }
            }
            (_, other) => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v1::schema::ContentBlock;

    #[test]
    fn session_card_renders_dynamic_fields_without_static_rules() {
        let mut card = SessionCard {
            identity: Some("issue-534-5 (thread-269)".into()),
            workspace: Some(PathBuf::from(r"Z:\velocity-work\velocity\issue-534-5")),
            work_item: Some("mESmaC/velocity#534 (name snapshot with `(#534)`)".into()),
            change: Some("vlkmxsurpnzsoqprtylnrmyzytoqrxox".into()),
            is_rosen: true,
            base_tip: Some("f683eed6d8dbcd5739a6499b071302833cae911e".into()),
            queue_depth: Some("0/2 light slots busy, 0 running, 0 waiting".into()),
            ..SessionCard::default()
        };
        card.push_claim("issue:534 (paths: none)");
        card.push_overlap("vpxuvrps (issue-531-5): AGENTS.md");
        card.push_unavailable_tool("velocity-browser");
        let rendered = card.render();
        assert!(rendered.contains("Identity: issue-534-5 (thread-269)"));
        assert!(
            rendered
                .contains("Change: vlkmxsurpnzsoqprtylnrmyzytoqrxox (Rosen) · base f683eed6d8db")
        );
        assert!(rendered.contains("Held claims: issue:534 (paths: none)"));
        assert!(rendered.contains("Overlapping changes: vpxuvrps (issue-531-5): AGENTS.md"));
        assert!(rendered.contains("Queue depth: 0/2 light slots busy, 0 running, 0 waiting"));
        assert!(rendered.contains("Unavailable tools: velocity-browser"));
        assert!(!rendered.contains("stress runs"));
    }

    #[test]
    fn adjacent_text_blocks_join_with_blank_line() {
        let joined = join_adjacent_text_blocks(vec![
            ContentBlock::from("Velocity sends the result in your next prompt."),
            ContentBlock::from("Use the workspace's velocity-graph tools first."),
        ]);
        assert_eq!(joined.len(), 1);
        let ContentBlock::Text(text) = &joined[0] else {
            panic!("expected merged text block");
        };
        assert_eq!(
            text.text,
            "Velocity sends the result in your next prompt.\n\nUse the workspace's velocity-graph tools first."
        );
    }
}
