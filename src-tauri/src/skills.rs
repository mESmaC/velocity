//! Tauri IPC commands for the machine-wide skill pack library (SPEC.md F18, §7 M12).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use tauri::{AppHandle, Manager, State};
use velocity_skills::{
    DesignSystemStatus, GameEngineDetection, InstalledSkillPack, SkillLibrary,
    SkillLibrarySnapshot, SkillPackUpdatePreview, design_system_status,
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
    Ok(())
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

/// UI UX Pro Max's design system for a project: where MASTER.md lives, whether it exists, and
/// whether AGENTS.md carries its rule (F18).
#[tauri::command]
#[specta::specta]
pub async fn get_design_system(
    store: State<'_, ManagedStateStore>,
    project_identifier: u32,
) -> Result<DesignSystemStatus, String> {
    let (checkout, name) = project_checkout(&store, project_identifier)?;
    tauri::async_runtime::spawn_blocking(move || design_system_status(&checkout, &name))
        .await
        .map_err(|error| error.to_string())
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

/// The game engine the project's files show, and the Game dev bundles it suggests (F18).
#[tauri::command]
#[specta::specta]
pub async fn detect_game_engine(
    store: State<'_, ManagedStateStore>,
    project_identifier: u32,
) -> Result<GameEngineDetection, String> {
    let (checkout, _) = project_checkout(&store, project_identifier)?;
    tauri::async_runtime::spawn_blocking(move || velocity_skills::detect_game_engine(&checkout))
        .await
        .map_err(|error| error.to_string())
}
