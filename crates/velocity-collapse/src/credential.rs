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

#[cfg(windows)]
const RELEASE_TOKEN_TARGET: &str = "velocity:collapse.sh:releases";
#[cfg(windows)]
const RELEASE_TOKEN_USER: &str = "velocity-releases";

#[cfg(windows)]
const ISSUES_TOKEN_TARGET: &str = "velocity:collapse.sh:issues";
#[cfg(windows)]
const ISSUES_TOKEN_USER: &str = "velocity-issues";

#[cfg(windows)]
const UPDATER_KEY_TARGET: &str = "velocity:updater-signing-key";
#[cfg(windows)]
const UPDATER_PASSWORD_TARGET: &str = "velocity:updater-signing-key-password";
#[cfg(windows)]
const UPDATER_SERVICE: &str = "velocity-updater";
#[cfg(windows)]
const UPDATER_KEY_USER: &str = "signing-key";
#[cfg(windows)]
const UPDATER_PASSWORD_USER: &str = "signing-key-password";

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

/// The Tauri updater signing key held in Windows Credential Manager (#593).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdaterSigningKey {
    /// The minisign private key contents (base64 or minisign key text).
    pub private_key: String,
    /// Optional password protecting the private key.
    #[serde(default)]
    pub password: Option<String>,
}

// Private key and password never reach a log through `{:?}`.
impl std::fmt::Debug for UpdaterSigningKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpdaterSigningKey")
            .field("has_password", &self.password.is_some())
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

/// Saves a Collapse personal access token with the `releases` scope in Windows Credential
/// Manager under `velocity:collapse.sh:releases` (#593).
#[cfg(windows)]
pub fn save_release_token(token: &str) -> Result<(), CollapseError> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(CollapseError::Credential(
            "Collapse release token cannot be empty".into(),
        ));
    }
    release_token_entry()?
        .set_secret(trimmed.as_bytes())
        .map_err(|error| CollapseError::Credential(error.to_string()))
}

/// Loads the optional Collapse releases token from Windows Credential Manager (#593).
#[cfg(windows)]
pub fn load_release_token() -> Result<Option<String>, CollapseError> {
    read_text_secret(&release_token_entry()?)
}

/// Removes the saved Collapse releases token from Windows Credential Manager (#593).
#[cfg(windows)]
pub fn forget_release_token() -> Result<(), CollapseError> {
    delete_entry(&release_token_entry()?)
}

#[cfg(not(windows))]
pub fn save_release_token(_token: &str) -> Result<(), CollapseError> {
    Err(CollapseError::Unsupported)
}

#[cfg(not(windows))]
pub fn load_release_token() -> Result<Option<String>, CollapseError> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn forget_release_token() -> Result<(), CollapseError> {
    Ok(())
}

/// Resolves the bearer token used to publish Collapse releases (#593): prefers a dedicated
/// personal access token with `releases` scope (`velocity:collapse.sh:releases`) and falls
/// back to the connected Collapse account's desktop `access_token`.
pub fn resolve_release_token() -> Result<Option<String>, CollapseError> {
    if let Some(token) = load_release_token()? {
        return Ok(Some(token));
    }
    Ok(load_credentials()?.map(|credentials| credentials.access_token))
}

/// Saves a Collapse personal access token with the `issues` scope in Windows Credential
/// Manager under `velocity:collapse.sh:issues` (#589).
#[cfg(windows)]
pub fn save_issues_token(token: &str) -> Result<(), CollapseError> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(CollapseError::Credential(
            "Collapse issues token cannot be empty".into(),
        ));
    }
    issues_token_entry()?
        .set_secret(trimmed.as_bytes())
        .map_err(|error| CollapseError::Credential(error.to_string()))
}

/// Loads the optional Collapse issues token from Windows Credential Manager (#589).
#[cfg(windows)]
pub fn load_issues_token() -> Result<Option<String>, CollapseError> {
    read_text_secret(&issues_token_entry()?)
}

/// Removes the saved Collapse issues token from Windows Credential Manager (#589).
#[cfg(windows)]
pub fn forget_issues_token() -> Result<(), CollapseError> {
    delete_entry(&issues_token_entry()?)
}

#[cfg(not(windows))]
pub fn save_issues_token(_token: &str) -> Result<(), CollapseError> {
    Err(CollapseError::Unsupported)
}

#[cfg(not(windows))]
pub fn load_issues_token() -> Result<Option<String>, CollapseError> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn forget_issues_token() -> Result<(), CollapseError> {
    Ok(())
}

/// Resolves the personal bearer token used for Collapse C14 Issues (#589): prefers a
/// dedicated personal access token with `issues` scope (`velocity:collapse.sh:issues`,
/// exempt from per-IP/per-token rate limits and allowed to write) and falls back to the
/// connected account's read-only personal `mcp_token`.
pub fn resolve_issues_token() -> Result<Option<String>, CollapseError> {
    if let Some(token) = load_issues_token()? {
        return Ok(Some(token));
    }
    Ok(load_credentials()?.map(|credentials| credentials.mcp_token))
}

/// Saves the Tauri updater signing key (and optional password) in Windows Credential Manager
/// under `velocity:updater-signing-key` (#593).
#[cfg(windows)]
pub fn save_updater_signing_key(key: &UpdaterSigningKey) -> Result<(), CollapseError> {
    let private_key = key.private_key.trim();
    if private_key.is_empty() {
        return Err(CollapseError::Credential(
            "Updater signing private key cannot be empty".into(),
        ));
    }
    let normalized = UpdaterSigningKey {
        private_key: private_key.to_owned(),
        password: key
            .password
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
    };
    let secret = serde_json::to_vec(&normalized)
        .map_err(|error| CollapseError::Credential(error.to_string()))?;
    updater_key_entry()?
        .set_secret(&secret)
        .map_err(|error| CollapseError::Credential(error.to_string()))?;
    let _ = delete_entry(&updater_password_entry()?);
    Ok(())
}

/// Loads the Tauri updater signing key from Windows Credential Manager (#593).
/// Accepts either JSON (`{"private_key": "...", "password": "..."}`) stored by Velocity or
/// plain key text stored directly under `velocity:updater-signing-key` (with an optional
/// separate password under `velocity:updater-signing-key-password`).
#[cfg(windows)]
pub fn load_updater_signing_key() -> Result<Option<UpdaterSigningKey>, CollapseError> {
    let secret = match updater_key_entry()?.get_secret() {
        Ok(secret) => secret,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(error) => return Err(CollapseError::Credential(error.to_string())),
    };
    if let Ok(parsed) = serde_json::from_slice::<UpdaterSigningKey>(&secret) {
        let trimmed = parsed.private_key.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        return Ok(Some(UpdaterSigningKey {
            private_key: trimmed.to_owned(),
            password: parsed.password.filter(|text| !text.is_empty()),
        }));
    }
    let raw = String::from_utf8(secret).map_err(|_| {
        CollapseError::Credential(
            "the saved updater signing key is not valid UTF-8; replace it in Settings".into(),
        )
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let password = read_text_secret(&updater_password_entry()?)?;
    Ok(Some(UpdaterSigningKey {
        private_key: trimmed.to_owned(),
        password,
    }))
}

/// Removes the Tauri updater signing key from Windows Credential Manager (#593).
#[cfg(windows)]
pub fn forget_updater_signing_key() -> Result<(), CollapseError> {
    delete_entry(&updater_key_entry()?)?;
    delete_entry(&updater_password_entry()?)
}

#[cfg(not(windows))]
pub fn save_updater_signing_key(_key: &UpdaterSigningKey) -> Result<(), CollapseError> {
    Err(CollapseError::Unsupported)
}

#[cfg(not(windows))]
pub fn load_updater_signing_key() -> Result<Option<UpdaterSigningKey>, CollapseError> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn forget_updater_signing_key() -> Result<(), CollapseError> {
    Ok(())
}

#[cfg(windows)]
fn release_token_entry() -> Result<keyring::Entry, CollapseError> {
    keyring::Entry::new_with_target(RELEASE_TOKEN_TARGET, SERVICE, RELEASE_TOKEN_USER)
        .map_err(|error| CollapseError::Credential(error.to_string()))
}

#[cfg(windows)]
fn issues_token_entry() -> Result<keyring::Entry, CollapseError> {
    keyring::Entry::new_with_target(ISSUES_TOKEN_TARGET, SERVICE, ISSUES_TOKEN_USER)
        .map_err(|error| CollapseError::Credential(error.to_string()))
}

#[cfg(windows)]
fn updater_key_entry() -> Result<keyring::Entry, CollapseError> {
    keyring::Entry::new_with_target(UPDATER_KEY_TARGET, UPDATER_SERVICE, UPDATER_KEY_USER)
        .map_err(|error| CollapseError::Credential(error.to_string()))
}

#[cfg(windows)]
fn updater_password_entry() -> Result<keyring::Entry, CollapseError> {
    keyring::Entry::new_with_target(
        UPDATER_PASSWORD_TARGET,
        UPDATER_SERVICE,
        UPDATER_PASSWORD_USER,
    )
    .map_err(|error| CollapseError::Credential(error.to_string()))
}

#[cfg(windows)]
fn read_text_secret(entry: &keyring::Entry) -> Result<Option<String>, CollapseError> {
    match entry.get_secret() {
        Ok(secret) => {
            let text = String::from_utf8(secret).map_err(|_| {
                CollapseError::Credential("stored credential is not valid UTF-8".into())
            })?;
            let trimmed = text.trim();
            Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(CollapseError::Credential(error.to_string())),
    }
}

#[cfg(windows)]
fn delete_entry(entry: &keyring::Entry) -> Result<(), CollapseError> {
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(CollapseError::Credential(error.to_string())),
    }
}
