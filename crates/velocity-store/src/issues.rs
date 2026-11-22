//! Persisted issue ownership (F9). A failed or closed thread retains its issue;
//! deleting the thread drops the association with the record (F12).
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{AgentActivity, ProjectItem, ThreadItem, ThreadState};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ThreadIssue {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub number: u32,
    /// Issue title when assigned, used to detect number collisions after a host switch (#614).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Issue host where this issue originated (`github` or `collapse`), when known (#614).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<velocity_core::collapse::IssueHost>,
}

impl ThreadIssue {
    pub fn new(repository: String, number: u32) -> Self {
        Self {
            repository,
            number,
            title: None,
            host: None,
        }
    }

    /// Attaches the issue's title and originating host so post-land comments, closes and
    /// dispatch skip checks can verify identity across host switches (#614).
    pub fn with_identity(
        mut self,
        title: impl Into<String>,
        host: velocity_core::collapse::IssueHost,
    ) -> Self {
        let title = title.into();
        self.title = (!title.trim().is_empty()).then_some(title);
        self.host = Some(host);
        self
    }
}

pub fn issue_thread<'a>(project: &'a ProjectItem, issue: &ThreadIssue) -> Option<&'a ThreadItem> {
    project.threads.iter().find(|thread| {
        !thread.supervision.released
            && thread.issue.as_ref().is_some_and(|assigned| {
                assigned.number == issue.number
                    && assigned.repository.eq_ignore_ascii_case(&issue.repository)
            })
            && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
    })
}

pub fn issue_thread_label(thread: &ThreadItem) -> String {
    match thread.state {
        ThreadState::Review => format!(
            "#{} · {}",
            thread.pull_request_number.unwrap_or_default(),
            match thread.review_stage {
                Some(crate::ReviewStage::Fixing) => "addressing review",
                Some(crate::ReviewStage::Approved) => "approved · landing",
                Some(crate::ReviewStage::ContinuousIntegration) => "checking",
                _ => "in review",
            }
        ),
        ThreadState::Error => "failed · kept".into(),
        ThreadState::Closed => "abandoned · kept".into(),
        ThreadState::Landed => "landed".into(),
        ThreadState::Working if thread.activity == AgentActivity::Idle => "agent idle".into(),
        _ => "agent working".into(),
    }
}
