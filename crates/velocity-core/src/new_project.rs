//! Plans how Create in the New project dialog sets a project up (SPEC.md F2).
//!
//! The plan is computed from what Velocity observed before it acts, so a refused
//! request runs nothing ("Esc cancels; nothing is created"). This module decides
//! which steps run and in what order. The I/O edge fills each step's arguments
//! (paths, repository, interview settings) from the dialog's request.

/// Where the project lives, as chosen under Location in the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewProjectLocation {
    /// A local folder. `create_if_needed` is the "create the folder and run
    /// `git init` if needed" option.
    LocalFolder { create_if_needed: bool },
    /// GitHub · New repo: create the repository, then clone it.
    GitHubNew,
    /// GitHub · Existing repo: clone it.
    GitHubExisting,
}

/// What Velocity observed about the request's path and repository before acting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    /// The project path is already a registered project.
    pub already_a_project: bool,
    /// The local folder is already a git repository.
    pub is_repository: bool,
    /// `github.com/<owner>/<name>` exists.
    pub remote_exists: bool,
    /// The clone target exists and is not empty.
    pub target_occupied: bool,
}

/// One unit of setup work, in the order Create runs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStep {
    /// `git init <path>`, which also creates a missing folder.
    GitInit,
    /// `gh repo create <owner>/<name> --private|--public`.
    CreateRemote,
    /// `git clone` of the repository into the target folder.
    Clone,
    /// Add the project to the project store.
    Register,
    /// Make it the active project.
    SwitchTo,
    /// Record the spec interview with the dialog's model, effort, speed and brief.
    /// The interview thread itself starts once F3 lands (#25).
    StartInterview,
}

/// Why Create cannot set the project up. Nothing has run when this is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The folder is not a git repository and the create option is off.
    NotARepository,
    /// The path is already a project; switch to it instead.
    AlreadyAProject,
    /// GitHub · New repo names a repository that already exists.
    RemoteAlreadyExists,
    /// GitHub · Existing repo names a repository that does not exist.
    RemoteMissing,
    /// The clone target exists and is not empty.
    TargetOccupied,
}

/// The steps that make the location a git checkout.
pub fn setup_steps(
    location: NewProjectLocation,
    observation: Observation,
) -> Result<Vec<SetupStep>, Refusal> {
    match location {
        NewProjectLocation::LocalFolder { create_if_needed } => {
            if observation.is_repository {
                Ok(Vec::new())
            } else if create_if_needed {
                Ok(Vec::from([SetupStep::GitInit]))
            } else {
                Err(Refusal::NotARepository)
            }
        }
        NewProjectLocation::GitHubNew => {
            if observation.remote_exists {
                Err(Refusal::RemoteAlreadyExists)
            } else if observation.target_occupied {
                Err(Refusal::TargetOccupied)
            } else {
                Ok(Vec::from([SetupStep::CreateRemote, SetupStep::Clone]))
            }
        }
        NewProjectLocation::GitHubExisting => {
            if !observation.remote_exists {
                Err(Refusal::RemoteMissing)
            } else if observation.target_occupied {
                Err(Refusal::TargetOccupied)
            } else {
                Ok(Vec::from([SetupStep::Clone]))
            }
        }
    }
}

/// Every step Create runs, or the reason it refuses before running any.
pub fn plan_new_project(
    location: NewProjectLocation,
    observation: Observation,
) -> Result<Vec<SetupStep>, Refusal> {
    if observation.already_a_project {
        return Err(Refusal::AlreadyAProject);
    }
    let mut steps = setup_steps(location, observation)?;
    steps.extend([
        SetupStep::Register,
        SetupStep::SwitchTo,
        SetupStep::StartInterview,
    ]);
    Ok(steps)
}
