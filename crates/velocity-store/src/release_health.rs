//! Persisted release-gate recovery history per project (#549).
//!
//! Keeps the IDs of refused `release.yml` runs that project health has already handled so
//! restarting Velocity does not re-dispatch a release for the same refused run on every
//! startup.

use crate::{StateStore, StoreError};

/// Maximum handled run IDs retained per project so `state.json` stays bounded.
const MAX_RETRIED_RELEASE_RUNS: usize = 64;

impl StateStore {
    /// Whether project health already handled the refused `release.yml` run `run_id`.
    pub fn has_retried_release_run(&self, project: u32, run_id: u64) -> bool {
        self.project(project)
            .is_some_and(|item| item.retried_release_runs.contains(&run_id))
    }

    /// Persists that project health handled refused `release.yml` run `run_id`, returning
    /// `true` when the saved state changed (#549).
    pub fn record_retried_release_run(
        &mut self,
        project: u32,
        run_id: u64,
    ) -> Result<bool, StoreError> {
        let Some(existing) = self.project(project) else {
            return Ok(false);
        };
        if existing.retried_release_runs.contains(&run_id) {
            return Ok(false);
        }
        let mut next = self.state.clone();
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            item.retried_release_runs.push(run_id);
            if item.retried_release_runs.len() > MAX_RETRIED_RELEASE_RUNS {
                let excess = item.retried_release_runs.len() - MAX_RETRIED_RELEASE_RUNS;
                item.retried_release_runs.drain(..excess);
            }
        }
        self.store_snapshot(next)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::MAX_RETRIED_RELEASE_RUNS;
    use crate::{ProjectItem, StateStore};

    // Risk (#549): losing handled refused release-gate run IDs across a restart causes
    // project health to re-dispatch `release.yml` on every startup while the refused run
    // remains the newest workflow run.
    #[test]
    fn retried_release_runs_survive_reload_and_stay_bounded()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-retried-runs-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;

        assert!(store.record_retried_release_run(1, 44_617_086_818)?);
        assert!(!store.record_retried_release_run(1, 44_617_086_818)?);

        let mut store = StateStore::load_or_default(&directory)?;
        assert!(store.has_retried_release_run(1, 44_617_086_818));
        assert!(!store.has_retried_release_run(1, 99));

        for offset in 0..(MAX_RETRIED_RELEASE_RUNS as u64 + 4) {
            assert!(store.record_retried_release_run(1, 1000 + offset)?);
        }
        let store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store.state.projects[0].retried_release_runs.len(),
            MAX_RETRIED_RELEASE_RUNS
        );
        assert!(!store.has_retried_release_run(1, 44_617_086_818));
        assert!(store.has_retried_release_run(1, 1000 + MAX_RETRIED_RELEASE_RUNS as u64 + 3));

        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
