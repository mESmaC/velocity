//! F14 project channels. Claims and step-boundary delivery are separate layers.
mod channel;
mod event_log;
mod protocol;
mod storage;

pub use channel::{Channel, EVENT_TAGS, Entry, Event, Presence};
pub use protocol::serve;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("AIRC storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("AIRC record: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}
