//! F8 project Task List: save before publishing, with thread links drawn from saved state.
use crate::{
    ProjectItem, StateStore, StoreError, TaskAttribution, TaskItem, TaskState, ThreadItem,
    ThreadState,
};
use velocity_core::layout_view::TaskView;
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("Project {0} is not registered")]
    UnknownProject(u32),
    #[error("A task needs a title")]
    EmptyTitle,
    #[error("This project has exhausted its task identifiers")]
    IdentifiersExhausted,
    #[error("Task #{0} is not on this project's Task List")]
    UnknownTask(u32),
    #[error("Task #{0} stays a draft until the spec is locked")]
    SpecUnlocked(u32),
    #[error("An agent is working on task #{0}: it can be marked done, not reopened")]
    TaskInProgress(u32),
    #[error(transparent)]
    Persistence(#[from] StoreError),
}
impl StateStore {
    pub fn add_task(
        &mut self,
        project_identifier: u32,
        title: String,
    ) -> Result<TaskItem, TaskError> {
        let mut next = self.state.clone();
        let project = next
            .projects
            .iter_mut()
            .find(|project| project.id == project_identifier)
            .ok_or(TaskError::UnknownProject(project_identifier))?;
        let task = append_task(project, &title, TaskAttribution::User)?;
        self.commit(next)?;
        Ok(task)
    }
    /// Retitle a task, move it between todo and done, or both (F8). `Now` and `Draft` are
    /// never requested: an agent's start sets `Now`, and the spec lock ends `Draft`.
    pub fn update_task(
        &mut self,
        project_identifier: u32,
        change: TaskUpdate,
    ) -> Result<TaskItem, TaskError> {
        let mut next = self.state.clone();
        let project = next
            .projects
            .iter_mut()
            .find(|project| project.id == project_identifier)
            .ok_or(TaskError::UnknownProject(project_identifier))?;
        let task = apply_update(project, &change)?;
        self.commit(next)?;
        Ok(task)
    }
    /// Remove a task (F8). A thread working on it keeps running; it just stops listing it.
    pub fn delete_task(
        &mut self,
        project_identifier: u32,
        identifier: u32,
    ) -> Result<TaskItem, TaskError> {
        let mut next = self.state.clone();
        let project = next
            .projects
            .iter_mut()
            .find(|project| project.id == project_identifier)
            .ok_or(TaskError::UnknownProject(project_identifier))?;
        let position = project
            .tasks
            .iter()
            .position(|task| task.id == identifier)
            .ok_or(TaskError::UnknownTask(identifier))?;
        let task = project.tasks.remove(position);
        for thread in &mut project.threads {
            thread.task_ids.retain(|listed| *listed != identifier);
        }
        self.commit(next)?;
        Ok(task)
    }
    pub fn list_tasks(&self, project_identifier: u32) -> Result<Vec<TaskView>, TaskError> {
        self.project(project_identifier)
            .map(task_views)
            .ok_or(TaskError::UnknownProject(project_identifier))
    }
}
pub(crate) fn append_task(
    project: &mut ProjectItem,
    title: &str,
    attribution: TaskAttribution,
) -> Result<TaskItem, TaskError> {
    let title = task_title(title).ok_or(TaskError::EmptyTitle)?;
    let identifier = project
        .next_task_identifier()
        .ok_or(TaskError::IdentifiersExhausted)?;
    let state = if project.spec_locked {
        TaskState::Todo
    } else {
        TaskState::Draft
    };
    let task = TaskItem::new(identifier, title, state, attribution);
    project.tasks.push(task.clone());
    Ok(task)
}

/// A task edit: a new title, a new state, or both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskUpdate {
    pub task: u32,
    pub title: Option<String>,
    pub state: Option<TaskState>,
}
impl TaskUpdate {
    pub fn new(task: u32) -> Self {
        Self {
            task,
            title: None,
            state: None,
        }
    }
}
fn apply_update(project: &mut ProjectItem, change: &TaskUpdate) -> Result<TaskItem, TaskError> {
    let title = change
        .title
        .as_deref()
        .map(|title| task_title(title).ok_or(TaskError::EmptyTitle))
        .transpose()?;
    let spec_locked = project.spec_locked;
    let task = project
        .tasks
        .iter_mut()
        .find(|task| task.id == change.task)
        .ok_or(TaskError::UnknownTask(change.task))?;
    let reopened = match change.state {
        Some(_) if !spec_locked => return Err(TaskError::SpecUnlocked(task.id)),
        Some(TaskState::Todo) if task.state == TaskState::Now => {
            return Err(TaskError::TaskInProgress(task.id));
        }
        Some(state) => {
            task.state = state;
            state == TaskState::Todo
        }
        None => false,
    };
    if reopened {
        task.assigned_thread_id = None;
    }
    if let Some(title) = title {
        task.text = title;
    }
    let task = task.clone();
    // Reopening frees the task from the thread that finished it, so that thread's merge
    // cannot close it again.
    if reopened {
        for thread in &mut project.threads {
            thread.task_ids.retain(|listed| *listed != task.id);
        }
    }
    Ok(task)
}
fn task_title(title: &str) -> Option<String> {
    let title = title.trim().strip_suffix('.').unwrap_or(title.trim());
    let mut characters = title.chars();
    let first = characters.next()?;
    Some(first.to_uppercase().chain(characters).collect())
}
pub(crate) fn task_views(project: &ProjectItem) -> Vec<TaskView> {
    project
        .tasks
        .iter()
        .map(|task| {
            let owner = task.assigned_thread_id.and_then(|identifier| {
                project
                    .threads
                    .iter()
                    .find(|thread| thread.id == identifier)
            });
            let (attribution, attribution_thread) = attribution(project, task.added_by);
            let state = match task.state {
                TaskState::Draft => "draft",
                TaskState::Todo => "todo",
                TaskState::Now => "now",
                TaskState::Done => "done",
            };
            let mut view = TaskView::new(task.id.to_string(), task.text.clone(), state.into());
            view.metadata = owner.map(thread_label).unwrap_or_default();
            view.thread_identifier = owner.map(|thread| thread.id.to_string());
            view.attribution = attribution;
            view.attribution_thread = attribution_thread;
            view
        })
        .collect()
}
fn attribution(project: &ProjectItem, source: TaskAttribution) -> (Option<String>, Option<String>) {
    match source {
        TaskAttribution::Spec => (None, None),
        TaskAttribution::User => (Some("added by you".into()), None),
        TaskAttribution::Thread(identifier) => {
            let thread = project
                .threads
                .iter()
                .find(|thread| thread.id == identifier);
            let duty = project.duty_roles.iter().find(|role| role.id == identifier);
            let name = thread
                .map(|thread| thread.title.as_str())
                .or_else(|| duty.map(|role| role.role.title()))
                .unwrap_or("deleted thread");
            (
                Some(format!("added by {name}")),
                (thread.is_some() || duty.is_some()).then(|| identifier.to_string()),
            )
        }
    }
}
fn thread_label(thread: &ThreadItem) -> String {
    let number = thread
        .pull_request_number
        .map(|number| format!("#{number}"));
    match thread.state {
        ThreadState::Merged => number
            .map(|number| format!("merged · {number}"))
            .unwrap_or_else(|| "merged".into()),
        ThreadState::Review => number
            .map(|number| format!("{number} · {}", review_label(thread)))
            .unwrap_or_else(|| "in review".into()),
        ThreadState::Error => "failed · worktree kept".into(),
        ThreadState::Closed => "closed · worktree kept".into(),
        // The agent's turn ended before its PR is followed (a draft claim, or the watcher's
        // next pass): it is waiting, not working (#280).
        ThreadState::Working if thread.activity == crate::AgentActivity::Idle => {
            "agent idle".into()
        }
        _ => "agent working".into(),
    }
}
fn review_label(thread: &ThreadItem) -> &'static str {
    if crate::follow_through::merged_awaiting_cleanup(thread) {
        return crate::follow_through::MERGED_CLEANING_UP;
    }
    match thread.review_stage {
        Some(crate::ReviewStage::ContinuousIntegration) => "checking",
        Some(crate::ReviewStage::Fixing) => "addressing review",
        Some(crate::ReviewStage::Approved) => "approved · merging",
        _ => "in review",
    }
}
