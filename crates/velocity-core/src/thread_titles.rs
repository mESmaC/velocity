//! Once-only thread title selection (SPEC.md F4).
//!
//! The caller owns provider requests, timers and worktree setup. Generic title
//! values keep this policy independent of transport and persisted thread data.

/// A late response must not rename the branch after the fallback was selected.
pub const TITLE_DEADLINE_MILLISECONDS: u64 = 2_000;

#[derive(Debug, PartialEq, Eq)]
pub enum TitleSource<Title> {
    Message(Title),
    Item(Title),
}

#[derive(Debug, PartialEq, Eq)]
enum Selection<Title> {
    Pending,
    Fallback,
    Model(Title),
}

/// Once settled, the selected value survives failures and late model callbacks.
#[derive(Debug, PartialEq, Eq)]
pub struct ThreadTitle<Title> {
    fallback: Title,
    selection: Selection<Title>,
}

impl<Title> ThreadTitle<Title> {
    /// Item-backed launches never request a title model.
    pub fn new(source: TitleSource<Title>, model_available: bool) -> Self {
        match source {
            TitleSource::Message(first_message) => Self {
                fallback: first_message,
                selection: if model_available {
                    Selection::Pending
                } else {
                    Selection::Fallback
                },
            },
            TitleSource::Item(title) => Self {
                fallback: title,
                selection: Selection::Fallback,
            },
        }
    }

    /// Only a new pending title needs a model request; settling never retries it.
    pub fn is_pending(&self) -> bool {
        matches!(self.selection, Selection::Pending)
    }

    /// Pass `None` on failure or when the two-second deadline expires. Elapsed
    /// time also rejects late responses when a delayed timer has not fired yet.
    pub fn settle(self, response: Option<Title>, elapsed_milliseconds: u64) -> Self {
        if !self.is_pending() {
            return self;
        }
        let selection = if elapsed_milliseconds <= TITLE_DEADLINE_MILLISECONDS {
            match response {
                Some(title) => Selection::Model(title),
                None => Selection::Fallback,
            }
        } else {
            Selection::Fallback
        };
        Self {
            fallback: self.fallback,
            selection,
        }
    }

    /// Pending titles display the first message until selection settles.
    pub fn displayed_title(&self) -> &Title {
        match &self.selection {
            Selection::Model(title) => title,
            Selection::Pending | Selection::Fallback => &self.fallback,
        }
    }

    /// Naming must wait for this value, so a late model cannot rename a running
    /// thread's branch or move its worktree.
    pub fn final_title(&self) -> Option<&Title> {
        if self.is_pending() {
            None
        } else {
            Some(self.displayed_title())
        }
    }

    /// Readiness is supplied by the worktree manager only after successful setup
    /// and application of the final title-derived branch and directory names.
    pub fn can_start(&self, worktree_ready: bool, name_applied: bool) -> bool {
        !self.is_pending() && worktree_ready && name_applied
    }
}
