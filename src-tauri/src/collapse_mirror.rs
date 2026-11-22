//! Collapse as each project's mirror (#427): provision the Collapse repository, hand its
//! push token to git's credential helper, point Rosen's `[mirror] remote` at it, and show
//! what Collapse reports about the repository's last land and downstream forwarder.
//!
//! The same `rosen.toml` change names the repository's Collapse root (#517), which is the
//! key Rosen's state feed, ledger push and action relay read. Without it Collapse receives
//! commits and nothing else. The relay runs only the kinds the server's operator allowed,
//! and Velocity owns the project's server, so linking allows what Collapse offers.
//!
//! Velocity keeps no push token: provisioning passes it to `git credential approve`, and
//! the account tokens stay in the OS credential store (§1, #430).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;
use velocity_collapse::{
    CollapseCredentials, CollapseError, DeletedPendingRepository, MirrorRequest,
    ProvisionDisposition, ProvisionedMirror, RepositoryActivity,
};
use velocity_core::collapse::{
    CollapseFeedHealth, CollapseMirrorHealth, CollapseRelayKind, DeletedCollapseRepository,
    DeletedRepositoryAction, ForwarderState, GitHubIssueSyncView, IssueHost, PushTokenStatus,
    allowed_settings_page, dashboard_poll_active, dashboard_poll_delay,
};
use velocity_core::project_health::short_commit;
use velocity_new_project::{
    NewProjectRequest, OpenProjectRequest, ProjectLocationRequest, ProjectMirror,
};
use velocity_rosen::collapse_link::{
    CollapseLink, RemoteBaseComparison, collapse_root, collapse_state, probe_remote_against_base,
    propose_collapse_channel, propose_collapse_link, propose_collapse_unlink,
    set_collapse_relay as apply_collapse_relay,
};
use velocity_rosen::landing::{LandOutcome, LandRefusal};
use velocity_rosen::workspace_settings::SETTINGS_AGENT;

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::lock;

pub(crate) const NOT_CONNECTED: &str = "Connect Collapse in Settings → Agents → Shared MCP servers first, so Velocity can create the repository";
const NOT_CONNECTED_OPEN: &str = "Connect Collapse in Settings → Agents → Shared MCP servers first, so Velocity can read and open your Collapse repositories";
const NO_PUSH_TOKEN: &str = "Git's credential helper holds no push token for this project's Collapse mirror. Connect Collapse in Settings → Agents → Shared MCP servers so Velocity can create one";
/// New projects start their Rosen base on `main` (F2).
const NEW_PROJECT_BRANCH: &str = "main";

pub(crate) async fn load_credentials() -> Result<Option<CollapseCredentials>, String> {
    tauri::async_runtime::spawn_blocking(velocity_collapse::load_credentials)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// Lists the connected Collapse account's repositories from Collapse's dashboard (#575).
pub(crate) async fn list_dashboard_repositories() -> Result<Vec<RepositoryActivity>, String> {
    let credentials = load_credentials().await?.ok_or(NOT_CONNECTED_OPEN)?;
    velocity_collapse::repository_activity(&credentials)
        .await
        .map_err(|error| error.to_string())
}

/// Prepares an `OpenProjectRequest::Collapse` before cloning (#575): validates the folder
/// and repository name, refuses repositories that already have a linked Rosen server
/// (offering machine enrollment instead), verifies the repository is ready, mints a push
/// token and configures proactive auth in git's credential helper.
pub(crate) async fn prepare_open_project(
    request: &mut OpenProjectRequest,
) -> Result<Option<String>, String> {
    let OpenProjectRequest::Collapse {
        repository,
        clone_to,
    } = request
    else {
        return Ok(None);
    };
    let (owner, name) = velocity_new_project::validate_open_collapse(repository, clone_to)
        .map_err(|error| error.to_string())?;
    let requested = format!("{owner}/{name}");
    let credentials = load_credentials().await?.ok_or(NOT_CONNECTED_OPEN)?;
    let rows = velocity_collapse::repository_activity(&credentials)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(row) = rows.iter().find(|row| row.is(&requested))
        && row.has_linked_server()
    {
        return Err(velocity_core::collapse::enrollment_notice(&row.full_name()));
    }
    let mirror = velocity_collapse::prepare_existing_mirror(&credentials, &owner, &name)
        .await
        .map_err(|error| error.to_string())?;
    let canonical = mirror.repository();
    *repository = canonical.clone();
    Ok(Some(canonical))
}

/// After cloning and importing an unlinked Collapse repository into Rosen (#575): ensures
/// `rosen.toml` names `[mirror] remote` and the `collapse` root (handling imported Git
/// repositories that already had a `rosen.toml`), applies the relay allow-list and
/// refreshes the saved mirror state.
pub(crate) async fn finalize_opened_collapse_project(
    application: &AppHandle,
    project: u32,
    repository: &str,
) -> Result<(), String> {
    let mirror_url = velocity_core::collapse::mirror_remote(repository)
        .ok_or("That is not a Collapse repository")?;
    let root = velocity_core::collapse::collapse_root(repository)
        .ok_or("That is not a Collapse repository")?;
    let server = crate::rosen::server(application, project).await?;
    let link = CollapseLink {
        remote: Some(&mirror_url),
        root: &root,
    };
    if let Some(change) = propose_collapse_link(&server, &link)
        .await
        .map_err(|error| error.to_string())?
    {
        let outcome = velocity_rosen::landing::land(&server, SETTINGS_AGENT, &change)
            .await
            .map_err(|error| error.to_string())?;
        if let LandOutcome::Refused(refusal) = outcome {
            return Err(format!(
                "Could not land Collapse mirror configuration ({change}): {}",
                refusal_reason(refusal)
            ));
        }
    }
    allow_relay_after_create(application, project).await;
    crate::rosen_mirror::refresh_for_owner(application, project).await?;
    Ok(())
}

async fn ensure_proactive_auth() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(velocity_collapse::ensure_proactive_auth)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// Downstream configuration result carried from `prepare_new_project` until the project is
/// registered and its Rosen server is running (#579).
pub(crate) struct PreparedCollapseDownstream {
    collapse_repository: String,
    result: DownstreamResult,
}

pub(crate) enum DownstreamResult {
    Configured {
        github_repository: String,
    },
    NeedsFinish {
        github_repository: String,
        page_url: Option<String>,
        reason: String,
        open_setup_page: bool,
    },
}

/// Create with a Collapse mirror: provisions the repository before the initial import,
/// which pushes the new base at once. A blank name uses the project folder's name, and
/// the request then names the provisioned `owner/repository`. When an optional GitHub
/// repository is given, configures Collapse's downstream remote via `POST /api/downstream`
/// (#579). Without a connection an existing `owner/repository` is still accepted, for push
/// tokens stored by hand.
pub(crate) async fn prepare_new_project(
    request: &mut NewProjectRequest,
) -> Result<Option<PreparedCollapseDownstream>, String> {
    let ProjectLocationRequest::RosenFolder {
        path,
        mirror:
            ProjectMirror::Collapse {
                repository,
                github_repository,
            },
    } = &mut request.location
    else {
        return Ok(None);
    };
    let folder = Path::new(path.trim());
    // Refuse a folder or GitHub repository Create would refuse before anything exists on Collapse.
    velocity_new_project::validate_project_root(folder).map_err(|error| error.to_string())?;
    let parsed_github = match github_repository.as_deref() {
        Some(github) => velocity_new_project::parse_github_repository(github)
            .map_err(|error| error.to_string())?
            .map(|name| name.full_name()),
        None => None,
    };
    let typed = typed_repository(repository);
    let Some(credentials) = load_credentials().await? else {
        if !typed.contains('/') {
            return Err(NOT_CONNECTED.into());
        }
        ensure_proactive_auth().await?;
        share_hand_stored_token(&typed).await;
        return Ok(None);
    };
    let (owner, name) = match typed.split_once('/') {
        Some((owner, name)) => (Some(owner.to_owned()), name.to_owned()),
        None if typed.is_empty() => (None, folder_repository_name(folder)?),
        None => (None, typed.clone()),
    };
    let provisioned = velocity_collapse::provision_mirror(
        &credentials,
        &MirrorRequest {
            owner: owner.as_deref(),
            name: &name,
            default_branch: NEW_PROJECT_BRANCH,
            description: "",
        },
    )
    .await
    .map_err(|error| match error {
        CollapseError::DeletedPending(deleted) => {
            let view = deleted_repository_view(*deleted);
            format!(
                "{}. Restore or manage it at {}, or choose another repository name.",
                view.notice(),
                view.settings_url
            )
        }
        other => other.to_string(),
    })?;
    let collapse_repository = provisioned.repository();
    *repository = collapse_repository.clone();
    let Some(github) = parsed_github else {
        return Ok(None);
    };
    let result =
        configure_downstream(&credentials, &provisioned.owner, &provisioned.name, &github).await;
    Ok(Some(PreparedCollapseDownstream {
        collapse_repository,
        result,
    }))
}

/// Completes Collapse downstream follow-up after `create_project` registers the project (#579):
/// opens the Collapse GitHub App install/permission page and publishes the project health row
/// when automatic configuration needed owner approval.
pub(crate) async fn finish_new_project_downstream(
    application: &AppHandle,
    project: u32,
    prepared: PreparedCollapseDownstream,
) {
    if let Err(error) = finish_downstream_setup(
        application,
        project,
        &prepared.collapse_repository,
        Some(prepared.result),
    ) {
        crate::diagnostics::record(
            "collapse mirror",
            &format!(
                "{}: could not open Collapse GitHub App setup page: {error}",
                prepared.collapse_repository
            ),
        );
    }
    refresh_soon(application);
}

/// The new project's `rosen.toml` names the Collapse root (#517), and Rosen's feed asks
/// the helper for the push token under its own endpoints. A token stored by hand for the
/// mirror is copied there. Without one the Rosen status pane shows the feed failing and
/// offers Send Collapse its live state, so nothing here stops Create.
async fn share_hand_stored_token(repository: &str) {
    let (Some(remote), Some(root)) = (
        velocity_core::collapse::mirror_remote(repository),
        velocity_core::collapse::collapse_root(repository),
    ) else {
        return;
    };
    if let Err(error) = velocity_collapse::share_push_token(&remote, &root).await {
        crate::diagnostics::record(
            "collapse mirror",
            &format!("{repository}: could not share the push token with the state feed: {error}"),
        );
    }
}

/// The dialog's text without Collapse's git host or `.git`, as `owner/name`, `name` or empty.
pub(crate) fn typed_repository(repository: &str) -> String {
    let trimmed = repository.trim().trim_end_matches('/');
    let trimmed = trimmed
        .strip_prefix("https://api.collapse.sh/")
        .or_else(|| trimmed.strip_prefix("https://collapse.sh/"))
        .unwrap_or(trimmed);
    trimmed.strip_suffix(".git").unwrap_or(trimmed).to_owned()
}

fn folder_repository_name(folder: &Path) -> Result<String, String> {
    folder
        .file_name()
        .and_then(|name| velocity_core::collapse::repository_name_for(&name.to_string_lossy()))
        .ok_or_else(|| {
            "Name the Collapse repository: the folder's name has no letters or digits".into()
        })
}

/// What Mirror to Collapse and Send Collapse its live state need from the saved project.
pub(crate) struct MirrorSubject {
    pub(crate) name: String,
    pub(crate) branch: String,
    pub(crate) repository_path: PathBuf,
    pub(crate) github_remote: Option<String>,
    /// The Collapse repository the project already mirrors to.
    pub(crate) repository: Option<String>,
}

pub(crate) fn mirror_subject(
    application: &AppHandle,
    project: u32,
) -> Result<MirrorSubject, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let item = store.project(project).ok_or("Unknown project")?;
    if item.version_control != velocity_store::VersionControl::Rosen {
        return Err("Mirror to Collapse is available after the project migrates to Rosen".into());
    }
    Ok(MirrorSubject {
        name: item.name.clone(),
        branch: item.default_branch.clone(),
        repository_path: PathBuf::from(&item.repository_path),
        github_remote: item.github_remote.clone(),
        repository: item.collapse_repository.clone(),
    })
}

/// Keeps `github_remote` pointing at GitHub when Mirror to Collapse moves an existing
/// GitHub project, capturing the previous GitHub mirror or checkout origin if unset (#574),
/// and returns the GitHub `owner/repository` when present (#579).
async fn preserve_github_remote(
    application: &AppHandle,
    project: u32,
    subject: &MirrorSubject,
    server: &velocity_rosen::ProjectServer,
) -> Result<Option<String>, String> {
    if let Some(saved) = subject.github_remote.as_deref() {
        let normalized = velocity_core::collapse::github_repository_from_remote(saved)
            .unwrap_or_else(|| saved.to_owned());
        return Ok(Some(normalized));
    }
    let from_mirror = velocity_rosen::mirror_remote(server)
        .await
        .ok()
        .flatten()
        .and_then(|remote| velocity_core::collapse::github_repository_from_remote(&remote));
    let repository = match from_mirror {
        Some(repository) => Some(repository),
        None => {
            let path = subject.repository_path.clone();
            tauri::async_runtime::spawn_blocking(move || {
                velocity_github::checkout_repository(&path).map(|name| name.full_name())
            })
            .await
            .map_err(|error| error.to_string())?
        }
    };
    let Some(repository) = repository else {
        return Ok(None);
    };
    let saved = repository.clone();
    crate::state::try_with_store(application, move |store| {
        store
            .save_github_remote(project, Some(saved))
            .map_err(|error| error.to_string())
    })
    .await?;
    Ok(Some(repository))
}

/// Project settings view for the Collapse section (#576, #589, #592).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, specta::Type)]
pub struct ProjectCollapseSettings {
    pub project_identifier: u32,
    pub collapse_repository: Option<String>,
    pub collapse_root: Option<String>,
    pub collapse_channel: bool,
    pub config_error: Option<String>,
    pub push_token: PushTokenStatus,
    pub state_feed: CollapseFeedHealth,
    pub ledger_push: CollapseFeedHealth,
    pub relay_allow: Vec<CollapseRelayKind>,
    pub server_relay_allow: Vec<CollapseRelayKind>,
    pub relay_revoked: bool,
    /// Effective issue host (`github` or `collapse`) for this project (#589).
    pub issue_host: IssueHost,
    /// Whether a dedicated Collapse PAT with `issues` scope is stored in Credential Manager (#589).
    pub collapse_issues_token_stored: bool,
    /// Whether a Collapse account is connected on this machine (#589).
    pub collapse_connected: bool,
    /// Saved GitHub `owner/repository` for this project, when configured (#592).
    pub github_repository: Option<String>,
    /// Downstream GitHub `owner/repository` last reported by Collapse, when configured (#592).
    pub downstream_github: Option<String>,
    /// Status of post-switch one-way sync of newly opened GitHub issues into Collapse (#612).
    pub github_issue_sync: Option<GitHubIssueSyncView>,
    /// Whether Collapse's GitHub issue import has already finished for this project (#615).
    pub issue_import_finished: bool,
    /// RFC 3339 timestamp of the completed GitHub issue import, when known (#615).
    pub issue_import_finished_at: Option<String>,
}

/// Result of a Collapse settings action that lands a `rosen.toml` change (#576).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, specta::Type)]
pub struct CollapseActionResult {
    pub settings: ProjectCollapseSettings,
    pub message: String,
}

/// `get_project_collapse_settings` (§3.1): reads the project's Collapse root, push token
/// status, state feed and ledger push health, channel lines flag and relay allow-list (#576).
/// Returns `None` for non-Rosen projects.
#[tauri::command]
#[specta::specta]
pub async fn get_project_collapse_settings(
    application: AppHandle,
    project_identifier: u32,
) -> Result<Option<ProjectCollapseSettings>, String> {
    if !uses_rosen(&application, project_identifier).await? {
        return Ok(None);
    }
    read_project_collapse_settings(&application, project_identifier)
        .await
        .map(Some)
}

/// `set_collapse_channel` (§3.1): toggles `collapse_channel` in `rosen.toml` through a
/// landed change so Rosen sends or stops sending channel lines to Collapse (#576).
#[tauri::command]
#[specta::specta]
pub async fn set_collapse_channel(
    application: AppHandle,
    project_identifier: u32,
    enabled: bool,
) -> Result<CollapseActionResult, String> {
    let subject = mirror_subject(&application, project_identifier)?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    crate::rosen_mirror::check_and_catch_up(&application, project_identifier).await;
    let change = propose_collapse_channel(&server, enabled)
        .await
        .map_err(|error| error.to_string())?;
    let message = match change {
        None if enabled => format!("{}: channel lines already go to Collapse", subject.name),
        None => format!("{}: channel lines are already off", subject.name),
        Some(change) => {
            let outcome = velocity_rosen::landing::land(&server, SETTINGS_AGENT, &change)
                .await
                .map_err(|error| error.to_string())?;
            match outcome {
                LandOutcome::Landed { .. } if enabled => {
                    format!("{}: channel lines now go to Collapse", subject.name)
                }
                LandOutcome::Landed { .. } => {
                    format!("{}: channel lines stopped going to Collapse", subject.name)
                }
                LandOutcome::Refused(refusal) => format!(
                    "{}: change {change} waits to land: {}",
                    subject.name,
                    refusal_reason(refusal)
                ),
            }
        }
    };
    toast(&application, message.clone())?;
    let settings = read_project_collapse_settings(&application, project_identifier).await?;
    Ok(CollapseActionResult { settings, message })
}

/// `set_collapse_relay` (§3.1): saves the allowed Collapse relay action kinds for the
/// project and applies them to the project's Rosen server immediately (#576).
#[tauri::command]
#[specta::specta]
pub async fn set_collapse_relay(
    application: AppHandle,
    project_identifier: u32,
    allow: Vec<CollapseRelayKind>,
) -> Result<ProjectCollapseSettings, String> {
    let _subject = mirror_subject(&application, project_identifier)?;
    let normalized = CollapseRelayKind::normalize(allow);
    let server = crate::rosen::server(&application, project_identifier).await?;
    apply_collapse_relay(&server, &normalized)
        .await
        .map_err(|error| error.to_string())?;
    let saved = normalized.clone();
    crate::state::try_with_store(&application, move |store| {
        store
            .save_collapse_relay_allow(project_identifier, Some(saved))
            .map_err(|error| error.to_string())
    })
    .await?;
    read_project_collapse_settings(&application, project_identifier).await
}

/// `set_project_issue_host` (§3.1): selects GitHub or Collapse as the project's issue host (#589).
#[tauri::command]
#[specta::specta]
pub async fn set_project_issue_host(
    application: AppHandle,
    project_identifier: u32,
    host: IssueHost,
) -> Result<ProjectCollapseSettings, String> {
    let subject = mirror_subject(&application, project_identifier)?;
    if host == IssueHost::Collapse && subject.repository.is_none() {
        return Err("Mirror this project to Collapse before moving its issues to Collapse".into());
    }
    crate::state::try_with_store(&application, move |store| {
        store
            .save_issue_host(project_identifier, host)
            .map_err(|error| error.to_string())
    })
    .await?;
    match host {
        IssueHost::Collapse => {
            crate::project_health::recheck_collapse_import_soon(&application, project_identifier);
            if let Some(repo) = subject.repository.as_deref() {
                let _ = crate::collapse_issue_sync::enable_sync_from_import_status(
                    &application,
                    project_identifier,
                    &subject.repository_path,
                    repo,
                    true,
                )
                .await;
            }
        }
        IssueHost::GitHub => {
            crate::project_health::clear_collapse_issues_not_imported(
                &application,
                project_identifier,
            );
        }
    }
    crate::project_health::sync_host_source_change(&application, project_identifier);
    crate::project_health::sync_host_issue_problems(&application, project_identifier);
    crate::base_ci::wake(&application);
    read_project_collapse_settings(&application, project_identifier).await
}

/// `set_collapse_issues_token` (§3.1): saves a Collapse personal access token with `issues`
/// scope in Windows Credential Manager (`velocity:collapse.sh:issues`) (#589).
#[tauri::command]
#[specta::specta]
pub async fn set_collapse_issues_token(
    application: AppHandle,
    project_identifier: u32,
    token: String,
) -> Result<ProjectCollapseSettings, String> {
    tauri::async_runtime::spawn_blocking(move || velocity_collapse::save_issues_token(&token))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    read_project_collapse_settings(&application, project_identifier).await
}

/// `clear_collapse_issues_token` (§3.1): removes the saved Collapse `issues` token from
/// Windows Credential Manager (#589).
#[tauri::command]
#[specta::specta]
pub async fn clear_collapse_issues_token(
    application: AppHandle,
    project_identifier: u32,
) -> Result<ProjectCollapseSettings, String> {
    tauri::async_runtime::spawn_blocking(velocity_collapse::forget_issues_token)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    read_project_collapse_settings(&application, project_identifier).await
}

/// `unlink_collapse` (§3.1): removes the top-level `collapse` root (and `collapse_channel`)
/// through a landed `rosen.toml` change and clears the server's relay allow-list (#576).
#[tauri::command]
#[specta::specta]
pub async fn unlink_collapse(
    application: AppHandle,
    project_identifier: u32,
) -> Result<CollapseActionResult, String> {
    let subject = mirror_subject(&application, project_identifier)?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    crate::rosen_mirror::check_and_catch_up(&application, project_identifier).await;
    let change = propose_collapse_unlink(&server)
        .await
        .map_err(|error| error.to_string())?;
    let (landed, mut message) = match change {
        None => (
            true,
            format!("{}: rosen.toml already has no Collapse root", subject.name),
        ),
        Some(change) => {
            let outcome = velocity_rosen::landing::land(&server, SETTINGS_AGENT, &change)
                .await
                .map_err(|error| error.to_string())?;
            match outcome {
                LandOutcome::Landed { .. } => (
                    true,
                    format!("{}: unlinked Collapse live state", subject.name),
                ),
                LandOutcome::Refused(refusal) => (
                    false,
                    format!(
                        "{}: unlink change {change} waits to land: {}",
                        subject.name,
                        refusal_reason(refusal)
                    ),
                ),
            }
        }
    };
    let cleared = apply_collapse_relay(&server, &[]).await;
    match cleared {
        Ok(_) => message.push_str(" · Collapse relay cleared"),
        Err(error) => {
            crate::diagnostics::record(
                "collapse relay",
                &format!("{}: could not clear Collapse relay: {error}", subject.name),
            );
            message.push_str(&format!(" · could not clear Collapse relay: {error}"));
        }
    }
    let next_allow = (!landed).then(Vec::new);
    crate::state::try_with_store(&application, move |store| {
        store
            .save_collapse_relay_allow(project_identifier, next_allow)
            .map_err(|error| error.to_string())
    })
    .await?;
    toast(&application, message.clone())?;
    crate::rosen_mirror::refresh_for_owner(&application, project_identifier).await?;
    let settings = read_project_collapse_settings(&application, project_identifier).await?;
    Ok(CollapseActionResult { settings, message })
}

async fn uses_rosen(application: &AppHandle, project: u32) -> Result<bool, String> {
    crate::state::try_with_store(application, move |store| {
        let item = store.project(project).ok_or("Unknown project")?;
        Ok(item.version_control == velocity_store::VersionControl::Rosen)
    })
    .await
}

pub(crate) async fn read_project_collapse_settings(
    application: &AppHandle,
    project: u32,
) -> Result<ProjectCollapseSettings, String> {
    let (
        collapse_repository,
        saved_override,
        effective_allow,
        issue_host,
        github_repository,
        downstream_github,
        github_issue_sync,
    ) = crate::state::try_with_store(application, move |store| {
        let item = store.project(project).ok_or("Unknown project")?;
        Ok((
            item.collapse_repository.clone(),
            item.collapse_relay_allow.clone(),
            store.collapse_relay_allow(project),
            item.effective_issue_host(),
            item.github_remote.clone(),
            item.collapse_mirror_health
                .as_ref()
                .and_then(|health| health.downstream_github.clone()),
            item.github_issue_sync
                .as_ref()
                .map(velocity_core::collapse::GitHubIssueSync::to_view),
        ))
    })
    .await?;
    let (collapse_issues_token_stored, collapse_connected) =
        tauri::async_runtime::spawn_blocking(|| {
            (
                velocity_collapse::load_issues_token()
                    .ok()
                    .flatten()
                    .is_some(),
                velocity_collapse::load_credentials()
                    .ok()
                    .flatten()
                    .is_some(),
            )
        })
        .await
        .unwrap_or((false, false));
    let server = crate::rosen::server(application, project).await?;
    let state = collapse_state(&server)
        .await
        .map_err(|error| error.to_string())?;
    let token_root = state.collapse_root.clone().or_else(|| {
        collapse_repository
            .as_deref()
            .and_then(velocity_core::collapse::collapse_root)
    });
    let push_token = match token_root {
        None => PushTokenStatus::NotConfigured,
        Some(root) => match velocity_collapse::check_push_token(&root).await {
            Ok((true, true)) => PushTokenStatus::Present,
            Ok((false, false)) => PushTokenStatus::Missing,
            Ok((state_feed, ledger_push)) => PushTokenStatus::Partial {
                state_feed,
                ledger_push,
            },
            Err(error) => PushTokenStatus::Unavailable {
                error: error.to_string(),
            },
        },
    };
    let relay_allow = if state.collapse_root.is_some() {
        effective_allow
    } else {
        saved_override.unwrap_or_else(|| state.relay_allow.clone())
    };
    let issue_import_finished_at = github_issue_sync
        .as_ref()
        .and_then(|sync| sync.import_finished_at.clone())
        .filter(|value| !value.trim().is_empty());
    let issue_import_finished = issue_import_finished_at.is_some()
        || collapse_repository.as_deref().is_some_and(|repo| {
            crate::project_health::collapse_import_finished(application, project, repo)
        });
    Ok(ProjectCollapseSettings {
        project_identifier: project,
        collapse_repository,
        collapse_root: state.collapse_root,
        collapse_channel: state.collapse_channel,
        config_error: state.config_error,
        push_token,
        state_feed: state.state_feed,
        ledger_push: state.ledger_push,
        relay_allow,
        server_relay_allow: state.relay_allow,
        relay_revoked: state.relay_revoked,
        issue_host,
        collapse_issues_token_stored,
        collapse_connected,
        github_repository,
        downstream_github,
        github_issue_sync,
        issue_import_finished,
        issue_import_finished_at,
    })
}

/// Outcome of `mirror_to_collapse` (§3.1, #427, #610).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, specta::Type)]
pub struct MirrorToCollapseResult {
    pub ok: bool,
    pub message: String,
    /// Populated when the target Collapse name is held by a deleted copy pending purge (#610).
    #[serde(default)]
    pub deleted_repository: Option<DeletedCollapseRepository>,
}

pub(crate) fn deleted_repository_view(
    deleted: DeletedPendingRepository,
) -> DeletedCollapseRepository {
    let repository = deleted.repository();
    let settings_url = allowed_settings_page(&repository, deleted.settings_url.as_deref())
        .unwrap_or_else(|| format!("https://collapse.sh/{repository}/settings"));
    DeletedCollapseRepository {
        repository,
        owner: deleted.owner,
        name: deleted.name,
        delete_after: deleted.delete_after,
        deleted_at: deleted.deleted_at,
        confirmed: deleted.confirmed,
        purge_available: deleted.purge_available,
        settings_url,
    }
}

/// `mirror_to_collapse` (§3.1): makes Collapse the project's mirror. Creates the Collapse
/// repository named after the project, stores its push token in git's credential helper,
/// and lands a `rosen.toml` change pointing `[mirror] remote` at it and naming its
/// `collapse` root, so Rosen also sends its live state and ledger entries and runs the
/// owner's actions from Collapse (#517). A project that was on GitHub keeps its
/// `github_remote` pointing at GitHub (#574) and configures Collapse's downstream remote
/// via `POST /api/downstream` (C4, #579), opening the Collapse GitHub App's install or
/// permission page and showing a project health row if the App is missing or lacks write
/// permissions. When the target name is held by a deleted copy pending purge (#610),
/// returns `deleted_repository` so the caller can offer Restore and link, Purge now and
/// create, or the repository settings page.
#[tauri::command]
#[specta::specta]
pub async fn mirror_to_collapse(
    application: AppHandle,
    project_identifier: u32,
    deleted_action: Option<DeletedRepositoryAction>,
) -> Result<MirrorToCollapseResult, String> {
    let subject = mirror_subject(&application, project_identifier)?;
    if let Some(repository) = &subject.repository {
        return Err(format!(
            "{} already mirrors to Collapse ({repository})",
            subject.name
        ));
    }
    let credentials = load_credentials().await?.ok_or(NOT_CONNECTED)?;
    let name = velocity_core::collapse::repository_name_for(&subject.name)
        .ok_or("The project's name has no letters or digits to name a Collapse repository")?;
    let provisioned =
        match provision_for_mirror(&credentials, &name, &subject.branch, deleted_action).await {
            Ok(provisioned) => provisioned,
            Err(CollapseError::DeletedPending(deleted)) => {
                let view = deleted_repository_view(*deleted);
                let message = view.notice();
                return Ok(MirrorToCollapseResult {
                    ok: false,
                    message,
                    deleted_repository: Some(view),
                });
            }
            Err(error) => return Err(error.to_string()),
        };
    let server = crate::rosen::server(&application, project_identifier).await?;
    if provisioned.disposition == ProvisionDisposition::Restored {
        ensure_restored_fast_forwardable(&server, &provisioned).await?;
    }
    let github_repository =
        preserve_github_remote(&application, project_identifier, &subject, &server).await?;
    let downstream = match github_repository.as_deref() {
        Some(github) => Some(
            configure_downstream(&credentials, &provisioned.owner, &provisioned.name, github).await,
        ),
        None => None,
    };
    crate::rosen_mirror::check_and_catch_up(&application, project_identifier).await;
    let root = provisioned.collapse_root();
    let link = CollapseLink {
        remote: Some(&provisioned.mirror_url),
        root: &root,
    };
    let collapse_repository = provisioned.repository();
    let mut message = land_link(
        &application,
        project_identifier,
        &server,
        &subject.name,
        &collapse_repository,
        &link,
        provisioned.disposition,
    )
    .await?;
    message.push_str(&downstream_note(downstream.as_ref()));
    toast(&application, message.clone())?;
    crate::rosen_mirror::refresh_for_owner(&application, project_identifier).await?;
    finish_downstream_setup(
        &application,
        project_identifier,
        &collapse_repository,
        downstream,
    )?;
    refresh_health(&application).await?;
    Ok(MirrorToCollapseResult {
        ok: true,
        message,
        deleted_repository: None,
    })
}

async fn provision_for_mirror(
    credentials: &CollapseCredentials,
    name: &str,
    branch: &str,
    deleted_action: Option<DeletedRepositoryAction>,
) -> Result<ProvisionedMirror, CollapseError> {
    let request = MirrorRequest {
        owner: None,
        name,
        default_branch: branch,
        description: "",
    };
    match deleted_action {
        None => velocity_collapse::provision_mirror(credentials, &request).await,
        Some(DeletedRepositoryAction::Restore) => {
            velocity_collapse::restore_mirror(credentials, &credentials.account, name).await
        }
        Some(DeletedRepositoryAction::PurgeAndCreate) => {
            velocity_collapse::purge_and_create_mirror(credentials, &request).await
        }
    }
}

async fn ensure_restored_fast_forwardable(
    server: &velocity_rosen::ProjectServer,
    provisioned: &ProvisionedMirror,
) -> Result<(), String> {
    let comparison = probe_remote_against_base(server, &provisioned.mirror_url)
        .await
        .map_err(|error| error.to_string())?;
    if let RemoteBaseComparison::NotAncestor {
        branch,
        base_commit,
        remote_tip,
    } = comparison
    {
        return Err(format!(
            "Restored Collapse repository {} ({branch} at {}) has commits not in Rosen's base ({}) and cannot be fast-forwarded. Reconcile the Collapse branch or purge it first.",
            provisioned.repository(),
            short_commit(&remote_tip),
            short_commit(&base_commit),
        ));
    }
    Ok(())
}

pub(crate) async fn configure_downstream(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
    github_repository: &str,
) -> DownstreamResult {
    let collapse_repository = format!("{owner}/{name}");
    match velocity_collapse::set_downstream(credentials, owner, name, github_repository).await {
        Ok(velocity_collapse::DownstreamSetup::Configured) => DownstreamResult::Configured {
            github_repository: github_repository.to_owned(),
        },
        Ok(velocity_collapse::DownstreamSetup::NeedsGitHubApp { page_url, reason }) => {
            let resolved = velocity_core::collapse::downstream_setup_page(
                &collapse_repository,
                page_url.as_deref(),
            );
            DownstreamResult::NeedsFinish {
                github_repository: github_repository.to_owned(),
                page_url: resolved,
                reason,
                open_setup_page: true,
            }
        }
        Err(error) => {
            let reason = error.to_string();
            crate::diagnostics::record(
                "collapse mirror",
                &format!(
                    "{collapse_repository}: could not set Collapse downstream to {github_repository}: {reason}"
                ),
            );
            DownstreamResult::NeedsFinish {
                github_repository: github_repository.to_owned(),
                page_url: velocity_core::collapse::downstream_settings_page(&collapse_repository),
                reason,
                open_setup_page: false,
            }
        }
    }
}

fn downstream_note(downstream: Option<&DownstreamResult>) -> String {
    match downstream {
        None => String::new(),
        Some(DownstreamResult::Configured { github_repository }) => {
            format!(" · forwarding to {github_repository}")
        }
        Some(DownstreamResult::NeedsFinish { .. }) => {
            " · finish Collapse forwarding to GitHub".into()
        }
    }
}

pub(crate) fn finish_downstream_setup(
    application: &AppHandle,
    project: u32,
    collapse_repository: &str,
    downstream: Option<DownstreamResult>,
) -> Result<(), String> {
    match downstream {
        None => Ok(()),
        Some(DownstreamResult::Configured { .. }) => {
            crate::project_health::clear_collapse_forwarding(application, project);
            Ok(())
        }
        Some(DownstreamResult::NeedsFinish {
            github_repository,
            page_url,
            reason,
            open_setup_page,
        }) => {
            let open_result = match page_url.filter(|_| open_setup_page) {
                Some(page) => application
                    .opener()
                    .open_url(page, None::<&str>)
                    .map(|()| true),
                None => Ok(false),
            };
            let opened = open_result.as_ref().copied().unwrap_or(false);
            crate::project_health::record_collapse_forwarding_needed(
                application,
                project,
                collapse_repository,
                &github_repository,
                &reason,
                opened,
            );
            open_result.map(|_| ()).map_err(|error| error.to_string())
        }
    }
}

/// `send_collapse_live_state` (§3.1): for a project that already mirrors to Collapse
/// without the `collapse` root (#517). Gives Rosen's state feed, ledger push and relay the
/// mirror's push token under their own endpoints, lands a `rosen.toml` change naming just
/// the root, and lets the relay run. A project whose root is already landed keeps its
/// file and gets the token and the relay, which mends a feed Collapse answers with 401.
#[tauri::command]
#[specta::specta]
pub async fn send_collapse_live_state(
    application: AppHandle,
    project_identifier: u32,
) -> Result<(), String> {
    let subject = mirror_subject(&application, project_identifier)?;
    let repository = subject.repository.clone().ok_or_else(|| {
        format!(
            "{} has no Collapse mirror; use Mirror to Collapse first",
            subject.name
        )
    })?;
    let root = velocity_core::collapse::collapse_root(&repository)
        .ok_or("The project's Collapse mirror does not name an owner and repository")?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    crate::rosen_mirror::check_and_catch_up(&application, project_identifier).await;
    share_push_token(&server, &subject, &repository, &root).await?;
    let link = CollapseLink {
        remote: None,
        root: &root,
    };
    let message = land_link(
        &application,
        project_identifier,
        &server,
        &subject.name,
        &repository,
        &link,
        ProvisionDisposition::Reused,
    )
    .await?;
    toast(&application, message)?;
    crate::rosen_mirror::refresh_for_owner(&application, project_identifier).await?;
    refresh_health(&application).await
}

/// Rosen asks git's credential helper for the push token under the feed's own endpoints,
/// where a token stored for the mirror URL is not found. Copies the mirror's token there,
/// or mints one when the helper holds none and Collapse is connected.
async fn share_push_token(
    server: &velocity_rosen::ProjectServer,
    subject: &MirrorSubject,
    repository: &str,
    root: &str,
) -> Result<(), String> {
    let remote = velocity_rosen::mirror_remote(server)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(remote) = remote
        && velocity_collapse::share_push_token(&remote, root)
            .await
            .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    let credentials = load_credentials().await?.ok_or(NO_PUSH_TOKEN)?;
    let (owner, name) = repository
        .split_once('/')
        .ok_or("Unknown Collapse mirror")?;
    velocity_collapse::provision_mirror(
        &credentials,
        &MirrorRequest {
            owner: Some(owner),
            name,
            default_branch: &subject.branch,
            description: "",
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Proposes and lands the `rosen.toml` link, then applies the project's relay allow-list;
/// the toast says where it stands.
#[allow(clippy::too_many_arguments)]
async fn land_link(
    application: &AppHandle,
    project_identifier: u32,
    server: &velocity_rosen::ProjectServer,
    project: &str,
    repository: &str,
    link: &CollapseLink<'_>,
    disposition: ProvisionDisposition,
) -> Result<String, String> {
    let change = propose_collapse_link(server, link)
        .await
        .map_err(|error| error.to_string())?;
    let mirroring = link.remote.is_some();
    let message = match change {
        None if mirroring => format!("{project}: rosen.toml already mirrors to {repository}"),
        None => format!("{project}: rosen.toml already sends {repository} its live state"),
        Some(change) => {
            let outcome = velocity_rosen::landing::land(server, SETTINGS_AGENT, &change)
                .await
                .map_err(|error| error.to_string())?;
            match outcome {
                LandOutcome::Landed { .. } if mirroring => match disposition {
                    ProvisionDisposition::Restored => {
                        format!(
                            "{project}: restored {repository} and linked lands and live state to Collapse"
                        )
                    }
                    ProvisionDisposition::PurgedAndCreated => {
                        format!(
                            "{project}: purged deleted {repository}, created a fresh copy, and linked lands and live state to Collapse"
                        )
                    }
                    ProvisionDisposition::Created | ProvisionDisposition::Reused => {
                        format!("{project}: lands and live state now go to Collapse · {repository}")
                    }
                },
                LandOutcome::Landed { .. } => {
                    format!("{project}: Collapse now receives live state · {repository}")
                }
                LandOutcome::Refused(refusal) => format!(
                    "{project}: {repository} is ready · change {change} waits to land: {}",
                    refusal_reason(refusal)
                ),
            }
        }
    };
    let allow = saved_relay_allow(application, project_identifier).await;
    Ok(format!(
        "{message}{}",
        relay_note(server, project, &allow).await
    ))
}

pub(crate) fn refusal_reason(refusal: LandRefusal) -> String {
    match refusal {
        LandRefusal::Regenerate { paths } => format!("regenerate {}", paths.join(", ")),
        LandRefusal::Conflict { message, .. } | LandRefusal::Other { message, .. } => message,
    }
}

pub(crate) async fn saved_relay_allow(
    application: &AppHandle,
    project: u32,
) -> Vec<CollapseRelayKind> {
    crate::state::with_store(application, move |store| {
        store.collapse_relay_allow(project)
    })
    .await
    .unwrap_or_else(|_| CollapseRelayKind::ALL.to_vec())
}

/// Applies the project's relay allow-list to its server and says so in the toast.
/// A failure leaves the mirror and the feed working, so it is reported, not returned.
pub(crate) async fn relay_note(
    server: &velocity_rosen::ProjectServer,
    project: &str,
    allow: &[CollapseRelayKind],
) -> String {
    match apply_collapse_relay(server, allow).await {
        Ok(kinds) if kinds.is_empty() => " · Collapse's owner actions stay off".into(),
        Ok(kinds) => {
            let labels: Vec<&str> = kinds.into_iter().map(CollapseRelayKind::as_str).collect();
            format!(
                " · Collapse's owner actions run here ({})",
                labels.join(", ")
            )
        }
        Err(error) => {
            crate::diagnostics::record(
                "collapse relay",
                &format!("{project}: could not set Collapse's actions: {error}"),
            );
            format!(" · Collapse's actions stay off: {error}")
        }
    }
}

/// Re-applies the project's Collapse relay allow-list whenever Velocity starts a server
/// whose base `rosen.toml` names `collapse` (#576). Returns `true` when synced or unlinked.
pub(crate) async fn sync_relay_on_start(
    application: &AppHandle,
    project: u32,
    server: &velocity_rosen::ProjectServer,
) -> bool {
    let root = match collapse_root(server).await {
        Ok(root) => root,
        Err(error) => {
            crate::diagnostics::record(
                "collapse relay",
                &format!("Project {project}: could not read rosen.toml collapse root: {error}"),
            );
            return false;
        }
    };
    if root.is_none() {
        return true;
    }
    let allow = saved_relay_allow(application, project).await;
    if let Err(error) = apply_collapse_relay(server, &allow).await {
        crate::diagnostics::record(
            "collapse relay",
            &format!("Project {project}: could not re-apply Collapse relay allow-list: {error}"),
        );
        return false;
    }
    true
}

/// Create or Open from Collapse wrote the `collapse` root in `rosen.toml`, so the
/// project's server applies its relay allow-list (#517, #575, #576).
pub(crate) async fn allow_relay_after_create(application: &AppHandle, project: u32) {
    match crate::rosen::server(application, project).await {
        Ok(server) => {
            let _ = sync_relay_on_start(application, project, &server).await;
        }
        Err(error) => {
            crate::diagnostics::record(
                "collapse relay",
                &format!("Project {project}: could not allow Collapse's actions: {error}"),
            );
        }
    }
}

pub(crate) fn toast(application: &AppHandle, message: String) -> Result<(), String> {
    let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
    layout.show_toast(message, None);
    crate::layout::publish(application, &layout.snapshot(), None)
}

#[derive(Default)]
struct DashboardPoller {
    last_land: Mutex<Option<Instant>>,
    wake: tokio::sync::Notify,
}

/// Signals when the startup Collapse issue-host backfill has finished so dispatch and the
/// issue panel do not race ahead with a stale GitHub host (#613).
#[derive(Default)]
struct IssueHostBackfillGate {
    done: Mutex<bool>,
    ready: Condvar,
}

impl IssueHostBackfillGate {
    fn mark_done(&self) {
        let mut done = self
            .done
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *done = true;
        self.ready.notify_all();
    }

    fn wait_timeout(&self, timeout: Duration) {
        let guard = self
            .done
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *guard {
            return;
        }
        let _ = self.ready.wait_timeout_while(guard, timeout, |done| !*done);
    }
}

struct BackfillGateGuard<'a>(&'a AppHandle);

impl Drop for BackfillGateGuard<'_> {
    fn drop(&mut self) {
        if let Some(gate) = self.0.try_state::<IssueHostBackfillGate>() {
            gate.mark_done();
        }
    }
}

/// Waits briefly on startup until Collapse issue-host backfill finishes (#613).
pub(crate) fn wait_for_issue_host_backfill(application: &AppHandle, timeout: Duration) {
    if let Some(gate) = application.try_state::<IssueHostBackfillGate>() {
        gate.wait_timeout(timeout);
    }
}

/// Polls Collapse's dashboard only during the 5-minute window after a land on a
/// Collapse-mirrored project; outside that window, reads happen on mirror pushes, window
/// focus, and opening the Rosen status pane (#572). Also runs the one-time startup backfill
/// keeping already-imported Collapse projects on Collapse issues (#613).
pub(crate) fn initialize(application: &AppHandle) {
    application.manage(DashboardPoller::default());
    application.manage(IssueHostBackfillGate::default());
    let poll_handle = application.clone();
    tauri::async_runtime::spawn(async move {
        poll_after_lands(&poll_handle).await;
    });
    let backfill_handle = application.clone();
    tauri::async_runtime::spawn(async move {
        backfill_collapse_issue_hosts(&backfill_handle).await;
    });
}

struct BackfillCandidate {
    project: u32,
    name: String,
    collapse_repository: String,
}

/// On startup, keeps already-moved projects on Collapse when Collapse's GitHub issue import
/// finished without an error, and reconciles host/CI drift across all projects (#613).
async fn backfill_collapse_issue_hosts(application: &AppHandle) {
    let _guard = BackfillGateGuard(application);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let Ok((candidates, others)) = crate::state::with_store(application, |store| {
        let mut candidates = Vec::new();
        let mut others = Vec::new();
        for item in &store.state().projects {
            if item.issue_host.is_none()
                && let Some(repo) = item.collapse_repository.clone()
            {
                candidates.push(BackfillCandidate {
                    project: item.id,
                    name: item.name.clone(),
                    collapse_repository: repo,
                });
            } else {
                others.push(item.id);
            }
        }
        (candidates, others)
    })
    .await
    else {
        return;
    };
    for project in others {
        reconcile_startup_host_sources(application, project, now_ms).await;
    }
    if candidates.is_empty() {
        return;
    }
    let token = tauri::async_runtime::spawn_blocking(velocity_collapse::resolve_issues_token)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    for candidate in candidates {
        backfill_one_project(application, &candidate, token.as_deref(), now_ms).await;
    }
}

async fn backfill_one_project(
    application: &AppHandle,
    candidate: &BackfillCandidate,
    token: Option<&str>,
    now_ms: u64,
) {
    let Some(token) = token else {
        return;
    };
    let Some((owner, name)) = candidate.collapse_repository.split_once('/') else {
        return;
    };
    let Ok(status) = velocity_collapse::get_issue_import_status(token, owner, name).await else {
        return;
    };
    let Some(finished_at) = status.finished_without_error_at().map(str::to_owned) else {
        reconcile_startup_host_sources(application, candidate.project, now_ms).await;
        return;
    };
    let project = candidate.project;
    let expected = candidate.collapse_repository.clone();
    let saved = crate::state::try_with_store(application, move |store| {
        let changed = store
            .backfill_collapse_issue_host(project, &expected)
            .map_err(|error| error.to_string())?;
        Ok((changed, changed.then(|| store.layout_refresh())))
    })
    .await;
    let Ok((true, refresh)) = saved else {
        return;
    };
    crate::diagnostics::record(
        "collapse",
        &format!(
            "{}: kept issues on Collapse (import finished {finished_at})",
            candidate.name
        ),
    );
    if let Some(refresh) = refresh {
        let handle = application.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            crate::layout::apply_refresh_and_publish(&handle, refresh)
        })
        .await;
    }
    velocity_github::IssueClient::invalidate_open_issues_for(
        IssueHost::Collapse,
        &candidate.collapse_repository,
    );
    crate::project_health::mark_collapse_issues_imported(
        application,
        project,
        &candidate.collapse_repository,
    );
    let github_repo = [
        status.github_repository.as_str(),
        status.suggested_github_repository.as_str(),
    ]
    .into_iter()
    .map(str::trim)
    .find(|value| !value.is_empty());
    if let Some(github_repo) = github_repo {
        let high_water = status
            .result
            .as_ref()
            .filter(|result| result.next_number > 1)
            .and_then(|result| u32::try_from(result.next_number.saturating_sub(1)).ok());
        let _ = crate::collapse_issue_sync::enable_post_switch_sync(
            application,
            project,
            github_repo,
            high_water,
            Some(&finished_at),
            true,
        )
        .await;
    }
    crate::project_health::sync_host_source_change(application, project);
    crate::project_health::sync_host_issue_problems(application, project);
    crate::base_ci::wake(application);
}

async fn reconcile_startup_host_sources(application: &AppHandle, project: u32, now_ms: u64) {
    let drift = crate::state::try_with_store(application, move |store| {
        store
            .reconcile_host_sources(project, "Changed on startup", now_ms)
            .map_err(|error| error.to_string())
    })
    .await;
    if matches!(drift, Ok(Some(_))) {
        crate::project_health::sync_host_source_change(application, project);
    }
}

async fn poll_after_lands(application: &AppHandle) {
    let Some(poller) = application.try_state::<DashboardPoller>() else {
        return;
    };
    loop {
        let Some(delay) = dashboard_poll_delay(elapsed_since_last_land(&poller)) else {
            poller.wake.notified().await;
            continue;
        };
        tokio::time::sleep(delay).await;
        if dashboard_poll_active(elapsed_since_last_land(&poller)) {
            // Collapse or the network being away shows as the last state Collapse gave.
            let _ = refresh_health(application).await;
        }
    }
}

fn elapsed_since_last_land(poller: &DashboardPoller) -> Option<Duration> {
    let last = *poller
        .last_land
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    last.map(|instant| instant.elapsed())
}

/// Records a land on `project` and activates the 5-minute post-land dashboard polling
/// window when `project` mirrors to Collapse (#572).
pub(crate) fn note_land(application: &AppHandle, project: u32) {
    crate::fleet_ledger::push_collapse_rollups_if_due(application, project, true);
    if !project_mirrors_to_collapse(application, project) {
        return;
    }
    if let Some(poller) = application.try_state::<DashboardPoller>() {
        let mut last = poller
            .last_land
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *last = Some(Instant::now());
        drop(last);
        poller.wake.notify_one();
    }
}

/// Reads Collapse's dashboard after a mirror push on a Collapse-mirrored project (#572).
pub(crate) fn on_mirror_push(application: &AppHandle, project: u32) {
    if project_mirrors_to_collapse(application, project) {
        refresh_soon(application);
    }
}

/// Reads Collapse's dashboard when the main window gains focus (#572).
pub(crate) fn on_window_focused(application: &AppHandle) {
    if any_project_mirrors_to_collapse(application) {
        refresh_soon(application);
    }
}

/// Reads Collapse's dashboard when the Rosen status pane opens for a Collapse project (#572).
pub(crate) fn on_rosen_pane_opened(application: &AppHandle, project: u32) {
    if project_mirrors_to_collapse(application, project) {
        refresh_soon(application);
    }
}

fn project_mirrors_to_collapse(application: &AppHandle, project: u32) -> bool {
    let Some(store) = application.try_state::<ManagedStateStore>() else {
        return false;
    };
    lock(store.inner(), "persisted state")
        .ok()
        .and_then(|store| {
            store
                .project(project)
                .map(|item| item.collapse_repository.is_some())
        })
        .unwrap_or(false)
}

fn any_project_mirrors_to_collapse(application: &AppHandle) -> bool {
    let Some(store) = application.try_state::<ManagedStateStore>() else {
        return false;
    };
    lock(store.inner(), "persisted state")
        .ok()
        .is_some_and(|store| {
            store
                .state()
                .projects
                .iter()
                .any(|item| item.collapse_repository.is_some())
        })
}

/// After a mirror push, focus or pane open: read Collapse's land and forwarder state.
pub(crate) fn refresh_soon(application: &AppHandle) {
    let application = application.clone();
    tauri::async_runtime::spawn(async move {
        let _ = refresh_health(&application).await;
    });
}

/// One read at a time; a read already running covers the request.
static READING: AtomicBool = AtomicBool::new(false);

struct ReadingGuard;

impl Drop for ReadingGuard {
    fn drop(&mut self) {
        READING.store(false, Ordering::SeqCst);
    }
}

/// Reads Collapse's dashboard once and saves each Collapse-mirrored project's health.
pub(crate) async fn refresh_health(application: &AppHandle) -> Result<(), String> {
    let mirrored = mirrored_projects(application).await?;
    if mirrored.is_empty() || READING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let _reading = ReadingGuard;
    let Some(credentials) = load_credentials().await? else {
        return Ok(());
    };
    let rows = velocity_collapse::repository_activity(&credentials)
        .await
        .map_err(|error| error.to_string())?;
    let mut updates = Vec::with_capacity(mirrored.len());
    let mut forwarding_states = Vec::with_capacity(mirrored.len());
    for (project, repository, github_remote) in mirrored {
        let mut next = rows.iter().find(|row| row.is(&repository)).map(health);
        if github_remote.is_none()
            && let Some(item) = &mut next
            && item.downstream_github.is_none()
            && item.forwarder != ForwarderState::Unconfigured
        {
            item.downstream_github = fetch_downstream_github(&credentials, &repository).await;
        }
        let forwarder = next.as_ref().map(|item| item.forwarder);
        let forwarder_message = next.as_ref().and_then(|item| item.message.clone());
        forwarding_states.push((
            project,
            repository,
            github_remote,
            forwarder,
            forwarder_message,
        ));
        updates.push((project, next));
    }
    let moved_projects = save_and_publish_health_updates(application, updates).await?;
    for (project, repository, github_remote, forwarder, forwarder_message) in forwarding_states {
        crate::project_health::sync_collapse_forwarding(
            application,
            project,
            Some(&repository),
            github_remote.as_deref(),
            forwarder,
            forwarder_message.as_deref(),
        );
    }
    for project in moved_projects {
        // Another machine may have just landed to Collapse; check the tip now (#520).
        crate::rosen_mirror::check_soon(application, project);
    }
    Ok(())
}

async fn save_and_publish_health_updates(
    application: &AppHandle,
    updates: Vec<(u32, Option<CollapseMirrorHealth>)>,
) -> Result<Vec<u32>, String> {
    let (refresh, moved_projects) = crate::state::try_with_store(application, move |store| {
        let mut changed = false;
        let mut moved_projects = Vec::new();
        for (project, health) in updates {
            let previous_land = store
                .project(project)
                .and_then(|item| item.collapse_mirror_health.as_ref())
                .and_then(|health| health.last_land_at.clone());
            let next_land = health.as_ref().and_then(|item| item.last_land_at.clone());
            if next_land.is_some() && next_land != previous_land {
                moved_projects.push(project);
            }
            changed |= store
                .save_collapse_mirror_health(project, health)
                .map_err(|error| error.to_string())?;
        }
        let refresh = changed.then(|| store.layout_refresh());
        Ok((refresh, moved_projects))
    })
    .await?;
    if let Some(refresh) = refresh {
        let handle = application.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::layout::apply_refresh_and_publish(&handle, refresh)
        })
        .await
        .map_err(|error| error.to_string())??;
    }
    Ok(moved_projects)
}

/// Returns Collapse's downstream GitHub `owner/repository` for `project`, checking the
/// saved mirror health first and querying Collapse on demand when needed (#574).
pub(crate) async fn read_downstream_github(
    application: &AppHandle,
    project: u32,
) -> Option<String> {
    let (collapse_repository, cached) = crate::state::with_store(application, move |store| {
        let item = store.project(project)?;
        Some((
            item.collapse_repository.clone()?,
            item.collapse_mirror_health
                .as_ref()
                .and_then(|health| health.downstream_github.clone()),
        ))
    })
    .await
    .ok()??;
    if cached.is_some() {
        return cached;
    }
    let credentials = load_credentials().await.ok()??;
    let downstream = fetch_downstream_github(&credentials, &collapse_repository).await?;
    let saved = downstream.clone();
    let _ = crate::state::try_with_store(application, move |store| {
        let Some(mut health) = store
            .project(project)
            .and_then(|item| item.collapse_mirror_health.clone())
        else {
            return Ok(false);
        };
        health.downstream_github = Some(saved);
        store
            .save_collapse_mirror_health(project, Some(health))
            .map_err(|error| error.to_string())
    })
    .await;
    Some(downstream)
}

async fn fetch_downstream_github(
    credentials: &CollapseCredentials,
    collapse_repository: &str,
) -> Option<String> {
    let (owner, name) = collapse_repository.split_once('/')?;
    let remote = velocity_collapse::repository_downstream(credentials, owner, name)
        .await
        .ok()??;
    velocity_core::collapse::github_repository_from_remote(&remote)
}

async fn mirrored_projects(
    application: &AppHandle,
) -> Result<Vec<(u32, String, Option<String>)>, String> {
    crate::state::with_store(application, |store| {
        store
            .state()
            .projects
            .iter()
            .filter_map(|item| {
                Some((
                    item.id,
                    item.collapse_repository.clone()?,
                    item.github_remote.clone(),
                ))
            })
            .collect()
    })
    .await
}

fn health(row: &RepositoryActivity) -> CollapseMirrorHealth {
    let present = |text: &Option<String>| text.clone().filter(|text| !text.trim().is_empty());
    let downstream_github = row
        .downstream_remote()
        .as_deref()
        .and_then(velocity_core::collapse::github_repository_from_remote);
    CollapseMirrorHealth {
        forwarder: ForwarderState::from_collapse(&row.forward.state),
        behind: u32::try_from(row.forward.behind.max(0)).unwrap_or(u32::MAX),
        // Git's rejection can run long; the first lines say what stopped.
        message: present(&row.forward.message)
            .map(|message| message.lines().take(3).collect::<Vec<_>>().join("\n")),
        next_attempt: present(&row.forward.next_attempt),
        last_land_at: row.last_land.as_ref().map(|land| land.time.clone()),
        last_land_agent: row
            .last_land
            .as_ref()
            .map(|land| land.agent.clone())
            .filter(|agent| !agent.is_empty()),
        archived: row.archived_at.is_some(),
        downstream_github,
    }
}
