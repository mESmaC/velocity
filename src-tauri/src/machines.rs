//! Native adapters for Rosen machine enrollment and authenticated remote dispatch.
use tauri::AppHandle;
use velocity_rosen::machines::{self, MachinesView, Role};

#[tauri::command]
#[specta::specta]
pub async fn get_machines(
    application: AppHandle,
    project_identifier: u32,
) -> Result<MachinesView, String> {
    let server = crate::rosen::server(&application, project_identifier).await?;
    machines::view(&server).await.map_err(error)
}

#[tauri::command]
#[specta::specta]
pub async fn enable_machine_listener(
    application: AppHandle,
    project_identifier: u32,
    address: String,
) -> Result<(), String> {
    let server = crate::rosen::server(&application, project_identifier).await?;
    machines::enable_listener(&server, &address)
        .await
        .map_err(error)
}

#[tauri::command]
#[specta::specta]
pub async fn create_machine_join_key(
    application: AppHandle,
    project_identifier: u32,
    address: String,
    expires_in_seconds: u32,
    uses: u32,
    role: Role,
) -> Result<String, String> {
    let server = crate::rosen::server(&application, project_identifier).await?;
    machines::create_key(&server, address, expires_in_seconds, uses, role)
        .await
        .map_err(error)
}

#[tauri::command]
#[specta::specta]
pub async fn approve_machine(
    application: AppHandle,
    project_identifier: u32,
    machine: String,
) -> Result<(), String> {
    let server = crate::rosen::server(&application, project_identifier).await?;
    machines::approve(&server, &machine).await.map_err(error)
}

#[tauri::command]
#[specta::specta]
pub async fn revoke_machine(
    application: AppHandle,
    project_identifier: u32,
    machine: String,
) -> Result<(), String> {
    let server = crate::rosen::server(&application, project_identifier).await?;
    machines::revoke(&server, &machine).await.map_err(error)
}

#[tauri::command]
#[specta::specta]
pub async fn dispatch_to_machine(
    application: AppHandle,
    project_identifier: u32,
    machine: String,
    prompt: String,
) -> Result<String, String> {
    use tauri::Manager;
    let store = application.state::<crate::state::ManagedStateStore>();
    {
        let store = crate::threads::lock(store.inner(), "persisted state")?;
        let project = store.project(project_identifier).ok_or("Unknown project")?;
        if !project.spec_locked {
            return Err("Lock the project spec before dispatching work".into());
        }
        if project.version_control != velocity_store::VersionControl::Rosen {
            return Err("Machine dispatch needs a Rosen project".into());
        }
    }
    let server = crate::rosen::server(&application, project_identifier).await?;
    velocity_rosen::machine_dispatch::dispatch(&server, &machine, prompt)
        .await
        .map_err(error)
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
