//! Persisted issue ownership (F9). A failed or closed thread retains its issue;
//! deleting the thread drops the association with the record (F12).
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{ProjectItem, ThreadItem, ThreadState};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ThreadIssue {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub number: u32,
}

impl ThreadIssue {
    pub fn new(repository: String, number: u32) -> Self {
        Self { repository, number }
    }
}

pub fn issue_thread<'a>(project: &'a ProjectItem, issue: &ThreadIssue) -> Option<&'a ThreadItem> {
    project.threads.iter().find(|thread| {
        thread.issue.as_ref().is_some_and(|assigned| {
            assigned.number == issue.number
                && assigned.repository.eq_ignore_ascii_case(&issue.repository)
        }) && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
    })
}

pub fn issue_thread_label(thread: &ThreadItem) -> String {
    match thread.state {
        ThreadState::Review => format!(
            "#{} · {}",
            thread.pull_request_number.unwrap_or_default(),
            match thread.review_stage {
                Some(crate::ReviewStage::Fixing) => "addressing review",
                Some(crate::ReviewStage::Approved) => "approved · merging",
                Some(crate::ReviewStage::ContinuousIntegration) => "checking",
                _ => "in review",
            }
        ),
        ThreadState::Error => "failed · kept".into(),
        ThreadState::Closed => "closed · kept".into(),
        ThreadState::Merged => "merged".into(),
        _ => "agent working".into(),
    }
}
