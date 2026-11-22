//! F14 project channels. Claims and step-boundary delivery are separate layers.
mod channel;
pub mod claims;
mod comment;
mod delivery;
mod event_log;
mod issue_tools;
mod launch;
mod next_work_tools;
mod payload;
mod presence;
mod process;
mod protocol;
mod read_tools;
mod reap;
mod rebase;
mod remote;
mod remote_claims;
mod remote_delivery;
mod remote_events;
mod remote_waits;
mod routing;
mod stack;
mod storage;
mod tail;
mod task_tools;
mod tasks;
pub mod waits;

pub use channel::{
    Channel, DiscoveryKind, DiscoveryReportInput, EVENT_TAGS, Entry, Event, FrictionReport,
    Presence, ToolRequestReport,
};
pub use claims::{Claim, ClaimResult, piece_key};
pub use delivery::{
    Delivery, DeliveryPriority, deliver_messages, deliver_targeted, message_for, prompt_lines,
};
pub use issue_tools::{IssueCreate, IssueRead, IssueRequest, IssueUpdate, IssueUpdateKind};
pub use launch::{
    CONTINUOUS_DISPATCH_ARGUMENT, LAZY_ARGUMENT, ROLE_ARGUMENT, SERVER_NAME, SERVER_SWITCH,
    serve_stdio, serve_stdio_with_handlers, serve_stdio_with_issues, server_arguments,
};
pub use next_work_tools::NextWorkRequest;
pub use payload::{EventPayload, OverlapDetail, OverlapSymbols, WokenDetail};
pub use protocol::serve;
pub use reap::{ReapedClaim, ScopedClaim};
pub use routing::{MessageMode, mode_for};
pub use tasks::{TaskChange, TaskListRow, TaskTarget};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("AIRC storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("AIRC record: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("Rosen AIRC: {0}")]
    Rosen(#[from] rosen_client::Error),
}
