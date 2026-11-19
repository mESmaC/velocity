//! Use private commit on Windows, the same quantity reported in event 2004.

use std::time::{Duration, Instant};

use serde::Serialize;
use specta::Type;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tauri::AppHandle;
use velocity_threads::REVIEW_SWARM_SWITCH;

const GIB: u64 = 1024 * 1024 * 1024;
const WINDOW_BUDGET: u64 = 2 * GIB;
const HELPER_BUDGET: u64 = GIB;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const LOG_INTERVAL: Duration = Duration::from_secs(120);

#[derive(Serialize, Type)]
pub struct ProcessMemory {
    pid: u32,
    role: &'static str,
    memory_mib: u32,
    budget_mib: u32,
}

fn role(argument: Option<&str>) -> &'static str {
    match argument {
        Some(REVIEW_SWARM_SWITCH) => "Review swarm MCP",
        Some("--airc-mcp") => "AIRC MCP",
        Some("--graph-mcp") => "Code graph MCP",
        Some("--graph-project") => "Project graph",
        Some("--graph-warm") => "Graph warmer",
        Some("--scout-mcp") => "Scout MCP",
        Some("--queued-run-mcp") => "Run queue MCP",
        Some("machine") => "Dispatch worker",
        Some("--velocity-supervised") => "Main window",
        _ => "Velocity launcher",
    }
}

fn budget(role: &str) -> u64 {
    if matches!(role, "Main window" | "Dispatch worker") {
        WINDOW_BUDGET
    } else {
        HELPER_BUDGET
    }
}

fn charged(process: &sysinfo::Process) -> u64 {
    if cfg!(windows) {
        process.virtual_memory()
    } else {
        process.memory()
    }
}

fn mib(bytes: u64) -> u32 {
    u32::try_from(bytes.div_ceil(1024 * 1024)).unwrap_or(u32::MAX)
}

fn sample(current_pid: u32, current_role: &'static str) -> Result<Vec<ProcessMemory>, String> {
    let mut system = System::new().map_err(|error| error.to_string())?;
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let executable = std::env::current_exe().ok();
    let name = executable.as_deref().and_then(std::path::Path::file_name);
    let pids: Vec<_> = system
        .processes()
        .values()
        .filter(|process| Some(process.name()) == name || process.pid().as_u32() == current_pid)
        .map(sysinfo::Process::pid)
        .collect();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::nothing()
            .with_memory()
            .with_cmd(UpdateKind::Always),
    );
    let mut rows: Vec<_> = pids
        .iter()
        .filter_map(|&pid| system.process(pid))
        .map(|process| {
            let pid = process.pid().as_u32();
            let role = if pid == current_pid {
                current_role
            } else {
                let arguments = process.cmd();
                if arguments
                    .iter()
                    .skip(1)
                    .any(|argument| argument == "--velocity-supervised")
                {
                    "Main window"
                } else {
                    role(arguments.get(1).and_then(|argument| argument.to_str()))
                }
            };
            ProcessMemory {
                pid,
                role,
                memory_mib: mib(charged(process)),
                budget_mib: mib(budget(role)),
            }
        })
        .collect();
    rows.sort_unstable_by_key(|row| (row.pid != current_pid, row.pid));
    Ok(rows)
}

fn enforce(limit: u64, mut exceeded: impl FnMut(u64), mut periodic: impl FnMut()) {
    let mut system = match System::new() {
        Ok(system) => system,
        Err(error) => {
            eprintln!("Could not start the process memory guard: {error}");
            return;
        }
    };
    let pid = Pid::from_u32(std::process::id());
    let mut next_log = Instant::now();
    loop {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        if let Some(process) = system.process(pid)
            && charged(process) >= limit
        {
            exceeded(charged(process));
            return;
        }
        if Instant::now() >= next_log {
            periodic();
            next_log = Instant::now() + LOG_INTERVAL;
        }
        std::thread::sleep(SAMPLE_INTERVAL);
    }
}

pub fn start_helper_guard(argument: &str) {
    let role = role(Some(argument));
    if role == "Velocity launcher" || role == "Main window" {
        return;
    }
    std::thread::spawn(move || {
        enforce(
            budget(role),
            |bytes| {
                eprintln!(
                    "{role} PID {} exceeded its memory budget at {} MiB",
                    std::process::id(),
                    mib(bytes)
                );
                std::process::exit(1);
            },
            || {},
        );
    });
}

pub fn start(application: AppHandle) {
    std::thread::spawn(move || {
        let pid = std::process::id();
        enforce(
            WINDOW_BUDGET,
            |bytes| {
                crate::diagnostics::record(
                    "memory",
                    &format!(
                        "Main window PID {pid} reached {} MiB, saving and exiting",
                        mib(bytes)
                    ),
                );
                application.exit(0);
            },
            || {
                if let Ok(rows) = sample(pid, "Main window")
                    && let Ok(message) = serde_json::to_string(&rows)
                {
                    crate::diagnostics::record("memory", &message);
                }
            },
        );
    });
}

#[tauri::command]
#[specta::specta]
pub async fn get_process_memory() -> Result<Vec<ProcessMemory>, String> {
    crate::layout::off_ui_thread(|| sample(std::process::id(), "Main window")).await
}
