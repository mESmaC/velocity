//! The project and targeted R9 streams have independent cursors. A quiet targeted
//! page must never be acknowledged through a later, unrelated project-view event.
use crate::{Channel, Delivery, Entry, Error};
use std::collections::{BTreeMap, BTreeSet};

struct Pending {
    entry: Entry,
    project: bool,
    targeted: bool,
}

pub(crate) fn deliver(
    channel: &Channel,
    nick: &str,
    after: u64,
    idle: bool,
    follows_github_reviews: bool,
    mut deliver: impl FnMut(&Delivery) -> Result<bool, String>,
) -> Result<u64, Error> {
    let remote = channel
        .remote
        .as_ref()
        .ok_or_else(|| Error::Invalid("Missing Rosen channel".into()))?;
    let native_after = remote.acknowledged_cursor(nick)?;
    let pending = pending(channel, nick, after, native_after, idle)?;
    let history = channel.snapshot()?;
    let acknowledged: BTreeSet<_> = history
        .iter()
        .filter(|entry| entry.nick == nick)
        .filter(|entry| entry.kind == "delivery_ack")
        .filter_map(|entry| entry.acknowledged)
        .collect();
    let mut project = Cursor::new(after);
    let mut targeted = Cursor::new(native_after);
    for pending in pending {
        let sequence = pending.entry.sequence;
        let message = if acknowledged.contains(&sequence) {
            None
        } else if pending.targeted && pending.entry.is_message() {
            crate::delivery::message_delivery(&pending.entry, nick)
        } else {
            crate::delivery::routed(
                &pending.entry,
                nick,
                &history,
                crate::MessageMode::Open,
                follows_github_reviews,
                pending.entry.kind == "merged" && pending.entry.pull_request.is_none(),
            )
        };
        let received = match message {
            Some(message) => {
                let received = deliver(&message).map_err(Error::Invalid)?;
                if received {
                    channel.acknowledge(nick, sequence)?;
                }
                received
            }
            None => true,
        };
        if pending.project {
            project.advance(sequence, received);
        }
        if pending.targeted {
            targeted.advance(sequence, received);
        }
    }
    remote.acknowledge_cursor(nick, targeted.sequence)?;
    Ok(project.sequence)
}

fn pending(
    channel: &Channel,
    nick: &str,
    after: u64,
    native_after: u64,
    idle: bool,
) -> Result<Vec<Pending>, Error> {
    let remote = channel
        .remote
        .as_ref()
        .ok_or_else(|| Error::Invalid("Missing Rosen channel".into()))?;
    let mut entries = BTreeMap::new();
    for entry in channel
        .read(after)?
        .into_iter()
        .filter(|entry| !entry.is_message())
    {
        entries.insert(
            entry.sequence,
            Pending {
                entry,
                project: true,
                targeted: false,
            },
        );
    }
    for entry in remote.targeted(nick, native_after, idle)? {
        if entry.kind == "overlap"
            && let Some(project) = entries.get_mut(&entry.sequence)
            && project.entry.kind == "overlap"
        {
            crate::remote_events::copy_overlap_symbols(&mut project.entry, &entry);
            project.targeted = true;
            continue;
        }
        let project = entries
            .get(&entry.sequence)
            .is_some_and(|pending| pending.project);
        entries.insert(
            entry.sequence,
            Pending {
                entry,
                project,
                targeted: true,
            },
        );
    }
    Ok(entries.into_values().collect())
}

struct Cursor {
    sequence: u64,
    blocked: bool,
}

impl Cursor {
    fn new(sequence: u64) -> Self {
        Self {
            sequence,
            blocked: false,
        }
    }

    fn advance(&mut self, sequence: u64, received: bool) {
        self.blocked |= !received;
        if !self.blocked {
            self.sequence = sequence;
        }
    }
}
