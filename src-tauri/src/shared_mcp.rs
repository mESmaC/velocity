//! Settings → Agents → shared MCP servers (SPEC.md §3.2 Tool access, F13).

use tauri::State;
use velocity_store::shared_mcp::{SharedMcpServer, SharedMcpServerEdit};

use crate::state::ManagedStateStore;

/// Adds or removes a shared server. New agents of every provider get the saved list; agents
/// already running keep the servers they started with.
#[tauri::command]
#[specta::specta]
pub fn edit_shared_mcp_servers(
    store: State<'_, ManagedStateStore>,
    edit: SharedMcpServerEdit,
) -> Result<Vec<SharedMcpServer>, String> {
    let mut guard = store
        .lock()
        .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
    guard
        .edit_shared_mcp_servers(edit)
        .map_err(|error| error.to_string())
}
