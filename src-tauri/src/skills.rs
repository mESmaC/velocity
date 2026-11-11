//! Tauri IPC commands for the machine-wide skill pack library (SPEC.md F18, §7 M12).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use tauri::{AppHandle, Manager, State};
use velocity_skills::{
    DesignSystemStatus, InstalledSkillPack, SkillLibrary, SkillLibrarySnapshot,
    SkillPackUpdatePreview,
};

use crate::state::ManagedStateStore;

pub type ManagedSkillLibrary = Arc<Mutex<SkillLibrary>>;

pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let app_data = application
        .path()
        .app_data_dir()
        .context("Could not resolve the application data directory for the skill library")?;
    let library = SkillLibrary::open(&app_data)
        .context("Could not initialize the machine-wide skill library")?;
    application.manage(Arc::new(Mutex::new(library)));
    let handle = application.clone();
    velocity_acp::client::install_shared_servers(move |folder| shared_servers(&handle, folder));
    Ok(())
}

/// The switched-on packs' MCP servers for an agent starting in `folder` (F18, §3.2): the
/// folder's main checkout picks the project, and the project's skill choices pick the servers.
fn shared_servers(
    application: &AppHandle,
    folder: &std::path::Path,
) -> Vec<velocity_acp::client::SharedServer> {
    let Ok(checkout) = velocity_git::main_checkout(folder) else {
        return Vec::new();
    };
    let choices = {
        let state = application.state::<ManagedStateStore>();
        let Ok(store) = state.lock() else {
            return Vec::new();
        };
        let project = store.state().projects.iter().find(|project| {
            std::fs::canonicalize(&project.repository_path).is_ok_and(|path| path == checkout)
        });
        match project {
            Some(project) => project.skills.clone(),
            None => return Vec::new(),
        }
    };
    let library = application.state::<ManagedSkillLibrary>();
    let Ok(library) = library.lock().map(|library| library.clone()) else {
        return Vec::new();
    };
    velocity_skills::pack_shared_servers(&choices, &library)
        .into_iter()
        .map(|server| {
            velocity_acp::client::SharedServer::new(server.name, server.command, server.arguments)
        })
        .collect()
}

#[tauri::command]
#[specta::specta]
pub async fn get_skill_library(
    library: State<'_, ManagedSkillLibrary>,
) -> Result<SkillLibrarySnapshot, String> {
    let library = Arc::clone(&library);
    tauri::async_runtime::spawn_blocking(move || {
        library
            .lock()
            .map_err(|error| error.to_string())?
            .snapshot()
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn install_skill_pack(
    library: State<'_, ManagedSkillLibrary>,
    pack_or_repo: String,
    source_override: Option<String>,
    initial_pin: Option<String>,
) -> Result<InstalledSkillPack, String> {
    let library = Arc::clone(&library);
    tauri::async_runtime::spawn_blocking(move || {
        library
            .lock()
            .map_err(|error| error.to_string())?
            .install_pack(
                &pack_or_repo,
                source_override.as_deref(),
                initial_pin.as_deref(),
            )
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn check_skill_pack_update(
    library: State<'_, ManagedSkillLibrary>,
    pack_id: String,
) -> Result<SkillPackUpdatePreview, String> {
    let library = Arc::clone(&library);
    tauri::async_runtime::spawn_blocking(move || {
        library
            .lock()
            .map_err(|error| error.to_string())?
            .check_for_updates(&pack_id)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn confirm_skill_pack_update(
    library: State<'_, ManagedSkillLibrary>,
    pack_id: String,
    candidate_commit: String,
) -> Result<InstalledSkillPack, String> {
    let library = Arc::clone(&library);
    tauri::async_runtime::spawn_blocking(move || {
        library
            .lock()
            .map_err(|error| error.to_string())?
            .confirm_update(&pack_id, &candidate_commit)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn get_skill_settings(
    application: AppHandle,
    project_identifier: u32,
) -> Result<velocity_skills::settings::SkillsSettingsView, String> {
    tauri::async_runtime::spawn_blocking(move || read_settings(&application, project_identifier))
        .await
        .map_err(|error| error.to_string())?
}
fn read_settings(
    application: &AppHandle,
    identifier: u32,
) -> Result<velocity_skills::settings::SkillsSettingsView, String> {
    let state = application.state::<crate::state::ManagedStateStore>();
    let (name, choices, checkout) = {
        let store = state.lock().map_err(|error| error.to_string())?;
        let project = store
            .state()
            .projects
            .iter()
            .find(|project| project.id == identifier)
            .ok_or("Project is not registered")?;
        (
            project.name.clone(),
            project.skills.clone(),
            PathBuf::from(&project.repository_path),
        )
    };
    let library = application.state::<ManagedSkillLibrary>();
    let snapshot = library
        .lock()
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    let mut view = velocity_skills::settings::settings_view(identifier, name, &choices, &snapshot);
    velocity_skills::add_pack_details(&mut view, &checkout, &snapshot);
    Ok(view)
}
#[tauri::command]
#[specta::specta]
pub async fn set_skill_settings(
    application: AppHandle,
    project_identifier: u32,
    edit: velocity_core::skills::SkillSettingsEdit,
) -> Result<velocity_skills::settings::SkillsSettingsView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        save_settings(&application, project_identifier, edit)?;
        read_settings(&application, project_identifier)
    })
    .await
    .map_err(|error| error.to_string())?
}
fn save_settings(
    application: &AppHandle,
    identifier: u32,
    edit: velocity_core::skills::SkillSettingsEdit,
) -> Result<(), String> {
    let library = application.state::<ManagedSkillLibrary>();
    let snapshot = library
        .lock()
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    let (pack, bundle) = match &edit {
        velocity_core::skills::SkillSettingsEdit::Pack { identifier, .. } => (identifier, None),
        velocity_core::skills::SkillSettingsEdit::Bundle {
            identifier, bundle, ..
        } => (identifier, Some(bundle)),
    };
    let catalog = snapshot
        .catalog
        .iter()
        .find(|entry| &entry.id == pack)
        .ok_or("Unknown skill pack")?;
    if !snapshot.installed.iter().any(|entry| &entry.id == pack) {
        return Err("Install this pack before enabling its skills".into());
    }
    if bundle.is_some_and(|bundle| !catalog.bundles.iter().any(|entry| &entry.id == bundle)) {
        return Err("Unknown skill bundle".into());
    }
    let state = application.state::<crate::state::ManagedStateStore>();
    let mut store = state.lock().map_err(|error| error.to_string())?;
    let project = store
        .state()
        .projects
        .iter()
        .find(|project| project.id == identifier)
        .ok_or("Project is not registered")?;
    let mut choices = project.skills.clone();
    choices.apply(edit);
    store
        .save_project_skills(identifier, choices)
        .map_err(|error| error.to_string())
}

fn project_checkout(
    store: &ManagedStateStore,
    project_identifier: u32,
) -> Result<(PathBuf, String), String> {
    let store = store.lock().map_err(|error| error.to_string())?;
    store
        .project(project_identifier)
        .map(|project| {
            (
                PathBuf::from(&project.repository_path),
                project.name.clone(),
            )
        })
        .ok_or_else(|| String::from("The project is no longer registered"))
}

/// Generate (or, with `regenerate`, Regenerate) the project's design system, add the AGENTS.md
/// rule and commit both in the main checkout (F18).
#[tauri::command]
#[specta::specta]
pub async fn generate_design_system(
    store: State<'_, ManagedStateStore>,
    library: State<'_, ManagedSkillLibrary>,
    project_identifier: u32,
    regenerate: bool,
) -> Result<DesignSystemStatus, String> {
    let (checkout, name) = project_checkout(&store, project_identifier)?;
    let library = Arc::clone(&library);
    tauri::async_runtime::spawn_blocking(move || {
        // Clone the library handle out of the lock: a search takes seconds and must not hold
        // up installs or update checks.
        let library = library.lock().map_err(|error| error.to_string())?.clone();
        velocity_skills::generate_design_system(&library, &checkout, &name, regenerate)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}
