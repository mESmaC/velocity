//! Keeping Velocity's Rosen pin current (SPEC.md F5 "Rosen · Pin bumps", #513). Where a
//! Rosen project's GitHub repository is Velocity's own, this edge looks every ten minutes
//! for a green commit on Rosen's `main` ahead of the pin and keeps exactly one open, current
//! "Bump the Rosen pin to `<commit>`" issue, which continuous dispatch then takes like any
//! other. It also publishes the Rosen release a landed bump names. No thread, agent session
//! or window is involved; `velocity-core` decides and `velocity-github` talks to GitHub.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_collapse::releases::{
    self as collapse_releases, CollapseRelease, CreateReleaseRequest, ROSEN_REPOSITORY,
};
use velocity_collapse::{CREATED_RELEASE_TOKEN_LOG, ReleaseTokenSession};
use velocity_core::rosen_pin::{
    PlatformPostLand, PostLandSnapshot, PostLandState, RosenCommit, RosenPin,
    merge_post_land_states, short_commit,
};
use velocity_github::IssueHost;
use velocity_github::rosen_pin::{IssueLook, PinIssue, ReleaseLook, RosenPinWatch};
use velocity_github::velocity_reports::VELOCITY_REPOSITORY;

use crate::state::ManagedStateStore;

const ROSEN_REQUIRED_ASSETS: [&str; 4] = [
    "rosen-x86_64-pc-windows-msvc.exe",
    "rosen-server-x86_64-pc-windows-msvc.exe",
    "rosen-client-commit.txt",
    "SHA256SUMS.txt",
];

/// How many newest commits ahead of the pin are checked for green status.
const GREEN_SCAN: usize = 8;

/// The first look waits until startup work and resumed threads settle.
const FIRST_LOOK: Duration = Duration::from_secs(2 * 60);
/// Rosen lands a few commits an hour and its CI takes longer than this, so a green commit
/// has its issue within minutes at about ten requests a look.
const LOOK_INTERVAL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
struct VelocityPinTarget {
    host: IssueHost,
    repository: String,
    collapse_repository: Option<String>,
}

/// What the looks remember between them. Nothing is saved: a restart only asks again.
#[derive(Default)]
struct Watch {
    /// The target host/repository whose maintainer permission was checked.
    target: Option<VelocityPinTarget>,
    /// The signed-in account may maintain Velocity's repository.
    maintainer: bool,
    /// The pinned Rosen release already found on Rosen.
    known_release: Option<String>,
    /// The failure last written to the log, so a failure that repeats is written once.
    last_failure: Option<String>,
}

pub fn initialize(application: &AppHandle) {
    // A development build is not the copy the owner runs: it files and publishes nothing.
    if cfg!(debug_assertions) {
        return;
    }
    let application = application.clone();
    std::thread::spawn(move || {
        let mut watch = Watch::default();
        std::thread::sleep(FIRST_LOOK);
        loop {
            if let Some(target) = velocity_pin_target(&application) {
                look(&application, &mut watch, &target);
            }
            std::thread::sleep(LOOK_INTERVAL);
        }
    });
}

/// Whether this machine has the Rosen project whose repository is Velocity's own, and which
/// issue host (`GitHub` or `Collapse`) backs it (#513, #590).
fn velocity_pin_target(application: &AppHandle) -> Option<VelocityPinTarget> {
    let store = application.try_state::<ManagedStateStore>()?;
    let store = store.lock().ok()?;
    let projects = &store.state().projects;
    let velocity = projects.iter().find(|project| {
        project.rosen_in_use()
            && (project
                .collapse_repository
                .as_deref()
                .is_some_and(|remote| remote.eq_ignore_ascii_case(VELOCITY_REPOSITORY))
                || project
                    .github_remote
                    .as_deref()
                    .is_some_and(|remote| remote.eq_ignore_ascii_case(VELOCITY_REPOSITORY)))
    })?;
    let host = velocity.effective_issue_host();
    let repository = velocity
        .issue_host_repository()
        .map(|(_, repository)| repository)
        .unwrap_or_else(|| VELOCITY_REPOSITORY.to_owned());
    let collapse_repository = projects.iter().find_map(|project| {
        if !project.rosen_in_use() {
            return None;
        }
        let candidate = match host {
            IssueHost::Collapse => project.collapse_repository.as_deref(),
            IssueHost::GitHub => project.github_remote.as_deref(),
        }?;
        velocity_github::graph_indexer_pin::COLLAPSE_REPOSITORIES
            .iter()
            .any(|known| candidate.eq_ignore_ascii_case(known))
            .then(|| candidate.to_owned())
    });
    Some(VelocityPinTarget {
        host,
        repository,
        collapse_repository,
    })
}

struct ObservedLook {
    issue: Result<IssueLook, velocity_github::GitHubCommandError>,
    release: Result<ReleaseLook, String>,
}

fn look(application: &AppHandle, watch: &mut Watch, target: &VelocityPinTarget) {
    let (done, failures) = match observe(application, watch, target) {
        Ok(Some(ObservedLook { issue, release })) => {
            let mut done = Vec::new();
            let mut failures = Vec::new();
            match issue {
                Ok(issue) => done.extend(issue_notes(&issue)),
                Err(error) => failures.push(format!("The pin-bump issue was not updated: {error}")),
            }
            match release {
                Ok(release) => {
                    done.extend(release_note(&release));
                    watch.known_release = release.release;
                }
                Err(error) => {
                    failures.push(format!(
                        "The pinned Rosen release was not published to Collapse: {error}"
                    ));
                }
            }
            (done, failures)
        }
        Ok(None) => (Vec::new(), Vec::new()),
        Err(error) => (
            Vec::new(),
            vec![format!("The Rosen pin was not read: {error}")],
        ),
    };
    for note in &done {
        crate::diagnostics::record("rosen pin", note);
    }
    let failure = (!failures.is_empty()).then(|| failures.join(" "));
    if let Some(failure) = failure.as_deref().filter(|_| failure != watch.last_failure) {
        crate::diagnostics::record("rosen pin", failure);
    }
    watch.last_failure = failure;
}

/// One look, or `None` when the signed-in account does not maintain Velocity's repository.
/// Builds and publishes the pinned Rosen release from source to Collapse without touching
/// GitHub's release workflow (#561, #593, #590).
fn observe(
    application: &AppHandle,
    watch: &mut Watch,
    target: &VelocityPinTarget,
) -> Result<Option<ObservedLook>, velocity_github::GitHubCommandError> {
    if watch.target.as_ref() != Some(target) {
        watch.target = Some(target.clone());
        watch.maintainer = false;
    }
    let mut client = RosenPinWatch::for_host(target.host, &target.repository)?;
    if let Some(collapse_repo) = target.collapse_repository.as_deref() {
        client = client.with_collapse_repository(collapse_repo);
    }
    if target.host == IssueHost::Collapse || rosen_project_on_collapse(application) {
        client = client.with_rosen_only_ci(true);
    }
    if !watch.maintainer {
        watch.maintainer = client.can_maintain()?;
    }
    if !watch.maintainer {
        return Ok(None);
    }
    let (pin, issue) =
        client.look_issue_with_post_land(|pin, ahead| read_post_land(application, pin, ahead))?;
    let release = publish_collapse_rosen_release(
        application,
        target.host,
        &pin,
        watch.known_release.as_deref(),
    );
    Ok(Some(ObservedLook { issue, release }))
}

/// Builds Rosen binaries from source at `pin.revision` (#561) and publishes them to the
/// Collapse Rosen repository when `pin.release_to_publish()` is missing or incomplete (#593).
fn publish_collapse_rosen_release(
    application: &AppHandle,
    host: IssueHost,
    pin: &RosenPin,
    known: Option<&str>,
) -> Result<ReleaseLook, String> {
    let Some(tag) = pin.release_to_publish() else {
        return Ok(ReleaseLook::default());
    };
    if known == Some(tag) {
        return Ok(ReleaseLook {
            release: Some(tag.to_owned()),
            published: false,
        });
    }
    let app_data = application
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not resolve app data directory: {error}"))?;
    let (collapse_repo, local_repo_path) = resolve_rosen_collapse_target(application, pin);
    let (owner, name) = velocity_core::collapse::parse_repository(&collapse_repo)
        .ok_or_else(|| format!("Invalid Collapse Rosen repository {collapse_repo}"))?;
    let read_token = release_read_token().map_err(|error| error.to_string())?;
    let existing = tauri::async_runtime::block_on(collapse_releases::get_release_by_tag(
        read_token.as_deref(),
        &owner,
        &name,
        tag,
    ))
    .map_err(|error| error.to_string())?;
    if existing.as_ref().is_some_and(has_all_rosen_assets) {
        return Ok(ReleaseLook {
            release: Some(tag.to_owned()),
            published: false,
        });
    }
    let resolved = tauri::async_runtime::block_on(velocity_collapse::ensure_release_token())
        .map_err(|error| error.to_string())?;
    if resolved.created_from_session {
        record_created_release_token();
    }
    let git_url = format!("{}/{}/{}.git", collapse_releases::API_HOST, owner, name);
    let source_url = match host {
        IssueHost::Collapse => git_url.clone(),
        IssueHost::GitHub => format!("https://github.com/{}.git", pin.repository),
    };
    let out_dir = app_data.join("rosen-pin-release").join(tag);
    if out_dir.exists() {
        let _ = std::fs::remove_dir_all(&out_dir);
    }
    let built =
        tauri::async_runtime::block_on(velocity_threads::local_release::build_rosen_pin_binaries(
            &app_data,
            &source_url,
            &pin.revision,
            &out_dir,
        ))?;
    let mut session = ReleaseTokenSession::new(resolved.record);
    let publish = tauri::async_runtime::block_on(upload_rosen_pin_release(
        &mut session,
        &owner,
        &name,
        &git_url,
        local_repo_path.as_deref(),
        pin,
        tag,
        &built,
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    publish?;
    Ok(ReleaseLook {
        release: Some(tag.to_owned()),
        published: true,
    })
}

fn release_read_token() -> Result<Option<String>, velocity_collapse::CollapseError> {
    if let Some(credentials) = velocity_collapse::load_credentials()? {
        return Ok(Some(credentials.mcp_token));
    }
    velocity_collapse::load_release_token()
}

fn record_created_release_token() {
    crate::diagnostics::record("rosen pin", CREATED_RELEASE_TOKEN_LOG);
}

fn has_all_rosen_assets(release: &CollapseRelease) -> bool {
    ROSEN_REQUIRED_ASSETS
        .iter()
        .all(|expected| release.assets.iter().any(|asset| asset.name == *expected))
}

#[allow(clippy::too_many_arguments)]
async fn upload_rosen_pin_release(
    session: &mut ReleaseTokenSession,
    owner: &str,
    name: &str,
    mirror_url: &str,
    local_repo: Option<&std::path::Path>,
    pin: &RosenPin,
    tag: &str,
    built: &velocity_threads::local_release::BuiltRosenPinAssets,
) -> Result<(), String> {
    let mirror = mirror_url.to_owned();
    let rev = pin.revision.clone();
    let tag_copy = tag.to_owned();
    let local = local_repo.map(PathBuf::from);
    tokio::task::spawn_blocking(move || {
        velocity_collapse::ensure_remote_tag(&mirror, &rev, &tag_copy, false, local.as_deref())
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    let short = short_commit(&pin.revision);
    let title = format!("Rosen at {short}, pinned by Velocity");
    let notes = format!(
        "Prebuilt Windows Rosen binaries built from source at `{}` for Velocity ({}).",
        pin.revision, VELOCITY_REPOSITORY
    );
    let request = CreateReleaseRequest {
        tag,
        name: &title,
        notes: &notes,
        prerelease: true,
        draft: false,
        rolling: false,
    };
    let release = session
        .run(record_created_release_token, |token| {
            let body = request.clone();
            async move { collapse_releases::ensure_release(&token, owner, name, &body).await }
        })
        .await
        .map_err(|error| error.to_string())?;
    upload_rosen_pin_assets(session, owner, name, &release, built).await
}

async fn upload_rosen_pin_assets(
    session: &mut ReleaseTokenSession,
    owner: &str,
    name: &str,
    release: &CollapseRelease,
    built: &velocity_threads::local_release::BuiltRosenPinAssets,
) -> Result<(), String> {
    let files: [(&std::path::Path, &str); 4] = [
        (&built.rosen_exe, "application/octet-stream"),
        (&built.rosen_server_exe, "application/octet-stream"),
        (&built.commit_file, "text/plain; charset=utf-8"),
        (&built.sha256sums, "text/plain; charset=utf-8"),
    ];
    for (path, mime) in files {
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("Invalid Rosen asset path {}", path.display()))?;
        session
            .run(record_created_release_token, |token| async move {
                let bytes = std::fs::read(path).map_err(|error| {
                    velocity_collapse::CollapseError::Api(format!(
                        "Could not read {}: {error}",
                        path.display()
                    ))
                })?;
                collapse_releases::upload_or_replace_asset(
                    &token, owner, name, release, file_name, mime, bytes,
                )
                .await
            })
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn resolve_rosen_collapse_target(
    application: &AppHandle,
    pin: &RosenPin,
) -> (String, Option<PathBuf>) {
    if let Some(store) = application.try_state::<ManagedStateStore>()
        && let Ok(store) = store.lock()
        && let Some(project) = store.state().projects.iter().find(|project| {
            project.rosen_in_use()
                && (project
                    .github_remote
                    .as_deref()
                    .is_some_and(|remote| remote.eq_ignore_ascii_case(&pin.repository))
                    || project
                        .collapse_repository
                        .as_deref()
                        .is_some_and(|remote| remote.eq_ignore_ascii_case(&pin.repository)))
        })
    {
        let repo = project
            .collapse_repository
            .clone()
            .unwrap_or_else(|| ROSEN_REPOSITORY.to_owned());
        return (repo, Some(PathBuf::from(&project.repository_path)));
    }
    (ROSEN_REPOSITORY.to_owned(), None)
}

/// Reads Rosen's post-land verdicts from this machine's local Rosen server for `pin.repository`
/// when present, falling back to Collapse's post-land read when needed (#580).
fn read_post_land(
    application: &AppHandle,
    pin: &RosenPin,
    ahead: &[RosenCommit],
) -> BTreeMap<String, PostLandState> {
    let mut states = BTreeMap::new();
    let local = local_rosen_project(application, &pin.repository);
    let candidate_commits: Vec<String> = ahead
        .iter()
        .take(GREEN_SCAN)
        .map(|commit| commit.commit.clone())
        .collect();
    if let Some((project_id, _)) = &local {
        match crate::rosen::observation_server(application, *project_id).and_then(|server| {
            tauri::async_runtime::block_on(velocity_rosen::integration::post_land_states_verified(
                &server,
                &candidate_commits,
            ))
            .map_err(|error| error.to_string())
        }) {
            Ok(local_states) => states = local_states,
            Err(error) => crate::diagnostics::record(
                "rosen pin",
                &format!("Could not read local Rosen post-land state: {error}"),
            ),
        }
    }
    let unresolved = candidate_commits
        .iter()
        .any(|commit| !states.contains_key(&commit.to_ascii_lowercase()));
    if local.is_none() || unresolved {
        let collapse_repo = local.and_then(|(_, collapse)| collapse);
        if let Some(collapse_states) = read_collapse_post_land(pin, collapse_repo.as_deref(), ahead)
        {
            merge_post_land_states(&mut states, collapse_states);
        }
    }
    states
}

/// True when a local Rosen repository project on this machine has Issues on Collapse (#591).
fn rosen_project_on_collapse(application: &AppHandle) -> bool {
    let Some(store) = application.try_state::<ManagedStateStore>() else {
        return false;
    };
    let Ok(store) = store.lock() else {
        return false;
    };
    store.state().projects.iter().any(|project| {
        project.rosen_in_use()
            && project.effective_issue_host() == IssueHost::Collapse
            && (project
                .collapse_repository
                .as_deref()
                .is_some_and(|remote| remote.eq_ignore_ascii_case(ROSEN_REPOSITORY))
                || project
                    .github_remote
                    .as_deref()
                    .is_some_and(|remote| remote.eq_ignore_ascii_case(ROSEN_REPOSITORY)))
    })
}

/// Finds a local Rosen project on this machine corresponding to `repository` (such as
/// `mESmaC/rosen`), returning `(project_id, collapse_repository)`.
fn local_rosen_project(application: &AppHandle, repository: &str) -> Option<(u32, Option<String>)> {
    let store = application.try_state::<ManagedStateStore>()?;
    let store = store.lock().ok()?;
    store.state().projects.iter().find_map(|project| {
        if !project.rosen_in_use() {
            return None;
        }
        let matches = project
            .github_remote
            .as_deref()
            .is_some_and(|remote| remote.eq_ignore_ascii_case(repository))
            || project
                .collapse_mirror_health
                .as_ref()
                .and_then(|health| health.downstream_github.as_deref())
                .is_some_and(|remote| remote.eq_ignore_ascii_case(repository))
            || project
                .collapse_repository
                .as_deref()
                .is_some_and(|remote| remote.eq_ignore_ascii_case(repository));
        matches.then(|| (project.id, project.collapse_repository.clone()))
    })
}

/// Reads Rosen's post-land checkpoint from Collapse when Collapse is connected (#580).
/// Failures are recorded and return `None` so GitHub CI still works when Collapse is away.
fn read_collapse_post_land(
    pin: &RosenPin,
    local_collapse_repo: Option<&str>,
    ahead: &[RosenCommit],
) -> Option<BTreeMap<String, PostLandState>> {
    let credentials = match velocity_collapse::load_credentials() {
        Ok(Some(credentials)) => credentials,
        Ok(None) => return None,
        Err(error) => {
            crate::diagnostics::record(
                "rosen pin",
                &format!("Could not load Collapse credentials for post-land check: {error}"),
            );
            return None;
        }
    };
    let target = local_collapse_repo.unwrap_or(&pin.repository);
    let post_land = match tauri::async_runtime::block_on(fetch_collapse_post_land(
        &credentials,
        target,
        &pin.repository,
        local_collapse_repo.is_some(),
    )) {
        Ok(Some(post_land)) => post_land,
        Ok(None) => return None,
        Err(error) => {
            crate::diagnostics::record(
                "rosen pin",
                &format!("Could not read {target} post-land state from Collapse: {error}"),
            );
            return None;
        }
    };
    let platforms: Vec<PlatformPostLand> = post_land
        .platforms
        .into_iter()
        .map(|entry| PlatformPostLand {
            platform: entry.platform,
            required_build: entry.required_build,
            required_test: entry.required_test,
            last_built: entry.last_built,
            last_green: entry.last_green,
        })
        .collect();
    let snapshot = PostLandSnapshot {
        green_commit: post_land.green_commit,
        green_at: post_land.green_at,
        green_events: post_land.green_events,
        failed_commits: post_land.failed_commits,
    };
    Some(snapshot.states_for_platforms(ahead, &platforms))
}

async fn fetch_collapse_post_land(
    credentials: &velocity_collapse::CollapseCredentials,
    primary_repo: &str,
    github_repo: &str,
    has_local_collapse_repo: bool,
) -> Result<Option<velocity_collapse::RepositoryPostLand>, velocity_collapse::CollapseError> {
    if let Some((owner, name)) = primary_repo.split_once('/')
        && let Some(state) =
            velocity_collapse::repository_post_land(credentials, owner, name).await?
    {
        return Ok(Some(state));
    }
    if has_local_collapse_repo {
        return Ok(None);
    }
    // If Rosen's Collapse mirror lives under another namespace, locate it by its GitHub downstream.
    let rows = velocity_collapse::repository_activity(credentials).await?;
    let matched = rows.into_iter().find(|row| {
        row.downstream_remote()
            .as_deref()
            .and_then(velocity_core::collapse::github_repository_from_remote)
            .is_some_and(|downstream| downstream.eq_ignore_ascii_case(github_repo))
    });
    let Some(row) = matched else {
        return Ok(None);
    };
    velocity_collapse::repository_post_land(credentials, &row.owner, &row.name).await
}

fn issue_notes(look: &IssueLook) -> Vec<String> {
    let mut notes = Vec::new();
    match look.issue {
        PinIssue::Opened(number) => notes.push(format!("Opened pin-bump issue #{number}.")),
        PinIssue::Rewritten(number) => notes.push(format!("Rewrote pin-bump issue #{number}.")),
        PinIssue::Current | PinIssue::NoGreenCommit | PinIssue::Kept(_) => {}
    }
    if !look.closed.is_empty() {
        let closed: Vec<String> = look
            .closed
            .iter()
            .map(|number| format!("#{number}"))
            .collect();
        notes.push(format!("Closed pin-bump issues {}.", closed.join(", ")));
    }
    notes
}

fn release_note(look: &ReleaseLook) -> Option<String> {
    let release = look.release.as_deref().filter(|_| look.published)?;
    Some(format!(
        "Built and published Rosen release {release} to Collapse for the pinned commit."
    ))
}
