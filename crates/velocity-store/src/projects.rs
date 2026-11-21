//! Project selection and persistence adapters. File writes finish before the
//! in-memory selection or the main-window context is published.

use serde::{Deserialize, Serialize};
use specta::Type;
use thiserror::Error;
use velocity_core::duty::{DutyFeedEvent, DutyRole};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutIntent, LayoutSnapshot};
use velocity_core::projects::{self as policy, ProjectSelection, ProjectSource};

use crate::{ProjectLocationKind, StateStore, StoreError};

pub use crate::project_layout::LayoutRefresh;

/// Data extracted under the store lock for a project switch so layout restoration, snapshot
/// creation and event emission run after releasing the store lock (#554).
pub struct ProjectSwitchData {
    pub refresh: LayoutRefresh,
    pub selected_thread: Option<usize>,
    pub search: String,
    pub selection: ProjectSelectionView,
    pub projects: ProjectList,
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error(transparent)]
    Task(#[from] crate::task_list::TaskError),
    #[error("Project {0} is not registered")]
    UnknownProject(u32),
    #[error("Invalid project identifier: {0}")]
    InvalidIdentifier(String),
    #[error("The selected project index exceeds the persisted-state format")]
    IndexOutOfRange,
    #[error(transparent)]
    Persistence(#[from] StoreError),
    #[error(transparent)]
    Provider(#[from] crate::providers::ProviderUnavailable),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum ProjectLabel {
    Local,
    GitHub,
    Speccing,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct ProjectRow {
    pub identifier: u32,
    pub name: String,
    pub repository_path: String,
    pub label: ProjectLabel,
    pub selected: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct ProjectList {
    pub revision: u32,
    pub rows: Vec<ProjectRow>,
}

pub struct ProjectDispatch {
    pub snapshot: LayoutSnapshot,
    pub intent: Option<LayoutIntent>,
    pub selection: Option<ProjectSelectionView>,
    pub task_added: Option<(u32, crate::TaskItem)>,
    pub duty_event: Option<(DutyRole, DutyFeedEvent)>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct ProjectSelectionView {
    pub active_project: Option<u32>,
    pub menu_open: bool,
}

impl StateStore {
    pub fn list_projects(&self) -> ProjectList {
        let rows = self
            .state
            .projects
            .iter()
            .enumerate()
            .map(|(index, project)| {
                let source = match project.location_kind {
                    ProjectLocationKind::Local => ProjectSource::Local,
                    ProjectLocationKind::GitHub => ProjectSource::GitHub,
                };
                let label = match policy::project_label(source, project.spec_locked) {
                    policy::ProjectLabel::Local => ProjectLabel::Local,
                    policy::ProjectLabel::GitHub => ProjectLabel::GitHub,
                    policy::ProjectLabel::Speccing => ProjectLabel::Speccing,
                };
                ProjectRow {
                    identifier: project.id,
                    name: project.name.clone(),
                    repository_path: project.repository_path.clone(),
                    label,
                    selected: usize::try_from(self.state.active_project_index).ok() == Some(index),
                }
            })
            .collect();
        ProjectList {
            revision: self.state.revision,
            rows,
        }
    }

    pub fn restore_project_layout(&self, layout: &mut LayoutState) {
        let selected = crate::project_layout::active_project(&self.state)
            .and_then(|project| project.active_thread_id)
            .and_then(|identifier| usize::try_from(identifier).ok());
        layout.restore_project(
            crate::project_layout::read_model(self),
            selected,
            String::new(),
        );
        crate::build_gate::sync_layout(&self.state, layout);
    }

    /// Updates the persisted active project in place and returns all inputs needed to restore
    /// `LayoutState` and publish selection events after dropping the store lock (#554).
    pub fn select_project(
        &mut self,
        requested: u32,
        current_search: &str,
    ) -> Result<ProjectSwitchData, ProjectError> {
        let identifiers: Vec<_> = self
            .state
            .projects
            .iter()
            .map(|project| project.id)
            .collect();
        let current = crate::project_layout::active_project(&self.state).map(|project| project.id);
        let selection = ProjectSelection {
            projects: &self.state.projects,
            active_project: current,
            search: current_search.to_owned(),
            menu_open: true,
        };
        let (selection, found) =
            policy::switch_project(selection, &identifiers, requested, String::new());
        let index = found.ok_or(ProjectError::UnknownProject(requested))?;
        let index = u32::try_from(index).map_err(|_| ProjectError::IndexOutOfRange)?;
        let search = selection.search;
        let view = ProjectSelectionView {
            active_project: selection.active_project,
            menu_open: selection.menu_open,
        };
        self.set_active_project_index(index)?;
        let selected_thread = crate::project_layout::active_project(&self.state)
            .and_then(|project| project.active_thread_id)
            .and_then(|identifier| usize::try_from(identifier).ok());
        Ok(ProjectSwitchData {
            refresh: self.layout_refresh(),
            selected_thread,
            search,
            selection: view,
            projects: self.list_projects(),
        })
    }

    pub fn switch_project_layout(
        &mut self,
        layout: &mut LayoutState,
        requested: u32,
    ) -> Result<ProjectSelectionView, ProjectError> {
        let current_search = layout.search_query().to_owned();
        let switched = self.select_project(requested, &current_search)?;
        layout.restore_store_project(
            switched.refresh.revision,
            switched.refresh.data,
            switched.selected_thread,
            switched.search,
            switched.refresh.spec_locked,
        );
        Ok(switched.selection)
    }

    fn set_active_project_index(&mut self, index: u32) -> Result<(), ProjectError> {
        let previous = self.state.active_project_index;
        if previous == index {
            return Ok(());
        }
        self.state.active_project_index = index;
        if let Err(error) = self.record_change() {
            self.state.active_project_index = previous;
            return Err(error.into());
        }
        Ok(())
    }

    fn toggle_active_project_settled(&mut self) -> Result<(), ProjectError> {
        let Ok(index) = usize::try_from(self.state.active_project_index) else {
            return Ok(());
        };
        let Some(project) = self.state.projects.get_mut(index) else {
            return Ok(());
        };
        let previous = project.settled_expanded;
        project.settled_expanded = !previous;
        if let Err(error) = self.record_change() {
            if let Some(project) = self.state.projects.get_mut(index) {
                project.settled_expanded = previous;
            }
            return Err(error.into());
        }
        Ok(())
    }

    pub fn dispatch_project_layout(
        &mut self,
        layout: &mut LayoutState,
        action: LayoutAction,
    ) -> Result<ProjectDispatch, ProjectError> {
        // A failed persistence write must leave both the live view and its saved
        // selection unchanged. Apply transient actions to a candidate first.
        if matches!(action, LayoutAction::ToggleSettled) {
            self.toggle_active_project_settled()?;
        }
        let mut candidate = layout.clone();
        let mut intent = crate::build_gate::dispatch_layout(&self.state, &mut candidate, action);
        let task_added = self.dispatch_task_intent(&mut candidate, &mut intent)?;
        let mut duty_event = None;
        let selection = if let Some(LayoutIntent::SwitchProject(identifier)) = &intent {
            let requested = identifier
                .parse()
                .map_err(|_| ProjectError::InvalidIdentifier(identifier.clone()))?;
            let selection = self.switch_project_layout(&mut candidate, requested)?;
            intent = None;
            Some(selection)
        } else {
            if let Some(LayoutIntent::SteerThread {
                identifier, prompt, ..
            }) = &intent
            {
                duty_event = self.record_duty_composer_message(
                    &mut candidate,
                    *identifier,
                    prompt.clone(),
                    crate::duty::clock_timestamp(),
                )?;
                if duty_event.is_some() {
                    intent = None;
                }
            }
            self.remember_project_thread(candidate.selected_thread())?;
            None
        };
        self.hydrate_selected_thread(&mut candidate);
        *layout = candidate;
        Ok(ProjectDispatch {
            snapshot: layout.snapshot(),
            intent,
            selection,
            task_added,
            duty_event,
        })
    }

    fn dispatch_task_intent(
        &mut self,
        layout: &mut LayoutState,
        intent: &mut Option<LayoutIntent>,
    ) -> Result<Option<(u32, crate::TaskItem)>, ProjectError> {
        let Some(project) = self.active_project() else {
            if matches!(intent, Some(LayoutIntent::AddTask(_))) {
                return Err(ProjectError::UnknownProject(0));
            }
            return Ok(None);
        };
        let project_identifier = project.id;
        match intent.as_ref() {
            Some(LayoutIntent::AddTask(title)) => {
                let task = self.add_task(project_identifier, title.clone())?;
                layout.set_task_rows(self.list_tasks(project_identifier)?);
                *intent = None;
                Ok(Some((project_identifier, task)))
            }
            Some(LayoutIntent::OpenTask(identifier)) => {
                let owner = project
                    .tasks
                    .iter()
                    .find(|task| task.id as usize == *identifier)
                    .and_then(|task| task.assigned_thread_id);
                if let Some(owner) = owner {
                    layout.apply(LayoutAction::OpenThread(owner as usize));
                }
                *intent = None;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn remember_project_thread(&mut self, selected: Option<usize>) -> Result<(), ProjectError> {
        let Some(project) = crate::project_layout::active_project(&self.state) else {
            return Ok(());
        };
        let selected = selected
            .and_then(|identifier| u32::try_from(identifier).ok())
            .filter(|identifier| {
                project
                    .threads
                    .iter()
                    .any(|thread| thread.id == *identifier)
                    || project.duty_roles.iter().any(|duty| duty.id == *identifier)
                    || project.swarms.iter().any(|swarm| swarm.id == *identifier)
            });
        if project.active_thread_id == selected {
            return Ok(());
        }
        let index = usize::try_from(self.state.active_project_index)
            .map_err(|_| ProjectError::IndexOutOfRange)?;
        let project = self
            .state
            .projects
            .get_mut(index)
            .ok_or(ProjectError::IndexOutOfRange)?;
        let previous = project.active_thread_id;
        project.active_thread_id = selected;
        if let Err(error) = self.record_change() {
            if let Some(project) = self.state.projects.get_mut(index) {
                project.active_thread_id = previous;
            }
            return Err(error.into());
        }
        Ok(())
    }
}
