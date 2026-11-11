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
    // A menu that fails to load leaves `$` empty; it never stops the app from starting.
    crate::threads::report(publish_skill_menus(application));
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
    // A scan can select a pack before installation. Declining that selection must
    // still be saved, so later scans and installation respect the user's choice.
    let declining_pack = matches!(
        &edit,
        velocity_core::skills::SkillSettingsEdit::Pack { enabled: false, .. }
    );
    if !declining_pack && !snapshot.installed.iter().any(|entry| &entry.id == pack) {
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
        // The `$` menu lists the switched-on skills; it takes the store lock itself.
        drop(store);
        publish_skill_menus(application)?;
    }
    Ok(())
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
