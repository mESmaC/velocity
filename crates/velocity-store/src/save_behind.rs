//! Saving persisted state behind the store lock (#285, #320, #552).
//!
//! In deferred mode, mutations update memory in place (or swap an `Arc`) and bump the
//! revision; [`save_pending`] takes an O(1) `Arc::clone` of [`PersistedState`] under the
//! store lock, then diffs and writes only changed root/project/settled/transcript files
//! outside the lock.
//!
//! A store that never calls [`StateStore::defer_saves`] (tests, examples, stress scripts)
//! writes every change immediately before returning and rolls back in-memory edits on error.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use velocity_core::thread_view_model::SharedBlocks;

use crate::storage;
use crate::transcripts::{self, PersistedTranscript, is_settled_state};
use crate::{PersistedState, ProjectItem, StateStore, StoreError, StoreLock};

struct TranscriptDelta {
    pending: BTreeMap<(u32, u32), SharedBlocks>,
    deleted: BTreeSet<(u32, u32)>,
    reopened: BTreeMap<(u32, u32), SharedBlocks>,
    evict_settled: BTreeSet<(u32, u32)>,
}

impl TranscriptDelta {
    fn needs_state_adjustment(&self) -> bool {
        !self.evict_settled.is_empty() || !self.reopened.is_empty()
    }
}

impl StateStore {
    /// Stops writing on every change: [`save_pending`] writes incremental diffs instead.
    pub fn defer_saves(&mut self) {
        self.deferred_saves = true;
    }

    /// Whether a change is in memory but not yet persisted to disk.
    pub fn has_unsaved_changes(&self) -> bool {
        self.disk_revision != self.state.revision
            || !self.pending_transcripts.is_empty()
            || !self.deleted_transcripts.is_empty()
    }

    /// Replaces the whole state with `next` as the next revision. Without deferred saves,
    /// `next` is written first, and a failed write leaves the state as it was.
    pub(crate) fn store_snapshot(&mut self, mut next: PersistedState) -> Result<(), StoreError> {
        next.revision = self.state.revision.wrapping_add(1);
        let delta = self.prepare_transcript_delta(&mut next);
        if !self.deferred_saves {
            let written = self.write_delta_immediately(&next, &delta)?;
            self.state.replace(next);
            self.disk_state = self.state.arc();
            self.disk_revision = self.state.revision;
            self.apply_persisted_delta(delta, written);
            return Ok(());
        }
        self.apply_deferred_delta(delta);
        self.state.replace(next);
        Ok(())
    }

    /// Records a change already made to `self.state` in place as the next revision.
    /// Without deferred saves it is written first; on a failed write the revision goes
    /// back and the caller undoes its change.
    pub(crate) fn record_change(&mut self) -> Result<(), StoreError> {
        let previous_revision = self.state.revision;
        self.state.revision = previous_revision.wrapping_add(1);
        let delta = self.collect_transcript_delta(&self.state);
        if self.deferred_saves {
            Self::apply_state_transcript_adjustments(&mut self.state, &delta);
            self.apply_deferred_delta(delta);
            return Ok(());
        }
        let written = if delta.needs_state_adjustment() {
            let mut next = self.state.clone();
            Self::apply_state_transcript_adjustments(&mut next, &delta);
            match self.write_delta_immediately(&next, &delta) {
                Ok(written) => {
                    self.state.replace(next);
                    written
                }
                Err(error) => {
                    self.state.revision = previous_revision;
                    return Err(error);
                }
            }
        } else {
            let current = self.state.arc();
            match self.write_delta_immediately(&current, &delta) {
                Ok(written) => written,
                Err(error) => {
                    self.state.revision = previous_revision;
                    return Err(error);
                }
            }
        };
        self.disk_state = self.state.arc();
        self.disk_revision = self.state.revision;
        self.apply_persisted_delta(delta, written);
        Ok(())
    }

    /// Flushes the current in-memory state immediately (used by `save_if_changed`).
    pub(crate) fn flush_immediate(&mut self) -> Result<(), StoreError> {
        let delta = self.collect_transcript_delta(&self.state);
        let written = if delta.needs_state_adjustment() {
            let mut next = self.state.clone();
            Self::apply_state_transcript_adjustments(&mut next, &delta);
            let written = self.write_delta_immediately(&next, &delta)?;
            self.state.replace(next);
            written
        } else {
            let current = self.state.arc();
            self.write_delta_immediately(&current, &delta)?
        };
        self.disk_state = self.state.arc();
        self.apply_persisted_delta(delta, written);
        Ok(())
    }

    /// Mutates one project in place, avoiding cloning other projects. In deferred mode no
    /// rollback clone is taken; in immediate mode only that project is cloned for rollback.
    pub(crate) fn edit_project_in_place<Value>(
        &mut self,
        project_id: u32,
        edit: impl FnOnce(&mut ProjectItem) -> (bool, Value),
    ) -> Result<Option<Value>, StoreError> {
        let Some(index) = self
            .state
            .projects
            .iter()
            .position(|project| project.id == project_id)
        else {
            return Ok(None);
        };
        let rollback = (!self.deferred_saves).then(|| self.state.projects[index].clone());
        let (changed, value) = edit(&mut self.state.projects[index]);
        if !changed {
            return Ok(Some(value));
        }
        if let Err(error) = self.record_change() {
            if let Some(previous) = rollback {
                self.state.projects[index] = previous;
            }
            return Err(error);
        }
        Ok(Some(value))
    }

    /// Mutates one project in place with a fallible closure, rolling back only that project
    /// if the closure or immediate persistence fails (#552).
    pub(crate) fn try_edit_project<Value, Failure: From<StoreError>>(
        &mut self,
        project_id: u32,
        missing: impl FnOnce() -> Failure,
        edit: impl FnOnce(&mut ProjectItem) -> Result<(bool, Value), Failure>,
    ) -> Result<Value, Failure> {
        let index = self
            .state
            .projects
            .iter()
            .position(|project| project.id == project_id)
            .ok_or_else(missing)?;
        let rollback = self.state.projects[index].clone();
        let (changed, value) = match edit(&mut self.state.projects[index]) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.state.projects[index] = rollback;
                return Err(error);
            }
        };
        if !changed {
            return Ok(value);
        }
        if let Err(error) = self.record_change() {
            self.state.projects[index] = rollback;
            return Err(error.into());
        }
        Ok(value)
    }

    fn prepare_transcript_delta(&self, next: &mut PersistedState) -> TranscriptDelta {
        let delta = self.collect_transcript_delta(next);
        Self::apply_state_transcript_adjustments(next, &delta);
        delta
    }

    fn collect_transcript_delta(&self, next: &PersistedState) -> TranscriptDelta {
        let mut pending = BTreeMap::new();
        let mut reopened = BTreeMap::new();
        let mut evict_settled = BTreeSet::new();
        let mut live_keys = BTreeSet::new();
        for project in &next.projects {
            for thread in &project.threads {
                let key = (project.id, thread.id);
                live_keys.insert(key);
                if is_settled_state(thread.state) {
                    if !thread.transcript.is_empty() {
                        pending.insert(key, Arc::clone(&thread.transcript));
                        evict_settled.insert(key);
                    } else if !thread.queued_run_history.is_empty() {
                        evict_settled.insert(key);
                    }
                    continue;
                }
                if !thread.transcript.is_empty() {
                    let known = self.pending_transcripts.get(&key).or_else(|| {
                        self.persisted_transcripts
                            .get(&key)
                            .map(|entry| &entry.blocks)
                    });
                    let changed = known.is_none_or(|prev| {
                        !Arc::ptr_eq(prev, &thread.transcript)
                            && prev.as_slice() != thread.transcript.as_slice()
                    });
                    if changed {
                        pending.insert(key, Arc::clone(&thread.transcript));
                    }
                } else if let Some(restored) = self.reopened_transcript(key) {
                    reopened.insert(key, restored);
                } else if self.persisted_transcripts.contains_key(&key)
                    || self
                        .pending_transcripts
                        .get(&key)
                        .is_some_and(|blocks| !blocks.is_empty())
                {
                    pending.insert(key, SharedBlocks::default());
                }
            }
        }
        let deleted = self.collect_deleted_transcripts(&live_keys);
        TranscriptDelta {
            pending,
            deleted,
            reopened,
            evict_settled,
        }
    }

    fn collect_deleted_transcripts(
        &self,
        live_keys: &BTreeSet<(u32, u32)>,
    ) -> BTreeSet<(u32, u32)> {
        let mut deleted = BTreeSet::new();
        for project in &self.disk_state.projects {
            for thread in &project.threads {
                let key = (project.id, thread.id);
                if !live_keys.contains(&key) {
                    deleted.insert(key);
                }
            }
        }
        for &key in self
            .persisted_transcripts
            .keys()
            .chain(self.pending_transcripts.keys())
        {
            if !live_keys.contains(&key) {
                deleted.insert(key);
            }
        }
        deleted
    }

    fn apply_state_transcript_adjustments(state: &mut PersistedState, delta: &TranscriptDelta) {
        if !delta.needs_state_adjustment() {
            return;
        }
        for project in &mut state.projects {
            for thread in &mut project.threads {
                let key = (project.id, thread.id);
                if delta.evict_settled.contains(&key) {
                    thread.transcript = SharedBlocks::default();
                    thread.queued_run_history.clear();
                } else if let Some(blocks) = delta.reopened.get(&key) {
                    thread.transcript = Arc::clone(blocks);
                }
            }
        }
    }

    /// Reloads an archived transcript when a settled thread is reopened (e.g. Rosen revert).
    fn reopened_transcript(&self, key: (u32, u32)) -> Option<SharedBlocks> {
        let was_settled = self
            .state
            .projects
            .iter()
            .find(|project| project.id == key.0)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
            .is_some_and(|thread| is_settled_state(thread.state))
            || self
                .disk_state
                .projects
                .iter()
                .find(|project| project.id == key.0)
                .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
                .is_some_and(|thread| is_settled_state(thread.state));
        if !was_settled {
            return None;
        }
        if let Some(pending) = self.pending_transcripts.get(&key)
            && !pending.is_empty()
        {
            return Some(Arc::clone(pending));
        }
        if let Some(cached) = self.settled_transcripts.get(key)
            && !cached.is_empty()
        {
            return Some(cached);
        }
        transcripts::read_transcript(&self.directory, key.0, key.1)
            .ok()
            .filter(|blocks| !blocks.is_empty())
    }

    fn write_delta_immediately(
        &mut self,
        next: &PersistedState,
        delta: &TranscriptDelta,
    ) -> Result<BTreeMap<(u32, u32), Option<PersistedTranscript>>, StoreError> {
        let mut pending = self.pending_transcripts.clone();
        for (key, blocks) in &delta.pending {
            pending.insert(*key, Arc::clone(blocks));
        }
        let mut deleted = self.deleted_transcripts.clone();
        for key in &delta.deleted {
            pending.remove(key);
            deleted.insert(*key);
        }
        for key in delta.pending.keys() {
            deleted.remove(key);
        }
        match storage::write_incremental_snapshot(
            &self.directory,
            &self.disk_state,
            next,
            &pending,
            &deleted,
            &self.persisted_transcripts,
        ) {
            Ok(written) => Ok(written),
            Err(failure) => {
                self.apply_written_transcripts(failure.written_transcripts);
                if let Some(key) = failure.failed_transcript {
                    self.persisted_transcripts.remove(&key);
                }
                Err(failure.error)
            }
        }
    }

    fn apply_written_transcripts(
        &mut self,
        written: BTreeMap<(u32, u32), Option<PersistedTranscript>>,
    ) {
        for (key, persisted) in written {
            let same_as_queued = match (&persisted, self.pending_transcripts.get(&key)) {
                (Some(entry), Some(queued)) => {
                    Arc::ptr_eq(queued, &entry.blocks)
                        || queued.as_slice() == entry.blocks.as_slice()
                }
                (None, Some(queued)) => queued.is_empty(),
                _ => false,
            };
            if same_as_queued {
                self.pending_transcripts.remove(&key);
            }
            match persisted {
                Some(entry)
                    if !self.deleted_transcripts.contains(&key)
                        && !self.is_settled_in_state(key)
                        && !entry.blocks.is_empty() =>
                {
                    self.persisted_transcripts.insert(key, entry);
                }
                _ => {
                    self.persisted_transcripts.remove(&key);
                }
            }
        }
    }

    fn apply_deferred_delta(&mut self, delta: TranscriptDelta) {
        for (key, blocks) in delta.reopened {
            self.persisted_transcripts
                .insert(key, PersistedTranscript::unconfirmed(blocks));
            self.settled_transcripts.remove(key);
        }
        for (key, blocks) in delta.pending {
            self.deleted_transcripts.remove(&key);
            if self.is_settled_in_state(key) && !blocks.is_empty() {
                self.settled_transcripts.insert(key, Arc::clone(&blocks));
            }
            self.pending_transcripts.insert(key, blocks);
        }
        for key in delta.deleted {
            self.pending_transcripts.remove(&key);
            self.settled_transcripts.remove(key);
            self.deleted_transcripts.insert(key);
        }
    }

    fn apply_persisted_delta(
        &mut self,
        delta: TranscriptDelta,
        written: BTreeMap<(u32, u32), Option<PersistedTranscript>>,
    ) {
        let existing_pending = std::mem::take(&mut self.pending_transcripts);
        let existing_deleted = std::mem::take(&mut self.deleted_transcripts);
        for (key, blocks) in delta.reopened {
            self.persisted_transcripts
                .insert(key, PersistedTranscript::unconfirmed(blocks));
            self.settled_transcripts.remove(key);
        }
        for (key, blocks) in existing_pending.into_iter().chain(delta.pending) {
            if self.is_settled_in_state(key) && !blocks.is_empty() {
                self.settled_transcripts.insert(key, blocks);
            }
        }
        self.apply_written_transcripts(written);
        for key in existing_deleted.into_iter().chain(delta.deleted) {
            self.persisted_transcripts.remove(&key);
            self.settled_transcripts.remove(key);
        }
    }

    fn is_settled_in_state(&self, key: (u32, u32)) -> bool {
        self.project(key.0)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
            .is_some_and(|thread| is_settled_state(thread.state))
    }
}

struct SaveBatch {
    directory: std::path::PathBuf,
    previous: Arc<PersistedState>,
    current: Arc<PersistedState>,
    pending: BTreeMap<(u32, u32), SharedBlocks>,
    deleted: BTreeSet<(u32, u32)>,
    previous_transcripts: BTreeMap<(u32, u32), PersistedTranscript>,
}

fn take_save_batch(store: &StoreLock, lock_time: &mut Duration) -> Option<SaveBatch> {
    let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
    let batch = guard.has_unsaved_changes().then(|| {
        let pending = std::mem::take(&mut guard.pending_transcripts);
        let deleted = std::mem::take(&mut guard.deleted_transcripts);
        let previous_transcripts: BTreeMap<_, _> = pending
            .keys()
            .filter_map(|key| {
                guard
                    .persisted_transcripts
                    .get(key)
                    .map(|entry| (*key, entry.clone()))
            })
            .collect();
        SaveBatch {
            directory: guard.directory.clone(),
            previous: Arc::clone(&guard.disk_state),
            current: guard.state.arc(),
            pending,
            deleted,
            previous_transcripts,
        }
    });
    *lock_time += guard.finish();
    batch
}

fn restore_failed_batch(
    store: &StoreLock,
    mut pending: BTreeMap<(u32, u32), SharedBlocks>,
    deleted: BTreeSet<(u32, u32)>,
    written: BTreeMap<(u32, u32), Option<PersistedTranscript>>,
    failed_transcript: Option<(u32, u32)>,
    lock_time: &mut Duration,
) {
    let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
    for key in written.keys() {
        pending.remove(key);
    }
    guard.apply_written_transcripts(written);
    if let Some(key) = failed_transcript {
        guard.persisted_transcripts.remove(&key);
    }
    for (key, blocks) in pending {
        if !guard.deleted_transcripts.contains(&key) {
            guard.pending_transcripts.entry(key).or_insert(blocks);
        }
    }
    for key in deleted {
        if !guard.pending_transcripts.contains_key(&key) {
            guard.deleted_transcripts.insert(key);
        }
    }
    *lock_time += guard.finish();
}

fn finish_save_batch(
    store: &StoreLock,
    current: Arc<PersistedState>,
    written: BTreeMap<(u32, u32), Option<PersistedTranscript>>,
    deleted: BTreeSet<(u32, u32)>,
    lock_time: &mut Duration,
) {
    let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
    guard.disk_revision = current.revision;
    guard.disk_state = current;
    guard.apply_written_transcripts(written);
    for key in deleted {
        guard.persisted_transcripts.remove(&key);
    }
    *lock_time += guard.finish();
}

/// Writes the store's latest revision incrementally to disk if anything changed. The store
/// lock is held only for O(1) `Arc::clone` / map swaps and to record completion, never while
/// serializing or writing. Every store lock acquisition (including idle polls) records
/// per-caller wait and hold metrics after releasing the mutex, alongside the per-save
/// aggregate (#694). Safe to call from several threads: writes are serialized and each one
/// writes the newest state.
pub fn save_pending(store: &StoreLock) -> Result<bool, StoreError> {
    let save_started = Instant::now();
    let mut lock_time = Duration::ZERO;
    let write_lock = {
        let guard = store.lock().unwrap_or_else(PoisonError::into_inner);
        let result = guard
            .has_unsaved_changes()
            .then(|| Arc::clone(&guard.write_lock));
        lock_time += guard.finish();
        let Some(write_lock) = result else {
            return Ok(false);
        };
        write_lock
    };
    let _writing = write_lock.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(batch) = take_save_batch(store, &mut lock_time) else {
        return Ok(false);
    };
    let written = match storage::write_incremental_snapshot(
        &batch.directory,
        &batch.previous,
        &batch.current,
        &batch.pending,
        &batch.deleted,
        &batch.previous_transcripts,
    ) {
        Ok(written) => written,
        Err(failure) => {
            restore_failed_batch(
                store,
                batch.pending,
                batch.deleted,
                failure.written_transcripts,
                failure.failed_transcript,
                &mut lock_time,
            );
            velocity_core::performance::record_save(save_started.elapsed(), lock_time);
            return Err(failure.error);
        }
    };
    finish_save_batch(store, batch.current, written, batch.deleted, &mut lock_time);
    velocity_core::performance::record_save(save_started.elapsed(), lock_time);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProjectItem, ThreadItem, ThreadState};
    use velocity_core::thread_view_model::TranscriptBlock;

    // A deferred change that never reaches disk is lost on the next start (#320).
    #[test]
    fn deferred_changes_reach_disk_on_save_pending() -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("velocity-save-behind-{}", std::process::id()));
        let _ignored = std::fs::remove_dir_all(&directory);
        let mut store = StateStore::load_or_default(&directory)?;
        store.defer_saves();
        let mut next = store.state().clone();
        next.projects
            .push(ProjectItem::new(1, "One".into(), "C:\\one".into()));
        store.store_snapshot(next)?;
        assert!(store.has_unsaved_changes());
        assert!(
            StateStore::load_or_default(&directory)?
                .state()
                .projects
                .is_empty()
        );

        let store = StoreLock::new(store);
        assert!(save_pending(&store)?);
        assert!(!save_pending(&store)?);
        let loaded = StateStore::load_or_default(&directory)?;
        assert_eq!(loaded.state().projects.len(), 1);
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }

    // Risk (#552): hundreds of settled threads with large transcripts must keep state.json
    // well under 1 MB, archive settled transcripts out of live memory and load them on
    // demand, and keep save_pending store-lock hold time under 10 ms.
    #[test]
    fn settled_transcripts_stay_out_of_state_json_and_save_fast()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-552-{unique}"));
        let _ignored = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;

        // Seed a legacy monolithic state.json with 200 settled threads and 2 MB+ of transcripts.
        let mut legacy = PersistedState::default();
        let mut project = ProjectItem::new(1, "Large".into(), r"C:\large".into());
        let payload = "x".repeat(4_096);
        for id in 1..=200u32 {
            let mut thread = ThreadItem::new(id, format!("Settled thread {id}"));
            thread.state = ThreadState::Merged;
            thread.activity = crate::AgentActivity::Idle;
            thread.transcript = Arc::new(vec![
                TranscriptBlock::UserMessage {
                    text: format!("Prompt {id}"),
                    queued: false,
                },
                TranscriptBlock::AgentMessage {
                    paragraphs: vec![format!("Answer {id}: {payload}")],
                },
            ]);
            project.threads.push(thread);
        }
        let mut active = ThreadItem::new(201, "Active thread".into());
        active.state = ThreadState::Working;
        active.transcript = Arc::new(vec![TranscriptBlock::UserMessage {
            text: "Working prompt".into(),
            queued: false,
        }]);
        project.threads.push(active);
        legacy.projects.push(project);
        let legacy_bytes = serde_json::to_vec(&legacy)?;
        assert!(legacy_bytes.len() > 800_000);
        std::fs::write(directory.join(crate::STATE_FILE_NAME), &legacy_bytes)?;

        // Loading migrates transcripts to transcripts/1/<id>.jsonl and splits projects.
        let mut store = StateStore::load_or_default(&directory)?;
        let compact_size = std::fs::metadata(directory.join(crate::STATE_FILE_NAME))?.len();
        assert!(
            compact_size < 100_000,
            "state.json should be compact after migration, got {compact_size} bytes"
        );
        let loaded_project = store.project(1).ok_or("missing migrated project")?;
        // Settled threads keep empty in-memory transcripts and hydrate on demand.
        assert!(loaded_project.threads[0].transcript.is_empty());
        assert_eq!(loaded_project.threads[200].transcript.len(), 1);
        let on_demand = store.thread_transcript(1, 1);
        assert_eq!(on_demand.len(), 2);

        // Incremental deferred save appends new blocks and stays well under 10 ms lock time.
        store.defer_saves();
        store.update_thread(1, 201, |thread| {
            Arc::make_mut(&mut thread.transcript).push(TranscriptBlock::AgentMessage {
                paragraphs: vec!["Appended block".into()],
            });
        })?;
        let state_mtime_before =
            std::fs::metadata(directory.join(crate::STATE_FILE_NAME))?.modified()?;
        let store = StoreLock::new(store);
        let start = Instant::now();
        assert!(save_pending(&store)?);
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "save_pending took unexpectedly long: {elapsed:?}"
        );
        let save_lock_ms = velocity_core::performance::snapshot().saves.last_lock_ms;
        assert!(
            save_lock_ms <= velocity_core::performance::SAVE_LOCK_BUDGET_MS,
            "save_pending held store lock for {save_lock_ms} ms"
        );
        // Transcript-only update must not rewrite state.json.
        assert_eq!(
            std::fs::metadata(directory.join(crate::STATE_FILE_NAME))?.modified()?,
            state_mtime_before
        );

        let reloaded = StateStore::load_or_default(&directory)?;
        let active_reloaded = reloaded
            .project(1)
            .and_then(|item| item.threads.iter().find(|thread| thread.id == 201))
            .ok_or("missing active thread")?;
        assert_eq!(active_reloaded.transcript.len(), 2);
        assert_eq!(reloaded.thread_transcript(1, 42).len(), 2);

        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }

    fn save_behind_acquisitions(snap: &velocity_core::performance::PerformanceDiagnostics) -> u32 {
        snap.store_locks
            .iter()
            .filter(|row| row.caller.starts_with("src/save_behind.rs:"))
            .map(|row| row.acquisitions)
            .sum()
    }

    fn save_behind_caller_count(
        snap: &velocity_core::performance::PerformanceDiagnostics,
    ) -> usize {
        snap.store_locks
            .iter()
            .filter(|row| row.caller.starts_with("src/save_behind.rs:"))
            .count()
    }

    // Risk (#694, #557): background saver lock acquisitions (including contended idle polls)
    // must record per-caller wait and hold telemetry while preserving the per-save aggregate.
    #[test]
    fn save_pending_records_store_lock_metrics_for_idle_polls_and_saves()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-694-{unique}"));
        let _ignored = std::fs::remove_dir_all(&directory);
        let mut raw_store = StateStore::load_or_default(&directory)?;
        raw_store.defer_saves();
        let store = Arc::new(StoreLock::new(raw_store));

        let before_idle = velocity_core::performance::snapshot();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let holder_store = Arc::clone(&store);
        let holder = std::thread::spawn(move || -> Result<(), String> {
            let _guard = holder_store
                .as_mutex()
                .lock()
                .map_err(|_| String::from("store mutex poisoned"))?;
            let _ = locked_tx.send(());
            std::thread::sleep(Duration::from_millis(35));
            Ok(())
        });
        locked_rx.recv()?;
        let idle_saved = save_pending(&store)?;
        holder.join().map_err(|_| "lock holder panicked")??;
        assert!(!idle_saved);

        let after_idle = velocity_core::performance::snapshot();
        assert!(save_behind_acquisitions(&after_idle) > save_behind_acquisitions(&before_idle));
        assert!(
            after_idle
                .store_locks
                .iter()
                .any(|row| row.caller.starts_with("src/save_behind.rs:") && row.max_wait_ms >= 10)
        );

        {
            let mut guard = store
                .as_mutex()
                .lock()
                .map_err(|_| "store mutex poisoned")?;
            let mut next = guard.state().clone();
            next.projects
                .push(ProjectItem::new(1, "Metrics".into(), "C:\\metrics".into()));
            guard.store_snapshot(next)?;
        }
        assert!(save_pending(&store)?);
        let after_save = velocity_core::performance::snapshot();
        assert!(after_save.saves.count > after_idle.saves.count);
        assert!(save_behind_caller_count(&after_save) >= 3);

        let mut restore_lock_time = Duration::ZERO;
        restore_failed_batch(
            &store,
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeMap::new(),
            None,
            &mut restore_lock_time,
        );
        assert!(save_behind_caller_count(&velocity_core::performance::snapshot()) >= 4);
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }

    fn working_thread(id: u32, prompt: &str) -> ThreadItem {
        let mut thread = ThreadItem::new(id, format!("Thread {id}"));
        thread.state = ThreadState::Working;
        thread.transcript = Arc::new(vec![TranscriptBlock::UserMessage {
            text: prompt.into(),
            queued: false,
        }]);
        thread
    }

    fn append_agent_reply(
        store: &StoreLock,
        thread_id: u32,
        text: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut guard = store
            .as_mutex()
            .lock()
            .map_err(|_| "store mutex poisoned")?;
        guard.update_thread(1, thread_id, |thread| {
            Arc::make_mut(&mut thread.transcript).push(TranscriptBlock::AgentMessage {
                paragraphs: vec![text.into()],
            });
        })?;
        Ok(())
    }

    // Risk (#700, #552, SPEC §2.2): if an earlier thread's transcript appends cleanly in
    // a save batch and a later thread's transcript fails, retrying the batch must not
    // append the first thread's new block a second time, and subsequent blocks on the
    // first thread must still append cleanly.
    #[test]
    fn retrying_partial_save_batch_does_not_duplicate_transcript_blocks()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-700-{unique}"));
        let _ignored = std::fs::remove_dir_all(&directory);
        let mut raw_store = StateStore::load_or_default(&directory)?;
        raw_store.defer_saves();
        let mut next = raw_store.state().clone();
        let mut project = ProjectItem::new(1, "Retry".into(), r"C:\retry".into());
        project.threads = vec![working_thread(1, "prior 1"), working_thread(2, "prior 2")];
        next.projects.push(project);
        raw_store.store_snapshot(next)?;

        let store = StoreLock::new(raw_store);
        assert!(save_pending(&store)?);
        append_agent_reply(&store, 1, "new 1")?;
        append_agent_reply(&store, 2, "new 2")?;

        // Force thread 2's transcript write to fail after thread 1's append succeeds.
        let path_two = transcripts::transcript_path(&directory, 1, 2);
        let original_two = std::fs::read(&path_two)?;
        std::fs::remove_file(&path_two)?;
        std::fs::create_dir(&path_two)?;
        assert!(save_pending(&store).is_err());

        // Restore thread 2's prior file and retry the failed batch.
        std::fs::remove_dir(&path_two)?;
        std::fs::write(&path_two, &original_two)?;
        assert_eq!(transcripts::read_transcript(&directory, 1, 1)?.len(), 2);
        assert!(save_pending(&store)?);

        let reloaded = StateStore::load_or_default(&directory)?;
        let expected_one = vec![
            TranscriptBlock::UserMessage {
                text: "prior 1".into(),
                queued: false,
            },
            TranscriptBlock::AgentMessage {
                paragraphs: vec!["new 1".into()],
            },
        ];
        let expected_two = vec![
            TranscriptBlock::UserMessage {
                text: "prior 2".into(),
                queued: false,
            },
            TranscriptBlock::AgentMessage {
                paragraphs: vec!["new 2".into()],
            },
        ];
        assert_eq!(reloaded.thread_transcript(1, 1).as_slice(), expected_one);
        assert_eq!(reloaded.thread_transcript(1, 2).as_slice(), expected_two);

        // A further block on thread 1 after recovery still appends cleanly once.
        append_agent_reply(&store, 1, "follow-up 1")?;
        assert!(save_pending(&store)?);
        assert_eq!(transcripts::read_transcript(&directory, 1, 1)?.len(), 3);
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }
}
