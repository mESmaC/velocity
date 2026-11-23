//! Per-thread JSONL transcript storage and on-demand settled-thread cache (#552).
//!
//! Each thread's conversation blocks are stored in `transcripts/<project>/<thread>.jsonl`,
//! one compact JSON `TranscriptBlock` per line, instead of inline in `state.json`. Active
//! threads keep their transcript in memory and append newly arrived blocks to disk; settled
//! (`merged` / `done`) threads evict their in-memory transcript once persisted and load it
//! on demand through a small LRU cache when opened in the UI.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use velocity_core::thread_view_model::{SharedBlocks, TranscriptBlock};

use crate::{ProjectItem, StoreError, ThreadState, extended_windows_path, replace_with_retry};

pub const TRANSCRIPTS_DIR_NAME: &str = "transcripts";

/// Maximum number of settled-thread transcripts retained in memory at once.
const SETTLED_CACHE_CAPACITY: usize = 4;

/// Confirmed on-disk transcript state used to decide whether appending a new
/// suffix is safe without duplicating or corrupting existing JSONL lines (#700).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PersistedTranscript {
    pub blocks: SharedBlocks,
    /// Exact byte length of `transcripts/<project>/<thread>.jsonl` when every
    /// line on disk cleanly matches `blocks` and ends with `\n`. `None` forces
    /// an atomic rewrite on the next write.
    pub confirmed_bytes: Option<u64>,
}

impl PersistedTranscript {
    pub(crate) fn unconfirmed(blocks: SharedBlocks) -> Self {
        Self {
            blocks,
            confirmed_bytes: None,
        }
    }
}

/// Whether `state` is a settled terminal state whose transcript is archived out of live state.
pub fn is_settled_state(state: ThreadState) -> bool {
    matches!(state, ThreadState::Merged | ThreadState::Done)
}

/// Canonical path for a thread's JSONL transcript file:
/// `<store_dir>/transcripts/<project>/<thread>.jsonl`.
pub fn transcript_path(directory: &Path, project: u32, thread: u32) -> PathBuf {
    extended_windows_path(
        &directory
            .join(TRANSCRIPTS_DIR_NAME)
            .join(project.to_string())
            .join(format!("{thread}.jsonl")),
    )
}

fn partial_transcript_path(directory: &Path, project: u32, thread: u32) -> PathBuf {
    extended_windows_path(
        &directory
            .join(TRANSCRIPTS_DIR_NAME)
            .join(project.to_string())
            .join(format!("{thread}.jsonl.partial")),
    )
}

/// Reads a thread's transcript from `transcripts/<project>/<thread>.jsonl`.
/// Returns an empty `SharedBlocks` when no file exists. Tolerates a truncated final line
/// after an abrupt crash so earlier blocks are preserved.
pub fn read_transcript(
    directory: &Path,
    project: u32,
    thread: u32,
) -> Result<SharedBlocks, StoreError> {
    Ok(read_transcript_persisted(directory, project, thread)?
        .map_or_else(SharedBlocks::default, |loaded| loaded.blocks))
}

pub(crate) fn read_transcript_persisted(
    directory: &Path,
    project: u32,
    thread: u32,
) -> Result<Option<PersistedTranscript>, StoreError> {
    let path = transcript_path(directory, project, thread);
    let partial = partial_transcript_path(directory, project, thread);
    recover_partial_transcript(&partial, &path)?;
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(source) => {
            return Err(StoreError::ReadSnapshot {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let (blocks, clean) = parse_jsonl_blocks(&path, &contents);
    if blocks.is_empty() {
        return Ok(None);
    }
    Ok(Some(PersistedTranscript {
        blocks: Arc::new(blocks),
        confirmed_bytes: clean.then_some(contents.len() as u64),
    }))
}

fn recover_partial_transcript(partial: &Path, destination: &Path) -> Result<(), StoreError> {
    if !partial.exists() {
        return Ok(());
    }
    if destination.exists() {
        let _ignored = fs::remove_file(partial);
        return Ok(());
    }
    if let Ok(contents) = fs::read_to_string(partial)
        && all_lines_valid_blocks(&contents)
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

fn all_lines_valid_blocks(contents: &str) -> bool {
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .all(|line| serde_json::from_str::<TranscriptBlock>(line).is_ok())
}

fn parse_jsonl_blocks(path: &Path, contents: &str) -> (Vec<TranscriptBlock>, bool) {
    let mut blocks = Vec::new();
    let mut all_valid = true;
    let lines: Vec<&str> = contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    for (index, line) in lines.iter().enumerate() {
        match serde_json::from_str::<TranscriptBlock>(line) {
            Ok(block) => blocks.push(block),
            Err(error) => {
                all_valid = false;
                let is_last = index + 1 == lines.len();
                eprintln!(
                    "Ignoring unreadable transcript line {} in '{}' ({error}){}",
                    index + 1,
                    path.display(),
                    if is_last { " (truncated tail)" } else { "" }
                );
                if is_last {
                    break;
                }
            }
        }
    }
    let clean = all_valid && (contents.is_empty() || contents.ends_with('\n'));
    (blocks, clean)
}

/// Persists `current` to `transcripts/<project>/<thread>.jsonl`.
/// Rewrites atomically via a `.partial` file when no confirmed byte length is known,
/// so direct callers remain retry-safe even with a stale prefix (#700). Never creates
/// directories for empty transcripts.
pub fn write_transcript(
    directory: &Path,
    project: u32,
    thread: u32,
    previous: Option<&SharedBlocks>,
    current: &SharedBlocks,
) -> Result<(), StoreError> {
    let tracked = previous.map(|blocks| PersistedTranscript::unconfirmed(Arc::clone(blocks)));
    write_transcript_persisted(directory, project, thread, tracked.as_ref(), current)?;
    Ok(())
}

/// Persists `current` to `transcripts/<project>/<thread>.jsonl`, returning the confirmed
/// on-disk transcript state. Appends only newly arrived blocks when `previous` is an exact
/// prefix and the file on disk still matches `previous.confirmed_bytes`; otherwise writes
/// atomically via a `.partial` file (#700).
pub(crate) fn write_transcript_persisted(
    directory: &Path,
    project: u32,
    thread: u32,
    previous: Option<&PersistedTranscript>,
    current: &SharedBlocks,
) -> Result<Option<PersistedTranscript>, StoreError> {
    if let Some(prev) = previous
        && (Arc::ptr_eq(&prev.blocks, current) || prev.blocks.as_slice() == current.as_slice())
    {
        return Ok((!current.is_empty()).then(|| prev.clone()));
    }
    if current.is_empty() {
        remove_transcript(directory, project, thread);
        return Ok(None);
    }
    let path = transcript_path(directory, project, thread);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| StoreError::CreateDirectory {
            path: parent.display().to_string(),
            source,
        })?;
    }
    if let Some(prev) = previous
        && let Some(expected_bytes) = prev.confirmed_bytes
        && !prev.blocks.is_empty()
        && current.len() > prev.blocks.len()
        && current[..prev.blocks.len()] == prev.blocks[..]
        && fs::metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() == expected_bytes)
    {
        let appended_bytes = append_blocks(&path, &current[prev.blocks.len()..])?;
        return Ok(Some(PersistedTranscript {
            blocks: Arc::clone(current),
            confirmed_bytes: expected_bytes.checked_add(appended_bytes),
        }));
    }
    let written_bytes = rewrite_transcript_atomic(directory, project, thread, &path, current)?;
    Ok(Some(PersistedTranscript {
        blocks: Arc::clone(current),
        confirmed_bytes: Some(written_bytes),
    }))
}

fn serialize_jsonl(blocks: &[TranscriptBlock]) -> Result<Vec<u8>, StoreError> {
    let mut buffer = Vec::new();
    for block in blocks {
        serde_json::to_writer(&mut buffer, block)
            .map_err(|source| StoreError::SerializeSnapshot { source })?;
        buffer.push(b'\n');
    }
    Ok(buffer)
}

fn append_blocks(path: &Path, added: &[TranscriptBlock]) -> Result<u64, StoreError> {
    let bytes = serialize_jsonl(added)?;
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|source| StoreError::WritePartial {
            path: path.display().to_string(),
            source,
        })?;
    file.write_all(&bytes)
        .map_err(|source| StoreError::WritePartial {
            path: path.display().to_string(),
            source,
        })?;
    file.flush().map_err(|source| StoreError::SyncPartial {
        path: path.display().to_string(),
        source,
    })?;
    Ok(bytes.len() as u64)
}

fn rewrite_transcript_atomic(
    directory: &Path,
    project: u32,
    thread: u32,
    destination: &Path,
    blocks: &[TranscriptBlock],
) -> Result<u64, StoreError> {
    let bytes = serialize_jsonl(blocks)?;
    let partial = partial_transcript_path(directory, project, thread);
    let mut file = File::create(&partial).map_err(|source| StoreError::WritePartial {
        path: partial.display().to_string(),
        source,
    })?;
    file.write_all(&bytes)
        .map_err(|source| StoreError::WritePartial {
            path: partial.display().to_string(),
            source,
        })?;
    file.sync_all().map_err(|source| StoreError::SyncPartial {
        path: partial.display().to_string(),
        source,
    })?;
    drop(file);
    replace_with_retry(&partial, destination).map_err(|source| StoreError::ReplaceSnapshot {
        partial: partial.display().to_string(),
        destination: destination.display().to_string(),
        source,
    })?;
    Ok(bytes.len() as u64)
}

/// Removes a deleted thread's transcript file and prunes empty transcript parent folders.
pub fn remove_transcript(directory: &Path, project: u32, thread: u32) {
    let path = transcript_path(directory, project, thread);
    let partial = partial_transcript_path(directory, project, thread);
    let _ignored = fs::remove_file(&path);
    let _ignored = fs::remove_file(&partial);
    if let Some(project_dir) = path.parent() {
        let _ignored = fs::remove_dir(project_dir);
        if let Some(root_dir) = project_dir.parent() {
            let _ignored = fs::remove_dir(root_dir);
        }
    }
}

type SettledCacheEntry = ((u32, u32), SharedBlocks);

/// Bounded LRU cache for settled threads loaded on demand when opened in the UI.
#[derive(Clone, Default)]
pub(crate) struct SettledTranscriptCache {
    entries: Arc<Mutex<Vec<SettledCacheEntry>>>,
}

impl SettledTranscriptCache {
    pub(crate) fn get(&self, key: (u32, u32)) -> Option<SharedBlocks> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let position = entries.iter().position(|(cached, _)| *cached == key)?;
        let entry = entries.remove(position);
        let blocks = Arc::clone(&entry.1);
        entries.push(entry);
        Some(blocks)
    }

    pub(crate) fn insert(&self, key: (u32, u32), blocks: SharedBlocks) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(position) = entries.iter().position(|(cached, _)| *cached == key) {
            entries.remove(position);
        } else if entries.len() >= SETTLED_CACHE_CAPACITY {
            entries.remove(0);
        }
        entries.push((key, blocks));
    }

    pub(crate) fn insert_if_absent(&self, key: (u32, u32), blocks: SharedBlocks) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.iter().any(|(cached, _)| *cached == key) {
            return;
        }
        if entries.len() >= SETTLED_CACHE_CAPACITY {
            entries.remove(0);
        }
        entries.push((key, blocks));
    }

    pub(crate) fn remove(&self, key: (u32, u32)) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|(cached, _)| *cached != key);
    }
}

/// Settled thread transcripts that must be read from disk outside `ManagedStateStore` and
/// `MainLayout` locks before hydrating the layout snapshot (#669).
#[derive(Clone)]
pub struct PendingTranscriptHydration {
    directory: PathBuf,
    project: u32,
    thread_ids: Vec<u32>,
    cache: SettledTranscriptCache,
}

/// Settled transcripts loaded from disk outside the store and layout locks (#669).
pub struct LoadedTranscripts {
    project: u32,
    entries: Vec<(u32, SharedBlocks)>,
}

impl PendingTranscriptHydration {
    pub(crate) fn new(
        directory: PathBuf,
        project: u32,
        thread_ids: Vec<u32>,
        cache: SettledTranscriptCache,
    ) -> Self {
        Self {
            directory,
            project,
            thread_ids,
            cache,
        }
    }

    pub fn project(&self) -> u32 {
        self.project
    }

    pub fn thread_ids(&self) -> &[u32] {
        &self.thread_ids
    }

    /// Reads and deserializes pending settled transcript files from disk and caches them
    /// without holding `ManagedStateStore` or `MainLayout` (#669).
    pub fn load_from_disk(self) -> LoadedTranscripts {
        let mut entries = Vec::with_capacity(self.thread_ids.len());
        for thread_id in self.thread_ids {
            match read_transcript(&self.directory, self.project, thread_id) {
                Ok(blocks) if !blocks.is_empty() => {
                    self.cache
                        .insert_if_absent((self.project, thread_id), Arc::clone(&blocks));
                    entries.push((thread_id, blocks));
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!(
                        "Could not read transcript for project {} thread {thread_id}: {error}",
                        self.project
                    );
                }
            }
        }
        LoadedTranscripts {
            project: self.project,
            entries,
        }
    }

    /// Loads pending transcripts from disk and attaches them onto a detached `LayoutData`
    /// before acquiring `MainLayout` (#669).
    pub fn hydrate_data(self, data: &mut velocity_core::layout_view::LayoutData) {
        if data.project_identifier != Some(self.project) {
            return;
        }
        let loaded = self.load_from_disk();
        for (thread_id, blocks) in loaded.entries {
            if let Some(detail) = data
                .threads
                .iter_mut()
                .find(|entry| entry.policy.identifier == thread_id as usize)
                .and_then(|entry| entry.detail.as_mut())
            {
                detail.blocks = blocks;
            }
        }
    }
}

impl LoadedTranscripts {
    /// Applies loaded settled transcripts to `layout` if its active project still matches (#669).
    pub fn apply(self, layout: &mut velocity_core::layout_state::LayoutState) {
        for (thread_id, blocks) in self.entries {
            layout.hydrate_project_thread_blocks(self.project, thread_id as usize, blocks);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Thread IDs in `project` whose settled transcripts should be hydrated for displaying
/// `selected` (including its `previous_work_thread` chain).
pub(crate) fn hydrated_thread_ids(project: &ProjectItem, selected: Option<usize>) -> BTreeSet<u32> {
    let mut ids = BTreeSet::new();
    let Some(mut current_id) = selected.and_then(|id| u32::try_from(id).ok()) else {
        return ids;
    };
    while ids.insert(current_id) {
        let Some(previous) = project
            .threads
            .iter()
            .find(|thread| thread.id == current_id)
            .and_then(|thread| thread.previous_work_thread)
        else {
            break;
        };
        if previous >= current_id {
            break;
        }
        current_id = previous;
    }
    ids
}
