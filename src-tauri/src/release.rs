//! Cut a release for a landed commit (#431, #521). The mirror forwards only the default
//! branch to GitHub (Collapse C4), so no tag reaches it. Velocity ensures the release
//! commit has a green `velocity/integration` status on GitHub (publishing a local pass,
//! cutting from the newest commit that already has one, or starting an integration build
//! immediately and queuing the release) before dispatching `release.yml`, which creates
//! the tag on the commit itself.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use velocity_core::project_health::short_commit;
use velocity_github::{GitHubCommandError, IssueClient};
use velocity_rosen::{ProjectServer, integration};

use crate::state::ManagedStateStore;
use crate::threads::lock;

/// How long the forwarder and the integration status publisher get to reach GitHub.
const DELIVERY_WAIT: Duration = Duration::from_secs(180);
const DELIVERY_POLL: Duration = Duration::from_secs(10);
/// How many recent commits on GitHub's default branch are checked for an existing green
/// `velocity/integration` status when Rosen's tip has none yet (#521).
const GITHUB_GREEN_LOOKBACK: usize = 20;

/// The release the workflow was started (or queued) for.
#[derive(Clone, Debug, Serialize, specta::Type)]
pub struct ReleaseCut {
    pub tag: String,
    pub commit: String,
    /// The green commit covering `commit`, or empty while waiting for an integration build.
    pub green: String,
    /// True when Velocity started an integration build and queued the release instead of
    /// dispatching `release.yml` immediately (#521).
    pub waiting_for_integration: bool,
    /// Human-readable status for the Rosen status pane (#521).
    pub note: String,
}

enum CutPlan {
    Dispatch {
        commit: String,
        green: String,
        local_green: bool,
        fell_back_from: Option<String>,
    },
    WaitForIntegration {
        commit: String,
    },
    StartIntegrationOnly,
}

/// `cut_release` (§3.1): without `commit`, releases the newest commit with a green
/// `velocity/integration` status; with an unverified commit, starts an integration build
/// immediately and queues the release so the release gate is never left refused (#431, #521).
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
    let requested = commit
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    if let Some(target) = requested.as_deref()
        && !velocity_core::release::is_full_commit(target)
    {
        return Err(format!("{target} is not a full 40-character commit ID"));
    }
    if !crate::rosen::uses_rosen(&application, project_identifier) {
        return Err("Releases are cut from a Rosen project's landed commits".into());
    }
    let (directory, repository) = github_repository(&application, project_identifier)?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    let plan = plan_cut(&server, &directory, &repository, requested.as_deref()).await?;
    execute_plan(
        &application,
        project_identifier,
        &directory,
        &repository,
        tag,
        plan,
    )
    .await
}

async fn plan_cut(
    server: &ProjectServer,
    directory: &Path,
    repository: &str,
    requested: Option<&str>,
) -> Result<CutPlan, String> {
    let rosen_release = velocity_rosen::release::release_commit(server, requested)
        .await
        .ok();
    let local_green = match &rosen_release {
        Some(release) => {
            let reading = server.clone();
            let green = release.green.clone();
            tauri::async_runtime::spawn_blocking(move || integration::passed(&reading, &green))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(false)
        }
        None => false,
    };
    let (dir, repo, req) = (
        directory.to_path_buf(),
        repository.to_owned(),
        requested.map(str::to_owned),
    );
    tauri::async_runtime::spawn_blocking(move || {
        let client = IssueClient::for_repository(&dir, &repo).map_err(|error| error.to_string())?;
        resolve_plan(&client, rosen_release, local_green, req.as_deref())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn resolve_plan(
    client: &IssueClient,
    rosen_release: Option<velocity_rosen::release::ReleaseCommit>,
    local_green: bool,
    requested: Option<&str>,
) -> Result<CutPlan, String> {
    if let Some(release) = rosen_release {
        if local_green {
            let _ = client.publish_integration_success(&release.green);
            return Ok(CutPlan::Dispatch {
                commit: release.commit,
                green: release.green,
                local_green: true,
                fell_back_from: None,
            });
        }
        if let Ok(Some(coverage)) = client.integration_status_covering(&release.commit) {
            return Ok(CutPlan::Dispatch {
                commit: release.commit,
                green: coverage.commit,
                local_green: false,
                fell_back_from: None,
            });
        }
        if requested.is_none()
            && let Ok(Some(github_green)) = client.newest_integration_green(GITHUB_GREEN_LOOKBACK)
        {
            return Ok(CutPlan::Dispatch {
                commit: github_green.clone(),
                green: github_green,
                local_green: false,
                fell_back_from: Some(release.commit),
            });
        }
        return Ok(CutPlan::WaitForIntegration {
            commit: release.commit,
        });
    }
    if let Some(target) = requested {
        if let Ok(Some(coverage)) = client.integration_status_covering(target) {
            return Ok(CutPlan::Dispatch {
                commit: target.to_owned(),
                green: coverage.commit,
                local_green: false,
                fell_back_from: None,
            });
        }
        return Ok(CutPlan::WaitForIntegration {
            commit: target.to_owned(),
        });
    }
    if let Ok(Some(github_green)) = client.newest_integration_green(GITHUB_GREEN_LOOKBACK) {
        return Ok(CutPlan::Dispatch {
            commit: github_green.clone(),
            green: github_green,
            local_green: false,
            fell_back_from: None,
        });
    }
    Ok(CutPlan::StartIntegrationOnly)
}

async fn execute_plan(
    application: &AppHandle,
    project: u32,
    directory: &Path,
    repository: &str,
    tag: String,
    plan: CutPlan,
) -> Result<ReleaseCut, String> {
    match plan {
        CutPlan::StartIntegrationOnly => {
            crate::rosen_landing::request_integration(application, project, None);
            Err(
                "No commit has a green velocity/integration status yet. Started an integration build now — try Cut release again once it finishes."
                    .into(),
            )
        }
        CutPlan::WaitForIntegration { commit } => {
            Ok(queue_integration_cut(application, project, tag, commit))
        }
        CutPlan::Dispatch {
            commit,
            green,
            local_green,
            fell_back_from,
        } => {
            dispatch_cut(
                application,
                project,
                directory,
                repository,
                tag,
                commit,
                green,
                local_green,
                fell_back_from,
            )
            .await
        }
    }
}

fn queue_integration_cut(
    application: &AppHandle,
    project: u32,
    tag: String,
    commit: String,
) -> ReleaseCut {
    crate::rosen_landing::request_integration(application, project, Some(commit.clone()));
    crate::project_health::queue_pending_release(application, project, tag.clone(), commit.clone());
    let note = format!(
        "Started integration build for {}; release workflow for {tag} will start automatically once it passes",
        short_commit(&commit)
    );
    ReleaseCut {
        tag,
        commit,
        green: String::new(),
        waiting_for_integration: true,
        note,
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_cut(
    application: &AppHandle,
    project: u32,
    directory: &Path,
    repository: &str,
    tag: String,
    commit: String,
    green: String,
    local_green: bool,
    fell_back_from: Option<String>,
) -> Result<ReleaseCut, String> {
    if let Some(newer) = &fell_back_from {
        crate::rosen_landing::request_integration(application, project, Some(newer.clone()));
    }
    let (dir, repo, tag_copy, commit_copy, green_copy) = (
        directory.to_path_buf(),
        repository.to_owned(),
        tag.clone(),
        commit.clone(),
        green.clone(),
    );
    tauri::async_runtime::spawn_blocking(move || {
        dispatch(
            &dir,
            &repo,
            &tag_copy,
            &commit_copy,
            &green_copy,
            local_green,
        )
    })
    .await
    .map_err(|error| error.to_string())??;
    crate::project_health::invalidate_release_health(application, project);
    let note = match fell_back_from {
        Some(newer) => format!(
            "Release workflow started for {tag} at {} (newest green integration build; started integration build for {})",
            short_commit(&commit),
            short_commit(&newer)
        ),
        None => format!(
            "Release workflow started for {tag} at {}",
            short_commit(&commit)
        ),
    };
    Ok(ReleaseCut {
        tag,
        commit,
        green,
        waiting_for_integration: false,
        note,
    })
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

fn dispatch(
    directory: &Path,
    repository: &str,
    tag: &str,
    commit: &str,
    green: &str,
    local_green: bool,
) -> Result<(), String> {
    let client =
        IssueClient::for_repository(directory, repository).map_err(|error| error.to_string())?;
    wait_for_green_on_github(&client, green, local_green)?;
    client
        .dispatch_release(tag, commit, green)
        .map_err(|error| error.to_string())
}

/// The release commit is an ancestor of the green commit, so the green commit reaching
/// GitHub's default branch means both have. The gate also needs its integration status.
fn wait_for_green_on_github(
    client: &IssueClient,
    green: &str,
    local_green: bool,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        // GitHub's compare answers "not found" for a commit it has not received yet,
        // which is the state this loop waits out, not a failure.
        let delivered = match client.rosen_commit_mirrored(green) {
            Ok(delivered) => delivered,
            Err(GitHubCommandError::Command { .. }) => false,
            Err(error) => return Err(error.to_string()),
        };
        if delivered && local_green {
            let _ = client.publish_integration_success(green);
        }
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
