//! A dependency continued on a Rosen stack ends its AIRC wait without a git update.
use crate::{Channel, Error, channel::entry, storage::Storage};

impl Channel {
    pub fn finish_stacked_wait(&self, member: &str, message: &str) -> Result<(), Error> {
        let Some(wait) = self
            .dependency_waits()?
            .into_iter()
            .find(|wait| wait.member == member)
        else {
            return Ok(());
        };
        let storage = Storage::open(self)?;
        let entries = storage.read()?;
        let mut record = entry(&entries, "unblocked", "velocity", &wait.nick, message);
        record.change = crate::claims::member_change(&entries, member);
        record.dependency = Some(wait);
        storage.append(&record)
    }
}
