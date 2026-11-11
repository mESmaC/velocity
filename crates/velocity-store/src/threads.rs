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
    #[error("Task {0} is not on this project's Task List")]
    UnknownTask(u32),
    #[error("Task {0} already has an agent or is still a draft")]
    TaskUnavailable(u32),
    #[error("Spec section {0} is already claimed or built")]
    SpecUnavailable(String),
    #[error("{0}")]
    InvalidLaunch(String),
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
        // A signed-out default gives way to the first signed-in provider (F15 Sign out).
        let provider = settings.new_work_choice();
        let model_name = settings.provider_model(provider);
        Self {
            title,
            branch_name,
            worktree_path,
            provider,
            model_supports_fast: settings.model_has_fast_mode(provider, &model_name),
            model_name,
            effort: settings.providers.default_effort,
            preferred_speed: settings.providers.default_speed,
        }
    }
}

impl StateStore {
    /// Processes from a previous app lifetime are gone. Keep parked and capped recovery
    /// checkpoints, but do not report their other saved activities as live work.
    pub fn reset_disconnected_threads(&mut self) -> Result<(), StoreError> {
        let mut next = self.state.clone();
        for project in &mut next.projects {
            for thread in &mut project.threads {
                for block in &mut thread.transcript {
                    if let velocity_core::thread_view_model::TranscriptBlock::ToolGroup { steps } =
                        block
                    {
                        for step in steps {
                            step.running = false;
                        }
                    }
                }
                if thread.interview.is_none()
                    && matches!(
                        thread.activity,
                        crate::AgentActivity::Thinking
                            | crate::AgentActivity::Executing
                            | crate::AgentActivity::Stalled
                    )
                {
                    thread.activity = crate::AgentActivity::Idle;
                }
            }
        }
        if next != self.state {
            self.commit(next)?;
        }
        Ok(())
    }

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
        let identifier = insert_thread(&mut next, project, thread, issue)?;
        self.commit(next)?;
        Ok(identifier)
    }

    pub(crate) fn commit(&mut self, mut next: crate::PersistedState) -> Result<(), StoreError> {
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

/// Records a `working` thread in `next` and makes it the project's active thread.
pub(crate) fn insert_thread(
    next: &mut crate::PersistedState,
    project: u32,
    thread: NewThread,
    issue: Option<crate::issues::ThreadIssue>,
) -> Result<u32, ThreadError> {
    if let Some(issue) = &issue
        && let Some(existing) = next
            .projects
            .iter()
            .find(|item| item.id == project)
            .and_then(|item| crate::issues::issue_thread(item, issue))
    {
        return Err(ThreadError::IssueAssigned {
            number: issue.number,
            thread: existing.id,
        });
    }
    let identifier = next_entry_identifier(next, project)?;
    next.thread_started_at
        .entry(project)
        .or_default()
        .insert(identifier, thread_start_timestamp());
    let item = next
        .projects
        .iter_mut()
        .find(|item| item.id == project)
        .ok_or(ThreadError::UnknownProject(project))?;
    let mut record = ThreadItem::for_build(thread, identifier);
    record.issue = issue;
    // Newest first, as the reference's sidebar lists new threads at the top of their group.
    item.threads.insert(0, record);
    item.active_thread_id = Some(identifier);
    Ok(identifier)
}

/// The next sidebar identifier for `project`. Threads, duty roles and swarms share one
/// space (F7, F10), and a deleted thread's identifier is never handed out again (#65).
pub(crate) fn next_entry_identifier(
    next: &mut crate::PersistedState,
    project: u32,
) -> Result<u32, ThreadError> {
    let item = next
        .projects
        .iter()
        .find(|item| item.id == project)
        .ok_or(ThreadError::UnknownProject(project))?;
    let identifier = highest_entry_identifier(item)
        .checked_add(1)
        .ok_or(ThreadError::IdentifiersExhausted)?
        .max(*next.next_thread_identifiers.get(&project).unwrap_or(&1));
    next.next_thread_identifiers.insert(
        project,
        identifier
            .checked_add(1)
            .ok_or(ThreadError::IdentifiersExhausted)?,
    );
    Ok(identifier)
}

pub(crate) fn highest_entry_identifier(project: &ProjectItem) -> u32 {
    let threads = project.threads.iter().map(|thread| thread.id);
    let duty = project.duty_roles.iter().map(|duty| duty.id);
    let swarms = project.swarms.iter().map(|swarm| swarm.id);
    let goal_swarms = project.goal_swarms.iter().map(|swarm| swarm.id);
    threads
        .chain(duty)
        .chain(swarms)
        .chain(goal_swarms)
        .max()
        .unwrap_or(0)
}

fn thread_start_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}
