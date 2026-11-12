//! `velocity.log` in the app data folder: errors the window reports, panics, and background
//! failures that have no caller to return to. A windowed release build has no console, so
//! without this file a blank window or a dead background task leaves no evidence (#273).
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

/// Errors repeat when every update re-renders a broken pane, so the log keeps one older
/// file beside the current one and never grows without bound.
const LOG_LIMIT: u64 = 1024 * 1024;
/// A message the window sends is cut here; a stack trace fits, a runaway payload doesn't.
const MESSAGE_LIMIT: usize = 8 * 1024;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
/// Serializes appends, so lines from the window and background threads never interleave.
static WRITING: Mutex<()> = Mutex::new(());

/// Points the log at the app data folder and records panics there too.
pub fn initialize(application: &AppHandle) {
    match application.path().app_data_dir() {
        Ok(directory) => {
            if let Err(error) = fs::create_dir_all(&directory) {
                eprintln!("Velocity log: {error}");
            }
            // Set once per process; a second Tauri setup keeps the first path.
            let _ = LOG_PATH.set(directory.join("velocity.log"));
        }
        Err(error) => eprintln!("Velocity log: {error}"),
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        // A panic while this thread holds the log must not wait on itself.
        write_entry("panic", &panic.to_string(), false);
        previous(panic);
    }));
}

/// Appends one entry. Failing to log must never fail the caller, so errors only reach stderr.
pub fn record(source: &str, message: &str) {
    write_entry(source, message, true);
}

fn write_entry(source: &str, message: &str, wait: bool) {
    eprintln!("Velocity {source}: {message}");
    let Some(path) = LOG_PATH.get() else {
        return;
    };
    let writing = if wait {
        WRITING.lock().ok()
    } else {
        WRITING.try_lock().ok()
    };
    let Some(_writing) = writing else {
        return;
    };
    if let Err(error) = append(path, source, message) {
        eprintln!("Velocity log: {error}");
    }
}

fn append(path: &Path, source: &str, message: &str) -> std::io::Result<()> {
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
