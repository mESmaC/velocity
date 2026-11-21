//! Adapt Rosen's mirror observation to saved project state, the window and AIRC, and
//! catch projects up from their mirror at start, while running, before lands and on
//! fast-forward push refusals (#501, #520).

use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_core::rosen_mirror::{MirrorBehind, MirrorDivergence};
use velocity_rosen::{CatchUp, MirrorTip, ProjectServer};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// How often Velocity checks active Rosen projects' mirror tips while running (#520).
const MIRROR_POLL_INTERVAL: Duration = Duration::from_secs(2 * 60);

/// Delay before re-reading mirror status when catch-up woke a retry push that has not
/// yet written its recovery event (#520).
const RECOVERED_REFRESH_DELAY: Duration = Duration::from_secs(5);

/// Starts the background mirror tip poller so running servers catch up when another
/// machine lands to the mirror (#520).
pub(crate) fn initialize(application: &AppHandle) {
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(MIRROR_POLL_INTERVAL).await;
            if !crate::rosen::paused(&handle) {
                poll_all_projects(&handle).await;
            }
        }
    });
}

async fn poll_all_projects(application: &AppHandle) {
    let projects = rosen_project_ids(application);
    for project in projects {
        if crate::rosen::paused(application) {
            return;
        }
        check_and_catch_up(application, project).await;
    }
}

fn rosen_project_ids(application: &AppHandle) -> Vec<u32> {
    let Ok(store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return Vec::new();
    };
    store
        .state()
        .projects
        .iter()
        .filter(|item| item.rosen_in_use())
        .map(|item| item.id)
        .collect()
}

/// Schedules a mirror tip check right away, e.g. when Collapse reports a new land (#520).
pub(crate) fn check_soon(application: &AppHandle, project: u32) {
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        check_and_catch_up(&handle, project).await;
    });
}

/// Whether a project's Rosen base is caught up with its Git mirror (#520, #551).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatchUpCheck {
    /// No mirror is configured, the base matches or leads the mirror, or catch-up just
    /// fast-forwarded it to the mirror tip.
    Current,
    /// The project is paused, not using Rosen, diverged, still behind after an error or
    /// timeout, or its mirror could not be checked.
    NotReady,
}

#[derive(Clone, Copy, Debug)]
struct CatchUpProgress {
    moved: bool,
    diverged: bool,
}

/// Checks `project`'s mirror tip and fast-forwards its base when another machine landed
/// ahead of it (#520). Safe to call before every land: up-to-date projects run only a
/// lightweight `ls-remote`, and network errors never block local landing.
pub(crate) async fn check_and_catch_up(application: &AppHandle, project: u32) {
    let _ = ensure_caught_up(application, project).await;
}

/// Serialized mirror catch-up that also reports whether the project's base is current
/// with its mirror, so local builds never pick a stale commit before catch-up (#551).
pub(crate) async fn ensure_caught_up(application: &AppHandle, project: u32) -> CatchUpCheck {
    if crate::rosen::paused(application) || !crate::rosen::rosen_in_use(application, project) {
        return CatchUpCheck::NotReady;
    }
    let Ok(server) = crate::rosen::observation_server(application, project) else {
        return CatchUpCheck::NotReady;
    };
    if !server.data_dir().join("metadata.sqlite").is_file() {
        return CatchUpCheck::NotReady;
    }
    let (moved, status) = check_and_catch_up_server(application, project, &server).await;
    if moved {
        let _ = refresh(application, project).await;
    }
    status
}

async fn check_and_catch_up_server(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
) -> (bool, CatchUpCheck) {
    let gate = application
        .state::<crate::rosen::ManagedRosen>()
        .catch_up_lock(project);
    let _guard = gate.lock().await;
    match velocity_rosen::probe_mirror_tip(server).await {
        Ok(MirrorTip::NotConfigured) => {
            set_mirror_behind(application, project, None);
            (false, CatchUpCheck::Current)
        }
        Ok(MirrorTip::UpToDate | MirrorTip::Ahead) => {
            set_mirror_behind(application, project, None);
            let _ = publish_divergence(application, project, None, None);
            (false, CatchUpCheck::Current)
        }
        Ok(MirrorTip::Behind {
            commits,
            base_commit,
            mirror_commit,
        }) => {
            catch_up_behind_server(
                application,
                project,
                server,
                commits,
                base_commit,
                mirror_commit,
            )
            .await
        }
        Ok(MirrorTip::Diverged { .. }) => {
            set_mirror_behind(application, project, None);
            match catch_up_within_wait_locked(application, project, server, false).await {
                Ok(progress) => (progress.moved, progress.check()),
                Err(_) => (false, CatchUpCheck::NotReady),
            }
        }
        Err(error) => {
            crate::diagnostics::record(
                "rosen mirror",
                &format!("Mirror tip check for project {project}: {error}"),
            );
            (false, CatchUpCheck::NotReady)
        }
    }
}

async fn catch_up_behind_server(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    commits: u32,
    base_commit: String,
    mirror_commit: String,
) -> (bool, CatchUpCheck) {
    set_mirror_behind(
        application,
        project,
        Some(MirrorBehind::new(
            commits,
            base_commit.clone(),
            mirror_commit.clone(),
            true,
            None,
        )),
    );
    match catch_up_within_wait_locked(application, project, server, false).await {
        Ok(progress) => (progress.moved, progress.check()),
        Err(error) => {
            set_mirror_behind(
                application,
                project,
                Some(MirrorBehind::new(
                    commits,
                    base_commit,
                    mirror_commit,
                    false,
                    Some(error.clone()),
                )),
            );
            crate::diagnostics::record(
                "rosen mirror",
                &format!("Catch-up from the mirror for project {project}: {error}"),
            );
            (false, CatchUpCheck::NotReady)
        }
    }
}

impl CatchUpProgress {
    fn check(self) -> CatchUpCheck {
        if self.diverged {
            CatchUpCheck::NotReady
        } else {
            CatchUpCheck::Current
        }
    }
}

/// Refresh on startup, mirror events and the periodic retry, one observation at a time.
pub(crate) async fn refresh(application: &AppHandle, project: u32) -> Result<(), String> {
    let server = crate::rosen::server(application, project).await?;
    let initial = velocity_rosen::mirror_status(&server)
        .await
        .map_err(|error| error.to_string())?;
    let deferred = maybe_recover_rejected(application, project, &server, initial.as_ref()).await;
    let running = application.state::<crate::rosen::ManagedRosen>();
    let _one_at_a_time = running.follow_ups.lock().await;
    let client = server.connect().await.map_err(|error| error.to_string())?;
    let head = velocity_rosen::EventHead::read(&client)
        .await
        .map_err(|error| error.to_string())?;
    let observation = velocity_rosen::mirror_status(&server)
        .await
        .map_err(|error| error.to_string())?;
    if observation
        .as_ref()
        .and_then(|item| item.state.as_ref())
        .is_none()
    {
        running.clear_rejection_recovery(project);
    }
    let stale_rejected = deferred.is_some_and(|sequence| {
        observation.as_ref().is_some_and(|item| {
            item.sequence == sequence && item.state.as_ref().is_some_and(|state| state.rejected)
        })
    });
    if stale_rejected {
        schedule_followup_refresh(application, project);
    } else {
        publish(application, project, head, observation)?;
    }
    let remote = velocity_rosen::mirror_remote(&server)
        .await
        .map_err(|error| error.to_string())?;
    // A configuration that could not be read keeps the saved answer, so a linked project
    // is not offered Send Collapse its live state again (#517).
    let live_state = velocity_rosen::collapse_link::collapse_root(&server)
        .await
        .ok()
        .map(|root| root.is_some());
    save_collapse_repository(
        application,
        project,
        remote
            .as_deref()
            .and_then(velocity_core::collapse::repository_from_remote),
        live_state,
    )?;
    let handle = application.clone();
    tauri::async_runtime::spawn_blocking(move || deliver_notices(&handle, project))
        .await
        .map_err(|error| error.to_string())?
}

/// Before publishing a rejected push as needing a person, tries catching up when the
/// remote is ahead and the server's base is an ancestor of its tip (#520).
async fn maybe_recover_rejected(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    initial: Option<&velocity_rosen::MirrorObservation>,
) -> Option<u64> {
    let observation = initial?;
    if !observation
        .state
        .as_ref()
        .is_some_and(|state| state.rejected)
        || crate::rosen::paused(application)
    {
        return None;
    }
    if !application
        .state::<crate::rosen::ManagedRosen>()
        .claim_rejection_recovery(project, observation.sequence)
    {
        return None;
    }
    recover_rejected_push(application, project, server, observation.sequence).await
}

async fn recover_rejected_push(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    sequence: u64,
) -> Option<u64> {
    let gate = application
        .state::<crate::rosen::ManagedRosen>()
        .catch_up_lock(project);
    let _guard = gate.lock().await;
    let behind = probe_and_show_behind(application, project, server).await;
    let outcome =
        match tokio::time::timeout(CATCH_UP_WAIT, velocity_rosen::catch_up_from_mirror(server))
            .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => {
                let message = error.to_string();
                mark_behind_pending(application, project, behind, message.clone());
                crate::diagnostics::record(
                    "rosen mirror",
                    &format!(
                        "Automatic catch-up after rejected push for project {project}: {message}"
                    ),
                );
                return None;
            }
            Err(_) => {
                mark_behind_pending(
                    application,
                    project,
                    behind,
                    "still running after two minutes; Rosen finishes it".into(),
                );
                return None;
            }
        };
    set_mirror_behind(application, project, None);
    let defer = matches!(outcome, CatchUp::UpToDate | CatchUp::Ahead);
    let _ = record_catch_up_outcome(application, project, outcome, false);
    defer.then_some(sequence)
}

fn schedule_followup_refresh(application: &AppHandle, project: u32) {
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(RECOVERED_REFRESH_DELAY).await;
        if !crate::rosen::paused(&handle) && crate::rosen::rosen_in_use(&handle, project) {
            report(refresh(&handle, project).await);
        }
    });
}

fn deliver_notices(application: &AppHandle, project: u32) -> Result<(), String> {
    let notices = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project)
    .map(|item| item.rosen_mirror_notices.clone())
    .unwrap_or_default();
    if notices.is_empty() {
        return Ok(());
    }
    // A channel outage must never hide the project warning. Save the notice before
    // posting it, retry it separately, and deduplicate a crash after the append.
    crate::state::save_now(application)?;
    for notice in notices {
        if let Some(state) = &notice.state {
            crate::airc_events::record_mirror_rejected(
                application,
                project,
                notice.sequence,
                &notice.name,
                state,
            )?;
        }
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .finish_mirror_notice(project, notice.sequence)
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn publish(
    application: &AppHandle,
    project: u32,
    head: velocity_rosen::EventHead,
    observation: Option<velocity_rosen::MirrorObservation>,
) -> Result<(), String> {
    let update = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let (sequence, state) = observation.map_or((0, None), |item| (item.sequence, item.state));
        store
            .resume_rosen_mirror(project, &head.identity, sequence, state.as_ref())
            .map_err(|error| error.to_string())?;
        let Some(item) = store.project(project) else {
            return Ok(());
        };
        if sequence < item.rosen_mirror_sequence {
            return Ok(());
        }
        let changed = item.rosen_mirror != state;
        let toast = (item.rosen_mirror_rejection_sequence.is_none()
            && state.as_ref().is_some_and(|state| state.rejected))
        .then(|| format!("{}: Rosen mirror rejected · needs a person", item.name));
        store
            .save_rosen_mirror(project, sequence, state)
            .map_err(|error| error.to_string())?;
        (changed || toast.is_some()).then(|| (store.layout_refresh(), toast))
    };
    if let Some((refresh, toast)) = update {
        let snapshot = {
            let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
            let _ = refresh.apply(&mut layout);
            if let Some(message) = toast {
                layout.show_toast(message, None);
            }
            layout.snapshot()
        };
        crate::layout::publish(application, &snapshot, None)?;
    }
    Ok(())
}

/// Shows or hides Open on Collapse when the mirror remote changes (#429), and Send
/// Collapse its live state when `rosen.toml` gains or loses its Collapse root (#517).
fn save_collapse_repository(
    application: &AppHandle,
    project: u32,
    repository: Option<String>,
    live_state: Option<bool>,
) -> Result<(), String> {
    let mirrored = repository.is_some();
    let refresh = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let changed = store
            .save_collapse_repository(project, repository, live_state)
            .map_err(|error| error.to_string())?;
        changed.then(|| store.layout_refresh())
    };
    if let Some(refresh) = refresh {
        crate::layout::apply_refresh_and_publish(application, refresh)?;
    }
    if mirrored {
        // A push just reached Collapse: read its land and forwarder state now (#427).
        crate::collapse_mirror::refresh_soon(application);
    }
    Ok(())
}

/// How long a project's first server start waits for its catch-up from the mirror. Rosen
/// finishes a catch-up it began even after Velocity stops waiting for it.
const CATCH_UP_WAIT: Duration = Duration::from_secs(120);

/// How long a catch-up that failed on start waits before it is tried again. Each later
/// wait doubles, up to [`CATCH_UP_RETRY_LIMIT`], because Rosen pauses landing while it
/// fetches the mirror.
const CATCH_UP_RETRY: Duration = Duration::from_secs(30);
const CATCH_UP_RETRY_LIMIT: Duration = Duration::from_secs(10 * 60);

/// Catches the project up from its mirror the first time this run readies its server
/// (#501), so the machine that sat idle lands fast-forward again with no manual step.
/// Concurrent first callers wait for the one catch-up; later calls return at once. A
/// catch-up that failed is tried again in the background until Rosen answers.
pub(crate) async fn catch_up_once(application: &AppHandle, project: u32, server: &ProjectServer) {
    let once = application
        .state::<crate::rosen::ManagedRosen>()
        .catch_up_once(project);
    once.get_or_init(|| async {
        let Err(error) = run_startup_catch_up(application, project, server).await else {
            return;
        };
        report_failed_catch_up(application, project, &error);
        application
            .state::<crate::rosen::ManagedRosen>()
            .begin_catch_up_retry(project);
        let (handle, server) = (application.clone(), server.clone());
        tauri::async_runtime::spawn(async move {
            retry_catch_up(&handle, project, &server, error).await;
        });
    })
    .await;
}

/// Runs startup catch-up serialized per project, showing "behind the mirror by N" while
/// the catch-up runs or waits to retry (#501, #520).
async fn run_startup_catch_up(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
) -> Result<(), String> {
    let gate = application
        .state::<crate::rosen::ManagedRosen>()
        .catch_up_lock(project);
    let _guard = gate.lock().await;
    match velocity_rosen::probe_mirror_tip(server).await {
        Ok(MirrorTip::NotConfigured) => {
            set_mirror_behind(application, project, None);
            let _ = application
                .state::<crate::rosen::ManagedRosen>()
                .end_catch_up_retry(project);
            Ok(())
        }
        Ok(MirrorTip::UpToDate | MirrorTip::Ahead) => {
            set_mirror_behind(application, project, None);
            let _ = publish_divergence(application, project, None, None);
            let _ = application
                .state::<crate::rosen::ManagedRosen>()
                .end_catch_up_retry(project);
            Ok(())
        }
        Ok(MirrorTip::Behind {
            commits,
            base_commit,
            mirror_commit,
        }) => {
            let behind = Some((commits, base_commit.clone(), mirror_commit.clone()));
            set_mirror_behind(
                application,
                project,
                Some(MirrorBehind::new(
                    commits,
                    base_commit,
                    mirror_commit,
                    true,
                    None,
                )),
            );
            let result = catch_up_within_wait_locked(application, project, server, false).await;
            if let Err(error) = &result {
                mark_behind_pending(application, project, behind, error.clone());
            }
            result.map(|_| ())
        }
        Ok(MirrorTip::Diverged { .. }) | Err(_) => {
            catch_up_within_wait_locked(application, project, server, false)
                .await
                .map(|_| ())
        }
    }
}

async fn catch_up_within_wait_locked(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    asked: bool,
) -> Result<CatchUpProgress, String> {
    let caught_up = catch_up_locked(application, project, server, asked);
    match tokio::time::timeout(CATCH_UP_WAIT, caught_up).await {
        Ok(result) => result,
        Err(_) => Err("still running after two minutes; Rosen finishes it".into()),
    }
}

/// Says once that the catch-up on start failed. A land before one gets through goes out
/// on a base behind the mirror, and the two histories then diverge.
fn report_failed_catch_up(application: &AppHandle, project: u32, error: &str) {
    crate::diagnostics::record(
        "rosen mirror",
        &format!("Catch-up from the mirror for project {project}: {error}"),
    );
    report(project_name(application, project).and_then(|name| {
        let message = format!("{name}: could not catch up from the mirror · trying again");
        toast(application, message)
    }));
}

/// Tries a catch-up that failed on start again until Rosen answers one, here or for the
/// project's action, or the project leaves Rosen. A mirror fetch that failed once, with
/// the network not up yet, would otherwise leave the base behind its mirror all run.
async fn retry_catch_up(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    mut last_error: String,
) {
    let mut wait = CATCH_UP_RETRY;
    loop {
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(CATCH_UP_RETRY_LIMIT);
        let retries = || {
            application
                .state::<crate::rosen::ManagedRosen>()
                .retries_catch_up(project)
        };
        if retries() && !crate::rosen::rosen_in_use(application, project) {
            application
                .state::<crate::rosen::ManagedRosen>()
                .end_catch_up_retry(project);
        }
        if !retries() {
            return;
        }
        // Connecting starts a stopped server, which an update or an exit must not find running.
        if crate::rosen::paused(application) {
            continue;
        }
        match run_startup_catch_up(application, project, server).await {
            // Rosen answered, and `catch_up_locked` ended the retries and said what happened.
            Ok(()) => return,
            Err(error) if error != last_error => {
                crate::diagnostics::record(
                    "rosen mirror",
                    &format!("Catch-up from the mirror for project {project}: {error}"),
                );
                last_error = error;
            }
            // The same failure again is already in the log.
            Err(_) => {}
        }
    }
}

/// What a health-triggered mirror push retry / catch-up did (#509).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MirrorRetryOutcome {
    UpToDate,
    RetriedPush,
    CaughtUp {
        commits: usize,
        new_base: String,
    },
    Diverged {
        base_commit: String,
        mirror_commit: String,
    },
    NotApplicable(String),
}

/// Retries a stalled mirror push or applies #501 catch-up when the mirror is ahead (#509).
/// Rosen's `mirror_catch_up` fetches the mirror, wakes its push worker when the base is
/// ahead, fast-forwards when the mirror is ahead, or returns both tips on divergence.
pub(crate) async fn retry_for_health(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
) -> Result<MirrorRetryOutcome, String> {
    let outcome = catch_up_for_health_locked(application, project, server).await?;
    let name = project_name(application, project)?;
    let (divergence, toast_message, result) = describe_health_retry(&name, outcome);
    let changed = publish_divergence(application, project, divergence, None)?;
    if let Some(message) = toast_message
        && (changed || matches!(result, MirrorRetryOutcome::CaughtUp { .. }))
    {
        let _ = toast(application, message);
    }
    let _ = refresh(application, project).await;
    Ok(result)
}

async fn catch_up_for_health_locked(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
) -> Result<CatchUp, String> {
    let gate = application
        .state::<crate::rosen::ManagedRosen>()
        .catch_up_lock(project);
    let _guard = gate.lock().await;
    let behind = probe_and_show_behind(application, project, server).await;
    match velocity_rosen::catch_up_from_mirror(server).await {
        Ok(outcome) => {
            set_mirror_behind(application, project, None);
            Ok(outcome)
        }
        Err(error) => {
            let message = error.to_string();
            mark_behind_pending(application, project, behind, message.clone());
            Err(message)
        }
    }
}

fn describe_health_retry(
    name: &str,
    outcome: CatchUp,
) -> (Option<MirrorDivergence>, Option<String>, MirrorRetryOutcome) {
    match outcome {
        CatchUp::NotApplicable(reason) => (None, None, MirrorRetryOutcome::NotApplicable(reason)),
        CatchUp::UpToDate => (None, None, MirrorRetryOutcome::UpToDate),
        CatchUp::Ahead => (None, None, MirrorRetryOutcome::RetriedPush),
        CatchUp::FastForwarded(moved) => {
            for failure in &moved.rebase_errors {
                crate::diagnostics::record("rosen mirror", &format!("{name}: {failure}"));
            }
            let message = format!(
                "{name}: caught up {} commit(s) from the mirror to {}",
                moved.commits,
                short(&moved.new_base)
            );
            let result = MirrorRetryOutcome::CaughtUp {
                commits: moved.commits,
                new_base: moved.new_base,
            };
            (None, Some(message), result)
        }
        CatchUp::Diverged(divergence) => {
            let result = MirrorRetryOutcome::Diverged {
                base_commit: divergence.base_commit.clone(),
                mirror_commit: divergence.mirror_commit.clone(),
            };
            let message = format!("{name}: Rosen base and mirror have diverged · needs a person");
            (Some(divergence), Some(message), result)
        }
    }
}

/// `catch_up_from_mirror` (§3.1): the project's Catch up from mirror action. Says what
/// happened in a toast, and shows a divergence in the project's mirror notice.
#[tauri::command]
#[specta::specta]
pub async fn catch_up_from_mirror(
    application: AppHandle,
    project_identifier: u32,
) -> Result<(), String> {
    let server = crate::rosen::server(&application, project_identifier).await?;
    {
        let gate = application
            .state::<crate::rosen::ManagedRosen>()
            .catch_up_lock(project_identifier);
        let _guard = gate.lock().await;
        let behind = probe_and_show_behind(&application, project_identifier, &server).await;
        if let Err(error) = catch_up_locked(&application, project_identifier, &server, true).await {
            mark_behind_pending(&application, project_identifier, behind, error.clone());
            return Err(error);
        }
    }
    refresh(&application, project_identifier).await
}

/// `adopt_mirror_tip` (§3.1): adopts the diverged mirror tip the project's notice shows,
/// for a mirror whose edits were already ported into Rosen (Rosen #396). Rust takes the
/// tip from the saved divergence; Rosen refuses a tip that moved or was not ported.
#[tauri::command]
#[specta::specta]
pub async fn adopt_mirror_tip(
    application: AppHandle,
    project_identifier: u32,
) -> Result<(), String> {
    let (name, divergence) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let item = store.project(project_identifier).ok_or("Unknown project")?;
        (item.name.clone(), item.rosen_mirror_divergence.clone())
    };
    let divergence = divergence.ok_or("This project's base and mirror have not diverged")?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    let base = velocity_rosen::adopt_mirror_tip(&server, &divergence.mirror_commit)
        .await
        .map_err(|error| error.to_string())?;
    let message = format!(
        "{name}: adopted mirror tip {} · Rosen base is now {}",
        short(&divergence.mirror_commit),
        short(&base)
    );
    set_mirror_behind(&application, project_identifier, None);
    publish_divergence(&application, project_identifier, None, Some(message))?;
    refresh(&application, project_identifier).await
}

/// Catches up while holding `catch_up_lock`, clears `MirrorBehind`, saves or clears the
/// divergence, and toasts what changed. Returns whether the base moved or diverged.
async fn catch_up_locked(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    asked: bool,
) -> Result<CatchUpProgress, String> {
    let outcome = velocity_rosen::catch_up_from_mirror(server)
        .await
        .map_err(|error| error.to_string())?;
    set_mirror_behind(application, project, None);
    record_catch_up_outcome(application, project, outcome, asked)
}

fn record_catch_up_outcome(
    application: &AppHandle,
    project: u32,
    outcome: CatchUp,
    asked: bool,
) -> Result<CatchUpProgress, String> {
    // Rosen answered, so a catch-up that failed on start needs no further tries, and the
    // owner who was told it failed is told how it ended.
    let asked = application
        .state::<crate::rosen::ManagedRosen>()
        .end_catch_up_retry(project)
        || asked;
    let name = project_name(application, project)?;
    if let CatchUp::FastForwarded(moved) = &outcome {
        for failure in &moved.rebase_errors {
            crate::diagnostics::record("rosen mirror", &format!("{name}: {failure}"));
        }
    }
    let (divergence, message, moved) = describe(&name, outcome);
    let diverged = divergence.clone();
    let is_diverged = diverged.is_some();
    let changed = publish_divergence(application, project, divergence, None)?;
    if asked || moved || (is_diverged && changed) {
        toast(application, message)?;
    }
    if let Some(divergence) = diverged.filter(|_| changed) {
        crate::diagnostics::record("rosen mirror", &format!("{name}: {}", divergence.detail));
    }
    Ok(CatchUpProgress {
        moved,
        diverged: is_diverged,
    })
}

async fn probe_and_show_behind(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
) -> Option<(u32, String, String)> {
    let Ok(MirrorTip::Behind {
        commits,
        base_commit,
        mirror_commit,
    }) = velocity_rosen::probe_mirror_tip(server).await
    else {
        return None;
    };
    set_mirror_behind(
        application,
        project,
        Some(MirrorBehind::new(
            commits,
            base_commit.clone(),
            mirror_commit.clone(),
            true,
            None,
        )),
    );
    Some((commits, base_commit, mirror_commit))
}

fn mark_behind_pending(
    application: &AppHandle,
    project: u32,
    behind: Option<(u32, String, String)>,
    error: String,
) {
    if let Some((commits, base_commit, mirror_commit)) = behind {
        set_mirror_behind(
            application,
            project,
            Some(MirrorBehind::new(
                commits,
                base_commit,
                mirror_commit,
                false,
                Some(error),
            )),
        );
    }
}

fn set_mirror_behind(application: &AppHandle, project: u32, behind: Option<MirrorBehind>) {
    let Ok(mut layout) = lock(application.state::<MainLayout>().inner(), "layout") else {
        return;
    };
    if layout.set_rosen_mirror_behind(project, behind) {
        report(crate::layout::publish(
            application,
            &layout.snapshot(),
            None,
        ));
    }
}

/// The divergence to keep, the toast, and whether the base moved.
fn describe(name: &str, outcome: CatchUp) -> (Option<MirrorDivergence>, String, bool) {
    match outcome {
        CatchUp::NotApplicable(reason) => (
            None,
            format!("{name}: nothing to catch up · {reason}"),
            false,
        ),
        CatchUp::UpToDate => (
            None,
            format!("{name}: Rosen base matches its mirror"),
            false,
        ),
        CatchUp::Ahead => (
            None,
            format!("{name}: Rosen base is ahead of its mirror · the next push sends it"),
            false,
        ),
        CatchUp::FastForwarded(moved) => {
            let mut message = format!(
                "{name}: caught up {} commit(s) from the mirror to {} · {} open change(s) rebased",
                moved.commits,
                short(&moved.new_base),
                moved.rebased
            );
            if moved.conflicted > 0 {
                message.push_str(&format!(", {} with conflicts", moved.conflicted));
            }
            if !moved.rebase_errors.is_empty() {
                let count = moved.rebase_errors.len();
                message.push_str(&format!(" · {count} rebase when they land"));
            }
            (None, message, true)
        }
        CatchUp::Diverged(divergence) => (
            Some(divergence),
            format!("{name}: Rosen base and mirror have diverged · needs a person"),
            false,
        ),
    }
}

/// Saves the divergence and republishes the window when it changed or `message` is a
/// toast to show. Reports whether the divergence changed.
fn publish_divergence(
    application: &AppHandle,
    project: u32,
    divergence: Option<MirrorDivergence>,
    message: Option<String>,
) -> Result<bool, String> {
    let (changed, refresh) = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let changed = store
            .save_mirror_divergence(project, divergence)
            .map_err(|error| error.to_string())?;
        let refresh = (changed || message.is_some()).then(|| store.layout_refresh());
        (changed, refresh)
    };
    if let Some(refresh) = refresh {
        let snapshot = {
            let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
            let _ = refresh.apply(&mut layout);
            if let Some(message) = message {
                layout.show_toast(message, None);
            }
            layout.snapshot()
        };
        crate::layout::publish(application, &snapshot, None)?;
    }
    Ok(changed)
}

fn project_name(application: &AppHandle, project: u32) -> Result<String, String> {
    Ok(lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project)
    .map(|item| item.name.clone())
    .unwrap_or_default())
}

fn toast(application: &AppHandle, message: String) -> Result<(), String> {
    let snapshot = {
        let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
        layout.show_toast(message, None);
        layout.snapshot()
    };
    crate::layout::publish(application, &snapshot, None)
}

/// A commit's first twelve characters, as toasts name it.
fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
