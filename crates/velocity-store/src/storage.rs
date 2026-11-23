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
    let settled_threads = match fs::read(&settled_path) {
        Ok(bytes) => {
            let settled_file: ProjectSettledFile = serde_json::from_slice(&bytes)
                .map_err(|source| StoreError::SerializeSnapshot { source })?;
            settled_file.threads
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => {
            return Err(StoreError::ReadSnapshot {
                path: settled_path.display().to_string(),
                source,
            });
        }
    };
    project.threads =
        reassemble_threads(project.threads, settled_threads, &active_file.thread_order);
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

fn settled_file_from_project(project: &ProjectItem) -> ProjectSettledFile {
    let threads = project
        .threads
        .iter()
        .filter(|thread| is_settled_state(thread.state))
        .map(archive_settled_thread)
        .collect();
    ProjectSettledFile { threads }
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
    let settled = settled_file_from_project(project);
    let settled_path = project_settled_path(directory, project.id);
    if settled.threads.is_empty() {
        let _ignored = fs::remove_file(&settled_path);
        let _ignored = fs::remove_file(project_settled_partial_path(directory, project.id));
    } else {
        let settled_bytes = serde_json::to_vec(&settled)
            .map_err(|source| StoreError::SerializeSnapshot { source })?;
        write_atomic_bytes(
            &project_settled_partial_path(directory, project.id),
            &settled_path,
            &settled_bytes,
        )?;
    }
    Ok(())
}

fn remove_project_files(directory: &Path, project_id: u32) {
    let _ignored = fs::remove_file(project_active_path(directory, project_id));
    let _ignored = fs::remove_file(project_active_partial_path(directory, project_id));
    let _ignored = fs::remove_file(project_settled_path(directory, project_id));
    let _ignored = fs::remove_file(project_settled_partial_path(directory, project_id));
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
    let settled_path = project_settled_path(directory, project_id);
    recover_partial_json::<ProjectSettledFile>(
        &project_settled_partial_path(directory, project_id),
        &settled_path,
    )?;
    let settled_threads = if settled_path.exists() {
        let settled_bytes = fs::read(&settled_path).map_err(|source| StoreError::ReadSnapshot {
            path: settled_path.display().to_string(),
            source,
        })?;
        let settled_file: ProjectSettledFile = serde_json::from_slice(&settled_bytes)
            .map_err(|source| StoreError::SerializeSnapshot { source })?;
        settled_file.threads
    } else {
        Vec::new()
    };
    let mut project = active_file.project;
    let mut active_threads = project.threads;
    for thread in &mut active_threads {
        if thread.transcript.is_empty()
            && !is_settled_state(thread.state)
            && let Some(loaded) =
                transcripts::read_transcript_persisted(directory, project_id, thread.id)?
        {
            thread.transcript = Arc::clone(&loaded.blocks);
            persisted_transcripts.insert((project_id, thread.id), loaded);
        }
    }
    project.threads =
        reassemble_threads(active_threads, settled_threads, &active_file.thread_order);
    Ok((project, active_file.revision))
}

fn reassemble_threads(
    active: Vec<ThreadItem>,
    settled: Vec<ThreadItem>,
    order: &[u32],
) -> Vec<ThreadItem> {
    let mut by_id: BTreeMap<u32, ThreadItem> = BTreeMap::new();
    for thread in active.into_iter().chain(settled) {
        by_id.insert(thread.id, thread);
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
    let settled_changed = previous.is_none_or(|prev| !settled_threads_equal(prev, current));
    if settled_changed {
        let settled = settled_file_from_project(current);
        let settled_path = project_settled_path(directory, current.id);
        if settled.threads.is_empty() {
            let _ignored = fs::remove_file(&settled_path);
            let _ignored = fs::remove_file(project_settled_partial_path(directory, current.id));
        } else {
            let settled_bytes = serde_json::to_vec(&settled)
                .map_err(|source| StoreError::SerializeSnapshot { source })?;
            write_atomic_bytes(
                &project_settled_partial_path(directory, current.id),
                &settled_path,
                &settled_bytes,
            )?;
        }
    }
    let active_file = active_file_from_project(current, revision);
    let active_bytes = serde_json::to_vec(&active_file)
        .map_err(|source| StoreError::SerializeSnapshot { source })?;
    let active_changed = match previous {
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
    if active_changed {
        write_atomic_bytes(
            &project_active_partial_path(directory, current.id),
            &project_active_path(directory, current.id),
            &active_bytes,
        )?;
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
}
