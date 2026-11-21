//! F6 targeted updates run at the receiving agent's next step, never mid-edit.
use crate::{Channel, Error, Event};
use std::path::Path;

impl Channel {
    /// Build threads register their checkout before ACP starts. Other sessions inherit
    /// the project's working directory, including duty agents and the spec interview.
    pub(crate) fn is_rosen_project(&self, member: &str) -> Result<bool, Error> {
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

    /// Only the launcher's registered checkout is updated, on whichever branch the
    /// thread has checked out there; git preserves dirty work and conflicts. The agent
    /// commits its current edits before invoking this tool.
    pub fn rebase(&self, member: &str) -> Result<(), Error> {
        if self.is_rosen_project(member)? {
            return Err(Error::Invalid(
                "Rosen applies rebases at step boundaries; airc_rebase is not needed on this project."
                    .into(),
            ));
        }
        let entries = self.read(0)?;
        let checkout = entries
            .iter()
            .rev()
            .find(|entry| entry.member == member && entry.checkout.is_some())
            .and_then(|entry| entry.checkout.as_ref())
            .ok_or_else(|| Error::Invalid("No registered build checkout".into()))?;
        let nick = entries
            .iter()
            .rev()
            .find(|entry| entry.member == member && entry.kind == "join")
            .map(|entry| entry.nick.as_str())
            .ok_or_else(|| Error::Invalid("Join first".into()))?;
        let result = velocity_git::continue_after_dependency(
            Path::new(&checkout.repository),
            Path::new(checkout.workspace_path()),
            &checkout.branch,
            &checkout.base,
        );
        match result {
            Ok(branch) => {
                self.post_event(Event::new(
                    "rebased",
                    nick,
                    &format!(
                        "updated {branch} with {} without rewriting history",
                        checkout.base
                    ),
                ))?;
                Ok(())
            }
            Err(error) => {
                self.post_event(
                    Event::new("conflict", nick, &error.to_string())
                        .addressed_to(vec![nick.into()]),
                )?;
                Err(Error::Invalid(error.to_string()))
            }
        }
    }
}
