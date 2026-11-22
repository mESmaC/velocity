//! Local CLI login fallbacks for Rosen `[deploy]` secrets (#604).
//!
//! When a declared deploy secret is not saved in Windows Credential Manager:
//! - `FLY_API_TOKEN` is read from `flyctl auth token` (direct spawn, no shell, never logged).
//! - `CLOUDFLARE_API_TOKEN` is read from wrangler's `default.toml` OAuth config
//!   (refreshing with `npx --yes wrangler whoami` when expired).
//!
//! Secret values are wrapped in [`CliSecretValue`] so they never appear in `Debug`
//! output, diagnostics, logs, or transcripts.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use velocity_core::deploy::{
    CLOUDFLARE_API_TOKEN, DeploySecretSource, FLY_API_TOKEN, WranglerConfig,
    is_wrangler_token_expired, parse_wrangler_config,
};

use crate::checker;
use crate::process::hidden_command;

const FLYCTL_TIMEOUT: Duration = Duration::from_secs(30);
const WRANGLER_REFRESH_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_TOKEN_OUTPUT_BYTES: usize = 32 * 1024;

/// An opaque deploy secret loaded from a local CLI login (#604).
#[derive(Clone, PartialEq, Eq)]
pub struct CliSecretValue(String);

impl CliSecretValue {
    /// Returns the raw secret string solely for injecting into a deploy child process
    /// or redacting from that child process's captured output.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

// Secret values never reach a log through `{:?}`.
impl std::fmt::Debug for CliSecretValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CliSecretValue([REDACTED])")
    }
}

/// Result of looking up a deploy secret from a known local CLI login (#604).
#[derive(Debug)]
pub enum CliSecretLookup {
    /// Secret was resolved from a signed-in local CLI.
    Found {
        source: DeploySecretSource,
        value: CliSecretValue,
    },
    /// Secret name has a known CLI fallback, but that CLI login is unavailable.
    Unavailable {
        source: DeploySecretSource,
        reason: String,
    },
    /// Secret name has no known CLI login fallback.
    Unsupported,
}

/// Resolves a declared deploy secret from a known local CLI login when absent
/// from Windows Credential Manager (#604).
pub fn load_cli_fallback_secret(name: &str) -> CliSecretLookup {
    match name {
        FLY_API_TOKEN => load_flyctl_token(),
        CLOUDFLARE_API_TOKEN => load_wrangler_token(),
        _ => CliSecretLookup::Unsupported,
    }
}

/// Reports whether a known CLI login fallback is currently signed in for `name`,
/// without returning or logging any secret value (#604).
pub fn probe_cli_fallback_source(name: &str) -> Option<DeploySecretSource> {
    match name {
        FLY_API_TOKEN => matches!(load_flyctl_token(), CliSecretLookup::Found { .. })
            .then_some(DeploySecretSource::FlyctlLogin),
        CLOUDFLARE_API_TOKEN => {
            let now = current_unix_seconds();
            let candidates = wrangler_config_candidates();
            read_best_wrangler_config(&candidates, now)
                .filter(|config| {
                    config.refresh_token.is_some()
                        || (config.oauth_token.is_some()
                            && !is_wrangler_token_expired(config.expiration_time_unix, now))
                })
                .map(|_| DeploySecretSource::WranglerLogin)
        }
        _ => None,
    }
}

/// Reads `FLY_API_TOKEN` directly from `flyctl auth token` without a shell (#604).
fn load_flyctl_token() -> CliSecretLookup {
    let mut command = hidden_command("flyctl");
    command
        .args(["auth", "token"])
        .env_remove(FLY_API_TOKEN)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    match run_capture_stdout_with_timeout(command, FLYCTL_TIMEOUT) {
        CommandOutcome::Output {
            success: true,
            stdout,
        } => {
            let token = stdout.trim();
            if token.is_empty() {
                CliSecretLookup::Unavailable {
                    source: DeploySecretSource::FlyctlLogin,
                    reason: "flyctl login returned an empty token (run `flyctl auth login`)".into(),
                }
            } else {
                CliSecretLookup::Found {
                    source: DeploySecretSource::FlyctlLogin,
                    value: CliSecretValue(token.to_owned()),
                }
            }
        }
        CommandOutcome::Output { success: false, .. } => CliSecretLookup::Unavailable {
            source: DeploySecretSource::FlyctlLogin,
            reason: "flyctl is not signed in (run `flyctl auth login`)".into(),
        },
        CommandOutcome::NotFound => CliSecretLookup::Unavailable {
            source: DeploySecretSource::FlyctlLogin,
            reason: "flyctl was not found on PATH".into(),
        },
        CommandOutcome::TimedOut => CliSecretLookup::Unavailable {
            source: DeploySecretSource::FlyctlLogin,
            reason: "flyctl auth token timed out".into(),
        },
        CommandOutcome::SpawnFailed => CliSecretLookup::Unavailable {
            source: DeploySecretSource::FlyctlLogin,
            reason: "could not run flyctl auth token".into(),
        },
    }
}

/// Reads `CLOUDFLARE_API_TOKEN` from wrangler's `default.toml`, refreshing first with
/// `npx --yes wrangler whoami` when the stored OAuth token has expired (#604).
fn load_wrangler_token() -> CliSecretLookup {
    let candidates = wrangler_config_candidates();
    let now = current_unix_seconds();
    let Some(config) = read_best_wrangler_config(&candidates, now) else {
        return CliSecretLookup::Unavailable {
            source: DeploySecretSource::WranglerLogin,
            reason: "wrangler login config not found (run `npx wrangler login`)".into(),
        };
    };
    if let Some(token) = config.oauth_token.as_deref()
        && !is_wrangler_token_expired(config.expiration_time_unix, now)
    {
        return CliSecretLookup::Found {
            source: DeploySecretSource::WranglerLogin,
            value: CliSecretValue(token.to_owned()),
        };
    }
    if let Err(reason) = refresh_wrangler_login() {
        return CliSecretLookup::Unavailable {
            source: DeploySecretSource::WranglerLogin,
            reason,
        };
    }
    let refreshed_now = current_unix_seconds();
    let Some(refreshed) = read_best_wrangler_config(&candidates, refreshed_now) else {
        return CliSecretLookup::Unavailable {
            source: DeploySecretSource::WranglerLogin,
            reason: "wrangler login config missing after refresh (run `npx wrangler login`)".into(),
        };
    };
    match refreshed.oauth_token {
        Some(token)
            if !is_wrangler_token_expired(refreshed.expiration_time_unix, refreshed_now) =>
        {
            CliSecretLookup::Found {
                source: DeploySecretSource::WranglerLogin,
                value: CliSecretValue(token),
            }
        }
        _ => CliSecretLookup::Unavailable {
            source: DeploySecretSource::WranglerLogin,
            reason: "wrangler OAuth token is expired (run `npx wrangler login`)".into(),
        },
    }
}

fn refresh_wrangler_login() -> Result<(), String> {
    let outcome = run_wrangler_whoami();
    match outcome {
        CommandOutcome::Output { success: true, .. } => Ok(()),
        CommandOutcome::Output { success: false, .. } => Err(
            "wrangler token expired and `npx wrangler whoami` failed (run `npx wrangler login`)"
                .into(),
        ),
        CommandOutcome::NotFound => {
            Err("npx was not found on PATH to refresh wrangler login".into())
        }
        CommandOutcome::TimedOut => {
            Err("`npx wrangler whoami` timed out while refreshing login".into())
        }
        CommandOutcome::SpawnFailed => Err("could not run `npx wrangler whoami`".into()),
    }
}

fn run_wrangler_whoami() -> CommandOutcome {
    #[cfg(windows)]
    let programs = ["npx.cmd", "npx"];
    #[cfg(not(windows))]
    let programs = ["npx"];
    for program in programs {
        let mut command = hidden_command(program);
        command
            .args(["--yes", "wrangler", "whoami"])
            // Clear API token overrides so wrangler refreshes its OAuth login in default.toml.
            .env_remove(CLOUDFLARE_API_TOKEN)
            .env_remove("CLOUDFLARE_API_KEY")
            .env_remove("CLOUDFLARE_EMAIL")
            .env("WRANGLER_SEND_METRICS", "false")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let outcome = run_status_with_timeout(command, WRANGLER_REFRESH_TIMEOUT);
        if !matches!(outcome, CommandOutcome::NotFound) {
            return outcome;
        }
    }
    CommandOutcome::NotFound
}

/// Candidate `default.toml` paths wrangler uses across Windows XDG, legacy home, and Unix (#604).
fn wrangler_config_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut push_unique = |path: PathBuf| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };
    if let Some(xdg) = non_empty_env_path("XDG_CONFIG_HOME") {
        push_unique(xdg.join(".wrangler").join("config").join("default.toml"));
    }
    if let Some(appdata) = non_empty_env_path("APPDATA") {
        push_unique(
            appdata
                .join("xdg.config")
                .join(".wrangler")
                .join("config")
                .join("default.toml"),
        );
    }
    for home_var in ["USERPROFILE", "HOME"] {
        if let Some(home) = non_empty_env_path(home_var) {
            push_unique(
                home.join(".config")
                    .join(".wrangler")
                    .join("config")
                    .join("default.toml"),
            );
            push_unique(home.join(".wrangler").join("config").join("default.toml"));
        }
    }
    if let Some(local_appdata) = non_empty_env_path("LOCALAPPDATA") {
        push_unique(
            local_appdata
                .join("xdg.config")
                .join(".wrangler")
                .join("config")
                .join("default.toml"),
        );
    }
    paths
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    let value = std::env::var_os(name)?;
    (!value.is_empty()).then(|| PathBuf::from(value))
}

fn read_best_wrangler_config(candidates: &[PathBuf], now_unix: u64) -> Option<WranglerConfig> {
    let mut best: Option<WranglerConfig> = None;
    for path in candidates {
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue;
        };
        let parsed = parse_wrangler_config(&contents);
        if parsed.oauth_token.is_none() && parsed.refresh_token.is_none() {
            continue;
        }
        let replace = match &best {
            None => true,
            Some(current) => {
                wrangler_config_rank(&parsed, now_unix) > wrangler_config_rank(current, now_unix)
            }
        };
        if replace {
            best = Some(parsed);
        }
    }
    best
}

fn wrangler_config_rank(config: &WranglerConfig, now_unix: u64) -> (u8, u64) {
    let valid_token = config.oauth_token.is_some()
        && !is_wrangler_token_expired(config.expiration_time_unix, now_unix);
    let tier = if valid_token {
        3
    } else if config.oauth_token.is_some() {
        2
    } else {
        1
    };
    (tier, config.expiration_time_unix.unwrap_or(0))
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

enum CommandOutcome {
    Output { success: bool, stdout: String },
    NotFound,
    TimedOut,
    SpawnFailed,
}

fn run_capture_stdout_with_timeout(mut command: Command, timeout: Duration) -> CommandOutcome {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CommandOutcome::NotFound;
        }
        Err(_) => return CommandOutcome::SpawnFailed,
    };
    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
    let reader = child.stdout.take().map(|pipe| {
        let buffer = Arc::clone(&captured);
        std::thread::spawn(move || read_bounded(pipe, buffer))
    });
    let exited = wait_child_with_timeout(&mut child, timeout);
    if let Some(handle) = reader {
        let _ = handle.join();
    }
    let Some(success) = exited else {
        return CommandOutcome::TimedOut;
    };
    let bytes = captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let stdout = String::from_utf8_lossy(&bytes).into_owned();
    CommandOutcome::Output { success, stdout }
}

fn run_status_with_timeout(mut command: Command, timeout: Duration) -> CommandOutcome {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CommandOutcome::NotFound;
        }
        Err(_) => return CommandOutcome::SpawnFailed,
    };
    match wait_child_with_timeout(&mut child, timeout) {
        Some(success) => CommandOutcome::Output {
            success,
            stdout: String::new(),
        },
        None => CommandOutcome::TimedOut,
    }
}

fn wait_child_with_timeout(child: &mut Child, timeout: Duration) -> Option<bool> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    checker::stop_tree(child);
                    return None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => return Some(false),
        }
    }
}

fn read_bounded<R: Read>(mut pipe: R, output: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 4096];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let mut guard = output
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let remaining = MAX_TOKEN_OUTPUT_BYTES.saturating_sub(guard.len());
                if remaining > 0 {
                    guard.extend_from_slice(&chunk[..count.min(remaining)]);
                }
            }
        }
    }
}
