//! Filtering owner credentials out of agent environments (#530).
//!
//! Agents run unrestricted (SPEC.md Rule 1) and providers authenticate through OAuth (F15),
//! never through inherited environment tokens. This module defines the pure deny-list and
//! pass-through rules shared by ACP launches, ACP terminals, queued runs, preflight checks,
//! and agent shims.

use std::collections::BTreeSet;

/// Prefix for the withheld-variable diagnostic line written to a thread's debug log.
pub const WITHHELD_LOG_PREFIX: &str = "Withheld environment variables (names only): ";

const DENIED_SUFFIXES: &[&str] = &[
    "_KEY",
    "_KEY_PATH",
    "_TOKEN",
    "_SECRET",
    "_PASSWORD",
    "_CREDENTIALS",
];

const DENIED_PREFIXES: &[&str] = &["VELOCITY_"];

const DENIED_EXACT: &[&str] = &["GH_TOKEN", "GITHUB_TOKEN", "AGY_ADC_AUTH"];

/// Whether an inherited environment variable name matches the credential deny-list.
/// Matching is ASCII case-insensitive so Windows mixed-case names cannot bypass it.
pub fn is_denied_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    DENIED_EXACT.contains(&upper.as_str())
        || DENIED_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || DENIED_SUFFIXES.iter().any(|suffix| upper.ends_with(suffix))
}

/// Whether `name` is explicitly allowed by the owner's Settings → Providers pass-through list.
pub fn is_passthrough(name: &str, passthrough: &[String]) -> bool {
    passthrough
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(name))
}

/// Whether an inherited variable should be stripped before spawning an agent or tool process.
pub fn should_withhold(name: &str, passthrough: &[String]) -> bool {
    is_denied_name(name) && !is_passthrough(name, passthrough)
}

/// Validates one variable name entered in Settings → Providers.
/// Rejects `=` so an owner can never accidentally paste or store a secret value (F15).
pub fn validate_passthrough_name(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter an environment variable name.".into());
    }
    if trimmed.contains('=') {
        return Err("Enter variable names only, never values (remove '=').".into());
    }
    if trimmed
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err("Variable names cannot contain spaces or control characters.".into());
    }
    Ok(trimmed.to_owned())
}

/// Validates, trims and case-insensitively deduplicates a list of pass-through variable names.
pub fn normalize_passthrough(names: &[String]) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in names {
        if raw.trim().is_empty() {
            continue;
        }
        let valid = validate_passthrough_name(raw)?;
        if seen.insert(valid.to_ascii_uppercase()) {
            normalized.push(valid);
        }
    }
    Ok(normalized)
}

/// Formats withheld variable names (never values) for a thread's debug log.
pub fn format_withheld_log(names: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    Some(format!("{WITHHELD_LOG_PREFIX}{}", names.join(", ")))
}
