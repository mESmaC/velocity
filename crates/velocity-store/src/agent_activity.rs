//! Durable supervision checkpoints survive application restarts.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct Supervision {
    #[serde(default)]
    #[specta(type = u32)]
    pub started: u64,
    #[serde(default)]
    #[specta(type = u32)]
    pub last_update: u64,
    #[serde(default)]
    pub turns: u32,
    #[serde(default)]
    pub nudged: bool,
    #[serde(default)]
    #[specta(type = Option<u32>)]
    pub unavailable_since: Option<u64>,
    #[serde(default)]
    pub released: bool,
    #[serde(default)]
    pub pushed_commit: Option<String>,
    #[serde(default)]
    #[specta(type = Option<u32>)]
    pub retry_after: Option<u64>,
    #[serde(default)]
    #[specta(skip)]
    pub idle_nudged: bool,
    /// Completed turn count when the last idle supervision nudge was sent (#544).
    /// When a later turn ends still idle with an unnamed change, dispatch escalates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[specta(skip)]
    pub idle_nudge_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[specta(skip)]
    pub last_wait: Option<String>,
}

impl crate::StateStore {
    pub fn reclaim_agent_work(
        &mut self,
        project_identifier: u32,
        identifier: u32,
    ) -> Result<(), crate::threads::ThreadError> {
        self.try_edit_project(
            project_identifier,
            || crate::threads::ThreadError::UnknownProject(project_identifier),
            |project| {
                let thread_index = project
                    .threads
                    .iter()
                    .position(|item| item.id == identifier)
                    .ok_or(crate::threads::ThreadError::UnknownThread {
                        project: project_identifier,
                        thread: identifier,
                    })?;
                if project.threads[thread_index].supervision.released {
                    let task_ids = &project.threads[thread_index].task_ids;
                    for task in project
                        .tasks
                        .iter_mut()
                        .filter(|task| task_ids.contains(&task.id))
                    {
                        if task
                            .assigned_thread_id
                            .is_some_and(|owner| owner != identifier)
                        {
                            return Err(crate::threads::ThreadError::TaskUnavailable(task.id));
                        }
                        task.assigned_thread_id = Some(identifier);
                        task.state = crate::TaskState::Now;
                    }
                }
                let thread = &mut project.threads[thread_index];
                thread.supervision.released = false;
                thread.supervision.unavailable_since = None;
                thread.supervision.retry_after = None;
                thread.supervision.nudged = false;
                thread.supervision.idle_nudged = false;
                thread.supervision.idle_nudge_turns = None;
                Ok((true, ()))
            },
        )
    }

    pub fn release_agent_work(
        &mut self,
        project: u32,
        identifier: u32,
        pushed: Option<String>,
    ) -> Result<(), crate::threads::ThreadError> {
        self.try_edit_project(
            project,
            || crate::threads::ThreadError::UnknownProject(project),
            |project| {
                let thread = project
                    .threads
                    .iter_mut()
                    .find(|item| item.id == identifier)
                    .ok_or(crate::threads::ThreadError::UnknownThread {
                        project: project.id,
                        thread: identifier,
                    })?;
                thread.supervision.released = true;
                thread.supervision.pushed_commit = pushed;
                for task in &mut project.tasks {
                    if task.assigned_thread_id == Some(identifier) {
                        task.assigned_thread_id = None;
                        task.state = crate::TaskState::Todo;
                    }
                }
                Ok((true, ()))
            },
        )
    }
}
