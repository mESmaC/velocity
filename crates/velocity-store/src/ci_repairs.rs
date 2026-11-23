//! Fix-forward ownership for GitHub CI failures on the base, and the base's CI state as
//! last read (F5 "Rosen · Base CI", #507). One open repair per workflow and job, kept
//! until a green run of that workflow, even when its thread is deleted.

use serde::{Deserialize, Serialize};
use velocity_core::base_ci::{
    BaseCi, Failure, ResolvedJob, SupersededRepair, effective_workflow_key, signatures_differ,
    workflow_matches_repair,
};

use crate::threads::{NewThread, ThreadError, insert_thread};
use crate::{StateStore, StoreError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiRepair {
    /// Stable workflow identity key used for grouping and repair matching (#653).
    /// Absent on repairs persisted before #653, which fall back to `workflow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_key: Option<String>,
    /// The workflow's display name, a commit status's context, or `<app> · <check>`.
    #[serde(default)]
    pub workflow: String,
    #[serde(default)]
    pub job: String,
    /// The base commit and run the failure was first seen on.
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub run: u64,
    #[serde(default)]
    pub thread: u32,
    /// Base commit landed by `thread`, once its change lands (#558).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_commit: Option<String>,
    /// Normalized error signature from the failing job's log, when available (#558).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl CiRepair {
    /// Stable workflow identity key, falling back to `workflow` for pre-#653 repairs (#653).
    pub fn effective_workflow_key(&self) -> &str {
        effective_workflow_key(self.workflow_key.as_deref().unwrap_or(""), &self.workflow)
    }
}

impl StateStore {
    /// Allocates the thread, its opening prompt and a repair for each of the failure's jobs
    /// that has no matching open repair, in one save. Retaining them prevents a repeat of
    /// the failure, a retry or a restart from starting a second thread. `None` when covered.
    pub fn add_ci_repair(
        &mut self,
        project: u32,
        failure: &Failure,
        thread: NewThread,
        prompt: &str,
    ) -> Result<Option<u32>, ThreadError> {
        let current = self
            .project(project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let uncovered: Vec<_> = failure
            .jobs
            .iter()
            .filter(|job| {
                !current.ci_repairs.iter().any(|repair| {
                    workflow_matches_repair(
                        repair.workflow_key.as_deref().unwrap_or(""),
                        &repair.workflow,
                        failure.identity(),
                        &failure.workflow,
                    ) && repair.job == job.job
                        && !signatures_differ(repair.signature.as_deref(), job.signature.as_deref())
                })
            })
            .collect();
        if uncovered.is_empty() {
            return Ok(None);
        }
        let mut next = self.state.clone();
        let selected = current.active_thread_id;
        let identifier = insert_thread(&mut next, project, thread, None)?;
        let current = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if let Some(thread) = current
            .threads
            .iter_mut()
            .find(|thread| thread.id == identifier)
        {
            thread.pending_rosen_launch = Some(prompt.into());
        }
        // A background repair never takes the window from the thread being read.
        current.active_thread_id = selected;
        current
            .ci_repairs
            .extend(uncovered.into_iter().map(|job| CiRepair {
                workflow_key: Some(failure.identity().to_owned()),
                workflow: failure.workflow.clone(),
                job: job.job.clone(),
                commit: failure.commit.clone(),
                run: failure.run,
                thread: identifier,
                landed_commit: None,
                signature: job.signature.clone(),
            }));
        self.commit(next)?;
        Ok(Some(identifier))
    }

    /// Records the base commit landed by `thread` on any open CI repairs it owns (#558).
    pub fn record_ci_repair_landed(
        &mut self,
        project: u32,
        thread: u32,
        commit: &str,
    ) -> Result<bool, StoreError> {
        let needs_update = self.project(project).is_some_and(|item| {
            item.ci_repairs.iter().any(|repair| {
                repair.thread == thread && repair.landed_commit.as_deref() != Some(commit)
            })
        });
        if !needs_update {
            return Ok(false);
        }
        let mut next = self.state.clone();
        if let Some(current) = next.projects.iter_mut().find(|item| item.id == project) {
            for repair in &mut current.ci_repairs {
                if repair.thread == thread {
                    repair.landed_commit = Some(commit.to_owned());
                }
            }
        }
        self.store_snapshot(next)?;
        Ok(true)
    }

    /// Drops open repairs superseded by a failure at or after their land or after their
    /// thread finished without landing, and returns the removed records (#558).
    pub fn close_superseded_ci_repairs(
        &mut self,
        project: u32,
        superseded: &[SupersededRepair],
    ) -> Result<Vec<CiRepair>, StoreError> {
        if superseded.is_empty() {
            return Ok(Vec::new());
        }
        let mut next = self.state.clone();
        let Some(current) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(Vec::new());
        };
        let (closed, open): (Vec<_>, Vec<_>) = std::mem::take(&mut current.ci_repairs)
            .into_iter()
            .partition(|repair| {
                superseded.iter().any(|item| {
                    workflow_matches_repair(
                        repair.workflow_key.as_deref().unwrap_or(""),
                        &repair.workflow,
                        item.identity(),
                        &item.workflow,
                    ) && item.job == repair.job
                        && item.thread == repair.thread
                })
            });
        if closed.is_empty() {
            return Ok(closed);
        }
        current.ci_repairs = open;
        self.store_snapshot(next)?;
        Ok(closed)
    }

    /// Drops open repairs whose matching job passed in a newer run (even while the rest of
    /// that run is still in progress), and returns the removed records (#587).
    pub fn close_resolved_ci_repairs(
        &mut self,
        project: u32,
        resolved: &[ResolvedJob],
    ) -> Result<Vec<CiRepair>, StoreError> {
        if resolved.is_empty() {
            return Ok(Vec::new());
        }
        let mut next = self.state.clone();
        let Some(current) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(Vec::new());
        };
        let (closed, open): (Vec<_>, Vec<_>) = std::mem::take(&mut current.ci_repairs)
            .into_iter()
            .partition(|repair| {
                resolved.iter().any(|item| {
                    workflow_matches_repair(
                        repair.workflow_key.as_deref().unwrap_or(""),
                        &repair.workflow,
                        item.identity(),
                        &item.workflow,
                    ) && item.job == repair.job
                        && (item.run == 0 || repair.run == 0 || item.run >= repair.run)
                })
            });
        if closed.is_empty() {
            return Ok(closed);
        }
        current.ci_repairs = open;
        self.store_snapshot(next)?;
        Ok(closed)
    }

    /// Drops the repairs of `workflows`, whose newest run on the base passed, and returns
    /// them. A later failure of the same job is then a new failure with its own thread.
    pub fn close_ci_repairs(
        &mut self,
        project: u32,
        workflows: &[String],
    ) -> Result<Vec<CiRepair>, StoreError> {
        let mut next = self.state.clone();
        let Some(current) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(Vec::new());
        };
        let (closed, open): (Vec<_>, Vec<_>) = std::mem::take(&mut current.ci_repairs)
            .into_iter()
            .partition(|repair| {
                workflows
                    .iter()
                    .any(|workflow| workflow == repair.effective_workflow_key())
            });
        if closed.is_empty() {
            return Ok(closed);
        }
        current.ci_repairs = open;
        self.store_snapshot(next)?;
        Ok(closed)
    }

    /// Saves the base's CI state as last read and reports whether it changed. `None`
    /// clears it, as when the project no longer has a GitHub repository.
    pub fn save_base_ci(&mut self, project: u32, ci: Option<BaseCi>) -> Result<bool, StoreError> {
        let unchanged = self.project(project).is_none_or(|item| item.base_ci == ci);
        if unchanged {
            return Ok(false);
        }
        let mut next = self.state.clone();
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            item.base_ci = ci;
        }
        self.store_snapshot(next)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use velocity_core::base_ci::{BaseCi, BaseCiFailure, BaseCiState, FailedJob, Failure};

    use crate::threads::NewThread;
    use crate::{ProjectItem, StateStore};

    // Risk (#507): a repair lost on reload starts a second thread for a failure that
    // already has one, and one kept after its workflow turned green hides the next failure.
    #[test]
    fn ci_repairs_and_state_survive_reload_until_a_green_run()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-ci-repairs-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;
        let job = |name: &str| FailedJob {
            job: name.into(),
            ..FailedJob::default()
        };
        let mut failure = Failure {
            workflow_key: "Build".into(),
            workflow: "Build".into(),
            run: 7,
            commit: "a".repeat(40),
            url: "https://github.com/owner/one/actions/runs/7".into(),
            detail: String::new(),
            jobs: vec![job("test (macos-latest)")],
        };
        let settings = store.settings();
        let thread = |slug: &str| {
            NewThread::new(
                "Fix CI".into(),
                format!("vel/{slug}"),
                format!(r"C:\worktrees\{slug}"),
                &settings,
            )
        };
        let first = store.add_ci_repair(1, &failure, thread("one"), "prompt")?;
        assert!(first.is_some());
        let failing = BaseCi {
            source: velocity_core::base_ci::BaseCiSource::GitHub,
            state: BaseCiState::Failed,
            commit: Some(failure.commit.clone()),
            failing: vec!["Build · test (macos-latest)".into()],
            failures: vec![BaseCiFailure {
                workflow_key: "Build".into(),
                workflow: "Build".into(),
                job: "test (macos-latest)".into(),
                commit: failure.commit.clone(),
                run: failure.run,
                waiting: false,
            }],
            running: false,
        };
        assert!(store.save_base_ci(1, Some(failing.clone()))?);

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.state.projects[0].base_ci, Some(failing));
        // The same job again is a repeat; a job that newly fails gets its own repair.
        assert_eq!(
            store.add_ci_repair(1, &failure, thread("two"), "prompt")?,
            None
        );
        failure.jobs.push(job("clippy"));
        let second = store.add_ci_repair(1, &failure, thread("three"), "prompt")?;
        assert!(second.is_some() && second != first);
        assert_eq!(store.state.projects[0].ci_repairs.len(), 2);
        assert!(store.close_ci_repairs(1, &["Docs".into()])?.is_empty());
        assert_eq!(store.close_ci_repairs(1, &["Build".into()])?.len(), 2);

        let mut store = StateStore::load_or_default(&directory)?;
        assert!(store.state.projects[0].ci_repairs.is_empty());
        assert!(
            store
                .add_ci_repair(1, &failure, thread("four"), "prompt")?
                .is_some()
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#558): landed commits and error signatures on CI repairs must survive save and
    // reload so a restart still reopens a landed repair whose fix left the base red.
    #[test]
    fn ci_repair_landed_commit_and_signature_survive_reload()
    -> Result<(), Box<dyn std::error::Error>> {
        use velocity_core::base_ci::SupersededRepair;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-ci-repair-reopen-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;
        let settings = store.settings();
        let make_thread = |slug: &str| {
            NewThread::new(
                "Fix CI".into(),
                format!("vel/{slug}"),
                format!(r"C:\worktrees\{slug}"),
                &settings,
            )
        };
        let failure = Failure {
            workflow_key: "Build and lint".into(),
            workflow: "Build and lint".into(),
            run: 10,
            commit: "40c32e0".repeat(6)[..40].to_owned(),
            url: "https://github.com/owner/one/actions/runs/10".into(),
            detail: String::new(),
            jobs: vec![FailedJob {
                job: "clippy".into(),
                signature: Some(
                    "error: redundant closure | --> crates/velocity-graph/src/lib.rs".into(),
                ),
                ..FailedJob::default()
            }],
        };
        let first = store
            .add_ci_repair(1, &failure, make_thread("first"), "prompt")?
            .ok_or("expected repair thread")?;
        let landed = "4101745".repeat(6)[..40].to_owned();
        assert!(store.record_ci_repair_landed(1, first, &landed)?);
        assert!(!store.record_ci_repair_landed(1, first, &landed)?);

        let mut store = StateStore::load_or_default(&directory)?;
        let saved = &store.state.projects[0].ci_repairs[0];
        assert_eq!(saved.landed_commit.as_deref(), Some(landed.as_str()));
        assert_eq!(
            saved.signature.as_deref(),
            Some("error: redundant closure | --> crates/velocity-graph/src/lib.rs")
        );

        let superseded = [SupersededRepair {
            workflow_key: saved.effective_workflow_key().to_owned(),
            workflow: saved.workflow.clone(),
            job: saved.job.clone(),
            thread: saved.thread,
            commit: saved.commit.clone(),
            landed_commit: saved.landed_commit.clone(),
        }];
        assert_eq!(store.close_superseded_ci_repairs(1, &superseded)?.len(), 1);
        assert!(store.state.projects[0].ci_repairs.is_empty());

        let next_failure = Failure {
            run: 11,
            commit: landed,
            jobs: vec![FailedJob {
                job: "clippy".into(),
                signature: Some(
                    "error[E0063]: missing fields | --> crates/velocity-threads/src/agent_memory.rs"
                        .into(),
                ),
                ..FailedJob::default()
            }],
            ..failure
        };
        let second = store.add_ci_repair(1, &next_failure, make_thread("second"), "prompt")?;
        assert!(second.is_some() && second != Some(first));
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#587): closing a single job whose matching job passed on a newer, still-running
    // workflow run must persist across save and reload without closing unresolved sibling jobs.
    #[test]
    fn resolved_job_closure_persists_across_reload_without_closing_sibling_jobs()
    -> Result<(), Box<dyn std::error::Error>> {
        use velocity_core::base_ci::ResolvedJob;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-ci-repair-resolved-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;
        let settings = store.settings();
        let thread = NewThread::new(
            "Fix CI".into(),
            "vel/fix-ci-100".into(),
            r"C:\worktrees\fix-ci-100".into(),
            &settings,
        );
        let failure = Failure {
            workflow_key: "CI".into(),
            workflow: "CI".into(),
            run: 100,
            commit: "a4d9b88".repeat(6)[..40].to_owned(),
            url: "https://github.com/mESmaC/rosen/actions/runs/100".into(),
            detail: String::new(),
            jobs: vec![
                FailedJob {
                    job: "macOS (build, clippy, test)".into(),
                    timed_out: true,
                    ..FailedJob::default()
                },
                FailedJob {
                    job: "Windows (build, clippy, test)".into(),
                    ..FailedJob::default()
                },
            ],
        };
        assert!(
            store
                .add_ci_repair(1, &failure, thread, "prompt")?
                .is_some()
        );
        let resolved = [ResolvedJob {
            workflow_key: "CI".into(),
            workflow: "CI".into(),
            job: "macOS (build, clippy, test)".into(),
            commit: "33426bb".repeat(6)[..40].to_owned(),
            run: 101,
        }];
        let closed = store.close_resolved_ci_repairs(1, &resolved)?;
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].job, "macOS (build, clippy, test)");

        let store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.state.projects[0].ci_repairs.len(), 1);
        assert_eq!(
            store.state.projects[0].ci_repairs[0].job,
            "Windows (build, clippy, test)"
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#653): two distinct workflows both named `CI` must persist separate repair
    // records keyed by stable workflow identity across save and reload, so closing or
    // resolving one workflow never clears the other workflow's repair.
    #[test]
    fn same_name_workflows_persist_and_close_independently_across_reload()
    -> Result<(), Box<dyn std::error::Error>> {
        use velocity_core::base_ci::ResolvedJob;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-ci-workflow-id-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;
        let settings = store.settings();
        let make_thread = |slug: &str| {
            NewThread::new(
                "Fix CI: CI · test".into(),
                format!("vel/{slug}"),
                format!(r"C:\worktrees\{slug}"),
                &settings,
            )
        };
        let macos = Failure {
            workflow_key: "workflow:11".into(),
            workflow: "CI".into(),
            run: 101,
            commit: "a".repeat(40),
            url: "https://github.com/owner/one/actions/runs/101".into(),
            detail: String::new(),
            jobs: vec![FailedJob {
                job: "test".into(),
                ..FailedJob::default()
            }],
        };
        let docs = Failure {
            workflow_key: "workflow:12".into(),
            workflow: "CI".into(),
            run: 102,
            commit: "b".repeat(40),
            url: "https://github.com/owner/one/actions/runs/102".into(),
            detail: String::new(),
            jobs: vec![FailedJob {
                job: "test".into(),
                ..FailedJob::default()
            }],
        };
        let first = store.add_ci_repair(1, &macos, make_thread("macos"), "prompt")?;
        let second = store.add_ci_repair(1, &docs, make_thread("docs"), "prompt")?;
        assert!(first.is_some() && second.is_some() && first != second);

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.state.projects[0].ci_repairs.len(), 2);
        assert_eq!(
            store.state.projects[0].ci_repairs[0]
                .workflow_key
                .as_deref(),
            Some("workflow:11")
        );
        assert_eq!(store.state.projects[0].ci_repairs[0].workflow, "CI");

        // Resolving or closing docs (`workflow:12`) leaves macOS (`workflow:11`) open.
        let resolved_docs = [ResolvedJob {
            workflow_key: "workflow:12".into(),
            workflow: "CI".into(),
            job: "test".into(),
            commit: "c".repeat(40),
            run: 103,
        }];
        let closed = store.close_resolved_ci_repairs(1, &resolved_docs)?;
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].workflow_key.as_deref(), Some("workflow:12"));
        assert!(
            store
                .close_ci_repairs(1, &["workflow:12".into()])?
                .is_empty()
        );

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.state.projects[0].ci_repairs.len(), 1);
        assert_eq!(
            store.state.projects[0].ci_repairs[0]
                .workflow_key
                .as_deref(),
            Some("workflow:11")
        );
        assert_eq!(store.close_ci_repairs(1, &["workflow:11".into()])?.len(), 1);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
