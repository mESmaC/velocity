//! Store updates for Create in the New project dialog (SPEC.md F2): the duplicate
//! check, and one commit that registers, activates and records the interview.

use crate::projects::ProjectError;
use crate::{
    PendingInterview, ProjectItem, ProjectLocationKind, StateStore, write_atomic_snapshot,
};

/// What Create knows about a project it has just set up.
pub struct NewProjectRecord {
    pub name: String,
    pub location_kind: ProjectLocationKind,
    pub repository_path: String,
    pub default_branch: String,
    pub github_remote: Option<String>,
}

impl StateStore {
    /// Whether a registered project already lives at `path`.
    pub fn has_project_at(&self, path: &str) -> bool {
        let wanted = comparable_path(path);
        self.state
            .projects
            .iter()
            .any(|project| comparable_path(&project.repository_path) == wanted)
    }

    /// Adds the project with its interview, making it active when asked, and saves.
    /// The live state changes only after the snapshot is on disk, so a failed save
    /// leaves the store as it was and Create can be retried.
    pub fn commit_new_project(
        &mut self,
        record: NewProjectRecord,
        interview: Option<PendingInterview>,
        activate: bool,
    ) -> Result<u32, ProjectError> {
        let mut candidate = self.state.clone();
        let identifier = candidate
            .projects
            .iter()
            .map(|project| project.id)
            .max()
            .map_or(1, |highest| highest.saturating_add(1));
        let index =
            u32::try_from(candidate.projects.len()).map_err(|_| ProjectError::IndexOutOfRange)?;
        candidate
            .projects
            .push(project_item(identifier, record, interview));
        if activate {
            candidate.active_project_index = index;
        }
        candidate.revision = self.state.revision.wrapping_add(1);
        write_atomic_snapshot(&self.directory, &candidate)?;
        self.disk_revision = candidate.revision;
        self.state = candidate;
        Ok(identifier)
    }
}

fn project_item(
    identifier: u32,
    record: NewProjectRecord,
    interview: Option<PendingInterview>,
) -> ProjectItem {
    let mut project = ProjectItem::new(identifier, record.name, record.repository_path);
    project.location_kind = record.location_kind;
    project.default_branch = record.default_branch;
    project.github_remote = record.github_remote;
    project.pending_interview = interview;
    project
}

/// Windows paths name the same folder regardless of case, separator style, a
/// trailing separator or the `\\?\` long-path prefix.
fn comparable_path(path: &str) -> String {
    let unified = path.replace('/', "\\");
    let without_prefix = unified.strip_prefix(r"\\?\").unwrap_or(&unified);
    let trimmed = without_prefix.trim_end_matches('\\');
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.replace('\\', "/")
    }
}
