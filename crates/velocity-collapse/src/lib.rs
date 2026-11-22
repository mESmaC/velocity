//! Connecting Velocity to the owner's Collapse account (Collapse C1 desktop sign-in, #430)
//! and making Collapse each project's mirror (#427).
//!
//! One browser approval gives Velocity two account credentials: a read-only personal access
//! token for Collapse's MCP server (C9) and a provisioning token for creating repositories.
//! Both live in the OS credential store, never in `state.json` (SPEC.md §1, F15). Each
//! repository's push token goes to the machine's git credential helper instead.

pub mod connect;
pub mod credential;
pub mod deploy_credential;
pub mod downstream;
mod git_credential;
pub mod health;
pub mod issues;
pub mod post_land;
pub mod provision;
pub mod releases;
mod url_text;

pub use connect::connect;
pub use credential::{
    CollapseCredentials, UpdaterSigningKey, forget_credentials, forget_issues_token,
    forget_release_token, forget_updater_signing_key, load_credentials, load_issues_token,
    load_release_token, load_updater_signing_key, resolve_issues_token, resolve_release_token,
    save_issues_token, save_release_token, save_updater_signing_key,
};
pub use deploy_credential::{
    DeploySecretValue, deploy_credentials_supported, forget_deploy_secret, has_deploy_secret,
    load_deploy_secret, save_deploy_secret,
};
pub use downstream::{DownstreamSetup, set_downstream};
pub use git_credential::{
    MIN_PROACTIVE_AUTH_GIT_VERSION, PROACTIVE_AUTH_KEY, ProactiveAuthStatus, check_proactive_auth,
    checkout_repository, clone_repository, ensure_proactive_auth, ensure_remote_tag,
    resolve_remote_tag_commit,
};
pub use health::{RepositoryActivity, repository_activity, repository_downstream};
pub use post_land::{RepositoryPostLand, repository_post_land};
pub use provision::{
    MirrorRequest, ProvisionedMirror, check_push_token, prepare_existing_mirror, provision_mirror,
    share_push_token,
};

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
