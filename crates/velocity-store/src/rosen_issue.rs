//! Durable GitHub effects of Rosen events (F5, #386). The independent event mark lets
//! this save precede the thread lifecycle save without losing either on a crash.
use serde::{Deserialize, Serialize};

use crate::{StateStore, StoreError, ThreadState, issues::ThreadIssue};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueProgress {
    #[serde(default)]
    sequence: u64,
    #[serde(default)]
    landed: Option<String>,
    #[serde(default, alias = "verified")]
    close_queued: bool,
    #[serde(default)]
    green_since: Option<u64>,
    #[serde(default)]
    last_reverted: Option<String>,
    #[serde(default)]
    pub pending: Vec<IssueAction>,
}

/// Issue follow-up state kept on the project after a landed thread is deleted so
/// green builds and reverts still close or reopen its issue (#570).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedIssueProgress {
    pub issue: ThreadIssue,
    #[serde(default = "default_close_enabled")]
    pub close_enabled: bool,
    #[serde(default)]
    pub progress: IssueProgress,
}

fn default_close_enabled() -> bool {
    true
}

impl RetainedIssueProgress {
    /// Whether every pending action has finished and no unverified land remains.
    pub fn is_done(&self) -> bool {
        if !self.progress.pending.is_empty() {
            return false;
        }
        match self.progress.landed.as_deref() {
            None => self.progress.was_reverted(),
            Some(_) => {
                self.progress.is_integration_green()
                    && (self.progress.close_queued || !self.close_enabled)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueAction {
    #[serde(default)]
    pub issue: Option<ThreadIssue>,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub sequence: u64,
    /// Some reopens with the revert's evidence. Older pending actions default to a close.
    #[serde(default)]
    pub revert: Option<String>,
    #[serde(default)]
    pub land_comment: bool,
}

impl IssueAction {
    fn new(issue: &ThreadIssue, commit: &str, sequence: u64) -> Self {
        Self {
            issue: Some(issue.clone()),
            commit: commit.into(),
            sequence,
            revert: None,
            land_comment: false,
        }
    }

    pub fn is_close(&self) -> bool {
        !self.land_comment && self.revert.is_none()
    }
}

pub enum IssueObservation<'a> {
    Landed(&'a str),
    BuildPassed(&'a str),
    Reverted {
        commit: Option<&'a str>,
        reason: &'a str,
    },
}

impl IssueProgress {
    pub(crate) fn was_reverted(&self) -> bool {
        self.last_reverted.is_some()
    }

    pub(crate) fn rewind_events(&mut self, replacing: bool, latest: u64) {
        if replacing || self.sequence > latest {
            self.sequence = 0;
        }
    }

    fn observe(
        &mut self,
        sequence: u64,
        issue: &ThreadIssue,
        event: IssueObservation<'_>,
        close_enabled: bool,
    ) {
        if sequence <= self.sequence {
            return;
        }
        self.sequence = sequence;
        let (commit, revert) = match event {
            IssueObservation::Landed(commit) => {
                if self.last_reverted.as_deref() == Some(commit) {
                    return;
                }
                if self.landed.as_deref() == Some(commit) {
                    return;
                }
                self.close_queued = close_enabled;
                self.green_since = None;
                self.landed = Some(commit.into());
                let mut action = IssueAction::new(issue, commit, sequence);
                action.land_comment = true;
                self.pending.push(action);
                if close_enabled {
                    self.pending.push(IssueAction::new(issue, commit, sequence));
                }
                return;
            }
            IssueObservation::BuildPassed(commit) => {
                self.confirm_build(issue, commit, close_enabled);
                return;
            }
            IssueObservation::Reverted { commit, reason } => {
                let Some(commit) = commit.or(self.landed.as_deref()) else {
                    return;
                };
                if self.last_reverted.as_deref() == Some(commit)
                    || self
                        .landed
                        .as_deref()
                        .is_some_and(|landed| landed != commit)
                {
                    return;
                }
                let commit = commit.to_owned();
                self.pending.retain(|action| !action.is_close());
                self.last_reverted = Some(commit.clone());
                self.landed = None;
                self.green_since = None;
                if !close_enabled {
                    return;
                }
                (commit, Some(reason.into()))
            }
        };
        let mut action = IssueAction::new(issue, &commit, sequence);
        action.revert = revert;
        self.pending.push(action);
    }

    fn confirm_build(&mut self, issue: &ThreadIssue, commit: &str, close_enabled: bool) {
        if self.landed.as_deref() != Some(commit) {
            return;
        }
        // Legacy pending closes must spend the full review window after upgrading.
        self.green_since.get_or_insert_with(unix_seconds);
        if close_enabled && !self.close_queued {
            self.close_queued = true;
            self.pending
                .push(IssueAction::new(issue, commit, self.sequence));
        }
    }

    /// Whether deleting this thread would otherwise drop pending issue actions or an
    /// unverified land (#570).
    pub(crate) fn needs_retention(&self, state: ThreadState, close_enabled: bool) -> bool {
        if !self.pending.is_empty() {
            return true;
        }
        if self.landed.is_some() {
            return !self.is_integration_green() || (close_enabled && !self.close_queued);
        }
        state == ThreadState::Merged
    }

    fn for_orphaned_green(issue: &ThreadIssue, commit: &str, green_since: u64) -> Self {
        Self {
            sequence: 0,
            landed: Some(commit.to_owned()),
            close_queued: true,
            green_since: Some(green_since),
            last_reverted: None,
            pending: vec![IssueAction::new(issue, commit, 0)],
        }
    }

    pub fn watcher_due(&self) -> bool {
        const REVIEW_WINDOW: u64 = 30 * 60;
        self.green_since
            .is_some_and(|green| unix_seconds().saturating_sub(green) >= REVIEW_WINDOW)
    }

    pub fn landed_commit(&self) -> Option<&str> {
        self.landed.as_deref()
    }

    /// Whether this thread's landed commit has been covered by a green integration build
    /// and has not been reverted (#529).
    pub fn is_integration_green(&self) -> bool {
        self.landed.is_some() && self.green_since.is_some()
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn close_enabled(project: &crate::ProjectItem, thread: u32) -> bool {
    crate::swarms::thread_swarm(project, thread).is_none_or(|(swarm, _)| swarm.close_issues)
}

fn same_issue(left: &ThreadIssue, right: &ThreadIssue) -> bool {
    left.number == right.number && left.repository.eq_ignore_ascii_case(&right.repository)
}

/// Moves pending or unverified issue follow-ups off a deleted thread onto the project (#570).
pub(crate) fn retain_deleted_thread(project: &mut crate::ProjectItem, thread: &crate::ThreadItem) {
    let (Some(issue), Some(change)) = (&thread.issue, &thread.rosen_change) else {
        return;
    };
    let enabled = close_enabled(project, thread.id);
    if !thread.rosen_issue.needs_retention(thread.state, enabled) {
        return;
    }
    project.retained_rosen_issues.insert(
        change.clone(),
        RetainedIssueProgress {
            issue: issue.clone(),
            close_enabled: enabled,
            progress: thread.rosen_issue.clone(),
        },
    );
}

impl StateStore {
    /// Includes already queued closes so their evidence is rechecked before every retry.
    /// Returns `(change, commit)` across live threads and retained deleted-thread records (#570).
    pub fn rosen_issue_build_candidates(&self, project: u32) -> Vec<(String, String)> {
        let Some(project) = self.project(project) else {
            return Vec::new();
        };
        let mut live_changes = std::collections::HashSet::new();
        let mut candidates: Vec<(String, String)> = project
            .threads
            .iter()
            .filter(|thread| {
                thread.issue.is_some()
                    && thread.rosen_change.is_some()
                    && close_enabled(project, thread.id)
            })
            .filter(|thread| {
                !thread.rosen_issue.close_queued
                    || thread.rosen_issue.pending.iter().any(IssueAction::is_close)
            })
            .filter_map(|thread| {
                let change = thread.rosen_change.clone()?;
                let commit = thread.rosen_issue.landed.clone()?;
                live_changes.insert(change.clone());
                Some((change, commit))
            })
            .collect();
        for (change, retained) in &project.retained_rosen_issues {
            if live_changes.contains(change) {
                continue;
            }
            let needs_build = if retained.close_enabled {
                !retained.progress.close_queued
                    || retained.progress.pending.iter().any(IssueAction::is_close)
            } else {
                !retained.progress.is_integration_green()
            };
            if needs_build && let Some(commit) = &retained.progress.landed {
                candidates.push((change.clone(), commit.clone()));
            }
        }
        candidates
    }

    /// Build-prefix observations carry no event sequence; preserve the land/revert cursor.
    pub fn confirm_rosen_issue_build(
        &mut self,
        project: u32,
        change: &str,
        commit: &str,
    ) -> Result<(), StoreError> {
        self.edit_project_in_place(project, |project| {
            if let Some(index) = project
                .threads
                .iter()
                .position(|item| item.rosen_change.as_deref() == Some(change))
            {
                let enabled = close_enabled(project, project.threads[index].id);
                let item = &mut project.threads[index];
                let Some(issue) = &item.issue else {
                    return (false, ());
                };
                let before = item.rosen_issue.clone();
                item.rosen_issue.confirm_build(issue, commit, enabled);
                return (item.rosen_issue != before, ());
            }
            let Some(retained) = project.retained_rosen_issues.get_mut(change) else {
                return (false, ());
            };
            let before = retained.progress.clone();
            retained
                .progress
                .confirm_build(&retained.issue, commit, retained.close_enabled);
            let mut changed = retained.progress != before;
            if retained.is_done() {
                project.retained_rosen_issues.remove(change);
                changed = true;
            }
            (changed, ())
        })?;
        Ok(())
    }

    pub fn observe_rosen_issue(
        &mut self,
        project: u32,
        change: &str,
        sequence: u64,
        event: IssueObservation<'_>,
    ) -> Result<(), StoreError> {
        self.edit_project_in_place(project, |project| {
            let observed = project
                .handoff_items
                .get_mut(change)
                .is_some_and(|item| item.observe_land(sequence, &event));
            if let Some(index) = project
                .threads
                .iter()
                .position(|thread| thread.rosen_change.as_deref() == Some(change))
            {
                let close_enabled = close_enabled(project, project.threads[index].id);
                let thread = &mut project.threads[index];
                let Some(issue) = thread
                    .issue
                    .as_ref()
                    .filter(|_| sequence > thread.rosen_issue.sequence)
                else {
                    return (observed, ());
                };
                // An upgrade can first hear the green event for a land the older app already
                // acknowledged. R12's per-change green event carries that exact landed SHA.
                if thread.rosen_issue.sequence == 0
                    && thread.state == ThreadState::Merged
                    && let IssueObservation::BuildPassed(commit) = &event
                {
                    thread.rosen_issue.landed = Some((*commit).into());
                }
                thread
                    .rosen_issue
                    .observe(sequence, issue, event, close_enabled);
                return (true, ());
            }
            let Some(retained) = project.retained_rosen_issues.get_mut(change) else {
                return (observed, ());
            };
            if sequence <= retained.progress.sequence {
                return (observed, ());
            }
            if retained.progress.sequence == 0
                && let IssueObservation::BuildPassed(commit) = &event
            {
                retained.progress.landed = Some((*commit).into());
            }
            retained
                .progress
                .observe(sequence, &retained.issue, event, retained.close_enabled);
            if retained.is_done() {
                project.retained_rosen_issues.remove(change);
            }
            (true, ())
        })?;
        Ok(())
    }

    pub fn finish_rosen_issue(
        &mut self,
        project: u32,
        change: &str,
        action: &IssueAction,
    ) -> Result<(), StoreError> {
        self.edit_project_in_place(project, |project| {
            if let Some(thread) = project
                .threads
                .iter_mut()
                .find(|item| item.rosen_change.as_deref() == Some(change))
            {
                let before = thread.rosen_issue.pending.len();
                thread
                    .rosen_issue
                    .pending
                    .retain(|pending| pending != action);
                return (thread.rosen_issue.pending.len() != before, ());
            }
            let Some(retained) = project.retained_rosen_issues.get_mut(change) else {
                return (false, ());
            };
            let before = retained.progress.pending.len();
            retained
                .progress
                .pending
                .retain(|pending| pending != action);
            let mut changed = retained.progress.pending.len() != before;
            if retained.is_done() {
                project.retained_rosen_issues.remove(change);
                changed = true;
            }
            (changed, ())
        })?;
        Ok(())
    }

    /// Adopts an orphaned landed `handoff_items` entry into `retained_rosen_issues` on
    /// startup when no live thread or retained record owns it (#570).
    pub fn adopt_orphaned_handoff_issue(
        &mut self,
        project: u32,
        change: String,
        issue: ThreadIssue,
        commit: String,
        green_since: u64,
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |project| {
                let landed_matches = project
                    .handoff_items
                    .get(&change)
                    .and_then(|item| item.landed.as_deref())
                    == Some(commit.as_str());
                let owned_by_retained = project.retained_rosen_issues.contains_key(&change)
                    || project
                        .retained_rosen_issues
                        .values()
                        .any(|retained| same_issue(&retained.issue, &issue));
                let owned_by_thread = project.threads.iter().any(|thread| {
                    thread.rosen_change.as_deref() == Some(change.as_str())
                        || thread.issue.as_ref().is_some_and(|assigned| {
                            same_issue(assigned, &issue)
                                && (thread.rosen_issue.landed_commit() == Some(commit.as_str())
                                    || matches!(
                                        thread.state,
                                        ThreadState::Working | ThreadState::Review
                                    ))
                        })
                });
                if !landed_matches || owned_by_retained || owned_by_thread {
                    return (false, false);
                }
                let progress = IssueProgress::for_orphaned_green(&issue, &commit, green_since);
                project.retained_rosen_issues.insert(
                    change,
                    RetainedIssueProgress {
                        issue,
                        close_enabled: true,
                        progress,
                    },
                );
                (true, true)
            })?
            .unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use IssueObservation::{BuildPassed, Landed, Reverted};
    use std::path::Path;

    // Risk (#386): a restart loses an issue close/reopen, or replay closes an issue whose
    // culprit was reverted. Saved effects and their replay marks must survive together.
    #[test]
    fn issue_follow_ups_survive_save_load_and_replay() -> Result<(), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "velocity-386-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let mut store = seed(&directory)?;
        let first = "a".repeat(40);
        let second = "b".repeat(40);
        record(&mut store, 1, Landed(&first))?;
        record(&mut store, 2, BuildPassed(&second))?;
        let landed = pending(&store);
        assert_eq!(landed.len(), 2);
        assert!(landed[0].land_comment);
        store.finish_rosen_issue(1, "change", &landed[0])?;
        assert!(
            store
                .project(1)
                .is_some_and(|project| !project.threads[0].rosen_issue.is_integration_green())
        );
        store.confirm_rosen_issue_build(1, "change", &first)?;
        store = StateStore::load_or_default(&directory)?;
        assert!(
            store
                .project(1)
                .is_some_and(|project| project.threads[0].rosen_issue.is_integration_green())
        );
        let close = pending(&store);
        assert_eq!(close.len(), 1);
        assert_eq!(close[0].commit, first);
        assert_eq!(
            close[0].issue,
            Some(ThreadIssue::new("example/smoke".into(), 386))
        );
        store.confirm_rosen_issue_build(1, "change", &first)?;
        assert_eq!(pending(&store), close);
        assert!(
            store
                .project(1)
                .is_some_and(|project| !project.threads[0].rosen_issue.watcher_due())
        );
        store.finish_rosen_issue(1, "change", &close[0])?;
        record(
            &mut store,
            5,
            Reverted {
                commit: Some(&first),
                reason: "build failed",
            },
        )?;
        store = StateStore::load_or_default(&directory)?;
        assert!(
            store
                .project(1)
                .is_some_and(|project| !project.threads[0].rosen_issue.is_integration_green())
        );
        let reopened = pending(&store);
        assert_eq!(reopened.len(), 1);
        assert_eq!(reopened[0].revert.as_deref(), Some("build failed"));
        record(&mut store, 6, BuildPassed(&first))?;
        assert_eq!(pending(&store), reopened);
        record(&mut store, 7, Landed(&second))?;
        record(
            &mut store,
            8,
            Reverted {
                commit: Some(&first),
                reason: "duplicate",
            },
        )?;
        store.confirm_rosen_issue_build(1, "change", &second)?;
        store = StateStore::load_or_default(&directory)?;
        let completed = pending(&store);
        assert_eq!(completed.len(), 3);
        assert_eq!(completed[0], reopened[0]);
        assert_eq!(completed[1].commit, second);
        assert!(completed[1].land_comment);
        assert_eq!(completed[2].commit, second);
        assert!(completed[2].is_close());
        assert!(directory.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#570): deleting a landed thread before post-land verification must keep its
    // pending issue close/reopen alive on the project across restarts until completed.
    #[test]
    fn deleted_landed_thread_retains_issue_follow_ups_through_green_and_revert()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "velocity-570-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let mut store = seed(&directory)?;
        let first = "a".repeat(40);
        let second = "b".repeat(40);
        record(&mut store, 1, Landed(&first))?;
        let landed = pending(&store);
        store.finish_rosen_issue(1, "change", &landed[0])?;

        store.remove_thread(1, 4)?;
        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store.rosen_issue_build_candidates(1),
            vec![("change".to_owned(), first.clone())]
        );
        let retained = store
            .project(1)
            .and_then(|project| project.retained_rosen_issues.get("change"))
            .ok_or("retained record missing")?;
        assert_eq!(retained.progress.pending.len(), 1);
        assert!(retained.progress.pending[0].is_close());

        store.confirm_rosen_issue_build(1, "change", &first)?;
        let mut store = StateStore::load_or_default(&directory)?;
        let close = store
            .project(1)
            .and_then(|project| project.retained_rosen_issues.get("change"))
            .ok_or("retained record missing after green")?
            .progress
            .pending[0]
            .clone();
        assert!(store.project(1).is_some_and(|project| {
            project.retained_rosen_issues["change"]
                .progress
                .is_integration_green()
        }));
        store.finish_rosen_issue(1, "change", &close)?;
        let store = StateStore::load_or_default(&directory)?;
        assert!(
            store
                .project(1)
                .is_some_and(|project| project.retained_rosen_issues.is_empty())
        );

        // A revert after thread deletion queues a reopen and prunes once finished.
        let mut store = seed(&directory)?;
        record(&mut store, 1, Landed(&second))?;
        let landed = pending(&store);
        store.finish_rosen_issue(1, "change", &landed[0])?;
        store.remove_thread(1, 4)?;
        record(
            &mut store,
            2,
            Reverted {
                commit: Some(&second),
                reason: "build broke",
            },
        )?;
        let mut store = StateStore::load_or_default(&directory)?;
        let reopen = store
            .project(1)
            .and_then(|project| project.retained_rosen_issues.get("change"))
            .ok_or("retained reopen missing")?
            .progress
            .pending[0]
            .clone();
        assert_eq!(reopen.revert.as_deref(), Some("build broke"));
        store.finish_rosen_issue(1, "change", &reopen)?;
        assert!(
            store
                .project(1)
                .is_some_and(|project| project.retained_rosen_issues.is_empty())
        );

        // Adopting an orphaned landed handoff persists and rejects duplicate ownership.
        let mut store = seed(&directory)?;
        store.enroll_handoff(1, 4)?;
        record(&mut store, 1, Landed(&first))?;
        store.finish_rosen_issue(1, "change", &pending(&store)[0])?;
        store.confirm_rosen_issue_build(1, "change", &first)?;
        store.finish_rosen_issue(1, "change", &pending(&store)[0])?;
        store.remove_thread(1, 4)?;
        assert!(
            store
                .project(1)
                .is_some_and(|project| project.retained_rosen_issues.is_empty())
        );
        let issue = ThreadIssue::new("example/smoke".into(), 386);
        assert!(store.adopt_orphaned_handoff_issue(
            1,
            "change".into(),
            issue.clone(),
            first.clone(),
            100,
        )?);
        assert!(!store.adopt_orphaned_handoff_issue(
            1,
            "change".into(),
            issue,
            first.clone(),
            100,
        )?);
        let store = StateStore::load_or_default(&directory)?;
        let adopted = store
            .project(1)
            .and_then(|project| project.retained_rosen_issues.get("change"))
            .ok_or("adopted record missing")?;
        assert!(adopted.progress.watcher_due());
        assert_eq!(adopted.progress.pending.len(), 1);
        assert!(adopted.progress.pending[0].is_close());
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    fn seed(directory: &Path) -> Result<StateStore, StoreError> {
        let mut store = StateStore::load_or_default(directory)?;
        let mut project =
            crate::ProjectItem::new(1, "Smoke".into(), r"C:\project with spaces".into());
        let mut thread = crate::ThreadItem::new(4, "Issue".into());
        thread.rosen_change = Some("change".into());
        thread.issue = Some(ThreadIssue::new("example/smoke".into(), 386));
        project.threads.push(thread);
        let mut next = store.state.clone();
        next.projects = vec![project];
        store.store_snapshot(next)?;
        Ok(store)
    }

    fn record(
        store: &mut StateStore,
        sequence: u64,
        event: IssueObservation<'_>,
    ) -> Result<(), StoreError> {
        store.observe_rosen_issue(1, "change", sequence, event)
    }

    fn pending(store: &StateStore) -> Vec<IssueAction> {
        store.project(1).map_or_else(Vec::new, |project| {
            project.threads[0].rosen_issue.pending.clone()
        })
    }
}
