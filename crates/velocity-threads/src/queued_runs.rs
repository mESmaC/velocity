//! Durable persistence for thread `queue_run` and `run_cancel` acknowledgements (#664).

use std::sync::Arc;

use velocity_acp::client::{QueuedRunPersistence, QueuedRunStore};
use velocity_core::queued_runs::{MAX_RUN_HISTORY, QueuedRunRecord};
use velocity_store::{StoreLock, ThreadItem};

use crate::ThreadKey;

/// Records a newly accepted queued run on `thread` without downgrading an already-saved run
/// or resurrecting one that has already finished or been cancelled (#664).
pub fn record_added(thread: &mut ThreadItem, record: &QueuedRunRecord) {
    if thread
        .queued_runs
        .iter()
        .any(|existing| existing.id == record.id)
        || thread
            .queued_run_history
            .iter()
            .any(|existing| existing.id == record.id)
    {
        return;
    }
    thread.queued_runs.push(record.clone());
}

/// Moves a terminal queued run out of `thread.queued_runs` and into `thread.queued_run_history`,
/// trimming history to [`MAX_RUN_HISTORY`].
pub fn record_finished(thread: &mut ThreadItem, record: &QueuedRunRecord) {
    thread
        .queued_runs
        .retain(|existing| existing.id != record.id);
    thread
        .queued_run_history
        .retain(|existing| existing.id != record.id);
    thread.queued_run_history.push(record.clone());
    let excess = thread
        .queued_run_history
        .len()
        .saturating_sub(MAX_RUN_HISTORY);
    if excess > 0 {
        thread.queued_run_history.drain(..excess);
    }
}

/// Persists an accepted queued run to `store` and flushes deferred writes to disk before
/// returning so an immediate restart can recover the acknowledged `run_id` (#664).
pub fn persist_added(
    store: &StoreLock,
    key: ThreadKey,
    record: &QueuedRunRecord,
) -> Result<(), String> {
    persist_thread_change(store, key, |thread| record_added(thread, record))
}

/// Persists a cancelled queued run to `store` and flushes deferred writes to disk before
/// returning so an immediate restart does not replay the cancelled command (#664).
pub fn persist_cancelled(
    store: &StoreLock,
    key: ThreadKey,
    record: &QueuedRunRecord,
) -> Result<(), String> {
    persist_thread_change(store, key, |thread| record_finished(thread, record))
}

/// Persists a terminal queued run record to `store` and flushes deferred writes to disk (#664).
pub fn persist_finished(
    store: &StoreLock,
    key: ThreadKey,
    record: &QueuedRunRecord,
) -> Result<(), String> {
    persist_thread_change(store, key, |thread| record_finished(thread, record))
}

fn persist_thread_change(
    store: &StoreLock,
    key: ThreadKey,
    change: impl FnOnce(&mut ThreadItem),
) -> Result<(), String> {
    {
        let mut guard = store
            .lock()
            .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
        guard
            .update_thread(key.0, key.1, change)
            .map_err(|error| error.to_string())?;
    }
    velocity_store::save_pending(store)
        .map_err(|error| format!("Could not flush persisted state: {error}"))?;
    Ok(())
}

/// Store-backed [`QueuedRunPersistence`] for a single persisted thread.
pub struct ThreadStorePersistence {
    store: Arc<StoreLock>,
    key: ThreadKey,
}

impl ThreadStorePersistence {
    pub fn new(store: Arc<StoreLock>, key: ThreadKey) -> Self {
        Self { store, key }
    }
}

impl QueuedRunPersistence for ThreadStorePersistence {
    fn persist_added(&self, record: &QueuedRunRecord) -> Result<(), String> {
        persist_added(&self.store, self.key, record)
    }

    fn persist_cancelled(&self, record: &QueuedRunRecord) -> Result<(), String> {
        persist_cancelled(&self.store, self.key, record)
    }

    fn persist_finished(&self, record: &QueuedRunRecord) -> Result<(), String> {
        persist_finished(&self.store, self.key, record)
    }
}

/// Builds a [`QueuedRunStore`] backed by `store` for `key`.
pub fn store_for_thread(store: Arc<StoreLock>, key: ThreadKey) -> QueuedRunStore {
    QueuedRunStore::new(Arc::new(ThreadStorePersistence::new(store, key)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use velocity_core::queued_runs::{QueuedRunClass, QueuedRunPhase};
    use velocity_store::{AgentActivity, PersistedState, ProjectItem, StateStore};

    // Risk (#664): positive `queue_run` and `run_cancel` replies must reflect a state transition
    // already flushed to disk (even when `StateStore::defer_saves` is active), so crashing
    // immediately after either acknowledgement recovers the queued run or keeps it cancelled.
    #[test]
    fn queued_run_add_and_cancel_flush_immediately_and_survive_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "velocity-664-durable-runs-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory)?;

        let mut initial = PersistedState::default();
        let mut project = ProjectItem::new(1, "durable".into(), "checkout".into());
        let mut thread = ThreadItem::new(1, "build thread".into());
        thread.agent_session = Some("session-664".into());
        thread.worktree_path = Some("checkout".into());
        thread.activity = AgentActivity::Idle;
        project.threads.push(thread);
        initial.projects.push(project);
        std::fs::write(directory.join("state.json"), serde_json::to_vec(&initial)?)?;

        let mut store = StateStore::load_or_default(&directory)?;
        store.defer_saves();
        let store = Arc::new(StoreLock::new(store));
        let persistence = ThreadStorePersistence::new(Arc::clone(&store), (1, 1));

        let mut accepted = QueuedRunRecord::new(
            "velocity-run-664-add".into(),
            "cargo".into(),
            vec!["check".into(), "-p".into(), "velocity-acp".into()],
            None,
            QueuedRunClass::Build,
        );
        accepted.phase = QueuedRunPhase::WaitingForTurnEnd;
        accepted.log = Some("C:\\temp\\velocity-run-664-add.log".into());

        // Simulate restart immediately after `queue_run` persists, without running the background
        // saver or the Tauri event pump.
        persistence.persist_added(&accepted)?;
        let after_add = StateStore::load_or_default(&directory)?;
        assert_eq!(
            crate::restart::interrupted_turns(after_add.state()),
            vec![(1, 1)]
        );
        let saved_after_add = &after_add.project(1).ok_or("missing project")?.threads[0];
        assert_eq!(saved_after_add.queued_runs.len(), 1);
        assert_eq!(saved_after_add.queued_runs[0].id, "velocity-run-664-add");
        assert!(saved_after_add.queued_run_history.is_empty());

        // Simulate restart immediately after `run_cancel` persists, before any finished event
        // reaches the event pump.
        let mut cancelled = accepted.clone();
        cancelled.phase = QueuedRunPhase::Cancelled;
        cancelled.queue_reason = None;
        cancelled.exit_status = Some("cancelled".into());
        cancelled.delivered = true;
        persistence.persist_cancelled(&cancelled)?;

        let after_cancel = StateStore::load_or_default(&directory)?;
        assert!(crate::restart::interrupted_turns(after_cancel.state()).is_empty());
        let saved_after_cancel = &after_cancel.project(1).ok_or("missing project")?.threads[0];
        assert!(saved_after_cancel.queued_runs.is_empty());
        assert_eq!(saved_after_cancel.queued_run_history.len(), 1);
        assert_eq!(
            saved_after_cancel.queued_run_history[0].id,
            "velocity-run-664-add"
        );
        assert_eq!(
            saved_after_cancel.queued_run_history[0].phase,
            QueuedRunPhase::Cancelled
        );
        assert!(saved_after_cancel.queued_run_history[0].delivered);

        // A delayed `QueuedRunAdded` event from the event pump must not resurrect a cancelled run.
        let mut replay_check = saved_after_cancel.clone();
        record_added(&mut replay_check, &accepted);
        assert!(replay_check.queued_runs.is_empty());

        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }
}
