//! Project selection and persistence adapters. File writes finish before the
//! in-memory selection or the main-window context is published.

use serde::{Deserialize, Serialize};
use specta::Type;
use thiserror::Error;
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutIntent, LayoutSnapshot};
use velocity_core::projects::{self as policy, ProjectSelection, ProjectSource};

use crate::{PersistedState, ProjectLocationKind, StateStore, StoreError, write_atomic_snapshot};

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("Project {0} is not registered")]
    UnknownProject(u32),
    #[error("Invalid project identifier: {0}")]
    InvalidIdentifier(String),
    #[error("The selected project index exceeds the persisted-state format")]
    IndexOutOfRange,
    #[error(transparent)]
    Persistence(#[from] StoreError),
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
            crate::project_layout::read_model(&self.state),
            selected,
            String::new(),
        );
    }

    pub fn switch_project_layout(
        &mut self,
        layout: &mut LayoutState,
        requested: u32,
    ) -> Result<ProjectSelectionView, ProjectError> {
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
            search: layout.search_query().to_owned(),
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
        if self.state.active_project_index != index {
            let mut next = self.state.clone();
            next.active_project_index = index;
            self.commit_project_state(next)?;
        }
        let selected = crate::project_layout::active_project(&self.state)
            .and_then(|project| project.active_thread_id)
            .and_then(|identifier| usize::try_from(identifier).ok());
        layout.restore_project(
            crate::project_layout::read_model(&self.state),
            selected,
            search,
        );
        Ok(view)
    }

    pub fn dispatch_project_layout(
        &mut self,
        layout: &mut LayoutState,
        action: LayoutAction,
    ) -> Result<ProjectDispatch, ProjectError> {
        // A failed persistence write must leave both the live view and its saved
        // selection unchanged. Apply transient actions to a candidate first.
        let mut candidate = layout.clone();
        let mut intent = candidate.apply(action);
        let selection = if let Some(LayoutIntent::SwitchProject(identifier)) = &intent {
            let requested = identifier
                .parse()
                .map_err(|_| ProjectError::InvalidIdentifier(identifier.clone()))?;
            let selection = self.switch_project_layout(&mut candidate, requested)?;
            intent = None;
            Some(selection)
        } else {
            self.remember_project_thread(candidate.selected_thread())?;
            None
        };
        *layout = candidate;
        Ok(ProjectDispatch {
            snapshot: layout.snapshot(),
            intent,
            selection,
        })
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
            });
        if project.active_thread_id == selected {
            return Ok(());
        }
        let mut next = self.state.clone();
        let index = usize::try_from(next.active_project_index)
            .map_err(|_| ProjectError::IndexOutOfRange)?;
        let project = next
            .projects
            .get_mut(index)
            .ok_or(ProjectError::IndexOutOfRange)?;
        project.active_thread_id = selected;
        self.commit_project_state(next)
    }

    fn commit_project_state(&mut self, mut next: PersistedState) -> Result<(), ProjectError> {
        next.revision = self.state.revision.wrapping_add(1);
        write_atomic_snapshot(&self.directory, &next)?;
        self.disk_revision = next.revision;
        self.state = next;
        Ok(())
    }
}
