//! `next_work` AIRC MCP tool backend: atomically claim the first unclaimed, unblocked work item
//! and return its structured task brief (#539).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde_json::{Value, json};
use velocity_airc::{Channel, ClaimResult, Error, NextWorkRequest};
use velocity_core::work_pool::{WorkKey, WorkSources, spec_files_pieces};
use velocity_github::{GitHubIssue, IssueClient};
use velocity_spec::ProjectSpec;
use velocity_store::issues::ThreadIssue;
use velocity_store::{PersistedState, ProjectItem, TaskAttribution, TaskItem, ThreadState};

use crate::continuous_dispatch::{dispatchable_task, ordered_issues};
use crate::task_brief::{BriefInput, build_brief_sync};

/// Keeps `next_work` JSON responses comfortably under `TOOL_ANSWER_BYTES` (8,000).
const NEXT_WORK_BRIEF_BUDGET: usize = 5_400;

enum CandidateWork {
    Task(TaskItem),
    Issue {
        repository: String,
        issue: Box<GitHubIssue>,
        unblocks: u32,
    },
    Spec {
        section: String,
        title: String,
        reference: String,
    },
}

impl CandidateWork {
    fn piece_key(&self) -> String {
        match self {
            Self::Task(task) => WorkKey::Task {
                identifier: task.id,
            }
            .claim_piece(),
            Self::Issue {
                repository, issue, ..
            } => WorkKey::issue(repository, issue.number).claim_piece(),
            Self::Spec { section, .. } => WorkKey::Spec {
                section: section.clone(),
            }
            .claim_piece(),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Task(_) => "task",
            Self::Issue { .. } => "issue",
            Self::Spec { .. } => "spec",
        }
    }

    fn title(&self) -> String {
        match self {
            Self::Task(task) => task.text.clone(),
            Self::Issue { issue, .. } => issue.title.clone(),
            Self::Spec { title, .. } => title.clone(),
        }
    }

    fn reference(&self) -> String {
        match self {
            Self::Task(task) => format!("Task List #{}", task.id),
            Self::Issue { issue, .. } => issue.url.clone(),
            Self::Spec { reference, .. } => reference.clone(),
        }
    }

    fn brief_input<'a>(
        &self,
        request: &NextWorkRequest<'_>,
        branch: &str,
        checkout: &'a Path,
        spec_path: &'a Path,
    ) -> BriefInput<'a> {
        let piece = self.piece_key();
        match self {
            Self::Task(task) => BriefInput {
                heading: format!("Task #{}: {}", task.id, task.text),
                reference: Some(self.reference()),
                piece: Some(piece),
                evidence_texts: vec![task.text.clone()],
                dependencies: Vec::new(),
                owner_note: format!(
                    "{} ({}) holds task:{} through land and after-land review. Velocity marks the task done on land.",
                    request.member, request.nick, task.id
                ),
                exclude_change: Some(branch.to_owned()),
                fix_forward: None,
                checkout,
                spec_path,
            },
            Self::Issue {
                repository,
                issue,
                unblocks,
            } => {
                let mut dependencies = Vec::new();
                if *unblocks > 0 {
                    dependencies.push(format!("Unblocks {unblocks} open issue(s)"));
                }
                BriefInput {
                    heading: format!("{repository}#{}: {}", issue.number, issue.title),
                    reference: Some(issue.url.clone()),
                    piece: Some(piece),
                    evidence_texts: vec![issue.title.clone(), issue.body.clone()],
                    dependencies,
                    owner_note: format!(
                        "{} ({}) holds issue:{} through land and after-land review. Leave the issue open; call issue_ready_to_close with evidence when naming the change.",
                        request.member, request.nick, issue.number
                    ),
                    exclude_change: Some(branch.to_owned()),
                    fix_forward: None,
                    checkout,
                    spec_path,
                }
            }
            Self::Spec {
                section,
                title,
                reference,
            } => BriefInput {
                heading: title.clone(),
                reference: Some(reference.clone()),
                piece: Some(piece),
                evidence_texts: vec![section.clone(), title.clone()],
                dependencies: Vec::new(),
                owner_note: format!(
                    "{} ({}) holds §7 item `{section}` through land and after-land review. Velocity ticks §7 on land.",
                    request.member, request.nick
                ),
                exclude_change: Some(branch.to_owned()),
                fix_forward: None,
                checkout,
                spec_path,
            },
        }
    }
}

/// Handles the AIRC MCP `next_work` tool call by reading project state, scanning work sources
/// in priority order, atomically claiming the first available piece, and returning its brief.
pub fn handle_next_work(
    app_data: &Path,
    project_id: &str,
    channel: &Channel,
    request: NextWorkRequest<'_>,
) -> Result<Value, Error> {
    let project = load_project(app_data, project_id)?;
    let fetch_issues = |project: &ProjectItem| read_project_issues(project);
    select_and_claim(app_data, &project, channel, request, fetch_issues)
}

pub(crate) fn select_and_claim(
    app_data: &Path,
    project: &ProjectItem,
    channel: &Channel,
    request: NextWorkRequest<'_>,
    issue_loader: impl Fn(&ProjectItem) -> Result<Option<(String, Vec<GitHubIssue>)>, String>,
) -> Result<Value, Error> {
    if !project.spec_locked {
        return Err(Error::Invalid(
            "Lock the spec before claiming build work".into(),
        ));
    }
    let sources = parse_sources(request.sources)?;
    let claimed: HashSet<String> = channel
        .claims()?
        .into_iter()
        .map(|claim| claim.key().to_owned())
        .collect();
    let (candidates, github_note) = collect_candidates(project, sources, &claimed, issue_loader);
    let branch = resolve_branch(project, &request);
    let files: Vec<String> = request.files.to_vec();
    let checkout = Path::new(&project.repository_path);
    let spec_path = Path::new(&project.spec_path);

    for candidate in candidates {
        let piece = candidate.piece_key();
        let title = candidate.title();
        match channel.claim_piece(request.member, &piece, &title, &branch, files.clone())? {
            ClaimResult::Granted { .. } => {
                let brief = build_brief_sync(
                    app_data,
                    project,
                    channel,
                    candidate.brief_input(&request, &branch, checkout, spec_path),
                );
                let mut answer = json!({
                    "status": "claimed",
                    "piece": piece,
                    "kind": candidate.kind(),
                    "title": title,
                    "reference": candidate.reference(),
                    "claim": {
                        "piece": candidate.piece_key(),
                        "task": candidate.title(),
                        "branch": branch,
                        "nick": request.nick,
                    },
                    "brief": brief.render(NEXT_WORK_BRIEF_BUDGET),
                });
                if let Some(note) = github_note {
                    answer["github_note"] = json!(note);
                }
                return Ok(answer);
            }
            ClaimResult::Refused { .. } => continue,
        }
    }
    let mut empty = json!({
        "status": "empty",
        "reason": "No unclaimed, unblocked work items remain.",
    });
    if let Some(note) = github_note {
        empty["github_note"] = json!(note);
    }
    Ok(empty)
}

fn load_project(app_data: &Path, project: &str) -> Result<ProjectItem, Error> {
    let identifier = project
        .parse::<u32>()
        .map_err(|_| Error::Invalid("Invalid project identity".into()))?;
    let snapshot =
        velocity_store::extended_windows_path(&app_data.join(velocity_store::STATE_FILE_NAME));
    let file = std::fs::File::open(snapshot)?;
    let state: PersistedState = serde_json::from_reader(std::io::BufReader::new(file))?;
    state
        .projects
        .into_iter()
        .find(|item| item.id == identifier)
        .ok_or_else(|| Error::Invalid("The project was removed".into()))
}

fn parse_sources(requested: &[String]) -> Result<WorkSources, Error> {
    if requested.is_empty() {
        return Ok(WorkSources::default());
    }
    let mut sources = WorkSources {
        tasks: false,
        issues: false,
        sections: false,
    };
    for source in requested {
        match source.to_ascii_lowercase().as_str() {
            "tasks" => sources.tasks = true,
            "issues" => sources.issues = true,
            "spec" | "sections" => sources.sections = true,
            other => {
                return Err(Error::Invalid(format!(
                    "Unknown work source `{other}`; expected tasks, issues or spec"
                )));
            }
        }
    }
    Ok(sources)
}

fn collect_candidates(
    project: &ProjectItem,
    sources: WorkSources,
    claimed: &HashSet<String>,
    issue_loader: impl Fn(&ProjectItem) -> Result<Option<(String, Vec<GitHubIssue>)>, String>,
) -> (Vec<CandidateWork>, Option<String>) {
    let mut candidates = Vec::new();
    let (mut user_tasks, mut other_tasks) = if sources.tasks {
        project
            .tasks
            .iter()
            .filter(|task| dispatchable_task(project, task, claimed))
            .partition::<Vec<_>, _>(|task| task.added_by == TaskAttribution::User)
    } else {
        (Vec::new(), Vec::new())
    };
    user_tasks.sort_by_key(|task| task.id);
    other_tasks.sort_by_key(|task| task.id);
    candidates.extend(user_tasks.into_iter().cloned().map(CandidateWork::Task));

    let mut github_note = None;
    if sources.issues {
        match issue_loader(project) {
            Ok(Some((repository, issues))) => {
                let mut effective_claimed = claimed.clone();
                include_project_holds(project, &repository, &issues, &mut effective_claimed);
                let unblocks = BTreeMap::new();
                for (issue, count) in
                    ordered_issues(project, &repository, &issues, &effective_claimed, &unblocks)
                {
                    candidates.push(CandidateWork::Issue {
                        repository: repository.clone(),
                        issue: Box::new(issue),
                        unblocks: count,
                    });
                }
            }
            Ok(None) => {}
            Err(error) => github_note = Some(error),
        }
    }

    if sources.sections {
        candidates.extend(available_spec_candidates(project, claimed));
    }

    candidates.extend(other_tasks.into_iter().cloned().map(CandidateWork::Task));
    (candidates, github_note)
}

fn include_project_holds(
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    claimed: &mut HashSet<String>,
) {
    let now = now_unix_seconds();
    for wait in project.continuous_dispatch.issue_overlaps.values() {
        if wait.issue.repository.eq_ignore_ascii_case(repository) {
            claimed.insert(format!("issue:{}", wait.issue.number));
        }
    }
    for held in project.continuous_dispatch.launch_holds(now) {
        if held.repository.eq_ignore_ascii_case(repository) {
            claimed.insert(format!("issue:{}", held.number));
        }
    }
    for issue in issues {
        let identity = ThreadIssue::new(repository.into(), issue.number);
        if project
            .remote_work_hold(&identity, Some(&issue.updated_at), now)
            .is_some()
        {
            claimed.insert(format!("issue:{}", issue.number));
        }
    }
}

fn available_spec_candidates(
    project: &ProjectItem,
    claimed: &HashSet<String>,
) -> Vec<CandidateWork> {
    let checkout = Path::new(&project.repository_path);
    let Ok(spec) = ProjectSpec::read(checkout, Path::new(&project.spec_path)) else {
        return Vec::new();
    };
    let files: Vec<(String, &str)> = spec
        .files
        .iter()
        .map(|file| {
            (
                file.path.to_string_lossy().replace('\\', "/"),
                file.markdown.as_str(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, markdown)| (path.as_str(), *markdown))
        .collect();
    spec_files_pieces(&borrowed)
        .into_iter()
        .filter_map(|piece| {
            let WorkKey::Spec { section } = &piece.key else {
                return None;
            };
            if project.built_spec_sections.contains(section) {
                return None;
            }
            if claimed.contains(&piece.key.claim_piece()) {
                return None;
            }
            let owned = project.threads.iter().any(|thread| {
                !thread.supervision.released
                    && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
                    && thread.spec_section.as_deref() == Some(section.as_str())
            });
            if owned {
                return None;
            }
            Some(CandidateWork::Spec {
                section: section.clone(),
                title: piece.title,
                reference: piece.reference,
            })
        })
        .collect()
}

fn read_project_issues(
    project: &ProjectItem,
) -> Result<Option<(String, Vec<GitHubIssue>)>, String> {
    let directory = Path::new(&project.repository_path);
    let client = match project.github_remote.as_deref() {
        Some(remote) => {
            IssueClient::for_repository(directory, remote).map_err(|error| error.to_string())?
        }
        None if directory.join(".git").exists()
            && IssueClient::has_origin(directory).unwrap_or(false) =>
        {
            IssueClient::discover(directory).map_err(|error| error.to_string())?
        }
        None => return Ok(None),
    };
    let repository = client.repository().to_owned();
    let issues = client.open_issues().map_err(|error| error.to_string())?;
    Ok(Some((repository, issues)))
}

fn resolve_branch(project: &ProjectItem, request: &NextWorkRequest<'_>) -> String {
    if let Some(branch) = request.branch {
        return branch.to_owned();
    }
    if let Ok(change) = std::env::var("ROSEN_CHANGE")
        && !change.trim().is_empty()
    {
        return change;
    }
    if let Some(thread_id) = request
        .member
        .strip_prefix("thread-")
        .and_then(|id| id.parse::<u32>().ok())
        && let Some(thread) = project.threads.iter().find(|thread| thread.id == thread_id)
    {
        if let Some(change) = &thread.rosen_change {
            return change.clone();
        }
        if !thread.branch_name.trim().is_empty() {
            return thread.branch_name.clone();
        }
    }
    request.nick.to_owned()
}

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use velocity_store::TaskState;

    // Risk (#539): next_work must respect Rule 2 (locked spec), skip blocked/claimed items,
    // atomically claim the first available item for each caller, and return its task brief.
    #[test]
    fn atomically_claims_first_unclaimed_unblocked_item_and_returns_brief()
    -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("velocity-next-work-test-{}", uuid::Uuid::new_v4()));
        let app_data = root.join("app-data");
        let repo = root.join("repo");
        fs::create_dir_all(&app_data)?;
        fs::create_dir_all(repo.join("spec"))?;
        fs::write(
            repo.join("SPEC.md"),
            "# SPEC\n\n## 7. Implementation order\n\n**M1 · Core**\n- [ ] Build next_work tool (F9)\n",
        )?;
        fs::write(
            repo.join("spec/F9-issues-and-dispatch.md"),
            "### F9 · Issues and dispatch\nDispatch rules.\n",
        )?;

        let mut project = ProjectItem::new(1, "velocity".into(), repo.display().to_string());
        let channel = Channel::open(&app_data, "1")?;
        channel.join("thread-1", "agent-1")?;
        channel.join("thread-2", "agent-2")?;

        // Unlocked spec must be refused (Rule 2).
        let unlocked = select_and_claim(
            &app_data,
            &project,
            &channel,
            NextWorkRequest {
                member: "thread-1",
                nick: "agent-1",
                sources: &[],
                branch: Some("chg-1"),
                files: &[],
            },
            |_| Ok(None),
        );
        assert!(unlocked.is_err());

        project.spec_locked = true;
        project.tasks.push(TaskItem::new(
            1,
            "Implement F9 brief".into(),
            TaskState::Todo,
            TaskAttribution::User,
        ));

        let first = select_and_claim(
            &app_data,
            &project,
            &channel,
            NextWorkRequest {
                member: "thread-1",
                nick: "agent-1",
                sources: &[],
                branch: Some("chg-1"),
                files: &[],
            },
            |_| Ok(None),
        )?;
        assert_eq!(first["status"], "claimed");
        assert_eq!(first["piece"], "task:1");
        assert!(
            first["brief"]
                .as_str()
                .is_some_and(|brief| brief.contains("Task Brief") && brief.contains("F9"))
        );

        // Second caller must atomically receive the next available piece (§7 item).
        let second = select_and_claim(
            &app_data,
            &project,
            &channel,
            NextWorkRequest {
                member: "thread-2",
                nick: "agent-2",
                sources: &[],
                branch: Some("chg-2"),
                files: &[],
            },
            |_| Ok(None),
        )?;
        assert_eq!(second["status"], "claimed");
        assert_eq!(second["kind"], "spec");

        // Third caller sees an empty pool.
        let third = select_and_claim(
            &app_data,
            &project,
            &channel,
            NextWorkRequest {
                member: "thread-2",
                nick: "agent-2",
                sources: &[],
                branch: Some("chg-2"),
                files: &[],
            },
            |_| Ok(None),
        )?;
        assert_eq!(third["status"], "empty");

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
