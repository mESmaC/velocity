//! F8: save an agent-requested task and its replay identity in one snapshot.
use crate::threads::ThreadError;
use crate::{StateStore, TaskAttribution, TaskItem, TaskState};

impl StateStore {
    pub fn add_agent_task(
        &mut self,
        project: u32,
        member: &str,
        sequence: u64,
        title: &str,
    ) -> Result<TaskItem, ThreadError> {
        let mut next = self.state.clone();
        let saved = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if let Some(task) = saved
            .tasks
            .iter()
            .find(|task| task.agent_request == Some(sequence))
        {
            return Ok(task.clone());
        }
        let attribution = agent_attribution(saved, member)?;
        let title = title.trim();
        if title.is_empty() {
            return Err(ThreadError::InvalidLaunch("Task title is empty".into()));
        }
        let identifier = saved
            .tasks
            .iter()
            .map(|task| task.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(ThreadError::IdentifiersExhausted)?;
        let state = if saved.spec_locked {
            TaskState::Todo
        } else {
            TaskState::Draft
        };
        let mut task = TaskItem::new(identifier, title.into(), state, attribution);
        task.agent_request = Some(sequence);
        saved.tasks.push(task.clone());
        self.commit(next)?;
        Ok(task)
    }
}

fn agent_attribution(
    project: &crate::ProjectItem,
    member: &str,
) -> Result<TaskAttribution, ThreadError> {
    if let Some(identifier) = member
        .strip_prefix("thread-")
        .or_else(|| member.strip_prefix("spec-"))
        .and_then(|text| text.parse::<u32>().ok())
        .filter(|identifier| {
            project
                .threads
                .iter()
                .any(|thread| thread.id == *identifier)
        })
    {
        return Ok(TaskAttribution::Thread(identifier));
    }
    if let Some(nick) = member.strip_prefix("duty-")
        && let Some(role) = project
            .duty_roles
            .iter()
            .find(|role| role.agents.iter().any(|agent| agent.nickname == nick))
    {
        return Ok(TaskAttribution::Thread(role.id));
    }
    Err(ThreadError::InvalidLaunch(
        "Task author is no longer registered in this project".into(),
    ))
}
