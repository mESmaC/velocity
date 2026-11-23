//! Pure domain types and helpers for Rosen `[deploy]` steps and secrets (#594, #604).
//!
//! Secret values are read from Windows Credential Manager under
//! `velocity/deploy/<project>/<NAME>` right before spawning the deploy step,
//! falling back to known local CLI logins (`flyctl` for `FLY_API_TOKEN` and
//! `wrangler` for `CLOUDFLARE_API_TOKEN`) when absent. Values are never stored
//! in `state.json`, never passed to agents, and redacted from all captured
//! command output before reporting or starting a fix-forward thread.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::base_ci::{JobPlatformOutcome, JobState, RunState, job_label};
use crate::project_health::short_commit;

/// Prefix for deploy secret targets in Windows Credential Manager (#594).
pub const DEPLOY_SECRET_PREFIX: &str = "velocity/deploy";

/// Replacement inserted whenever a secret value appears in deploy output.
pub const REDACTED: &str = "[REDACTED]";

/// Marker included in a `skipped` deploy result when required secrets are missing.
pub const MISSING_SECRETS_PREFIX: &str = "Missing deploy secret(s) in Windows Credential Manager:";

/// Default deploy target platform for hosted Fly and Cloudflare targets (#685).
pub const DEFAULT_HOSTED_DEPLOY_PLATFORM: &str = "linux";

/// Marker included in a `skipped` deploy result when a commit is skipped by platform gating (#685).
pub const PLATFORM_GATE_SKIP_PREFIX: &str = "[Velocity] Deploy skipped by platform gate:";

/// Well-known deploy secret name satisfied by a local `flyctl` login (#604).
pub const FLY_API_TOKEN: &str = "FLY_API_TOKEN";

/// Well-known deploy secret name satisfied by a local `wrangler` login (#604).
pub const CLOUDFLARE_API_TOKEN: &str = "CLOUDFLARE_API_TOKEN";

/// Prefix for the secret-source summary line prepended to deploy results (#604).
pub const DEPLOY_SECRETS_HEADER_PREFIX: &str = "[Velocity] Deploy secrets:";

/// Prefix for the scope-error guidance appended when a CLI login lacks scopes (#604).
pub const CLI_SCOPE_HINT_PREFIX: &str = "[Velocity] CLI login may lack a required scope";

/// Settings navigation path shown in missing-secret and scope-error guidance (#594, #604).
pub const DEPLOY_SECRETS_SETTINGS_PATH: &str = "Settings -> Workspaces -> Deploy secrets";

/// Refresh a wrangler OAuth token slightly before expiry so a deploy does not race it (#604).
pub const WRANGLER_EXPIRY_MARGIN_SECONDS: u64 = 60;

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
    Some(truncate_chars(line, max_chars))
}

/// Formats a concise error line from redacted deploy output while skipping Velocity's
/// own `[Velocity]` metadata headers and footers (#604).
pub fn summarize_deploy_error(output: &str, max_chars: usize) -> Option<String> {
    let line = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("[Velocity]"));
    match line {
        Some(error_line) => Some(truncate_chars(error_line, max_chars)),
        None => summarize_output(output, max_chars),
    }
}

/// Formats the last `max_lines` non-empty redacted output lines for `[deploy]` failure logs,
/// skipping Velocity's own `[Velocity]` metadata lines when command output is present (#606).
pub fn format_deploy_failure_tail(
    output: &str,
    max_lines: usize,
    max_chars: usize,
) -> Option<String> {
    if max_lines == 0 || max_chars == 0 {
        return None;
    }
    let mut lines: Vec<&str> = output
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("[Velocity]"))
        .take(max_lines)
        .collect();
    if lines.is_empty() {
        lines = output
            .lines()
            .rev()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(max_lines)
            .collect();
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(tail_chars(&lines.join(" | "), max_chars))
}

/// Formats elapsed deploy execution time concisely for `[deploy]` logs and activity (#606).
pub fn format_deploy_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 10.0 {
        format!("{seconds:.1}s")
    } else if seconds < 60.0 {
        format!("{}s", seconds.round() as u64)
    } else {
        let total = duration.as_secs();
        format!("{}m {}s", total / 60, total % 60)
    }
}

/// Returns the isolated per-step checkout folder name `deploy-checkout-<step>` (#606).
pub fn deploy_checkout_folder_name(step: &str) -> String {
    let cleaned: String = step
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    let slug: String = if trimmed.is_empty() {
        "step".to_owned()
    } else {
        trimmed.chars().take(48).collect()
    };
    format!("deploy-checkout-{slug}")
}

/// Declaration position of `step` in `rosen.toml`'s `[deploy]` list, or `usize::MAX` when absent (#606).
pub fn deploy_step_position(step_order: &[String], step: &str) -> usize {
    step_order
        .iter()
        .position(|name| name == step)
        .unwrap_or(usize::MAX)
}

/// Formats the skip message recorded when required secrets are missing from
/// Windows Credential Manager and any known CLI login (never includes any values).
pub fn missing_secrets_output(missing: &[(String, String)]) -> String {
    let detailed: Vec<(String, String, Option<String>)> = missing
        .iter()
        .map(|(name, target)| (name.clone(), target.clone(), None))
        .collect();
    missing_secrets_output_with_notes(&detailed)
}

/// Formats the skip message for missing secrets, optionally noting why a known
/// CLI login fallback was unavailable (never includes any secret values) (#604).
pub fn missing_secrets_output_with_notes(missing: &[(String, String, Option<String>)]) -> String {
    let listed: Vec<String> = missing
        .iter()
        .map(|(name, target, note)| match note.as_deref() {
            Some(reason) if !reason.trim().is_empty() => {
                format!("{name} ({target}; {})", reason.trim())
            }
            _ => format!("{name} ({target})"),
        })
        .collect();
    format!(
        "{MISSING_SECRETS_PREFIX} {}. Set them in {DEPLOY_SECRETS_SETTINGS_PATH}.",
        listed.join(", ")
    )
}

/// Where a deploy secret value was resolved from (#594, #604).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum DeploySecretSource {
    CredentialManager,
    FlyctlLogin,
    WranglerLogin,
}

impl DeploySecretSource {
    /// Human-readable source label included in deploy results and Settings (#604).
    pub const fn label(self) -> &'static str {
        match self {
            Self::CredentialManager => "from Windows Credential Manager",
            Self::FlyctlLogin => "from flyctl login",
            Self::WranglerLogin => "from wrangler login",
        }
    }

    /// True when the secret came from a local CLI login fallback instead of Credential Manager.
    pub const fn is_cli_fallback(self) -> bool {
        matches!(self, Self::FlyctlLogin | Self::WranglerLogin)
    }
}

/// Formats resolved secret sources without values, for example `"FLY_API_TOKEN from flyctl login"`.
pub fn format_secret_sources(sources: &[(String, DeploySecretSource)]) -> Option<String> {
    if sources.is_empty() {
        return None;
    }
    let items: Vec<String> = sources
        .iter()
        .map(|(name, source)| format!("{name} {}", source.label()))
        .collect();
    Some(items.join(", "))
}

/// Builds the redacted deploy result body submitted to Rosen while keeping the secret-source
/// header and optional scope hint within `max_bytes` (#604).
pub fn format_deploy_result_output(
    sources: &[(String, DeploySecretSource)],
    redacted_output: &str,
    scope_hint: Option<&str>,
    max_bytes: usize,
) -> String {
    let header = format_secret_sources(sources)
        .map(|summary| format!("{DEPLOY_SECRETS_HEADER_PREFIX} {summary}"));
    let footer = scope_hint
        .map(str::trim)
        .filter(|hint| !hint.is_empty())
        .map(str::to_owned);
    let mut reserved = 0usize;
    if let Some(line) = &header {
        reserved = reserved.saturating_add(line.len() + 1);
    }
    if let Some(line) = &footer {
        reserved = reserved.saturating_add(line.len() + 1);
    }
    let body_budget = max_bytes.saturating_sub(reserved);
    let trimmed_body = tail_utf8_bytes(redacted_output.trim_end(), body_budget);
    let mut sections = Vec::with_capacity(3);
    if let Some(line) = header {
        sections.push(line);
    }
    if !trimmed_body.is_empty() {
        sections.push(trimmed_body);
    }
    if let Some(line) = footer {
        sections.push(line);
    }
    sections.join("\n")
}

/// Extracts the secret-source summary from a stored `deploy_result` output (#604).
pub fn extract_secret_sources_line(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix(DEPLOY_SECRETS_HEADER_PREFIX)
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
    })
}

/// Extracts the CLI scope-hint line from a stored failed `deploy_result` output (#604).
pub fn extract_cli_scope_hint(output: &str) -> Option<&str> {
    output.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .starts_with(CLI_SCOPE_HINT_PREFIX)
            .then_some(trimmed)
    })
}

/// Parsed scope/authorization failure when a step ran with a CLI login fallback (#604).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliScopeFailure {
    /// Concise API error line extracted from redacted command output.
    pub api_error: String,
    /// Names of secrets that came from a CLI login fallback.
    pub fallback_names: Vec<String>,
    /// Short action suggesting the owner store a scoped token in Settings.
    pub suggestion: String,
    /// Full `[Velocity]` footer line appended to the deploy result output.
    pub output_note: String,
}

/// Detects when a deploy step using a CLI login fallback failed due to an API
/// authorization or missing-scope error (#604).
pub fn detect_cli_scope_error(
    redacted_output: &str,
    sources: &[(String, DeploySecretSource)],
) -> Option<CliScopeFailure> {
    let fallbacks: Vec<(String, DeploySecretSource)> = sources
        .iter()
        .filter(|(_, source)| source.is_cli_fallback())
        .cloned()
        .collect();
    if fallbacks.is_empty() {
        return None;
    }
    let matched_line = redacted_output.lines().rev().map(str::trim).find(|line| {
        !line.is_empty() && !line.starts_with("[Velocity]") && looks_like_scope_or_auth_error(line)
    })?;
    let api_error = truncate_chars(matched_line, 180);
    let fallback_names: Vec<String> = fallbacks.iter().map(|(name, _)| name.clone()).collect();
    let secret_list = fallback_names.join(", ");
    let source_summary = format_secret_sources(&fallbacks).unwrap_or_default();
    let suggestion = format!("Set {secret_list} in {DEPLOY_SECRETS_SETTINGS_PATH}");
    let output_note = format!("{CLI_SCOPE_HINT_PREFIX} ({source_summary}). {suggestion}.");
    Some(CliScopeFailure {
        api_error,
        fallback_names,
        suggestion,
        output_note,
    })
}

fn looks_like_scope_or_auth_error(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        "http 401",
        "http 403",
        "status 401",
        "status 403",
        "status code 401",
        "status code 403",
        "status: 401",
        "status: 403",
        "401 unauthorized",
        "403 forbidden",
        "unauthorized",
        "forbidden",
        "insufficient scope",
        "missing scope",
        "lacks scope",
        "required scope",
        "permission denied",
        "insufficient permissions",
        "missing permissions",
        "lacks permission",
        "not authorized",
        "access denied",
        "authentication error",
        "invalid api token",
        "invalid access token",
        "could not authenticate",
        "error code: 10000",
        "code: 10000",
        "code 10000",
        "code: 9109",
        "code 9109",
    ];
    PATTERNS.iter().any(|pattern| lower.contains(pattern))
}

/// Parsed fields from wrangler's `default.toml` OAuth login file (#604).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WranglerConfig {
    pub oauth_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expiration_time_unix: Option<u64>,
}

// OAuth tokens never appear in `{:?}` logs.
impl std::fmt::Debug for WranglerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WranglerConfig")
            .field("oauth_token", &self.oauth_token.as_ref().map(|_| REDACTED))
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| REDACTED),
            )
            .field("expiration_time_unix", &self.expiration_time_unix)
            .finish()
    }
}

/// Parses `oauth_token`, `refresh_token`, and `expiration_time` from wrangler's `default.toml` (#604).
pub fn parse_wrangler_config(toml_text: &str) -> WranglerConfig {
    let mut config = WranglerConfig::default();
    let mut in_root = true;
    for raw_line in toml_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_root = false;
            continue;
        }
        if !in_root {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let Some(parsed) = parse_toml_scalar(value.trim()) else {
            continue;
        };
        let trimmed = parsed.trim();
        if trimmed.is_empty() {
            continue;
        }
        match key {
            "oauth_token" => config.oauth_token = Some(trimmed.to_owned()),
            "refresh_token" => config.refresh_token = Some(trimmed.to_owned()),
            "expiration_time" => config.expiration_time_unix = parse_rfc3339_seconds(trimmed),
            _ => {}
        }
    }
    config
}

/// Returns true when wrangler's OAuth token has expired or is within the refresh margin (#604).
pub fn is_wrangler_token_expired(expiration_unix: Option<u64>, now_unix: u64) -> bool {
    expiration_unix
        .is_none_or(|expiry| expiry <= now_unix.saturating_add(WRANGLER_EXPIRY_MARGIN_SECONDS))
}

/// Parses an RFC 3339 UTC or offset timestamp into Unix seconds (#604).
pub fn parse_rfc3339_seconds(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    let (date, time_and_zone) = trimmed.split_once(['T', 't'])?;
    let mut date_parts = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (
        date_parts.next()??,
        date_parts.next()??,
        date_parts.next()??,
    );
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (time_part, offset_seconds) = split_rfc3339_offset(time_and_zone)?;
    let clock = time_part.split('.').next()?;
    let mut clock_parts = clock.split(':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (
        clock_parts.next()??,
        clock_parts.next()??,
        clock_parts.next()??,
    );
    if clock_parts.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let year_of_era = adjusted_year.rem_euclid(400);
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = adjusted_year.div_euclid(400) * 146_097 + day_of_era - 719_468;
    let utc_seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_seconds;
    u64::try_from(utc_seconds).ok()
}

fn split_rfc3339_offset(time_and_zone: &str) -> Option<(&str, i64)> {
    if let Some(clock) = time_and_zone
        .strip_suffix('Z')
        .or_else(|| time_and_zone.strip_suffix('z'))
    {
        return Some((clock, 0));
    }
    let sign_index = time_and_zone.rfind(['+', '-'])?;
    let (clock, zone) = time_and_zone.split_at(sign_index);
    let sign = if zone.starts_with('+') { 1 } else { -1 };
    let (hours, minutes) = zone[1..].split_once(':')?;
    let hours = hours.parse::<i64>().ok()?;
    let minutes = minutes.parse::<i64>().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some((clock, sign * (hours * 3_600 + minutes * 60)))
}

fn parse_toml_scalar(raw: &str) -> Option<String> {
    if let Some(rest) = raw.strip_prefix('"') {
        return parse_toml_basic_string(rest);
    }
    if let Some(rest) = raw.strip_prefix('\'') {
        let (content, _) = rest.split_once('\'')?;
        return Some(content.to_owned());
    }
    let unquoted = raw.split('#').next().unwrap_or(raw).trim();
    (!unquoted.is_empty()).then(|| unquoted.to_owned())
}

fn parse_toml_basic_string(rest: &str) -> Option<String> {
    let mut output = String::with_capacity(rest.len());
    let mut chars = rest.chars();
    while let Some(character) = chars.next() {
        match character {
            '"' => return Some(output),
            '\\' => match chars.next()? {
                '"' => output.push('"'),
                '\\' => output.push('\\'),
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                other => output.push(other),
            },
            other => output.push(other),
        }
    }
    None
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut truncated: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        truncated.push('…');
    }
    truncated
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let skip = count.saturating_sub(max_chars);
    let kept: String = text.chars().skip(skip).collect();
    format!("…{kept}")
}

fn tail_utf8_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let start = text.ceil_char_boundary(text.len().saturating_sub(max_bytes));
    text[start..].to_owned()
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

/// Presence of one project deploy secret in Windows Credential Manager or a CLI login.
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
    /// Active CLI login fallback when `stored` is false (#604), for example `"from flyctl login"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
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

/// Normalizes a deploy target platform string (`linux`, `windows`, `macos`, optional `-<arch>`).
/// Returns `None` for empty strings or `"any"` / `"none"` (#685).
pub fn normalize_deploy_platform(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
        || trimmed.eq_ignore_ascii_case("any")
        || trimmed.eq_ignore_ascii_case("none")
    {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let (raw_os, raw_arch) = match lower.split_once('-') {
        Some((os, arch)) => (os, Some(arch)),
        None => (lower.as_str(), None),
    };
    let Some(os) = normalize_os_name(raw_os) else {
        return Some(lower);
    };
    match raw_arch.and_then(normalize_arch_name) {
        Some(arch) => Some(format!("{os}-{arch}")),
        None => Some(os.to_owned()),
    }
}

fn normalize_os_name(raw: &str) -> Option<&'static str> {
    match raw {
        "linux" | "ubuntu" | "debian" | "alpine" => Some("linux"),
        "windows" | "win" | "win32" | "win64" => Some("windows"),
        "macos" | "darwin" | "osx" => Some("macos"),
        _ => None,
    }
}

fn normalize_arch_name(raw: &str) -> Option<&'static str> {
    match raw {
        "x86_64" | "amd64" | "x64" | "intel" => Some("x86_64"),
        "arm64" | "aarch64" | "arm" => Some("arm64"),
        _ => None,
    }
}

fn split_platform(normalized: &str) -> (&str, Option<&str>) {
    match normalized.split_once('-') {
        Some((os, arch)) => (os, Some(arch)),
        None => (normalized, None),
    }
}

/// Whether `candidate` (a checker or CI runner platform) satisfies `target` (#685).
pub fn platform_matches_target(candidate: &str, target: &str) -> bool {
    let (Some(candidate), Some(target)) = (
        normalize_deploy_platform(candidate),
        normalize_deploy_platform(target),
    ) else {
        return false;
    };
    let (cand_os, cand_arch) = split_platform(&candidate);
    let (target_os, target_arch) = split_platform(&target);
    cand_os == target_os && (target_arch.is_none() || cand_arch == target_arch)
}

/// Returns true when a `[deploy.<step>]` targets Fly or Cloudflare (#685).
pub fn is_fly_or_cloudflare_target(command: &str, paths: &[String], secrets: &[String]) -> bool {
    if secrets.iter().any(|secret| {
        secret.eq_ignore_ascii_case(FLY_API_TOKEN)
            || secret.eq_ignore_ascii_case(CLOUDFLARE_API_TOKEN)
    }) {
        return true;
    }
    if command
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '\\' | '\'' | '"' | '=' | ':'))
        .any(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "fly"
                    | "flyctl"
                    | "fly.py"
                    | "fly.toml"
                    | "wrangler"
                    | "cloudflare"
                    | "cloudflare.py"
            )
        })
    {
        return true;
    }
    paths.iter().any(|path| {
        path.split(['/', '\\']).any(|segment| {
            matches!(
                segment.to_ascii_lowercase().as_str(),
                "fly.toml"
                    | "fly.py"
                    | "wrangler.toml"
                    | "wrangler.json"
                    | "wrangler.jsonc"
                    | "cloudflare.py"
            )
        })
    })
}

/// Resolves the deploy target platform that must be green before executing a step (#685).
///
/// Uses `[deploy.<step>].platform` when configured (allowing `"any"`/`"none"` to disable
/// gating), and otherwise defaults to `"linux"` for Fly and Cloudflare targets.
pub fn resolve_deploy_target_platform(
    configured_platform: Option<&str>,
    command: &str,
    paths: &[String],
    secrets: &[String],
) -> Option<String> {
    if let Some(raw) = configured_platform.map(str::trim).filter(|s| !s.is_empty()) {
        return normalize_deploy_platform(raw);
    }
    is_fly_or_cloudflare_target(command, paths, secrets)
        .then(|| DEFAULT_HOSTED_DEPLOY_PLATFORM.to_owned())
}

/// Infers a platform (`linux`, `windows`, `macos`, optional `-<arch>`) from GitHub Actions
/// runner labels, falling back to the job/status display name when labels are absent (#685).
pub fn infer_ci_job_platform(job_name: &str, labels: &[String]) -> Option<String> {
    infer_platform_from_tokens(labels.iter().map(String::as_str))
        .or_else(|| infer_platform_from_text(job_name))
}

fn infer_platform_from_tokens<'a>(tokens: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut os: Option<&'static str> = None;
    let mut arch: Option<&'static str> = None;
    for raw in tokens {
        let lower = raw.trim().to_ascii_lowercase();
        if lower.is_empty() {
            continue;
        }
        if os.is_none() {
            os = os_from_label(&lower);
        }
        if arch.is_none() {
            arch = arch_from_label(&lower);
        }
    }
    let os = os?;
    Some(match arch {
        Some(arch) => format!("{os}-{arch}"),
        None => os.to_owned(),
    })
}

fn infer_platform_from_text(text: &str) -> Option<String> {
    let tokens = text.split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.'));
    infer_platform_from_tokens(tokens)
}

fn os_from_label(lower: &str) -> Option<&'static str> {
    if lower == "linux"
        || lower == "ubuntu"
        || lower.starts_with("ubuntu-")
        || lower.starts_with("linux-")
        || lower.starts_with("debian")
        || lower.starts_with("alpine")
    {
        return Some("linux");
    }
    if lower == "windows" || lower == "win32" || lower == "win64" || lower.starts_with("windows-") {
        return Some("windows");
    }
    if lower == "macos" || lower == "darwin" || lower == "osx" || lower.starts_with("macos-") {
        return Some("macos");
    }
    None
}

fn arch_from_label(lower: &str) -> Option<&'static str> {
    if lower == "arm64"
        || lower == "aarch64"
        || lower.ends_with("-arm64")
        || lower.ends_with("-aarch64")
        || lower.ends_with("-arm")
    {
        return Some("arm64");
    }
    if lower == "x86_64"
        || lower == "x64"
        || lower == "amd64"
        || lower.ends_with("-x86_64")
        || lower.ends_with("-x64")
        || lower.ends_with("-amd64")
        || lower.ends_with("-intel")
    {
        return Some("x86_64");
    }
    None
}

/// Returns true when a GitHub CI job or status covers `target_platform` (#685).
pub fn ci_job_matches_target_platform(
    job_name: &str,
    labels: &[String],
    target_platform: &str,
) -> bool {
    infer_ci_job_platform(job_name, labels)
        .is_some_and(|inferred| platform_matches_target(&inferred, target_platform))
}

/// One GitHub Actions run, commit status, or standalone check run with per-job runner labels
/// for deploy platform gating (#685).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubRunPlatformSnapshot {
    pub workflow_key: String,
    pub workflow: String,
    pub run: u64,
    pub commit: String,
    pub state: RunState,
    pub jobs: Vec<JobPlatformOutcome>,
}

/// Verification status of one commit on a deploy step's target platform (#685).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlatformCommitStatus {
    /// Confirmed green on the target platform.
    Green { source: String },
    /// Failed on the target platform; never deploy this commit.
    Failed { reasons: Vec<String> },
    /// Still queued or running on the target platform; hold the deploy.
    Pending { jobs: Vec<String> },
    /// Neither a post-land checker nor a base CI job has reported a result for this commit.
    NoResult,
}

/// Evaluates GitHub base CI runs on `commit` for `target_platform` (#685).
///
/// Every matching job on `commit` is decided by the newest run of its workflow in which it
/// has a non-`Undecided` outcome. Any failing matching job yields `Failed`; any running
/// matching job (or running workflow whose jobs have not appeared yet) yields `Pending`;
/// at least one passing matching job with no failures or pending jobs yields `Green`.
pub fn evaluate_github_platform_status(
    runs: &[GitHubRunPlatformSnapshot],
    commit: &str,
    target_platform: &str,
) -> PlatformCommitStatus {
    let mut by_workflow: BTreeMap<&str, Vec<&GitHubRunPlatformSnapshot>> = BTreeMap::new();
    let mut failed = Vec::new();
    let mut pending = Vec::new();
    let mut passed = Vec::new();
    for run in runs
        .iter()
        .filter(|run| run.commit.trim().eq_ignore_ascii_case(commit.trim()))
    {
        if run.run == 0 {
            record_status_platform_outcome(
                run,
                target_platform,
                &mut failed,
                &mut pending,
                &mut passed,
            );
        } else {
            let key = if run.workflow_key.trim().is_empty() {
                run.workflow.as_str()
            } else {
                run.workflow_key.as_str()
            };
            by_workflow.entry(key).or_default().push(run);
        }
    }
    for mut workflow_runs in by_workflow.into_values() {
        workflow_runs.sort_by_key(|run| std::cmp::Reverse(run.run));
        evaluate_workflow_platform_runs(
            &workflow_runs,
            target_platform,
            &mut failed,
            &mut pending,
            &mut passed,
        );
    }
    finalize_platform_status(failed, pending, passed)
}

fn record_status_platform_outcome(
    run: &GitHubRunPlatformSnapshot,
    target_platform: &str,
    failed: &mut Vec<String>,
    pending: &mut Vec<String>,
    passed: &mut Vec<String>,
) {
    if !ci_job_matches_target_platform(&run.workflow, &[], target_platform) {
        return;
    }
    match run.state {
        RunState::Failed => failed.push(run.workflow.clone()),
        RunState::Running => pending.push(run.workflow.clone()),
        RunState::Passed => passed.push(run.workflow.clone()),
        RunState::Undecided => {}
    }
}

fn evaluate_workflow_platform_runs(
    newest_first: &[&GitHubRunPlatformSnapshot],
    target_platform: &str,
    failed: &mut Vec<String>,
    pending: &mut Vec<String>,
    passed: &mut Vec<String>,
) {
    let Some(newest) = newest_first.first().copied() else {
        return;
    };
    let mut matching_names: Vec<String> = Vec::new();
    for run in newest_first {
        for job in &run.jobs {
            if ci_job_matches_target_platform(&job.job, &job.labels, target_platform)
                && !matching_names.iter().any(|existing| existing == &job.job)
            {
                matching_names.push(job.job.clone());
            }
        }
    }
    if matching_names.is_empty() {
        if newest.state == RunState::Running && newest.jobs.is_empty() {
            pending.push(newest.workflow.clone());
        }
        return;
    }
    for job_name in matching_names {
        let state = newest_job_state_on_commit(newest_first, &job_name);
        let label = job_label(&newest.workflow, &job_name);
        match state {
            Some(JobState::Failed) => failed.push(label),
            Some(JobState::Running) => pending.push(label),
            Some(JobState::Passed) => passed.push(label),
            Some(JobState::Undecided) | None => {}
        }
    }
}

fn newest_job_state_on_commit(
    newest_first: &[&GitHubRunPlatformSnapshot],
    job_name: &str,
) -> Option<JobState> {
    for run in newest_first {
        let mut saw_passed = false;
        let mut saw_running = false;
        for job in run.jobs.iter().filter(|entry| entry.job == job_name) {
            match job.state {
                JobState::Failed => return Some(JobState::Failed),
                JobState::Running => saw_running = true,
                JobState::Passed => saw_passed = true,
                JobState::Undecided => {}
            }
        }
        if saw_running {
            return Some(JobState::Running);
        }
        if saw_passed {
            return Some(JobState::Passed);
        }
    }
    None
}

fn finalize_platform_status(
    failed: Vec<String>,
    pending: Vec<String>,
    passed: Vec<String>,
) -> PlatformCommitStatus {
    if !failed.is_empty() {
        return PlatformCommitStatus::Failed { reasons: failed };
    }
    if !pending.is_empty() {
        return PlatformCommitStatus::Pending { jobs: pending };
    }
    if !passed.is_empty() {
        return PlatformCommitStatus::Green {
            source: format!("base CI ({})", passed.join(", ")),
        };
    }
    PlatformCommitStatus::NoResult
}

/// Combines Rosen post-land checker status and GitHub base CI status for one commit and
/// target platform (#685).
///
/// Any failure or pending base CI job on the target platform holds the deploy; otherwise
/// either a passing post-land checker on that platform or passing base CI jobs count as green.
pub fn evaluate_platform_commit_status(
    rosen_status: PlatformCommitStatus,
    github_status: PlatformCommitStatus,
) -> PlatformCommitStatus {
    if matches!(github_status, PlatformCommitStatus::Failed { .. }) {
        return github_status;
    }
    if matches!(rosen_status, PlatformCommitStatus::Failed { .. }) {
        return rosen_status;
    }
    if matches!(github_status, PlatformCommitStatus::Pending { .. }) {
        return github_status;
    }
    if matches!(rosen_status, PlatformCommitStatus::Green { .. }) {
        return rosen_status;
    }
    if matches!(github_status, PlatformCommitStatus::Green { .. }) {
        return github_status;
    }
    if matches!(rosen_status, PlatformCommitStatus::Pending { .. }) {
        return rosen_status;
    }
    PlatformCommitStatus::NoResult
}

/// Decision for a due `[deploy.<step>]` run against its target platform gate (#685).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeployGateDecision {
    /// Target platform is green for this commit (and no newer pending commit is already green).
    Run,
    /// Hold this deploy run without executing it until the target platform resolves.
    Wait,
    /// Skip this commit in Rosen because it failed on the target platform and a newer
    /// pending run is waiting behind it.
    SkipRed { reason: String },
    /// Skip this commit in Rosen because a newer pending commit for the same step is
    /// already green (or already has CI running while this older commit has no result).
    SkipSuperseded {
        newer_commit: String,
        reason: String,
    },
}

/// Decides whether to run, wait, or skip a due deploy run (#685).
pub fn decide_deploy_gate(
    step: &str,
    commit: &str,
    target_platform: &str,
    current_status: &PlatformCommitStatus,
    newer_same_step: Option<(&str, &PlatformCommitStatus)>,
    has_other_pending: bool,
) -> DeployGateDecision {
    let current_short = short_commit(commit);
    if let Some((newer_commit, PlatformCommitStatus::Green { .. })) = newer_same_step {
        return DeployGateDecision::SkipSuperseded {
            newer_commit: newer_commit.to_owned(),
            reason: format!(
                "{PLATFORM_GATE_SKIP_PREFIX} `{step}` at {current_short} superseded by green {target_platform} commit {}",
                short_commit(newer_commit)
            ),
        };
    }
    match current_status {
        PlatformCommitStatus::Green { .. } => DeployGateDecision::Run,
        PlatformCommitStatus::Failed { reasons } => {
            if newer_same_step.is_some() || has_other_pending {
                DeployGateDecision::SkipRed {
                    reason: format!(
                        "{PLATFORM_GATE_SKIP_PREFIX} `{step}` at {current_short} failed on {target_platform} ({})",
                        reasons.join(", ")
                    ),
                }
            } else {
                DeployGateDecision::Wait
            }
        }
        PlatformCommitStatus::NoResult => {
            if let Some((newer_commit, newer_status)) = newer_same_step
                && matches!(
                    newer_status,
                    PlatformCommitStatus::Pending { .. } | PlatformCommitStatus::Failed { .. }
                )
            {
                return DeployGateDecision::SkipSuperseded {
                    newer_commit: newer_commit.to_owned(),
                    reason: format!(
                        "{PLATFORM_GATE_SKIP_PREFIX} `{step}` at {current_short} had no {target_platform} result; advancing to {}",
                        short_commit(newer_commit)
                    ),
                };
            }
            DeployGateDecision::Wait
        }
        PlatformCommitStatus::Pending { .. } => DeployGateDecision::Wait,
    }
}

/// Formats Project Health `(signal, action, detail)` for a deploy step waiting on its
/// target platform (#685).
pub fn format_deploy_waiting_health(
    step: &str,
    target_platform: &str,
    commit: &str,
    status: &PlatformCommitStatus,
) -> (String, String, Option<String>) {
    let short = short_commit(commit);
    match status {
        PlatformCommitStatus::NoResult => (
            format!("deploy {step} waiting: no {target_platform} result for {short}"),
            format!("Waiting for a {target_platform} post-land checker or passing base CI job"),
            None,
        ),
        PlatformCommitStatus::Pending { jobs } => (
            format!("deploy {step} waiting: no {target_platform} result for {short}"),
            format!("Waiting for {target_platform} base CI to finish"),
            (!jobs.is_empty()).then(|| format!("Pending: {}", jobs.join(", "))),
        ),
        PlatformCommitStatus::Failed { reasons } => (
            format!("deploy {step} held: {target_platform} failed for {short}"),
            format!("Holding deploy until a newer commit is green on {target_platform}"),
            (!reasons.is_empty()).then(|| reasons.join(", ")),
        ),
        PlatformCommitStatus::Green { source } => (
            format!("Deploy `{step}` ready on {target_platform} at {short}"),
            format!("Green via {source}"),
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ci_run(
        workflow: &str,
        run: u64,
        commit: &str,
        state: RunState,
        jobs: Vec<(&str, JobState, &[&str])>,
    ) -> GitHubRunPlatformSnapshot {
        GitHubRunPlatformSnapshot {
            workflow_key: format!("workflow:{workflow}"),
            workflow: workflow.to_owned(),
            run,
            commit: commit.to_owned(),
            state,
            jobs: jobs
                .into_iter()
                .map(|(job, state, labels)| JobPlatformOutcome {
                    job: job.to_owned(),
                    state,
                    labels: labels.iter().map(|label| (*label).to_owned()).collect(),
                })
                .collect(),
        }
    }

    // Risk (#685): Fly and Cloudflare deploy steps without an explicit `platform` must default
    // to `linux`, while an explicit `platform` overrides the default and non-hosted steps stay
    // unconstrained unless configured.
    #[test]
    fn resolve_deploy_target_platform_defaults_hosted_targets_to_linux() {
        assert_eq!(
            resolve_deploy_target_platform(
                None,
                "python -B ops/release.py",
                &["server/**".into(), "ops/fly.toml".into()],
                &["FLY_API_TOKEN".into()],
            ),
            Some("linux".to_owned())
        );
        assert_eq!(
            resolve_deploy_target_platform(
                None,
                "python -B ops/edge.py",
                &["ops/cloudflare.py".into()],
                &["CLOUDFLARE_API_TOKEN".into()],
            ),
            Some("linux".to_owned())
        );
        assert_eq!(
            resolve_deploy_target_platform(
                Some("windows"),
                "python -B ops/release.py",
                &["ops/fly.toml".into()],
                &["FLY_API_TOKEN".into()],
            ),
            Some("windows".to_owned())
        );
        assert_eq!(
            resolve_deploy_target_platform(
                Some("any"),
                "python -B ops/release.py",
                &["ops/fly.toml".into()],
                &["FLY_API_TOKEN".into()],
            ),
            None
        );
        assert_eq!(
            resolve_deploy_target_platform(None, "echo done", &["docs/**".into()], &[]),
            None
        );
    }

    // Risk (#685): a collapse.sh commit that passes on Windows and fails on `ubuntu-latest`
    // must never deploy; it waits while red and is skipped once a Linux-green fix lands so
    // only the fix commit deploys.
    #[test]
    fn windows_green_linux_red_commit_waits_and_skips_for_linux_green_fix() {
        let broken = "5249658aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let fix = "9999999bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        // 1. Immediately after Windows post-land green: no Linux checker and no Linux CI result.
        let no_linux = evaluate_platform_commit_status(
            PlatformCommitStatus::NoResult,
            evaluate_github_platform_status(&[], broken, "linux"),
        );
        assert_eq!(no_linux, PlatformCommitStatus::NoResult);
        let (signal, _, _) = format_deploy_waiting_health("server", "linux", broken, &no_linux);
        assert_eq!(
            signal,
            "deploy server waiting: no linux result for 5249658aaaaa"
        );
        assert_eq!(
            decide_deploy_gate("server", broken, "linux", &no_linux, None, false),
            DeployGateDecision::Wait
        );

        // 2. GitHub CI reports Windows green, Linux red on `broken`.
        let runs_after_fail = vec![ci_run(
            "CI",
            101,
            broken,
            RunState::Failed,
            vec![
                (
                    "Server (windows-latest)",
                    JobState::Passed,
                    &["windows-latest"],
                ),
                (
                    "Server (ubuntu-latest)",
                    JobState::Failed,
                    &["ubuntu-latest"],
                ),
                (
                    "Web (check, test, build)",
                    JobState::Passed,
                    &["ubuntu-latest"],
                ),
            ],
        )];
        let broken_status = evaluate_platform_commit_status(
            PlatformCommitStatus::NoResult,
            evaluate_github_platform_status(&runs_after_fail, broken, "linux"),
        );
        assert_eq!(
            broken_status,
            PlatformCommitStatus::Failed {
                reasons: vec!["CI · Server (ubuntu-latest)".to_owned()]
            }
        );
        // Without a newer commit yet, `broken` stays held (never deployed).
        assert_eq!(
            decide_deploy_gate("server", broken, "linux", &broken_status, None, false),
            DeployGateDecision::Wait
        );

        // 3. Fix-forward lands and `ubuntu-latest` is still running: `broken` is skipped so
        // Rosen advances to `fix`, and `fix` waits while Linux CI is pending.
        let runs_fix_pending = vec![
            runs_after_fail[0].clone(),
            ci_run(
                "CI",
                102,
                fix,
                RunState::Running,
                vec![
                    (
                        "Server (windows-latest)",
                        JobState::Passed,
                        &["windows-latest"],
                    ),
                    (
                        "Server (ubuntu-latest)",
                        JobState::Running,
                        &["ubuntu-latest"],
                    ),
                    (
                        "Web (check, test, build)",
                        JobState::Passed,
                        &["ubuntu-latest"],
                    ),
                ],
            ),
        ];
        let fix_pending = evaluate_platform_commit_status(
            PlatformCommitStatus::NoResult,
            evaluate_github_platform_status(&runs_fix_pending, fix, "linux"),
        );
        assert_eq!(
            fix_pending,
            PlatformCommitStatus::Pending {
                jobs: vec!["CI · Server (ubuntu-latest)".to_owned()]
            }
        );
        assert!(matches!(
            decide_deploy_gate(
                "server",
                broken,
                "linux",
                &broken_status,
                Some((fix, &fix_pending)),
                true,
            ),
            DeployGateDecision::SkipRed { .. }
        ));
        assert_eq!(
            decide_deploy_gate("server", fix, "linux", &fix_pending, None, false),
            DeployGateDecision::Wait
        );

        // 4. `fix` turns green on all Linux jobs: `fix` deploys automatically even if
        // Windows CI were still running.
        let runs_fix_green = vec![
            runs_after_fail[0].clone(),
            ci_run(
                "CI",
                102,
                fix,
                RunState::Running,
                vec![
                    (
                        "Server (windows-latest)",
                        JobState::Running,
                        &["windows-latest"],
                    ),
                    (
                        "Server (ubuntu-latest)",
                        JobState::Passed,
                        &["ubuntu-latest"],
                    ),
                    (
                        "Web (check, test, build)",
                        JobState::Passed,
                        &["ubuntu-latest"],
                    ),
                    (
                        "Server image (build, git HTTP backend, graph indexer)",
                        JobState::Passed,
                        &["ubuntu-latest"],
                    ),
                ],
            ),
        ];
        let fix_green = evaluate_platform_commit_status(
            PlatformCommitStatus::NoResult,
            evaluate_github_platform_status(&runs_fix_green, fix, "linux"),
        );
        assert!(matches!(fix_green, PlatformCommitStatus::Green { .. }));
        assert_eq!(
            decide_deploy_gate("server", fix, "linux", &fix_green, None, false),
            DeployGateDecision::Run
        );
    }

    // Risk (#685): a passing Rosen post-land checker on the deploy target platform must count
    // as green when GitHub CI is absent, while failing base CI on that platform still blocks.
    #[test]
    fn rosen_platform_checker_counts_as_green_unless_base_ci_failed_or_pending() {
        let rosen_green = PlatformCommitStatus::Green {
            source: "Rosen post-land (linux)".to_owned(),
        };
        assert_eq!(
            evaluate_platform_commit_status(rosen_green.clone(), PlatformCommitStatus::NoResult),
            rosen_green
        );
        let ci_failed = PlatformCommitStatus::Failed {
            reasons: vec!["CI · Server image".to_owned()],
        };
        assert_eq!(
            evaluate_platform_commit_status(rosen_green.clone(), ci_failed.clone()),
            ci_failed
        );
        let ci_pending = PlatformCommitStatus::Pending {
            jobs: vec!["CI · Server (ubuntu-latest)".to_owned()],
        };
        assert_eq!(
            evaluate_platform_commit_status(rosen_green, ci_pending.clone()),
            ci_pending
        );
    }
}
