//! Durable project mirror state, independent of the thread event cursor (F5, #402).

use velocity_core::rosen_mirror::{MirrorDivergence, MirrorState};

use crate::{ProjectLocationKind, StateStore, StoreError};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MirrorRejection {
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: Option<MirrorState>,
}

impl StateStore {
    pub fn resume_rosen_mirror(
        &mut self,
        project: u32,
        identity: &str,
        latest: u64,
        state: Option<&MirrorState>,
    ) -> Result<(), StoreError> {
        let Some(current) = self.project(project) else {
            return Ok(());
        };
        let changed = current.rosen_mirror_server_identity.as_deref() != Some(identity);
        if !changed && current.rosen_mirror_sequence <= latest {
            return Ok(());
        }
        let outcome = self.edit_project_in_place(project, |item| {
            let previous = item.rosen_mirror_sequence;
            item.rosen_mirror_sequence = 0;
            if item.rosen_mirror.as_ref() != state {
                item.rosen_mirror_rejection_sequence = None;
                item.rosen_mirror_notices.clear();
            }
            let replacing = item.rosen_mirror_server_identity.is_some() && changed;
            item.rosen_mirror_server_identity = Some(identity.to_owned());
            (true, (previous, replacing))
        })?;
        if let Some((previous, replacing)) = outcome
            && (previous != 0 || replacing)
        {
            eprintln!(
                "Rosen mirror for project {project}: restarting at 0 (saved {previous}, latest {latest}, database changed: {changed})"
            );
        }
        Ok(())
    }

    pub fn save_rosen_mirror(
        &mut self,
        project: u32,
        sequence: u64,
        state: Option<MirrorState>,
    ) -> Result<(), StoreError> {
        let Some(current) = self.project(project) else {
            return Ok(());
        };
        if sequence < current.rosen_mirror_sequence
            || (sequence == current.rosen_mirror_sequence && state == current.rosen_mirror)
        {
            return Ok(());
        }
        self.edit_project_in_place(project, |item| {
            if state.as_ref().is_some_and(|state| state.rejected)
                && item.rosen_mirror_rejection_sequence.is_none()
            {
                item.rosen_mirror_rejection_sequence = Some(sequence);
                item.rosen_mirror_notices.push(MirrorRejection {
                    sequence,
                    name: item.name.clone(),
                    state: state.clone(),
                });
            }
            if state.is_none() {
                item.rosen_mirror_rejection_sequence = None;
            }
            item.rosen_mirror = state;
            item.rosen_mirror_sequence = sequence;
            (true, ())
        })?;
        Ok(())
    }

    /// Saves the project's Collapse repository (#429) and whether the landed `rosen.toml`
    /// names its Collapse root (#517), and reports whether either changed. `live_state`
    /// of `None` keeps what was saved, for a refresh that could not read the configuration.
    pub fn save_collapse_repository(
        &mut self,
        project: u32,
        repository: Option<String>,
        live_state: Option<bool>,
    ) -> Result<bool, StoreError> {
        let Some(current) = self.project(project) else {
            return Ok(false);
        };
        let moved = current.collapse_repository != repository;
        // Live state belongs to the mirrored repository: none without one, and another
        // repository's is unknown until its configuration is read.
        let kept = !moved && current.collapse_live_state;
        let live_state = repository.is_some() && live_state.unwrap_or(kept);
        if !moved && current.collapse_live_state == live_state {
            return Ok(false);
        }
        self.edit_project_in_place(project, |item| {
            if moved {
                // Another repository's forwarder state would mislead until the next read.
                item.collapse_mirror_health = None;
            }
            item.collapse_repository = repository;
            item.collapse_live_state = live_state;
            (true, ())
        })?;
        Ok(true)
    }

    /// Saves the project's GitHub `owner/repository` for issues, base CI, releases and
    /// Rosen pin bumps (#574), and reports whether it changed. Clearing or changing the
    /// repository clears any cached base CI from the previous repository.
    pub fn save_github_remote(
        &mut self,
        project: u32,
        repository: Option<String>,
    ) -> Result<bool, StoreError> {
        let Some(current) = self.project(project) else {
            return Ok(false);
        };
        let location_kind = if repository.is_some() {
            ProjectLocationKind::GitHub
        } else {
            ProjectLocationKind::Local
        };
        if current.github_remote == repository && current.location_kind == location_kind {
            return Ok(false);
        }
        self.edit_project_in_place(project, |item| {
            if item.github_remote != repository {
                item.base_ci = None;
            }
            item.github_remote = repository;
            item.location_kind = location_kind;
            (true, ())
        })?;
        Ok(true)
    }

    /// Saves what Collapse last reported for the project's mirror and reports whether it
    /// changed (#427). `None` clears it, as when Collapse no longer lists the repository.
    pub fn save_collapse_mirror_health(
        &mut self,
        project: u32,
        health: Option<velocity_core::collapse::CollapseMirrorHealth>,
    ) -> Result<bool, StoreError> {
        let unchanged = self
            .project(project)
            .is_none_or(|item| item.collapse_mirror_health == health);
        if unchanged {
            return Ok(false);
        }
        self.edit_project_in_place(project, |item| {
            item.collapse_mirror_health = health;
            (true, ())
        })?;
        Ok(true)
    }

    /// Saves the divergence the last catch-up from the mirror found, or clears it, and
    /// reports whether it changed (#501).
    pub fn save_mirror_divergence(
        &mut self,
        project: u32,
        divergence: Option<MirrorDivergence>,
    ) -> Result<bool, StoreError> {
        let unchanged = self
            .project(project)
            .is_none_or(|item| item.rosen_mirror_divergence == divergence);
        if unchanged {
            return Ok(false);
        }
        self.edit_project_in_place(project, |item| {
            item.rosen_mirror_divergence = divergence;
            (true, ())
        })?;
        Ok(true)
    }

    pub fn finish_mirror_notice(&mut self, project: u32, sequence: u64) -> Result<(), StoreError> {
        self.edit_project_in_place(project, |item| {
            let before = item.rosen_mirror_notices.len();
            item.rosen_mirror_notices
                .retain(|notice| notice.sequence != sequence);
            (item.rosen_mirror_notices.len() != before, ())
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use velocity_core::rosen_mirror::MirrorDivergence;

    use crate::{ProjectItem, StateStore};

    // Risk (#501): a divergence lost on reload hides that the base and mirror need a
    // person, and one that survives its clearing shows a stale notice after a catch-up.
    #[test]
    fn mirror_divergence_survives_reload_until_cleared() -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-mirror-divergence-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;
        let divergence = MirrorDivergence {
            base_commit: "a".repeat(40),
            mirror_commit: "b".repeat(40),
            detail: "base main and mirror branch main have diverged".into(),
        };
        assert!(store.save_mirror_divergence(1, Some(divergence.clone()))?);
        assert!(!store.save_mirror_divergence(1, Some(divergence.clone()))?);

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store.state.projects[0].rosen_mirror_divergence,
            Some(divergence)
        );
        assert!(store.save_mirror_divergence(1, None)?);

        let store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.state.projects[0].rosen_mirror_divergence, None);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#427): a stopped forwarder lost on reload hides that GitHub stopped receiving
    // lands, and health kept after the mirror moves describes another repository.
    #[test]
    fn collapse_mirror_health_survives_reload_until_the_repository_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        use velocity_core::collapse::{CollapseMirrorHealth, ForwarderState};
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-collapse-health-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;
        assert!(store.save_collapse_repository(1, Some("owner/one".into()), Some(false))?);
        // Risk (#517): a linked project that reloads as unlinked offers to land the
        // `collapse` root again, and an unread configuration must not unlink it.
        assert!(store.save_collapse_repository(1, Some("owner/one".into()), Some(true))?);
        assert!(!store.save_collapse_repository(1, Some("owner/one".into()), None)?);
        let health = CollapseMirrorHealth {
            forwarder: ForwarderState::Stopped,
            behind: 0,
            message: Some("! [rejected] main -> main (fetch first)".into()),
            next_attempt: None,
            last_land_at: Some("2026-11-19T20:00:00Z".into()),
            last_land_agent: Some("issue-427@server".into()),
            archived: false,
            downstream_github: Some("mESmaC/one".into()),
        };
        assert!(store.save_collapse_mirror_health(1, Some(health.clone()))?);
        assert!(!store.save_collapse_mirror_health(1, Some(health.clone()))?);

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.state.projects[0].collapse_mirror_health, Some(health));
        assert!(store.state.projects[0].collapse_live_state);
        assert!(store.save_collapse_repository(1, Some("owner/two".into()), None)?);

        let store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.state.projects[0].collapse_mirror_health, None);
        assert!(!store.state.projects[0].collapse_live_state);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#574): moving a GitHub project to a Collapse mirror must keep `github_remote`
    // pointing at GitHub across save and reload, and edits to `github_remote` must persist
    // while clearing stale base CI from another repository.
    #[test]
    fn github_remote_survives_collapse_mirror_and_reload() -> Result<(), Box<dyn std::error::Error>>
    {
        use crate::ProjectLocationKind;
        use velocity_core::base_ci::{BaseCi, BaseCiState};

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-github-remote-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;

        assert!(store.save_github_remote(1, Some("mESmaC/velocity".into()))?);
        assert!(!store.save_github_remote(1, Some("mESmaC/velocity".into()))?);
        store.save_base_ci(
            1,
            Some(BaseCi {
                state: BaseCiState::Green,
                commit: Some("a".repeat(40)),
                failing: Vec::new(),
                running: false,
            }),
        )?;
        assert!(store.save_collapse_repository(1, Some("owner/velocity".into()), Some(true))?);

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store.state.projects[0].github_remote.as_deref(),
            Some("mESmaC/velocity")
        );
        assert_eq!(
            store.state.projects[0].location_kind,
            ProjectLocationKind::GitHub
        );
        assert_eq!(
            store.state.projects[0].collapse_repository.as_deref(),
            Some("owner/velocity")
        );
        assert!(store.state.projects[0].base_ci.is_some());

        assert!(store.save_github_remote(1, None)?);
        let store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.state.projects[0].github_remote, None);
        assert_eq!(
            store.state.projects[0].location_kind,
            ProjectLocationKind::Local
        );
        assert_eq!(store.state.projects[0].base_ci, None);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
