//! Writes the running totals of Velocity's `gh` calls to `github-requests.log` in the app
//! data folder once a minute (#217). A windowed app has no console, so this file is where an
//! exhausted GitHub budget can be traced to the feature that spent it.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

const LOG_INTERVAL: Duration = Duration::from_secs(60);
/// One line a minute stays small, but never let the log grow without bound.
const LOG_LIMIT: u64 = 1024 * 1024;

pub fn initialize(application: &AppHandle) {
    let Ok(directory) = application.path().app_data_dir() else {
        return;
    };
    let started = Instant::now();
    thread::spawn(move || {
        loop {
            thread::sleep(LOG_INTERVAL);
            if let Err(error) = append(&directory, started) {
                eprintln!("Velocity GitHub request log: {error}");
            }
        }
    });
}

fn append(directory: &Path, started: Instant) -> std::io::Result<()> {
    let path = directory.join("github-requests.log");
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > LOG_LIMIT) {
        fs::rename(&path, directory.join("github-requests.old.log"))?;
    }
    let totals = velocity_github::request_totals();
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut line = format!(
        "unix {unix_seconds} · up {}s · {} gh calls",
        started.elapsed().as_secs(),
        totals.iter().map(|total| total.count).sum::<u64>()
    );
    for total in totals {
        line.push_str(&format!(
            " · {} {} {}",
            total.caller, total.operation, total.count
        ));
    }
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}
