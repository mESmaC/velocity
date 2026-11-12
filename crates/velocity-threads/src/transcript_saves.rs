//! How often a streaming thread's transcript is copied into `state.json` (#273).
//!
//! Each save rewrites and syncs the whole file under the store lock, and the window's
//! commands wait on that lock on its UI thread. Saving after every streamed chunk of every
//! agent kept the lock busy for seconds at a time once a swarm ran, so the window stopped
//! responding. The window shows the live transcript meanwhile, so a streaming thread saves
//! at most once per interval, and the end of a turn always saves.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::runtime::ThreadKey;

/// At most this much of a running turn is lost if Velocity itself crashes mid-turn.
pub const TRANSCRIPT_SAVE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct TranscriptSaves {
    last_saved: BTreeMap<ThreadKey, Instant>,
}

impl TranscriptSaves {
    /// Whether the thread's transcript should be saved now. A due save is recorded as made.
    pub fn due(&mut self, key: ThreadKey, now: Instant, turn_over: bool) -> bool {
        let due = turn_over
            || self.last_saved.get(&key).is_none_or(|saved| {
                now.saturating_duration_since(*saved) >= TRANSCRIPT_SAVE_INTERVAL
            });
        if due {
            self.last_saved.insert(key, now);
        }
        due
    }
}
