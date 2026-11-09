//! Create in the New project dialog (SPEC.md F2). `velocity_core::new_project`
//! decides which steps run; this crate resolves the dialog's request, observes the
//! target, runs each step through `velocity-git`, `velocity-github` and the store,
//! and hands the resulting main-window state to the caller to publish.

mod create;
mod error;
mod request;
mod resolve;

pub use create::{Publication, create_project};
pub use error::NewProjectError;
pub use request::{
    CreatedProject, NewProjectDefaults, NewProjectRequest, ProjectLocationRequest,
    RepositoryVisibility, new_project_defaults,
};
