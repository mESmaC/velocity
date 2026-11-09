//! Worktrees settings policies (`SPEC.md` F5, F13).
//!
//! Paths and setup text stay opaque here. Filesystem measurements, native folder
//! picking, persistence and git operations belong at the I/O edges.

/// Settings for the active project, including its optional location override.
#[derive(Clone)]
pub struct WorktreePreferences {
    pub root: String,
    pub project_override: Option<String>,
    pub delete_local_branch: bool,
    pub share_build_cache: bool,
    pub copy_files: String,
    pub setup_command: String,
    pub ready_count: u32,
}

/// An explicit user edit. Lifecycle creation/deletion rules have no setters.
pub enum WorktreePreferenceChange {
    Root(String),
    ProjectOverride(Option<String>),
    DeleteLocalBranch(bool),
    ShareBuildCache(bool),
    CopyFiles(String),
    SetupCommand(String),
    ReadyCount(u32),
}

pub fn changes_location(change: &WorktreePreferenceChange) -> bool {
    matches!(
        change,
        WorktreePreferenceChange::Root(_) | WorktreePreferenceChange::ProjectOverride(_)
    )
}

pub fn update_preferences(
    mut preferences: WorktreePreferences,
    change: WorktreePreferenceChange,
) -> WorktreePreferences {
    match change {
        WorktreePreferenceChange::Root(location) => preferences.root = location,
        WorktreePreferenceChange::ProjectOverride(location) => {
            preferences.project_override = location;
        }
        WorktreePreferenceChange::DeleteLocalBranch(enabled) => {
            preferences.delete_local_branch = enabled;
        }
        WorktreePreferenceChange::ShareBuildCache(enabled) => {
            preferences.share_build_cache = enabled
        }
        WorktreePreferenceChange::CopyFiles(files) => preferences.copy_files = files,
        WorktreePreferenceChange::SetupCommand(command) => preferences.setup_command = command,
        WorktreePreferenceChange::ReadyCount(count) => preferences.ready_count = count,
    }
    preferences
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeDisplayStatus {
    Working,
    PullRequest(u32),
    Failed,
    Closed,
    Review,
    Sweep,
    Deleted,
}

/// Closed and failed worktrees still consume space and count as live (F5).
pub fn is_live(status: WorktreeDisplayStatus) -> bool {
    !matches!(status, WorktreeDisplayStatus::Deleted)
}

/// The reference keeps the last four recently deleted rows after the live rows.
pub fn deleted_row_window_start(recent_deleted_count: usize) -> usize {
    recent_deleted_count.saturating_sub(4)
}
