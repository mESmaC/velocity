//! Pure domain types and helpers for Rosen `[deploy]` steps and secrets (#594).
//!
//! Secret values are read only from Windows Credential Manager under
//! `velocity/deploy/<project>/<NAME>` right before spawning the deploy step,
//! never stored in `state.json`, never passed to agents, and redacted from all
//! captured command output before reporting or starting a fix-forward thread.

use serde::{Deserialize, Serialize};

/// Prefix for deploy secret targets in Windows Credential Manager (#594).
pub const DEPLOY_SECRET_PREFIX: &str = "velocity/deploy";

/// Replacement inserted whenever a secret value appears in deploy output.
pub const REDACTED: &str = "[REDACTED]";

/// Marker included in a `skipped` deploy result when required secrets are missing.
pub const MISSING_SECRETS_PREFIX: &str = "Missing deploy secret(s) in Windows Credential Manager:";

/// Validates a deploy secret name (letters, digits, `_` or `-`, never `=`).
pub fn validate_secret_name(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter a secret name.".into());
    }
    if trimmed.contains('=') {
        return Err("Enter secret names only, never values (remove '=').".into());
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("Secret names may contain only ASCII letters, digits, '_' or '-'.".into());
    }
    Ok(trimmed.to_owned())
}

/// Normalizes a project name for the `<project>` segment of
/// `velocity/deploy/<project>/<NAME>`.
pub fn project_secret_segment(project_name: &str, project_id: u32) -> String {
    let mut segment = String::with_capacity(project_name.len());
    for character in project_name.trim().chars() {
        if matches!(character, '/' | '\\' | ':') || character.is_control() {
            segment.push('-');
        } else {
            segment.push(character);
        }
    }
    let cleaned = segment.trim_matches('-').trim();
    if cleaned.is_empty() {
        format!("project-{project_id}")
    } else {
        cleaned.to_owned()
    }
}

/// Builds the Windows Credential Manager target name `velocity/deploy/<project>/<NAME>`.
pub fn secret_target(
    project_name: &str,
    project_id: u32,
    secret_name: &str,
) -> Result<String, String> {
    let valid = validate_secret_name(secret_name)?;
    let segment = project_secret_segment(project_name, project_id);
    Ok(format!("{DEPLOY_SECRET_PREFIX}/{segment}/{valid}"))
}

/// Redacts every non-empty secret value from `text` before logging, posting to
/// Rosen, or handing output to a fix-forward thread.
///
/// Sorts patterns longest-first so a shorter secret cannot leave a suffix of a
/// longer secret intact. Also redacts trimmed values and multi-line fragments
/// of length >= 4 so line-wrapped or newline-trimmed secrets cannot leak.
pub fn redact_secrets(text: &str, secrets: &[&str]) -> String {
    if text.is_empty() || secrets.is_empty() {
        return text.to_owned();
    }
    let mut needles: Vec<&str> = Vec::new();
    for &secret in secrets {
        if secret.is_empty() {
            continue;
        }
        needles.push(secret);
        let trimmed = secret.trim();
        if trimmed.len() >= 4 && trimmed != secret {
            needles.push(trimmed);
        }
        if trimmed.contains('\n') {
            for line in trimmed.lines() {
                let piece = line.trim();
                if piece.len() >= 4 {
                    needles.push(piece);
                }
            }
        }
    }
    if needles.is_empty() {
        return text.to_owned();
    }
    needles.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    needles.dedup();
    let mut redacted = text.to_owned();
    for needle in needles {
        if redacted.contains(needle) {
            redacted = redacted.replace(needle, REDACTED);
        }
    }
    redacted
}

/// Convenience wrapper for redacting `(name, value)` secret pairs.
pub fn redact_secret_pairs(text: &str, secrets: &[(String, String)]) -> String {
    let values: Vec<&str> = secrets.iter().map(|(_, value)| value.as_str()).collect();
    redact_secrets(text, &values)
}

/// Formats a concise one-line summary from redacted deploy output for health rows.
pub fn summarize_output(output: &str, max_chars: usize) -> Option<String> {
    let line = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let mut summary: String = line.chars().take(max_chars).collect();
    if line.chars().count() > max_chars {
        summary.push('…');
    }
    Some(summary)
}

/// Formats the skip message recorded when required secrets are missing from
/// Windows Credential Manager (never includes any values).
pub fn missing_secrets_output(missing: &[(String, String)]) -> String {
    let listed: Vec<String> = missing
        .iter()
        .map(|(name, target)| format!("{name} ({target})"))
        .collect();
    format!(
        "{MISSING_SECRETS_PREFIX} {}. Set them in Settings -> Workspaces -> Deploy secrets.",
        listed.join(", ")
    )
}

/// Lifecycle state of a Rosen `[deploy]` run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum DeployRunState {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
}

/// Read-only description of one `[deploy.<name>]` step from `rosen.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct DeployStepSummary {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
}

/// Presence of one project deploy secret in Windows Credential Manager.
/// Never carries the secret value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct DeploySecretStatus {
    pub name: String,
    pub target: String,
    pub stored: bool,
    /// Names of `[deploy.<step>]` entries that request this secret.
    #[serde(default)]
    pub used_by: Vec<String>,
}

/// Project settings view for `[deploy]` steps and Windows Credential Manager secrets (#594).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct ProjectDeploySettings {
    pub project_identifier: u32,
    pub project_name: String,
    pub supported: bool,
    #[serde(default)]
    pub steps: Vec<DeployStepSummary>,
    #[serde(default)]
    pub secrets: Vec<DeploySecretStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
}
