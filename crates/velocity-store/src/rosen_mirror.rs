//! Durable project mirror state, independent of the thread event cursor (F5, #402).

use velocity_core::base_ci::BaseCiSource;
use velocity_core::collapse::IssueHost;
use velocity_core::project_health::{HostSourceChange, host_source_change_can_switch_back};
use velocity_core::rosen_mirror::{MirrorDivergence, MirrorState};

use crate::{ProjectItem, ProjectLocationKind, StateStore, StoreError};

/// Outcome of resolving a non-owner issue host or base CI source change (#613).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedHostChange {
    pub project_name: String,
    pub effective_issue_host: IssueHost,
    pub effective_ci_source: BaseCiSource,
    pub issue_host_changed: bool,
    pub ci_source_changed: bool,
    pub collapse_repository: Option<String>,
}

/// Result of attempting to switch back or acknowledge a pending host/CI change (#613).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostChangeResolution {
    Resolved(ResolvedHostChange),
    CannotSwitchBack(String),
    NothingPending,
}

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
        self.save_collapse_repository_with_origin(project, repository, live_state, false, 0)
    }

    /// Saves the project's Collapse repository and live-state flag, recording a non-owner
    /// host/CI drift notice when `owner_choice` is `false` and the effective host or CI
    /// source shifted away from the acknowledged setting (#613).
    pub fn save_collapse_repository_with_origin(
        &mut self,
        project: u32,
        repository: Option<String>,
        live_state: Option<bool>,
        owner_choice: bool,
        now_ms: u64,
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
            if owner_choice {
                return self.acknowledge_host_sources(project);
            }
            return Ok(false);
        }
        self.edit_project_in_place(project, |item| {
            let previous_ci = item.effective_base_ci_source();
            if moved {
                // Another repository's forwarder state would mislead until the next read.
                item.collapse_mirror_health = None;
            }
            item.collapse_repository = repository;
            item.collapse_live_state = live_state;
            let next_ci = item.effective_base_ci_source();
            if next_ci != previous_ci
                || item.base_ci.as_ref().is_some_and(|ci| ci.source != next_ci)
            {
                item.base_ci = None;
            }
            if owner_choice {
                acknowledge_item_host_sources(item);
            } else {
                record_non_owner_host_drift(item, "[mirror] remote in rosen.toml changed", now_ms);
            }
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

    /// Returns the Collapse relay action kinds configured for `project`, defaulting to
    /// every kind Collapse offers when none have been customized (#576).
    pub fn collapse_relay_allow(
        &self,
        project: u32,
    ) -> Vec<velocity_core::collapse::CollapseRelayKind> {
        self.project(project)
            .and_then(|item| item.collapse_relay_allow.as_ref())
            .map(|kinds| {
                velocity_core::collapse::CollapseRelayKind::normalize(kinds.iter().copied())
            })
            .unwrap_or_else(|| velocity_core::collapse::CollapseRelayKind::ALL.to_vec())
    }

    /// Saves the Collapse relay action kinds for `project` (`None` restores the default
    /// all-kinds policy) and reports whether the saved value changed (#576).
    pub fn save_collapse_relay_allow(
        &mut self,
        project: u32,
        allow: Option<Vec<velocity_core::collapse::CollapseRelayKind>>,
    ) -> Result<bool, StoreError> {
        let normalized = allow.map(velocity_core::collapse::CollapseRelayKind::normalize);
        let unchanged = self
            .project(project)
            .is_none_or(|item| item.collapse_relay_allow == normalized);
        if unchanged {
            return Ok(false);
        }
        self.edit_project_in_place(project, |item| {
            item.collapse_relay_allow = normalized;
            (true, ())
        })?;
        Ok(true)
    }

    /// Saves the project's explicit issue host (`github` or `collapse`) (#589) and reports
    /// whether the saved value changed. Clears cached `base_ci` when the effective host or CI
    /// source changes so GitHub Actions status is never shown while CI runs from Rosen (#592, #613).
    pub fn save_issue_host(&mut self, project: u32, host: IssueHost) -> Result<bool, StoreError> {
        let unchanged = self.project(project).is_none_or(|item| {
            item.issue_host == Some(host)
                && item.host_source_change.is_none()
                && item.acknowledged_issue_host == Some(item.effective_issue_host())
                && item.acknowledged_base_ci_source == Some(item.effective_base_ci_source())
        });
        if unchanged {
            return Ok(false);
        }
        let changed = self.edit_project_in_place(project, |item| {
            let saved_changed = item.issue_host != Some(host);
            let previous_host = item.effective_issue_host();
            let previous_ci = item.effective_base_ci_source();
            item.issue_host = Some(host);
            let next_host = item.effective_issue_host();
            let next_ci = item.effective_base_ci_source();
            if next_host != previous_host
                || next_ci != previous_ci
                || item.base_ci.as_ref().is_some_and(|ci| ci.source != next_ci)
            {
                item.base_ci = None;
            }
            let ack_changed = acknowledge_item_host_sources(item);
            (saved_changed || ack_changed, saved_changed)
        })?;
        Ok(changed.unwrap_or(false))
    }

    /// Startup backfill (#613): saves `IssueHost::Collapse` once when the project has the
    /// expected `collapse_repository` and no saved `issue_host` yet, keeping pre-#612 moved
    /// projects on Collapse without flagging an unexpected host change.
    pub fn backfill_collapse_issue_host(
        &mut self,
        project: u32,
        expected_collapse_repository: &str,
    ) -> Result<bool, StoreError> {
        let eligible = self.project(project).is_some_and(|item| {
            item.issue_host.is_none()
                && item
                    .collapse_repository
                    .as_deref()
                    .is_some_and(|repo| repo.eq_ignore_ascii_case(expected_collapse_repository))
        });
        if !eligible {
            return Ok(false);
        }
        self.edit_project_in_place(project, |item| {
            item.issue_host = Some(IssueHost::Collapse);
            if item
                .base_ci
                .as_ref()
                .is_some_and(|ci| ci.source != BaseCiSource::Rosen)
            {
                item.base_ci = None;
            }
            acknowledge_item_host_sources(item);
            (true, ())
        })?;
        Ok(true)
    }

    /// Marks the project's current effective issue host and CI source as owner-acknowledged
    /// and clears any pending unexpected host/CI change notice (#613).
    pub fn acknowledge_host_sources(&mut self, project: u32) -> Result<bool, StoreError> {
        let changed = self.edit_project_in_place(project, |item| {
            let mut dirty = acknowledge_item_host_sources(item);
            let next_ci = item.effective_base_ci_source();
            if item.base_ci.as_ref().is_some_and(|ci| ci.source != next_ci) {
                item.base_ci = None;
                dirty = true;
            }
            (dirty, dirty)
        })?;
        Ok(changed.unwrap_or(false))
    }

    /// Compares a project's effective issue host and base CI source against its acknowledged
    /// values, recording a non-owner drift notice when either changed or establishing the
    /// initial baseline when unset (#613).
    pub fn reconcile_host_sources(
        &mut self,
        project: u32,
        reason: &str,
        now_ms: u64,
    ) -> Result<Option<HostSourceChange>, StoreError> {
        let result = self.edit_project_in_place(project, |item| {
            let mut dirty = record_non_owner_host_drift(item, reason, now_ms);
            let next_ci = item.effective_base_ci_source();
            if item.base_ci.as_ref().is_some_and(|ci| ci.source != next_ci) {
                item.base_ci = None;
                dirty = true;
            }
            if item.host_source_change.is_none() {
                if item.acknowledged_issue_host.is_none() {
                    item.acknowledged_issue_host = Some(item.effective_issue_host());
                    dirty = true;
                }
                if item.acknowledged_base_ci_source.is_none() {
                    item.acknowledged_base_ci_source = Some(next_ci);
                    dirty = true;
                }
            }
            (dirty, item.host_source_change.clone())
        })?;
        Ok(result.flatten())
    }

    /// Resolves a pending non-owner host/CI source change by switching back in one click or
    /// acknowledging the current setting (#613).
    pub fn resolve_host_source_change(
        &mut self,
        project: u32,
        switch_back: bool,
    ) -> Result<HostChangeResolution, StoreError> {
        let Some(current) = self.project(project) else {
            return Ok(HostChangeResolution::NothingPending);
        };
        let Some(change) = current.host_source_change.clone() else {
            return Ok(HostChangeResolution::NothingPending);
        };
        if switch_back
            && !host_source_change_can_switch_back(
                &change,
                current.collapse_repository.is_some(),
                current.github_remote.is_some(),
            )
        {
            return Ok(HostChangeResolution::CannotSwitchBack(
                cannot_switch_back_reason(&change, current),
            ));
        }
        let resolved = self.edit_project_in_place(project, |item| {
            let prev_host = item.effective_issue_host();
            let prev_ci = item.effective_base_ci_source();
            if switch_back {
                apply_host_switch_back(item, &change);
            }
            let next_host = item.effective_issue_host();
            let next_ci = item.effective_base_ci_source();
            if next_ci != prev_ci || item.base_ci.as_ref().is_some_and(|ci| ci.source != next_ci) {
                item.base_ci = None;
            }
            acknowledge_item_host_sources(item);
            let outcome = ResolvedHostChange {
                project_name: item.name.clone(),
                effective_issue_host: next_host,
                effective_ci_source: next_ci,
                issue_host_changed: next_host != prev_host,
                ci_source_changed: next_ci != prev_ci,
                collapse_repository: item.collapse_repository.clone(),
            };
            (true, outcome)
        })?;
        Ok(match resolved {
            Some(outcome) => HostChangeResolution::Resolved(outcome),
            None => HostChangeResolution::NothingPending,
        })
    }

    /// Saves the project's post-switch one-way GitHub issue sync configuration and progress
    /// (#612), reporting whether the stored record changed.
    pub fn save_github_issue_sync(
        &mut self,
        project: u32,
        sync: Option<velocity_core::collapse::GitHubIssueSync>,
    ) -> Result<bool, StoreError> {
        let unchanged = self
            .project(project)
            .is_none_or(|item| item.github_issue_sync == sync);
        if unchanged {
            return Ok(false);
        }
        self.edit_project_in_place(project, |item| {
            item.github_issue_sync = sync;
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

fn acknowledge_item_host_sources(item: &mut ProjectItem) -> bool {
    let next_host = Some(item.effective_issue_host());
    let next_ci = Some(item.effective_base_ci_source());
    let changed = item.acknowledged_issue_host != next_host
        || item.acknowledged_base_ci_source != next_ci
        || item.host_source_change.is_some();
    item.acknowledged_issue_host = next_host;
    item.acknowledged_base_ci_source = next_ci;
    item.host_source_change = None;
    changed
}

fn record_non_owner_host_drift(item: &mut ProjectItem, reason: &str, now_ms: u64) -> bool {
    let current_host = item.effective_issue_host();
    let current_ci = item.effective_base_ci_source();
    let expected_host = item.acknowledged_issue_host;
    let expected_ci = item.acknowledged_base_ci_source.or_else(|| {
        item.base_ci
            .as_ref()
            .map(|ci| ci.source)
            .filter(|source| *source == BaseCiSource::Rosen && current_ci == BaseCiSource::GitHub)
    });
    let host_diff = expected_host.filter(|prev| *prev != current_host);
    let ci_diff = expected_ci.filter(|prev| *prev != current_ci);
    if host_diff.is_none() && ci_diff.is_none() {
        if item.host_source_change.is_some()
            && item.acknowledged_issue_host.is_some()
            && item.acknowledged_base_ci_source.is_some()
        {
            item.host_source_change = None;
            return true;
        }
        return false;
    }
    let next = HostSourceChange {
        previous_issue_host: host_diff,
        current_issue_host: host_diff.map(|_| current_host),
        previous_ci_source: ci_diff,
        current_ci_source: ci_diff.map(|_| current_ci),
        reason: reason.to_owned(),
        detected_at_ms: now_ms,
    };
    if item.host_source_change.as_ref() == Some(&next) {
        return false;
    }
    item.host_source_change = Some(next);
    true
}

fn apply_host_switch_back(item: &mut ProjectItem, change: &HostSourceChange) {
    if let Some((previous_host, _)) = change.issue_host_changed() {
        item.issue_host = Some(previous_host);
    }
    if let Some((previous_ci, _)) = change.ci_source_changed() {
        let default_ci = velocity_core::base_ci::effective_base_ci_source(
            item.collapse_repository.is_some(),
            item.effective_issue_host(),
            None,
            item.github_remote.is_some(),
        );
        item.base_ci_source = (default_ci != previous_ci).then_some(previous_ci);
    }
}

fn cannot_switch_back_reason(change: &HostSourceChange, item: &ProjectItem) -> String {
    if matches!(change.issue_host_changed(), Some((IssueHost::Collapse, _)))
        && item.collapse_repository.is_none()
    {
        return "This project no longer mirrors to Collapse; link a Collapse mirror before switching issues back to Collapse.".into();
    }
    if matches!(change.ci_source_changed(), Some((BaseCiSource::GitHub, _))) {
        if item.collapse_repository.is_some() {
            return "Projects whose [mirror] remote is Collapse always use Rosen checkpoints instead of GitHub Actions.".into();
        }
        if item.github_remote.is_none() {
            return "Configure a GitHub repository in Settings before switching base CI back to GitHub Actions.".into();
        }
    }
    "Cannot switch back automatically for this project.".into()
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

    // Risk (#574, #613): moving a GitHub project to a Collapse mirror must keep `github_remote`
    // pointing at GitHub across save and reload while clearing stale GitHub base CI as CI
    // switches to Rosen, and edits to `github_remote` must persist while clearing stale base CI.
    #[test]
    fn github_remote_survives_collapse_mirror_and_reload() -> Result<(), Box<dyn std::error::Error>>
    {
        use crate::ProjectLocationKind;
        use velocity_core::base_ci::{BaseCi, BaseCiSource, BaseCiState};

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-github-remote-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;

        assert!(store.save_github_remote(1, Some("mESmaC/velocity".into()))?);
        assert!(!store.save_github_remote(1, Some("mESmaC/velocity".into()))?);
        let github_ci = BaseCi {
            source: BaseCiSource::GitHub,
            state: BaseCiState::Green,
            commit: Some("a".repeat(40)),
            ..BaseCi::default()
        };
        store.save_base_ci(1, Some(github_ci))?;
        assert!(store.save_collapse_repository(1, Some("owner/velocity".into()), Some(true))?);

        let mut store = StateStore::load_or_default(&directory)?;
        let project = &store.state.projects[0];
        assert_eq!(project.github_remote.as_deref(), Some("mESmaC/velocity"));
        assert_eq!(project.location_kind, ProjectLocationKind::GitHub);
        assert_eq!(
            project.collapse_repository.as_deref(),
            Some("owner/velocity")
        );
        assert_eq!(project.effective_base_ci_source(), BaseCiSource::Rosen);
        // Linking a Collapse mirror switches base CI to Rosen and clears stale GitHub CI (#613).
        assert_eq!(project.base_ci, None);

        let rosen_ci = BaseCi {
            source: BaseCiSource::Rosen,
            state: BaseCiState::Green,
            ..BaseCi::default()
        };
        store.save_base_ci(1, Some(rosen_ci))?;
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

    // Risk (#576): a customized or cleared Collapse relay allow-list lost on reload would
    // re-enable disallowed actions when Velocity restarts or rebuilds the Rosen server.
    #[test]
    fn collapse_relay_allow_survives_reload() -> Result<(), Box<dyn std::error::Error>> {
        use velocity_core::collapse::CollapseRelayKind;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-collapse-relay-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;

        assert_eq!(
            store.collapse_relay_allow(1),
            CollapseRelayKind::ALL.to_vec()
        );
        let subset = vec![
            CollapseRelayKind::Mark,
            CollapseRelayKind::Approve,
            CollapseRelayKind::Approve,
        ];
        assert!(store.save_collapse_relay_allow(1, Some(subset))?);
        assert!(!store.save_collapse_relay_allow(
            1,
            Some(vec![CollapseRelayKind::Approve, CollapseRelayKind::Mark]),
        )?);

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store.collapse_relay_allow(1),
            vec![CollapseRelayKind::Approve, CollapseRelayKind::Mark]
        );
        assert!(store.save_collapse_relay_allow(1, Some(Vec::new()))?);

        let mut store = StateStore::load_or_default(&directory)?;
        assert!(store.collapse_relay_allow(1).is_empty());
        assert!(store.save_collapse_relay_allow(1, None)?);

        let store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store.collapse_relay_allow(1),
            CollapseRelayKind::ALL.to_vec()
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#589, #612): a project with a Collapse repository but no explicit issue host must
    // stay on GitHub so never-imported Collapse repositories do not hide open GitHub issues,
    // and explicit host + post-switch sync state must survive save and reload.
    #[test]
    fn issue_host_defaults_and_survives_reload() -> Result<(), Box<dyn std::error::Error>> {
        use velocity_core::base_ci::{BaseCi, BaseCiState};
        use velocity_core::collapse::{GitHubIssueSync, IssueHost};

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-issue-host-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;

        // Without a Collapse mirror, default is GitHub.
        assert_eq!(
            store.state.projects[0].effective_issue_host(),
            IssueHost::GitHub
        );
        // Linking a Collapse repository alone does NOT flip the issue host (#612).
        assert!(store.save_github_remote(1, Some("mESmaC/one".into()))?);
        assert!(store.save_collapse_repository(1, Some("mESmaC/one".into()), Some(true))?);
        assert_eq!(store.state.projects[0].issue_host, None);
        assert_eq!(
            store.state.projects[0].effective_issue_host(),
            IssueHost::GitHub
        );
        assert_eq!(
            store.state.projects[0].issue_host_repository(),
            Some((IssueHost::GitHub, "mESmaC/one".into()))
        );

        // Explicit GitHub host persists across reload.
        assert!(store.save_issue_host(1, IssueHost::GitHub)?);
        assert!(!store.save_issue_host(1, IssueHost::GitHub)?);
        store.save_base_ci(
            1,
            Some(BaseCi {
                source: velocity_core::base_ci::BaseCiSource::GitHub,
                state: BaseCiState::Green,
                commit: Some("a".repeat(40)),
                failing: Vec::new(),
                running: false,
                ..BaseCi::default()
            }),
        )?;

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.state.projects[0].issue_host, Some(IssueHost::GitHub));
        assert_eq!(
            store.state.projects[0].effective_issue_host(),
            IssueHost::GitHub
        );
        assert!(store.state.projects[0].base_ci.is_some());

        // Switching explicitly to Collapse activates Collapse and clears cached GitHub CI.
        assert!(store.save_issue_host(1, IssueHost::Collapse)?);
        assert_eq!(store.state.projects[0].base_ci, None);
        let mut sync = GitHubIssueSync {
            enabled: true,
            github_repository: "mESmaC/one".into(),
            high_water_number: 42,
            expires_at_secs: Some(1_900_000_000),
            last_synced_at_ms: Some(1_800_000_000_000),
            last_error: None,
            ..GitHubIssueSync::default()
        };
        sync.synced_numbers.insert(43, 44);
        assert!(store.save_github_issue_sync(1, Some(sync.clone()))?);
        assert!(!store.save_github_issue_sync(1, Some(sync.clone()))?);

        let store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store.state.projects[0].issue_host,
            Some(IssueHost::Collapse)
        );
        assert_eq!(
            store.state.projects[0].effective_issue_host(),
            IssueHost::Collapse
        );
        assert_eq!(
            store.state.projects[0].issue_host_repository(),
            Some((IssueHost::Collapse, "mESmaC/one".into()))
        );
        assert_eq!(store.state.projects[0].github_issue_sync, Some(sync));
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#613): a pre-#612 moved project must persist its startup-backfilled Collapse host
    // across restarts without triggering a false host-change warning, a Collapse-mirrored
    // project must stay on Rosen CI even when its issue host is GitHub, and a non-owner host
    // or CI drift must survive reload until switched back or acknowledged.
    #[test]
    fn startup_backfill_and_non_owner_host_drift_survive_reload()
    -> Result<(), Box<dyn std::error::Error>> {
        use velocity_core::base_ci::{BaseCi, BaseCiSource, BaseCiState};
        use velocity_core::collapse::IssueHost;

        use crate::rosen_mirror::HostChangeResolution;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-host-drift-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(
            1,
            "Velocity".into(),
            r"C:\project velocity".into(),
        )];
        store.store_snapshot(store.state.clone())?;

        // Collapse mirror forces Rosen CI even when issue_host is unset (GitHub).
        assert!(store.save_github_remote(1, Some("mESmaC/velocity".into()))?);
        assert!(store.save_collapse_repository_with_origin(
            1,
            Some("mESmaC/velocity".into()),
            Some(true),
            true,
            1_700_000_000_000,
        )?);
        assert_eq!(
            store.state.projects[0].effective_issue_host(),
            IssueHost::GitHub
        );
        assert_eq!(
            store.state.projects[0].effective_base_ci_source(),
            BaseCiSource::Rosen
        );

        // Simulate a stale GitHub BaseCi left from #612, then run startup backfill.
        store.save_base_ci(
            1,
            Some(BaseCi {
                source: BaseCiSource::GitHub,
                state: BaseCiState::Failed,
                commit: None,
                failing: vec!["ci".into()],
                running: false,
                ..BaseCi::default()
            }),
        )?;
        assert!(store.backfill_collapse_issue_host(1, "mESmaC/velocity")?);
        // Second backfill is a no-op once issue_host is saved.
        assert!(!store.backfill_collapse_issue_host(1, "mESmaC/velocity")?);
        assert_eq!(
            store.state.projects[0].issue_host,
            Some(IssueHost::Collapse)
        );
        assert_eq!(store.state.projects[0].base_ci, None);
        assert_eq!(store.state.projects[0].host_source_change, None);

        // Simulate an external change (e.g. upgrade or state edit) clearing issue_host back to None.
        store.edit_project_in_place(1, |item| {
            item.issue_host = None;
            (true, ())
        })?;
        let drift = store.reconcile_host_sources(1, "Changed on startup", 1_700_000_010_000)?;
        assert!(drift.is_some());

        // Reload and verify the pending drift persists and one-click switch back restores Collapse.
        let mut store = StateStore::load_or_default(&directory)?;
        let saved_drift = store.state.projects[0].host_source_change.clone();
        assert_eq!(
            saved_drift
                .as_ref()
                .and_then(|item| item.issue_host_changed()),
            Some((IssueHost::Collapse, IssueHost::GitHub))
        );
        let resolution = store.resolve_host_source_change(1, true)?;
        assert!(matches!(
            resolution,
            HostChangeResolution::Resolved(ref outcome)
                if outcome.effective_issue_host == IssueHost::Collapse
                    && outcome.effective_ci_source == BaseCiSource::Rosen
                    && outcome.issue_host_changed
        ));

        let store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store.state.projects[0].issue_host,
            Some(IssueHost::Collapse)
        );
        assert_eq!(store.state.projects[0].host_source_change, None);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
