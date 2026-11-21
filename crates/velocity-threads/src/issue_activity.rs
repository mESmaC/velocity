use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use velocity_airc::{Error, IssueCreate, IssueRead, IssueRequest, IssueUpdate, IssueUpdateKind};
use velocity_core::issue_holds::UpstreamTarget;
use velocity_core::project_health::AircIssueToolFailure;
use velocity_core::text_budget::{json_text_page, preview};
use velocity_core::work_pool::WorkKey;
use velocity_github::{
    AgentIssueDraft, AgentIssueOutcome, GitHubCommandError, IssueClient, IssueDetail,
    RepositoryName,
};
use velocity_rosen::ProjectServer;
use velocity_store::{ProjectItem, VersionControl};

/// Stays below `TOOL_ANSWER_BYTES` (8,000) so `issue_read` always returns unclipped JSON.
const READ_RESPONSE_BUDGET: usize = 7_500;
const FULL_BODY_BUDGET: usize = 6_000;
const BODY_WITH_COMMENTS_BUDGET: usize = 4_200;
const COMMENT_PREVIEW_BYTES: usize = 1_200;
const ISSUE_HEALTH_FOLDER: &str = "airc-issue-health";
const MAX_HEALTH_MESSAGE_CHARS: usize = 240;

enum IssueActivityFailure {
    /// Failed locally (Velocity state, missing `gh`, shim refusal, etc.) rather than GitHub.
    Local(Error),
    /// Failed at GitHub, over the network, or because of caller argument validation.
    External(Error),
}

impl IssueActivityFailure {
    fn from_github(error: GitHubCommandError) -> Self {
        let local = error.is_local_tool_failure();
        let message = Error::Invalid(error.to_string());
        if local {
            Self::Local(message)
        } else {
            Self::External(message)
        }
    }

    fn into_error(self) -> Error {
        match self {
            Self::Local(error) | Self::External(error) => error,
        }
    }
}

/// The tool process reads the project's GitHub identity from the app, since a Rosen
/// workspace has no Git remote and the issue number alone is only repository-local.
pub fn handle_issue_request(
    app_data: &Path,
    project: &str,
    request: IssueRequest<'_>,
) -> Result<Value, Error> {
    let tool = request_tool_name(&request);
    let thread = request_thread_id(&request);
    let project_id = project.parse::<u32>().ok();
    match run_issue_request(app_data, project, request) {
        Ok(value) => {
            if let Some(id) = project_id {
                clear_issue_tool_failure(app_data, id);
            }
            Ok(value)
        }
        Err(failure) => {
            if let IssueActivityFailure::Local(error) = &failure
                && let Some(id) = project_id
            {
                record_issue_tool_failure(app_data, id, tool, &error.to_string(), thread);
            }
            Err(failure.into_error())
        }
    }
}

fn run_issue_request(
    app_data: &Path,
    project: &str,
    request: IssueRequest<'_>,
) -> Result<Value, IssueActivityFailure> {
    let (project, client) = open_issue_project(app_data, project)?;
    match request {
        IssueRequest::Read(read) => read_issue(app_data, &project, &client, read),
        IssueRequest::Create(create) => create_issue(app_data, &project, &client, create),
        IssueRequest::Update(update) => {
            let url = post_update(project.id, &client, update)?;
            Ok(json!({"url": url}))
        }
    }
}

fn open_issue_project(
    app_data: &Path,
    project: &str,
) -> Result<(ProjectItem, IssueClient), IssueActivityFailure> {
    let local = |message: String| IssueActivityFailure::Local(Error::Invalid(message));
    let identifier = project
        .parse::<u32>()
        .map_err(|_| local("Invalid project identity".into()))?;
    // Startup recovery belongs to the app that owns the state writer. Read only
    // the atomically replaced snapshot from this MCP child process (#562).
    let project = velocity_store::read_project_snapshot(app_data, identifier, false)
        .map_err(|error| local(error.to_string()))?
        .ok_or_else(|| local("The project was removed".into()))?;
    let directory = Path::new(&project.repository_path);
    let client = match &project.github_remote {
        Some(remote) => IssueClient::for_repository(directory, remote),
        None => IssueClient::discover(directory),
    }
    .map_err(IssueActivityFailure::from_github)?;
    Ok((project, client))
}

fn read_issue(
    app_data: &Path,
    project: &ProjectItem,
    client: &IssueClient,
    read: IssueRead,
) -> Result<Value, IssueActivityFailure> {
    let target = resolve_read_target(app_data, project, client, &read)?;
    let comment_limit = if read.offset > 0 { 0 } else { read.comments };
    let detail = match target {
        Some(repository) => client.read_issue_detail_in(&repository, read.issue, comment_limit),
        None => client.read_issue_detail(read.issue, comment_limit),
    }
    .map_err(IssueActivityFailure::from_github)?;
    format_issue_read(&detail, read.offset, read.comments).map_err(IssueActivityFailure::External)
}

/// Resolves an optional cross-repository `issue_read` target (#598). Returns `None` for this
/// project's own repository, `Some(repo)` for an allowed sibling repository (a configured
/// Velocity project or a repository referenced in `blocked_by` dependencies), or an external
/// validation error when the repository is not allowed.
fn resolve_read_target(
    app_data: &Path,
    project: &ProjectItem,
    client: &IssueClient,
    read: &IssueRead,
) -> Result<Option<RepositoryName>, IssueActivityFailure> {
    let Some(raw_repository) = read.repository.as_deref() else {
        return Ok(None);
    };
    let requested = RepositoryName::parse(raw_repository)
        .map_err(|error| IssueActivityFailure::External(Error::Invalid(error.to_string())))?;
    let requested_name = requested.full_name();
    if requested_name.eq_ignore_ascii_case(client.repository()) {
        return Ok(None);
    }
    if let Some(configured) = find_configured_project_repository(app_data, &requested_name)? {
        return Ok(Some(configured));
    }
    if let Some(blocker) = find_blocker_repository(project, client, read, &requested_name)? {
        return Ok(Some(blocker));
    }
    Err(IssueActivityFailure::External(Error::Invalid(format!(
        "Repository `{requested_name}` is not allowed for issue_read. Only this project's repository (`{}`), repositories of projects configured in Velocity, and repositories referenced in blocked_by dependencies may be read.",
        client.repository()
    ))))
}

fn find_configured_project_repository(
    app_data: &Path,
    requested_name: &str,
) -> Result<Option<RepositoryName>, IssueActivityFailure> {
    let projects = velocity_store::read_all_project_snapshots(app_data, false)
        .map_err(|error| IssueActivityFailure::Local(Error::Invalid(error.to_string())))?;
    for item in projects {
        let candidate = item
            .github_remote
            .as_deref()
            .and_then(|remote| RepositoryName::parse(remote).ok())
            .or_else(|| velocity_github::checkout_repository(Path::new(&item.repository_path)));
        if let Some(repository) = candidate
            && repository.full_name().eq_ignore_ascii_case(requested_name)
        {
            return Ok(Some(repository));
        }
    }
    Ok(None)
}

fn find_blocker_repository(
    project: &ProjectItem,
    client: &IssueClient,
    read: &IssueRead,
    requested_name: &str,
) -> Result<Option<RepositoryName>, IssueActivityFailure> {
    for issue_number in caller_issue_numbers(project, client.repository(), read) {
        let blockers = match client.linked_blocker_repositories(issue_number) {
            Ok(repositories) => repositories,
            Err(GitHubCommandError::Missing(_)) => continue,
            Err(error) => return Err(IssueActivityFailure::from_github(error)),
        };
        if let Some(matched) = match_repository(&blockers, requested_name) {
            return Ok(Some(matched));
        }
    }
    let open_blockers = client
        .open_issue_blocker_repositories()
        .map_err(IssueActivityFailure::from_github)?;
    Ok(match_repository(&open_blockers, requested_name))
}

fn caller_issue_numbers(
    project: &ProjectItem,
    home_repository: &str,
    read: &IssueRead,
) -> BTreeSet<u32> {
    let mut issues: BTreeSet<u32> = read
        .held_issues
        .iter()
        .copied()
        .filter(|number| *number > 0)
        .collect();
    let Some(thread_id) = read
        .member
        .as_deref()
        .and_then(velocity_core::thread_titles::thread_id_from_member)
    else {
        return issues;
    };
    if let Some(thread) = project.threads.iter().find(|thread| thread.id == thread_id)
        && let Some(assigned) = &thread.issue
        && assigned.number > 0
        && assigned.repository.eq_ignore_ascii_case(home_repository)
    {
        issues.insert(assigned.number);
    }
    for work in &project.rosen_work_items {
        if work.thread_identifier == Some(thread_id)
            && let WorkKey::Issue { repository, number } = &work.key
            && *number > 0
            && repository.eq_ignore_ascii_case(home_repository)
        {
            issues.insert(*number);
        }
    }
    issues
}

fn match_repository(candidates: &[String], requested_name: &str) -> Option<RepositoryName> {
    candidates
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(requested_name))
        .and_then(|candidate| RepositoryName::parse(candidate).ok())
}

fn format_issue_read(
    detail: &IssueDetail,
    offset: usize,
    comment_limit: usize,
) -> Result<Value, Error> {
    let include_comments = offset == 0 && comment_limit > 0 && !detail.comments.is_empty();
    let body_budget = if include_comments {
        BODY_WITH_COMMENTS_BUDGET
    } else {
        FULL_BODY_BUDGET
    };
    let (body, next_offset) = json_text_page(&detail.body, offset, body_budget)
        .ok_or_else(|| Error::Invalid("Use the returned byte offset".into()))?;
    let mut answer = json!({
        "number": detail.number,
        "repository": detail.repository,
        "url": detail.url,
        "state": detail.state,
        "title": preview(&detail.title, 300),
        "author": detail.author,
        "labels": detail.labels,
        "blocked_by": detail.blocked_by,
        "body": body,
    });
    if let Some(next) = next_offset {
        answer["next_offset"] = json!(next);
    }
    if offset > 0 {
        return Ok(answer);
    }
    answer["total_comments"] = json!(detail.total_comments);
    let mut remaining = READ_RESPONSE_BUDGET.saturating_sub(answer.to_string().len() + 96);
    let mut recent = Vec::new();
    if include_comments {
        for comment in detail.comments.iter().rev() {
            let entry = json!({
                "author": comment.author,
                "created_at": comment.created_at,
                "body": preview(&comment.body, COMMENT_PREVIEW_BYTES),
            });
            let size = entry.to_string().len() + 1;
            if size > remaining {
                break;
            }
            remaining -= size;
            recent.push(entry);
        }
        recent.reverse();
    }
    let omitted = detail.total_comments.saturating_sub(recent.len());
    answer["comments"] = json!(recent);
    if omitted > 0 {
        answer["comments_omitted"] = json!(omitted);
    }
    Ok(answer)
}

fn create_issue(
    app_data: &Path,
    project: &ProjectItem,
    client: &IssueClient,
    create: IssueCreate<'_>,
) -> Result<Value, IssueActivityFailure> {
    let blockers = velocity_core::issue_dependencies::parse_blockers(create.blocked_by)
        .map_err(|error| IssueActivityFailure::External(Error::Invalid(error)))?;
    let thread = velocity_core::thread_titles::thread_id_from_member(create.member);
    let byline = issue_byline(project.id, thread, Some(create.nick));
    let draft = AgentIssueDraft {
        title: create.title,
        body: create.body,
        labels: create.labels,
        blocked_by: &blockers,
        byline: &byline,
    };
    let outcome = client
        .create_agent_issue(&draft, |texts| {
            mentioned_issue_paths(app_data, project, texts)
        })
        .map_err(IssueActivityFailure::from_github)?;
    Ok(match outcome {
        AgentIssueOutcome::Filed {
            issue,
            blocked_by,
            unlinked,
        } => {
            let mut reply = json!({
                "outcome": "filed",
                "number": issue.number,
                "url": issue.url,
                "title": issue.title,
                "labels": issue.labels,
            });
            if !blocked_by.is_empty() {
                reply["blocked_by"] = json!(blocked_by);
            }
            if let Some(unlinked) = unlinked {
                reply["unlinked"] = json!(unlinked);
            }
            reply
        }
        AgentIssueOutcome::Duplicate { issue, reason } => json!({
            "outcome": "duplicate",
            "issue": {
                "number": issue.number,
                "url": issue.url,
                "title": issue.title,
                "state": issue.state,
                "labels": issue.labels,
            },
            "reason": reason,
        }),
    })
}

fn post_update(
    identifier: u32,
    client: &IssueClient,
    update: IssueUpdate<'_>,
) -> Result<String, IssueActivityFailure> {
    let thread = velocity_core::thread_titles::thread_id_from_member(&update.claim.member);
    let byline = issue_byline(identifier, thread, Some(&update.claim.nick));
    // The issues this one waits on become GitHub dependencies, never a hold label (#511).
    let blockers = velocity_core::issue_dependencies::parse_blockers(update.blocked_by)
        .map_err(|error| IssueActivityFailure::External(Error::Invalid(error)))?;
    match update.kind {
        IssueUpdateKind::Comment => {
            client.post_issue_progress(update.issue, &byline, update.body, false, &blockers)
        }
        IssueUpdateKind::ReadyToClose => {
            client.post_issue_progress(update.issue, &byline, update.body, true, &blockers)
        }
        IssueUpdateKind::BlockedUpstream { upstream } => {
            let target = UpstreamTarget::parse(upstream).ok_or_else(|| {
                IssueActivityFailure::External(Error::Invalid(format!(
                    "upstream must be an upstream PR or release such as owner/repo#123, owner/repo@v1.0.0, or owner/repo#123 -> release:owner/repo (got {upstream:?})"
                )))
            })?;
            client.post_blocked_upstream(update.issue, &byline, &target, update.body, &blockers)
        }
        IssueUpdateKind::Hold { release_action } => client.post_person_hold(
            update.issue,
            &byline,
            update.body,
            release_action,
            &blockers,
        ),
        IssueUpdateKind::CloseNotPlanned {
            spec_section,
            removed_commit,
        } => client.close_not_planned(
            update.issue,
            &byline,
            spec_section,
            removed_commit,
            update.body,
        ),
    }
    .map_err(IssueActivityFailure::from_github)
}

/// Reads the latest non-GitHub AIRC issue tool failure recorded for `project`, if any (#581).
pub fn read_issue_tool_failure(app_data: &Path, project: u32) -> Option<AircIssueToolFailure> {
    let bytes = std::fs::read(issue_health_path(app_data, project)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Clears any recorded non-GitHub AIRC issue tool failure for `project` (#581).
pub fn clear_issue_tool_failure(app_data: &Path, project: u32) {
    let _ = std::fs::remove_file(issue_health_path(app_data, project));
}

fn issue_health_path(app_data: &Path, project: u32) -> PathBuf {
    app_data
        .join(ISSUE_HEALTH_FOLDER)
        .join(format!("{project}.json"))
}

fn record_issue_tool_failure(
    app_data: &Path,
    project: u32,
    tool: &str,
    message: &str,
    thread_identifier: Option<u32>,
) {
    let path = issue_health_path(app_data, project);
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let previous_count = read_issue_tool_failure(app_data, project)
        .map(|existing| existing.count)
        .unwrap_or(0);
    let record = AircIssueToolFailure {
        tool: tool.to_owned(),
        message: compact_health_message(message),
        count: previous_count.saturating_add(1),
        failed_at_ms: unix_millis(),
        thread_identifier,
    };
    let Ok(bytes) = serde_json::to_vec(&record) else {
        return;
    };
    let temporary = parent.join(format!("{project}.{}.tmp", uuid::Uuid::new_v4()));
    if std::fs::write(&temporary, &bytes).is_ok() && std::fs::rename(&temporary, &path).is_err() {
        let _ = std::fs::write(&path, &bytes);
        let _ = std::fs::remove_file(&temporary);
    }
}

fn compact_health_message(message: &str) -> String {
    let line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut compact: String = line.chars().take(MAX_HEALTH_MESSAGE_CHARS).collect();
    if line.chars().count() > MAX_HEALTH_MESSAGE_CHARS {
        compact.push_str("...");
    }
    compact
}

fn request_tool_name(request: &IssueRequest<'_>) -> &'static str {
    match request {
        IssueRequest::Read(_) => "issue_read",
        IssueRequest::Create(_) => "issue_create",
        IssueRequest::Update(update) => match update.kind {
            IssueUpdateKind::Comment => "issue_comment",
            IssueUpdateKind::ReadyToClose => "issue_ready_to_close",
            IssueUpdateKind::BlockedUpstream { .. } => "issue_blocked_upstream",
            IssueUpdateKind::Hold { .. } => "issue_hold",
            IssueUpdateKind::CloseNotPlanned { .. } => "issue_close_not_planned",
        },
    }
}

fn request_thread_id(request: &IssueRequest<'_>) -> Option<u32> {
    let member = match request {
        IssueRequest::Read(read) => read.member.as_deref()?,
        IssueRequest::Create(create) => create.member,
        IssueRequest::Update(update) => update.claim.member.as_str(),
    };
    velocity_core::thread_titles::thread_id_from_member(member)
}

fn unix_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn mentioned_issue_paths(
    app_data: &Path,
    project: &ProjectItem,
    texts: &[(&str, &str)],
) -> Vec<BTreeSet<String>> {
    let Ok(executable) = std::env::current_exe() else {
        return vec![BTreeSet::new(); texts.len()];
    };
    let endpoint = (project.version_control == VersionControl::Rosen)
        .then(|| ProjectServer::new(app_data, project.id).endpoint().clone());
    velocity_graph::mentioned_paths(
        &executable,
        app_data,
        &project.id.to_string(),
        Path::new(&project.repository_path),
        endpoint,
        texts,
    )
}

pub fn issue_byline(project: u32, thread: Option<u32>, nick: Option<&str>) -> String {
    let agent = nick.map_or_else(
        || "Velocity agent".into(),
        |nick| format!("Velocity agent `{nick}`"),
    );
    match thread {
        Some(thread) => {
            let link = format!("velocity://thread/{project}/{thread}");
            format!("{agent} · [Thread {thread}]({link}) · `{link}`")
        }
        None => agent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use velocity_store::{PersistedState, StateStore};

    // Risk (#562): AIRC issue_read / issue_* MCP tools must open projects stored in
    // #552 split format where root `state.json` omits the legacy `projects` array.
    #[test]
    fn open_issue_project_reads_split_state_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "velocity-issue-activity-562-{}",
            uuid::Uuid::new_v4()
        ));
        let app_data = root.join("app-data");
        let repo = root.join("repo");
        fs::create_dir_all(&app_data)?;
        fs::create_dir_all(&repo)?;

        let mut legacy = PersistedState::default();
        let mut project = ProjectItem::new(3, "collapse.sh".into(), repo.display().to_string());
        project.github_remote = Some("mESmaC/collapse.sh".into());
        legacy.projects.push(project);
        fs::write(
            app_data.join(velocity_store::STATE_FILE_NAME),
            serde_json::to_vec(&legacy)?,
        )?;
        let _store = StateStore::load_or_default(&app_data)?;

        let (loaded, client) =
            open_issue_project(&app_data, "3").map_err(IssueActivityFailure::into_error)?;
        assert_eq!(loaded.id, 3);
        assert_eq!(client.repository(), "mESmaC/collapse.sh");

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
