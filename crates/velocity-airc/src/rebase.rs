//! F6 targeted updates run through Rosen at the receiving agent's next step, never via git.
use crate::{Channel, Error};
use std::path::Path;

impl Channel {
    /// Build threads register their workspace before ACP starts. Other sessions inherit
    /// the project's working directory, including duty agents and the spec interview.
    pub fn is_rosen_project(&self, member: &str) -> Result<bool, Error> {
        if self.remote.is_some() {
            return Ok(true);
        }
        let entries = self.read(0)?;
        if let Some(checkout) = entries
            .iter()
            .rev()
            .find(|entry| entry.member == member && entry.checkout.is_some())
            .and_then(|entry| entry.checkout.as_ref())
        {
            return Ok(Path::new(&checkout.repository).join("rosen.toml").is_file()
                || Path::new(checkout.workspace_path())
                    .join("rosen.toml")
                    .is_file());
        }
        Ok(std::env::current_dir()?.join("rosen.toml").is_file())
    }

    /// Rosen applies rebases at step boundaries; git worktree rebases have been removed (#418).
    pub fn rebase(&self, _member: &str) -> Result<(), Error> {
        Err(Error::Invalid(
            "Rosen applies rebases at step boundaries; airc_rebase is not needed on this project."
                .into(),
        ))
    }
}
