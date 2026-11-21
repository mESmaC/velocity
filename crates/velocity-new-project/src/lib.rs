//! Rosen project creation and opening (F2).

mod create;
mod error;
mod open;
mod project_root;
mod request;
mod resolve;

pub use create::{Publication, create_project};
pub use error::NewProjectError;
pub use open::{RepositoryPicker, RepositoryPickerView, open_project};
pub use project_root::validate_project_root;
pub use request::{
    CreatedProject, NewProjectDefaults, NewProjectRequest, OpenProjectRequest,
    ProjectLocationRequest, ProjectMirror, new_project_defaults, projects_directory,
};
pub use resolve::parse_github_repository;
