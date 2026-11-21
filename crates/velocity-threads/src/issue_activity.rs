use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};
use velocity_airc::{Error, IssueCreate, IssueRead, IssueRequest, IssueUpdate, IssueUpdateKind};
use velocity_core::issue_holds::UpstreamTarget;
use velocity_core::text_budget::{json_text_page, preview};
use velocity_github::{AgentIssueDraft, AgentIssueOutcome, IssueClient, IssueDetail};
use velocity_rosen::ProjectServer;
use velocity_store::{ProjectItem, VersionControl};

/// Stays below `TOOL_ANSWER_BYTES` (8,000) so `issue_read` always returns unclipped JSON.
const READ_RESPONSE_BUDGET: usize = 7_500;
const FULL_BODY_BUDGET: usize = 6_000;
const BODY_WITH_COMMENTS_BUDGET: usize = 4_200;
const COMMENT_PREVIEW_BYTES: usize = 1_200;

/// The tool process reads the project's GitHub identity from the app, since a Rosen
/// workspace has no Git remote and the issue number alone is only repository-local.
pub fn handle_issue_request(
    app_data: &Path,
    project: &str,
    request: IssueRequest<'_>,
) -> Result<Value, Error> {
    let (project, client) = open_issue_project(app_data, project)?;
    match request {
        IssueRequest::Read(read) => read_issue(&client, read),
        IssueRequest::Create(create) => create_issue(app_data, &project, &client, create),
        IssueRequest::Update(update) => {
            let url = post_update(project.id, &client, update)?;
            Ok(json!({"url": url}))
        }
    }
}

fn open_issue_project(app_data: &Path, project: &str) -> Result<(ProjectItem, IssueClient), Error> {
    let invalid = |message: &str| Error::Invalid(message.into());
    let identifier = project
        .parse::<u32>()
        .map_err(|_| invalid("Invalid project identity"))?;
    // Startup recovery belongs to the app that owns the state writer. Read only
    // the atomically replaced snapshot from this MCP child process (#562).
    let project = velocity_store::read_project_snapshot(app_data, identifier, false)
        .map_err(|error| Error::Invalid(error.to_string()))?
        .ok_or_else(|| invalid("The project was removed"))?;
    let directory = Path::new(&project.repository_path);
    let client = match &project.github_remote {
        Some(remote) => IssueClient::for_repository(directory, remote),
        None => IssueClient::discover(directory),
    }
    .map_err(|error| Error::Invalid(error.to_string()))?;
    Ok((project, client))
}

fn read_issue(client: &IssueClient, read: IssueRead) -> Result<Value, Error> {
    let detail = client
        .read_issue_detail(read.issue, if read.offset > 0 { 0 } else { read.comments })
        .map_err(|error| Error::Invalid(error.to_string()))?;
    format_issue_read(&detail, read.offset, read.comments)
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
) -> Result<Value, Error> {
    let blockers = velocity_core::issue_dependencies::parse_blockers(create.blocked_by)
        .map_err(Error::Invalid)?;
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
        .map_err(|error| Error::Invalid(error.to_string()))?;
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
) -> Result<String, Error> {
    let thread = velocity_core::thread_titles::thread_id_from_member(&update.claim.member);
    let byline = issue_byline(identifier, thread, Some(&update.claim.nick));
    // The issues this one waits on become GitHub dependencies, never a hold label (#511).
    let blockers = velocity_core::issue_dependencies::parse_blockers(update.blocked_by)
        .map_err(Error::Invalid)?;
    match update.kind {
        IssueUpdateKind::Comment => {
            client.post_issue_progress(update.issue, &byline, update.body, false, &blockers)
        }
        IssueUpdateKind::ReadyToClose => {
            client.post_issue_progress(update.issue, &byline, update.body, true, &blockers)
        }
        IssueUpdateKind::BlockedUpstream { upstream } => {
            let target = UpstreamTarget::parse(upstream).ok_or_else(|| {
                Error::Invalid(format!(
                    "upstream must be an upstream PR or release such as owner/repo#123, owner/repo@v1.0.0, or owner/repo#123 -> release:owner/repo (got {upstream:?})"
                ))
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
    .map_err(|error| Error::Invalid(error.to_string()))
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

        let (loaded, client) = open_issue_project(&app_data, "3")?;
        assert_eq!(loaded.id, 3);
        assert_eq!(client.repository(), "mESmaC/collapse.sh");

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
