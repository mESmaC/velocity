//! Read-only F16 work inventory from the existing project, GitHub and spec sources.
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;
use velocity_core::work_pool::{
    WorkItem, WorkKey, WorkPiece, WorkPool, WorkSources, spec_files_pieces,
};
use velocity_github::{GitHubIssue, IssueClient, OPEN_ISSUES_REFRESH};
use velocity_spec::ProjectSpec;
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::rosen_work::IssueVersion;
use velocity_store::{ProjectItem, TaskState, ThreadState};

#[derive(Debug, Error)]
pub enum WorkPoolError {
    #[error("Could not read the project spec (SPEC.md and spec/): {0}")]
    Spec(#[from] std::io::Error),
    #[error(transparent)]
    GitHub(#[from] velocity_github::GitHubCommandError),
}

/// How old an open-issue list a display read (the claims board) may show. Shares the
/// workspace-wide open-issues cache with dispatch and the issue panel (#556).
const DISPLAY_ISSUE_AGE: Duration = OPEN_ISSUES_REFRESH;

/// No selection or claim happens here: callers must refresh and atomically claim
/// before starting work. Failure to read a source fails the snapshot, never shows
/// a partial pool as if it were complete. The open issues are always read fresh.
pub fn read_work_pool(project: &ProjectItem) -> Result<WorkPool, WorkPoolError> {
    read_selected_work_pool(project, WorkSources::default())
}

/// Read only the enabled sources for a goal swarm's claims board.
pub fn read_selected_display_work_pool(
    project: &ProjectItem,
    sources: WorkSources,
) -> Result<WorkPool, WorkPoolError> {
    read_pool(project, DISPLAY_ISSUE_AGE, sources, |_| {})
}

/// `read_work_pool` for display only: open issues read in the last minute are reused.
/// Never choose or claim work from this pool.
pub fn read_display_work_pool(project: &ProjectItem) -> Result<WorkPool, WorkPoolError> {
    read_selected_display_work_pool(project, WorkSources::default())
}

pub fn read_selected_work_pool(
    project: &ProjectItem,
    sources: WorkSources,
) -> Result<WorkPool, WorkPoolError> {
    read_pool(project, Duration::ZERO, sources, |_| {})
}

pub(crate) struct GoalInventory {
    pub pool: WorkPool,
    pub issue_files: HashMap<u32, Vec<String>>,
}

/// Selection uses the same inventory as the board, with issue evidence available locally
/// for path matching. The evidence never becomes part of the board or the choosing prompt.
pub(crate) fn read_goal_work_pool(
    project: &ProjectItem,
    sources: WorkSources,
    workspace: &Path,
) -> Result<GoalInventory, WorkPoolError> {
    let mut issue_files = HashMap::new();
    let pool = read_pool(project, Duration::ZERO, sources, |issue| {
        issue_files.insert(
            issue.number,
            crate::goal_selection::referenced_files(workspace, &issue.body),
        );
    })?;
    Ok(GoalInventory { pool, issue_files })
}

fn read_pool(
    project: &ProjectItem,
    issue_age: Duration,
    sources: WorkSources,
    issue_evidence: impl FnMut(&GitHubIssue),
) -> Result<WorkPool, WorkPoolError> {
    if !project.spec_locked {
        return Ok(WorkPool::new(project.id, Vec::new()));
    }
    let mut pieces = if sources.tasks {
        task_pieces(project)
    } else {
        Vec::new()
    };
    let checkout = Path::new(&project.repository_path);
    if sources.issues {
        pieces.extend(issue_pieces(project, checkout, issue_age, issue_evidence)?);
        pieces.extend(held_rosen_issues(project));
    }
    if sources.sections {
        let spec = ProjectSpec::read(checkout, Path::new(&project.spec_path))?;
        pieces.extend(available_spec_pieces(project, &spec));
    }
    for piece in &mut pieces {
        let prior = project.threads.iter().rev().find(|thread| {
            thread.supervision.released
                && thread.supervision.pushed_commit.is_some()
                && velocity_store::claim_piece::thread_claim_piece(thread, &thread.branch_name)
                    == piece.key.claim_piece()
        });
        if let Some(thread) = prior {
            piece.reference.push_str(&format!(
                " - prior work on {} at {} (optional)",
                thread.branch_name,
                thread
                    .supervision
                    .pushed_commit
                    .as_deref()
                    .unwrap_or_default()
            ));
        }
    }
    let mut pool = WorkPool::new(project.id, pieces);
    pool.rosen_items = projected_items(project, sources);
    Ok(pool)
}

fn projected_items(project: &ProjectItem, sources: WorkSources) -> Vec<WorkItem> {
    project
        .rosen_work_items
        .iter()
        .filter_map(|item| {
            if !sources.includes(&item.key) {
                return None;
            }
            let mut item = item.clone();
            if let WorkKey::Task { identifier } = &item.key {
                let task = project.tasks.iter().find(|task| task.id == *identifier)?;
                item.title.clone_from(&task.text);
            }
            if !item.status.completed()
                && item.thread_identifier.is_some_and(|identifier| {
                    !project
                        .threads
                        .iter()
                        .any(|thread| thread.id == identifier && holds_work(thread))
                })
            {
                return None;
            }
            Some(item)
        })
        .collect()
}

fn issue_pieces(
    project: &ProjectItem,
    checkout: &Path,
    issue_age: Duration,
    mut issue_evidence: impl FnMut(&GitHubIssue),
) -> Result<Vec<WorkPiece>, WorkPoolError> {
    let mut pieces = Vec::new();
    if let Some((repository, mut issues)) = open_issues(project, checkout, issue_age)? {
        issues.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.number.cmp(&right.number))
        });
        // Unlabelled owner questions can also leave an agent holding a claim with
        // nothing to build (#389). Use the dispatch filter so both sources of work
        // wait for the owner's answer, including before a question is labelled.
        for issue in issues.iter().filter(|issue| {
            crate::continuous_dispatch::actionable_in_project(project, &repository, issue)
        }) {
            let assignment = ThreadIssue::new(repository.clone(), issue.number);
            if project
                .rosen_issue_completed_for_version(&assignment, IssueVersion::from_github(issue))
            {
                continue;
            }
            let owner = issue_thread(project, &assignment).map(|thread| thread.id);
            issue_evidence(issue);
            pieces.push(WorkPiece::new(
                WorkKey::issue(&repository, issue.number),
                issue.title.clone(),
                issue.url.clone(),
                owner,
            ));
        }
    }
    Ok(pieces)
}

fn held_rosen_issues(project: &ProjectItem) -> Vec<WorkPiece> {
    project
        .threads
        .iter()
        .filter_map(|thread| {
            let issue = thread.issue.as_ref()?;
            if thread.rosen_change.is_none()
                || !holds_work(thread)
                || (thread.goal_member.is_none()
                    && project
                        .rosen_work_item(&WorkKey::issue(&issue.repository, issue.number))
                        .is_none())
            {
                return None;
            }
            // A reverted issue can already be closed in the mirror. Its original thread
            // still owns the fix and must stay visible on the board until it lands again.
            Some(WorkPiece::new(
                WorkKey::issue(&issue.repository, issue.number),
                thread.title.clone(),
                format!("{}#{}", issue.repository, issue.number),
                Some(thread.id),
            ))
        })
        .collect()
}

fn holds_work(thread: &velocity_store::ThreadItem) -> bool {
    !thread.supervision.released && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
}

/// The project's issue repository and its open issues, no older than `issue_age`. Only open
/// issues are read: the pool never shows a closed one, and the closed pages were most of the cost.
fn open_issues(
    project: &ProjectItem,
    checkout: &Path,
    issue_age: Duration,
) -> Result<Option<(String, Vec<GitHubIssue>)>, WorkPoolError> {
    let host = project.effective_issue_host();
    let client = match project.issue_host_repository() {
        Some((host, repository)) => IssueClient::for_host(checkout, host, &repository)?,
        None if host == velocity_github::IssueHost::GitHub
            && checkout.join(".git").exists()
            && IssueClient::has_origin(checkout)? =>
        {
            IssueClient::discover(checkout)?
        }
        None => return Ok(None),
    };
    let issues = client.cached_open_issues(issue_age)?;
    Ok(Some((client.repository().to_owned(), issues)))
}

fn task_pieces(project: &ProjectItem) -> Vec<WorkPiece> {
    project
        .tasks
        .iter()
        .filter_map(|task| {
            if !matches!(task.state, TaskState::Todo | TaskState::Now) {
                return None;
            }
            let key = WorkKey::Task {
                identifier: task.id,
            };
            if project
                .rosen_work_item(&key)
                .is_some_and(|item| item.status.completed())
            {
                return None;
            }
            let owner = task
                .assigned_thread_id
                .and_then(|identifier| {
                    project
                        .threads
                        .iter()
                        .find(|thread| thread.id == identifier && holds_work(thread))
                })
                .map(|thread| thread.id);
            if task.state == TaskState::Now && owner.is_none() {
                return None;
            }
            Some(WorkPiece::new(
                key,
                task.text.clone(),
                "Task List".into(),
                owner,
            ))
        })
        .collect()
}

fn available_spec_pieces(project: &ProjectItem, spec: &ProjectSpec) -> Vec<WorkPiece> {
    let paths: Vec<_> = spec
        .files
        .iter()
        .map(|file| file.path.to_string_lossy().replace('\\', "/"))
        .collect();
    let files: Vec<_> = spec
        .files
        .iter()
        .zip(&paths)
        .map(|(file, path)| (path.as_str(), file.markdown.as_str()))
        .collect();
    spec_files_pieces(&files)
        .into_iter()
        .filter_map(|mut piece| {
            let WorkKey::Spec { section } = &piece.key else {
                return None;
            };
            if project.built_spec_sections.contains(section) {
                return None;
            }
            let owners: Vec<_> = project
                .threads
                .iter()
                .filter(|thread| {
                    !thread.supervision.released && thread.spec_section.as_ref() == Some(section)
                })
                .collect();
            if owners
                .iter()
                .any(|thread| thread.state == ThreadState::Merged)
            {
                return None;
            }
            piece.thread_identifier = owners.first().map(|thread| thread.id);
            Some(piece)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use velocity_core::work_pool::{WorkKey, WorkPiece, WorkSources, WorkStatus};
    use velocity_store::rosen::RosenProgress;
    use velocity_store::rosen_work::WorkEvidence;
    use velocity_store::threads::{NewThread, ThreadError};
    use velocity_store::{PersistedState, ProjectItem, StateStore, TaskState, VersionControl};

    use super::read_selected_work_pool;

    // Risk (#636): after landing a task, deleting its thread, reverting, and relanding the
    // detached change, the Work pool and Goal Swarm claim check must both reject the
    // unclaimed task across save and reload while preserving an independently claimed task.
    #[test]
    fn detached_task_reland_excludes_task_from_work_pool_across_reload()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, mut store, task_1, task_2) = seed_detached_tasks()?;
        let sources = WorkSources {
            tasks: true,
            issues: false,
            sections: false,
        };
        let reopened = read_selected_work_pool(store.project(1).ok_or("project")?, sources)?;
        assert_eq!(reopened.available_tasks, 2);

        let replacement = add_thread(&mut store, "Replacement")?;
        store.claim_goal_piece(1, replacement, &task_2, "vel/task-2".into())?;
        store.save_rosen_work_progress(
            1,
            3,
            "original",
            (RosenProgress::Landed, ""),
            WorkEvidence {
                commit: Some("second"),
                revert_reason: None,
            },
        )?;

        let mut reloaded = StateStore::load_or_default(&directory)?;
        let project = reloaded.project(1).ok_or("reloaded project")?;
        assert_eq!(
            project
                .tasks
                .iter()
                .find(|task| task.id == 1)
                .map(|t| t.state),
            Some(TaskState::Done)
        );
        let pool = read_selected_work_pool(project, sources)?;
        assert_eq!(pool.available_tasks, 0);
        assert!(!pool.pieces.iter().any(|piece| piece.key == task_1.key));
        assert!(pool.pieces.iter().any(|piece| {
            piece.key == task_2.key && piece.thread_identifier == Some(replacement)
        }));

        let claimant = add_thread(&mut reloaded, "Claimant")?;
        assert!(matches!(
            reloaded.claim_goal_piece(1, claimant, &task_1, "vel/task-1".into()),
            Err(ThreadError::TaskUnavailable(1))
        ));

        // Legacy snapshots with a Todo task beside a Landed WorkItem stay out of the pool.
        let mut stale = reloaded.project(1).ok_or("project")?.clone();
        stale.tasks[0].state = TaskState::Todo;
        let stale_pool = read_selected_work_pool(&stale, sources)?;
        assert_eq!(stale_pool.available_tasks, 0);
        assert!(
            !stale_pool
                .pieces
                .iter()
                .any(|piece| piece.key == task_1.key)
        );
        assert_eq!(
            stale.rosen_work_item(&task_1.key).map(|item| item.status),
            Some(WorkStatus::Landed)
        );

        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    fn seed_detached_tasks()
    -> Result<(PathBuf, StateStore, WorkPiece, WorkPiece), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-work-pool-636-{unique}"));
        std::fs::create_dir_all(&directory)?;
        let mut state = PersistedState::default();
        let mut project = ProjectItem::new(1, "Demo".into(), directory.display().to_string());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        state.projects.push(project);
        std::fs::write(directory.join("state.json"), serde_json::to_vec(&state)?)?;

        let mut store = StateStore::load_or_default(&directory)?;
        let first = store.add_task(1, "First task".into())?;
        let second = store.add_task(1, "Second task".into())?;
        let task_1 = WorkPiece::new(
            WorkKey::Task {
                identifier: first.id,
            },
            first.text,
            "Task List".into(),
            None,
        );
        let task_2 = WorkPiece::new(
            WorkKey::Task {
                identifier: second.id,
            },
            second.text,
            "Task List".into(),
            None,
        );
        let owner = add_thread(&mut store, "Original")?;
        store.claim_goal_piece(1, owner, &task_1, "vel/task-1".into())?;
        store.claim_goal_piece(1, owner, &task_2, "vel/task-2".into())?;
        store.update_rosen_work_thread(1, owner, |thread| {
            thread.rosen_change = Some("original".into());
        })?;
        store.save_rosen_work_progress(
            1,
            1,
            "original",
            (RosenProgress::Landed, ""),
            WorkEvidence {
                commit: Some("first"),
                revert_reason: None,
            },
        )?;
        store.remove_thread(1, owner)?;
        store.save_rosen_work_progress(
            1,
            2,
            "original",
            (RosenProgress::Reverted, "check failed"),
            WorkEvidence {
                commit: Some("first"),
                revert_reason: Some("check failed"),
            },
        )?;
        Ok((directory, store, task_1, task_2))
    }

    fn add_thread(store: &mut StateStore, title: &str) -> Result<u32, ThreadError> {
        let thread = NewThread::new(
            title.into(),
            "vel/work".into(),
            r"C:\project\work".into(),
            &store.settings(),
        );
        store.add_thread(1, thread)
    }
}
