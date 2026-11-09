//! How `session/update` notifications become a thread's transcript (SPEC.md §3.2, F4).
//!
//! Each entry shows one thing: a user message, an agent message, the agent's thoughts, a
//! tool call or the plan. Message chunks and plan updates join the last entry when it shows
//! the same thing, so a streamed message is one entry. A tool call's updates join that call's
//! entry wherever it is, so a tool step updates in place. `proofs/AcpClient/Design.lean`
//! (`transcript`) proves that every update is kept, in order, under its own entry, and that no
//! transcript showing things in the order they happened has fewer entries.

/// The message a chunk belongs to: ACP's optional `messageId`. Two messages streamed back to
/// back stay apart when the agent names them.
///
/// Its own type rather than `Option<Key>`: the derived impls then translate without
/// `Option`'s `Clone`, `Debug` and `PartialEq`, which the pinned Aeneas library does not model.
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

fn is_tool_call<Key>(class: &EntryClass<Key>) -> bool {
    match class {
        EntryClass::ToolCall(_) => true,
        EntryClass::UserMessage(_)
        | EntryClass::AgentMessage(_)
        | EntryClass::AgentThought(_)
        | EntryClass::Plan => false,
    }
}

/// The entry an update joins, or `None` when it starts a new one.
fn entry_for<Key: PartialEq, Payload>(
    entries: &[Entry<Key, Payload>],
    class: &EntryClass<Key>,
) -> Option<usize> {
    if is_tool_call(class) {
        find_class(entries, class)
    } else if entries.is_empty() {
        None
    } else {
        let last = entries.len() - 1;
        if same_class(&entries[last].class, class) {
            Some(last)
        } else {
            None
        }
    }
}

/// The first entry that shows `class`.
fn find_class<Key: PartialEq, Payload>(
    entries: &[Entry<Key, Payload>],
    class: &EntryClass<Key>,
) -> Option<usize> {
    let mut index = 0;
    while index < entries.len() {
        if same_class(&entries[index].class, class) {
            return Some(index);
        }
        index += 1;
    }
    None
}

// Written out rather than derived so that the translation compares keys only through
// `Key::eq`, the one assumption the confidence proof makes about keys.
fn same_class<Key: PartialEq>(first: &EntryClass<Key>, second: &EntryClass<Key>) -> bool {
    match first {
        EntryClass::UserMessage(first) => match second {
            EntryClass::UserMessage(second) => same_message(first, second),
            _ => false,
        },
        EntryClass::AgentMessage(first) => match second {
            EntryClass::AgentMessage(second) => same_message(first, second),
            _ => false,
        },
        EntryClass::AgentThought(first) => match second {
            EntryClass::AgentThought(second) => same_message(first, second),
            _ => false,
        },
        EntryClass::ToolCall(first) => match second {
            EntryClass::ToolCall(second) => first == second,
            _ => false,
        },
        EntryClass::Plan => matches!(second, EntryClass::Plan),
    }
}

fn same_message<Key: PartialEq>(first: &MessageKey<Key>, second: &MessageKey<Key>) -> bool {
    match first {
        MessageKey::Named(first) => match second {
            MessageKey::Named(second) => first == second,
            MessageKey::Unnamed => false,
        },
        MessageKey::Unnamed => matches!(second, MessageKey::Unnamed),
    }
}
