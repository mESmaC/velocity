//! Connecting Velocity to the owner's Collapse account (Collapse C1 desktop sign-in, #430).
//!
//! One browser approval gives Velocity two account credentials: a read-only personal access
//! token for Collapse's MCP server (C9) and a provisioning token for creating repositories.
//! Both live in the OS credential store, never in `state.json` (SPEC.md §1, F15).

pub mod connect;
pub mod credential;
mod url_text;

pub use connect::connect;
pub use credential::{CollapseCredentials, forget_credentials, load_credentials};

/// Collapse's read-only MCP endpoint (Collapse C9), streamable HTTP with a bearer token.
pub const MCP_URL: &str = "https://api.collapse.sh/mcp";

/// The name agents see for Collapse's MCP server among the shared servers.
pub const MCP_SERVER_NAME: &str = "collapse";

#[derive(Debug, thiserror::Error)]
pub enum CollapseError {
    #[error("Could not listen for Collapse's sign-in reply: {0}")]
    Listen(#[from] std::io::Error),
    #[error("Could not open the browser for Collapse sign-in: {0}")]
    Browser(String),
    #[error("Collapse sign-in timed out; try Connect again")]
    TimedOut,
    #[error("The Collapse connection was denied in the browser")]
    Denied,
    #[error("Collapse sign-in failed: {0}")]
    Exchange(String),
    #[error("Could not use the OS credential store: {0}")]
    Credential(String),
    #[error("Collapse sign-in needs the Windows credential store on this platform")]
    Unsupported,
}
