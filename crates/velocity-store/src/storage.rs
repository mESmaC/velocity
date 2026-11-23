//! Split per-project and settled-archive persistence for `StateStore` (#552).
//!
//! Layout inside the application data directory:
//! - `state.json`: compact root metadata (`revision`, `active_project_index`, `settings`,
//!   `next_thread_identifiers`, `thread_started_at`, `project_ids`). Legacy monolithic
//!   `state.json` files with inline `projects` and `transcript` arrays are loaded and
//!   migrated cleanly on startup.
//! - `projects/<id>.json`: one project's active state and non-settled threads (without
//!   transcripts), plus `thread_order` to preserve thread ordering across reloads.
//! - `projects/<id>.settled.json`: archived `merged`/`done` thread summary rows (without
//!   transcripts or `queued_run_history`), rewritten only when settled threads change.
//! - `transcripts/<project>/<thread>.jsonl`: per-thread JSONL transcripts (see `transcripts`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use velocity_core::thread_view_model::SharedBlocks;

use crate::transcripts::{self, PersistedTranscript, is_settled_state};
use crate::{
    PARTIAL_STATE_FILE_NAME, PersistedState, ProjectItem, STATE_FILE_NAME, Settings, StoreError,
    ThreadItem, extended_windows_path, replace_with_retry,
};

pub const PROJECTS_DIR_NAME: &str = "projects";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RootStateFile {
    pub revision: u32,
    pub active_project_index: u32,
    /// Legacy monolithic snapshot field; empty in split-format writes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<ProjectItem>,
    /// Ordered project IDs stored in `projects/<id>.json`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_ids: Vec<u32>,
    pub settings: Settings,
    #[serde(default)]
    pub next_thread_identifiers: BTreeMap<u32, u32>,
    #[serde(default)]
    pub thread_started_at: BTreeMap<u32, BTreeMap<u32, String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProjectActiveFile {
    #[serde(default)]
    pub revision: u32,
    pub project: ProjectItem,
    #[serde(default)]
    pub thread_order: Vec<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ProjectSettledFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
    #[serde(default)]
    pub threads: Vec<ThreadItem>,
}

#[derive(Deserialize)]
struct RootProjectIndex {
    #[serde(default)]
    projects: Vec<ProjectItem>,
    #[serde(default)]
    project_ids: Vec<u32>,
}

/// Read-only lookup of one project from the atomically replaced snapshot files (#562).
///
/// Supports both split persistence (`state.json` + `projects/<id>.json` [+ optional
/// `projects/<id>.settled.json`]) and legacy monolithic `state.json` without mutating,
/// migrating, or recovering files on disk. Safe for concurrent MCP child processes.
pub fn read_project_snapshot(
    directory: &Path,
    project_id: u32,
    include_settled: bool,
) -> Result<Option<ProjectItem>, StoreError> {
    let root_path = extended_windows_path(&directory.join(STATE_FILE_NAME));
    let bytes = fs::read(&root_path).map_err(|source| StoreError::ReadSnapshot {
        path: root_path.display().to_string(),
        source,
    })?;
    let root: RootProjectIndex = serde_json::from_slice(&bytes)
        .map_err(|source| StoreError::SerializeSnapshot { source })?;
    if !root.projects.is_empty() {
        return Ok(root
            .projects
            .into_iter()
            .find(|project| project.id == project_id)
            .map(ProjectItem::normalize));
    }
    if !root.project_ids.contains(&project_id) {
        return Ok(None);
    }
    let project = read_split_project_readonly(directory, project_id, include_settled)?;
    Ok(Some(project.normalize()))
}

/// Read-only list of all configured projects from the atomically replaced snapshot files (#598).
///
/// Supports both split persistence (`state.json` + `projects/<id>.json`) and legacy
/// monolithic `state.json` without mutating, migrating, or recovering files on disk.
/// Safe for concurrent MCP child processes.
pub fn read_all_project_snapshots(
    directory: &Path,
    include_settled: bool,
) -> Result<Vec<ProjectItem>, StoreError> {
    let root_path = extended_windows_path(&directory.join(STATE_FILE_NAME));
    let bytes = fs::read(&root_path).map_err(|source| StoreError::ReadSnapshot {
        path: root_path.display().to_string(),
        source,
    })?;
    let root: RootProjectIndex = serde_json::from_slice(&bytes)
        .map_err(|source| StoreError::SerializeSnapshot { source })?;
    if !root.projects.is_empty() {
        return Ok(root
            .projects
            .into_iter()
            .map(ProjectItem::normalize)
            .collect());
    }
    let mut projects = Vec::with_capacity(root.project_ids.len());
    for project_id in root.project_ids {
        match read_split_project_readonly(directory, project_id, include_settled) {
            Ok(project) => projects.push(project.normalize()),
            Err(StoreError::ReadSnapshot { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(projects)
}

fn read_split_project_readonly(
    directory: &Path,
    project_id: u32,
    include_settled: bool,
) -> Result<ProjectItem, StoreError> {
    let active_path = project_active_path(directory, project_id);
    let active_bytes = fs::read(&active_path).map_err(|source| StoreError::ReadSnapshot {
        path: active_path.display().to_string(),
        source,
    })?;
    let active_file: ProjectActiveFile = serde_json::from_slice(&active_bytes)
        .map_err(|source| StoreError::SerializeSnapshot { source })?;
    let mut project = active_file.project;
    if !include_settled {
        return Ok(project);
    }
    let settled_path = project_settled_path(directory, project_id);
    let settled_file = match fs::read(&settled_path) {
        Ok(bytes) => serde_json::from_slice::<ProjectSettledFile>(&bytes)
            .map_err(|source| StoreError::SerializeSnapshot { source })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProjectSettledFile::default(),
        Err(source) => {
            return Err(StoreError::ReadSnapshot {
                path: settled_path.display().to_string(),
                source,
            });
        }
    };
    let settled_wins = settled_is_newer(settled_file.revision, active_file.revision);
    project.threads = reassemble_threads(
        project.threads,
        settled_file.threads,
        &active_file.thread_order,
        settled_wins,
    );
    Ok(project)
}

pub(crate) struct LoadedState {
    pub state: PersistedState,
    pub persisted_transcripts: BTreeMap<(u32, u32), PersistedTranscript>,
    pub recovered_corrupt_snapshot: Option<PathBuf>,
}

/// Failure from [`write_incremental_snapshot`] together with any transcripts that
/// already landed on disk and the transcript whose write failed (#700).
pub(crate) struct SnapshotWriteFailure {
    pub error: StoreError,
    pub written_transcripts: BTreeMap<(u32, u32), Option<PersistedTranscript>>,
    pub failed_transcript: Option<(u32, u32)>,
}

impl From<StoreError> for SnapshotWriteFailure {
    fn from(error: StoreError) -> Self {
        Self {
            error,
            written_transcripts: BTreeMap::new(),
            failed_transcript: None,
        }
    }
}

fn project_active_path(directory: &Path, project_id: u32) -> PathBuf {
    extended_windows_path(
        &directory
            .join(PROJECTS_DIR_NAME)
            .join(format!("{project_id}.json")),
    )
}

fn project_active_partial_path(directory: &Path, project_id: u32) -> PathBuf {
    extended_windows_path(
        &directory
            .join(PROJECTS_DIR_NAME)
            .join(format!("{project_id}.json.partial")),
    )
}

fn project_settled_path(directory: &Path, project_id: u32) -> PathBuf {
    extended_windows_path(
        &directory
            .join(PROJECTS_DIR_NAME)
            .join(format!("{project_id}.settled.json")),
    )
}

fn project_settled_partial_path(directory: &Path, project_id: u32) -> PathBuf {
    extended_windows_path(
        &directory
            .join(PROJECTS_DIR_NAME)
            .join(format!("{project_id}.settled.json.partial")),
    )
}

fn write_atomic_bytes(partial: &Path, destination: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|source| StoreError::CreateDirectory {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let mut partial_file = File::create(partial).map_err(|source| StoreError::WritePartial {
        path: partial.display().to_string(),
        source,
    })?;
    partial_file
        .write_all(bytes)
        .map_err(|source| StoreError::WritePartial {
            path: partial.display().to_string(),
            source,
        })?;
    partial_file
        .sync_all()
        .map_err(|source| StoreError::SyncPartial {
            path: partial.display().to_string(),
            source,
        })?;
    drop(partial_file);
    replace_with_retry(partial, destination).map_err(|source| StoreError::ReplaceSnapshot {
        partial: partial.display().to_string(),
        destination: destination.display().to_string(),
        source,
    })
}

fn recover_partial_json<T: serde::de::DeserializeOwned>(
    partial: &Path,
    destination: &Path,
) -> Result<(), StoreError> {
    if !partial.exists() {
        return Ok(());
    }
    if destination.exists() {
        let _ignored = fs::remove_file(partial);
        return Ok(());
    }
    if let Ok(bytes) = fs::read(partial)
        && serde_json::from_slice::<T>(&bytes).is_ok()
    {
        replace_with_retry(partial, destination).map_err(|source| StoreError::ReplaceSnapshot {
            partial: partial.display().to_string(),
            destination: destination.display().to_string(),
            source,
        })?;
        return Ok(());
    }
    let _ignored = fs::remove_file(partial);
    Ok(())
}

fn root_file_from_state(state: &PersistedState) -> RootStateFile {
    RootStateFile {
        revision: state.revision,
        active_project_index: state.active_project_index,
        projects: Vec::new(),
        project_ids: state.projects.iter().map(|project| project.id).collect(),
        settings: state.settings.clone(),
        next_thread_identifiers: state.next_thread_identifiers.clone(),
        thread_started_at: state.thread_started_at.clone(),
    }
}

fn active_file_from_project(project: &ProjectItem, revision: u32) -> ProjectActiveFile {
    let thread_order = project.threads.iter().map(|thread| thread.id).collect();
    let mut active_project = project.clone();
    active_project.threads = project
        .threads
        .iter()
        .filter(|thread| !is_settled_state(thread.state))
        .map(strip_transcript)
        .collect();
    ProjectActiveFile {
        revision,
        project: active_project,
        thread_order,
    }
}

fn settled_file_from_project(project: &ProjectItem, revision: Option<u32>) -> ProjectSettledFile {
    let threads = project
        .threads
        .iter()
        .filter(|thread| is_settled_state(thread.state))
        .map(archive_settled_thread)
        .collect();
    ProjectSettledFile { revision, threads }
}

fn strip_transcript(thread: &ThreadItem) -> ThreadItem {
    let mut copy = thread.clone();
    copy.transcript = SharedBlocks::default();
    copy
}

fn archive_settled_thread(thread: &ThreadItem) -> ThreadItem {
    let mut copy = thread.clone();
    copy.transcript = SharedBlocks::default();
    copy.queued_run_history.clear();
    copy
}

fn write_root_file(directory: &Path, state: &PersistedState) -> Result<(), StoreError> {
    let root = root_file_from_state(state);
    let bytes =
        serde_json::to_vec(&root).map_err(|source| StoreError::SerializeSnapshot { source })?;
    let partial = extended_windows_path(&directory.join(PARTIAL_STATE_FILE_NAME));
    let destination = extended_windows_path(&directory.join(STATE_FILE_NAME));
    write_atomic_bytes(&partial, &destination, &bytes)
}

fn write_settled_file(
    directory: &Path,
    project_id: u32,
    settled: &ProjectSettledFile,
) -> Result<(), StoreError> {
    let settled_path = project_settled_path(directory, project_id);
    let partial_path = project_settled_partial_path(directory, project_id);
    if settled.threads.is_empty() {
        let _ignored = fs::remove_file(&partial_path);
        let _ignored = fs::remove_file(&settled_path);
        return Ok(());
    }
    let settled_bytes =
        serde_json::to_vec(settled).map_err(|source| StoreError::SerializeSnapshot { source })?;
    write_atomic_bytes(&partial_path, &settled_path, &settled_bytes)
}

fn write_project_files(
    directory: &Path,
    project: &ProjectItem,
    revision: u32,
) -> Result<(), StoreError> {
    let active = active_file_from_project(project, revision);
    let active_bytes =
        serde_json::to_vec(&active).map_err(|source| StoreError::SerializeSnapshot { source })?;
    write_atomic_bytes(
        &project_active_partial_path(directory, project.id),
        &project_active_path(directory, project.id),
        &active_bytes,
    )?;
    let settled = settled_file_from_project(project, Some(revision));
    write_settled_file(directory, project.id, &settled)
}

fn remove_project_files(directory: &Path, project_id: u32) {
    let _ignored = fs::remove_file(project_active_partial_path(directory, project_id));
    let _ignored = fs::remove_file(project_active_path(directory, project_id));
    let _ignored = fs::remove_file(project_settled_partial_path(directory, project_id));
    let _ignored = fs::remove_file(project_settled_path(directory, project_id));
    let _ignored = fs::remove_dir(extended_windows_path(&directory.join(PROJECTS_DIR_NAME)));
}

fn move_corrupt_snapshot_aside(
    directory: &Path,
    snapshot_path: &Path,
) -> Result<PathBuf, StoreError> {
    let mut backup_index: u32 = 1;
    let corrupt_path = loop {
        let candidate = extended_windows_path(
            &directory.join(format!("{STATE_FILE_NAME}.corrupt-{backup_index}")),
        );
        if !candidate.exists() {
            break candidate;
        }
        backup_index = backup_index.saturating_add(1);
    };
    fs::rename(snapshot_path, &corrupt_path).map_err(|source| {
        StoreError::QuarantineCorruptSnapshot {
            corrupt: corrupt_path.display().to_string(),
            destination: snapshot_path.display().to_string(),
            source,
        }
    })?;
    Ok(corrupt_path)
}

pub(crate) fn load_or_initialize(directory: &Path) -> Result<LoadedState, StoreError> {
    let partial = extended_windows_path(&directory.join(PARTIAL_STATE_FILE_NAME));
    let destination = extended_windows_path(&directory.join(STATE_FILE_NAME));
    recover_partial_json::<RootStateFile>(&partial, &destination)?;
    if !destination.exists() {
        return initialize_fresh(directory, None);
    }
    let bytes = fs::read(&destination).map_err(|source| StoreError::ReadSnapshot {
        path: destination.display().to_string(),
        source,
    })?;
    match serde_json::from_slice::<RootStateFile>(&bytes) {
        Ok(root) => load_parsed_root(directory, root),
        Err(parse_error) => {
            let corrupt_path = move_corrupt_snapshot_aside(directory, &destination)?;
            eprintln!(
                "Quarantined unreadable state file '{}' to '{}' ({parse_error}); starting from defaults.",
                destination.display(),
                corrupt_path.display()
            );
            initialize_fresh(directory, Some(corrupt_path))
        }
    }
}

fn initialize_fresh(
    directory: &Path,
    recovered_corrupt_snapshot: Option<PathBuf>,
) -> Result<LoadedState, StoreError> {
    let state = PersistedState::default().normalize();
    write_root_file(directory, &state)?;
    Ok(LoadedState {
        state,
        persisted_transcripts: BTreeMap::new(),
        recovered_corrupt_snapshot,
    })
}

fn load_parsed_root(directory: &Path, root: RootStateFile) -> Result<LoadedState, StoreError> {
    if !root.projects.is_empty() {
        return migrate_legacy_root(directory, root);
    }
    let mut persisted_transcripts = BTreeMap::new();
    let mut projects = Vec::with_capacity(root.project_ids.len());
    let mut max_revision = root.revision;
    for project_id in &root.project_ids {
        let (project, project_revision) =
            load_split_project(directory, *project_id, &mut persisted_transcripts)?;
        if (project_revision.wrapping_sub(max_revision) as i32) > 0 {
            max_revision = project_revision;
        }
        projects.push(project);
    }
    let state = PersistedState {
        revision: max_revision,
        active_project_index: root.active_project_index,
        projects,
        settings: root.settings,
        next_thread_identifiers: root.next_thread_identifiers,
        thread_started_at: root.thread_started_at,
    }
    .normalize();
    Ok(LoadedState {
        state,
        persisted_transcripts,
        recovered_corrupt_snapshot: None,
    })
}

fn migrate_legacy_root(directory: &Path, root: RootStateFile) -> Result<LoadedState, StoreError> {
    let mut persisted_transcripts = BTreeMap::new();
    let mut projects = root.projects;
    for project in &mut projects {
        for thread in &mut project.threads {
            if !thread.transcript.is_empty() {
                let persisted = transcripts::write_transcript_persisted(
                    directory,
                    project.id,
                    thread.id,
                    None,
                    &thread.transcript,
                )?;
                if is_settled_state(thread.state) {
                    thread.transcript = SharedBlocks::default();
                    thread.queued_run_history.clear();
                } else if let Some(persisted) = persisted {
                    persisted_transcripts.insert((project.id, thread.id), persisted);
                }
            } else if !is_settled_state(thread.state) {
                if let Some(loaded) =
                    transcripts::read_transcript_persisted(directory, project.id, thread.id)?
                {
                    thread.transcript = Arc::clone(&loaded.blocks);
                    persisted_transcripts.insert((project.id, thread.id), loaded);
                }
            } else {
                thread.queued_run_history.clear();
            }
        }
    }
    let state = PersistedState {
        revision: root.revision,
        active_project_index: root.active_project_index,
        projects,
        settings: root.settings,
        next_thread_identifiers: root.next_thread_identifiers,
        thread_started_at: root.thread_started_at,
    }
    .normalize();
    for project in &state.projects {
        write_project_files(directory, project, state.revision)?;
    }
    write_root_file(directory, &state)?;
    Ok(LoadedState {
        state,
        persisted_transcripts,
        recovered_corrupt_snapshot: None,
    })
}

fn load_settled_file(directory: &Path, project_id: u32) -> Result<ProjectSettledFile, StoreError> {
    let settled_path = project_settled_path(directory, project_id);
    recover_partial_json::<ProjectSettledFile>(
        &project_settled_partial_path(directory, project_id),
        &settled_path,
    )?;
    if !settled_path.exists() {
        return Ok(ProjectSettledFile::default());
    }
    let settled_bytes = fs::read(&settled_path).map_err(|source| StoreError::ReadSnapshot {
        path: settled_path.display().to_string(),
        source,
    })?;
    serde_json::from_slice(&settled_bytes)
        .map_err(|source| StoreError::SerializeSnapshot { source })
}

fn reconcile_overlapping_project_files(
    directory: &Path,
    project: &ProjectItem,
    revision: u32,
    settled_wins: bool,
) -> Result<(), StoreError> {
    if settled_wins {
        let active = active_file_from_project(project, revision);
        let active_bytes = serde_json::to_vec(&active)
            .map_err(|source| StoreError::SerializeSnapshot { source })?;
        write_atomic_bytes(
            &project_active_partial_path(directory, project.id),
            &project_active_path(directory, project.id),
            &active_bytes,
        )
    } else {
        let settled = settled_file_from_project(project, Some(revision));
        write_settled_file(directory, project.id, &settled)
    }
}

fn load_split_project(
    directory: &Path,
    project_id: u32,
    persisted_transcripts: &mut BTreeMap<(u32, u32), PersistedTranscript>,
) -> Result<(ProjectItem, u32), StoreError> {
    let active_path = project_active_path(directory, project_id);
    recover_partial_json::<ProjectActiveFile>(
        &project_active_partial_path(directory, project_id),
        &active_path,
    )?;
    let bytes = fs::read(&active_path).map_err(|source| StoreError::ReadSnapshot {
        path: active_path.display().to_string(),
        source,
    })?;
    let active_file: ProjectActiveFile = serde_json::from_slice(&bytes)
        .map_err(|source| StoreError::SerializeSnapshot { source })?;
    let settled_file = load_settled_file(directory, project_id)?;
    let settled_wins = settled_is_newer(settled_file.revision, active_file.revision);
    let effective_revision = match settled_file.revision {
        Some(revision) if settled_wins => revision,
        _ => active_file.revision,
    };
    let mut project = active_file.project;
    let active_threads = std::mem::take(&mut project.threads);
    let has_overlap = active_threads
        .iter()
        .any(|active| settled_file.threads.iter().any(|s| s.id == active.id));
    project.threads = reassemble_threads(
        active_threads,
        settled_file.threads,
        &active_file.thread_order,
        settled_wins,
    );
    if has_overlap {
        reconcile_overlapping_project_files(directory, &project, effective_revision, settled_wins)?;
    }
    for thread in &mut project.threads {
        if thread.transcript.is_empty()
            && !is_settled_state(thread.state)
            && let Some(loaded) =
                transcripts::read_transcript_persisted(directory, project_id, thread.id)?
        {
            thread.transcript = Arc::clone(&loaded.blocks);
            persisted_transcripts.insert((project_id, thread.id), loaded);
        }
    }
    Ok((project, effective_revision))
}

fn settled_is_newer(settled_revision: Option<u32>, active_revision: u32) -> bool {
    settled_revision.is_some_and(|revision| (revision.wrapping_sub(active_revision) as i32) > 0)
}

fn reassemble_threads(
    active: Vec<ThreadItem>,
    settled: Vec<ThreadItem>,
    order: &[u32],
    settled_wins_duplicates: bool,
) -> Vec<ThreadItem> {
    let mut by_id: BTreeMap<u32, ThreadItem> = BTreeMap::new();
    let ordered_sources = if settled_wins_duplicates {
        [active, settled]
    } else {
        [settled, active]
    };
    for source in ordered_sources {
        for thread in source {
            by_id.insert(thread.id, thread);
        }
    }
    let mut ordered = Vec::with_capacity(by_id.len());
    for id in order {
        if let Some(thread) = by_id.remove(id) {
            ordered.push(thread);
        }
    }
    ordered.extend(by_id.into_values());
    ordered
}

fn root_metadata_changed(previous: &PersistedState, current: &PersistedState) -> bool {
    previous.active_project_index != current.active_project_index
        || previous.settings != current.settings
        || previous.next_thread_identifiers != current.next_thread_identifiers
        || previous.thread_started_at != current.thread_started_at
        || previous.projects.len() != current.projects.len()
        || previous
            .projects
            .iter()
            .zip(&current.projects)
            .any(|(left, right)| left.id != right.id)
        || (current.projects.is_empty() && previous.revision != current.revision)
}

fn settled_threads_equal(previous: &ProjectItem, current: &ProjectItem) -> bool {
    let mut prev_iter = previous
        .threads
        .iter()
        .filter(|thread| is_settled_state(thread.state));
    let mut curr_iter = current
        .threads
        .iter()
        .filter(|thread| is_settled_state(thread.state));
    loop {
        match (prev_iter.next(), curr_iter.next()) {
            (None, None) => return true,
            (Some(left), Some(right)) => {
                if left != right {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// Incrementally writes only the root/project/settled/transcript files that actually
/// changed between `previous` and `current` (#552, #700).
pub(crate) fn write_incremental_snapshot(
    directory: &Path,
    previous: &PersistedState,
    current: &PersistedState,
    pending_transcripts: &BTreeMap<(u32, u32), SharedBlocks>,
    deleted_transcripts: &BTreeSet<(u32, u32)>,
    previous_transcripts: &BTreeMap<(u32, u32), PersistedTranscript>,
) -> Result<BTreeMap<(u32, u32), Option<PersistedTranscript>>, SnapshotWriteFailure> {
    let root_changed = root_metadata_changed(previous, current);
    let mut newly_created_projects = Vec::new();
    for project in &current.projects {
        let previous_project = previous.projects.iter().find(|item| item.id == project.id);
        let is_new = previous_project.is_none();
        if let Some(prev) = previous_project
            && prev == project
        {
            continue;
        }
        write_changed_project(directory, previous_project, project, current.revision)?;
        if is_new {
            newly_created_projects.push(project.id);
        }
    }
    if root_changed && let Err(error) = write_root_file(directory, current) {
        for project_id in newly_created_projects {
            remove_project_files(directory, project_id);
        }
        return Err(error.into());
    }
    for previous_project in &previous.projects {
        if !current
            .projects
            .iter()
            .any(|project| project.id == previous_project.id)
        {
            remove_project_files(directory, previous_project.id);
        }
    }
    let written_transcripts =
        write_pending_transcripts(directory, pending_transcripts, previous_transcripts)?;
    for &(project_id, thread_id) in deleted_transcripts {
        transcripts::remove_transcript(directory, project_id, thread_id);
    }
    Ok(written_transcripts)
}

fn write_changed_project(
    directory: &Path,
    previous: Option<&ProjectItem>,
    current: &ProjectItem,
    revision: u32,
) -> Result<(), StoreError> {
    write_changed_project_with_hook(directory, previous, current, revision, &mut |_| Ok(()))
}

fn reopened_settled_threads(
    previous: Option<&ProjectItem>,
    current: &ProjectItem,
) -> Vec<ThreadItem> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    let current_active_ids: BTreeSet<u32> = current
        .threads
        .iter()
        .filter(|thread| !is_settled_state(thread.state))
        .map(|thread| thread.id)
        .collect();
    previous
        .threads
        .iter()
        .filter(|thread| is_settled_state(thread.state) && current_active_ids.contains(&thread.id))
        .map(archive_settled_thread)
        .collect()
}

fn has_newly_settled_threads(previous: Option<&ProjectItem>, current: &ProjectItem) -> bool {
    let Some(previous) = previous else {
        return current
            .threads
            .iter()
            .any(|thread| is_settled_state(thread.state));
    };
    let previous_active_ids: BTreeSet<u32> = previous
        .threads
        .iter()
        .filter(|thread| !is_settled_state(thread.state))
        .map(|thread| thread.id)
        .collect();
    current
        .threads
        .iter()
        .any(|thread| is_settled_state(thread.state) && previous_active_ids.contains(&thread.id))
}

fn serialized_active_if_changed(
    previous: Option<&ProjectItem>,
    current: &ProjectItem,
    revision: u32,
) -> Result<Option<Vec<u8>>, StoreError> {
    let active_file = active_file_from_project(current, revision);
    let active_bytes = serde_json::to_vec(&active_file)
        .map_err(|source| StoreError::SerializeSnapshot { source })?;
    let changed = match previous {
        None => true,
        Some(prev) => {
            // Compare with the same revision so a transcript-only update doesn't rewrite
            // `projects/<id>.json` solely because the revision bumped.
            let prev_file = active_file_from_project(prev, revision);
            let prev_bytes = serde_json::to_vec(&prev_file)
                .map_err(|source| StoreError::SerializeSnapshot { source })?;
            prev_bytes != active_bytes
        }
    };
    Ok(changed.then_some(active_bytes))
}

fn write_pre_active_settled(
    directory: &Path,
    project_id: u32,
    final_settled: &ProjectSettledFile,
    newly_settled: bool,
    reopened: Vec<ThreadItem>,
) -> Result<Option<bool>, StoreError> {
    if !newly_settled {
        return Ok(None);
    }
    if reopened.is_empty() {
        write_settled_file(directory, project_id, final_settled)?;
        return Ok(Some(true));
    }
    let mut bridge = final_settled.clone();
    bridge.revision = None;
    bridge.threads.extend(reopened);
    write_settled_file(directory, project_id, &bridge)?;
    Ok(Some(false))
}

/// Writes `projects/<id>.json` and `projects/<id>.settled.json` with copy-before-delete
/// ordering so no thread can vanish across an interrupted save boundary (#701).
fn write_changed_project_with_hook(
    directory: &Path,
    previous: Option<&ProjectItem>,
    current: &ProjectItem,
    revision: u32,
    on_after_write: &mut impl FnMut(usize) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let settled_changed = previous.is_none_or(|prev| !settled_threads_equal(prev, current));
    let active_bytes = serialized_active_if_changed(previous, current, revision)?;
    let settled = settled_changed.then(|| settled_file_from_project(current, Some(revision)));
    let reopened = if settled_changed {
        reopened_settled_threads(previous, current)
    } else {
        Vec::new()
    };
    let newly_settled = settled_changed && has_newly_settled_threads(previous, current);
    let mut completed_writes = 0usize;

    // Copy-before-delete across the active/settled split: write settled before active when
    // active threads settle (using a revision-less bridge archive if other settled threads
    // reopen in the same save), and write active before pruning/removing settled otherwise.
    let wrote_final_settled = match settled.as_ref() {
        Some(final_settled) => {
            match write_pre_active_settled(
                directory,
                current.id,
                final_settled,
                newly_settled,
                reopened,
            )? {
                Some(is_final) => {
                    completed_writes += 1;
                    on_after_write(completed_writes)?;
                    is_final
                }
                None => false,
            }
        }
        None => false,
    };

    if let Some(bytes) = active_bytes {
        write_atomic_bytes(
            &project_active_partial_path(directory, current.id),
            &project_active_path(directory, current.id),
            &bytes,
        )?;
        completed_writes += 1;
        on_after_write(completed_writes)?;
    }

    if let Some(ref final_settled) = settled
        && !wrote_final_settled
    {
        write_settled_file(directory, current.id, final_settled)?;
        completed_writes += 1;
        on_after_write(completed_writes)?;
    }
    Ok(())
}

fn write_pending_transcripts(
    directory: &Path,
    pending_transcripts: &BTreeMap<(u32, u32), SharedBlocks>,
    previous_transcripts: &BTreeMap<(u32, u32), PersistedTranscript>,
) -> Result<BTreeMap<(u32, u32), Option<PersistedTranscript>>, SnapshotWriteFailure> {
    let mut written_transcripts = BTreeMap::new();
    for (&key, blocks) in pending_transcripts {
        let (project_id, thread_id) = key;
        let persisted = match transcripts::write_transcript_persisted(
            directory,
            project_id,
            thread_id,
            previous_transcripts.get(&key),
            blocks,
        ) {
            Ok(persisted) => persisted,
            Err(error) => {
                return Err(SnapshotWriteFailure {
                    error,
                    written_transcripts,
                    failed_transcript: Some(key),
                });
            }
        };
        written_transcripts.insert(key, persisted);
    }
    Ok(written_transcripts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StateStore, ThreadState};

    fn temp_store_dir(tag: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-storage-{tag}-{unique}"));
        let _ignored = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory)?;
        Ok(directory)
    }

    // Risk (#562): MCP child processes read persisted project state after #552 split
    // storage omits `projects` from `state.json`; both split and legacy layouts must load
    // read-only without mutating files on disk.
    #[test]
    fn read_project_snapshot_handles_split_and_legacy_layouts()
    -> Result<(), Box<dyn std::error::Error>> {
        let split_dir = temp_store_dir("split")?;
        let mut legacy = PersistedState::default();
        let mut project = ProjectItem::new(7, "collapse.sh".into(), r"C:\collapse".into());
        project.github_remote = Some("mESmaC/collapse.sh".into());
        let mut settled = ThreadItem::new(1, "Settled".into());
        settled.state = ThreadState::Merged;
        let mut active = ThreadItem::new(2, "Active".into());
        active.state = ThreadState::Working;
        project.threads = vec![settled, active];
        legacy.projects.push(project);
        fs::write(
            split_dir.join(STATE_FILE_NAME),
            serde_json::to_vec(&legacy)?,
        )?;

        // Migrate to split layout through StateStore; root state.json omits `projects`.
        let _store = StateStore::load_or_default(&split_dir)?;
        let root_json: serde_json::Value =
            serde_json::from_slice(&fs::read(split_dir.join(STATE_FILE_NAME))?)?;
        assert!(root_json.get("projects").is_none());

        let active_only = read_project_snapshot(&split_dir, 7, false)?
            .ok_or("expected split project without settled threads")?;
        assert_eq!(
            active_only.github_remote.as_deref(),
            Some("mESmaC/collapse.sh")
        );
        assert_eq!(active_only.threads.len(), 1);
        assert_eq!(active_only.threads[0].id, 2);

        let with_settled = read_project_snapshot(&split_dir, 7, true)?
            .ok_or("expected split project with settled threads")?;
        let ids: Vec<u32> = with_settled.threads.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert!(read_project_snapshot(&split_dir, 99, false)?.is_none());
        let all_split = read_all_project_snapshots(&split_dir, false)?;
        assert_eq!(all_split.len(), 1);
        assert_eq!(
            all_split[0].github_remote.as_deref(),
            Some("mESmaC/collapse.sh")
        );

        // Legacy monolithic state.json must load without creating a split projects/ folder.
        let legacy_dir = temp_store_dir("legacy")?;
        fs::write(
            legacy_dir.join(STATE_FILE_NAME),
            serde_json::to_vec(&legacy)?,
        )?;
        let legacy_loaded =
            read_project_snapshot(&legacy_dir, 7, false)?.ok_or("expected legacy project")?;
        assert_eq!(legacy_loaded.id, 7);
        let all_legacy = read_all_project_snapshots(&legacy_dir, false)?;
        assert_eq!(all_legacy.len(), 1);
        assert_eq!(all_legacy[0].id, 7);
        assert!(!legacy_dir.join(PROJECTS_DIR_NAME).exists());

        let _ignored = fs::remove_dir_all(&split_dir);
        let _ignored = fs::remove_dir_all(&legacy_dir);
        Ok(())
    }

    fn simulated_interrupt() -> StoreError {
        StoreError::WritePartial {
            path: "simulated-interrupt".into(),
            source: std::io::Error::other("simulated interrupt"),
        }
    }

    fn thread_with_transcript(id: u32, title: &str, state: ThreadState) -> ThreadItem {
        let mut thread = ThreadItem::new(id, title.into());
        thread.state = state;
        thread.transcript = Arc::new(vec![
            velocity_core::thread_view_model::TranscriptBlock::UserMessage {
                text: format!("Prompt for thread {id}"),
                queued: false,
            },
        ]);
        thread
    }

    fn seed_project_store(
        tag: &str,
        threads: Vec<ThreadItem>,
    ) -> Result<(PathBuf, ProjectItem, u32), Box<dyn std::error::Error>> {
        let directory = temp_store_dir(tag)?;
        let mut store = StateStore::load_or_default(&directory)?;
        let mut next = store.state().clone();
        let mut project = ProjectItem::new(1, "Velocity".into(), r"C:\velocity".into());
        project.threads = threads;
        next.projects.push(project);
        store.store_snapshot(next)?;
        let saved = store.project(1).ok_or("missing seeded project")?.clone();
        let revision = store.state().revision;
        Ok((directory, saved, revision))
    }

    // Risk (#701, #552, SPEC §2.2, F6): an interrupted save while reopening a settled
    // thread must never leave the thread missing from both project files; restart and
    // read-only snapshots must recover the reopened thread with its transcript and
    // reconcile stale settled rows so later deletes cannot resurrect them.
    #[test]
    fn interrupted_reopen_preserves_thread_across_every_write_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, previous, prev_rev) = seed_project_store(
            "reopen-crash",
            vec![thread_with_transcript(7, "Reverted", ThreadState::Merged)],
        )?;
        assert!(project_settled_path(&directory, 1).exists());

        let mut reopened = previous.clone();
        reopened.threads[0].state = ThreadState::Working;
        let result = write_changed_project_with_hook(
            &directory,
            Some(&previous),
            &reopened,
            prev_rev.wrapping_add(1),
            &mut |step| {
                if step == 1 {
                    return Err(simulated_interrupt());
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        // Step 1 replaced the active file first; the old settled file is still intact.
        assert!(project_settled_path(&directory, 1).exists());

        let readonly =
            read_project_snapshot(&directory, 1, true)?.ok_or("missing readonly snapshot")?;
        assert_eq!(readonly.threads.len(), 1);
        assert_eq!(readonly.threads[0].id, 7);
        assert_eq!(readonly.threads[0].state, ThreadState::Working);

        // Restart recovers the reopened thread + transcript and cleans up stale settled file.
        let mut recovered = StateStore::load_or_default(&directory)?;
        assert!(!project_settled_path(&directory, 1).exists());
        let thread = &recovered
            .project(1)
            .ok_or("missing recovered project")?
            .threads[0];
        assert_eq!(thread.id, 7);
        assert_eq!(thread.state, ThreadState::Working);
        assert_eq!(thread.transcript.len(), 1);

        // Deleting the recovered thread must not resurrect the stale settled row on next start.
        let mut deleted = recovered.state().clone();
        deleted.projects[0].threads.clear();
        recovered.store_snapshot(deleted)?;
        let after_delete = StateStore::load_or_default(&directory)?;
        assert!(
            after_delete
                .project(1)
                .ok_or("missing project")?
                .threads
                .is_empty()
        );

        let _ignored = fs::remove_dir_all(&directory);
        Ok(())
    }

    // Risk (#701, #552): an interrupted save while settling an active thread must write the
    // settled archive before removing the row from the active file and reconcile the stale
    // active copy on restart without regressing revision.
    #[test]
    fn interrupted_settle_preserves_thread_across_every_write_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, previous, prev_rev) = seed_project_store(
            "settle-crash",
            vec![thread_with_transcript(7, "Landing", ThreadState::Working)],
        )?;
        assert!(!project_settled_path(&directory, 1).exists());

        let mut settled = previous.clone();
        settled.threads[0].state = ThreadState::Merged;
        let next_rev = prev_rev.wrapping_add(1);
        let result = write_changed_project_with_hook(
            &directory,
            Some(&previous),
            &settled,
            next_rev,
            &mut |step| {
                if step == 1 {
                    return Err(simulated_interrupt());
                }
                Ok(())
            },
        );
        assert!(result.is_err());

        let readonly =
            read_project_snapshot(&directory, 1, true)?.ok_or("missing readonly snapshot")?;
        assert_eq!(readonly.threads.len(), 1);
        assert_eq!(readonly.threads[0].id, 7);
        assert_eq!(readonly.threads[0].state, ThreadState::Merged);

        let recovered = StateStore::load_or_default(&directory)?;
        assert_eq!(recovered.state().revision, next_rev);
        let thread = &recovered
            .project(1)
            .ok_or("missing recovered project")?
            .threads[0];
        assert_eq!(thread.id, 7);
        assert_eq!(thread.state, ThreadState::Merged);
        // Active file was reconciled to drop the stale active copy.
        let active_only =
            read_project_snapshot(&directory, 1, false)?.ok_or("missing active snapshot")?;
        assert!(active_only.threads.is_empty());

        let _ignored = fs::remove_dir_all(&directory);
        Ok(())
    }

    // Risk (#701, #552): a deferred save batch that simultaneously reopens one settled
    // thread and settles another active thread must preserve both rows at every write step.
    #[test]
    fn interrupted_simultaneous_reopen_and_settle_preserves_both_threads()
    -> Result<(), Box<dyn std::error::Error>> {
        for interrupt_after in [1usize, 2] {
            let (directory, previous, prev_rev) = seed_project_store(
                &format!("both-crash-{interrupt_after}"),
                vec![
                    thread_with_transcript(7, "Reverted", ThreadState::Merged),
                    thread_with_transcript(8, "Landing", ThreadState::Working),
                ],
            )?;
            let mut next = previous.clone();
            next.threads[0].state = ThreadState::Working;
            next.threads[1].state = ThreadState::Merged;
            let result = write_changed_project_with_hook(
                &directory,
                Some(&previous),
                &next,
                prev_rev.wrapping_add(1),
                &mut |step| {
                    if step == interrupt_after {
                        return Err(simulated_interrupt());
                    }
                    Ok(())
                },
            );
            assert!(result.is_err());

            let expected = if interrupt_after == 1 {
                [(7, ThreadState::Merged), (8, ThreadState::Working)]
            } else {
                [(7, ThreadState::Working), (8, ThreadState::Merged)]
            };
            let readonly =
                read_project_snapshot(&directory, 1, true)?.ok_or("missing readonly snapshot")?;
            let readonly_states: Vec<(u32, ThreadState)> = readonly
                .threads
                .iter()
                .map(|thread| (thread.id, thread.state))
                .collect();
            assert_eq!(readonly_states, expected);

            let recovered = StateStore::load_or_default(&directory)?;
            let threads = &recovered.project(1).ok_or("missing project")?.threads;
            let recovered_states: Vec<(u32, ThreadState)> = threads
                .iter()
                .map(|thread| (thread.id, thread.state))
                .collect();
            assert_eq!(recovered_states, expected);
            let active_thread = threads
                .iter()
                .find(|thread| thread.state == ThreadState::Working)
                .ok_or("missing active thread")?;
            assert_eq!(active_thread.transcript.len(), 1);

            let _ignored = fs::remove_dir_all(&directory);
        }
        Ok(())
    }
}
