//! Worktrees settings policies (`SPEC.md` F5, F13).
//!
//! Paths and setup text stay opaque here. Filesystem measurements, native folder
//! picking, persistence and git operations belong at the I/O edges.

/// Settings for the active project, including its optional location override.
pub struct WorktreePreferences<Location, Text> {
    pub root: Location,
    pub project_override: Option<Location>,
    pub delete_local_branch: bool,
    pub share_build_cache: bool,
    pub copy_files: Text,
    pub setup_command: Text,
    pub ready_count: u32,
}

/// An explicit user edit. Lifecycle creation/deletion rules have no setters.
pub enum WorktreePreferenceChange<Location, Text> {
    Root(Location),
    ProjectOverride(Option<Location>),
    DeleteLocalBranch(bool),
    ShareBuildCache(bool),
    CopyFiles(Text),
    SetupCommand(Text),
    ReadyCount(u32),
}

/// The inventory remains fixed while preferences for future worktrees change.
pub struct WorktreeSettingsSnapshot<Location, Text, Entry> {
    pub preferences: WorktreePreferences<Location, Text>,
    pub entries: Vec<Entry>,
    pub location_changed: bool,
}

pub fn changes_location<Location, Text>(change: &WorktreePreferenceChange<Location, Text>) -> bool {
    matches!(
        change,
        WorktreePreferenceChange::Root(_) | WorktreePreferenceChange::ProjectOverride(_)
    )
}

pub fn update_preferences<Location, Text>(
    mut preferences: WorktreePreferences<Location, Text>,
    change: WorktreePreferenceChange<Location, Text>,
) -> WorktreePreferences<Location, Text> {
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

/// Return the location for a future worktree without touching existing paths.
pub fn effective_root<Location, Text>(
    preferences: &WorktreePreferences<Location, Text>,
) -> &Location {
    match &preferences.project_override {
        Some(location) => location,
        None => &preferences.root,
    }
}

pub fn apply_change<Location, Text, Entry>(
    mut snapshot: WorktreeSettingsSnapshot<Location, Text, Entry>,
    change: WorktreePreferenceChange<Location, Text>,
) -> WorktreeSettingsSnapshot<Location, Text, Entry> {
    snapshot.location_changed = snapshot.location_changed || changes_location(&change);
    snapshot.preferences = update_preferences(snapshot.preferences, change);
    snapshot
}

#[derive(Clone, Copy)]
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

pub struct WorktreeMeasurement {
    pub bytes: u64,
    pub status: WorktreeDisplayStatus,
}

pub struct WorktreeTotals {
    pub count: usize,
    pub bytes: u128,
}

/// A slice has at most `usize::MAX` entries and each file measurement fits in
/// `u64`, so `u128` holds their exact sum even on a 64-bit Windows host.
pub fn collect_totals(measurements: &[WorktreeMeasurement]) -> WorktreeTotals {
    let mut totals = WorktreeTotals { count: 0, bytes: 0 };
    let mut index = 0;
    while index < measurements.len() {
        let measurement = &measurements[index];
        if is_live(measurement.status) {
            totals.count += 1;
            totals.bytes += u128::from(measurement.bytes);
        }
        index += 1;
    }
    totals
}

/// The reference keeps the last four recently deleted rows after the live rows.
pub fn deleted_row_window_start(recent_deleted_count: usize) -> usize {
    recent_deleted_count.saturating_sub(4)
}

#[derive(Clone, Copy)]
pub enum WorktreeSettingsAction {
    Refresh,
    ChangeSettings,
    OpenOwner,
    Browse,
    PruneOrphans,
}

/// Only the explicit Prune orphans action can request repository-wide pruning.
pub fn runs_prune(action: WorktreeSettingsAction) -> bool {
    matches!(action, WorktreeSettingsAction::PruneOrphans)
}
