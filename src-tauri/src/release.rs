//! Cut a release for a landed commit (#431, #521, #593).
//!
//! When a project mirrors to Collapse, Cut release builds locally on this machine via
//! `scripts/release-local.ps1 -Sign`, signs every download with the updater key held in
//! Windows Credential Manager (`velocity:updater-signing-key`), publishes the release,
//! assets and standalone Linux `velocity-graph` archive to Collapse, and rewrites
//! `latest.json` on Collapse's rolling `updater` release (#593).
//!
//! For GitHub-only projects without a Collapse mirror, Cut release keeps the existing
//! GitHub `release.yml` workflow dispatch path (#431, #521).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use velocity_collapse::UpdaterSigningKey;
use velocity_collapse::releases::{
    self as collapse_releases, CollapseRelease, CreateReleaseRequest, UPDATER_TAG,
};
use velocity_core::project_health::short_commit;
use velocity_core::release::{build_updater_manifest, is_prerelease_tag, version_from_release_tag};
use velocity_core::rosen_pin::format_utc_seconds;
use velocity_github::velocity_reports::VELOCITY_REPOSITORY;
use velocity_github::{GitHubCommandError, IssueClient};
use velocity_rosen::{ProjectServer, integration};
use velocity_store::duty::LandingProject;
use velocity_threads::local_release::{self, BuiltReleaseAssets, SignedRelease};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::lock;

/// How long the forwarder and the integration status publisher get to reach GitHub.
const DELIVERY_WAIT: Duration = Duration::from_secs(180);
const DELIVERY_POLL: Duration = Duration::from_secs(10);
/// How many recent commits on GitHub's default branch are checked for an existing green
/// `velocity/integration` status when Rosen's tip has none yet (#521).
const GITHUB_GREEN_LOOKBACK: usize = 20;
const RELEASE_LOG_SOURCE: &str = "release";

/// Guards `local-release` so only one Collapse release build runs at a time.
static CUTTING_COLLAPSE_RELEASE: AtomicBool = AtomicBool::new(false);

struct CollapseCutGuard;

impl Drop for CollapseCutGuard {
    fn drop(&mut self) {
        CUTTING_COLLAPSE_RELEASE.store(false, Ordering::SeqCst);
    }
}

/// The release the build/publish or workflow was started (or queued) for.
#[derive(Clone, Debug, Serialize, specta::Type)]
pub struct ReleaseCut {
    pub tag: String,
    pub commit: String,
    /// The green commit covering `commit`, or empty while waiting for an integration build.
    pub green: String,
    /// True when Velocity started an integration build and queued the release instead of
    /// building or dispatching immediately (#521).
    pub waiting_for_integration: bool,
    /// Human-readable status for the Rosen status pane (#521).
    pub note: String,
}

/// Presence of local release credentials in Windows Credential Manager (#593).
/// Never exposes any secret bytes or tokens to the window.
#[derive(Clone, Debug, Serialize, specta::Type)]
pub struct ReleaseCredentialsStatus {
    pub updater_key_stored: bool,
    pub collapse_release_token_stored: bool,
    pub collapse_connected: bool,
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

struct ReleaseProject {
    landing: LandingProject,
    directory: PathBuf,
    collapse_repository: Option<String>,
    github_repository: Option<String>,
}

/// `get_release_credentials_status` (§3.1): reports whether the Tauri updater signing key
/// and optional Collapse release token are saved in Windows Credential Manager (#593).
#[tauri::command]
#[specta::specta]
pub async fn get_release_credentials_status() -> Result<ReleaseCredentialsStatus, String> {
    tauri::async_runtime::spawn_blocking(read_credentials_status)
        .await
        .map_err(|error| error.to_string())?
}

/// `set_updater_signing_key` (§3.1): saves or replaces the Tauri updater signing key and
/// optional password in Windows Credential Manager (`velocity:updater-signing-key`) (#593).
#[tauri::command]
#[specta::specta]
pub async fn set_updater_signing_key(
    private_key: String,
    password: Option<String>,
) -> Result<ReleaseCredentialsStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        velocity_collapse::save_updater_signing_key(&UpdaterSigningKey {
            private_key,
            password,
        })
        .map_err(|error| error.to_string())?;
        read_credentials_status()
    })
    .await
    .map_err(|error| error.to_string())?
}

/// `clear_updater_signing_key` (§3.1): removes the Tauri updater signing key from Windows
/// Credential Manager (#593).
#[tauri::command]
#[specta::specta]
pub async fn clear_updater_signing_key() -> Result<ReleaseCredentialsStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        velocity_collapse::forget_updater_signing_key().map_err(|error| error.to_string())?;
        read_credentials_status()
    })
    .await
    .map_err(|error| error.to_string())?
}

/// `set_collapse_release_token` (§3.1): saves or replaces a Collapse personal access token
/// with `releases` scope in Windows Credential Manager (`velocity:collapse.sh:releases`) (#593).
#[tauri::command]
#[specta::specta]
pub async fn set_collapse_release_token(token: String) -> Result<ReleaseCredentialsStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        velocity_collapse::save_release_token(&token).map_err(|error| error.to_string())?;
        read_credentials_status()
    })
    .await
    .map_err(|error| error.to_string())?
}

/// `clear_collapse_release_token` (§3.1): removes the Collapse release token from Windows
/// Credential Manager (#593).
#[tauri::command]
#[specta::specta]
pub async fn clear_collapse_release_token() -> Result<ReleaseCredentialsStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        velocity_collapse::forget_release_token().map_err(|error| error.to_string())?;
        read_credentials_status()
    })
    .await
    .map_err(|error| error.to_string())?
}

fn read_credentials_status() -> Result<ReleaseCredentialsStatus, String> {
    let updater_key_stored = velocity_collapse::load_updater_signing_key()
        .map_err(|error| error.to_string())?
        .is_some();
    let collapse_release_token_stored = velocity_collapse::load_release_token()
        .map_err(|error| error.to_string())?
        .is_some();
    let collapse_connected = velocity_collapse::load_credentials()
        .map_err(|error| error.to_string())?
        .is_some();
    Ok(ReleaseCredentialsStatus {
        updater_key_stored,
        collapse_release_token_stored,
        collapse_connected,
    })
}

/// `cut_release` (§3.1): without `commit`, releases the newest green landed commit; with
/// an unverified commit, starts an integration build and queues the release. Projects
/// mirrored to Collapse build locally, sign with Windows Credential Manager's updater key
/// and publish to Collapse (including the rolling `updater/latest.json`) (#593); GitHub-only
/// projects dispatch `release.yml` (#431, #521).
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
    let project = release_project(&application, project_identifier)?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    if let Some(collapse_repo) = project.collapse_repository.clone() {
        return cut_collapse_release(
            &application,
            &project,
            &server,
            collapse_repo,
            tag,
            requested.as_deref(),
        )
        .await;
    }
    let repository = project
        .github_repository
        .ok_or("This project has no Collapse or GitHub repository to publish releases on")?;
    let plan = plan_cut(
        &server,
        &project.directory,
        &repository,
        requested.as_deref(),
    )
    .await?;
    execute_plan(
        &application,
        project_identifier,
        &project.directory,
        &repository,
        tag,
        plan,
    )
    .await
}

/// Starts a previously queued Collapse release once its commit becomes green (#593).
pub(crate) async fn trigger_pending_collapse_release(
    application: &AppHandle,
    project_identifier: u32,
    tag: String,
    commit: String,
    green: String,
) -> Result<String, String> {
    let project = release_project(application, project_identifier)?;
    let collapse_repo = project
        .collapse_repository
        .clone()
        .ok_or("This project has no Collapse repository")?;
    let cut = start_collapse_release_job(application, &project, &collapse_repo, tag, commit, green)
        .await?;
    Ok(cut.note)
}

async fn cut_collapse_release(
    application: &AppHandle,
    project: &ReleaseProject,
    server: &ProjectServer,
    collapse_repo: String,
    tag: String,
    requested: Option<&str>,
) -> Result<ReleaseCut, String> {
    use velocity_rosen::release::{ReleaseGateCheck, check_release_gate};
    match check_release_gate(server, requested)
        .await
        .map_err(|error| error.to_string())?
    {
        ReleaseGateCheck::Ready(release) => {
            start_collapse_release_job(
                application,
                project,
                &collapse_repo,
                tag,
                release.commit,
                release.green,
            )
            .await
        }
        ReleaseGateCheck::Waiting {
            local_needs_run,
            message,
            ..
        } if requested.is_some() => {
            let commit = requested.unwrap_or_default().to_owned();
            Ok(queue_collapse_release_wait(
                application,
                project.landing.project.id,
                tag,
                commit,
                local_needs_run,
                message,
            ))
        }
        ReleaseGateCheck::Waiting {
            local_needs_run,
            message,
            ..
        } => {
            if local_needs_run {
                crate::rosen_landing::request_integration(
                    application,
                    project.landing.project.id,
                    None,
                );
            }
            Err(message)
        }
    }
}

fn queue_collapse_release_wait(
    application: &AppHandle,
    project: u32,
    tag: String,
    commit: String,
    local_needs_run: bool,
    reason: String,
) -> ReleaseCut {
    if local_needs_run {
        crate::rosen_landing::request_integration(application, project, Some(commit.clone()));
    }
    crate::project_health::queue_pending_release(application, project, tag.clone(), commit.clone());
    let note = format!(
        "Queued release {tag} for {}: {reason}. It will start automatically once every required platform is green.",
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

async fn start_collapse_release_job(
    application: &AppHandle,
    project: &ReleaseProject,
    collapse_repo: &str,
    tag: String,
    commit: String,
    green: String,
) -> Result<ReleaseCut, String> {
    if !cfg!(windows) {
        return Err("Local signed releases to Collapse are built on Windows only".into());
    }
    let version = version_from_release_tag(&tag)
        .ok_or_else(|| format!("{tag} is not a v<semver> release tag"))?;
    let (owner, name) = velocity_core::collapse::parse_repository(collapse_repo)
        .ok_or_else(|| format!("Invalid Collapse repository {collapse_repo}"))?;
    let (signing_key, token) = load_collapse_release_secrets().await?;
    let app_data = application
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve app data directory: {error}"))?;
    if CUTTING_COLLAPSE_RELEASE.swap(true, Ordering::SeqCst) {
        return Err("A local Collapse release build is already in progress".into());
    }
    let build_graph_indexer = is_velocity_project(project);
    let handle = application.clone();
    let landing = project.landing.clone();
    let project_id = landing.project.id;
    let directory = project.directory.clone();
    let repo_label = format!("{owner}/{name}");
    let tag_copy = tag.clone();
    let commit_copy = commit.clone();
    tauri::async_runtime::spawn(async move {
        let _guard = CollapseCutGuard;
        let outcome = run_collapse_release(CollapseReleaseJob {
            app_data,
            landing,
            directory,
            owner,
            name,
            tag: tag_copy.clone(),
            version,
            commit: commit_copy.clone(),
            build_graph_indexer,
            signing_key,
            token,
        })
        .await;
        match outcome {
            Ok(summary) => {
                crate::project_health::invalidate_release_health(&handle, project_id);
                crate::diagnostics::record(RELEASE_LOG_SOURCE, &summary);
                let _ = toast(&handle, summary);
            }
            Err(error) => {
                let message =
                    format!("Release {tag_copy} to Collapse ({repo_label}) failed: {error}");
                crate::diagnostics::record(RELEASE_LOG_SOURCE, &message);
                let _ = toast(&handle, message);
            }
        }
    });
    let note = format!(
        "Building, signing and publishing {tag} at {} to Collapse ({collapse_repo})…",
        short_commit(&commit)
    );
    Ok(ReleaseCut {
        tag,
        commit,
        green,
        waiting_for_integration: false,
        note,
    })
}

async fn load_collapse_release_secrets() -> Result<(UpdaterSigningKey, String), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let signing_key = velocity_collapse::load_updater_signing_key()
            .map_err(|error| error.to_string())?
            .ok_or(
                "No Tauri updater signing key is saved in Windows Credential Manager (target `velocity:updater-signing-key`). Add it in Settings → Collapse → Release credentials.",
            )?;
        let token = velocity_collapse::resolve_release_token()
            .map_err(|error| error.to_string())?
            .ok_or(
                "No Collapse release token is available. Connect Collapse or save a personal access token with `releases` scope in Settings → Collapse → Release credentials.",
            )?;
        Ok((signing_key, token))
    })
    .await
    .map_err(|error| error.to_string())?
}

struct CollapseReleaseJob {
    app_data: PathBuf,
    landing: LandingProject,
    directory: PathBuf,
    owner: String,
    name: String,
    tag: String,
    version: String,
    commit: String,
    build_graph_indexer: bool,
    signing_key: UpdaterSigningKey,
    token: String,
}

async fn run_collapse_release(job: CollapseReleaseJob) -> Result<String, String> {
    let out_dir = job.app_data.join("collapse-releases").join(&job.version);
    if out_dir.exists() {
        let _ = std::fs::remove_dir_all(&out_dir);
    }
    let built = local_release::build_signed(
        &job.app_data,
        &job.landing,
        &SignedRelease {
            commit: job.commit.clone(),
            version: job.version.clone(),
            out_dir: out_dir.clone(),
            build_graph_indexer: job.build_graph_indexer,
        },
        &job.signing_key,
    )
    .await?;
    let publish_result = publish_built_release_to_collapse(&job, &built).await;
    let _ = std::fs::remove_dir_all(&out_dir);
    publish_result?;
    Ok(format!(
        "Published {} to Collapse ({}/{}) and updated updater/latest.json",
        job.tag, job.owner, job.name
    ))
}

async fn publish_built_release_to_collapse(
    job: &CollapseReleaseJob,
    built: &BuiltReleaseAssets,
) -> Result<(), String> {
    let mirror_url = format!(
        "{}/{}/{}.git",
        collapse_releases::API_HOST,
        job.owner,
        job.name
    );
    ensure_collapse_tag(
        mirror_url.clone(),
        job.commit.clone(),
        job.tag.clone(),
        false,
        Some(job.directory.clone()),
    )
    .await?;
    let title = format!("Velocity {}", job.version);
    let release = collapse_releases::ensure_release(
        &job.token,
        &job.owner,
        &job.name,
        &CreateReleaseRequest {
            tag: &job.tag,
            name: &title,
            notes: &built.notes,
            prerelease: is_prerelease_tag(&job.tag),
            draft: false,
            rolling: false,
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    upload_signed_release_files(&job.token, &job.owner, &job.name, &release, built).await?;
    let manifest_bytes = build_manifest_bytes(job, built)?;
    collapse_releases::upload_or_replace_asset(
        &job.token,
        &job.owner,
        &job.name,
        &release,
        "latest.json",
        "application/json",
        manifest_bytes.clone(),
    )
    .await
    .map_err(|error| error.to_string())?;
    update_rolling_updater_release(job, &mirror_url, manifest_bytes).await
}

async fn upload_signed_release_files(
    token: &str,
    owner: &str,
    name: &str,
    release: &CollapseRelease,
    built: &BuiltReleaseAssets,
) -> Result<(), String> {
    let mut uploads: Vec<(&Path, &str)> = vec![
        (&built.setup_exe, "application/octet-stream"),
        (&built.setup_sig, "text/plain; charset=utf-8"),
        (&built.portable_exe, "application/octet-stream"),
        (&built.portable_sig, "text/plain; charset=utf-8"),
        (&built.sha256sums, "text/plain; charset=utf-8"),
    ];
    if let (Some(msi), Some(msi_sig)) = (&built.msi, &built.msi_sig) {
        uploads.push((msi, "application/octet-stream"));
        uploads.push((msi_sig, "text/plain; charset=utf-8"));
    }
    if let (Some(archive), Some(checksum)) =
        (&built.graph_indexer_archive, &built.graph_indexer_sha256)
    {
        uploads.push((archive, "application/gzip"));
        uploads.push((checksum, "text/plain; charset=utf-8"));
    }
    for (path, mime) in uploads {
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("Invalid asset path {}", path.display()))?;
        let bytes = std::fs::read(path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        collapse_releases::upload_or_replace_asset(
            token, owner, name, release, file_name, mime, bytes,
        )
        .await
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn build_manifest_bytes(
    job: &CollapseReleaseJob,
    built: &BuiltReleaseAssets,
) -> Result<Vec<u8>, String> {
    let setup_sig = std::fs::read_to_string(&built.setup_sig)
        .map_err(|error| format!("Could not read setup signature: {error}"))?;
    let portable_sig = std::fs::read_to_string(&built.portable_sig)
        .map_err(|error| format!("Could not read portable signature: {error}"))?;
    let msi_sig = match &built.msi_sig {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .map_err(|error| format!("Could not read MSI signature: {error}"))?,
        ),
        None => None,
    };
    let pub_date = current_utc_rfc3339();
    let download_base = collapse_releases::tag_download_base(&job.owner, &job.name, &job.tag);
    let manifest = build_updater_manifest(
        &job.version,
        &built.notes,
        &pub_date,
        &download_base,
        &setup_sig,
        &portable_sig,
        msi_sig.as_deref(),
    );
    serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())
}

async fn update_rolling_updater_release(
    job: &CollapseReleaseJob,
    mirror_url: &str,
    manifest_bytes: Vec<u8>,
) -> Result<(), String> {
    ensure_collapse_tag(
        mirror_url.to_owned(),
        job.commit.clone(),
        UPDATER_TAG.to_owned(),
        true,
        Some(job.directory.clone()),
    )
    .await?;
    let updater = collapse_releases::ensure_release(
        &job.token,
        &job.owner,
        &job.name,
        &CreateReleaseRequest {
            tag: UPDATER_TAG,
            name: "Update manifest",
            notes: "Holds latest.json, which installed copies of Velocity read to find updates. Not a download.",
            prerelease: true,
            draft: false,
            rolling: true,
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    collapse_releases::upload_or_replace_asset(
        &job.token,
        &job.owner,
        &job.name,
        &updater,
        "latest.json",
        "application/json",
        manifest_bytes,
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn ensure_collapse_tag(
    mirror_url: String,
    commit: String,
    tag: String,
    allow_move: bool,
    local_repo: Option<PathBuf>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        velocity_collapse::ensure_remote_tag(
            &mirror_url,
            &commit,
            &tag,
            allow_move,
            local_repo.as_deref(),
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn is_velocity_project(project: &ReleaseProject) -> bool {
    project
        .collapse_repository
        .as_deref()
        .is_some_and(|repo| repo.eq_ignore_ascii_case(collapse_releases::VELOCITY_REPOSITORY))
        || project
            .github_repository
            .as_deref()
            .is_some_and(|repo| repo.eq_ignore_ascii_case(VELOCITY_REPOSITORY))
        || project
            .directory
            .join("crates/velocity-graph/Cargo.toml")
            .is_file()
}

fn current_utc_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format_utc_seconds(seconds)
}

fn toast(application: &AppHandle, message: String) -> Result<(), String> {
    let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
    layout.show_toast(message, None);
    crate::layout::publish(application, &layout.snapshot(), None)
}

fn release_project(application: &AppHandle, project: u32) -> Result<ReleaseProject, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let item = store.project(project).ok_or("Unknown project")?;
    Ok(ReleaseProject {
        landing: LandingProject {
            project: item.clone(),
            worktrees: store.worktree_preferences(item),
        },
        directory: PathBuf::from(&item.repository_path),
        collapse_repository: item.collapse_repository.clone(),
        github_repository: item.github_remote.clone(),
    })
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
        "Started integration build for {}; release {tag} will start automatically once it passes",
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
