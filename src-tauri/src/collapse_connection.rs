//! Settings → Agents → shared MCP servers → Collapse (#430): connect the owner's Collapse
//! account through the browser and switch Collapse's read-only MCP server on or off.
//!
//! The tokens stay in the OS credential store; the window only ever sees the account name.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::state::ManagedStateStore;
use crate::threads::lock;

/// What Settings shows for Collapse's MCP server.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CollapseConnection {
    /// The connected Collapse account, or `None` before Connect.
    pub account: Option<String>,
    /// The Settings switch: agents get the server only while this is on and connected.
    pub enabled: bool,
    /// The MCP server's name and URL as agents see them.
    pub name: String,
    pub url: String,
}

/// One browser sign-in at a time; a second Connect would only race the first for the reply.
static SIGNING_IN: AtomicBool = AtomicBool::new(false);

struct SignInGuard;

impl Drop for SignInGuard {
    fn drop(&mut self) {
        SIGNING_IN.store(false, Ordering::SeqCst);
    }
}

/// `get_collapse_connection` (§3.1).
#[tauri::command]
#[specta::specta]
pub async fn get_collapse_connection(application: AppHandle) -> Result<CollapseConnection, String> {
    connection(application).await
}

/// `connect_collapse` (§3.1): opens Collapse's approval page and waits for the owner to
/// approve. New agents get the MCP server once this returns.
#[tauri::command]
#[specta::specta]
pub async fn connect_collapse(application: AppHandle) -> Result<CollapseConnection, String> {
    if SIGNING_IN.swap(true, Ordering::SeqCst) {
        return Err("Collapse sign-in is already waiting in the browser".into());
    }
    let _guard = SignInGuard;
    let opener = application.clone();
    velocity_collapse::connect(&machine_name(), move |page| {
        opener
            .opener()
            .open_url(page, None::<&str>)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?;
    connection(application).await
}

/// `disconnect_collapse` (§3.1): forgets the tokens on this machine. They stay valid on
/// Collapse until revoked in its token list.
#[tauri::command]
#[specta::specta]
pub async fn disconnect_collapse(application: AppHandle) -> Result<CollapseConnection, String> {
    tauri::async_runtime::spawn_blocking(velocity_collapse::forget_credentials)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    connection(application).await
}

/// `set_collapse_mcp { enabled }` (§3.1): the Settings switch. Running agents keep theirs.
#[tauri::command]
#[specta::specta]
pub async fn set_collapse_mcp(
    application: AppHandle,
    enabled: bool,
) -> Result<CollapseConnection, String> {
    let handle = application.clone();
    crate::layout::off_ui_thread(move || {
        let mut store = lock(
            handle.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let mut settings = store.settings();
        settings.agents.collapse_mcp = enabled;
        store
            .set_settings(settings)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await?;
    connection(application).await
}

async fn connection(application: AppHandle) -> Result<CollapseConnection, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let enabled = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .settings()
        .agents
        .collapse_mcp;
        let account = velocity_collapse::load_credentials()
            .map_err(|error| error.to_string())?
            .map(|credentials| credentials.account);
        Ok(CollapseConnection {
            account,
            enabled,
            name: velocity_collapse::MCP_SERVER_NAME.into(),
            url: velocity_collapse::MCP_URL.into(),
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Collapse names the connection `Velocity on <machine>` in its token lists.
fn machine_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .into_iter()
        .find_map(|variable| std::env::var(variable).ok())
        .unwrap_or_default()
}
