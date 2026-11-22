//! The Collapse account credentials in the OS credential store (Collapse C1).
//!
//! One generic credential holds the account name and both tokens as JSON, so the connection
//! is read and removed as a unit. On Windows it appears in Credential Manager as
//! `velocity:collapse.sh`; on Linux it is a Secret Service item (GNOME Keyring, KWallet)
//! with service `velocity:collapse.sh` (see `os_store`).

use serde::{Deserialize, Serialize};

use crate::CollapseError;

#[cfg(any(windows, target_os = "linux"))]
const TARGET: &str = "velocity:collapse.sh";
#[cfg(any(windows, target_os = "linux"))]
const SERVICE: &str = "collapse.sh";
#[cfg(any(windows, target_os = "linux"))]
const USER: &str = "velocity";

#[cfg(any(windows, target_os = "linux"))]
const RELEASE_TOKEN_TARGET: &str = "velocity:collapse.sh:releases";
#[cfg(any(windows, target_os = "linux"))]
const RELEASE_TOKEN_USER: &str = "velocity-releases";

#[cfg(any(windows, target_os = "linux"))]
const ISSUES_TOKEN_TARGET: &str = "velocity:collapse.sh:issues";
#[cfg(any(windows, target_os = "linux"))]
const ISSUES_TOKEN_USER: &str = "velocity-issues";

#[cfg(any(windows, target_os = "linux"))]
const UPDATER_KEY_TARGET: &str = "velocity:updater-signing-key";
#[cfg(any(windows, target_os = "linux"))]
const UPDATER_PASSWORD_TARGET: &str = "velocity:updater-signing-key-password";
#[cfg(any(windows, target_os = "linux"))]
const UPDATER_SERVICE: &str = "velocity-updater";
#[cfg(any(windows, target_os = "linux"))]
const UPDATER_KEY_USER: &str = "signing-key";
#[cfg(any(windows, target_os = "linux"))]
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
    /// Normalized machine name sent during sign-in (`Velocity on <machine>`), used when
    /// minting `velocity-releases-<machine>` tokens (#607).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

// Tokens never reach a log through `{:?}`.
impl std::fmt::Debug for CollapseCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CollapseCredentials")
            .field("account", &self.account)
            .field("machine", &self.machine)
            .finish_non_exhaustive()
    }
}

/// How the saved Collapse release token in Windows Credential Manager was created (#607).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseTokenSource {
    /// Pasted manually in Settings → Collapse → Release credentials.
    Pasted,
    /// Minted automatically from the signed-in Collapse desktop session (C1).
    SignIn,
}

fn default_release_token_source() -> ReleaseTokenSource {
    ReleaseTokenSource::Pasted
}

/// The Collapse release token and its provenance stored under `velocity:collapse.sh:releases`
/// (#593, #607).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredReleaseToken {
    /// The personal access token secret (`clp_pat_...`).
    pub token: String,
    /// Whether the owner pasted this token or Velocity created it from sign-in.
    #[serde(default = "default_release_token_source")]
    pub source: ReleaseTokenSource,
    /// RFC 3339 UTC timestamp returned by Collapse when created from sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

// Token value never reaches a log through `{:?}`.
impl std::fmt::Debug for StoredReleaseToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredReleaseToken")
            .field("source", &self.source)
            .field("created_at", &self.created_at)
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

#[cfg(any(windows, target_os = "linux"))]
fn entry() -> Result<keyring::Entry, CollapseError> {
    crate::os_store::entry(TARGET, SERVICE, USER)
}

/// Replaces any saved connection with `credentials`.
#[cfg(any(windows, target_os = "linux"))]
pub fn save_credentials(credentials: &CollapseCredentials) -> Result<(), CollapseError> {
    let secret = serde_json::to_vec(credentials)
        .map_err(|error| CollapseError::Credential(error.to_string()))?;
    entry()?
        .set_secret(&secret)
        .map_err(|error| CollapseError::Credential(error.to_string()))
}

/// The saved connection, or `None` when Collapse is not connected.
#[cfg(any(windows, target_os = "linux"))]
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
#[cfg(any(windows, target_os = "linux"))]
pub fn forget_credentials() -> Result<(), CollapseError> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(CollapseError::Credential(error.to_string())),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn save_credentials(_credentials: &CollapseCredentials) -> Result<(), CollapseError> {
    Err(CollapseError::Unsupported)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn load_credentials() -> Result<Option<CollapseCredentials>, CollapseError> {
    Ok(None)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn forget_credentials() -> Result<(), CollapseError> {
    Ok(())
}

/// Saves a manually pasted Collapse personal access token with the `releases` scope in
/// Windows Credential Manager under `velocity:collapse.sh:releases` (#593, #607).
#[cfg(any(windows, target_os = "linux"))]
pub fn save_release_token(token: &str) -> Result<(), CollapseError> {
    let record = StoredReleaseToken {
        token: token.to_owned(),
        source: ReleaseTokenSource::Pasted,
        created_at: None,
    };
    save_release_token_record(&record).map(|_| ())
}

/// Saves a Collapse `releases`-scoped token minted from the signed-in desktop session in
/// Windows Credential Manager under `velocity:collapse.sh:releases` (#607).
#[cfg(any(windows, target_os = "linux"))]
pub fn save_sign_in_release_token(
    token: &str,
    created_at: Option<&str>,
) -> Result<StoredReleaseToken, CollapseError> {
    let record = StoredReleaseToken {
        token: token.to_owned(),
        source: ReleaseTokenSource::SignIn,
        created_at: created_at.and_then(trim_non_empty),
    };
    save_release_token_record(&record)
}

#[cfg(any(windows, target_os = "linux"))]
fn save_release_token_record(
    record: &StoredReleaseToken,
) -> Result<StoredReleaseToken, CollapseError> {
    let (normalized, secret) = serialize_release_token_record(record)?;
    release_token_entry()?
        .set_secret(&secret)
        .map_err(|error| CollapseError::Credential(error.to_string()))?;
    Ok(normalized)
}

/// Loads the saved Collapse releases token and its provenance from Windows Credential
/// Manager (#593, #607). Accepts both JSON metadata written by Velocity and legacy plain
/// token text (treated as a pasted token).
#[cfg(any(windows, target_os = "linux"))]
pub fn load_release_token_record() -> Result<Option<StoredReleaseToken>, CollapseError> {
    match release_token_entry()?.get_secret() {
        Ok(secret) => parse_release_token_secret(&secret),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(CollapseError::Credential(error.to_string())),
    }
}

/// Loads the optional Collapse releases token value from Windows Credential Manager (#593).
#[cfg(any(windows, target_os = "linux"))]
pub fn load_release_token() -> Result<Option<String>, CollapseError> {
    Ok(load_release_token_record()?.map(|record| record.token))
}

/// Removes the saved Collapse releases token from Windows Credential Manager (#593).
#[cfg(any(windows, target_os = "linux"))]
pub fn forget_release_token() -> Result<(), CollapseError> {
    delete_entry(&release_token_entry()?)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn save_release_token(_token: &str) -> Result<(), CollapseError> {
    Err(CollapseError::Unsupported)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn save_sign_in_release_token(
    _token: &str,
    _created_at: Option<&str>,
) -> Result<StoredReleaseToken, CollapseError> {
    Err(CollapseError::Unsupported)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn load_release_token_record() -> Result<Option<StoredReleaseToken>, CollapseError> {
    Ok(None)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn load_release_token() -> Result<Option<String>, CollapseError> {
    Ok(None)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn forget_release_token() -> Result<(), CollapseError> {
    Ok(())
}

/// Returns any saved Collapse release token in Windows Credential Manager (#593, #607).
/// Desktop `access_token` credentials are refused on release routes; callers that create
/// a token from the signed-in session should use `ensure_release_token`.
pub fn resolve_release_token() -> Result<Option<String>, CollapseError> {
    load_release_token()
}

fn serialize_release_token_record(
    record: &StoredReleaseToken,
) -> Result<(StoredReleaseToken, Vec<u8>), CollapseError> {
    let Some(token) = trim_non_empty(&record.token) else {
        return Err(CollapseError::Credential(
            "Collapse release token cannot be empty".into(),
        ));
    };
    let normalized = StoredReleaseToken {
        token,
        source: record.source,
        created_at: record.created_at.as_deref().and_then(trim_non_empty),
    };
    let secret = serde_json::to_vec(&normalized)
        .map_err(|error| CollapseError::Credential(error.to_string()))?;
    Ok((normalized, secret))
}

fn parse_release_token_secret(secret: &[u8]) -> Result<Option<StoredReleaseToken>, CollapseError> {
    if let Ok(parsed) = serde_json::from_slice::<StoredReleaseToken>(secret) {
        return Ok(
            trim_non_empty(&parsed.token).map(|token| StoredReleaseToken {
                token,
                source: parsed.source,
                created_at: parsed.created_at.as_deref().and_then(trim_non_empty),
            }),
        );
    }
    let raw = String::from_utf8(secret.to_vec())
        .map_err(|_| CollapseError::Credential("stored credential is not valid UTF-8".into()))?;
    Ok(trim_non_empty(&raw).map(|token| StoredReleaseToken {
        token,
        source: ReleaseTokenSource::Pasted,
        created_at: None,
    }))
}

fn trim_non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Saves a Collapse personal access token with the `issues` scope in Windows Credential
/// Manager under `velocity:collapse.sh:issues` (#589).
#[cfg(any(windows, target_os = "linux"))]
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
#[cfg(any(windows, target_os = "linux"))]
pub fn load_issues_token() -> Result<Option<String>, CollapseError> {
    read_text_secret(&issues_token_entry()?)
}

/// Removes the saved Collapse issues token from Windows Credential Manager (#589).
#[cfg(any(windows, target_os = "linux"))]
pub fn forget_issues_token() -> Result<(), CollapseError> {
    delete_entry(&issues_token_entry()?)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn save_issues_token(_token: &str) -> Result<(), CollapseError> {
    Err(CollapseError::Unsupported)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn load_issues_token() -> Result<Option<String>, CollapseError> {
    Ok(None)
}

#[cfg(not(any(windows, target_os = "linux")))]
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
#[cfg(any(windows, target_os = "linux"))]
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
#[cfg(any(windows, target_os = "linux"))]
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
#[cfg(any(windows, target_os = "linux"))]
pub fn forget_updater_signing_key() -> Result<(), CollapseError> {
    delete_entry(&updater_key_entry()?)?;
    delete_entry(&updater_password_entry()?)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn save_updater_signing_key(_key: &UpdaterSigningKey) -> Result<(), CollapseError> {
    Err(CollapseError::Unsupported)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn load_updater_signing_key() -> Result<Option<UpdaterSigningKey>, CollapseError> {
    Ok(None)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn forget_updater_signing_key() -> Result<(), CollapseError> {
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn release_token_entry() -> Result<keyring::Entry, CollapseError> {
    crate::os_store::entry(RELEASE_TOKEN_TARGET, SERVICE, RELEASE_TOKEN_USER)
}

#[cfg(any(windows, target_os = "linux"))]
fn issues_token_entry() -> Result<keyring::Entry, CollapseError> {
    crate::os_store::entry(ISSUES_TOKEN_TARGET, SERVICE, ISSUES_TOKEN_USER)
}

#[cfg(any(windows, target_os = "linux"))]
fn updater_key_entry() -> Result<keyring::Entry, CollapseError> {
    crate::os_store::entry(UPDATER_KEY_TARGET, UPDATER_SERVICE, UPDATER_KEY_USER)
}

#[cfg(any(windows, target_os = "linux"))]
fn updater_password_entry() -> Result<keyring::Entry, CollapseError> {
    crate::os_store::entry(
        UPDATER_PASSWORD_TARGET,
        UPDATER_SERVICE,
        UPDATER_PASSWORD_USER,
    )
}

#[cfg(any(windows, target_os = "linux"))]
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

#[cfg(any(windows, target_os = "linux"))]
fn delete_entry(entry: &keyring::Entry) -> Result<(), CollapseError> {
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(CollapseError::Credential(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk: persisted credential payloads in Windows Credential Manager must survive save/load
    // and keep pre-#607 plain-text release tokens and legacy CollapseCredentials readable.
    #[test]
    fn release_token_record_round_trips_and_reads_legacy_plain_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let sign_in = StoredReleaseToken {
            token: "  clp_pat_sign_in_secret  ".into(),
            source: ReleaseTokenSource::SignIn,
            created_at: Some(" 2026-11-22T04:00:00Z ".into()),
        };
        let (normalized, encoded) = serialize_release_token_record(&sign_in)?;
        assert_eq!(normalized.token, "clp_pat_sign_in_secret");
        assert_eq!(
            normalized.created_at.as_deref(),
            Some("2026-11-22T04:00:00Z")
        );
        let decoded = parse_release_token_secret(&encoded)?;
        assert_eq!(decoded, Some(normalized));

        let legacy = parse_release_token_secret(b"  clp_pat_legacy_pasted \n")?;
        assert_eq!(
            legacy,
            Some(StoredReleaseToken {
                token: "clp_pat_legacy_pasted".into(),
                source: ReleaseTokenSource::Pasted,
                created_at: None,
            })
        );

        let legacy_credentials: CollapseCredentials = serde_json::from_str(
            r#"{"account":"mESmaC","mcp_token":"clp_pat_a","access_token":"clp_desktop_b"}"#,
        )?;
        assert_eq!(legacy_credentials.machine, None);
        assert!(!format!("{sign_in:?}").contains("clp_pat_sign_in_secret"));
        Ok(())
    }
}
