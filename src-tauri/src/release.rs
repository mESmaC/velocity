//! Cut a release for a landed commit (#431). The mirror forwards only the default branch
//! to GitHub (Collapse C4), so no tag reaches it. Velocity reads Rosen's last green commit,
//! waits until GitHub has that commit and its integration status, then dispatches the
//! release workflow, which creates the tag on the commit itself.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use velocity_github::{GitHubCommandError, IssueClient};

use crate::state::ManagedStateStore;
use crate::threads::lock;

/// How long the forwarder and the integration status publisher get to reach GitHub.
const DELIVERY_WAIT: Duration = Duration::from_secs(180);
const DELIVERY_POLL: Duration = Duration::from_secs(10);

/// The release the workflow was started for.
#[derive(Clone, Debug, Serialize, specta::Type)]
pub struct ReleaseCut {
    pub tag: String,
    pub commit: String,
    /// Rosen's last green commit, which covers `commit`.
    pub green: String,
}

/// `cut_release` (§3.1): without `commit`, Rosen's last green commit is released.
#[tauri::command]
#[specta::specta]
pub async fn cut_release(
    application: AppHandle,
    project_identifier: u32,
    version: String,
    commit: Option<String>,
) -> Result<ReleaseCut, String> {
    let tag = velocity_core::release::release_tag(&version).ok_or_else(|| {
        format!("{version} is not a release version such as 0.1.0 or 0.1.0-beta.2")
    })?;
    if let Some(requested) = commit
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        && !velocity_core::release::is_full_commit(requested)
    {
        return Err(format!("{requested} is not a full 40-character commit ID"));
    }
    if !crate::rosen::uses_rosen(&application, project_identifier) {
        return Err("Releases are cut from a Rosen project's landed commits".into());
    }
    let (directory, repository) = github_repository(&application, project_identifier)?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    let release = velocity_rosen::release::release_commit(&server, commit.as_deref())
        .await
        .map_err(|error| error.to_string())?;
    let cut = ReleaseCut {
        tag,
        commit: release.commit,
        green: release.green,
    };
    let dispatched = cut.clone();
    tauri::async_runtime::spawn_blocking(move || dispatch(&directory, &repository, &dispatched))
        .await
        .map_err(|error| error.to_string())??;
    Ok(cut)
}

/// The project's folder and the GitHub repository its releases are published on.
fn github_repository(application: &AppHandle, project: u32) -> Result<(PathBuf, String), String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let item = store.project(project).ok_or("Unknown project")?;
    let repository = item
        .github_remote
        .clone()
        .ok_or("This project has no GitHub repository to publish releases on")?;
    Ok((PathBuf::from(&item.repository_path), repository))
}

fn dispatch(directory: &Path, repository: &str, cut: &ReleaseCut) -> Result<(), String> {
    let client =
        IssueClient::for_repository(directory, repository).map_err(|error| error.to_string())?;
    wait_for_green_on_github(&client, &cut.green)?;
    client
        .dispatch_release(&cut.tag, &cut.commit, &cut.green)
        .map_err(|error| error.to_string())
}

/// The release commit is an ancestor of the green commit, so the green commit reaching
/// GitHub's default branch means both have. The gate also needs its integration status.
fn wait_for_green_on_github(client: &IssueClient, green: &str) -> Result<(), String> {
    let started = Instant::now();
    loop {
        // GitHub's compare answers "not found" for a commit it has not received yet,
        // which is the state this loop waits out, not a failure.
        let delivered = match client.rosen_commit_mirrored(green) {
            Ok(delivered) => delivered,
            Err(GitHubCommandError::Command { .. }) => false,
            Err(error) => return Err(error.to_string()),
        };
        let passed = delivered
            && client
                .integration_passed(green)
                .map_err(|error| error.to_string())?;
        if passed {
            return Ok(());
        }
        if started.elapsed() >= DELIVERY_WAIT {
            let missing = if delivered {
                "has no green velocity/integration status on"
            } else {
                "has not received"
            };
            return Err(format!(
                "GitHub {missing} Rosen's green commit {green} yet. Try again once the mirror has forwarded it."
            ));
        }
        std::thread::sleep(DELIVERY_POLL);
    }
}
