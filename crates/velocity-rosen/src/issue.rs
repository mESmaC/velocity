//! Work identity on the server-owned change, before its agent starts.
use rosen_client::protocol::{CommentTarget, LogRequest, ReadAnswer, ThreadFilter};
use rosen_core::SnapshotKind;
use velocity_core::work_pool::WorkKey;

use crate::{ProjectServer, RosenError, workspace::session};

/// Landed commits covered by Velocity's integration build or the corresponding
/// legacy built prefix. Commits absent from this server's local history are treated
/// as uncovered rather than failing the whole batch (#519).
pub async fn issue_build_coverage(
    server: &ProjectServer,
    commits: &[String],
) -> Result<Vec<String>, RosenError> {
    if commits.is_empty() {
        return Ok(Vec::new());
    }
    let client = crate::connection::requests(server.endpoint()).await?;
    let mut greens = Vec::new();
    if let Some(green) = crate::integration::last_green(server)? {
        greens.push(green);
    }
    if let Some(green) = legacy_green(&client).await?
        && !greens.contains(&green)
    {
        greens.push(green);
    }
    if greens.is_empty() {
        return Ok(Vec::new());
    }
    let mut covered = Vec::new();
    for commit in commits {
        let Ok(identifier) = crate::policy::object(commit) else {
            continue;
        };
        for green in &greens {
            match client.merge_base(commit, green).await {
                Ok(answer) if answer.merge_base == Some(identifier) => {
                    covered.push(commit.clone());
                    break;
                }
                Ok(_) | Err(rosen_client::Error::Server(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(covered)
}

async fn legacy_green(client: &rosen_client::Client) -> Result<Option<String>, RosenError> {
    let Some(green) = client.post_land_build_next().await?.last_green else {
        return Ok(None);
    };
    if client
        .project_config(Some(green))
        .await?
        .config
        .checks
        .pre_land
        .commands
        .is_empty()
    {
        return Ok(None);
    }
    Ok(Some(green.to_string()))
}

/// A resolved note on the root spec (or a folder source file) records the
/// source issue without naming unfinished work or leaving an open review discussion.
pub async fn record_work_sources(
    server: &ProjectServer,
    agent: &str,
    change: &str,
    project: u32,
    items: &[WorkKey],
) -> Result<(), RosenError> {
    if items.is_empty() {
        return Ok(());
    }
    let session = session(server, agent).await?;
    let path = issue_spec_path(session.client(), change).await?;
    let notes = session
        .client()
        .review_list(ThreadFilter {
            change: change.to_owned(),
            state: None,
        })
        .await?;
    for item in items {
        let (prefix, suffix) = match item {
            WorkKey::Issue { repository, number } => (
                format!("Velocity source issue: {repository}#{number}."),
                format!(
                    " The thread retains this association; its named snapshot uses the (#{number}) suffix."
                ),
            ),
            WorkKey::Task { identifier } => (
                format!("Velocity source task: project {project}, task {identifier}."),
                String::new(),
            ),
            WorkKey::Spec { .. } => continue,
        };
        let identifier = if let Some(note) = notes.iter().find(|note| {
            note.comments.first().is_some_and(|comment| {
                comment
                    .body
                    .get(..prefix.len())
                    .is_some_and(|body| body.eq_ignore_ascii_case(&prefix))
            })
        }) {
            if note.state == rosen_core::ThreadState::Resolved {
                continue;
            }
            note.id
        } else {
            let target = CommentTarget::Line {
                path: path.clone(),
                line: 1,
            };
            session
                .comment(change, target, prefix + &suffix, None)
                .await?
                .id
        };
        session.resolve_thread(identifier).await?;
    }
    Ok(())
}

async fn issue_spec_path(
    client: &rosen_client::Client,
    change: &str,
) -> Result<String, RosenError> {
    let mut folders = vec![String::new()];
    let mut paths = Vec::new();
    while let Some(folder) = folders.pop() {
        let ReadAnswer::Directory { entries, .. } = client.workspace_read(change, &folder).await?
        else {
            continue;
        };
        for entry in entries {
            let path = if folder.is_empty() {
                entry.name.clone()
            } else {
                format!("{folder}/{}", entry.name)
            };
            if folder.is_empty() && entry.name == velocity_spec::ROOT_FILE && entry.mode == "100644"
            {
                return Ok(path);
            }
            if entry.mode == "40000" && (path == velocity_spec::FOLDER || path.starts_with("spec/"))
            {
                folders.push(path);
            } else if path.starts_with("spec/")
                && entry.mode == "100644"
                && velocity_spec::is_markdown(std::path::Path::new(&path))
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.into_iter().next().ok_or_else(|| {
        RosenError::Client(rosen_client::Error::InvalidArgument(
            "The project has no spec file for its source issue".into(),
        ))
    })
}

/// Resolves the source GitHub issue `(repository, number)` recorded on a Rosen change or
/// its landed commit, for recovering orphaned `handoff_items` whose thread was deleted (#570).
pub async fn change_source_issue(
    server: &ProjectServer,
    change: &str,
    commit: &str,
    default_repository: Option<&str>,
) -> Result<Option<(String, u32)>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    match client
        .review_list(ThreadFilter {
            change: change.to_owned(),
            state: None,
        })
        .await
    {
        Ok(notes) => {
            for comment in notes.iter().flat_map(|note| note.comments.iter()) {
                if let Some(found) = parse_source_issue_note(&comment.body) {
                    return Ok(Some(found));
                }
            }
        }
        Err(rosen_client::Error::Server(_)) => {}
        Err(error) => return Err(error.into()),
    }
    let Some(repository) = default_repository else {
        return Ok(None);
    };
    if let Ok(log) = client
        .log(LogRequest {
            at: Some(commit.to_owned()),
            limit: Some(1),
            ..LogRequest::default()
        })
        .await
        && let Some(number) = log
            .entries
            .first()
            .and_then(|entry| parse_issue_suffix(&entry.summary))
    {
        return Ok(Some((repository.to_owned(), number)));
    }
    if let Ok(answer) = client.change_show(change).await {
        if let Some(number) = parse_owner_issue(answer.change.owner.as_str()) {
            return Ok(Some((repository.to_owned(), number)));
        }
        for snapshot in answer.change.snapshots.iter().rev() {
            if let SnapshotKind::Named(message) = &snapshot.kind
                && let Some(number) = parse_issue_suffix(message)
            {
                return Ok(Some((repository.to_owned(), number)));
            }
        }
    }
    Ok(None)
}

fn parse_source_issue_note(body: &str) -> Option<(String, u32)> {
    const PREFIX: &str = "Velocity source issue: ";
    let rest = body
        .get(..PREFIX.len())
        .filter(|head| head.eq_ignore_ascii_case(PREFIX))
        .and_then(|_| body.get(PREFIX.len()..))?;
    let token = rest
        .split(|character: char| character.is_whitespace() || character == '.')
        .next()?;
    let (repository, number) = token.rsplit_once('#')?;
    let number = number.parse::<u32>().ok().filter(|number| *number > 0)?;
    (repository.contains('/') && !repository.starts_with('/') && !repository.ends_with('/'))
        .then(|| (repository.to_owned(), number))
}

fn parse_issue_suffix(text: &str) -> Option<u32> {
    let mut found = None;
    let mut rest = text;
    while let Some(index) = rest.find("(#") {
        let after = &rest[index + 2..];
        let digits: String = after
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .collect();
        if !digits.is_empty()
            && after.get(digits.len()..digits.len() + 1) == Some(")")
            && let Ok(number) = digits.parse::<u32>()
            && number > 0
        {
            found = Some(number);
        }
        rest = after;
    }
    found
}

fn parse_owner_issue(owner: &str) -> Option<u32> {
    let rest = owner.strip_prefix("issue-")?;
    let digits = rest.split('-').next()?;
    digits.parse::<u32>().ok().filter(|number| *number > 0)
}

#[cfg(test)]
mod tests {
    use super::{parse_issue_suffix, parse_owner_issue, parse_source_issue_note};

    // Risk (#570): recovering an orphaned landed handoff after its thread is deleted relies
    // on accurately identifying the source repository and issue number from Rosen metadata.
    #[test]
    fn parses_source_issue_from_notes_commits_and_owners() {
        assert_eq!(
            parse_source_issue_note(
                "Velocity source issue: mESmaC/rosen#427. The thread retains this association."
            ),
            Some(("mESmaC/rosen".to_owned(), 427))
        );
        assert_eq!(parse_source_issue_note("Unrelated review comment"), None);
        assert_eq!(
            parse_issue_suffix("Fix post-land verification (#427)"),
            Some(427)
        );
        assert_eq!(
            parse_issue_suffix("Follow-up to (#193) for (#427)"),
            Some(427)
        );
        assert_eq!(parse_issue_suffix("No issue number"), None);
        assert_eq!(parse_owner_issue("issue-427-192"), Some(427));
        assert_eq!(parse_owner_issue("issue-427"), Some(427));
        assert_eq!(parse_owner_issue("scout-1"), None);
    }
}
