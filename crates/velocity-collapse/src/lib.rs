//! Connecting Velocity to the owner's Collapse account (Collapse C1 desktop sign-in, #430)
//! and making Collapse each project's mirror (#427).
//!
//! One browser approval gives Velocity two account credentials: a read-only personal access
//! token for Collapse's MCP server (C9) and a provisioning token for creating repositories.
//! Both live in the OS credential store, never in `state.json` (SPEC.md §1, F15). Each
//! repository's push token goes to the machine's git credential helper instead.

pub mod connect;
pub mod credential;
mod git_credential;
pub mod health;
pub mod provision;
mod url_text;

pub use connect::connect;
pub use credential::{CollapseCredentials, forget_credentials, load_credentials};
pub use git_credential::{
    MIN_PROACTIVE_AUTH_GIT_VERSION, PROACTIVE_AUTH_KEY, ProactiveAuthStatus, check_proactive_auth,
    ensure_proactive_auth,
};
pub use health::{RepositoryActivity, repository_activity};
pub use provision::{MirrorRequest, ProvisionedMirror, provision_mirror, share_push_token};

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
    #[error("{0}")]
    Api(String),
    #[error("{0}")]
    Git(String),
}
