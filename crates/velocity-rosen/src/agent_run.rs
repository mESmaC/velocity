//! The agent shell, managed Rosen CLI and MCP queue share this policy. Light Rust runs
//! (single-crate checks, binding generation and Velocity's own bindings check before a
//! land) take one of a few machine-wide slots and start once their own memory fits above
//! Rosen's reserve.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::OnceLock;

use rosen_client::run_queue::{HELD_ENV, POLL, RunClass, RunQueue};

use crate::process::{background_command, hidden_command};

static AGENT_COMMANDS: OnceLock<Option<AgentCommands>> = OnceLock::new();
const APP_RECORD: &str = "velocity-program";
const ROSEN_RECORD: &str = "rosen-program";
const RUST_COMMANDS: &[&str] = &[
    "cargo",
    "rustc",
    "cargo-clippy",
    "cargo-tauri",
    "clippy-driver",
];
const LAUNCHER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/agent-launcher.exe"));
const REFUSAL: &str = "The integration build covers Rust compilation, lint and tests. Agents may run cargo fmt or queue cargo check -p <crate>.";
/// Light runs going at once on the machine. Each uses two Cargo jobs.
pub(crate) const LIGHT_SLOTS: usize = 2;
/// What a light run may use: two Cargo jobs plus the linker of a debug build.
pub(crate) const LIGHT_MEMORY: u64 = 3 << 30;
/// The shared target folder beside a Rosen project's workspaces.
const PROJECT_TARGET: &str = ".velocity-agent-target";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentRun {
    Ordinary,
    /// Rust formatting or the managed Cargo.lock generator.
    RustUtility,
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
        return cargo_policy(arguments, true);
    }
    let invokes_rust = std::iter::once(program)
        .chain(arguments.iter().map(String::as_str))
        .any(heavy_script)
        || (matches!(name.as_str(), "cmd" | "powershell" | "pwsh" | "sh" | "bash")
            && heavy_script(&arguments.join(" ")));
    if RUST_COMMANDS.contains(&name.as_str())
        || name == "stress"
        || (name == "rustup" && arguments.first().is_some_and(|arg| arg == "run"))
        || name == "rosen"
        || invokes_rust
    {
        return Err(REFUSAL);
    }
    Ok(AgentRun::Ordinary)
}

fn cargo_policy(arguments: &[String], managed: bool) -> Result<AgentRun, &'static str> {
    match arguments {
        [command, flag, package] if command == "check" && flag == "-p" && package_name(package) => {
            Ok(AgentRun::RustCheck)
        }
        [command, ..] if command == "fmt" => Ok(AgentRun::RustUtility),
        // Rebase repair needs to regenerate Cargo.lock without starting a Rust build.
        [command, flag, version]
            if managed && command == "metadata" && flag == "--format-version" && version == "1" =>
        {
            Ok(AgentRun::RustUtility)
        }
        _ => Err(REFUSAL),
    }
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

struct AgentCommands {
    folder: PathBuf,
    rosen: Option<PathBuf>,
}

/// Install native console entry points before the real programs on agent PATHs.
pub fn install(executable: &Path) {
    AGENT_COMMANDS.get_or_init(|| {
        create_commands(executable)
            .inspect_err(|error| {
                eprintln!("Could not install the agent shell commands: {error}");
            })
            .ok()
    });
}

fn create_commands(executable: &Path) -> io::Result<AgentCommands> {
    let folder = tempfile::Builder::new()
        .prefix("velocity-agent-")
        .tempdir()?;
    let path = folder.path();
    std::fs::write(
        path.join(APP_RECORD),
        executable.to_string_lossy().as_bytes(),
    )?;
    let cargo = path.join(executable_file("cargo"));
    std::fs::write(&cargo, LAUNCHER)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o700))?;
    }
    for name in &RUST_COMMANDS[1..] {
        copy_launcher(&cargo, &path.join(executable_file(name)))?;
    }
    let rosen_name = executable_file("rosen");
    let rosen = if let Some(real_rosen) = crate::server::program_folder("rosen") {
        let program = std::path::absolute(real_rosen.join(&rosen_name))?;
        std::fs::write(
            path.join(ROSEN_RECORD),
            program.to_string_lossy().as_bytes(),
        )?;
        let shim = path.join(rosen_name);
        copy_launcher(&cargo, &shim)?;
        Some(shim)
    } else {
        None
    };
    Ok(AgentCommands {
        folder: folder.keep(),
        rosen,
    })
}

fn executable_file(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn copy_launcher(source: &Path, destination: &Path) -> io::Result<()> {
    if std::fs::hard_link(source, destination).is_err() {
        std::fs::copy(source, destination)?;
    }
    Ok(())
}

pub(crate) fn command_folder() -> Option<&'static Path> {
    Some(&AGENT_COMMANDS.get()?.as_ref()?.folder)
}

pub(crate) fn is_command_folder(folder: &Path) -> bool {
    folder.join(APP_RECORD).is_file() || folder.join(ROSEN_RECORD).is_file()
}

pub fn cli() -> Option<&'static Path> {
    AGENT_COMMANDS.get()?.as_ref()?.rosen.as_deref()
}

/// App entry point for the console launcher, returning `None` for regular app calls.
pub fn serve() -> Option<(String, io::Result<ExitStatus>)> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next()? != "--velocity-agent-command" {
        return None;
    }
    let executable = PathBuf::from(arguments.next()?);
    let folder = executable.parent()?;
    let name = executable_name(executable.file_name()?.to_str()?);
    let arguments: Vec<_> = arguments.collect();
    let result = if name == "rosen" {
        rosen_program(folder).and_then(|program| run_rosen(&program, &arguments))
    } else {
        run_direct(folder, &name, &arguments)
    };
    Some((name, result))
}

fn rosen_program(folder: &Path) -> io::Result<PathBuf> {
    std::fs::read_to_string(folder.join(ROSEN_RECORD)).map(PathBuf::from)
}

fn run_direct(folder: &Path, name: &str, arguments: &[OsString]) -> io::Result<ExitStatus> {
    if name != "cargo" {
        return Err(io::Error::other(REFUSAL));
    }
    match cargo_policy(&policy_arguments(arguments), false).map_err(io::Error::other)? {
        AgentRun::RustUtility | AgentRun::RustCheck if inside_held_turn() => {
            run_held(name, arguments)
        }
        AgentRun::RustUtility => run_utility(OsStr::new(name), arguments),
        AgentRun::RustCheck => run_light(&rosen_program(folder)?, &[], OsStr::new(name), arguments),
        _ => Err(io::Error::other(REFUSAL)),
    }
}

fn run_rosen(program: &Path, arguments: &[OsString]) -> io::Result<ExitStatus> {
    let subcommand = subcommand_index(arguments);
    if !arguments.get(subcommand).is_some_and(|arg| arg == "run") {
        return hidden_command(program).args(arguments).status();
    }
    let run_arguments = &arguments[subcommand + 1..];
    let (program_at, run) = classify_run(run_arguments).map_err(io::Error::other)?;
    if run != AgentRun::Ordinary && inside_held_turn() {
        // `rosen run` sees the held turn and starts at once (#548).
        return hidden_command(program).args(arguments).status();
    }
    match run {
        AgentRun::Ordinary => hidden_command(program)
            .args(arguments)
            .env_remove(HELD_ENV)
            .status(),
        AgentRun::RustUtility => {
            run_utility(&run_arguments[program_at], &run_arguments[program_at + 1..])
        }
        AgentRun::RustCheck | AgentRun::Bindings => run_light(
            program,
            &arguments[..subcommand],
            &run_arguments[program_at],
            &run_arguments[program_at + 1..],
        ),
    }
}

/// Whether this process already runs inside a turn that Velocity or `rosen run` holds. A
/// queued light run holds its project's light-run lock and a machine slot; a managed
/// command under it that waited for its own turn would wait on that ancestor forever (#548).
fn inside_held_turn() -> bool {
    std::env::var_os(HELD_ENV).is_some()
}

/// Runs the real `name` inside a held turn. It is resolved here, skipping managed command
/// folders, so a `PATH` that still names this shim's folder cannot start the shim again.
fn run_held(name: &str, arguments: &[OsString]) -> io::Result<ExitStatus> {
    let path = toolchain_path()?;
    let program = real_program(name, &path).ok_or_else(|| {
        io::Error::other(format!(
            "Could not find {name} outside Velocity's managed commands"
        ))
    })?;
    background_command(program)
        .args(arguments)
        .env("PATH", path)
        .status()
}

/// The first `name` executable on `path` outside a managed command folder.
fn real_program(name: &str, path: &OsStr) -> Option<PathBuf> {
    let file = executable_file(name);
    std::env::split_paths(path)
        .filter(|folder| !is_command_folder(folder))
        .map(|folder| folder.join(&file))
        .find(|candidate| candidate.is_file())
}

fn run_utility(program: &OsStr, arguments: &[OsString]) -> io::Result<ExitStatus> {
    let _slot = light_slot()?;
    background_command(program)
        .args(arguments)
        .env("PATH", toolchain_path()?)
        .status()
}

fn run_light(
    rosen: &Path,
    rosen_arguments: &[OsString],
    program: &OsStr,
    arguments: &[OsString],
) -> io::Result<ExitStatus> {
    let directory = std::env::current_dir()?;
    let root = directory
        .ancestors()
        .find(|path| path.join(".rosen").exists() || path.join(".git").exists())
        .unwrap_or(&directory);
    let target = light_target(root);
    let _turn = light_turn(&target, root, &directory)?;
    light_command(rosen, rosen_arguments, program, &target)?
        .args(arguments)
        .status()
}

/// Velocity's own light run (#503): `program` in `directory`, building into `target`, with
/// what it printed. It waits and runs like an agent's (blocking).
pub(crate) fn light_output(
    directory: &Path,
    target: &Path,
    program: &str,
    arguments: &[&str],
) -> io::Result<Output> {
    let rosen = crate::server::program_folder("rosen")
        .map(|folder| folder.join(executable_file("rosen")))
        .ok_or_else(|| io::Error::other("the rosen program was not found"))?;
    let _turn = light_turn(target, directory, directory)?;
    light_command(&rosen, &[], OsStr::new(program), target)?
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .output()
}

/// What a light run holds until it ends: its project's target folder and a machine slot.
struct LightTurn {
    _project: File,
    _slot: File,
}

/// Waits for the target folder, a slot and memory, in that order, then prepares the shared
/// target folder for `workspace_root`.
fn light_turn(target: &Path, workspace_root: &Path, directory: &Path) -> io::Result<LightTurn> {
    std::fs::create_dir_all(target)?;
    // Cargo locks a target folder for a whole build, so runs that share one wait here
    // rather than in a machine slot.
    let project = locked(&target.join(".velocity-light-run.lock"))?;
    let slot = light_slot()?;
    wait_for_memory()?;
    crate::light_target::prepare(target, workspace_root, directory)?;
    Ok(LightTurn {
        _project: project,
        _slot: slot,
    })
}

/// `rosen run -- <program>` set up as a light run; the caller adds the arguments.
fn light_command(
    rosen: &Path,
    rosen_arguments: &[OsString],
    program: &OsStr,
    target: &Path,
) -> io::Result<Command> {
    // No Rosen turn: its admission charges every running turn its full cap (6 GiB for
    // an integration build), which kept two-job checks waiting while gigabytes sat free.
    // A light run starts when its own memory fits above Rosen's reserve; the run below
    // still executes in a job capped at a build turn's memory.
    let mut command = background_command(rosen);
    command
        .args(rosen_arguments)
        .args(["run", "--class", RunClass::Build.name(), "--"])
        .arg(program)
        .env("PATH", toolchain_path()?)
        .env(HELD_ENV, RunClass::Build.name())
        .env("CARGO_BUILD_JOBS", "2")
        .env("CARGO_INCREMENTAL", "1")
        // The shared compiler cache rejects incremental compilation.
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("CARGO_TARGET_DIR", target);
    Ok(command)
}

/// Cargo and the binding generator need the real tools for their own subprocesses.
pub fn toolchain_path_from(path: &OsStr) -> io::Result<OsString> {
    let folders = std::env::split_paths(path).filter(|folder| !is_command_folder(folder));
    std::env::join_paths(folders).map_err(io::Error::other)
}

fn toolchain_path() -> io::Result<OsString> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    toolchain_path_from(&path)
}

/// One target folder per Rosen project, beside its workspaces: registry and git
/// dependencies build once, while switching workspaces invalidates local crate
/// fingerprints and build-script outputs so Cargo cannot reuse another workspace's
/// compiled crates (#527). Elsewhere the workspace keeps its own.
pub(crate) fn light_target(root: &Path) -> PathBuf {
    match root.parent() {
        Some(project) if root.join(".rosen").exists() => project_target(project),
        _ => root.join("target").join("agent-check"),
    }
}

/// The light runs' shared target folder for the Rosen project whose workspaces lie in
/// `project_root`.
pub fn project_target(project_root: &Path) -> PathBuf {
    project_root.join(PROJECT_TARGET)
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
    policy(program, &policy_arguments(&arguments[index + 1..])).map(|policy| (index, policy))
}

fn policy_arguments(arguments: &[OsString]) -> Vec<String> {
    arguments
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect()
}

/// Tries to take the first free machine-wide light slot without blocking.
pub(crate) fn try_light_slot() -> io::Result<Option<File>> {
    let folder = std::env::temp_dir();
    for slot in 0..LIGHT_SLOTS {
        let name = light_slot_name(slot);
        if let Some(file) = try_lock_file(&folder.join(name))? {
            return Ok(Some(file));
        }
    }
    Ok(None)
}

/// Counts how many machine-wide light Rust slots are currently held without blocking.
pub(crate) fn busy_light_slots() -> usize {
    let folder = std::env::temp_dir();
    let mut busy = 0;
    for slot in 0..LIGHT_SLOTS {
        if matches!(try_lock_file(&folder.join(light_slot_name(slot))), Ok(None)) {
            busy += 1;
        }
    }
    busy
}

fn light_slot_name(slot: usize) -> String {
    match slot {
        0 => "velocity-agent-rust-check.lock".to_owned(),
        _ => format!("velocity-agent-rust-check-{slot}.lock"),
    }
}

/// Takes the first free machine-wide light slot, waiting while all are taken. Slot 0
/// keeps the single slot's old name, so older shims still count against it.
fn light_slot() -> io::Result<File> {
    loop {
        if let Some(file) = try_light_slot()? {
            return Ok(file);
        }
        std::thread::sleep(POLL);
    }
}

pub(crate) fn try_lock_file(path: &Path) -> io::Result<Option<File>> {
    let file = lock_file(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
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

#[cfg(test)]
mod tests {
    use super::{AgentRun, policy};

    #[test]
    fn held_run_never_resolves_the_shim() -> std::io::Result<()> {
        // Risk (#548): a held light run that resolved `cargo` to the shim waited on its own
        // ancestor's light-run lock forever.
        let shim = tempfile::tempdir()?;
        let toolchain = tempfile::tempdir()?;
        let cargo = super::executable_file("cargo");
        std::fs::write(shim.path().join(&cargo), b"")?;
        std::fs::write(shim.path().join(super::APP_RECORD), b"")?;
        std::fs::write(toolchain.path().join(&cargo), b"")?;
        let path =
            std::env::join_paths([shim.path(), toolchain.path()]).map_err(std::io::Error::other)?;
        assert_eq!(
            super::real_program("cargo", &path),
            Some(toolchain.path().join(&cargo))
        );
        Ok(())
    }

    #[test]
    fn pnpm_bindings_uses_the_light_run_policy() {
        // Missing a bindings invocation puts a Rust build in the ordinary queue.
        let cases: &[(&[&str], AgentRun)] = &[
            (&["bindings"], AgentRun::Bindings),
            (&["-C", "ui", "bindings"], AgentRun::Bindings),
            (&["--dir", "ui", "bindings"], AgentRun::Bindings),
            (&["run", "bindings"], AgentRun::Bindings),
            (&["-C", "ui", "run", "bindings"], AgentRun::Bindings),
            (&["-C", "ui", "check"], AgentRun::Ordinary),
            (&["-C", "bindings"], AgentRun::Ordinary),
            (&["exec", "bindings"], AgentRun::Ordinary),
        ];
        for (arguments, expected) in cases {
            let arguments: Vec<_> = arguments
                .iter()
                .map(|&argument| argument.to_owned())
                .collect();
            assert!(
                policy("pnpm.cmd", &arguments) == Ok(*expected),
                "Unexpected run policy for {arguments:?}"
            );
        }
    }
}
