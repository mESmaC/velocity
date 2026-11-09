//! Persist watcher cursors, outbox and thread lifecycle together, before publication.
use crate::{AgentActivity, ReviewStage, StateStore, StoreError, ThreadState};
use velocity_github::{PullRequestSnapshot, PullRequestState, follow_through::FollowThrough};

impl StateStore {
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
