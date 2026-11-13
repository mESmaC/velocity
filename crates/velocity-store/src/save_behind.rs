//! Saving `state.json` behind the store lock (#285, #320).
//!
//! Every thread change used to clone the whole saved state, serialize it, fsync it and
//! rename it into place while holding the store lock, several times a second while agents
//! ran. Window commands waited on that lock. In deferred mode a change only updates the
//! state in memory and bumps its revision; [`save_pending`] then writes the latest
//! revision from a background loop, serializing and writing outside the store lock.
//!
//! A store that never calls [`StateStore::defer_saves`] (tests, examples, stress scripts)
//! still writes every change before returning, as before.

use std::sync::{Arc, Mutex, PoisonError};

use crate::{PersistedState, StateStore, StoreError, write_atomic_snapshot};

impl StateStore {
    /// Stops writing `state.json` on every change: [`save_pending`] writes it instead.
    pub fn defer_saves(&mut self) {
        self.deferred_saves = true;
    }

    /// Whether a change is in memory but not yet in `state.json`.
    pub fn has_unsaved_changes(&self) -> bool {
        self.disk_revision != self.state.revision
    }

    /// Replaces the whole state with `next` as the next revision. Without deferred saves,
    /// `next` is written first, and a failed write leaves the state as it was.
    pub(crate) fn store_snapshot(&mut self, mut next: PersistedState) -> Result<(), StoreError> {
        next.revision = self.state.revision.wrapping_add(1);
        if !self.deferred_saves {
            write_atomic_snapshot(&self.directory, &next)?;
            self.disk_revision = next.revision;
        }
        self.state = next;
        Ok(())
    }

    /// Records a change already made to `self.state` in place as the next revision.
    /// Without deferred saves it is written first; on a failed write the revision goes
    /// back and the caller undoes its change.
    pub(crate) fn record_change(&mut self) -> Result<(), StoreError> {
        let previous = self.state.revision;
        self.state.revision = previous.wrapping_add(1);
        if self.deferred_saves {
            return Ok(());
        }
        match write_atomic_snapshot(&self.directory, &self.state) {
            Ok(()) => {
                self.disk_revision = self.state.revision;
                Ok(())
            }
            Err(error) => {
                self.state.revision = previous;
                Err(error)
            }
        }
    }
}

/// Writes the store's latest revision to `state.json` if it isn't there yet. The store
/// lock is held only to copy the state and to record the write, never while serializing
/// or writing. Returns whether it wrote. Safe to call from several threads: writes are
/// serialized and each one copies the newest state, so an older revision never replaces a
/// newer one on disk.
pub fn save_pending(store: &Mutex<StateStore>) -> Result<bool, StoreError> {
    let write_lock = {
        let guard = store.lock().unwrap_or_else(PoisonError::into_inner);
        if !guard.has_unsaved_changes() {
            return Ok(false);
        }
        Arc::clone(&guard.write_lock)
    };
    let _writing = write_lock.lock().unwrap_or_else(PoisonError::into_inner);
    let (directory, snapshot) = {
        let guard = store.lock().unwrap_or_else(PoisonError::into_inner);
        if !guard.has_unsaved_changes() {
            return Ok(false);
        }
        (guard.directory.clone(), guard.state.clone())
    };
    write_atomic_snapshot(&directory, &snapshot)?;
    let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
    guard.disk_revision = snapshot.revision;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectItem;

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

        let store = Mutex::new(store);
        assert!(save_pending(&store)?);
        assert!(!save_pending(&store)?);
        let loaded = StateStore::load_or_default(&directory)?;
        assert_eq!(loaded.state().projects.len(), 1);
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }
}
