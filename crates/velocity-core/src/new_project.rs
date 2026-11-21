//! Pure decisions for the New and Open project entry points (F2).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewProjectLocation {
    NewFolder,
    OpenFolder,
    OpenGitHub,
    OpenCollapse,
}

impl NewProjectLocation {
    pub fn opens_existing(self) -> bool {
        self != Self::NewFolder
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub already_a_project: bool,
    pub is_repository: bool,
    pub remote_exists: bool,
    pub target_occupied: bool,
    pub target_is_checkout: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NotARepository,
    AlreadyAProject,
    RemoteMissing,
    TargetOccupied,
}

/// Whether setup needs to clone before opening or importing the project.
pub fn should_clone(
    location: NewProjectLocation,
    observation: Observation,
) -> Result<bool, Refusal> {
    if observation.already_a_project {
        return Err(Refusal::AlreadyAProject);
    }
    match location {
        NewProjectLocation::NewFolder => Ok(false),
        NewProjectLocation::OpenFolder if observation.is_repository => Ok(false),
        NewProjectLocation::OpenFolder => Err(Refusal::NotARepository),
        NewProjectLocation::OpenGitHub | NewProjectLocation::OpenCollapse
            if observation.target_is_checkout =>
        {
            Ok(false)
        }
        NewProjectLocation::OpenGitHub | NewProjectLocation::OpenCollapse
            if observation.target_occupied =>
        {
            Err(Refusal::TargetOccupied)
        }
        NewProjectLocation::OpenGitHub | NewProjectLocation::OpenCollapse
            if !observation.remote_exists =>
        {
            Err(Refusal::RemoteMissing)
        }
        NewProjectLocation::OpenGitHub | NewProjectLocation::OpenCollapse => Ok(true),
    }
}
