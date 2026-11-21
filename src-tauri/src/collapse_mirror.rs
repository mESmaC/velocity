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
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;
use velocity_collapse::{CollapseCredentials, MirrorRequest, RepositoryActivity};
use velocity_core::collapse::{
    CollapseMirrorHealth, ForwarderState, dashboard_poll_active, dashboard_poll_delay,
};
use velocity_new_project::{
    NewProjectRequest, OpenProjectRequest, ProjectLocationRequest, ProjectMirror,
};
use velocity_rosen::collapse_link::{CollapseLink, allow_collapse_relay, propose_collapse_link};
use velocity_rosen::landing::{LandOutcome, LandRefusal};
use velocity_rosen::workspace_settings::SETTINGS_AGENT;

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::lock;

const NOT_CONNECTED: &str = "Connect Collapse in Settings → Agents → Shared MCP servers first, so Velocity can create the repository";
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
    crate::rosen_mirror::refresh(application, project).await?;
    Ok(())
}

async fn ensure_proactive_auth() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(velocity_collapse::ensure_proactive_auth)
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
        mirror:
            ProjectMirror::Collapse {
                repository,
                github_repository,
            },
    } = &mut request.location
    else {
        return Ok(());
    };
    let folder = Path::new(path.trim());
    // Refuse a folder or GitHub repository Create would refuse before anything exists on Collapse.
    velocity_new_project::validate_project_root(folder).map_err(|error| error.to_string())?;
    if let Some(github) = github_repository.as_deref() {
        velocity_new_project::parse_github_repository(github).map_err(|error| error.to_string())?;
    }
    let typed = typed_repository(repository);
    let Some(credentials) = load_credentials().await? else {
        if !typed.contains('/') {
            return Err(NOT_CONNECTED.into());
        }
        ensure_proactive_auth().await?;
        share_hand_stored_token(&typed).await;
        return Ok(());
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

/// What Mirror to Collapse and Send Collapse its live state need from the saved project.
struct MirrorSubject {
    name: String,
    branch: String,
    repository_path: PathBuf,
    github_remote: Option<String>,
    /// The Collapse repository the project already mirrors to.
    repository: Option<String>,
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
    Ok(MirrorSubject {
        name: item.name.clone(),
        branch: item.default_branch.clone(),
        repository_path: PathBuf::from(&item.repository_path),
        github_remote: item.github_remote.clone(),
        repository: item.collapse_repository.clone(),
    })
}

/// Keeps `github_remote` pointing at GitHub when Mirror to Collapse moves an existing
/// GitHub project, capturing the previous GitHub mirror or checkout origin if unset (#574).
async fn preserve_github_remote(
    application: &AppHandle,
    project: u32,
    subject: &MirrorSubject,
    server: &velocity_rosen::ProjectServer,
) -> Result<bool, String> {
    if subject.github_remote.is_some() {
        return Ok(true);
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
        return Ok(false);
    };
    crate::state::try_with_store(application, move |store| {
        store
            .save_github_remote(project, Some(repository))
            .map_err(|error| error.to_string())
    })
    .await?;
    Ok(true)
}

/// `mirror_to_collapse` (§3.1): makes Collapse the project's mirror. Creates the Collapse
/// repository named after the project, stores its push token in git's credential helper,
/// and lands a `rosen.toml` change pointing `[mirror] remote` at it and naming its
/// `collapse` root, so Rosen also sends its live state and ledger entries and runs the
/// owner's actions from Collapse (#517). A project that was on GitHub keeps its
/// `github_remote` pointing at GitHub (#574) and opens the repository's Downstream remote
/// settings, where the owner keeps GitHub fed through Collapse's forwarder (C4).
#[tauri::command]
#[specta::specta]
pub async fn mirror_to_collapse(
    application: AppHandle,
    project_identifier: u32,
) -> Result<(), String> {
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
    let on_github =
        preserve_github_remote(&application, project_identifier, &subject, &server).await?;
    crate::rosen_mirror::check_and_catch_up(&application, project_identifier).await;
    let root = provisioned.collapse_root();
    let link = CollapseLink {
        remote: Some(&provisioned.mirror_url),
        root: &root,
    };
    let message = land_link(&server, &subject.name, &provisioned.repository(), &link).await?;
    toast(&application, message)?;
    crate::rosen_mirror::refresh(&application, project_identifier).await?;
    if on_github
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
    let message = land_link(&server, &subject.name, &repository, &link).await?;
    toast(&application, message)?;
    crate::rosen_mirror::refresh(&application, project_identifier).await?;
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

/// Proposes and lands the `rosen.toml` link, then lets the relay run; the toast says
/// where it stands.
async fn land_link(
    server: &velocity_rosen::ProjectServer,
    project: &str,
    repository: &str,
    link: &CollapseLink<'_>,
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
                LandOutcome::Landed { .. } if mirroring => {
                    format!("{project}: lands and live state now go to Collapse · {repository}")
                }
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
    Ok(format!("{message}{}", relay_note(server, project).await))
}

fn refusal_reason(refusal: LandRefusal) -> String {
    match refusal {
        LandRefusal::Regenerate { paths } => format!("regenerate {}", paths.join(", ")),
        LandRefusal::Conflict { message, .. } | LandRefusal::Other { message, .. } => message,
    }
}

/// Lets the project's server run the actions Collapse's owner pages offer, and says so in
/// the toast: this is the one place the owner hands Collapse authority over the server.
/// A failure leaves the mirror and the feed working, so it is reported, not returned.
async fn relay_note(server: &velocity_rosen::ProjectServer, project: &str) -> String {
    match allow_collapse_relay(server).await {
        Ok(kinds) => format!(
            " · Collapse's owner actions run here ({})",
            kinds.join(", ")
        ),
        Err(error) => {
            crate::diagnostics::record(
                "collapse relay",
                &format!("{project}: could not allow Collapse's actions: {error}"),
            );
            format!(" · Collapse's actions stay off: {error}")
        }
    }
}

/// Create with a Collapse mirror wrote the `collapse` root with the first `rosen.toml`
/// (F2), so the new project's server only needs its relay allowed.
pub(crate) async fn allow_relay_after_create(application: &AppHandle, project: u32) {
    let allowed = match crate::rosen::server(application, project).await {
        Ok(server) => allow_collapse_relay(&server)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    if let Err(error) = allowed {
        crate::diagnostics::record(
            "collapse relay",
            &format!("Project {project}: could not allow Collapse's actions: {error}"),
        );
    }
}

fn toast(application: &AppHandle, message: String) -> Result<(), String> {
    let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
    layout.show_toast(message, None);
    crate::layout::publish(application, &layout.snapshot(), None)
}

#[derive(Default)]
struct DashboardPoller {
    last_land: Mutex<Option<Instant>>,
    wake: tokio::sync::Notify,
}

/// Polls Collapse's dashboard only during the 5-minute window after a land on a
/// Collapse-mirrored project; outside that window, reads happen on mirror pushes, window
/// focus, and opening the Rosen status pane (#572).
pub(crate) fn initialize(application: &AppHandle) {
    application.manage(DashboardPoller::default());
    let application = application.clone();
    tauri::async_runtime::spawn(async move {
        poll_after_lands(&application).await;
    });
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
    for (project, repository, has_github) in mirrored {
        let mut next = rows.iter().find(|row| row.is(&repository)).map(health);
        if !has_github
            && let Some(item) = &mut next
            && item.downstream_github.is_none()
            && item.forwarder != ForwarderState::Unconfigured
        {
            item.downstream_github = fetch_downstream_github(&credentials, &repository).await;
        }
        updates.push((project, next));
    }
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
    for project in moved_projects {
        // Another machine may have just landed to Collapse; check the tip now (#520).
        crate::rosen_mirror::check_soon(application, project);
    }
    Ok(())
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

async fn mirrored_projects(application: &AppHandle) -> Result<Vec<(u32, String, bool)>, String> {
    crate::state::with_store(application, |store| {
        store
            .state()
            .projects
            .iter()
            .filter_map(|item| {
                Some((
                    item.id,
                    item.collapse_repository.clone()?,
                    item.github_remote.is_some(),
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
