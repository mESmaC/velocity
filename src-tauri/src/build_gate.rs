use tauri::State;
use velocity_store::build_gate::{LaunchEligibility, LaunchRequest, launch_eligibility};

use crate::state::ManagedStateStore;

#[tauri::command]
#[specta::specta]
pub fn get_launch_eligibility(
    store: State<'_, ManagedStateStore>,
    request: LaunchRequest,
) -> Result<LaunchEligibility, String> {
    let guard = store
        .lock()
        .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
    Ok(launch_eligibility(guard.state(), request))
}
