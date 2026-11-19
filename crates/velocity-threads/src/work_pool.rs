//! Read-only F16 work inventory from the existing project, GitHub and spec sources.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};
use thiserror::Error;
use velocity_core::work_pool::{
    WorkItem, WorkKey, WorkPiece, WorkPool, WorkSources, spec_files_pieces,
};
use velocity_github::{GitHubIssue, IssueClient};
use velocity_spec::ProjectSpec;
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::{ProjectItem, TaskState, ThreadState};

#[derive(Debug, Error)]
pub enum WorkPoolError {
    #[error("Could not read the project spec (SPEC.md and spec/): {0}")]
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
    if IssueClient::has_origin(checkout)? {
        let (repository, mut issues) = open_issues(checkout, issue_age)?;
        issues.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.number.cmp(&right.number))
        });
        // Unlabelled owner questions can also leave an agent holding a claim with
        // nothing to build (#389). Use the dispatch filter so both sources of work
        // wait for the owner's answer, including before a question is labelled.
        for issue in issues
            .iter()
            .filter(|issue| crate::continuous_dispatch::actionable(issue))
        {
            let assignment = ThreadIssue::new(repository.clone(), issue.number);
            if project.rosen_issue_completed(&assignment) {
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
