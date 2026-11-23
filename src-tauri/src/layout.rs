use crate::state::ManagedStateStore;
use std::panic::Location;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, Window, Wry};
use tauri_specta::Event;
use velocity_core::board::BoardLane;
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{
    LayoutAction, LayoutChrome, LayoutDelta, LayoutIntent, LayoutSnapshot, PaletteSlice, RailSlice,
    SidebarSlice,
};
use velocity_core::navigation::{NavigationIntent, NavigationView};
use velocity_core::thread_view_model::{ThreadDetailMeta, TranscriptUpdate};
use velocity_interview::Interviews;

pub type MainLayout = Mutex<LayoutState>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "layout://state")]
pub struct LayoutStateChanged {
    pub state: LayoutSnapshot,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "layout://delta")]
pub struct LayoutDeltaChanged {
    pub delta: LayoutDelta,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "layout://intent")]
pub struct LayoutIntentRequested {
    pub intent: LayoutIntent,
}

/// Last published metadata baseline used to build minimal [`LayoutDelta`] payloads (#553).
/// Intentionally omits `active_thread.blocks` so `LiveThreadView::blocks` keeps a unique
/// `Arc` reference count during streaming and mutates in place without copying.
struct PublishedState {
    revision: u32,
    chrome: LayoutChrome,
    sidebar: SidebarSlice,
    board: Vec<BoardLane>,
    rail: RailSlice,
    palette: PaletteSlice,
    active_thread_id: Option<usize>,
    active_thread_meta: Option<ThreadDetailMeta>,
    active_thread_transcript_revision: u32,
    has_active_duty: bool,
    has_active_swarm: bool,
    has_active_goal_swarm: bool,
}

impl PublishedState {
    fn from_snapshot(snapshot: &LayoutSnapshot) -> Self {
        Self {
            revision: snapshot.revision,
            chrome: LayoutChrome::from_snapshot(snapshot),
            sidebar: SidebarSlice {
                groups: snapshot.groups.clone(),
                pulse: snapshot.pulse.clone(),
            },
            board: if snapshot.view == NavigationView::Board {
                snapshot.board.clone()
            } else {
                Vec::new()
            },
            rail: RailSlice::from_snapshot(snapshot),
            palette: PaletteSlice::from_snapshot(snapshot),
            active_thread_id: snapshot
                .active_thread
                .as_ref()
                .map(|detail| detail.identifier),
            active_thread_meta: snapshot
                .active_thread
                .as_ref()
                .map(ThreadDetailMeta::from_snapshot),
            active_thread_transcript_revision: snapshot
                .active_thread
                .as_ref()
                .map_or(0, |detail| detail.transcript_revision),
            has_active_duty: snapshot.active_duty.is_some(),
            has_active_swarm: snapshot.active_swarm.is_some(),
            has_active_goal_swarm: snapshot.active_goal_swarm.is_some(),
        }
    }
}

fn published_slot() -> &'static Mutex<Option<PublishedState>> {
    static PUBLISHED: OnceLock<Mutex<Option<PublishedState>>> = OnceLock::new();
    PUBLISHED.get_or_init(|| Mutex::new(None))
}

fn is_stale_revision(previous: u32, candidate: u32) -> bool {
    (previous.wrapping_sub(candidate) as i32) > 0
}

fn diff_overlay<Value: Clone>(
    active_view: NavigationView,
    expected: NavigationView,
    had_previous: bool,
    current: &Option<Value>,
) -> (Option<Value>, bool) {
    let next = if active_view == expected {
        current.clone()
    } else {
        None
    };
    let cleared = had_previous && next.is_none();
    (next, cleared)
}

fn diff_layout_delta(previous: &PublishedState, snapshot: &LayoutSnapshot) -> LayoutDelta {
    let next_chrome = LayoutChrome::from_snapshot(snapshot);
    let chrome = (next_chrome != previous.chrome).then_some(next_chrome);

    let next_sidebar = SidebarSlice {
        groups: snapshot.groups.clone(),
        pulse: snapshot.pulse.clone(),
    };
    let sidebar = (next_sidebar != previous.sidebar).then_some(next_sidebar);

    let board = if snapshot.view == NavigationView::Board
        && (previous.chrome.view != NavigationView::Board || snapshot.board != previous.board)
    {
        Some(snapshot.board.clone())
    } else {
        None
    };

    let next_rail = RailSlice::from_snapshot(snapshot);
    let rail = (next_rail != previous.rail).then_some(next_rail);

    let next_palette = PaletteSlice::from_snapshot(snapshot);
    let palette = (next_palette != previous.palette).then_some(next_palette);

    let (active_thread_meta, active_thread_blocks, active_thread_cleared) =
        diff_active_thread(previous, snapshot);
    let (active_duty, active_duty_cleared) = diff_overlay(
        snapshot.view,
        NavigationView::Duty,
        previous.has_active_duty,
        &snapshot.active_duty,
    );
    let (active_swarm, active_swarm_cleared) = diff_overlay(
        snapshot.view,
        NavigationView::Swarm,
        previous.has_active_swarm,
        &snapshot.active_swarm,
    );
    let (active_goal_swarm, active_goal_swarm_cleared) = diff_overlay(
        snapshot.view,
        NavigationView::Swarm,
        previous.has_active_goal_swarm,
        &snapshot.active_goal_swarm,
    );

    LayoutDelta {
        base_revision: previous.revision,
        revision: snapshot.revision,
        chrome,
        sidebar,
        board,
        rail,
        palette,
        active_thread_meta,
        active_thread_blocks,
        active_thread_cleared,
        active_duty,
        active_duty_cleared,
        active_swarm,
        active_swarm_cleared,
        active_goal_swarm,
        active_goal_swarm_cleared,
    }
}

fn diff_active_thread(
    previous: &PublishedState,
    snapshot: &LayoutSnapshot,
) -> (Option<ThreadDetailMeta>, Option<TranscriptUpdate>, bool) {
    let Some(detail) = &snapshot.active_thread else {
        return (None, None, previous.active_thread_id.is_some());
    };
    let next_meta = ThreadDetailMeta::from_snapshot(detail);
    let meta_changed = previous.active_thread_meta.as_ref() != Some(&next_meta);
    let blocks_changed = previous.active_thread_id != Some(detail.identifier)
        || previous.active_thread_transcript_revision != detail.transcript_revision;
    let blocks = blocks_changed.then(|| TranscriptUpdate::Full {
        revision: detail.transcript_revision,
        blocks: detail.blocks.clone(),
    });
    (meta_changed.then_some(next_meta), blocks, false)
}

fn empty_delta(revision: u32) -> LayoutDelta {
    LayoutDelta {
        base_revision: revision,
        revision,
        chrome: None,
        sidebar: None,
        board: None,
        rail: None,
        palette: None,
        active_thread_meta: None,
        active_thread_blocks: None,
        active_thread_cleared: false,
        active_duty: None,
        active_duty_cleared: false,
        active_swarm: None,
        active_swarm_cleared: false,
        active_goal_swarm: None,
        active_goal_swarm_cleared: false,
    }
}

fn seed_published(snapshot: &LayoutSnapshot) {
    let mut last = published_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stale = last
        .as_ref()
        .is_some_and(|previous| is_stale_revision(previous.revision, snapshot.revision));
    if !stale {
        *last = Some(PublishedState::from_snapshot(snapshot));
    }
}

/// Emits a thread-scoped transcript delta and advances the published transcript baseline
/// so subsequent layout deltas skip re-serializing `active_thread.blocks` (#553).
pub(crate) fn publish_thread_transcript(
    application: &AppHandle,
    event: crate::threads::ThreadTranscriptChanged,
) -> Result<(), String> {
    let mut last = published_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(previous) = last.as_mut()
        && previous.chrome.project_identifier == Some(event.project_identifier)
        && previous.active_thread_id == Some(event.thread_identifier)
        && previous.active_thread_transcript_revision == event.base_transcript_revision
    {
        previous.active_thread_transcript_revision = event.transcript_revision;
        if let Some(meta) = &mut previous.active_thread_meta {
            if let Some(added) = event.added {
                meta.added = added as usize;
            }
            if let Some(deleted) = event.deleted {
                meta.deleted = deleted as usize;
            }
            if let Some(busy) = event.agent_busy {
                meta.agent_busy = busy;
            }
            if let Some((titling, ref model)) = event.titling {
                meta.titling = titling;
                meta.title_model_name.clone_from(model);
            }
        }
    }
    event
        .emit(application)
        .map_err(|error| format!("Could not publish transcript delta: {error}"))
}

// Commands wait for the store lock, which an agent's update can hold for a while. A
// synchronous command runs on the window's UI thread, and waiting there froze the whole
// window while a swarm streamed (#273, #286), so every command that takes a lock or does
// I/O waits on the blocking pool instead, through `off_ui_thread`.

#[tauri::command]
#[specta::specta]
pub async fn get_layout_state(window: Window) -> Result<LayoutSnapshot, String> {
    run_concurrently("get_layout_state", move || read_layout(&window)).await
}

#[tauri::command]
#[specta::specta]
pub async fn dispatch_layout(window: Window, action: LayoutAction) -> Result<LayoutDelta, String> {
    run_concurrently("dispatch_layout", move || dispatch(&window, action)).await
}

/// Runs a UI command on Tokio's blocking pool without queuing behind the serial command
/// thread, recording per-command latency under `command` (#273, #554).
pub(crate) async fn run_concurrently<Value: Send + 'static>(
    command: &'static str,
    work: impl FnOnce() -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
    let started = Instant::now();
    let outcome = tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| format!("Main-window state task failed: {error}"))?;
    velocity_core::performance::record_ui_command(command, started.elapsed());
    outcome
}

type CommandBody = Box<dyn FnOnce() + Send>;

/// Runs a command's body off the window's UI thread, on one command thread, one at a
/// time and in the order the window sent them. On the UI thread they ran in order, and
/// some depend on it: two model choices for one thread save the model and reconfigure
/// the agent in separate steps, which side by side could interleave (#286).
/// Not `async`: `#[track_caller]` does nothing on an async fn, so the caller is read here,
/// synchronously at the call, and the labeled future is returned (#557).
#[track_caller]
pub(crate) fn off_ui_thread<Value: Send + 'static>(
    work: impl FnOnce() -> Result<Value, String> + Send + 'static,
) -> impl Future<Output = Result<Value, String>> {
    let label = velocity_core::performance::caller_label(Location::caller());
    off_ui_thread_labeled(label, work)
}

/// Same as [`off_ui_thread`], recording command latency under an explicit IPC command name.
pub(crate) async fn off_ui_thread_named<Value: Send + 'static>(
    command: &'static str,
    work: impl FnOnce() -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
    off_ui_thread_labeled(command.to_owned(), work).await
}

async fn off_ui_thread_labeled<Value: Send + 'static>(
    label: String,
    work: impl FnOnce() -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
    static COMMANDS: OnceLock<Mutex<mpsc::Sender<CommandBody>>> = OnceLock::new();
    let started = Instant::now();
    let commands = COMMANDS.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<CommandBody>();
        std::thread::spawn(move || {
            for body in receiver {
                body();
            }
        });
        Mutex::new(sender)
    });
    let (reply, result) = tokio::sync::oneshot::channel();
    let body: CommandBody = Box::new(move || {
        // A panicking command fails its own request, not every command after it.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .unwrap_or_else(|_| Err("The command failed unexpectedly".into()));
        let _ignored = reply.send(outcome);
    });
    let sent = commands
        .lock()
        .map_err(|error| error.to_string())?
        .send(body)
        .map_err(|_| String::from("The command thread has stopped"));
    if let Err(error) = sent {
        velocity_core::performance::record_ui_command(&label, started.elapsed());
        return Err(error);
    }
    let outcome = result
        .await
        .map_err(|_| String::from("The command thread has stopped"));
    velocity_core::performance::record_ui_command(&label, started.elapsed());
    outcome?
}

fn read_layout(window: &Window) -> Result<LayoutSnapshot, String> {
    let spec_locked = {
        let store = window.state::<ManagedStateStore>();
        let store = store.lock().map_err(|error| error.to_string())?;
        store.active_project().map(|project| project.spec_locked)
    };
    let selected = {
        let state = window.state::<MainLayout>();
        let mut state = state
            .lock()
            .map_err(|error| format!("Could not read main-window state: {error}"))?;
        state.set_project_spec_locked(spec_locked);
        state.selected_thread_key()
    };
    if let Some((project, identifier)) = selected {
        let _ = crate::threads::sync_selected_live_view(window.app_handle(), project, identifier);
    }
    let snapshot = {
        let state = window.state::<MainLayout>();
        let state = state
            .lock()
            .map_err(|error| format!("Could not read main-window state: {error}"))?;
        state.snapshot()
    };
    seed_published(&snapshot);
    crate::collapse::flush_pending_link(window.app_handle());
    Ok(snapshot)
}

type ProjectSelectionPublication = (
    velocity_store::projects::ProjectList,
    velocity_store::projects::ProjectSelectionView,
);

type DispatchSnapshot = (
    LayoutSnapshot,
    Option<(u32, usize)>,
    Option<ProjectSelectionPublication>,
);

/// Hydrates any uncached settled transcripts outside both locks, restores the switched
/// project into `MainLayout`, overlays live interviews, and snapshots (#554, #669, #698).
pub(crate) fn apply_project_switch(
    application: &AppHandle,
    mut switched: velocity_store::projects::ProjectSwitchData,
) -> Result<DispatchSnapshot, String> {
    switched.refresh.hydrate_from_disk();
    let layout = application.state::<MainLayout>();
    let mut state = layout
        .lock()
        .map_err(|error| format!("Could not update main-window state: {error}"))?;
    let applied = state.restore_store_project(
        switched.refresh.revision,
        switched.refresh.data,
        switched.selected_thread,
        switched.search,
        switched.refresh.spec_locked,
    );
    if applied {
        velocity_interview::overlay_live_for_project(
            application.state::<Interviews>().inner(),
            switched.refresh.active_project,
            &mut state,
        );
    }
    Ok((
        state.snapshot(),
        state.selected_thread_key(),
        applied.then_some((switched.projects, switched.selection)),
    ))
}

/// Finishes a layout dispatch after `ManagedStateStore` and `MainLayout` have been dropped,
/// reading any uncached settled transcripts from disk outside both locks before re-acquiring
/// only `MainLayout` to hydrate blocks and build the snapshot (#669).
pub(crate) fn finish_dispatch(
    application: &AppHandle,
    project_switch: Option<velocity_store::projects::ProjectSwitchData>,
    pending_hydration: Option<velocity_store::projects::PendingTranscriptHydration>,
) -> Result<DispatchSnapshot, String> {
    if let Some(switched) = project_switch {
        return apply_project_switch(application, switched);
    }
    let loaded = pending_hydration.map(|hydration| hydration.load_from_disk());
    let layout = application.state::<MainLayout>();
    let mut state = layout
        .lock()
        .map_err(|error| format!("Could not update main-window state: {error}"))?;
    if let Some(loaded) = loaded {
        loaded.apply(&mut state);
    }
    Ok((state.snapshot(), state.selected_thread_key(), None))
}

fn dispatch(window: &Window, action: LayoutAction) -> Result<LayoutDelta, String> {
    let (previous_selection, outcome, fast_snapshot) = {
        let store = window.state::<ManagedStateStore>();
        let state = window.state::<MainLayout>();
        let mut store = store.lock().map_err(|error| error.to_string())?;
        let mut state = state
            .lock()
            .map_err(|error| format!("Could not update main-window state: {error}"))?;
        let previous_selection = state.selected_thread_key();
        let outcome = store
            .dispatch_project_layout(&mut state, action)
            .map_err(|error| error.to_string())?;
        // Release the store lock before snapshotting, disk hydration, or project-switch restore.
        drop(store);
        let fast_snapshot = (outcome.project_switch.is_none()
            && outcome.pending_hydration.is_none())
        .then(|| (state.snapshot(), state.selected_thread_key()));
        (previous_selection, outcome, fast_snapshot)
    };
    let (mut snapshot, next_selection, project_selection) = match fast_snapshot {
        Some((snapshot, next_selection)) => (snapshot, next_selection, None),
        None => finish_dispatch(
            window.app_handle(),
            outcome.project_switch,
            outcome.pending_hydration,
        )?,
    };
    if next_selection != previous_selection
        && let Some((project, identifier)) = next_selection
        && let Some(synced) =
            crate::threads::sync_selected_live_view(window.app_handle(), project, identifier)?
    {
        snapshot = synced;
    }
    if let Some((project, task)) = outcome.task_added {
        crate::tasks::record_added(window.app_handle(), project, &task);
    }
    // The interview consumes spec-thread intents; build-thread intents remain queued.
    let intent = outcome
        .intent
        .filter(|intent| !crate::interview::handle_intent(window.app_handle(), intent))
        .filter(|intent| !crate::issue_panel::handle_intent(window.app_handle(), intent))
        .filter(|intent| !crate::collapse::handle_intent(window.app_handle(), intent));
    let delta = publish_returning_delta(window, &snapshot, intent.clone())?;
    if let Some((role, event)) = outcome.duty_event {
        crate::sweeps::instruct(window.app_handle(), role, &event.message);
        crate::duty::publish_duty_event(window, role, event)?;
    }
    if let Some((projects, selection)) = project_selection {
        crate::projects::publish_selection(window, projects, selection)?;
    }
    // Opening the modal marks the channel read now, not at the watcher's next tick.
    if let Some(LayoutIntent::Navigation(NavigationIntent::Airc)) = &intent {
        crate::threads::report(crate::airc_modal::refresh(window.app_handle()));
    }
    carry_out(
        window.app_handle(),
        intent,
        snapshot.project_identifier,
        delta,
    )
}

/// Starting or steering a thread publishes its own newer delta.
fn carry_out(
    application: &AppHandle,
    intent: Option<LayoutIntent>,
    project_identifier: Option<u32>,
    delta: LayoutDelta,
) -> Result<LayoutDelta, String> {
    match intent {
        Some(
            intent @ (LayoutIntent::SteerThread { .. } | LayoutIntent::RemoveQueuedMessage { .. }),
        ) => {
            // The reducer has already cleared the composer and added the message. The
            // window draws nothing while a request is pending, so waiting here for the
            // agent, a resume and their saves left the message in the composer until the
            // agent replied (#318). Withdrawing a queued message goes through the same
            // queue, so it never overtakes the send that put the message there.
            steer_in_background(application, project_identifier, intent);
            Ok(delta)
        }
        Some(intent) => {
            // `threads::handle_intent` publishes its own newer delta when it mutates state.
            let _ = crate::threads::handle_intent(application, &intent)?;
            Ok(delta)
        }
        None => Ok(delta),
    }
}

/// Sends thread messages on one background thread, in the order they were typed.
fn steer_in_background(application: &AppHandle, project: Option<u32>, intent: LayoutIntent) {
    type Queued = (AppHandle, Option<u32>, LayoutIntent);
    static QUEUE: OnceLock<Mutex<mpsc::Sender<Queued>>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Queued>();
        std::thread::spawn(move || {
            for (application, project, intent) in receiver {
                deliver_steer(&application, project, &intent);
            }
        });
        Mutex::new(sender)
    });
    let unsent = match queue.lock() {
        Ok(sender) => sender
            .send((application.clone(), project, intent))
            .err()
            .map(|error| error.0.2),
        Err(_) => Some(intent),
    };
    if let Some(intent) = unsent {
        deliver_steer(application, project, &intent);
    }
}

fn deliver_steer(application: &AppHandle, project: Option<u32>, intent: &LayoutIntent) {
    if let LayoutIntent::RemoveQueuedMessage { identifier, index } = intent {
        let removed = match project {
            Some(project) => {
                crate::threads::remove_queued_message(application, project, *identifier, *index)
            }
            None => Err("Select a project first".into()),
        };
        if let Err(error) = removed {
            show_toast(application, error);
        }
        return;
    }
    let LayoutIntent::SteerThread {
        identifier, prompt, ..
    } = intent
    else {
        return;
    };
    // Sent to the project the message was written in, even after a project switch.
    let delivered = match project {
        Some(project) => crate::threads::deliver_message(application, project, *identifier, prompt),
        None => Err("Select or create a project first".into()),
    };
    let Err(error) = delivered else {
        if let (Some(project), Ok(mut state)) = (project, application.state::<MainLayout>().lock())
        {
            state.message_delivered(project, *identifier, prompt);
        }
        return;
    };
    // A steer error means the agent never got the message: a send that reached it reports
    // later failures without an error (#274). Put it back in the composer and say why.
    let snapshot = {
        let managed = application.state::<MainLayout>();
        let Ok(mut state) = managed.lock() else {
            return;
        };
        if let Some(project) = project {
            state.return_unsent_message(project, *identifier, prompt);
        }
        state.show_toast(error, None);
        state.snapshot()
    };
    crate::threads::report(publish(application, &snapshot, None));
}

pub(crate) fn show_toast(application: &AppHandle, message: String) {
    let snapshot = {
        let managed = application.state::<MainLayout>();
        let Ok(mut state) = managed.lock() else {
            return;
        };
        state.show_toast(message, None);
        state.snapshot()
    };
    crate::threads::report(publish(application, &snapshot, None));
}

/// Applies pre-extracted store data to `MainLayout`, drops the layout lock, and emits the
/// resulting delta without ever holding `ManagedStateStore` across layout or emit (#554).
pub(crate) fn apply_refresh_and_publish(
    application: &AppHandle,
    mut refresh: velocity_store::projects::LayoutRefresh,
) -> Result<(), String> {
    refresh.hydrate_from_disk();
    let snapshot = {
        let managed = application.state::<MainLayout>();
        let mut layout = managed
            .lock()
            .map_err(|error| format!("Could not read main-window state: {error}"))?;
        if !refresh.apply(&mut layout) {
            return Ok(());
        }
        layout.snapshot()
    };
    publish(application, &snapshot, None)
}

/// Extracts fresh store data, drops the store lock, updates `MainLayout`, drops the layout
/// lock, and publishes the resulting delta (#554).
pub(crate) fn refresh_and_publish(application: &AppHandle) -> Result<(), String> {
    let refresh = {
        let managed = application.state::<ManagedStateStore>();
        let store = managed.lock().map_err(|error| error.to_string())?;
        store.layout_refresh()
    };
    apply_refresh_and_publish(application, refresh)
}

/// Applies `change` over fresh store data without holding `ManagedStateStore` across layout
/// work or `MainLayout` across event emission (#554).
pub(crate) fn refresh_with_change_and_publish(
    application: &AppHandle,
    change: impl FnOnce(&mut LayoutState),
) -> Result<LayoutSnapshot, String> {
    let mut refresh = {
        let managed = application.state::<ManagedStateStore>();
        let store = managed.lock().map_err(|error| error.to_string())?;
        store.layout_refresh()
    };
    refresh.hydrate_from_disk();
    let snapshot = {
        let managed = application.state::<MainLayout>();
        let mut layout = managed
            .lock()
            .map_err(|error| format!("Could not read main-window state: {error}"))?;
        let _ = refresh.apply(&mut layout);
        change(&mut layout);
        layout.snapshot()
    };
    publish(application, &snapshot, None)?;
    Ok(snapshot)
}

/// How often `publish_soon` sends the window its latest state delta at most.
const PUBLISH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Sends the window the latest changed slices within `PUBLISH_INTERVAL`, coalescing every
/// change made in between into one delta (#319, #553).
pub fn publish_soon(application: &AppHandle) {
    static PENDING: AtomicBool = AtomicBool::new(false);
    static PUBLISHER: OnceLock<()> = OnceLock::new();
    PENDING.store(true, Ordering::Release);
    PUBLISHER.get_or_init(|| {
        let application = application.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(PUBLISH_INTERVAL);
                if PENDING.swap(false, Ordering::AcqRel) {
                    crate::threads::report(publish_latest(&application));
                }
            }
        });
    });
}

fn publish_latest(application: &AppHandle) -> Result<(), String> {
    let snapshot = {
        let managed = application.state::<MainLayout>();
        let layout = managed.lock().map_err(|error| error.to_string())?;
        layout.snapshot()
    };
    publish(application, &snapshot, None)
}

pub fn publish<Handle: Emitter<Wry> + Manager<Wry>>(
    window: &Handle,
    snapshot: &LayoutSnapshot,
    intent: Option<LayoutIntent>,
) -> Result<(), String> {
    let _ = publish_returning_delta(window, snapshot, intent)?;
    Ok(())
}

fn publish_returning_delta<Handle: Emitter<Wry> + Manager<Wry>>(
    window: &Handle,
    snapshot: &LayoutSnapshot,
    intent: Option<LayoutIntent>,
) -> Result<LayoutDelta, String> {
    // Serialize only emission (never under store/layout locks) so concurrent publishers
    // deliver deltas in non-decreasing revision order and drop stale ones (#553, #554).
    let mut last = published_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let delta = match last.as_ref() {
        None => {
            LayoutStateChanged {
                state: snapshot.clone(),
            }
            .emit(window)
            .map_err(|error| format!("Could not publish main-window state: {error}"))?;
            *last = Some(PublishedState::from_snapshot(snapshot));
            empty_delta(snapshot.revision)
        }
        Some(previous) if is_stale_revision(previous.revision, snapshot.revision) => {
            empty_delta(previous.revision)
        }
        Some(previous) => {
            let delta = diff_layout_delta(previous, snapshot);
            if delta.is_empty() {
                empty_delta(previous.revision)
            } else {
                LayoutDeltaChanged {
                    delta: delta.clone(),
                }
                .emit(window)
                .map_err(|error| format!("Could not publish main-window delta: {error}"))?;
                *last = Some(PublishedState::from_snapshot(snapshot));
                delta
            }
        }
    };
    drop(last);
    if let Some(intent) = intent {
        LayoutIntentRequested { intent }
            .emit(window)
            .map_err(|error| format!("Could not publish navigation intent: {error}"))?;
    }
    Ok(delta)
}
