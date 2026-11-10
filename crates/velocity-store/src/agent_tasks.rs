//! F8: save an agent-requested task and its replay identity in one snapshot.
use crate::threads::ThreadError;
use crate::{StateStore, TaskAttribution, TaskItem};

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
        let mut task = crate::task_list::append_task(saved, title, attribution)
            .map_err(|error| ThreadError::InvalidLaunch(error.to_string()))?;
        task.agent_request = Some(sequence);
        if let Some(saved_task) = saved.tasks.last_mut() {
            saved_task.agent_request = Some(sequence);
        }
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
