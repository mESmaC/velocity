//! Create in the New project dialog, and Open project in the switcher (SPEC.md F2). `velocity_core::new_project`
//! decides which steps run; this crate resolves the dialog's request, observes the
//! target, runs each step through `velocity-git`, `velocity-github` and the store,
//! and hands the resulting main-window state to the caller to publish.

mod create;
mod error;
mod open;
mod request;
mod resolve;

pub use create::{Publication, create_project};
pub use error::NewProjectError;
pub use open::{RepositoryPicker, RepositoryPickerView, open_project};
pub use request::{
    CreatedProject, NewProjectDefaults, NewProjectRequest, OpenProjectRequest,
    ProjectLocationRequest, RepositoryVisibility, new_project_defaults,
};
