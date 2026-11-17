//! F8 project Task List: save before publishing, with thread links drawn from saved state.
use crate::{
    ProjectItem, StateStore, StoreError, TaskAttribution, TaskItem, TaskState, ThreadItem,
    ThreadState,
};
use serde::{Deserialize, Serialize};
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
    #[error("This event has no Task List change to undo")]
    UnknownTaskUndo,
    #[error("Task #{0} has changed since that event")]
    TaskUndoConflict(u32),
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
        let task = remove_task(project, identifier)?;
        self.commit(next)?;
        Ok(task)
    }
    pub fn list_tasks(&self, project_identifier: u32) -> Result<Vec<TaskView>, TaskError> {
        self.project(project_identifier)
            .map(task_views)
            .ok_or(TaskError::UnknownProject(project_identifier))
    }
}

fn remove_task(project: &mut ProjectItem, identifier: u32) -> Result<TaskItem, TaskError> {
    let position = project
        .tasks
        .iter()
        .position(|task| task.id == identifier)
        .ok_or(TaskError::UnknownTask(identifier))?;
    let task = project.tasks.remove(position);
    project
        .continuous_dispatch
        .task_attempts
        .remove(&identifier);
    for thread in &mut project.threads {
        thread.task_ids.retain(|listed| *listed != identifier);
    }
    Ok(task)
}

pub enum TaskMutation {
    Update(TaskUpdate),
    Delete(u32),
}

/// Saved together with its mutation so a replay can still post the original undoable event.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct TaskUndo {
    before: TaskItem,
    after: Option<TaskItem>,
    #[serde(default)]
    undone: bool,
}

impl TaskUndo {
    fn changed(&self) -> bool {
        self.after.as_ref() != Some(&self.before)
    }

    fn is_current(&self, project: &ProjectItem) -> bool {
        !self.undone
            && project.tasks.iter().find(|item| item.id == self.before.id) == self.after.as_ref()
            && !(self.after.is_some()
                && self.before.state == TaskState::Now
                && project.threads.iter().any(|thread| {
                    Some(thread.id) == self.before.assigned_thread_id
                        && thread.state == ThreadState::Merged
                }))
    }
}

pub fn undoable_task_requests(project: &ProjectItem) -> std::collections::HashSet<u64> {
    let mut seen = std::collections::HashSet::new();
    project
        .task_undo
        .iter()
        .rev()
        .filter(|(_, undo)| !undo.undone && undo.changed() && seen.insert(undo.before.id))
        .filter_map(|(request, undo)| undo.is_current(project).then_some(*request))
        .collect()
}

impl StateStore {
    pub fn apply_agent_task_mutation(
        &mut self,
        project: u32,
        member: &str,
        request: u64,
        mutation: TaskMutation,
    ) -> Result<(TaskItem, bool), TaskError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(TaskError::UnknownProject(project))?;
        if let Some(undo) = item.task_undo.get(&request) {
            return Ok((undo.after.as_ref().unwrap_or(&undo.before).clone(), true));
        }
        let identifier = match &mutation {
            TaskMutation::Update(update) => update.task,
            TaskMutation::Delete(identifier) => *identifier,
        };
        let dispatched = member
            .strip_prefix("thread-")
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|identifier| {
                item.threads
                    .iter()
                    .any(|thread| thread.id == identifier && thread.started_by_dispatch)
            });
        let before = item
            .tasks
            .iter()
            .find(|task| task.id == identifier)
            .cloned()
            .ok_or(TaskError::UnknownTask(identifier))?;
        let (task, after) = match mutation {
            TaskMutation::Update(update) => {
                let updated = apply_update(item, &update)?;
                (updated.clone(), Some(updated))
            }
            TaskMutation::Delete(identifier) => (remove_task(item, identifier)?, None),
        };
        if dispatched {
            item.task_undo.insert(
                request,
                TaskUndo {
                    before,
                    after,
                    undone: false,
                },
            );
        }
        self.commit(next)?;
        Ok((task, dispatched))
    }

    pub fn undo_task_request(&mut self, project: u32, request: u64) -> Result<TaskItem, TaskError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(TaskError::UnknownProject(project))?;
        let undo = item
            .task_undo
            .get(&request)
            .cloned()
            .ok_or(TaskError::UnknownTaskUndo)?;
        if !undoable_task_requests(item).contains(&request) {
            return Err(TaskError::TaskUndoConflict(undo.before.id));
        }
        let task = restore_task(item, undo.before);
        if let Some(undo) = item.task_undo.get_mut(&request) {
            undo.undone = true;
        }
        self.commit(next)?;
        Ok(task)
    }
}

fn restore_task(project: &mut ProjectItem, task: TaskItem) -> TaskItem {
    let mut task = task.normalize(project.spec_locked);
    let owner = task.assigned_thread_id.and_then(|identifier| {
        project.threads.iter_mut().find(|thread| {
            thread.id == identifier
                && (task.state == TaskState::Done
                    || (!thread.supervision.released
                        && thread.task_ids.iter().all(|listed| *listed == task.id)
                        && matches!(thread.state, ThreadState::Working | ThreadState::Review)))
        })
    });
    if let Some(owner) = owner {
        if !owner.task_ids.contains(&task.id) {
            owner.task_ids.push(task.id);
        }
    } else {
        task.assigned_thread_id = None;
        if task.state == TaskState::Now {
            task.state = TaskState::Todo;
        }
    }
    if let Some(current) = project.tasks.iter_mut().find(|entry| entry.id == task.id) {
        *current = task.clone();
    } else {
        let position = project
            .tasks
            .iter()
            .position(|entry| entry.id > task.id)
            .unwrap_or(project.tasks.len());
        project.tasks.insert(position, task.clone());
    }
    task
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
        project.continuous_dispatch.task_attempts.remove(&task.id);
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
