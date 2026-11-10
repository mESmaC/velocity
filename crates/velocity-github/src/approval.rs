//! Velocity's own approval record (F7, answers Q28).
//!
//! Every agent shares the user's one GitHub login, and GitHub refuses a review
//! that approves the login's own PR. So the approval Rule 4 requires is an
//! `approved` AIRC event naming the PR, the reviewer's nick and the head commit
//! it approved. GitHub gets a mirror comment for people reading the PR; its
//! review state is advisory, like CI.
use std::path::Path;

use velocity_airc::{Channel, Entry, Event};
use velocity_core::pr_review::ReviewStanding;

use crate::PullRequestError;

pub struct ReviewRecord {
    channel: Channel,
    nick: String,
}

impl ReviewRecord {
    /// Opens the project's AIRC channel for the reviewer with this nick
    /// (`you` for the user, a PR duty nick such as `pr-1` for an agent).
    pub fn open(app_data: &Path, project: &str, nick: &str) -> Result<Self, PullRequestError> {
        if nick.trim().is_empty() {
            return Err(PullRequestError::Invalid(
                "Reviewer nick cannot be empty".into(),
            ));
        }
        Ok(Self {
            channel: Channel::open(app_data, project)?,
            nick: nick.into(),
        })
    }

    pub(crate) fn nick(&self) -> &str {
        &self.nick
    }

    pub(crate) fn approve(&self, number: u32, head: &str) -> Result<(), PullRequestError> {
        let text = format!("approved #{number} at {}", short_head(head));
        self.post("approved", number, Some(head), &text)
    }

    pub(crate) fn request_changes(&self, number: u32, head: &str) -> Result<(), PullRequestError> {
        let text = format!("requested changes on #{number} at {}", short_head(head));
        self.post("changes", number, Some(head), &text)
    }

    pub(crate) fn merge_failed(
        &self,
        number: u32,
        head: &str,
        reason: &str,
    ) -> Result<(), PullRequestError> {
        // An event is one line; GitHub errors often span several.
        let reason: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
        let reason: String = reason.chars().take(300).collect();
        let text = format!("merge of #{number} failed: {reason}");
        self.post("failed", number, Some(head), &text)
    }

    /// The nick whose approval covers `head`, read afresh from the channel.
    pub(crate) fn approver(&self, number: u32, head: &str) -> Result<String, PullRequestError> {
        approved_reviewer(&self.channel.read(0)?, number, head)
    }

    fn post(
        &self,
        tag: &str,
        number: u32,
        head: Option<&str>,
        text: &str,
    ) -> Result<(), PullRequestError> {
        let event = Event::new(tag, &self.nick, text).for_pull_request(number, head);
        self.channel.post_event(event)?;
        Ok(())
    }
}

/// Folds the PR's review events in channel order. An `approved` event sets the
/// approval to its head; any later `changes` event clears it. The merge is
/// allowed only when the surviving approval names exactly the head being
/// merged, so a commit pushed after approval needs a new review.
pub(crate) fn approved_reviewer(
    entries: &[Entry],
    number: u32,
    head: &str,
) -> Result<String, PullRequestError> {
    let mut approval: Option<&Entry> = None;
    for entry in entries
        .iter()
        .filter(|entry| entry.pull_request == Some(number))
    {
        match entry.kind.as_str() {
            "approved" => approval = Some(entry),
            "changes" => approval = None,
            _ => {}
        }
    }
    match approval {
        Some(entry) if entry.head.as_deref() == Some(head) => Ok(entry.nick.clone()),
        Some(entry) => Err(PullRequestError::Invalid(format!(
            "#{number} was approved at {}, not at {}; review the new commit",
            short_head(entry.head.as_deref().unwrap_or("an unknown head")),
            short_head(head)
        ))),
        None => Err(PullRequestError::Invalid(format!(
            "#{number} has no standing approval (none recorded, or changes requested since); approve its head first"
        ))),
    }
}

pub(crate) fn short_head(head: &str) -> &str {
    head.get(..7).unwrap_or(head)
}

/// The GitHub mirror of a recorded decision, starting `Approved at <sha7>` as
/// F7 and AGENTS.md describe, followed by the reviewer's own reason.
pub(crate) fn mirror_body(approve: bool, head: &str, nick: &str, reason: &str) -> String {
    let decision = if approve {
        "Approved"
    } else {
        "Changes requested"
    };
    format!(
        "{decision} at `{}` by {nick}\n\n{}",
        short_head(head),
        reason.trim()
    )
}

/// Where the record leaves PR `number`'s `head` (F7): the fold [`approved_reviewer`] uses,
/// plus a `changes` or a refused merge (`failed`) at this head, so PR duty knows whether the
/// head needs a review, a merge, or nothing until its author pushes.
pub fn review_standing(entries: &[Entry], number: u32, head: &str) -> ReviewStanding {
    let mut approval: Option<&Entry> = None;
    let mut changes_at_head = false;
    let mut merge_failed = false;
    for entry in entries
        .iter()
        .filter(|entry| entry.pull_request == Some(number))
    {
        let at_head = entry.head.as_deref() == Some(head);
        match entry.kind.as_str() {
            "approved" => {
                approval = Some(entry);
                changes_at_head = false;
                merge_failed = false;
            }
            "changes" => {
                approval = None;
                changes_at_head = at_head;
            }
            "failed" if approval.is_some() && at_head => merge_failed = true,
            _ => {}
        }
    }
    match approval {
        Some(entry) if entry.head.as_deref() == Some(head) && merge_failed => {
            ReviewStanding::MergeFailed
        }
        Some(entry) if entry.head.as_deref() == Some(head) => ReviewStanding::Approved,
        _ if changes_at_head => ReviewStanding::ChangesRequested,
        _ => ReviewStanding::Unreviewed,
    }
}

impl ReviewRecord {
    /// Where the record leaves each `(PR, head)`, from one fresh read of the channel.
    pub fn standings(
        &self,
        heads: &[(u32, &str)],
    ) -> Result<Vec<ReviewStanding>, PullRequestError> {
        let entries = self.channel.read(0)?;
        Ok(heads
            .iter()
            .map(|(number, head)| review_standing(&entries, *number, head))
            .collect())
    }

    /// Posts this reviewer's `claim` on PR `number` at `head` (F7: one PR per agent).
    pub fn claim(&self, number: u32, head: &str) -> Result<(), PullRequestError> {
        let text = format!("claimed #{number} for review at {}", short_head(head));
        self.post("claim", number, Some(head), &text)
    }

    /// Posts that this reviewer let PR `number` go.
    pub fn release(&self, number: u32) -> Result<(), PullRequestError> {
        let text = format!("released #{number}");
        self.post("released", number, None, &text)
    }
}
