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
    NewProjectDefaults {
        clone_to: folder.clone(),
        folder,
        provider: settings.providers.default_provider,
        model_name: settings.providers.default_model,
        effort: settings.providers.default_effort,
        speed: settings.providers.default_speed,
    }
}
