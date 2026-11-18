//! Issue identity on the server-owned change, before its agent starts (#386).
use rosen_client::protocol::{CommentTarget, ReadAnswer};

use crate::{ProjectServer, RosenError, workspace::session};

/// Landed commits covered by Velocity's integration build or the corresponding
/// legacy built prefix.
pub async fn issue_build_coverage(
    server: &ProjectServer,
    commits: &[String],
) -> Result<Vec<String>, RosenError> {
    if commits.is_empty() {
        return Ok(Vec::new());
    }
    let client = rosen_client::Client::connect(server.endpoint()).await?;
    let green = if crate::post_land::after_land(server).await? {
        crate::integration::last_green(server)?
    } else {
        legacy_green(&client).await?
    };
    let Some(green) = green else {
        return Ok(Vec::new());
    };
    let mut covered = Vec::new();
    for commit in commits {
        let identifier = crate::policy::object(commit)?;
        if client.merge_base(commit, &green).await?.merge_base == Some(identifier) {
            covered.push(commit.clone());
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
pub async fn record_issue(
    server: &ProjectServer,
    agent: &str,
    change: &str,
    repository: &str,
    number: u32,
) -> Result<(), RosenError> {
    let session = session(server, agent).await?;
    let path = issue_spec_path(session.client(), change).await?;
    let target = CommentTarget::Line { path, line: 1 };
    let note = session.comment(change, target, format!(
        "Velocity source issue: {repository}#{number}. The thread retains this association; \
         its named snapshot uses the (#{number}) suffix."
    ), None).await?;
    session.resolve_thread(note.id).await?;
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
