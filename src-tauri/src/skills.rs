//! Tauri IPC commands for the machine-wide skill pack library (SPEC.md F18, §7 M12).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use tauri::{AppHandle, Manager, State};
use tauri_specta::Event;
use velocity_skills::{
    DesignSystemStatus, InstalledSkillPack, SkillLibrary, SkillLibrarySnapshot,
    SkillPackInstallStep, SkillPackUpdatePreview,
};

use crate::state::ManagedStateStore;

pub type ManagedSkillLibrary = Arc<Mutex<SkillLibrary>>;

#[derive(Default)]
struct ManagedSkillInstallations(Mutex<SkillPackInstallationsChanged>);

#[derive(Clone, Default, serde::Serialize, specta::Type, Event)]
#[tauri_specta(event_name = "skills://installations")]
pub struct SkillPackInstallationsChanged {
    revision: u32,
    packs: Vec<SkillPackInstallation>,
}

#[derive(Clone, serde::Serialize, specta::Type)]
struct SkillPackInstallation {
    pack_id: String,
    name: String,
    built_in: bool,
    status: SkillPackInstallStatus,
}

#[derive(Clone, PartialEq, serde::Serialize, specta::Type)]
#[serde(tag = "state", content = "message", rename_all = "snake_case")]
enum SkillPackInstallStatus {
    Running(String),
    Complete,
    Failed(String),
}

impl ManagedSkillInstallations {
    fn publish(
        &self,
        application: &AppHandle,
        installation: SkillPackInstallation,
        starting: bool,
    ) -> Result<(), String> {
        let mut view = self.0.lock().map_err(|error| error.to_string())?;
        if let Some(existing) = view
            .packs
            .iter_mut()
            .find(|pack| pack.pack_id == installation.pack_id)
        {
            if starting && matches!(existing.status, SkillPackInstallStatus::Running(_)) {
                return Err(format!("{} is already being installed", existing.name));
            }
            if existing.status == installation.status {
                return Ok(());
            }
            *existing = installation;
        } else {
            view.packs.push(installation);
        }
        view.revision += 1;
        if let Err(error) = view.emit(application) {
            crate::diagnostics::record(
                "skills",
                &format!("Could not publish install progress: {error}"),
            );
        }
        Ok(())
    }
}

pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let app_data = application
        .path()
        .app_data_dir()
        .context("Could not resolve the application data directory for the skill library")?;
    let library = SkillLibrary::open(&app_data)
        .context("Could not initialize the machine-wide skill library")?;
    let prompt_library = library.clone();
    velocity_acp::client::install_skill_prompt_resolver(move |folder, text| {
        match prompt_library.snapshot() {
            Ok(snapshot) => {
                velocity_skills::workspace::prepare_skill_prompt(text, &snapshot, folder)
            }
            Err(error) => {
                eprintln!("Could not resolve skill invocation: {error}");
                None
            }
        }
    });
    application.manage(Arc::new(Mutex::new(library)));
    application.manage(ManagedSkillInstallations::default());
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

/// Set up always-on skills and their MCP servers before the provider starts (F18, §3.2).
/// Saved workspace paths identify Rosen agents without invoking git in their workspace.
fn shared_servers(
    application: &AppHandle,
    folder: &std::path::Path,
) -> Vec<velocity_acp::client::SharedServer> {
    let Ok(folder) = std::fs::canonicalize(folder) else {
        return Vec::new();
    };
    let choices = choices_for_folder(application, &folder).or_else(|| {
        if folder.join(".rosen").is_dir() {
            return None;
        }
        let checkout = velocity_git::main_checkout(&folder).ok()?;
        choices_for_folder(application, &checkout)
    });
    let Some(choices) = choices else {
        return Vec::new();
    };
    let library = application.state::<ManagedSkillLibrary>();
    let Ok(library) = library.lock().map(|library| library.clone()) else {
        return Vec::new();
    };
    let linked = library.snapshot().and_then(|snapshot| {
        velocity_skills::workspace::link_workspace_skills(&choices, &snapshot, &folder)
    });
    if let Err(error) = linked {
        eprintln!("Could not link always-on skills: {error}");
    }
    velocity_skills::pack_shared_servers(&choices, &library)
        .into_iter()
        .map(|server| {
            velocity_acp::client::SharedServer::new(server.name, server.command, server.arguments)
        })
        .collect()
}

fn choices_for_folder(
    application: &AppHandle,
    folder: &std::path::Path,
) -> Option<velocity_core::skills::ProjectSkills> {
    let state = application.state::<ManagedStateStore>();
    let store = state.lock().ok()?;
    let project = store.state().projects.iter().find(|project| {
        std::fs::canonicalize(&project.repository_path).is_ok_and(|path| path == folder)
            || velocity_threads::project_workspace_root(
                project,
                &store.worktree_preferences(project),
            )
            .and_then(|root| std::fs::canonicalize(root).ok())
            .is_some_and(|root| folder.starts_with(root))
            || project.threads.iter().any(|thread| {
                thread
                    .worktree_path
                    .as_ref()
                    .and_then(|path| std::fs::canonicalize(path).ok())
                    .is_some_and(|path| path == folder)
            })
    })?;
    Some(project.skills.clone())
}

/// The per-agent part of F18, at a Rosen workspace's final path, before setup runs.
pub(crate) fn setup_workspace(
    application: &AppHandle,
    project: u32,
    request: &velocity_git::WorktreeSetupRequest,
) -> Result<(), String> {
    let choices = {
        let state = application.state::<ManagedStateStore>();
        let store = crate::threads::lock(state.inner(), "persisted state")?;
        store
            .project(project)
            .ok_or("Unknown project")?
            .skills
            .clone()
    };
    let app_data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    velocity_skills::workspace::finish_workspace_setup(&app_data, &choices, request)
        .map_err(|error| error.to_string())
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
pub fn get_skill_pack_installations(
    application: AppHandle,
) -> Result<SkillPackInstallationsChanged, String> {
    application
        .state::<ManagedSkillInstallations>()
        .0
        .lock()
        .map(|view| view.clone())
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn install_skill_pack(
    application: AppHandle,
    library: State<'_, ManagedSkillLibrary>,
    pack_or_repo: String,
    source_override: Option<String>,
    initial_pin: Option<String>,
) -> Result<(), String> {
    let (catalog, _) =
        velocity_skills::resolve_pack_source(&pack_or_repo, source_override.as_deref()).map_err(
            |error| {
                crate::diagnostics::record("skills", &error.to_string());
                error.to_string()
            },
        )?;
    let installation = SkillPackInstallation {
        pack_id: catalog.id,
        name: catalog.name,
        built_in: catalog.built_in,
        status: SkillPackInstallStatus::Running("Cloning…".into()),
    };
    application
        .state::<ManagedSkillInstallations>()
        .publish(&application, installation.clone(), true)
        .inspect_err(|error| crate::diagnostics::record("skills", error))?;
    let library = Arc::clone(&library);
    let job_application = application.clone();
    let job_installation = installation.clone();
    let job = tauri::async_runtime::spawn_blocking(move || {
        let result = library
            .lock()
            .map(|handle| handle.clone())
            .map_err(|error| error.to_string())
            .and_then(|library| {
                library
                    .install_pack_with_progress(
                        &pack_or_repo,
                        source_override.as_deref(),
                        initial_pin.as_deref(),
                        |step| report_install_step(&job_application, &job_installation, step),
                    )
                    .map_err(|error| error.to_string())
            });
        finish_installation(&job_application, job_installation, result);
    });
    tauri::async_runtime::spawn(async move {
        if let Err(error) = job.await {
            finish_installation(&application, installation, Err(error.to_string()));
        }
    });
    Ok(())
}

fn report_install_step(
    application: &AppHandle,
    installation: &SkillPackInstallation,
    step: SkillPackInstallStep,
) {
    let message = match step {
        SkillPackInstallStep::Cloning => "Cloning…",
        SkillPackInstallStep::CheckingOut => "Checking out…",
        SkillPackInstallStep::Pinning => "Pinning commit…",
        SkillPackInstallStep::Discovering => "Discovering skills…",
        SkillPackInstallStep::Registering => "Registering pack…",
    };
    crate::diagnostics::record("skills", &format!("{} {step:?}", installation.pack_id));
    let mut installation = installation.clone();
    installation.status = SkillPackInstallStatus::Running(message.into());
    crate::threads::report(application.state::<ManagedSkillInstallations>().publish(
        application,
        installation,
        false,
    ));
}

fn finish_installation(
    application: &AppHandle,
    mut installation: SkillPackInstallation,
    result: Result<InstalledSkillPack, String>,
) {
    let installed = result.is_ok();
    installation.status = match result {
        Ok(pack) => {
            crate::diagnostics::record(
                "skills",
                &format!("{} installed at {}", pack.id, pack.pinned_commit),
            );
            SkillPackInstallStatus::Complete
        }
        Err(error) => {
            crate::diagnostics::record(
                "skills",
                &format!("{} installation failed: {error}", installation.pack_id),
            );
            SkillPackInstallStatus::Failed(error)
        }
    };
    crate::threads::report(application.state::<ManagedSkillInstallations>().publish(
        application,
        installation,
        false,
    ));
    if installed && let Err(error) = publish_skill_menus(application) {
        crate::diagnostics::record("skills", &format!("Could not publish skill menus: {error}"));
    }
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
pub async fn remove_skill_pack(application: AppHandle, pack_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        application
            .state::<ManagedSkillLibrary>()
            .lock()
            .map_err(|error| error.to_string())?
            .remove_pack(&pack_id)
            .map_err(|error| error.to_string())?;
        publish_skill_menus(&application)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn set_skill_bundle_default(
    application: AppHandle,
    pack_id: String,
    bundle_id: String,
    enabled: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        application
            .state::<ManagedSkillLibrary>()
            .lock()
            .map_err(|error| error.to_string())?
            .set_default_bundle(&pack_id, &bundle_id, enabled)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn get_skill_settings(
    application: AppHandle,
    project_identifier: Option<u32>,
) -> Result<velocity_skills::settings::SkillsSettingsView, String> {
    tauri::async_runtime::spawn_blocking(move || read_settings(&application, project_identifier))
        .await
        .map_err(|error| error.to_string())?
}
fn read_settings(
    application: &AppHandle,
    identifier: Option<u32>,
) -> Result<velocity_skills::settings::SkillsSettingsView, String> {
    let snapshot = application
        .state::<ManagedSkillLibrary>()
        .lock()
        .map_err(|error| error.to_string())?
        .snapshot()
        .map_err(|error| error.to_string())?;
    let Some(identifier) = identifier else {
        return Ok(velocity_skills::settings::settings_view(
            0,
            String::new(),
            &Default::default(),
            &snapshot,
        ));
    };
    let (name, choices, checkout) = {
        let state = application.state::<ManagedStateStore>();
        let store = state.lock().map_err(|error| error.to_string())?;
        let project = store
            .project(identifier)
            .ok_or("Project is not registered")?;
        (
            project.name.clone(),
            project.skills.clone(),
            PathBuf::from(&project.repository_path),
        )
    };
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
        read_settings(&application, Some(project_identifier))
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
    let defaults = snapshot.default_bundles_for(pack);
    choices.apply_with_defaults(edit, defaults);
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
    }
    Ok(())
}

/// Publish one machine-wide composer menu after any library change.
pub fn publish_skill_menus(application: &AppHandle) -> Result<(), String> {
    let library = application
        .state::<ManagedSkillLibrary>()
        .lock()
        .map_err(|error| error.to_string())?
        .clone();
    let layout = application.state::<crate::layout::MainLayout>();
    let mut layout = layout.lock().map_err(|error| error.to_string())?;
    // Read under the layout lock so concurrent installs cannot publish stale menus.
    let snapshot = library.snapshot().map_err(|error| error.to_string())?;
    layout.set_installed_skills(velocity_skills::menu::skill_menu_entries(&snapshot));
    crate::layout::publish(application, &layout.snapshot(), None)
}
