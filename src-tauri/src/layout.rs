use crate::state::ManagedStateStore;
use std::sync::{Mutex, OnceLock, mpsc};
use tauri::{AppHandle, Emitter, Manager, Window, Wry};
use tauri_specta::Event;
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutIntent, LayoutSnapshot};
use velocity_core::navigation::NavigationIntent;
use velocity_interview::Interviews;
use velocity_store::build_gate;

pub type MainLayout = Mutex<LayoutState>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "layout://state")]
pub struct LayoutStateChanged {
    pub state: LayoutSnapshot,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "layout://intent")]
pub struct LayoutIntentRequested {
    pub intent: LayoutIntent,
}

// Commands wait for the store lock, which an agent's update can hold for a while. A
// synchronous command runs on the window's UI thread, and waiting there froze the whole
// window while a swarm streamed (#273, #286), so every command that takes a lock or does
// I/O waits on the blocking pool instead, through `off_ui_thread`.

#[tauri::command]
#[specta::specta]
pub async fn get_layout_state(window: Window) -> Result<LayoutSnapshot, String> {
    concurrently(move || read_layout(&window)).await
}

#[tauri::command]
#[specta::specta]
pub async fn dispatch_layout(
    window: Window,
    action: LayoutAction,
) -> Result<LayoutSnapshot, String> {
    concurrently(move || dispatch(&window, action)).await
}

/// The layout commands take only the store and layout locks, in that order, so they run
/// side by side on the blocking pool and never wait behind a slower command (#273).
async fn concurrently<Value: Send + 'static>(
    work: impl FnOnce() -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| format!("Main-window state task failed: {error}"))?
}

type CommandBody = Box<dyn FnOnce() + Send>;

/// Runs a command's body off the window's UI thread, on one command thread, one at a
/// time and in the order the window sent them. On the UI thread they ran in order, and
/// some depend on it: two model choices for one thread save the model and reconfigure
/// the agent in separate steps, which side by side could interleave (#286).
pub(crate) async fn off_ui_thread<Value: Send + 'static>(
    work: impl FnOnce() -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
    static COMMANDS: OnceLock<Mutex<mpsc::Sender<CommandBody>>> = OnceLock::new();
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
    commands
        .lock()
        .map_err(|error| error.to_string())?
        .send(body)
        .map_err(|_| String::from("The command thread has stopped"))?;
    result
        .await
        .map_err(|_| String::from("The command thread has stopped"))?
}

fn read_layout(window: &Window) -> Result<LayoutSnapshot, String> {
    let store = window.state::<ManagedStateStore>();
    let state = window.state::<MainLayout>();
    let store = store.lock().map_err(|error| error.to_string())?;
    let mut state = state
        .lock()
        .map_err(|error| format!("Could not read main-window state: {error}"))?;
    Ok(build_gate::layout_snapshot(store.state(), &mut state))
}

fn dispatch(window: &Window, action: LayoutAction) -> Result<LayoutSnapshot, String> {
    let store = window.state::<ManagedStateStore>();
    let state = window.state::<MainLayout>();
    let mut store = store.lock().map_err(|error| error.to_string())?;
    let mut state = state
        .lock()
        .map_err(|error| format!("Could not update main-window state: {error}"))?;
    let outcome = store
        .dispatch_project_layout(&mut state, action)
        .map_err(|error| error.to_string())?;
    if outcome.selection.is_some() {
        // A project switch rebuilds the window from state.json; put live interviews back.
        velocity_interview::overlay_live(window.state::<Interviews>().inner(), &store, &mut state);
    }
    let snapshot = state.snapshot();
    let project_selection = outcome
        .selection
        .map(|selection| (store.list_projects(), selection));
    drop(state);
    drop(store);
    if let Some((project, task)) = outcome.task_added {
        crate::tasks::record_added(window.app_handle(), project, &task);
    }
    // The interview consumes spec-thread intents; build-thread intents remain queued.
    let intent = outcome
        .intent
        .filter(|intent| !crate::interview::handle_intent(window.app_handle(), intent))
        .filter(|intent| !crate::issue_panel::handle_intent(window.app_handle(), intent));
    publish(window, &snapshot, intent.clone())?;
    if let Some((role, event)) = outcome.duty_event {
        crate::reviews::instruct(window.app_handle(), role, &event.message);
        crate::sweeps::instruct(window.app_handle(), role, &event.message);
        crate::main_watch::instruct(window.app_handle(), role, &event.message);
        crate::duty::publish_duty_event(window, role, event)?;
    }
    if let Some((projects, selection)) = project_selection {
        crate::projects::publish_selection(window, projects, selection)?;
    }
    // Opening the modal marks the channel read now, not at the watcher's next tick.
    if let Some(LayoutIntent::Navigation(NavigationIntent::Airc)) = &intent {
        crate::threads::report(crate::airc_modal::refresh(window.app_handle()));
    }
    carry_out(window.app_handle(), intent, snapshot)
}

/// Starting or steering a thread publishes its own newer snapshot.
fn carry_out(
    application: &AppHandle,
    intent: Option<LayoutIntent>,
    snapshot: LayoutSnapshot,
) -> Result<LayoutSnapshot, String> {
    match intent {
        Some(
            intent @ (LayoutIntent::SteerThread { .. } | LayoutIntent::RemoveQueuedMessage { .. }),
        ) => {
            // The reducer has already cleared the composer and added the message. The
            // window draws nothing while a request is pending, so waiting here for the
            // agent, a resume and their saves left the message in the composer until the
            // agent replied (#318). Withdrawing a queued message goes through the same
            // queue, so it never overtakes the send that put the message there.
            steer_in_background(application, snapshot.project_identifier, intent);
            Ok(snapshot)
        }
        Some(intent) => crate::threads::handle_intent(application, &intent)
            .map(|updated| updated.unwrap_or(snapshot)),
        None => Ok(snapshot),
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
    let managed = application.state::<MainLayout>();
    let Ok(mut state) = managed.lock() else {
        return;
    };
    if let Some(project) = project {
        state.return_unsent_message(project, *identifier, prompt);
    }
    state.show_toast(error, None);
    crate::threads::report(publish(application, &state.snapshot(), None));
}

fn show_toast(application: &AppHandle, message: String) {
    let managed = application.state::<MainLayout>();
    let Ok(mut state) = managed.lock() else {
        return;
    };
    state.show_toast(message, None);
    crate::threads::report(publish(application, &state.snapshot(), None));
}

pub fn publish<Handle: Emitter<Wry> + Manager<Wry>>(
    window: &Handle,
    snapshot: &LayoutSnapshot,
    intent: Option<LayoutIntent>,
) -> Result<(), String> {
    // Publish under the same lock as the update, preserving reducer/event order.
    LayoutStateChanged {
        state: snapshot.clone(),
    }
    .emit(window)
    .map_err(|error| format!("Could not publish main-window state: {error}"))?;
    if let Some(intent) = intent {
        LayoutIntentRequested { intent }
            .emit(window)
            .map_err(|error| format!("Could not publish navigation intent: {error}"))?;
    }
    Ok(())
}
