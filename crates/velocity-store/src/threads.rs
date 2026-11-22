//! Thread records for threads Velocity starts and runs (SPEC.md §2.1, §2.2, F4, F5).
//! Each change is written to `state.json` before the main window sees it.

use thiserror::Error;

use crate::provider_views::ModelMenuChoice;
use crate::{
    EffortLevel, ProjectItem, ProviderKind, Settings, SpeedMode, StateStore, StoreError, TaskState,
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
    #[error("Issue #{0} already has a landed Rosen change")]
    IssueCompleted(u32),
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

/// The resolved model settings shared by the composer, its menu and the launch plan.
pub struct ThreadModel {
    pub(crate) provider: ProviderKind,
    pub(crate) model: String,
    pub(crate) preferred_account: Option<u32>,
    pub(crate) effort: EffortLevel,
    pub(crate) speed: SpeedMode,
}

impl ThreadModel {
    pub fn from_settings(settings: &Settings) -> Self {
        DraftThreadModel::default().resolve(settings)
    }
}

#[derive(Default)]
pub(crate) struct DraftThreadModel {
    provider: Option<ProviderKind>,
    model: Option<String>,
    account: Option<u32>,
    effort: Option<EffortLevel>,
    speed: Option<SpeedMode>,
}

impl DraftThreadModel {
    fn select_provider(&mut self, provider: ProviderKind, settings: &Settings) {
        if provider != self.provider.unwrap_or_else(|| settings.new_work_choice()) {
            self.model = None;
            self.account = None;
        }
        self.provider = Some(provider);
    }

    fn resolve(&self, settings: &Settings) -> ThreadModel {
        let provider = self.provider.unwrap_or_else(|| settings.new_work_choice());
        ThreadModel {
            provider,
            model: self
                .model
                .clone()
                .unwrap_or_else(|| settings.provider_model(provider)),
            preferred_account: self.account,
            effort: self.effort.unwrap_or(settings.providers.default_effort),
            speed: self.speed.unwrap_or(settings.providers.default_speed),
        }
    }
}

/// What a new build thread starts with (F4): its title, branch and fixed worktree path (F5),
/// and the provider settings it runs on (F15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewThread {
    pub title: String,
    pub branch_name: String,
    pub worktree_path: String,
    pub role: velocity_core::tool_policy::ThreadRole,
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
        settings: &Settings,
    ) -> Self {
        Self::with_model(
            title,
            branch_name,
            worktree_path,
            settings,
            ThreadModel::from_settings(settings),
        )
    }

    pub fn with_model(
        title: String,
        branch_name: String,
        worktree_path: String,
        settings: &Settings,
        model: ThreadModel,
    ) -> Self {
        Self {
            title,
            branch_name,
            worktree_path,
            role: velocity_core::tool_policy::ThreadRole::Builder,
            provider: model.provider,
            model_supports_fast: settings.model_has_fast_mode(model.provider, &model.model),
            model_name: model.model,
            effort: model.effort,
            preferred_speed: model.speed,
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
                if thread.pending_rosen_launch.is_some()
                    && thread.state == crate::ThreadState::Working
                {
                    thread.state = crate::ThreadState::Error;
                }
                // Copied only when a step needs clearing: the transcript is shared (#319).
                if thread.transcript.iter().any(has_running_step) {
                    for block in std::sync::Arc::make_mut(&mut thread.transcript) {
                        if let velocity_core::thread_view_model::TranscriptBlock::ToolGroup {
                            steps,
                        } = block
                        {
                            for step in steps {
                                step.running = false;
                            }
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

    pub fn composer_thread_model(&self, project: u32) -> ThreadModel {
        self.composer_models.get(&project).map_or_else(
            || ThreadModel::from_settings(&self.state.settings),
            |draft| draft.resolve(&self.state.settings),
        )
    }

    pub fn choose_composer_model(
        &mut self,
        project: u32,
        choice: ModelMenuChoice,
    ) -> Result<(), ThreadError> {
        if self.project(project).is_none() {
            return Err(ThreadError::UnknownProject(project));
        }
        let draft = self.composer_models.entry(project).or_default();
        match choice {
            ModelMenuChoice::Model { provider, model } => {
                draft.select_provider(provider, &self.state.settings);
                draft.model = Some(model);
            }
            ModelMenuChoice::Account { provider, account } => {
                draft.select_provider(provider, &self.state.settings);
                draft.account = Some(account);
            }
            ModelMenuChoice::Effort(effort) => draft.effort = Some(effort),
            ModelMenuChoice::Speed(speed) => draft.speed = Some(speed),
        }
        Ok(())
    }

    /// A recorded thread owns its model choice, including when workspace creation retries.
    pub fn add_composer_thread(
        &mut self,
        project: u32,
        thread: NewThread,
    ) -> Result<u32, ThreadError> {
        let preferred = self
            .composer_models
            .get(&project)
            .and_then(|draft| draft.account);
        let identifier = self.add_linked_thread(project, thread, None, preferred)?;
        self.composer_models.remove(&project);
        Ok(identifier)
    }

    /// Records a read-only question thread with no workspace, change, claim or landable status (#533).
    pub fn add_question_thread(
        &mut self,
        project: u32,
        thread: NewThread,
    ) -> Result<u32, ThreadError> {
        let preferred = self
            .composer_models
            .get(&project)
            .and_then(|draft| draft.account);
        let mut next = self.state.clone();
        let identifier = next_entry_identifier(&mut next, project)?;
        next.thread_started_at
            .entry(project)
            .or_default()
            .insert(identifier, thread_start_timestamp());
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let mut record = ThreadItem::for_question(thread, identifier);
        record.preferred_account = preferred;
        item.threads.insert(0, record);
        item.active_thread_id = Some(identifier);
        self.commit(next)?;
        self.composer_models.remove(&project);
        Ok(identifier)
    }

    /// Records a `working` thread and makes it the project's active thread.
    pub fn add_thread(&mut self, project: u32, thread: NewThread) -> Result<u32, ThreadError> {
        self.add_linked_thread(project, thread, None, None)
    }

    pub fn add_issue_thread(
        &mut self,
        project: u32,
        thread: NewThread,
        issue: crate::issues::ThreadIssue,
    ) -> Result<u32, ThreadError> {
        self.add_issue_thread_for_version(
            project,
            thread,
            issue,
            crate::rosen_work::IssueVersion::default(),
        )
    }

    /// Version-aware variant of [`Self::add_issue_thread`] that accepts an open issue
    /// reopened after an earlier completed Rosen work item (#602).
    pub fn add_issue_thread_for_version(
        &mut self,
        project: u32,
        thread: NewThread,
        issue: crate::issues::ThreadIssue,
        version: crate::rosen_work::IssueVersion<'_>,
    ) -> Result<u32, ThreadError> {
        let mut next = self.state.clone();
        let identifier =
            insert_thread_for_version(&mut next, project, thread, Some(issue), version)?;
        self.commit(next)?;
        Ok(identifier)
    }

    fn add_linked_thread(
        &mut self,
        project: u32,
        thread: NewThread,
        issue: Option<crate::issues::ThreadIssue>,
        preferred: Option<u32>,
    ) -> Result<u32, ThreadError> {
        let mut next = self.state.clone();
        let identifier = insert_thread(&mut next, project, thread, issue)?;
        thread_record(&mut next, project, identifier)?.preferred_account = preferred;
        self.commit(next)?;
        Ok(identifier)
    }

    pub(crate) fn commit(&mut self, next: crate::PersistedState) -> Result<(), StoreError> {
        self.store_snapshot(next)?;
        Ok(())
    }

    /// Applies `change` to one thread and saves it when anything changed.
    pub fn update_thread(
        &mut self,
        project: u32,
        thread: u32,
        change: impl FnOnce(&mut ThreadItem),
    ) -> Result<bool, ThreadError> {
        // Changed in place: cloning the whole state on every agent burst cost more than
        // the change itself (#320). Only the one thread is copied, to see if it changed.
        let item = thread_record(&mut self.state, project, thread)?;
        let before = item.clone();
        change(item);
        if *item == before {
            return Ok(false);
        }
        if let Err(error) = self.record_change() {
            if let Ok(item) = thread_record(&mut self.state, project, thread) {
                *item = before;
            }
            return Err(error.into());
        }
        Ok(true)
    }

    /// Removes a deleted thread's record once its undo window has passed (F12). Tasks it
    /// was working on go back to todo for someone else to pick up, and it stops being the
    /// project's active thread. Returns the removed record.
    pub fn remove_thread(&mut self, project: u32, thread: u32) -> Result<ThreadItem, ThreadError> {
        let mut next = self.state.clone();
        let removed = remove_thread_record(&mut next, project, thread)?;
        self.commit(next)?;
        Ok(removed)
    }
}

fn has_running_step(block: &velocity_core::thread_view_model::TranscriptBlock) -> bool {
    matches!(
        block,
        velocity_core::thread_view_model::TranscriptBlock::ToolGroup { steps }
            if steps.iter().any(|step| step.running)
    )
}

fn thread_record(
    state: &mut crate::PersistedState,
    project: u32,
    thread: u32,
) -> Result<&mut ThreadItem, ThreadError> {
    state
        .projects
        .iter_mut()
        .find(|item| item.id == project)
        .ok_or(ThreadError::UnknownProject(project))?
        .threads
        .iter_mut()
        .find(|item| item.id == thread)
        .ok_or(ThreadError::UnknownThread { project, thread })
}

/// Takes a thread's record out of `next`, as [`StateStore::remove_thread`] describes.
pub(crate) fn remove_thread_record(
    next: &mut crate::PersistedState,
    project: u32,
    thread: u32,
) -> Result<ThreadItem, ThreadError> {
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
    crate::rosen_issue::retain_deleted_thread(item, &removed);
    crate::rosen_work::remove_thread(item, thread);
    for later in &mut item.threads {
        if later.previous_work_thread == Some(thread) {
            later.previous_work_thread = removed.previous_work_thread;
        }
    }
    if removed.state == crate::ThreadState::Merged
        && let Some(section) = &removed.spec_section
    {
        item.retain_deleted_thread_spec_section(section);
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
    Ok(removed)
}

/// Records a `working` thread in `next` and makes it the project's active thread.
pub(crate) fn insert_thread(
    next: &mut crate::PersistedState,
    project: u32,
    thread: NewThread,
    issue: Option<crate::issues::ThreadIssue>,
) -> Result<u32, ThreadError> {
    insert_thread_for_version(
        next,
        project,
        thread,
        issue,
        crate::rosen_work::IssueVersion::default(),
    )
}

pub(crate) fn insert_thread_for_version(
    next: &mut crate::PersistedState,
    project: u32,
    thread: NewThread,
    issue: Option<crate::issues::ThreadIssue>,
    version: crate::rosen_work::IssueVersion<'_>,
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
    if let Some(issue) = &issue
        && item.rosen_issue_completed_for_version(issue, version)
    {
        return Err(ThreadError::IssueCompleted(issue.number));
    }
    let mut record = ThreadItem::for_build(thread, identifier);
    record.issue = issue;
    // Newest first, as the reference's sidebar lists new threads at the top of their group.
    item.threads.insert(0, record);
    crate::rosen_work::track_claim(item, identifier);
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

pub(crate) fn thread_start_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewProjectRecord, ProjectLocationKind, StateStore};
    use velocity_core::tool_policy::ThreadRole;

    // Risk (#533): a read-only question thread must persist its role with no workspace,
    // Rosen change, or branch across save and reload.
    #[test]
    fn question_thread_persists_read_only_without_workspace_or_change()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "velocity-store-question-thread-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&root)?;
        let mut store = StateStore::load_or_default(&root)?;
        let project = store.commit_new_project(
            NewProjectRecord {
                name: "demo".into(),
                location_kind: ProjectLocationKind::Local,
                repository_path: root.display().to_string(),
                default_branch: "master".into(),
                github_remote: None,
            },
            None,
            true,
        )?;
        let mut thread = NewThread::new(
            "How does claim locking work?".into(),
            String::new(),
            String::new(),
            &store.settings(),
        );
        thread.role = ThreadRole::Question;
        let id = store.add_question_thread(project, thread)?;

        let reloaded = StateStore::load_or_default(&root)?;
        let saved = reloaded
            .project(project)
            .and_then(|item| item.threads.iter().find(|entry| entry.id == id))
            .ok_or("saved question thread missing")?;
        assert_eq!(saved.role, ThreadRole::Question);
        assert!(saved.worktree_path.is_none());
        assert!(saved.rosen_change.is_none());
        assert!(saved.branch_name.is_empty());

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
