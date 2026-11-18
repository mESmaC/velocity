//! The managed Rosen CLI and MCP queue share this policy. Agents' light Rust runs
//! (single-crate checks and binding generation) take one of a few machine-wide slots
//! and start once their own memory fits above Rosen's reserve.

use std::ffi::OsString;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::OnceLock;

use rosen_client::run_queue::{HELD_ENV, POLL, RunClass, RunQueue};

use crate::process::hidden_command;

static AGENT_CLI: OnceLock<Option<PathBuf>> = OnceLock::new();
const PROGRAM_RECORD: &str = "rosen-program";
const REFUSAL: &str = "The integration build covers Rust compilation, lint and tests. Agents may run cargo fmt or queue cargo check -p <crate>.";
/// Light runs going at once on the machine. Each uses two Cargo jobs.
const LIGHT_SLOTS: usize = 2;
/// What a light run may use: two Cargo jobs plus the linker of a debug build.
const LIGHT_MEMORY: u64 = 3 << 30;
/// The shared target folder beside a Rosen project's workspaces.
const PROJECT_TARGET: &str = ".velocity-agent-target";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentRun {
    Ordinary,
    /// `cargo check -p <crate>`.
    RustCheck,
    /// The UI's `bindings` script, which builds and runs `export-bindings`.
    Bindings,
}

pub fn policy(program: &str, arguments: &[String]) -> Result<AgentRun, &'static str> {
    let name = executable_name(program);
    if name == "pnpm" && runs_bindings_script(arguments) {
        return Ok(AgentRun::Bindings);
    }
    if name == "cargo" {
        return match arguments {
            [command, flag, package]
                if command == "check" && flag == "-p" && package_name(package) =>
            {
                Ok(AgentRun::RustCheck)
            }
            [command, ..] if command == "fmt" => Ok(AgentRun::Ordinary),
            _ => Err(REFUSAL),
        };
    }
    let invokes_rust = std::iter::once(program)
        .chain(arguments.iter().map(String::as_str))
        .any(heavy_script)
        || (matches!(name.as_str(), "cmd" | "powershell" | "pwsh" | "sh" | "bash")
            && heavy_script(&arguments.join(" ")));
    if matches!(
        name.as_str(),
        "rustc" | "clippy-driver" | "cargo-clippy" | "cargo-tauri" | "stress"
    ) || (name == "rustup" && arguments.first().is_some_and(|arg| arg == "run"))
        || name == "rosen"
        || invokes_rust
    {
        return Err(REFUSAL);
    }
    Ok(AgentRun::Ordinary)
}

/// `pnpm -C ui bindings`, `pnpm --dir ui bindings` or `pnpm run bindings`.
fn runs_bindings_script(arguments: &[String]) -> bool {
    let mut rest = arguments;
    loop {
        match rest {
            [flag, _, tail @ ..] if flag == "-C" || flag == "--dir" => rest = tail,
            [run, tail @ ..] if run == "run" => rest = tail,
            [script] => return script == "bindings",
            _ => return false,
        }
    }
}

fn heavy_script(argument: &str) -> bool {
    let argument = argument.to_ascii_lowercase();
    [
        "check.ps1",
        "check-fast.mjs",
        "check-full.mjs",
        "release-gate.ps1",
        "cargo ",
        "cargo.exe ",
    ]
    .iter()
    .any(|pattern| argument.contains(pattern))
}

fn executable_name(program: &str) -> String {
    let name = program.rsplit(['/', '\\']).next().unwrap_or_default();
    let name = name.to_ascii_lowercase();
    [".exe", ".cmd", ".bat"]
        .into_iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(&name)
        .to_owned()
}

fn package_name(package: &str) -> bool {
    !package.starts_with('-')
        && !package.is_empty()
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Put a native CLI entry point before the bundled Rosen program on agent PATHs.
/// A hard link keeps the installed app's bytes shared when both are on one volume.
pub fn install(executable: &Path) {
    let Some(rosen) = crate::server::program_folder("rosen")
        .map(|folder| folder.join(format!("rosen{}", std::env::consts::EXE_SUFFIX)))
    else {
        return;
    };
    AGENT_CLI.get_or_init(|| {
        create_cli(executable, &rosen)
            .inspect_err(|error| {
                eprintln!("Could not install the agent Rosen command: {error}");
            })
            .ok()
    });
}

fn create_cli(executable: &Path, rosen: &Path) -> io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let folder = std::env::temp_dir().join(format!(
        "velocity-agent-rosen-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&folder)?;
    let shim = folder.join(format!("rosen{}", std::env::consts::EXE_SUFFIX));
    if std::fs::hard_link(executable, &shim).is_err() {
        std::fs::copy(executable, &shim)?;
    }
    std::fs::write(
        folder.join(PROGRAM_RECORD),
        rosen.to_string_lossy().as_bytes(),
    )?;
    Ok(shim)
}

pub fn cli() -> Option<&'static Path> {
    AGENT_CLI.get().and_then(Option::as_deref)
}

/// Returns None in the normal app executable. The shim forwards Rosen's other
/// subcommands, preserving their argument boundaries and inherited streams.
pub fn serve() -> Option<io::Result<ExitStatus>> {
    let executable = std::env::current_exe().ok()?;
    if executable_name(executable.file_name()?.to_str()?) != "rosen" {
        return None;
    }
    let record = executable.parent()?.join(PROGRAM_RECORD);
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    Some(std::fs::read_to_string(record).and_then(|program| run(program, &arguments)))
}

fn run(program: String, arguments: &[OsString]) -> io::Result<ExitStatus> {
    let mut command = hidden_command(program);
    let subcommand = subcommand_index(arguments);
    if !arguments.get(subcommand).is_some_and(|arg| arg == "run") {
        return command.args(arguments).status();
    }
    let run_arguments = &arguments[subcommand + 1..];
    let (program_at, policy) = classify_run(run_arguments).map_err(io::Error::other)?;
    if policy == AgentRun::Ordinary {
        return command.args(arguments).env_remove(HELD_ENV).status();
    }
    let directory = std::env::current_dir()?;
    let root = directory
        .ancestors()
        .find(|path| path.join(".rosen").exists() || path.join(".git").exists())
        .unwrap_or(&directory);
    let target = light_target(root);
    std::fs::create_dir_all(&target)?;
    // Cargo locks a target folder for a whole build, so runs that share one wait here
    // rather than in a machine slot.
    let _project = locked(&target.join(".velocity-light-run.lock"))?;
    let _slot = light_slot()?;
    wait_for_memory()?;
    // No Rosen turn: its admission charges every running turn its full cap (6 GiB for
    // an integration build), which kept two-job checks waiting while gigabytes sat free.
    // A light run starts when its own memory fits above Rosen's reserve; the run below
    // still executes in a job capped at a build turn's memory.
    command
        .args(&arguments[..=subcommand])
        .args(["--class", RunClass::Build.name(), "--"])
        .args(&run_arguments[program_at..])
        .env(HELD_ENV, RunClass::Build.name())
        .env("CARGO_BUILD_JOBS", "2")
        .env("CARGO_INCREMENTAL", "1")
        // The shared compiler cache rejects incremental compilation.
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("CARGO_TARGET_DIR", target)
        .status()
}

/// One target folder per Rosen project, beside its workspaces: registry dependencies
/// build once, and each workspace's own crates keep separate artifacts because their
/// path is part of the package ID. Elsewhere the workspace keeps its own.
fn light_target(root: &Path) -> PathBuf {
    match root.parent() {
        Some(project) if root.join(".rosen").exists() => project.join(PROJECT_TARGET),
        _ => root.join("target").join("agent-check"),
    }
}

fn wait_for_memory() -> io::Result<()> {
    let queue = RunQueue::machine()?;
    while !queue.memory_headroom().fits(LIGHT_MEMORY) {
        std::thread::sleep(POLL);
    }
    Ok(())
}

fn subcommand_index(arguments: &[OsString]) -> usize {
    let mut index = 0;
    while let Some(prefix) = arguments.get(index).and_then(|argument| argument.to_str()) {
        match prefix {
            "--endpoint" => index += 2,
            value if value.starts_with("--endpoint=") || value == "--json" => index += 1,
            _ => break,
        }
    }
    index
}

fn classify_run(arguments: &[OsString]) -> Result<(usize, AgentRun), &'static str> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.to_str() {
            Some("--") => {
                index += 1;
                break;
            }
            Some("--class" | "--endpoint") => index += 2,
            Some(value)
                if value.starts_with("--class=")
                    || value.starts_with("--endpoint=")
                    || value == "--json" =>
            {
                index += 1
            }
            Some("--help" | "-h" | "--version" | "-V" | "--slots" | "--json-schema") => {
                return Ok((index, AgentRun::Ordinary));
            }
            _ => break,
        }
    }
    let Some(program) = arguments.get(index).and_then(|value| value.to_str()) else {
        return Ok((index, AgentRun::Ordinary));
    };
    let rest: Vec<_> = arguments[index + 1..]
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    policy(program, &rest).map(|policy| (index, policy))
}

/// Takes the first free machine-wide light slot, waiting while all are taken. Slot 0
/// keeps the single slot's old name, so older shims still count against it.
fn light_slot() -> io::Result<File> {
    let folder = std::env::temp_dir();
    loop {
        for slot in 0..LIGHT_SLOTS {
            let name = match slot {
                0 => "velocity-agent-rust-check.lock".to_owned(),
                _ => format!("velocity-agent-rust-check-{slot}.lock"),
            };
            let file = lock_file(&folder.join(name))?;
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
        std::thread::sleep(POLL);
    }
}

fn locked(path: &Path) -> io::Result<File> {
    let file = lock_file(path)?;
    file.lock()?;
    Ok(file)
}

fn lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}
