//! F6 targeted updates run at the receiving agent's next step, never mid-edit.
use crate::{Channel, Error, Event};
use std::path::Path;

impl Channel {
    /// Only the launcher's registered checkout is updated; git preserves dirty work
    /// and conflicts. The agent commits its current edits before invoking this tool.
    pub fn rebase(&self, member: &str) -> Result<(), Error> {
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
            Path::new(&checkout.worktree),
            &checkout.branch,
            &checkout.base,
        );
        match result {
            Ok(()) => {
                self.post_event(Event::new(
                    "rebased",
                    nick,
                    &format!(
                        "updated {} with {} without rewriting history",
                        checkout.branch, checkout.base
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
