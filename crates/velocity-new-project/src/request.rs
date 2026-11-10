//! What the dialog sends and receives over IPC (SPEC.md §3.1).

use std::path::{MAIN_SEPARATOR, Path};
use velocity_store::{EffortLevel, ProviderKind, Settings, SpeedMode};

#[derive(Clone, Copy, Debug, serde::Deserialize, specta::Type)]
pub enum RepositoryVisibility {
    Private,
    Public,
}

#[derive(Clone, Debug, serde::Deserialize, specta::Type)]
pub enum ProjectLocationRequest {
    LocalFolder {
        path: String,
        create_if_needed: bool,
    },
    GitHubNew {
        repository: String,
        visibility: RepositoryVisibility,
        clone_to: String,
    },
    GitHubExisting {
        repository: String,
        clone_to: String,
    },
}

#[derive(Clone, Debug, serde::Deserialize, specta::Type)]
pub struct NewProjectRequest {
    pub location: ProjectLocationRequest,
    pub model_name: Option<String>,
    pub effort: EffortLevel,
    pub speed: SpeedMode,
    pub brief: String,
}

impl NewProjectRequest {
    /// Register a local project using the same path as the New project dialog.
    pub fn local_folder(path: String, brief: String) -> Self {
        Self {
            location: ProjectLocationRequest::LocalFolder {
                path,
                create_if_needed: true,
            },
            model_name: None,
            effort: EffortLevel::Low,
            speed: SpeedMode::Regular,
            brief,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct NewProjectDefaults {
    pub folder: String,
    pub clone_to: String,
    pub provider: ProviderKind,
    pub model_name: Option<String>,
    pub effort: EffortLevel,
    pub speed: SpeedMode,
}

#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct CreatedProject {
    pub identifier: u32,
    pub name: String,
    pub repository_path: String,
}

/// The dialog opens on the home folder, with the interview settings from
/// Settings · Providers.
pub fn new_project_defaults(home: &Path, settings: Settings) -> NewProjectDefaults {
    let folder = format!("{}{MAIN_SEPARATOR}", home.display());
    // A signed-out default gives way to the first signed-in provider and its model (F15).
    let provider = settings.new_work_choice();
    let model_name = if provider == settings.providers.default_provider {
        settings.providers.default_model.clone()
    } else {
        Some(settings.provider_model(provider))
    };
    NewProjectDefaults {
        clone_to: folder.clone(),
        folder,
        provider,
        model_name,
        effort: settings.providers.default_effort,
        speed: settings.providers.default_speed,
    }
}
