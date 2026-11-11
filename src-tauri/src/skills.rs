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
    // The project restored at launch counts as opened (F18: "opening a Godot project
    // suggests the router and the Godot bundle").
    let active = application
        .state::<crate::state::ManagedStateStore>()
        .lock()
        .ok()
        .and_then(|store| store.active_project().map(|project| project.id));
    if let Some(project) = active {
        recheck_suggestions(application, project, SuggestionCheck::Opened);
    }
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

/// What prompted a re-check of the project scan's skill suggestions (F3, F18).
#[derive(Clone, Copy, Debug)]
pub enum SuggestionCheck {
    /// Opening or scanning a project reads the checkout and the tip already fetched.
    Opened,
    /// `master` moved: fetch it first, so a project that gains a UI gets the suggestion
    /// even before the main checkout pulls.
    Merged,
}

/// Re-runs the project scan in the background and switches its suggestions on, unless you
/// turned them off (F18 Suggestions: "Velocity re-checks after every merge").
pub fn recheck_suggestions(
    application: &AppHandle,
    project_identifier: u32,
    check: SuggestionCheck,
) {
    let application = application.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = recheck(&application, project_identifier, check) {
            eprintln!("Could not re-check skill suggestions: {error}");
        }
    });
}

fn recheck(application: &AppHandle, identifier: u32, check: SuggestionCheck) -> Result<(), String> {
    let state = application.state::<crate::state::ManagedStateStore>();
    let (root, branch) = {
        let store = state.lock().map_err(|error| error.to_string())?;
        let project = store
            .project(identifier)
            .ok_or("Project is not registered")?;
        (
            std::path::PathBuf::from(&project.repository_path),
            project.default_branch.clone(),
        )
    };
    // The scan runs without the store lock; the choices are re-read under it afterwards so
    // a switch you flipped meanwhile is kept.
    let tip = match check {
        SuggestionCheck::Opened => velocity_skills::suggestions::known_tip(&root, &branch),
        SuggestionCheck::Merged => velocity_skills::suggestions::fetched_tip(&root, &branch),
    };
    let suggestions = velocity_skills::suggestions::scan_project(&root, tip.as_deref());
    let mut store = state.lock().map_err(|error| error.to_string())?;
    let mut choices = store
        .project(identifier)
        .ok_or("Project is not registered")?
        .skills
        .clone();
    if choices.apply_suggestions(suggestions) {
        store
            .save_project_skills(identifier, choices)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}
