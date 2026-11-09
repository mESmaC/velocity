//! Store updates for Create in the New project dialog (SPEC.md F2). Each one is a
//! step of the plan proven in `proofs/NewProject/`; the caller saves once after the
//! last step so a failed save leaves no half-registered project on disk.

use crate::{PendingInterview, ProjectItem, ProjectLocationKind, StateStore};

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

    /// Adds the project and returns its identifier. It is not active yet.
    pub fn register_project(&mut self, record: NewProjectRecord) -> u32 {
        let identifier = self
            .state
            .projects
            .iter()
            .map(|project| project.id)
            .max()
            .map_or(1, |highest| highest.saturating_add(1));
        self.state.projects.push(ProjectItem {
            id: identifier,
            name: record.name,
            location_kind: record.location_kind,
            repository_path: record.repository_path,
            default_branch: record.default_branch,
            github_remote: record.github_remote,
            spec_path: String::from("SPEC.md"),
            spec_locked: false,
            active_thread_id: None,
            merged_today: 0,
            tasks: Vec::new(),
            threads: Vec::new(),
            pending_interview: None,
        });
        self.changed();
        identifier
    }

    /// Makes the project with `identifier` the active one. False if there is none.
    pub fn activate_project(&mut self, identifier: u32) -> bool {
        let Some(index) = self.index_of(identifier) else {
            return false;
        };
        self.state.active_project_index = u32::try_from(index).unwrap_or(u32::MAX);
        self.changed();
        true
    }

    /// Records the interview the dialog asked for. False if there is no such project.
    pub fn request_interview(&mut self, identifier: u32, interview: PendingInterview) -> bool {
        let Some(index) = self.index_of(identifier) else {
            return false;
        };
        self.state.projects[index].pending_interview = Some(interview);
        self.changed();
        true
    }

    fn index_of(&self, identifier: u32) -> Option<usize> {
        self.state
            .projects
            .iter()
            .position(|project| project.id == identifier)
    }

    fn changed(&mut self) {
        self.state.revision = self.state.revision.wrapping_add(1);
    }
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
