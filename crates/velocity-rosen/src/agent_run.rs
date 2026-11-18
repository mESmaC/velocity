//! The managed Rosen CLI and MCP queue share this policy. Agent checks take one
//! machine-wide slot inside Rosen's memory-aware queue.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::OnceLock;

use rosen_client::run_queue::{HELD_ENV, RunClass, RunQueue};

use crate::process::hidden_command;

static AGENT_CLI: OnceLock<Option<PathBuf>> = OnceLock::new();
const PROGRAM_RECORD: &str = "rosen-program";
const REFUSAL: &str = "The integration build covers Rust compilation, lint and tests. Agents may run cargo fmt or queue cargo check -p <crate>.";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentRun {
    Ordinary,
    RustCheck,
}

pub fn policy(program: &str, arguments: &[String]) -> Result<AgentRun, &'static str> {
    let name = executable_name(program);
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
    let _slot = light_slot()?;
    let queue = RunQueue::machine()?;
    let _turn = queue.turn(
        RunClass::Build,
        &format!("agent cargo check in {}", root.display()),
    )?;
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
        .env("CARGO_TARGET_DIR", root.join("target").join("agent-check"))
        .status()
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

fn light_slot() -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(std::env::temp_dir().join("velocity-agent-rust-check.lock"))?;
    file.lock()?;
    Ok(file)
}
