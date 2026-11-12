//! Read-only F16 work inventory from the existing project, GitHub and spec sources.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};
use thiserror::Error;
use velocity_core::work_pool::{WorkKey, WorkPiece, WorkPool, spec_pieces};
use velocity_github::{GitHubIssue, IssueClient, IssueState};
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::{ProjectItem, TaskState, ThreadState, extended_windows_path};

#[derive(Debug, Error)]
pub enum WorkPoolError {
    #[error("Could not read SPEC.md: {0}")]
    Spec(#[from] std::io::Error),
    #[error(transparent)]
    GitHub(#[from] velocity_github::PullRequestError),
}

/// How old an open-issue list a display read (the claims board) may show. A swarm's claims,
/// PRs and merges move the board in bursts; without this each move cost a GitHub read, and a
/// busy swarm spent the whole GraphQL budget.
const DISPLAY_ISSUE_AGE: Duration = Duration::from_secs(60);

/// Per checkout: the GitHub client, so its `gh repo view` identity is reused while current
/// (#229), and the last open-issue list with when it was read.
struct CachedIssues {
    client: IssueClient,
    read_at: Instant,
    issues: Vec<GitHubIssue>,
}

static OPEN_ISSUES: LazyLock<Mutex<HashMap<PathBuf, CachedIssues>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// No selection or claim happens here: callers must refresh and atomically claim
/// before starting work. Failure to read a source fails the snapshot, never shows
/// a partial pool as if it were complete. The open issues are always read fresh.
pub fn read_work_pool(project: &ProjectItem) -> Result<WorkPool, WorkPoolError> {
    read_pool(project, Duration::ZERO)
}

/// `read_work_pool` for display only: open issues read in the last minute are reused.
/// Never choose or claim work from this pool.
pub fn read_display_work_pool(project: &ProjectItem) -> Result<WorkPool, WorkPoolError> {
    read_pool(project, DISPLAY_ISSUE_AGE)
}

fn read_pool(project: &ProjectItem, issue_age: Duration) -> Result<WorkPool, WorkPoolError> {
    if !project.spec_locked {
        return Ok(WorkPool::new(project.id, Vec::new()));
    }
    let mut pieces = task_pieces(project);
    let checkout = Path::new(&project.repository_path);
    if IssueClient::has_origin(checkout)? {
        let (repository, issues) = open_issues(checkout, issue_age)?;
        for issue in issues
            .iter()
            .filter(|issue| issue.state == IssueState::Open)
        {
            let assignment = ThreadIssue::new(repository.clone(), issue.number);
            let owner = issue_thread(project, &assignment).map(|thread| thread.id);
            pieces.push(WorkPiece::new(
                WorkKey::Issue {
                    repository: repository.to_lowercase(),
                    number: issue.number,
                },
                issue.title.clone(),
                issue.url.clone(),
                owner,
            ));
        }
    }
    let path = checkout.join(&project.spec_path);
    let markdown = std::fs::read_to_string(extended_windows_path(&path))?;
    pieces.extend(available_spec_pieces(project, &markdown));
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
    Ok(WorkPool::new(project.id, pieces))
}

/// The checkout's repository and its open issues, no older than `issue_age`. Only open issues
/// are read: the pool never shows a closed one, and the closed pages were most of the cost.
fn open_issues(
    checkout: &Path,
    issue_age: Duration,
) -> Result<(String, Vec<GitHubIssue>), WorkPoolError> {
    let key = std::fs::canonicalize(checkout)?;
    // The lock is never held across a `gh` call, so one slow read cannot stall the others.
    let cached = OPEN_ISSUES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&key)
        .filter(|cached| cached.client.is_current(checkout));
    let client = match cached {
        Some(cached) if cached.read_at.elapsed() < issue_age => {
            let found = (cached.client.repository().to_owned(), cached.issues.clone());
            store_issues(key, cached);
            return Ok(found);
        }
        Some(cached) => cached.client,
        None => IssueClient::discover(checkout)?,
    };
    let issues = client.open_issues()?;
    let found = (client.repository().to_owned(), issues.clone());
    store_issues(
        key,
        CachedIssues {
            client,
            read_at: Instant::now(),
            issues,
        },
    );
    Ok(found)
}

fn store_issues(key: PathBuf, cached: CachedIssues) {
    OPEN_ISSUES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, cached);
}

fn task_pieces(project: &ProjectItem) -> Vec<WorkPiece> {
    project
        .tasks
        .iter()
        .filter_map(|task| {
            if !matches!(task.state, TaskState::Todo | TaskState::Now) {
                return None;
            }
            let owner = task
                .assigned_thread_id
                .and_then(|identifier| {
                    project.threads.iter().find(|thread| {
                        !thread.supervision.released
                            && thread.id == identifier
                            && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
                    })
                })
                .map(|thread| thread.id);
            if task.state == TaskState::Now && owner.is_none() {
                return None;
            }
            Some(WorkPiece::new(
                WorkKey::Task {
                    identifier: task.id,
                },
                task.text.clone(),
                "Task List".into(),
                owner,
            ))
        })
        .collect()
}

fn available_spec_pieces(project: &ProjectItem, markdown: &str) -> Vec<WorkPiece> {
    spec_pieces(markdown)
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
