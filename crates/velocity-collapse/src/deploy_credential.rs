//! Per-project deploy secrets stored in Windows Credential Manager under
//! `velocity/deploy/<project>/<NAME>` (#594).
//!
//! Values are stored with native Windows UTF-16LE password encoding so credentials
//! created either in Velocity's Project Settings or directly in Windows Credential
//! Manager / `cmdkey` are interoperable. Secret values never appear in `Debug`
//! output or error messages.

use crate::CollapseError;

#[cfg(windows)]
const DEPLOY_SECRET_SERVICE: &str = "velocity-deploy";

/// An opaque deploy secret loaded from Windows Credential Manager (#594).
#[derive(Clone, PartialEq, Eq)]
pub struct DeploySecretValue(String);

impl DeploySecretValue {
    /// Returns the raw secret string solely for injecting into a deploy child process
    /// or redacting from that child process's captured output.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

// Secret values never reach a log through `{:?}`.
impl std::fmt::Debug for DeploySecretValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DeploySecretValue([REDACTED])")
    }
}

/// Whether this platform supports storing deploy secrets in the OS credential store.
pub const fn deploy_credentials_supported() -> bool {
    cfg!(windows)
}

/// Saves or replaces a deploy secret under `target` (`velocity/deploy/<project>/<NAME>`).
#[cfg(windows)]
pub fn save_deploy_secret(target: &str, name: &str, value: &str) -> Result<(), CollapseError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CollapseError::Credential(format!(
            "Deploy secret {name} cannot be empty"
        )));
    }
    deploy_entry(target, name)?
        .set_password(trimmed)
        .map_err(|error| CollapseError::Credential(error.to_string()))
}

/// Loads a deploy secret from Windows Credential Manager under `target`, or `None` if absent.
#[cfg(windows)]
pub fn load_deploy_secret(
    target: &str,
    name: &str,
) -> Result<Option<DeploySecretValue>, CollapseError> {
    let bytes = match deploy_entry(target, name)?.get_secret() {
        Ok(bytes) => bytes,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(error) => return Err(CollapseError::Credential(error.to_string())),
    };
    let decoded = decode_credential_bytes(&bytes, name)?;
    let trimmed = decoded.trim();
    Ok((!trimmed.is_empty()).then(|| DeploySecretValue(trimmed.to_owned())))
}

/// Reports whether a non-empty deploy secret is saved under `target` without returning it.
#[cfg(windows)]
pub fn has_deploy_secret(target: &str, name: &str) -> Result<bool, CollapseError> {
    Ok(load_deploy_secret(target, name)?.is_some())
}

/// Removes a deploy secret from Windows Credential Manager if present.
#[cfg(windows)]
pub fn forget_deploy_secret(target: &str, name: &str) -> Result<(), CollapseError> {
    match deploy_entry(target, name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(CollapseError::Credential(error.to_string())),
    }
}

#[cfg(not(windows))]
pub fn save_deploy_secret(_target: &str, _name: &str, _value: &str) -> Result<(), CollapseError> {
    Err(CollapseError::Unsupported)
}

#[cfg(not(windows))]
pub fn load_deploy_secret(
    _target: &str,
    _name: &str,
) -> Result<Option<DeploySecretValue>, CollapseError> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn has_deploy_secret(_target: &str, _name: &str) -> Result<bool, CollapseError> {
    Ok(false)
}

#[cfg(not(windows))]
pub fn forget_deploy_secret(_target: &str, _name: &str) -> Result<(), CollapseError> {
    Ok(())
}

#[cfg(windows)]
fn deploy_entry(target: &str, name: &str) -> Result<keyring::Entry, CollapseError> {
    keyring::Entry::new_with_target(target, DEPLOY_SECRET_SERVICE, name)
        .map_err(|error| CollapseError::Credential(error.to_string()))
}

/// Decodes either native Windows Credential Manager UTF-16LE bytes (written by
/// `set_password`, `cmdkey`, or the Credential Manager UI) or raw UTF-8 bytes.
#[cfg(windows)]
fn decode_credential_bytes(bytes: &[u8], name: &str) -> Result<String, CollapseError> {
    if bytes.is_empty() {
        return Ok(String::new());
    }
    let looks_utf16le = bytes.len().is_multiple_of(2) && bytes.contains(&0);
    if looks_utf16le && let Some(text) = decode_utf16le(bytes) {
        return Ok(text);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_owned());
    }
    decode_utf16le(bytes).ok_or_else(|| {
        CollapseError::Credential(format!(
            "stored deploy secret {name} is neither UTF-8 nor UTF-16LE; replace it in Settings"
        ))
    })
}

#[cfg(windows)]
fn decode_utf16le(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    String::from_utf16(&units).ok()
}
