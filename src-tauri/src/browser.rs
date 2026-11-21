//! Tauri adapter for the per-thread browser. The page never runs in the Velocity webview.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use agent_client_protocol::v1::schema::{EnvVariable, McpServer, McpServerStdio};
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_acp::client::SessionSetup;
use velocity_browser::{BrowserHub, BrowserInput, BrowserKey, BrowserView, Frame};

use crate::{state::ManagedStateStore, threads::lock};

#[derive(Clone, Serialize, Type, Event)]
pub struct BrowserFrame {
    project: u32,
    thread: u32,
    view: BrowserView,
    frame: Option<Frame>,
}

pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let hub = BrowserHub::default();
    let state = application.state::<ManagedStateStore>();
    hub.limit(
        lock(state.inner(), "persisted state")
            .map_err(anyhow::Error::msg)?
            .settings()
            .agents
            .browser_limit,
    );
    let runtime = tauri::async_runtime::handle();
    let _runtime = runtime.inner().enter();
    hub.start()?;
    application.manage(hub.clone());
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            interval.tick().await;
            hub.reap().await;
        }
    });
    Ok(())
}

fn profile(root: &Path, key: BrowserKey) -> PathBuf {
    root.join(".velocity-browser")
        .join(format!("{}-{}", key.0, key.1))
}

fn check_location(root: &Path) -> Result<(), String> {
    if !root.is_absolute() {
        return Err("Set an absolute workspace location before using the agent browser".into());
    }
    #[cfg(windows)]
    if let Some(drive) = std::env::var_os("SystemDrive")
        && root
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_ascii_lowercase()
            .starts_with(&drive.to_string_lossy().to_ascii_lowercase())
    {
        return Err("Set Settings → Workspaces → Location to a drive other than the system drive to use the agent browser".into());
    }
    Ok(())
}

fn resolve_profile(application: &AppHandle, key: BrowserKey) -> Result<PathBuf, String> {
    let state = application.state::<ManagedStateStore>();
    let (root, persist) = {
        let store = lock(state.inner(), "persisted state")?;
        let project = store
            .state()
            .projects
            .iter()
            .find(|project| project.id == key.0)
            .ok_or("The browser's project no longer exists")?;
        let thread = project
            .threads
            .iter()
            .find(|thread| thread.id == key.1)
            .ok_or("The browser's thread no longer exists")?;
        if thread.interview.is_some() || thread.role.is_read_only() {
            return Err("The agent browser is available in implementation threads".into());
        }
        let saved = thread.browser_location.as_deref();
        let root = saved
            .map(PathBuf::from)
            .or_else(|| {
                velocity_threads::project_workspace_root(
                    project,
                    &store.worktree_preferences(project),
                )
            })
            .ok_or("Set a workspace location before using the agent browser")?;
        (root, saved.is_none())
    };
    check_location(&root)?;
    if persist {
        lock(state.inner(), "persisted state")?
            .update_thread(key.0, key.1, |thread| {
                thread.browser_location = Some(root.to_string_lossy().into_owned());
            })
            .map_err(|error| error.to_string())?;
        crate::state::save_now(application)?;
    }
    Ok(velocity_store::extended_windows_path(&profile(&root, key)))
}

pub fn configure(application: &AppHandle, key: BrowserKey, setup: &mut SessionSetup) {
    configure_for_prompt(application, key, "", "", setup);
}

/// Attaches `velocity-browser` eagerly on UI work, lazily behind `browser_tools` on
/// non-UI builder work, or omits it for roles that never drive a browser (#533).
pub fn configure_for_prompt(
    application: &AppHandle,
    key: BrowserKey,
    title: &str,
    prompt: &str,
    setup: &mut SessionSetup,
) {
    let touches_ui = velocity_core::tool_policy::prompt_touches_ui(title, prompt);
    let attachment = velocity_core::tool_policy::browser_attachment(setup.role, touches_ui);
    if attachment == velocity_core::tool_policy::BrowserAttachment::Omitted {
        return;
    }
    let lazy = attachment == velocity_core::tool_policy::BrowserAttachment::Lazy;
    let result = (|| -> Result<(), String> {
        let hub = application.state::<BrowserHub>();
        hub.register(key, resolve_profile(application, key));
        let route = hub.route(key).map_err(|error| error.to_string())?;
        let program = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut args = vec![velocity_browser::SWITCH.into()];
        if lazy {
            args.push(velocity_browser::LAZY_FLAG.into());
        }
        let server = McpServerStdio::new(velocity_browser::SERVER, program)
            .args(args)
            .env(vec![
                EnvVariable::new(velocity_browser::PORT_ENV, route.port.to_string()),
                EnvVariable::new(velocity_browser::TOKEN_ENV, route.token.to_string()),
            ]);
        setup.shared_servers.push(McpServer::Stdio(server));
        Ok(())
    })();
    if let Err(error) = result {
        if let Some(card) = setup.session_card.as_mut() {
            card.push_unavailable_tool("velocity-browser");
        }
        crate::diagnostics::record("browser", &error);
    }
}

pub async fn stop(application: &AppHandle, key: BrowserKey) {
    application.state::<BrowserHub>().stop(key).await;
}

pub async fn delete(
    application: &AppHandle,
    key: BrowserKey,
    root: Option<&str>,
) -> Result<(), String> {
    let path = root
        .map(|root| {
            let root = Path::new(root);
            check_location(root)?;
            Ok::<_, String>(velocity_store::extended_windows_path(&profile(root, key)))
        })
        .transpose()?;
    application
        .state::<BrowserHub>()
        .delete(key, path.as_deref())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn open_browser_view(
    application: AppHandle,
    project: u32,
    thread: u32,
) -> Result<BrowserView, String> {
    let key = (project, thread);
    let action = crate::threads::action_lock(&application, key)?;
    let _action = action.lock().await;
    let profile = resolve_profile(&application, key)?;
    let hub = application.state::<BrowserHub>().inner().clone();
    hub.register(key, Ok(profile));
    let view = hub
        .open_view(key)
        .await
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn(stream(application, hub, key, view.clone()));
    Ok(view)
}

async fn stream(
    application: AppHandle,
    hub: BrowserHub,
    key: BrowserKey,
    mut previous: BrowserView,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(160));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let Ok((view, frame)) = hub.view(key, &previous.view_id).await else {
            break;
        };
        if frame.is_some() || view != previous {
            previous = view.clone();
            if (BrowserFrame {
                project: key.0,
                thread: key.1,
                view,
                frame,
            })
            .emit(&application)
            .is_err()
            {
                break;
            }
        }
    }
}

#[tauri::command]
#[specta::specta]
pub async fn close_browser_view(
    application: AppHandle,
    project: u32,
    thread: u32,
    view: String,
) -> Result<(), String> {
    application
        .state::<BrowserHub>()
        .close_view((project, thread), &view)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn take_over_browser(
    application: AppHandle,
    project: u32,
    thread: u32,
    view: String,
) -> Result<(), String> {
    application
        .state::<BrowserHub>()
        .take_over((project, thread), &view)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn hand_back_browser(
    application: AppHandle,
    project: u32,
    thread: u32,
    view: String,
) -> Result<(), String> {
    static NOTICES: AtomicU64 = AtomicU64::new(0);
    let key = (project, thread);
    if let Some(message) = application
        .state::<BrowserHub>()
        .hand_back(key, &view)
        .await
        .map_err(|error| error.to_string())?
    {
        let identifier = format!(
            "browser-handback-{}",
            NOTICES.fetch_add(1, Ordering::Relaxed)
        );
        if let Ok(mut runtimes) = lock(
            application
                .state::<crate::threads::ManagedThreads>()
                .inner(),
            "thread sessions",
        ) {
            let _ = runtimes.deliver_follow_up(key, &identifier, &message);
        }
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn browser_input(
    application: AppHandle,
    project: u32,
    thread: u32,
    view: String,
    tab: String,
    input: BrowserInput,
) -> Result<(), String> {
    application
        .state::<BrowserHub>()
        .owner_input((project, thread), &view, &tab, input)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn switch_browser_tab(
    application: AppHandle,
    project: u32,
    thread: u32,
    view: String,
    tab: String,
) -> Result<(), String> {
    application
        .state::<BrowserHub>()
        .switch_tab((project, thread), &view, &tab)
        .await
        .map_err(|error| error.to_string())
}
