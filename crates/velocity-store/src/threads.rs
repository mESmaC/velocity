//! Thread records for threads Velocity starts and runs (SPEC.md §2.1, §2.2, F4, F5).
//! Each change is written to `state.json` before the main window sees it.

use thiserror::Error;
use velocity_core::layout_state::LayoutState;

use crate::{
    AgentActivity, EffortLevel, ProjectItem, ProviderKind, SpeedMode, StateStore, StoreError,
    TaskState, ThreadItem, ThreadState, WorktreeRole,
};

#[derive(Debug, Error)]
pub enum ThreadError {
    #[error("Project {0} is not registered")]
    UnknownProject(u32),
    #[error("Thread {thread} is not in project {project}")]
    UnknownThread { project: u32, thread: u32 },
    #[error(transparent)]
    Persistence(#[from] StoreError),
}

/// What a new build thread starts with (F4): its title, branch and fixed worktree path (F5),
/// and the provider settings it runs on (F15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewThread {
    pub title: String,
    pub branch_name: String,
    pub worktree_path: String,
    pub provider: ProviderKind,
    pub model_name: String,
    pub model_supports_fast: bool,
    pub effort: EffortLevel,
    pub preferred_speed: SpeedMode,
}

impl StateStore {
    pub fn active_project(&self) -> Option<&ProjectItem> {
        crate::project_layout::active_project(&self.state)
    }

    pub fn project(&self, project: u32) -> Option<&ProjectItem> {
        self.state.projects.iter().find(|item| item.id == project)
    }

    /// Records a `working` thread and makes it the project's active thread.
    pub fn add_thread(&mut self, project: u32, thread: NewThread) -> Result<u32, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let identifier = item
            .threads
            .iter()
            .map(|existing| existing.id)
            .max()
            .map_or(1, |highest| highest.saturating_add(1));
        let record = ThreadItem {
            id: identifier,
            title: thread.title,
            state: ThreadState::Working,
            activity: AgentActivity::Executing,
            branch_name: thread.branch_name,
            worktree_path: Some(thread.worktree_path),
            // A thread started from the composer builds in its own worktree (F4, F5).
            worktree_role: WorktreeRole::Thread,
            worktree_path_id: None,
            pull_request_number: None,
            review_stage: None,
            task_ids: Vec::new(),
            additions: 0,
            deletions: 0,
            elapsed_seconds: 0,
            provider: thread.provider,
            model_name: thread.model_name,
            effort: thread.effort,
            preferred_speed: thread.preferred_speed,
            effective_speed: thread.preferred_speed,
            model_supports_fast: thread.model_supports_fast,
        }
        .normalize();
        // Newest first, as the reference's sidebar lists new threads at the top of their group.
        item.threads.insert(0, record);
        item.active_thread_id = Some(identifier);
        self.commit(next)?;
        Ok(identifier)
    }

    fn commit(&mut self, mut next: crate::PersistedState) -> Result<(), StoreError> {
        next.revision = self.state.revision.wrapping_add(1);
        crate::write_atomic_snapshot(&self.directory, &next)?;
        self.disk_revision = next.revision;
        self.state = next;
        Ok(())
    }

    /// Applies `change` to one thread and saves it when anything changed.
    pub fn update_thread(
        &mut self,
        project: u32,
        thread: u32,
        change: impl FnOnce(&mut ThreadItem),
    ) -> Result<bool, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?
            .threads
            .iter_mut()
            .find(|item| item.id == thread)
            .ok_or(ThreadError::UnknownThread { project, thread })?;
        let before = item.clone();
        change(item);
        if *item == before {
            return Ok(false);
        }
        self.commit(next)?;
        Ok(true)
    }

    /// Removes a deleted thread's record once its undo window has passed (F12). Tasks it
    /// was working on go back to todo for someone else to pick up, and it stops being the
    /// project's active thread. Returns the removed record.
    pub fn remove_thread(&mut self, project: u32, thread: u32) -> Result<ThreadItem, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let position = item
            .threads
            .iter()
            .position(|record| record.id == thread)
            .ok_or(ThreadError::UnknownThread { project, thread })?;
        let removed = item.threads.remove(position);
        for task in &mut item.tasks {
            if task.assigned_thread_id == Some(thread) || removed.task_ids.contains(&task.id) {
                task.assigned_thread_id = None;
                if task.state == TaskState::Now {
                    task.state = TaskState::Todo;
                }
            }
        }
        if item.active_thread_id == Some(thread) {
            item.active_thread_id = None;
        }
        self.commit(next)?;
        Ok(removed)
    }

    /// Rebuilds the main window's rows from the saved state, keeping the current view.
    pub fn refresh_layout(&self, layout: &mut LayoutState) {
        layout.replace_data(crate::project_layout::read_model(&self.state));
        crate::build_gate::sync_layout(&self.state, layout);
    }
}
