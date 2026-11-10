//! Thread records for threads Velocity starts and runs (SPEC.md §2.1, §2.2, F4, F5).
//! Each change is written to `state.json` before the main window sees it.

use thiserror::Error;

use crate::{
    EffortLevel, ProjectItem, ProviderKind, SpeedMode, StateStore, StoreError, TaskState,
    ThreadItem,
};

#[derive(Debug, Error)]
pub enum ThreadError {
    #[error("This project has exhausted its thread identifiers")]
    IdentifiersExhausted,
    #[error("Project {0} is not registered")]
    UnknownProject(u32),
    #[error("Thread {thread} is not in project {project}")]
    UnknownThread { project: u32, thread: u32 },
    #[error("Issue #{number} already belongs to thread {thread}")]
    IssueAssigned { number: u32, thread: u32 },
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

impl NewThread {
    pub fn new(
        title: String,
        branch_name: String,
        worktree_path: String,
        settings: &crate::Settings,
    ) -> Self {
        Self {
            title,
            branch_name,
            worktree_path,
            provider: settings.providers.default_provider,
            model_name: settings
                .providers
                .default_model
                .clone()
                .unwrap_or_else(|| "agent default".into()),
            model_supports_fast: false,
            effort: settings.providers.default_effort,
            preferred_speed: settings.providers.default_speed,
        }
    }
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
        self.add_linked_thread(project, thread, None)
    }

    pub fn add_issue_thread(
        &mut self,
        project: u32,
        thread: NewThread,
        issue: crate::issues::ThreadIssue,
    ) -> Result<u32, ThreadError> {
        self.add_linked_thread(project, thread, Some(issue))
    }

    fn add_linked_thread(
        &mut self,
        project: u32,
        thread: NewThread,
        issue: Option<crate::issues::ThreadIssue>,
    ) -> Result<u32, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if let Some(issue) = &issue
            && let Some(existing) = crate::issues::issue_thread(item, issue)
        {
            return Err(ThreadError::IssueAssigned {
                number: issue.number,
                thread: existing.id,
            });
        }
        let highest_thread = item
            .threads
            .iter()
            .map(|existing| existing.id)
            .max()
            .unwrap_or(0);
        // Duty roles share the project's entry identifiers with threads (F7), and a deleted
        // thread's identifier is never handed out again (#65).
        let highest_duty = item
            .duty_roles
            .iter()
            .map(|duty| duty.id)
            .max()
            .unwrap_or(0);
        let identifier = highest_thread
            .max(highest_duty)
            .checked_add(1)
            .ok_or(ThreadError::IdentifiersExhausted)?
            .max(*next.next_thread_identifiers.get(&project).unwrap_or(&1));
        next.next_thread_identifiers.insert(
            project,
            identifier
                .checked_add(1)
                .ok_or(ThreadError::IdentifiersExhausted)?,
        );
        next.thread_started_at
            .entry(project)
            .or_default()
            .insert(identifier, thread_start_timestamp());
        let mut record = ThreadItem::for_build(thread, identifier);
        record.issue = issue;
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
        if removed.state == crate::ThreadState::Merged
            && let Some(section) = &removed.spec_section
            && !item.built_spec_sections.contains(section)
        {
            item.built_spec_sections.push(section.clone());
        }
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
        if let Some(started) = next.thread_started_at.get_mut(&project) {
            started.remove(&thread);
        }
        self.commit(next)?;
        Ok(removed)
    }
}

fn thread_start_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}
