//! Tauri IPC commands for the machine-wide skill pack library (SPEC.md F18, §7 M12).

use std::sync::{Arc, Mutex};

use anyhow::Context;
use tauri::{AppHandle, Manager, State};
use velocity_skills::{
    InstalledSkillPack, SkillLibrary, SkillLibrarySnapshot, SkillPackUpdatePreview,
};

pub type ManagedSkillLibrary = Arc<Mutex<SkillLibrary>>;

pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let app_data = application
        .path()
        .app_data_dir()
        .context("Could not resolve the application data directory for the skill library")?;
    let library = SkillLibrary::open(&app_data)
        .context("Could not initialize the machine-wide skill library")?;
    application.manage(Arc::new(Mutex::new(library)));
    // A menu that fails to load leaves `$` empty; it never stops the app from starting.
    crate::threads::report(publish_skill_menus(application));
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
    application: AppHandle,
    library: State<'_, ManagedSkillLibrary>,
    pack_or_repo: String,
    source_override: Option<String>,
    initial_pin: Option<String>,
) -> Result<InstalledSkillPack, String> {
    let library = Arc::clone(&library);
    tauri::async_runtime::spawn_blocking(move || {
        let pack = library
            .lock()
            .map_err(|error| error.to_string())?
            .install_pack(
                &pack_or_repo,
                source_override.as_deref(),
                initial_pin.as_deref(),
            )
            .map_err(|error| error.to_string())?;
        // The library lock above is released by now; the menu takes it again.
        publish_skill_menus(&application)?;
        Ok(pack)
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
    application: AppHandle,
    library: State<'_, ManagedSkillLibrary>,
    pack_id: String,
    candidate_commit: String,
) -> Result<InstalledSkillPack, String> {
    let library = Arc::clone(&library);
    tauri::async_runtime::spawn_blocking(move || {
        let pack = library
            .lock()
            .map_err(|error| error.to_string())?
            .confirm_update(&pack_id, &candidate_commit)
            .map_err(|error| error.to_string())?;
        // The library lock above is released by now; the menu takes it again.
        publish_skill_menus(&application)?;
        Ok(pack)
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
    let (name, choices) = {
        let store = state.lock().map_err(|error| error.to_string())?;
        let project = store
            .state()
            .projects
            .iter()
            .find(|project| project.id == identifier)
            .ok_or("Project is not registered")?;
        (project.name.clone(), project.skills.clone())
    };
    let library = application.state::<ManagedSkillLibrary>();
    let snapshot = library
        .lock()
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    Ok(velocity_skills::settings::settings_view(
        identifier, name, &choices, &snapshot,
    ))
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
        publish_skill_menus(&application)?;
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

/// Gives every project's `$` menu the skills it has switched on (F4, F18), after the library
/// or a project's choices change.
pub fn publish_skill_menus(application: &AppHandle) -> Result<(), String> {
    let snapshot = application
        .state::<ManagedSkillLibrary>()
        .lock()
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    let menus: Vec<_> = {
        let state = application.state::<crate::state::ManagedStateStore>();
        let store = state.lock().map_err(|error| error.to_string())?;
        store
            .state()
            .projects
            .iter()
            .map(|project| {
                let entries = velocity_skills::menu::skill_menu_entries(&project.skills, &snapshot);
                (project.id, entries)
            })
            .collect()
    };
    let layout = application.state::<crate::layout::MainLayout>();
    let mut layout = layout.lock().map_err(|error| error.to_string())?;
    for (project, entries) in menus {
        layout.set_project_skills(project, entries);
    }
    // Published under the layout lock, as every layout update is, to keep events in order.
    crate::layout::publish(application, &layout.snapshot(), None)
}
