//! How `session/update` notifications become a thread's transcript (SPEC.md §3.2, F4).
//!
//! Each entry shows one thing: a user message, an agent message, the agent's thoughts, a
//! tool call or the plan. Message chunks and plan updates join the last entry when it shows
//! the same thing, so a streamed message is one entry. A tool call's updates join that call's
//! entry wherever it is, so a tool step updates in place. Every update is
//! kept, in order, under its own entry.

/// The message a chunk belongs to: ACP's optional `messageId`. Two messages streamed back to
/// back stay apart when the agent names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageKey<Key> {
    Unnamed,
    Named(Key),
}

/// What an entry shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryClass<Key> {
    UserMessage(MessageKey<Key>),
    AgentMessage(MessageKey<Key>),
    AgentThought(MessageKey<Key>),
    ToolCall(Key),
    Plan,
}

/// One entry: what it shows and the updates it holds, oldest first. The thread view merges a
/// tool call's updates and shows a plan's latest version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry<Key, Payload> {
    pub class: EntryClass<Key>,
    pub payloads: Vec<Payload>,
}

/// Where [`Transcript::record`] put an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Added to the existing entry at this index.
    Appended(usize),
    /// Started a new entry at this index.
    Added(usize),
}

/// A thread's transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript<Key, Payload> {
    entries: Vec<Entry<Key, Payload>>,
}

impl<Key, Payload> Default for Transcript<Key, Payload> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Key, Payload> Transcript<Key, Payload> {
    pub fn new() -> Self {
        Transcript {
            entries: Vec::new(),
        }
    }

    pub fn entries(&self) -> &[Entry<Key, Payload>] {
        &self.entries
    }
}

impl<Key: PartialEq, Payload> Transcript<Key, Payload> {
    pub fn record(&mut self, class: EntryClass<Key>, payload: Payload) -> Placement {
        match entry_for(&self.entries, &class) {
            Some(index) => {
                self.entries[index].payloads.push(payload);
                Placement::Appended(index)
            }
            None => {
                let index = self.entries.len();
                self.entries.push(Entry {
                    class,
                    payloads: Vec::from([payload]),
                });
                Placement::Added(index)
            }
        }
    }
}

/// The entry an update joins, or `None` when it starts a new one.
fn entry_for<Key: PartialEq, Payload>(
    entries: &[Entry<Key, Payload>],
    class: &EntryClass<Key>,
) -> Option<usize> {
    if matches!(class, EntryClass::ToolCall(_)) {
        find_class(entries, class)
    } else {
        let last = entries.len().checked_sub(1)?;
        (entries[last].class == *class).then_some(last)
    }
}

/// The first entry that shows `class`.
fn find_class<Key: PartialEq, Payload>(
    entries: &[Entry<Key, Payload>],
    class: &EntryClass<Key>,
) -> Option<usize> {
    entries.iter().position(|entry| entry.class == *class)
}
