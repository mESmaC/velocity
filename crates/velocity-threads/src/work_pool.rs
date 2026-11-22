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
use velocity_store::rosen_work::{IssueVersion, OwnedIssueVersion};
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
    pub issue_versions: HashMap<u32, OwnedIssueVersion>,
}

/// Selection uses the same inventory as the board, with issue evidence available locally
/// for path matching and claim-time version rechecks. The evidence never becomes part of
/// the board or the choosing prompt.
pub(crate) fn read_goal_work_pool(
    project: &ProjectItem,
    sources: WorkSources,
    workspace: &Path,
) -> Result<GoalInventory, WorkPoolError> {
    let mut issue_files = HashMap::new();
    let mut issue_versions = HashMap::new();
    let pool = read_pool(project, Duration::ZERO, sources, |issue| {
        issue_files.insert(
            issue.number,
            crate::goal_selection::referenced_files(workspace, &issue.body),
        );
        issue_versions.insert(issue.number, OwnedIssueVersion::from_github(issue));
    })?;
    Ok(GoalInventory {
        pool,
        issue_files,
        issue_versions,
    })
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
    issue_evidence: impl FnMut(&GitHubIssue),
) -> Result<Vec<WorkPiece>, WorkPoolError> {
    let Some((repository, issues)) = open_issues(project, checkout, issue_age)? else {
        return Ok(Vec::new());
    };
    Ok(issue_pieces_from_open(
        project,
        &repository,
        issues,
        issue_evidence,
    ))
}

fn issue_pieces_from_open(
    project: &ProjectItem,
    repository: &str,
    mut issues: Vec<GitHubIssue>,
    mut issue_evidence: impl FnMut(&GitHubIssue),
) -> Vec<WorkPiece> {
    let mut pieces = Vec::new();
    issues.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.number.cmp(&right.number))
    });
    // Unlabelled owner questions can also leave an agent holding a claim with
    // nothing to build (#389). Use the dispatch filter so both sources of work
    // wait for the owner's answer, including before a question is labelled.
    for issue in issues.iter().filter(|issue| {
        crate::continuous_dispatch::actionable_in_project(project, repository, issue)
    }) {
        let assignment = ThreadIssue::new(repository.to_owned(), issue.number);
        if project.rosen_issue_completed_for_version(&assignment, IssueVersion::from_github(issue))
        {
            continue;
        }
        let owner = issue_thread(project, &assignment).map(|thread| thread.id);
        issue_evidence(issue);
        pieces.push(WorkPiece::new(
            WorkKey::issue(repository, issue.number),
            issue.title.clone(),
            issue.url.clone(),
            owner,
        ));
    }
    pieces
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
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use velocity_core::collapse::IssueHost;
    use velocity_core::goal_swarm_choice::{
        ChoiceDecision, choose_pool_piece, is_piece_unclaimed, open_flags_for_pool,
    };
    use velocity_core::work_pool::{
        WorkItem, WorkKey, WorkPiece, WorkPool, WorkSources, WorkStatus,
    };
    use velocity_github::{GitHubIssue, IssueState};
    use velocity_store::issues::ThreadIssue;
    use velocity_store::rosen::RosenProgress;
    use velocity_store::rosen_work::{IssueVersion, WorkEvidence};
    use velocity_store::threads::{NewThread, ThreadError};
    use velocity_store::{
        PersistedState, ProjectItem, StateStore, TaskAttribution, TaskItem, TaskState, ThreadItem,
        VersionControl,
    };

    use super::{issue_pieces_from_open, read_selected_work_pool};

    const FIXTURE_REPO: &str = "owner/repo";
    const COMPLETED_AT: &str = "2026-01-01T00:00:00Z";
    const AFTER_COMPLETION: &str = "2026-01-02T00:00:00Z";

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
                occurred_at_ms: None,
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
                occurred_at_ms: None,
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
                occurred_at_ms: None,
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

    #[derive(Clone, Copy)]
    struct FixtureSlot {
        is_issue: bool,
        source_open: bool,
        active_owner: bool,
        airc_held: bool,
        projection: Option<WorkStatus>,
        reopened_after: bool,
        updated_after: bool,
    }

    struct SeededSlot {
        spec: FixtureSlot,
        piece: WorkPiece,
        issue: Option<GitHubIssue>,
    }

    struct SeededFixture {
        directory: PathBuf,
        store: StateStore,
        seeded: Vec<SeededSlot>,
        held_keys: BTreeSet<String>,
    }

    fn parse_bit(raw: &str) -> Result<bool, Box<dyn std::error::Error>> {
        match raw {
            "1" => Ok(true),
            "0" => Ok(false),
            other => Err(format!("invalid fixture bit {other}").into()),
        }
    }

    fn parse_slot(raw: &str) -> Result<FixtureSlot, Box<dyn std::error::Error>> {
        let parts: Vec<_> = raw.split(':').collect();
        let &[kind, src, own, held, proj, reop, upd] = parts.as_slice() else {
            return Err(format!("invalid fixture slot {raw}").into());
        };
        let is_issue = match kind {
            "task" => false,
            "issue" => true,
            other => return Err(format!("invalid slot kind {other}").into()),
        };
        let projection = match proj {
            "absent" => None,
            "claimed" => Some(WorkStatus::Claimed),
            "in_change" => Some(WorkStatus::InChange),
            "landed" => Some(WorkStatus::Landed),
            "green" => Some(WorkStatus::Green),
            "reopened" => Some(WorkStatus::Reopened),
            other => return Err(format!("invalid projection {other}").into()),
        };
        Ok(FixtureSlot {
            is_issue,
            source_open: parse_bit(src)?,
            active_owner: parse_bit(own)?,
            airc_held: parse_bit(held)?,
            projection,
            reopened_after: parse_bit(reop)?,
            updated_after: parse_bit(upd)?,
        })
    }

    fn fixture_issue(number: u32, slot: FixtureSlot) -> GitHubIssue {
        GitHubIssue {
            id: format!("issue-{number}"),
            number,
            title: format!("Issue {number}"),
            body: String::new(),
            url: format!("https://example.test/{FIXTURE_REPO}/issues/{number}"),
            state: if slot.source_open {
                IssueState::Open
            } else {
                IssueState::Closed
            },
            author: None,
            created_at: format!("2026-01-01T00:00:{number:02}Z"),
            updated_at: if slot.updated_after {
                AFTER_COMPLETION.into()
            } else {
                COMPLETED_AT.into()
            },
            closed_at: None,
            reopened_at: slot.reopened_after.then(|| AFTER_COMPLETION.into()),
            reopened_by_import: false,
            reopened_by_non_person: false,
            labels: Vec::new(),
            scout: None,
            milestone: None,
            open_blocker_count: 0,
            open_blockers: Vec::new(),
            closed_blockers: Vec::new(),
            host: IssueHost::GitHub,
        }
    }

    fn fixture_work_item(
        key: WorkKey,
        title: String,
        status: WorkStatus,
        owner: Option<u32>,
    ) -> WorkItem {
        let completed = matches!(status, WorkStatus::Landed | WorkStatus::Green);
        WorkItem {
            key,
            title,
            thread_identifier: owner,
            change: Some("change-fixture".into()),
            status,
            claimed_at: Some(COMPLETED_AT.into()),
            updated_at: Some(COMPLETED_AT.into()),
            landed_at: completed.then(|| COMPLETED_AT.into()),
            green_at: (status == WorkStatus::Green).then(|| COMPLETED_AT.into()),
            reverted_at: (status == WorkStatus::Reopened).then(|| COMPLETED_AT.into()),
            landed_commit: completed.then(|| "commit-fixture".into()),
            revert_reason: None,
        }
    }

    fn seed_slots(
        row_index: usize,
        slots: &[FixtureSlot],
    ) -> Result<SeededFixture, Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("velocity-work-pool-637-{unique}-{row_index}"));
        std::fs::create_dir_all(&directory)?;
        let mut state = PersistedState::default();
        let mut project = ProjectItem::new(1, "Demo".into(), directory.display().to_string());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        let mut seeded = Vec::new();
        let mut held_keys = BTreeSet::new();
        for (index, &spec) in slots.iter().enumerate() {
            let slot_num = u32::try_from(index + 1)?;
            let entry = push_slot(&mut project, slot_num, spec);
            if spec.airc_held {
                held_keys.insert(entry.piece.key.claim_piece());
            }
            seeded.push(entry);
        }
        state.projects.push(project);
        std::fs::write(directory.join("state.json"), serde_json::to_vec(&state)?)?;
        let store = StateStore::load_or_default(&directory)?;
        Ok(SeededFixture {
            directory,
            store,
            seeded,
            held_keys,
        })
    }

    fn push_slot(project: &mut ProjectItem, slot_num: u32, spec: FixtureSlot) -> SeededSlot {
        let owner = spec.active_owner.then(|| {
            let thread_id = 100 + slot_num;
            let mut thread = ThreadItem::new(thread_id, format!("Owner {slot_num}"));
            if spec.is_issue {
                thread.issue = Some(ThreadIssue::new(FIXTURE_REPO.into(), slot_num));
            }
            project.threads.push(thread);
            thread_id
        });
        let (key, title, reference, issue) = if spec.is_issue {
            let issue = fixture_issue(slot_num, spec);
            let key = WorkKey::issue(FIXTURE_REPO, slot_num);
            let (title, url) = (issue.title.clone(), issue.url.clone());
            (key, title, url, Some(issue))
        } else {
            let title = format!("Task {slot_num}");
            let key = WorkKey::Task {
                identifier: slot_num,
            };
            project.tasks.push(TaskItem {
                agent_request: None,
                id: slot_num,
                text: title.clone(),
                state: if spec.source_open {
                    TaskState::Todo
                } else {
                    TaskState::Done
                },
                added_by: TaskAttribution::User,
                assigned_thread_id: owner,
                completed_at: None,
            });
            (key, title, "Task List".into(), None)
        };
        if let Some(status) = spec.projection {
            project.rosen_work_items.push(fixture_work_item(
                key.clone(),
                title.clone(),
                status,
                owner,
            ));
        }
        SeededSlot {
            spec,
            piece: WorkPiece::new(key, title, reference, None),
            issue,
        }
    }

    fn production_pool_for_slots(
        project: &ProjectItem,
        seeded: &[SeededSlot],
    ) -> Result<WorkPool, Box<dyn std::error::Error>> {
        let task_pool = read_selected_work_pool(
            project,
            WorkSources {
                tasks: true,
                issues: false,
                sections: false,
            },
        )?;
        let issues: Vec<_> = seeded.iter().filter_map(|s| s.issue.clone()).collect();
        let issue_pieces = issue_pieces_from_open(project, FIXTURE_REPO, issues, |_| {});
        let mut ordered = Vec::new();
        for slot in seeded {
            let found = if slot.spec.is_issue {
                issue_pieces.iter().find(|p| p.key == slot.piece.key)
            } else {
                task_pool.pieces.iter().find(|p| p.key == slot.piece.key)
            };
            if let Some(piece) = found {
                ordered.push(piece.clone());
            }
        }
        Ok(WorkPool::new(project.id, ordered))
    }

    fn slot_claim_result(
        store: &mut StateStore,
        slot: &SeededSlot,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let claimant = add_thread(store, "Claimant")?;
        let version = slot
            .issue
            .as_ref()
            .map(IssueVersion::from_github)
            .unwrap_or_default();
        Ok(store
            .claim_goal_piece_for_version(1, claimant, &slot.piece, "vel/claim".into(), version)
            .is_ok())
    }

    fn check_projected_row(
        row_index: usize,
        fields: &[&str],
        line: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let spec = parse_slot(fields[1])?;
        let expected: bool = fields[3].parse()?;
        let mut fixture = seed_slots(row_index, &[spec])?;
        let project = fixture.store.project(1).ok_or("missing project")?;
        let pool = production_pool_for_slots(project, &fixture.seeded)?;
        let flags = open_flags_for_pool(&pool, &fixture.held_keys);
        let pool_open = flags.first().copied().unwrap_or(false);
        assert_eq!(pool_open, expected, "inventory mismatch for {line}");
        if !spec.airc_held && (!spec.is_issue || spec.source_open) {
            assert_eq!(
                slot_claim_result(&mut fixture.store, &fixture.seeded[0])?,
                expected,
                "claim predicate mismatch for {line}"
            );
        }
        std::fs::remove_dir_all(fixture.directory)?;
        Ok(())
    }

    fn check_projected_choose_row(
        row_index: usize,
        fields: &[&str],
        line: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let specs = fields[1]
            .split(';')
            .map(parse_slot)
            .collect::<Result<Vec<_>, _>>()?;
        let expected_flags = fields[2]
            .split(',')
            .map(parse_bit)
            .collect::<Result<Vec<_>, _>>()?;
        let expected = match fields[3].strip_prefix("claim:") {
            Some(index) => ChoiceDecision::Claim(index.parse()?),
            None => ChoiceDecision::Leave,
        };
        let mut fixture = seed_slots(row_index, &specs)?;
        let project = fixture.store.project(1).ok_or("missing project")?;
        let pool = production_pool_for_slots(project, &fixture.seeded)?;
        let actual_flags: Vec<bool> = fixture
            .seeded
            .iter()
            .map(|slot| {
                pool.pieces
                    .iter()
                    .find(|p| p.key == slot.piece.key)
                    .is_some_and(|p| is_piece_unclaimed(p, &fixture.held_keys))
            })
            .collect();
        assert_eq!(actual_flags, expected_flags, "flags mismatch for {line}");
        let chosen = choose_pool_piece(&pool, &fixture.held_keys).map(|piece| piece.key.clone());
        let expected_key = match expected {
            ChoiceDecision::Claim(index) => Some(fixture.seeded[index].piece.key.clone()),
            ChoiceDecision::Leave => None,
        };
        assert_eq!(chosen, expected_key, "choice mismatch for {line}");
        let skipped = match expected {
            ChoiceDecision::Claim(index) => index,
            ChoiceDecision::Leave => fixture.seeded.len(),
        };
        for slot in &fixture.seeded[..skipped] {
            if !slot.spec.airc_held && (!slot.spec.is_issue || slot.spec.source_open) {
                assert!(
                    !slot_claim_result(&mut fixture.store, slot)?,
                    "skipped slot unexpectedly claimable for {line}"
                );
            }
        }
        if let ChoiceDecision::Claim(index) = expected {
            assert!(
                slot_claim_result(&mut fixture.store, &fixture.seeded[index])?,
                "chosen slot rejected by store claim predicate for {line}"
            );
        }
        std::fs::remove_dir_all(fixture.directory)?;
        Ok(())
    }

    // Risk (#636, #637, #638): projected Rosen completion (detached Landed/Green tasks and
    // unchanged Landed/Green issues) must match Lean's F16 model in both production pool
    // inventory and store claim predicates, while legitimately reopened items stay claimable.
    #[test]
    fn lean_projected_eligibility_fixtures() -> Result<(), Box<dyn std::error::Error>> {
        let (mut projected_rows, mut projected_choose_rows) = (0usize, 0usize);
        for (row_index, line) in include_str!("../../../proofs/fixtures/goal-swarm-work-choice.tsv")
            .lines()
            .enumerate()
        {
            let fields: Vec<_> = line.split('\t').collect();
            match fields.first().copied() {
                Some("projected") => {
                    projected_rows += 1;
                    check_projected_row(row_index, &fields, line)?;
                }
                Some("projected_choose") => {
                    projected_choose_rows += 1;
                    check_projected_choose_row(row_index, &fields, line)?;
                }
                Some("choose" | "attempt" | "merge" | "identity" | "reservation") => {}
                other => return Err(format!("unknown fixture kind {other:?}").into()),
            }
        }
        assert!(projected_rows >= 17 && projected_choose_rows >= 5);
        Ok(())
    }
}
