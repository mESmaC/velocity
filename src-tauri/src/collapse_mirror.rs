//! Collapse as each project's mirror (#427): provision the Collapse repository, hand its
//! push token to git's credential helper, point Rosen's `[mirror] remote` at it, and show
//! what Collapse reports about the repository's last land and downstream forwarder.
//!
//! Velocity keeps no push token: provisioning passes it to `git credential approve`, and
//! the account tokens stay in the OS credential store (§1, #430).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;
use velocity_collapse::{
    CollapseCredentials, MirrorRequest, ProvisionedMirror, RepositoryActivity,
};
use velocity_core::collapse::{CollapseMirrorHealth, ForwarderState};
use velocity_new_project::{NewProjectRequest, ProjectLocationRequest, ProjectMirror};
use velocity_rosen::landing::{LandOutcome, LandRefusal};
use velocity_rosen::workspace_settings::{SETTINGS_AGENT, propose_mirror_remote};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::lock;

const NOT_CONNECTED: &str = "Connect Collapse in Settings → Agents → Shared MCP servers first, so Velocity can create the repository";
/// How often Collapse's forwarder state is read while any project mirrors to Collapse.
const HEALTH_INTERVAL: Duration = Duration::from_secs(60);
/// New projects start their Rosen base on `main` (F2).
const NEW_PROJECT_BRANCH: &str = "main";

async fn load_credentials() -> Result<Option<CollapseCredentials>, String> {
    tauri::async_runtime::spawn_blocking(velocity_collapse::load_credentials)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// Create with a Collapse mirror: provisions the repository before the initial import,
/// which pushes the new base at once. A blank name uses the project folder's name, and
/// the request then names the provisioned `owner/repository`. Without a connection an
/// existing `owner/repository` is still accepted, for push tokens stored by hand.
pub(crate) async fn prepare_new_project(request: &mut NewProjectRequest) -> Result<(), String> {
    let ProjectLocationRequest::RosenFolder {
        path,
        mirror: ProjectMirror::Collapse { repository },
    } = &mut request.location
    else {
        return Ok(());
    };
    let folder = Path::new(path.trim());
    // Refuse a folder Create would refuse before anything exists on Collapse.
    velocity_new_project::validate_project_root(folder).map_err(|error| error.to_string())?;
    let typed = typed_repository(repository);
    let Some(credentials) = load_credentials().await? else {
        return if typed.contains('/') {
            Ok(())
        } else {
            Err(NOT_CONNECTED.into())
        };
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
    .map_err(|error| error.to_string())?;
    *repository = provisioned.repository();
    Ok(())
}

/// The dialog's text without Collapse's git host or `.git`, as `owner/name`, `name` or empty.
fn typed_repository(repository: &str) -> String {
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

/// What Mirror to Collapse needs from the saved project.
struct MirrorSubject {
    name: String,
    branch: String,
    on_github: bool,
}

fn mirror_subject(application: &AppHandle, project: u32) -> Result<MirrorSubject, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let item = store.project(project).ok_or("Unknown project")?;
    if item.version_control != velocity_store::VersionControl::Rosen {
        return Err("Mirror to Collapse is available after the project migrates to Rosen".into());
    }
    if let Some(repository) = &item.collapse_repository {
        return Err(format!(
            "{} already mirrors to Collapse ({repository})",
            item.name
        ));
    }
    Ok(MirrorSubject {
        name: item.name.clone(),
        branch: item.default_branch.clone(),
        on_github: item.github_remote.is_some(),
    })
}

/// `mirror_to_collapse` (§3.1): makes Collapse the project's mirror. Creates the Collapse
/// repository named after the project, stores its push token in git's credential helper,
/// and lands a `rosen.toml` change pointing `[mirror] remote` at it. A project that was on
/// GitHub then opens the repository's Downstream remote settings, where the owner keeps
/// GitHub fed through Collapse's forwarder (C4). Says what happened in a toast.
#[tauri::command]
#[specta::specta]
pub async fn mirror_to_collapse(
    application: AppHandle,
    project_identifier: u32,
) -> Result<(), String> {
    let subject = mirror_subject(&application, project_identifier)?;
    let credentials = load_credentials().await?.ok_or(NOT_CONNECTED)?;
    let name = velocity_core::collapse::repository_name_for(&subject.name)
        .ok_or("The project's name has no letters or digits to name a Collapse repository")?;
    let provisioned = velocity_collapse::provision_mirror(
        &credentials,
        &MirrorRequest {
            owner: None,
            name: &name,
            default_branch: &subject.branch,
            description: "",
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    let message = point_mirror(&server, &subject.name, &provisioned).await?;
    toast(&application, message)?;
    crate::rosen_mirror::refresh(&application, project_identifier).await?;
    if subject.on_github
        && let Some(page) =
            velocity_core::collapse::downstream_settings_page(&provisioned.repository())
    {
        application
            .opener()
            .open_url(page, None::<&str>)
            .map_err(|error| error.to_string())?;
    }
    refresh_health(&application).await
}

/// Proposes and lands the `[mirror] remote` change; the toast says where it stands.
async fn point_mirror(
    server: &velocity_rosen::ProjectServer,
    project: &str,
    provisioned: &ProvisionedMirror,
) -> Result<String, String> {
    let repository = provisioned.repository();
    let change = propose_mirror_remote(server, &provisioned.mirror_url)
        .await
        .map_err(|error| error.to_string())?;
    let Some(change) = change else {
        return Ok(format!(
            "{project}: rosen.toml already mirrors to {repository}"
        ));
    };
    let outcome = velocity_rosen::landing::land(server, SETTINGS_AGENT, &change)
        .await
        .map_err(|error| error.to_string())?;
    Ok(match outcome {
        LandOutcome::Landed { .. } => {
            format!("{project}: lands now mirror to Collapse · {repository}")
        }
        LandOutcome::Refused(refusal) => {
            let reason = match refusal {
                LandRefusal::Regenerate { paths } => format!("regenerate {}", paths.join(", ")),
                LandRefusal::Conflict { message, .. } | LandRefusal::Other { message, .. } => {
                    message
                }
            };
            format!("{project}: {repository} is ready · change {change} waits to land: {reason}")
        }
    })
}

fn toast(application: &AppHandle, message: String) -> Result<(), String> {
    let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
    layout.show_toast(message, None);
    crate::layout::publish(application, &layout.snapshot(), None)
}

/// Reads Collapse's forwarder state every minute while any project mirrors to Collapse.
pub(crate) fn initialize(application: &AppHandle) {
    let application = application.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(HEALTH_INTERVAL).await;
            // Collapse or the network being away shows as the last state Collapse gave.
            let _ = refresh_health(&application).await;
        }
    });
}

/// After a mirror push: Collapse has just recorded the land and told its forwarder.
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
    let mirrored = mirrored_projects(application)?;
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
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let mut changed = false;
    for (project, repository) in mirrored {
        let health = rows.iter().find(|row| row.is(&repository)).map(health);
        changed |= store
            .save_collapse_mirror_health(project, health)
            .map_err(|error| error.to_string())?;
    }
    if changed {
        let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
        store.refresh_layout(&mut layout);
        crate::layout::publish(application, &layout.snapshot(), None)?;
    }
    Ok(())
}

fn mirrored_projects(application: &AppHandle) -> Result<Vec<(u32, String)>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store
        .state()
        .projects
        .iter()
        .filter_map(|item| Some((item.id, item.collapse_repository.clone()?)))
        .collect())
}

fn health(row: &RepositoryActivity) -> CollapseMirrorHealth {
    let present = |text: &Option<String>| text.clone().filter(|text| !text.trim().is_empty());
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
    }
}
