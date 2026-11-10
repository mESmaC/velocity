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
}

impl crate::StateStore {
    pub fn reclaim_agent_work(
        &mut self,
        project_identifier: u32,
        identifier: u32,
    ) -> Result<(), crate::threads::ThreadError> {
        let mut next = self.state.clone();
        let project = next
            .projects
            .iter_mut()
            .find(|item| item.id == project_identifier)
            .ok_or(crate::threads::ThreadError::UnknownProject(
                project_identifier,
            ))?;
        let thread = project
            .threads
            .iter_mut()
            .find(|item| item.id == identifier)
            .ok_or(crate::threads::ThreadError::UnknownThread {
                project: project_identifier,
                thread: identifier,
            })?;
        if thread.supervision.released {
            for task in project
                .tasks
                .iter_mut()
                .filter(|task| thread.task_ids.contains(&task.id))
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
        thread.supervision.released = false;
        thread.supervision.unavailable_since = None;
        thread.supervision.retry_after = None;
        thread.supervision.nudged = false;
        self.commit(next)?;
        Ok(())
    }

    pub fn release_agent_work(
        &mut self,
        project: u32,
        identifier: u32,
        pushed: Option<String>,
    ) -> Result<(), crate::threads::ThreadError> {
        let mut next = self.state.clone();
        let project = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(crate::threads::ThreadError::UnknownProject(project))?;
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
        self.commit(next)?;
        Ok(())
    }
}
