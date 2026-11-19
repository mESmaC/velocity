//! The Collapse account credentials in the OS credential store (Collapse C1).
//!
//! One generic credential holds the account name and both tokens as JSON, so the connection
//! is read and removed as a unit. On Windows it appears in Credential Manager as
//! `velocity:collapse.sh`.

use serde::{Deserialize, Serialize};

use crate::CollapseError;

#[cfg(windows)]
const TARGET: &str = "velocity:collapse.sh";
#[cfg(windows)]
const SERVICE: &str = "collapse.sh";
#[cfg(windows)]
const USER: &str = "velocity";

/// What Collapse's token exchange returned for this machine's connection.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseCredentials {
    /// The Collapse namespace, used as the owner of repositories Velocity creates.
    pub account: String,
    /// The read-only personal access token for the MCP server (C9).
    pub mcp_token: String,
    /// The provisioning token for creating repositories and minting push tokens (#427).
    pub access_token: String,
}

// Tokens never reach a log through `{:?}`.
impl std::fmt::Debug for CollapseCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CollapseCredentials")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
fn entry() -> Result<keyring::Entry, CollapseError> {
    keyring::Entry::new_with_target(TARGET, SERVICE, USER)
        .map_err(|error| CollapseError::Credential(error.to_string()))
}

/// Replaces any saved connection with `credentials`.
#[cfg(windows)]
pub fn save_credentials(credentials: &CollapseCredentials) -> Result<(), CollapseError> {
    let secret = serde_json::to_vec(credentials)
        .map_err(|error| CollapseError::Credential(error.to_string()))?;
    entry()?
        .set_secret(&secret)
        .map_err(|error| CollapseError::Credential(error.to_string()))
}

/// The saved connection, or `None` when Collapse is not connected.
#[cfg(windows)]
pub fn load_credentials() -> Result<Option<CollapseCredentials>, CollapseError> {
    match entry()?.get_secret() {
        Ok(secret) => serde_json::from_slice(&secret).map(Some).map_err(|_| {
            CollapseError::Credential(
                "the saved Collapse connection is unreadable; disconnect and connect again".into(),
            )
        }),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(CollapseError::Credential(error.to_string())),
    }
}

/// Removes the saved connection. The tokens stay valid on Collapse until revoked there.
#[cfg(windows)]
pub fn forget_credentials() -> Result<(), CollapseError> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(CollapseError::Credential(error.to_string())),
    }
}

#[cfg(not(windows))]
pub fn save_credentials(_credentials: &CollapseCredentials) -> Result<(), CollapseError> {
    Err(CollapseError::Unsupported)
}

#[cfg(not(windows))]
pub fn load_credentials() -> Result<Option<CollapseCredentials>, CollapseError> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn forget_credentials() -> Result<(), CollapseError> {
    Ok(())
}
