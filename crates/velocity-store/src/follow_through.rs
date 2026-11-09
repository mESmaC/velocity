//! Persist watcher cursors, outbox and thread lifecycle together, before publication.
use crate::{
    AgentActivity, ProjectItem, ReviewStage, StateStore, StoreError, ThreadItem, ThreadState,
};
use velocity_github::{PullRequestSnapshot, PullRequestState, follow_through::FollowThrough};

#[derive(Debug, thiserror::Error)]
pub enum FollowThroughError {
    #[error("Project {0} is not registered")]
    UnknownProject(u32),
    #[error("The thread no longer exists in this project")]
    MissingThread,
    #[error("This project's spec must be locked before its agent starts")]
    UnlockedSpec,
    #[error("The thread has no retained worktree")]
    MissingWorktree,
}

impl StateStore {
    pub fn follow_through_observer(&self, project: u32, thread: u32) -> Option<FollowThrough> {
        let record = self
            .project(project)?
            .threads
            .iter()
            .find(|record| record.id == thread)?;
        Some(record.follow_through.clone().unwrap_or_else(|| {
            self.state
                .thread_started_at
                .get(&project)
                .and_then(|threads| threads.get(&thread))
                .map_or_else(FollowThrough::default, |started| {
                    FollowThrough::for_thread(started.clone())
                })
        }))
    }

    pub fn follow_through_thread(
        &self,
        project_identifier: u32,
        thread_identifier: u32,
    ) -> Result<(ProjectItem, ThreadItem), FollowThroughError> {
        let project = self
            .project(project_identifier)
            .ok_or(FollowThroughError::UnknownProject(project_identifier))?;
        if !project.spec_locked {
            return Err(FollowThroughError::UnlockedSpec);
        }
        let thread = project
            .threads
            .iter()
            .find(|thread| thread.id == thread_identifier)
            .ok_or(FollowThroughError::MissingThread)?;
        if thread.worktree_path.is_none() {
            return Err(FollowThroughError::MissingWorktree);
        }
        Ok((project.clone(), thread.clone()))
    }

    pub fn thread_has_merged_pull_request(&self, project: u32, thread: u32) -> bool {
        self.project(project)
            .and_then(|project| project.threads.iter().find(|item| item.id == thread))
            .and_then(|thread| thread.follow_through.as_ref())
            .is_some_and(|watch| watch.stage == "merged")
    }

    pub fn refresh_follow_through_layout(
        &self,
        layout: &mut velocity_core::layout_state::LayoutState,
    ) {
        layout.replace_data(crate::project_layout::read_model(&self.state));
        crate::build_gate::sync_layout(&self.state, layout);
    }

    pub fn save_follow_through(
        &mut self,
        project_identifier: u32,
        thread_identifier: u32,
        snapshot: &PullRequestSnapshot,
        follow_through: FollowThrough,
    ) -> Result<bool, StoreError> {
        if !follow_through.should_follow(&snapshot.pull_request) {
            return Ok(false);
        }
        let mut next = self.state.clone();
        let Some(thread) = next
            .projects
            .iter_mut()
            .find(|project| project.id == project_identifier)
            .and_then(|project| {
                project
                    .threads
                    .iter_mut()
                    .find(|thread| thread.id == thread_identifier)
            })
        else {
            return Ok(false);
        };
        let previous = thread.clone();
        thread.pull_request_number = Some(snapshot.pull_request.number);
        thread.additions = snapshot.pull_request.additions;
        thread.deletions = snapshot.pull_request.deletions;
        thread.state = match snapshot.pull_request.state {
            PullRequestState::Open => ThreadState::Review,
            PullRequestState::Closed => ThreadState::Closed,
            PullRequestState::Merged if follow_through.worktree_deleted => ThreadState::Merged,
            // A merged PR is not finished until its checkout is actually gone.
            PullRequestState::Merged => ThreadState::Review,
        };
        thread.review_stage = match follow_through.stage.as_str() {
            "ci" => Some(ReviewStage::ContinuousIntegration),
            "fixing" => Some(ReviewStage::Fixing),
            "approved" | "merged" => Some(ReviewStage::Approved),
            "review" => Some(ReviewStage::Review),
            _ => None,
        };
        if thread.state == ThreadState::Merged {
            thread.activity = AgentActivity::Idle;
        }
        thread.follow_through = Some(follow_through);
        if *thread == previous {
            return Ok(false);
        }
        next.revision = next.revision.wrapping_add(1);
        crate::write_atomic_snapshot(&self.directory, &next)?;
        self.disk_revision = next.revision;
        self.state = next;
        Ok(true)
    }
}
