//! Persisted fleet development ledger mutations and save/load helpers on `StateStore` (#619).

use std::time::{SystemTime, UNIX_EPOCH};

use velocity_core::fleet_ledger::{
    FleetCheckRecord, FleetIssueRecord, FleetLedger, FleetStatusView, issue_key,
};

use crate::{StateStore, StoreError, ThreadItem};

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Metadata recorded when an issue is dispatched (automatically or manually).
#[derive(Clone, Debug)]
pub struct FleetDispatchInput<'a> {
    pub repository: &'a str,
    pub issue: u32,
    pub title: Option<&'a str>,
    pub url: Option<&'a str>,
    pub filed_at: Option<u64>,
    pub thread_id: u32,
    pub provider: &'a str,
    pub model: &'a str,
    pub now_secs: u64,
}

impl StateStore {
    /// Returns the project's fleet status view for the last 24 hours and 7 days.
    pub fn fleet_status_view(&self, project: u32, now_secs: u64) -> Option<FleetStatusView> {
        let item = self.project(project)?;
        Some(FleetStatusView::from_ledger(&item.fleet_ledger, now_secs))
    }

    /// Returns a cloned copy of the project's fleet ledger.
    pub fn fleet_ledger(&self, project: u32) -> Option<FleetLedger> {
        self.project(project).map(|item| item.fleet_ledger.clone())
    }

    /// Records a dispatch attempt for an issue, incrementing `retries` on repeat dispatches.
    pub fn record_fleet_dispatch(
        &mut self,
        project: u32,
        input: FleetDispatchInput<'_>,
    ) -> Result<bool, StoreError> {
        if input.repository.trim().is_empty() || input.issue == 0 {
            return Ok(false);
        }
        let key = issue_key(input.repository, input.issue);
        Ok(self
            .edit_project_in_place(project, |project| {
                let entry = project.fleet_ledger.issues.entry(key).or_insert_with(|| {
                    FleetIssueRecord::new(input.repository.to_owned(), input.issue)
                });
                if entry.dispatched_at.is_some() {
                    entry.retries = entry.retries.saturating_add(1);
                } else {
                    entry.dispatched_at = Some(input.now_secs);
                }
                entry.last_dispatched_at = Some(input.now_secs);
                if entry.filed_at.is_none() {
                    entry.filed_at = input.filed_at;
                }
                if let Some(title) = input.title.filter(|value| !value.trim().is_empty()) {
                    entry.title = Some(title.to_owned());
                }
                if let Some(url) = input.url.filter(|value| !value.trim().is_empty()) {
                    entry.url = Some(url.to_owned());
                }
                entry.thread_id = Some(input.thread_id);
                if !input.provider.trim().is_empty() {
                    entry.provider = Some(input.provider.to_owned());
                }
                if !input.model.trim().is_empty() {
                    entry.model = Some(input.model.to_owned());
                }
                project.fleet_ledger.prune_issues();
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Links a Rosen change ID to the fleet issue record owned by `thread_id`.
    pub fn bind_fleet_change(
        &mut self,
        project: u32,
        thread_id: u32,
        change: &str,
    ) -> Result<bool, StoreError> {
        if change.trim().is_empty() {
            return Ok(false);
        }
        Ok(self
            .edit_project_in_place(project, |project| {
                let Some(record) = ensure_thread_record(project, thread_id) else {
                    return (false, false);
                };
                if record.change.as_deref() == Some(change) {
                    return (false, false);
                }
                record.change = Some(change.to_owned());
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Records the first snapshot timestamp for `change` if not already set.
    pub fn record_fleet_snapshot(
        &mut self,
        project: u32,
        change: &str,
        at_secs: u64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let Some(record) = ensure_change_record(project, change) else {
                    return (false, false);
                };
                if record.first_snapshot_at.is_some() {
                    return (false, false);
                }
                record.first_snapshot_at = Some(at_secs);
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Records when `change` was named (`rosen commit -m`).
    pub fn record_fleet_named(
        &mut self,
        project: u32,
        change: &str,
        at_secs: u64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let Some(record) = ensure_change_record(project, change) else {
                    return (false, false);
                };
                if record.first_snapshot_at.is_none() {
                    record.first_snapshot_at = Some(at_secs);
                }
                if record.named_at == Some(at_secs) {
                    return (false, false);
                }
                record.named_at = Some(at_secs);
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Records when `change` landed at `commit`.
    pub fn record_fleet_landed(
        &mut self,
        project: u32,
        change: &str,
        commit: Option<&str>,
        at_secs: u64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let Some(record) = ensure_change_record(project, change) else {
                    return (false, false);
                };
                let already_same_commit = commit.is_some()
                    && record.landed_commit.as_deref() == commit
                    && record.landed_at.is_some();
                if already_same_commit {
                    return (false, false);
                }
                if record.landed_at.is_none()
                    || commit.is_some_and(|c| record.landed_commit.as_deref() != Some(c))
                {
                    record.landed_at = Some(at_secs);
                }
                if let Some(commit) = commit {
                    record.landed_commit = Some(commit.to_owned());
                }
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Records when post-land verification turned green for `change` / `commit`.
    pub fn record_fleet_post_land_green(
        &mut self,
        project: u32,
        change: &str,
        commit: &str,
        at_secs: u64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let key = ensure_change_key(project, change)
                    .or_else(|| find_key_by_commit(project, commit));
                let Some(record) = key.and_then(|key| project.fleet_ledger.issues.get_mut(&key))
                else {
                    return (false, false);
                };
                if record.post_land_green_at.is_some()
                    && record.landed_commit.as_deref() == Some(commit)
                {
                    return (false, false);
                }
                record.landed_commit = Some(commit.to_owned());
                record.post_land_green_at = Some(at_secs);
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Records a post-land failure or revert against `change` or `commit`, deduplicated by `event_seq`.
    pub fn record_fleet_failure_or_revert(
        &mut self,
        project: u32,
        change: Option<&str>,
        commit: Option<&str>,
        event_seq: u64,
        is_revert: bool,
        at_secs: u64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let key = change
                    .and_then(|id| ensure_change_key(project, id))
                    .or_else(|| commit.and_then(|sha| find_key_by_commit(project, sha)));
                let Some(record) = key.and_then(|key| project.fleet_ledger.issues.get_mut(&key))
                else {
                    return (false, false);
                };
                if event_seq > 0 && record.seen_failure_events.contains(&event_seq) {
                    return (false, false);
                }
                if event_seq > 0 {
                    record.seen_failure_events.push(event_seq);
                }
                record.failed_or_reverted_at = Some(at_secs);
                if is_revert {
                    record.reverts = record.reverts.saturating_add(1);
                } else {
                    record.post_land_failures = record.post_land_failures.saturating_add(1);
                }
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Records when the issue for `change` was closed.
    pub fn record_fleet_closed(
        &mut self,
        project: u32,
        change: &str,
        issue: Option<(&str, u32)>,
        at_secs: u64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let key = ensure_change_key(project, change)
                    .or_else(|| issue.map(|(repository, number)| issue_key(repository, number)));
                let Some(record) = key.and_then(|key| project.fleet_ledger.issues.get_mut(&key))
                else {
                    return (false, false);
                };
                if record.closed_at.is_some() {
                    return (false, false);
                }
                record.closed_at = Some(at_secs);
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Adds one completed turn's tokens, wall time and turn count to the thread's issue record.
    pub fn record_fleet_turn(
        &mut self,
        project: u32,
        thread_id: u32,
        tokens_in: u64,
        tokens_out: u64,
        wall_ms: u64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let provider_model = project
                    .threads
                    .iter()
                    .find(|item| item.id == thread_id)
                    .map(|item| (item.provider.key(), item.model_name.clone()));
                let Some(record) = ensure_thread_record(project, thread_id) else {
                    return (false, false);
                };
                record.tokens_in = record.tokens_in.saturating_add(tokens_in);
                record.tokens_out = record.tokens_out.saturating_add(tokens_out);
                record.agent_wall_ms = record.agent_wall_ms.saturating_add(wall_ms);
                record.turns = record.turns.saturating_add(1);
                if let Some((provider, model)) = provider_model {
                    // Only backfill missing metadata; do not overwrite the provider or model
                    // already recorded by dispatch with a thread's default fields (#631).
                    if record.provider.is_none() && !provider.is_empty() {
                        record.provider = Some(provider);
                    }
                    if record.model.is_none() && !model.trim().is_empty() {
                        record.model = Some(model);
                    }
                }
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Records a finished queued check (`queue_run` or `preflight`) on the thread's issue record.
    pub fn record_fleet_check(
        &mut self,
        project: u32,
        thread_id: u32,
        check: FleetCheckRecord,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let Some(record) = ensure_thread_record(project, thread_id) else {
                    return (false, false);
                };
                let changed = record.record_check(check);
                (changed, changed)
            })?
            .unwrap_or(false))
    }

    /// Increments the agent restart counter on the thread's issue record.
    pub fn record_fleet_restart(
        &mut self,
        project: u32,
        thread_id: u32,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let Some(record) = ensure_thread_record(project, thread_id) else {
                    return (false, false);
                };
                record.restarts = record.restarts.saturating_add(1);
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Records a review finding filed against a landed change, deduplicated by `target`.
    pub fn record_fleet_review_finding(
        &mut self,
        project: u32,
        change: &str,
        target: &str,
    ) -> Result<bool, StoreError> {
        if target.trim().is_empty() {
            return Ok(false);
        }
        Ok(self
            .edit_project_in_place(project, |project| {
                let Some(record) = ensure_change_record(project, change) else {
                    return (false, false);
                };
                if record.seen_review_targets.iter().any(|seen| seen == target) {
                    return (false, false);
                }
                record.seen_review_targets.push(target.to_owned());
                record.review_findings = record.review_findings.saturating_add(1);
                (true, true)
            })?
            .unwrap_or(false))
    }

    /// Persists idle slot-seconds across `[start_secs, end_secs)` for `free_slots`.
    pub fn record_fleet_idle_interval(
        &mut self,
        project: u32,
        start_secs: u64,
        end_secs: u64,
        free_slots: u32,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let changed = project
                    .fleet_ledger
                    .record_idle_interval(start_secs, end_secs, free_slots);
                (changed, changed)
            })?
            .unwrap_or(false))
    }
}

fn ensure_thread_record(
    project: &mut crate::ProjectItem,
    thread_id: u32,
) -> Option<&mut FleetIssueRecord> {
    let thread_info = project
        .threads
        .iter()
        .find(|item| item.id == thread_id)
        .and_then(|thread| {
            let issue = thread.issue.as_ref()?;
            Some((
                issue.repository.clone(),
                issue.number,
                issue.title.clone().or_else(|| Some(thread.title.clone())),
                thread.rosen_change.clone(),
                thread.provider.key(),
                thread.model_name.clone(),
            ))
        });
    let Some((repository, number, title, change, provider, model)) = thread_info else {
        return project.fleet_ledger.find_by_thread_mut(thread_id);
    };
    let key = issue_key(&repository, number);
    let record = project.fleet_ledger.issues.entry(key).or_insert_with(|| {
        let mut record = FleetIssueRecord::new(repository, number);
        record.dispatched_at = Some(unix_now());
        record.title = title;
        record.provider = (!provider.is_empty()).then_some(provider);
        record.model = (!model.trim().is_empty()).then_some(model);
        record
    });
    record.thread_id = Some(thread_id);
    if record.change.is_none() && change.is_some() {
        record.change = change;
    }
    Some(record)
}

fn find_key_by_commit(project: &crate::ProjectItem, commit: &str) -> Option<String> {
    project
        .fleet_ledger
        .issues
        .iter()
        .find(|(_, record)| record.landed_commit.as_deref() == Some(commit))
        .map(|(key, _)| key.clone())
}

fn ensure_change_key(project: &mut crate::ProjectItem, change: &str) -> Option<String> {
    if let Some(key) = project
        .fleet_ledger
        .issues
        .iter()
        .find(|(_, record)| record.change.as_deref() == Some(change))
        .map(|(key, _)| key.clone())
    {
        return Some(key);
    }
    let thread_info: Option<(u32, String, u32, Option<String>, String, String)> = project
        .threads
        .iter()
        .find(|item| item.rosen_change.as_deref() == Some(change))
        .and_then(|thread: &ThreadItem| {
            let issue = thread.issue.as_ref()?;
            Some((
                thread.id,
                issue.repository.clone(),
                issue.number,
                issue.title.clone().or_else(|| Some(thread.title.clone())),
                thread.provider.key(),
                thread.model_name.clone(),
            ))
        });
    if let Some((thread_id, repository, number, title, provider, model)) = thread_info {
        let key = issue_key(&repository, number);
        let record = project
            .fleet_ledger
            .issues
            .entry(key.clone())
            .or_insert_with(|| {
                let mut record = FleetIssueRecord::new(repository, number);
                record.title = title;
                record.provider = (!provider.is_empty()).then_some(provider);
                record.model = (!model.trim().is_empty()).then_some(model);
                record
            });
        record.thread_id = Some(thread_id);
        record.change = Some(change.to_owned());
        return Some(key);
    }
    let retained = project.retained_rosen_issues.get(change)?;
    let key = issue_key(&retained.issue.repository, retained.issue.number);
    let record = project
        .fleet_ledger
        .issues
        .entry(key.clone())
        .or_insert_with(|| {
            let mut record =
                FleetIssueRecord::new(retained.issue.repository.clone(), retained.issue.number);
            record.title = retained.issue.title.clone();
            record
        });
    record.change = Some(change.to_owned());
    Some(key)
}

fn ensure_change_record<'a>(
    project: &'a mut crate::ProjectItem,
    change: &str,
) -> Option<&'a mut FleetIssueRecord> {
    let key = ensure_change_key(project, change)?;
    project.fleet_ledger.issues.get_mut(&key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PersistedState, ProjectItem, ThreadItem};
    use velocity_core::fleet_ledger::{HOUR_SECONDS, build_window_summary};

    #[test]
    fn fleet_ledger_records_and_idle_buckets_survive_save_and_load()
    -> Result<(), Box<dyn std::error::Error>> {
        // Risk (#619): fleet ledger issue timings, tokens, checks, retries and hourly idle
        // slot-seconds must survive StateStore save and reload across Velocity restarts.
        let directory = std::env::temp_dir().join(format!(
            "velocity-fleet-ledger-roundtrip-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;

        let mut state = PersistedState::default();
        let mut project = ProjectItem::new(1, "Velocity".into(), ".".into());
        let mut thread = ThreadItem::new(10, "Fleet ledger".into());
        thread.issue = Some(crate::issues::ThreadIssue::new(
            "mESmaC/velocity".into(),
            619,
        ));
        project.threads.push(thread);
        state.projects.push(project);
        std::fs::write(directory.join("state.json"), serde_json::to_vec(&state)?)?;

        let base_hour = 1_795_262_400;
        let mut store = StateStore::load_or_default(&directory)?;
        assert!(store.record_fleet_dispatch(
            1,
            FleetDispatchInput {
                repository: "mESmaC/velocity",
                issue: 619,
                title: Some("Fleet ledger"),
                url: Some("https://collapse.sh/mESmaC/velocity/issues/619"),
                filed_at: Some(base_hour - 1_800),
                thread_id: 10,
                provider: "antigravity",
                model: "Gemini 4 Neon",
                now_secs: base_hour + 60,
            },
        )?);
        assert!(store.bind_fleet_change(1, 10, "change-619")?);
        assert!(store.record_fleet_snapshot(1, "change-619", base_hour + 120)?);
        assert!(store.record_fleet_turn(1, 10, 40_000, 10_000, 18_500)?);
        assert!(store.record_fleet_check(
            1,
            10,
            FleetCheckRecord {
                id: "run-1".into(),
                command: "preflight".into(),
                queued_at: base_hour + 600,
                wait_ms: 2_500,
                run_ms: 14_000,
                succeeded: true,
            },
        )?);
        assert!(store.record_fleet_restart(1, 10)?);
        assert!(store.record_fleet_named(1, "change-619", base_hour + 900)?);
        assert!(store.record_fleet_landed(1, "change-619", Some("abc1234"), base_hour + 1_080)?);
        assert!(store.record_fleet_post_land_green(
            1,
            "change-619",
            "abc1234",
            base_hour + 1_380
        )?);
        assert!(store.record_fleet_review_finding(1, "change-619", "land:abc1234")?);
        assert!(store.record_fleet_closed(
            1,
            "change-619",
            Some(("mESmaC/velocity", 619)),
            base_hour + 1_500,
        )?);
        assert!(store.record_fleet_idle_interval(1, base_hour, base_hour + 900, 2)?);

        let reloaded = StateStore::load_or_default(&directory)?;
        let ledger = reloaded
            .fleet_ledger(1)
            .ok_or("missing project after reload")?;
        let record = ledger
            .issues
            .get(&issue_key("mESmaC/velocity", 619))
            .ok_or("missing fleet issue record after reload")?;
        assert_eq!(record.repository, "mESmaC/velocity");
        assert_eq!(record.issue, 619);
        assert_eq!(record.filed_at, Some(base_hour - 1_800));
        assert_eq!(record.dispatched_at, Some(base_hour + 60));
        assert_eq!(record.first_snapshot_at, Some(base_hour + 120));
        assert_eq!(record.named_at, Some(base_hour + 900));
        assert_eq!(record.landed_at, Some(base_hour + 1_080));
        assert_eq!(record.post_land_green_at, Some(base_hour + 1_380));
        assert_eq!(record.closed_at, Some(base_hour + 1_500));
        assert_eq!(record.provider.as_deref(), Some("antigravity"));
        assert_eq!(record.model.as_deref(), Some("Gemini 4 Neon"));
        assert_eq!(record.tokens_in, 40_000);
        assert_eq!(record.tokens_out, 10_000);
        assert_eq!(record.agent_wall_ms, 18_500);
        assert_eq!(record.turns, 1);
        assert_eq!(record.checks.len(), 1);
        assert_eq!(record.checks[0].wait_ms, 2_500);
        assert_eq!(record.checks[0].run_ms, 14_000);
        assert_eq!(record.restarts, 1);
        assert_eq!(record.review_findings, 1);

        let summary = build_window_summary(&ledger, 24, base_hour + HOUR_SECONDS - 1);
        assert_eq!(summary.lands, 1);
        assert_eq!(summary.median_dispatched_to_landed_secs, Some(1_020));
        assert_eq!(summary.tokens_per_landed_change, Some(50_000));
        assert_eq!(summary.idle_slot_minutes, 30.0);
        let feed_item =
            velocity_core::fleet_ledger::FleetCollapseHourRollup::from(&summary.hours[0]);
        let serialized = serde_json::to_string(&feed_item)?;
        assert!(!serialized.contains("mESmaC"));
        assert!(!serialized.contains("Fleet ledger"));
        assert!(!serialized.contains("preflight"));

        let _ = std::fs::remove_dir_all(&directory);
        Ok(())
    }

    fn write_provider_test_state(
        directory: &std::path::Path,
        provider_count: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut state = PersistedState::default();
        let mut project = ProjectItem::new(1, "Velocity".into(), ".".into());
        for index in 0..provider_count {
            let issue_number = 700 + index as u32;
            let mut thread = ThreadItem::new(100 + index as u32, format!("Issue #{issue_number}"));
            thread.issue = Some(crate::issues::ThreadIssue::new(
                "mESmaC/velocity".into(),
                issue_number,
            ));
            project.threads.push(thread);
        }
        let mut fallback_thread = ThreadItem::new(200, "Backfill".into());
        fallback_thread.provider = crate::ProviderKind::Kimi;
        fallback_thread.model_name = "kimi-k2".into();
        fallback_thread.issue = Some(crate::issues::ThreadIssue::new(
            "mESmaC/velocity".into(),
            800,
        ));
        project.threads.push(fallback_thread);
        project.fleet_ledger.issues.insert(
            issue_key("mESmaC/velocity", 800),
            FleetIssueRecord::new("mESmaC/velocity".into(), 800),
        );
        state.projects.push(project);
        std::fs::write(directory.join("state.json"), serde_json::to_vec(&state)?)?;
        Ok(())
    }

    #[test]
    fn fleet_ledger_preserves_every_provider_across_turns_and_reload()
    -> Result<(), Box<dyn std::error::Error>> {
        // Risk (#631): every provider persisted in a fleet ledger record must survive
        // subsequent turn updates and StateStore reload without being clobbered by
        // ThreadItem's default Codex provider.
        let directory = std::env::temp_dir().join(format!(
            "velocity-fleet-ledger-providers-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory)?;
        let providers = [
            crate::ProviderKind::Claude,
            crate::ProviderKind::Codex,
            crate::ProviderKind::Antigravity,
            crate::ProviderKind::Kimi,
            crate::ProviderKind::Custom(7),
        ];
        write_provider_test_state(&directory, providers.len())?;

        let mut store = StateStore::load_or_default(&directory)?;
        for (index, provider) in providers.iter().enumerate() {
            let issue = 700 + index as u32;
            let thread_id = 100 + index as u32;
            let key = provider.key();
            let model = format!("model-{key}");
            assert!(store.record_fleet_dispatch(
                1,
                FleetDispatchInput {
                    repository: "mESmaC/velocity",
                    issue,
                    title: Some("Provider roundtrip"),
                    url: None,
                    filed_at: None,
                    thread_id,
                    provider: &key,
                    model: &model,
                    now_secs: 1_795_262_400,
                },
            )?);
            assert!(store.record_fleet_turn(1, thread_id, 1_000, 500, 2_000)?);
        }
        assert!(store.record_fleet_turn(1, 200, 500, 250, 1_000)?);

        let reloaded = StateStore::load_or_default(&directory)?;
        let ledger = reloaded.fleet_ledger(1).ok_or("missing project")?;
        for (index, provider) in providers.iter().enumerate() {
            let key = issue_key("mESmaC/velocity", 700 + index as u32);
            let record = ledger.issues.get(&key).ok_or("missing provider record")?;
            let expected = provider.key();
            assert_eq!(record.provider.as_deref(), Some(expected.as_str()));
            assert_eq!(
                record.model.as_deref(),
                Some(format!("model-{expected}").as_str())
            );
        }
        let backfill = ledger
            .issues
            .get(&issue_key("mESmaC/velocity", 800))
            .ok_or("missing backfill record")?;
        assert_eq!(backfill.provider.as_deref(), Some("kimi"));
        assert_eq!(backfill.model.as_deref(), Some("kimi-k2"));
        let _ = std::fs::remove_dir_all(&directory);
        Ok(())
    }
}
