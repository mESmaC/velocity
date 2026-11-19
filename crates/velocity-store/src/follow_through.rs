//! Thread lookups for follow-through work (F6): the retained thread a resumed turn, parking
//! or Rosen event acts on, and the Task List effects of a land and a revert.
use crate::{ProjectItem, StateStore, ThreadItem, ThreadState};

#[derive(Debug, thiserror::Error)]
pub enum FollowThroughError {
    #[error("Project {0} is not registered")]
    UnknownProject(u32),
    #[error("The thread no longer exists in this project")]
    MissingThread,
    #[error("This project's spec must be locked before its agent starts")]
    UnlockedSpec,
    #[error("The thread has no retained worktree")]
    MissingWorktree,
}

impl StateStore {
    pub fn follow_through_thread(
        &self,
        project_identifier: u32,
        thread_identifier: u32,
    ) -> Result<(ProjectItem, ThreadItem), FollowThroughError> {
        let project = self
            .project(project_identifier)
            .ok_or(FollowThroughError::UnknownProject(project_identifier))?;
        if !project.spec_locked {
            return Err(FollowThroughError::UnlockedSpec);
        }
        let thread = project
            .threads
            .iter()
            .find(|thread| thread.id == thread_identifier)
            .ok_or(FollowThroughError::MissingThread)?;
        if thread.worktree_path.is_none() {
            return Err(FollowThroughError::MissingWorktree);
        }
        Ok((project.clone(), thread.clone()))
    }

    pub fn refresh_follow_through_layout(
        &self,
        layout: &mut velocity_core::layout_state::LayoutState,
    ) {
        layout.replace_data(crate::project_layout::read_model(self));
        crate::build_gate::sync_layout(&self.state, layout);
    }
}

/// A task is done when its change lands (F8): the landed thread's tasks tick off (F10).
pub(crate) fn finish_thread_tasks(state: &mut crate::PersistedState, project: u32, thread: u32) {
    let Some(project) = state.projects.iter_mut().find(|item| item.id == project) else {
        return;
    };
    let (task_ids, spec_section) = project
        .threads
        .iter()
        .find(|item| item.id == thread)
        .map(|item| (item.task_ids.clone(), item.spec_section.clone()))
        .unwrap_or_default();
    for task in &mut project.tasks {
        if task.assigned_thread_id == Some(thread) || task_ids.contains(&task.id) {
            task.state = crate::TaskState::Done;
        }
    }
    if let Some(section) = spec_section
        && !project.built_spec_sections.contains(&section)
    {
        project.built_spec_sections.push(section);
    }
}

/// Undoes [`finish_thread_tasks`] when a landed change is reverted (Rosen R12): the work is
/// no longer on the base, so the thread's tasks are its open work again, and its SPEC
/// section is unbuilt unless another merged thread built it too, including deleted history.
/// The thread fixes the change itself, so it keeps owning its tasks.
pub(crate) fn reopen_thread_tasks(state: &mut crate::PersistedState, project: u32, thread: u32) {
    let Some(project) = state.projects.iter_mut().find(|item| item.id == project) else {
        return;
    };
    let (task_ids, spec_section) = project
        .threads
        .iter()
        .find(|item| item.id == thread)
        .map(|item| (item.task_ids.clone(), item.spec_section.clone()))
        .unwrap_or_default();
    for task in &mut project.tasks {
        let owned = task.assigned_thread_id == Some(thread)
            || (task.assigned_thread_id.is_none() && task_ids.contains(&task.id));
        if owned && task.state == crate::TaskState::Done {
            task.state = crate::TaskState::Now;
            task.assigned_thread_id = Some(thread);
        }
    }
    let Some(section) = spec_section else {
        return;
    };
    let built_elsewhere = project
        .deleted_thread_built_spec_sections
        .contains(&section)
        || project.threads.iter().any(|item| {
            item.id != thread
                && item.state == ThreadState::Merged
                && item.spec_section.as_ref() == Some(&section)
        });
    if !built_elsewhere {
        project
            .built_spec_sections
            .retain(|built| *built != section);
    }
}

#[cfg(test)]
mod tests {
    use crate::{PersistedState, ProjectItem, StateStore, ThreadItem};

    /// Risk: saves written before #426 carry a GitHub PR record on each thread. Dropping the
    /// field must not stop `state.json` from loading or lose the thread it was saved on.
    #[test]
    fn saves_with_a_legacy_pull_request_record_still_load() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = std::env::temp_dir().join(format!(
            "velocity-legacy-pr-record-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory)?;
        let mut project = ProjectItem::new(1, "legacy".into(), directory.display().to_string());
        project
            .threads
            .push(ThreadItem::new(7, "Merged PR thread".into()));
        let mut state = PersistedState::default();
        state.projects.push(project);
        let mut legacy = serde_json::to_value(state)?;
        legacy["projects"][0]["threads"][0]
            .as_object_mut()
            .ok_or("missing thread")?
            .insert(
                "follow_through".into(),
                serde_json::json!({
                    "number": 12,
                    "url": "https://github.com/owner/repository/pull/12",
                    "stage": "merged",
                    "worktree_deleted": true,
                    "history": [{"identifier": "opened", "title": "PR #12 opened"}],
                }),
            );
        std::fs::write(directory.join("state.json"), serde_json::to_vec(&legacy)?)?;
        let store = StateStore::load_or_default(&directory)?;
        let thread = store
            .project(1)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == 7))
            .ok_or("the legacy thread was lost")?;
        assert_eq!(thread.title, "Merged PR thread");
        assert!(
            serde_json::to_value(thread)?
                .get("follow_through")
                .is_none(),
            "the next save drops the legacy record"
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
