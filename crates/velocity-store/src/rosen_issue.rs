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
                if close_enabled {
                    self.confirm_build(issue, commit);
                }
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

    fn confirm_build(&mut self, issue: &ThreadIssue, commit: &str) {
        if self.landed.as_deref() != Some(commit) {
            return;
        }
        // Legacy pending closes must spend the full review window after upgrading.
        self.green_since.get_or_insert_with(unix_seconds);
        if !self.close_queued {
            self.close_queued = true;
            self.pending
                .push(IssueAction::new(issue, commit, self.sequence));
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

impl StateStore {
    /// Includes already queued closes so their evidence is rechecked before every retry.
    pub fn rosen_issue_build_candidates(&self, project: u32) -> Vec<(u32, String)> {
        self.project(project).map_or_else(Vec::new, |project| {
            project
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
                    thread
                        .rosen_issue
                        .landed
                        .clone()
                        .map(|commit| (thread.id, commit))
                })
                .collect()
        })
    }

    /// Build-prefix observations carry no event sequence; preserve the land/revert cursor.
    pub fn confirm_rosen_issue_build(
        &mut self,
        project: u32,
        thread: u32,
        commit: &str,
    ) -> Result<(), StoreError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .and_then(|project| project.threads.iter_mut().find(|item| item.id == thread));
        let Some(item) = item else { return Ok(()) };
        let Some(issue) = &item.issue else {
            return Ok(());
        };
        let before = item.rosen_issue.clone();
        item.rosen_issue.confirm_build(issue, commit);
        if item.rosen_issue == before {
            return Ok(());
        }
        self.store_snapshot(next)
    }

    pub fn observe_rosen_issue(
        &mut self,
        project: u32,
        change: &str,
        sequence: u64,
        event: IssueObservation<'_>,
    ) -> Result<(), StoreError> {
        let mut next = self.state.clone();
        let Some(project) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(());
        };
        let observed = project
            .handoff_items
            .get_mut(change)
            .is_some_and(|item| item.observe_land(sequence, &event));
        let index = project
            .threads
            .iter()
            .position(|thread| thread.rosen_change.as_deref() == Some(change));
        let Some(index) = index else {
            return if observed {
                self.store_snapshot(next)
            } else {
                Ok(())
            };
        };
        let close_enabled = close_enabled(project, project.threads[index].id);
        let thread = &mut project.threads[index];
        let Some(issue) = thread
            .issue
            .as_ref()
            .filter(|_| sequence > thread.rosen_issue.sequence)
        else {
            return if observed {
                self.store_snapshot(next)
            } else {
                Ok(())
            };
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
        self.store_snapshot(next)
    }

    pub fn finish_rosen_issue(
        &mut self,
        project: u32,
        thread: u32,
        action: &IssueAction,
    ) -> Result<(), StoreError> {
        let mut next = self.state.clone();
        if let Some(thread) = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .and_then(|project| project.threads.iter_mut().find(|item| item.id == thread))
        {
            thread
                .rosen_issue
                .pending
                .retain(|pending| pending != action);
        }
        self.store_snapshot(next)
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
        store.finish_rosen_issue(1, 4, &landed[0])?;
        store.confirm_rosen_issue_build(1, 4, &first)?;
        store = StateStore::load_or_default(&directory)?;
        let close = pending(&store);
        assert_eq!(close.len(), 1);
        assert_eq!(close[0].commit, first);
        assert_eq!(
            close[0].issue,
            Some(ThreadIssue::new("example/smoke".into(), 386))
        );
        store.confirm_rosen_issue_build(1, 4, &first)?;
        assert_eq!(pending(&store), close);
        assert!(
            store
                .project(1)
                .is_some_and(|project| !project.threads[0].rosen_issue.watcher_due())
        );
        store.finish_rosen_issue(1, 4, &close[0])?;
        record(
            &mut store,
            5,
            Reverted {
                commit: Some(&first),
                reason: "build failed",
            },
        )?;
        store = StateStore::load_or_default(&directory)?;
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
        store.confirm_rosen_issue_build(1, 4, &second)?;
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
