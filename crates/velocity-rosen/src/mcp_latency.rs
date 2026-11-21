//! Per-call MCP server latency telemetry, split into process start, connecting to Rosen
//! (or the server's backing daemon), and the tool work itself (#541).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Optional override path for MCP latency logs (used by benchmarks and tests).
pub const LATENCY_LOG_ENV: &str = "VELOCITY_MCP_LATENCY_LOG";
const LOG_FILE: &str = "mcp-latency.jsonl";
const ROTATED_LOG_FILE: &str = "mcp-latency.old.jsonl";
const LOG_LIMIT: u64 = 1024 * 1024;

static PROCESS_START: OnceLock<Instant> = OnceLock::new();
static DEFAULT_APP_DATA: OnceLock<PathBuf> = OnceLock::new();

/// Records process entry time before any MCP server dispatch runs.
pub fn mark_process_start() {
    let _ = PROCESS_START.get_or_init(Instant::now);
}

/// Registers the default app data directory for helper MCP servers that do not receive
/// `<app-data>` on their command line (such as `velocity-browser` and `velocity-run`).
pub fn set_default_app_data(path: PathBuf) {
    let _ = DEFAULT_APP_DATA.set(path);
}

/// Milliseconds elapsed since process entry (or since the first latency call if unmarked).
pub fn elapsed_since_process_start_ms() -> u64 {
    duration_ms(PROCESS_START.get_or_init(Instant::now).elapsed())
}

pub fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// One measured MCP call, split into process startup, connection setup, and tool work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCallTiming {
    pub unix_ms: u64,
    pub server: String,
    pub tool: String,
    pub warm: bool,
    pub ok: bool,
    pub process_start_ms: u64,
    pub rosen_connect_ms: u64,
    pub work_ms: u64,
    pub total_ms: u64,
}

/// Non-blocking JSONL recorder for MCP server calls. Writes happen on one background thread
/// so instrumentation never adds I/O latency to a tool response.
#[derive(Clone)]
pub struct LatencyRecorder {
    server: &'static str,
    sender: Option<mpsc::SyncSender<McpCallTiming>>,
    first_recorded: Arc<AtomicBool>,
}

impl LatencyRecorder {
    /// Creates a recorder targeting `VELOCITY_MCP_LATENCY_LOG` when set, otherwise
    /// `<app_data>/mcp-latency.jsonl`. When neither is available, recording is a no-op.
    pub fn open(server: &'static str, app_data: Option<&Path>) -> Self {
        let path = resolve_log_path(app_data);
        let sender = path.map(spawn_writer);
        Self {
            server,
            sender,
            first_recorded: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn record(&self, tool: &str, connect: Duration, total: Duration, ok: bool) {
        let Some(sender) = &self.sender else {
            return;
        };
        let was_first = !self.first_recorded.swap(true, Ordering::AcqRel);
        let process_start_ms = if was_first {
            elapsed_since_process_start_ms()
        } else {
            0
        };
        let total_ms = duration_ms(total);
        let rosen_connect_ms = duration_ms(connect).min(total_ms);
        let work_ms = total_ms.saturating_sub(rosen_connect_ms);
        let entry = McpCallTiming {
            unix_ms: unix_now_ms(),
            server: self.server.to_owned(),
            tool: tool.to_owned(),
            warm: !was_first,
            ok,
            process_start_ms,
            rosen_connect_ms,
            work_ms,
            total_ms,
        };
        // Never block a tool reply if disk I/O stalls.
        let _ = sender.try_send(entry);
    }

    /// Records a call with explicit work and connect durations (e.g. when a shared project
    /// worker reports its own Rosen connect and execution times).
    pub fn record_split(&self, tool: &str, connect_ms: u64, work_ms: u64, total_ms: u64, ok: bool) {
        let Some(sender) = &self.sender else {
            return;
        };
        let was_first = !self.first_recorded.swap(true, Ordering::AcqRel);
        let process_start_ms = if was_first {
            elapsed_since_process_start_ms()
        } else {
            0
        };
        let rosen_connect_ms = connect_ms.min(total_ms);
        let entry = McpCallTiming {
            unix_ms: unix_now_ms(),
            server: self.server.to_owned(),
            tool: tool.to_owned(),
            warm: !was_first,
            ok,
            process_start_ms,
            rosen_connect_ms,
            work_ms: work_ms.min(total_ms),
            total_ms,
        };
        let _ = sender.try_send(entry);
    }
}

fn resolve_log_path(app_data: Option<&Path>) -> Option<PathBuf> {
    if let Some(override_path) = std::env::var_os(LATENCY_LOG_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(override_path));
    }
    app_data
        .or_else(|| DEFAULT_APP_DATA.get().map(PathBuf::as_path))
        .map(|folder| folder.join(LOG_FILE))
}

fn unix_now_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn spawn_writer(path: PathBuf) -> mpsc::SyncSender<McpCallTiming> {
    let (sender, receiver) = mpsc::sync_channel::<McpCallTiming>(256);
    let _ = std::thread::Builder::new()
        .name("mcp-latency-log".into())
        .spawn(move || {
            while let Ok(entry) = receiver.recv() {
                let _ = append_entry(&path, &entry);
            }
        });
    sender
}

fn append_entry(path: &Path, entry: &McpCallTiming) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() > LOG_LIMIT) {
        let _ = fs::rename(path, path.with_file_name(ROTATED_LOG_FILE));
    }
    let mut bytes = serde_json::to_vec(entry)?;
    bytes.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_cold_and_warm_calls_into_phases() {
        // Risk (#541): latency logs must separate cold startup, Rosen connect, and work.
        let directory = tempfile::tempdir().ok();
        assert!(directory.is_some());
        let Some(directory) = directory else { return };
        let path = directory.path().join(LOG_FILE);
        mark_process_start();
        let recorder = LatencyRecorder {
            server: "test-mcp",
            sender: Some(spawn_writer(path.clone())),
            first_recorded: Arc::new(AtomicBool::new(false)),
        };
        recorder.record(
            "status",
            Duration::from_millis(12),
            Duration::from_millis(45),
            true,
        );
        recorder.record(
            "status",
            Duration::from_millis(0),
            Duration::from_millis(8),
            true,
        );
        drop(recorder);
        // Wait briefly for the background writer thread to flush both lines.
        let deadline = Instant::now() + Duration::from_secs(2);
        let records = loop {
            if let Ok(text) = fs::read_to_string(&path) {
                let rows: Vec<McpCallTiming> = text
                    .lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect();
                if rows.len() == 2 {
                    break rows;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for latency log"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(!records[0].warm);
        assert_eq!(records[0].rosen_connect_ms, 12);
        assert_eq!(records[0].work_ms, 33);
        assert_eq!(records[0].total_ms, 45);
        assert!(records[1].warm);
        assert_eq!(records[1].process_start_ms, 0);
        assert_eq!(records[1].rosen_connect_ms, 0);
        assert_eq!(records[1].work_ms, 8);
    }
}
