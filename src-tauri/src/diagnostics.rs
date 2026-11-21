//! `velocity.log` in the app data folder: errors the window reports, panics, and background
//! failures that have no caller to return to. A windowed release build has no console, so
//! without this file a blank window or a dead background task leaves no evidence (#273).
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

/// Errors repeat when every update re-renders a broken pane, so the log keeps one older
/// file beside the current one and never grows without bound.
const LOG_LIMIT: u64 = 1024 * 1024;
/// A message the window sends is cut here; a stack trace fits, a runaway payload doesn't.
const MESSAGE_LIMIT: usize = 8 * 1024;
const LOG_FILE: &str = "velocity.log";

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Points the log at the app data folder and records panics there too.
pub fn initialize(application: &AppHandle) {
    match application.path().app_data_dir() {
        Ok(directory) => {
            if let Err(error) = fs::create_dir_all(&directory) {
                eprintln!("Velocity log: {error}");
            }
            // Set once per process; a second Tauri setup keeps the first path.
            let _ = LOG_PATH.set(directory.join(LOG_FILE));
        }
        Err(error) => eprintln!("Velocity log: {error}"),
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        // A panic while this thread holds the log must not wait on itself.
        write_entry(
            LOG_PATH.get().map(PathBuf::as_path),
            "panic",
            &panic.to_string(),
            false,
        );
        previous(panic);
    }));
    velocity_core::performance::set_breach_logger(record_performance_breach);
}

fn record_performance_breach(message: &str) {
    record(velocity_core::performance::BREACH_LOG_SOURCE, message);
}

/// Appends one entry. Failing to log must never fail the caller, so errors only reach stderr.
pub fn record(source: &str, message: &str) {
    write_entry(LOG_PATH.get().map(PathBuf::as_path), source, message, true);
}

pub(crate) fn record_helper_memory(message: &str) {
    let path = helper_log_path();
    write_entry(path.as_deref(), "memory", message, true);
}

fn helper_log_path() -> Option<PathBuf> {
    #[derive(serde::Deserialize)]
    struct AppConfig {
        identifier: String,
    }

    // Match Tauri's desktop app_data_dir without starting a Tauri application in the helper.
    let config: AppConfig = serde_json::from_str(include_str!("../tauri.conf.json")).ok()?;
    let directory = dirs::data_dir()?.join(config.identifier);
    let _ = fs::create_dir_all(&directory);
    Some(directory.join(LOG_FILE))
}

fn write_entry(path: Option<&Path>, source: &str, message: &str, wait: bool) {
    eprintln!("Velocity {source}: {message}");
    if let Some(path) = path
        && let Err(error) = append(path, source, message, wait)
    {
        eprintln!("Velocity log: {error}");
    }
}

fn append(path: &Path, source: &str, message: &str, wait: bool) -> std::io::Result<()> {
    // Lock a separate file so appends stay serialized even while rotating the log.
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))?;
    if wait {
        lock.lock()?;
    } else if lock.try_lock().is_err() {
        return Ok(());
    }
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() > LOG_LIMIT) {
        fs::rename(path, path.with_file_name("velocity.old.log"))?;
    }
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // Continuation lines are indented, so each entry still starts with its timestamp.
    let body = message.trim_end().replace('\n', "\n    ");
    let line = format!(
        "unix {}.{:03} [{source}] {body}\n",
        elapsed.as_secs(),
        elapsed.subsec_millis()
    );
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

/// Records an error the window caught: a pane that failed to render, an uncaught exception
/// or an unhandled promise rejection. `source` names where the window caught it.
#[tauri::command]
#[specta::specta]
pub fn log_window_error(source: String, message: String) {
    let source: String = source.chars().take(64).collect();
    let message: String = message.chars().take(MESSAGE_LIMIT).collect();
    record(&format!("window {source}"), &message);
}
