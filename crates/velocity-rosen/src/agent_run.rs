//! The agent shell, managed Rosen CLI and MCP queue share this policy. Light Rust runs
//! (single-crate checks, binding generation and Velocity's own bindings check before a
//! land) take one of a few machine-wide slots and start once their own memory fits above
//! Rosen's reserve.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::{OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use rosen_client::run_queue::{HELD_ENV, POLL, RunClass, RunQueue};
use rosen_client::snapshot::WorkspaceMarker;
use rosen_core::PathPattern;
use velocity_core::repair_tests::{RequiredTestsRecord, cargo_test_output_passed_at_least_one};

use crate::process::{background_command, hidden_command};
use crate::queued_turn::DEFAULT_WAIT_TIMEOUT;
use crate::registered_generator::{
    self, GeneratorExecution, RegisteredGenerator, begin_guard_blocking, finish_guard_blocking,
};

static AGENT_COMMANDS: OnceLock<Option<AgentCommands>> = OnceLock::new();
static PASSTHROUGH_ENV: RwLock<Vec<String>> = RwLock::new(Vec::new());
const APP_RECORD: &str = "velocity-program";
const ROSEN_RECORD: &str = "rosen-program";
const PASSTHROUGH_RECORD: &str = "passthrough-env";
const RUST_COMMANDS: &[&str] = &[
    "cargo",
    "rustc",
    "cargo-clippy",
    "cargo-tauri",
    "clippy-driver",
];
const VCS_COMMANDS: &[&str] = &["git", "gh"];
/// Set on Rosen agent processes so `git` and `gh` shims refuse immediately (#533).
pub const BLOCK_VCS_ENV: &str = "VELOCITY_BLOCK_GIT_GH";
const LAUNCHER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/agent-launcher.exe"));
const REFUSAL: &str = "The integration build covers Rust compilation, lint and tests. Agents may run cargo fmt, queue cargo check -p <crate>, or queue a generator registered under [generated.commands] in rosen.toml.";
const GIT_REFUSAL: &str = "Rosen manages this workspace, not git. Use the rosen MCP tools or CLI (rosen status, rosen diff, rosen log, rosen commit -m) instead.";
const GH_REFUSAL: &str = "Agents do not call gh directly. Use AIRC issue tools (issue_read, issue_create, issue_comment, issue_ready_to_close) or velocity-run's workflow_run instead.";
/// Light runs going at once on the machine. Each uses two Cargo jobs.
pub(crate) const LIGHT_SLOTS: usize = 2;
/// What a light run may use: two Cargo jobs plus the linker of a debug build.
pub(crate) const LIGHT_MEMORY: u64 = 3 << 30;
/// The shared target folder prefix beside a Rosen project's workspaces.
const PROJECT_TARGET: &str = ".velocity-agent-target";
/// Per-target lock file held while a light Rust run uses a pooled target directory.
pub(crate) const LIGHT_RUN_LOCK: &str = ".velocity-light-run.lock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRun {
    Ordinary,
    /// Rust formatting or the managed Cargo.lock generator.
    RustUtility,
    /// `cargo check -p <crate>`.
    RustCheck,
    /// The UI's `bindings` script or managed `cargo run ... --bin export-bindings`.
    Bindings,
    /// A command registered under `[generated.commands]` in the base's `rosen.toml`.
    RegisteredGenerator,
}

pub fn policy(program: &str, arguments: &[String]) -> Result<AgentRun, &'static str> {
    policy_with_vcs_blocked(program, arguments, vcs_blocked_in_process())
}

/// Classifies a queued or managed command against workspace-scoped required repair tests (#741)
/// and registered `[generated.commands]` in the base's `rosen.toml` (#582), then falls back
/// to Velocity's Rust and VCS policy.
pub fn policy_with_workspace_overrides(
    program: &str,
    arguments: &[String],
    block_vcs: bool,
    at_workspace_root: bool,
    generated_commands: &BTreeMap<PathPattern, String>,
    required_tests: Option<&RequiredTestsRecord>,
    ambient_env: &dyn Fn(&str) -> Option<String>,
) -> Result<(AgentRun, Option<RegisteredGenerator>), &'static str> {
    if block_vcs && let Some(refusal) = direct_or_wrapped_vcs_refusal(program, arguments) {
        return Err(refusal);
    }
    if at_workspace_root
        && executable_name(program) == "cargo"
        && required_tests.is_some_and(|record| record.allows_cargo_test(arguments))
    {
        return Ok((AgentRun::RustCheck, None));
    }
    policy_with_registered_generators(
        program,
        arguments,
        block_vcs,
        at_workspace_root,
        generated_commands,
        ambient_env,
    )
}

/// Classifies a queued or managed command against registered `[generated.commands]` in the
/// base's `rosen.toml` first, then falls back to Velocity's Rust and VCS policy (#582).
pub fn policy_with_registered_generators(
    program: &str,
    arguments: &[String],
    block_vcs: bool,
    at_workspace_root: bool,
    generated_commands: &BTreeMap<PathPattern, String>,
    ambient_env: &dyn Fn(&str) -> Option<String>,
) -> Result<(AgentRun, Option<RegisteredGenerator>), &'static str> {
    if block_vcs && let Some(refusal) = direct_or_wrapped_vcs_refusal(program, arguments) {
        return Err(refusal);
    }
    if at_workspace_root
        && let Some(matched) = registered_generator::match_registered(
            generated_commands,
            program,
            arguments,
            ambient_env,
        )
    {
        return Ok((AgentRun::RegisteredGenerator, Some(matched)));
    }
    policy_with_vcs_blocked(program, arguments, block_vcs).map(|run| (run, None))
}

fn direct_or_wrapped_vcs_refusal(program: &str, arguments: &[String]) -> Option<&'static str> {
    if let Some(refusal) = vcs_refusal_for(&executable_name(program)) {
        return Some(refusal);
    }
    let unwrapped = registered_generator::unwrap_env_prefix(program, arguments)?;
    vcs_refusal_for(&executable_name(unwrapped.program))
}

/// Classifies a queued or managed command against Velocity's Rust and VCS policy.
/// Callers running inside Velocity itself (such as `velocity-run`'s queue host) pass
/// `block_vcs` for the owning session rather than reading Velocity's own process env (#563).
pub fn policy_with_vcs_blocked(
    program: &str,
    arguments: &[String],
    block_vcs: bool,
) -> Result<AgentRun, &'static str> {
    let name = executable_name(program);
    if block_vcs && let Some(refusal) = direct_or_wrapped_vcs_refusal(program, arguments) {
        return Err(refusal);
    }
    if let Some(unwrapped) = registered_generator::unwrap_env_prefix(program, arguments) {
        let wrapped_name = executable_name(unwrapped.program);
        if RUST_COMMANDS.contains(&wrapped_name.as_str())
            || wrapped_name == "stress"
            || (wrapped_name == "rustup"
                && unwrapped.arguments.first().is_some_and(|arg| arg == "run"))
            || wrapped_name == "rosen"
        {
            return Err(REFUSAL);
        }
    }
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
        // Queued `pnpm -C ui bindings` invokes `cargo run ... --bin export-bindings` (#559).
        _ if managed && runs_export_bindings(arguments) => Ok(AgentRun::Bindings),
        _ => Err(REFUSAL),
    }
}

/// `cargo run [--manifest-path <path>] [-p|--package <crate>] [--locked] --bin export-bindings [-- [--check]]`.
fn runs_export_bindings(arguments: &[String]) -> bool {
    let Some((command, mut rest)) = arguments.split_first() else {
        return false;
    };
    if command != "run" {
        return false;
    }
    let mut has_bin = false;
    while let Some((first, tail)) = rest.split_first() {
        if first == "--" {
            return has_bin && (tail.is_empty() || matches!(tail, [flag] if flag == "--check"));
        }
        if first == "--locked" {
            rest = tail;
        } else if let Some((path, next)) = flag_value(first, tail, &["--manifest-path"]) {
            if path.is_empty() || path.starts_with('-') {
                return false;
            }
            rest = next;
        } else if let Some((package, next)) = flag_value(first, tail, &["-p", "--package"]) {
            if !package_name(package) {
                return false;
            }
            rest = next;
        } else if let Some((binary, next)) = flag_value(first, tail, &["--bin"]) {
            if binary != "export-bindings" {
                return false;
            }
            has_bin = true;
            rest = next;
        } else {
            return false;
        }
    }
    has_bin
}

fn flag_value<'a>(
    first: &'a str,
    tail: &'a [String],
    names: &[&str],
) -> Option<(&'a str, &'a [String])> {
    for &name in names {
        if first == name {
            let (value, next) = tail.split_first()?;
            return Some((value.as_str(), next));
        }
        if !name.starts_with("--") {
            continue;
        }
        if let Some(value) = first
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return Some((value, tail));
        }
    }
    None
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

pub(crate) fn executable_name(program: &str) -> String {
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
            .inspect(|commands| crate::lean_cache::install_lake_shim(&commands.folder))
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
    for name in VCS_COMMANDS {
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
    let passthrough = in_process_passthrough();
    if !passthrough.is_empty() {
        let _ = std::fs::write(path.join(PASSTHROUGH_RECORD), passthrough.join("\n"));
    }
    Ok(AgentCommands {
        folder: folder.keep(),
        rosen,
    })
}

/// Updates the owner-approved pass-through variable names used by agent shims and
/// Velocity's own light checks (#530).
pub fn set_passthrough_env(names: &[String]) {
    let normalized = velocity_core::agent_env::normalize_passthrough(names).unwrap_or_default();
    if let Some(folder) = command_folder() {
        let _ = std::fs::write(folder.join(PASSTHROUGH_RECORD), normalized.join("\n"));
    }
    *PASSTHROUGH_ENV
        .write()
        .unwrap_or_else(PoisonError::into_inner) = normalized;
}

fn in_process_passthrough() -> Vec<String> {
    PASSTHROUGH_ENV
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn saved_passthrough(folder: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(folder.join(PASSTHROUGH_RECORD)) else {
        return Vec::new();
    };
    let names: Vec<String> = text.lines().map(str::to_owned).collect();
    velocity_core::agent_env::normalize_passthrough(&names).unwrap_or_default()
}

pub(crate) fn filter_environment(command: &mut Command, passthrough: &[String]) {
    for (os_name, _) in std::env::vars_os() {
        if velocity_core::agent_env::should_withhold(&os_name.to_string_lossy(), passthrough) {
            command.env_remove(&os_name);
        }
    }
}

pub(crate) fn executable_file(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

pub(crate) fn copy_launcher(source: &Path, destination: &Path) -> io::Result<()> {
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
    let passthrough = saved_passthrough(folder);
    let result = if name == "rosen" {
        rosen_program(folder).and_then(|program| run_rosen(&program, &arguments, &passthrough))
    } else if name == "lake" {
        crate::lean_cache::run_lake(&arguments, &passthrough)
    } else {
        run_direct(folder, &name, &arguments, &passthrough)
    };
    Some((name, result))
}

fn rosen_program(folder: &Path) -> io::Result<PathBuf> {
    std::fs::read_to_string(folder.join(ROSEN_RECORD)).map(PathBuf::from)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectAction {
    Held,
    Utility,
    Light,
}

fn direct_action(arguments: &[OsString], held: bool) -> Result<DirectAction, &'static str> {
    match cargo_policy(&policy_arguments(arguments), held)? {
        AgentRun::RustUtility | AgentRun::RustCheck | AgentRun::Bindings if held => {
            Ok(DirectAction::Held)
        }
        AgentRun::RustUtility => Ok(DirectAction::Utility),
        AgentRun::RustCheck => Ok(DirectAction::Light),
        _ => Err(REFUSAL),
    }
}

fn run_direct(
    folder: &Path,
    name: &str,
    arguments: &[OsString],
    passthrough: &[String],
) -> io::Result<ExitStatus> {
    if VCS_COMMANDS.contains(&name) {
        if let Some(refusal) = vcs_refusal(name) {
            return Err(io::Error::other(refusal));
        }
        return run_held(name, arguments, passthrough);
    }
    if name != "cargo" {
        return Err(io::Error::other(REFUSAL));
    }
    if !inside_held_turn()
        && let Some(matched) = direct_required_test(arguments)
    {
        return run_required_test(&rosen_program(folder)?, &[], &matched, passthrough, false);
    }
    match direct_action(arguments, inside_held_turn()).map_err(io::Error::other)? {
        DirectAction::Held => run_held(name, arguments, passthrough),
        DirectAction::Utility => run_utility(OsStr::new(name), arguments, passthrough),
        DirectAction::Light => run_light(
            &rosen_program(folder)?,
            &[],
            OsStr::new(name),
            arguments,
            passthrough,
        ),
    }
}

/// Returns the one-line refusal for `git` or `gh` when running inside a Rosen workspace or
/// Rosen agent environment (#533), or `None` outside Rosen so legacy git projects keep working.
pub fn vcs_refusal(name: &str) -> Option<&'static str> {
    if !VCS_COMMANDS.contains(&name) || !vcs_blocked_in_process() {
        return None;
    }
    vcs_refusal_for(name)
}

fn vcs_blocked_in_process() -> bool {
    std::env::var_os(BLOCK_VCS_ENV).is_some_and(|value| value == "1")
        || std::env::var_os("ROSEN_CHANGE").is_some()
        || std::env::var_os("ROSEN_ENDPOINT").is_some()
        || std::env::current_dir().ok().is_some_and(|directory| {
            directory
                .ancestors()
                .any(|path| path.join(".rosen").exists())
        })
}

fn vcs_refusal_for(name: &str) -> Option<&'static str> {
    match name {
        "git" => Some(GIT_REFUSAL),
        "gh" => Some(GH_REFUSAL),
        _ => None,
    }
}

fn run_rosen(
    program: &Path,
    arguments: &[OsString],
    passthrough: &[String],
) -> io::Result<ExitStatus> {
    let subcommand = subcommand_index(arguments);
    if arguments.get(subcommand).is_some_and(|arg| arg == "commit") {
        crate::preflight::verify_cli_commit(arguments, subcommand)?;
    }
    if !arguments.get(subcommand).is_some_and(|arg| arg == "run") {
        let mut command = hidden_command(program);
        filter_environment(&mut command, passthrough);
        return command.args(arguments).status();
    }
    let run_arguments = &arguments[subcommand + 1..];
    let classified = classify_run(arguments, run_arguments).map_err(io::Error::other)?;
    if let Some(generator) = classified.registered {
        return run_registered_generator(
            program,
            &arguments[..subcommand],
            &generator,
            passthrough,
            inside_held_turn(),
        );
    }
    if let Some(required_test) = classified.required_test {
        return run_required_test(
            program,
            &arguments[..subcommand],
            &required_test,
            passthrough,
            inside_held_turn(),
        );
    }
    let (program_at, run) = (classified.program_at, classified.run);
    if run != AgentRun::Ordinary && inside_held_turn() {
        // `rosen run` sees the held turn and starts at once (#548).
        let mut command = hidden_command(program);
        filter_environment(&mut command, passthrough);
        return command
            .args(arguments)
            .env("PATH", toolchain_path()?)
            .env_remove(BLOCK_VCS_ENV)
            .status();
    }
    match run {
        AgentRun::Ordinary => {
            let path = std::env::var_os("PATH").unwrap_or_default();
            ordinary_command(
                program,
                arguments,
                run_arguments.get(program_at),
                &path,
                passthrough,
            )?
            .status()
        }
        AgentRun::RustUtility => run_utility(
            &run_arguments[program_at],
            &run_arguments[program_at + 1..],
            passthrough,
        ),
        AgentRun::RustCheck | AgentRun::Bindings | AgentRun::RegisteredGenerator => run_light(
            program,
            &arguments[..subcommand],
            &run_arguments[program_at],
            &run_arguments[program_at + 1..],
            passthrough,
        ),
    }
}

struct ClassifiedGenerator {
    endpoint: String,
    workspace_root: PathBuf,
    generator: RegisteredGenerator,
}

struct ClassifiedRequiredTest {
    endpoint: String,
    workspace_root: PathBuf,
    arguments: Vec<String>,
}

struct ClassifiedRun {
    program_at: usize,
    run: AgentRun,
    registered: Option<ClassifiedGenerator>,
    required_test: Option<ClassifiedRequiredTest>,
}

fn direct_required_test(arguments: &[OsString]) -> Option<ClassifiedRequiredTest> {
    if arguments.first().and_then(|value| value.to_str()) != Some("test") {
        return None;
    }
    let (endpoint, workspace_root) = run_workspace_context(&[])?;
    let policy_args = policy_arguments(arguments);
    let record =
        crate::preflight::load_required_tests_for_workspace_blocking(&endpoint, &workspace_root)?;
    record
        .allows_cargo_test(&policy_args)
        .then_some(ClassifiedRequiredTest {
            endpoint,
            workspace_root,
            arguments: policy_args,
        })
}

fn run_required_test(
    rosen: &Path,
    rosen_arguments: &[OsString],
    matched: &ClassifiedRequiredTest,
    passthrough: &[String],
    held: bool,
) -> io::Result<ExitStatus> {
    let head_before = crate::preflight::snapshot_workspace_head_blocking(
        &matched.endpoint,
        &matched.workspace_root,
    )
    .ok()
    .flatten()
    .map(|(_, head)| head);
    let (status, test_results) = if held {
        execute_required_test_held(&matched.arguments, passthrough)?
    } else {
        execute_required_test_light(
            rosen,
            rosen_arguments,
            &matched.workspace_root,
            &matched.arguments,
            passthrough,
        )?
    };
    if status.success() {
        if !cargo_test_output_passed_at_least_one(&test_results) {
            return Err(io::Error::other(
                "Targeted repair test ran 0 tests — do not skip, delete, or rename the failing test.",
            ));
        }
        if let Some(head) = head_before {
            let _ = crate::preflight::record_required_test_pass_blocking(
                &matched.endpoint,
                &matched.workspace_root,
                &head,
                &matched.arguments,
            );
        }
    }
    Ok(status)
}

fn execute_required_test_light(
    rosen: &Path,
    rosen_arguments: &[OsString],
    workspace_root: &Path,
    arguments: &[String],
    passthrough: &[String],
) -> io::Result<(ExitStatus, String)> {
    let scope = cli_endpoint_for_run(rosen_arguments).and_then(|endpoint| {
        crate::light_target::inspect_workspace_scope_blocking(&endpoint, workspace_root)
    });
    let targets = light_targets(workspace_root);
    let turn = light_turn(&targets, workspace_root, workspace_root, scope.as_ref())?;
    let mut command = light_command(
        rosen,
        rosen_arguments,
        OsStr::new("cargo"),
        turn.target(),
        passthrough,
    )?;
    command.args(arguments);
    run_capturing_test_results(command)
}

fn execute_required_test_held(
    arguments: &[String],
    passthrough: &[String],
) -> io::Result<(ExitStatus, String)> {
    let path = toolchain_path()?;
    let program = real_program("cargo", &path).ok_or_else(|| {
        io::Error::other("Could not find cargo outside Velocity's managed commands")
    })?;
    let mut command = background_command(program);
    filter_environment(&mut command, passthrough);
    command
        .args(arguments)
        .env("PATH", path)
        .env_remove(BLOCK_VCS_ENV);
    run_capturing_test_results(command)
}

fn run_capturing_test_results(mut command: Command) -> io::Result<(ExitStatus, String)> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_thread = std::thread::spawn(move || forward_and_collect_results(stdout, true));
    let err_thread = std::thread::spawn(move || forward_and_collect_results(stderr, false));
    let status = child.wait()?;
    let mut results = out_thread.join().unwrap_or_default();
    let err_results = err_thread.join().unwrap_or_default();
    if !err_results.is_empty() {
        if !results.is_empty() {
            results.push('\n');
        }
        results.push_str(&err_results);
    }
    Ok((status, results))
}

fn forward_and_collect_results<R: io::Read>(stream: Option<R>, to_stdout: bool) -> String {
    use io::{BufRead, Write};

    let Some(stream) = stream else {
        return String::new();
    };
    let mut reader = io::BufReader::new(stream);
    let mut line = Vec::new();
    let mut results = String::new();
    while reader.read_until(b'\n', &mut line).unwrap_or(0) > 0 {
        if to_stdout {
            let _ = io::stdout().write_all(&line);
            let _ = io::stdout().flush();
        } else {
            let _ = io::stderr().write_all(&line);
            let _ = io::stderr().flush();
        }
        let text = String::from_utf8_lossy(&line);
        if text.contains("test result:") {
            results.push_str(&text);
        }
        line.clear();
    }
    results
}

fn run_registered_generator(
    rosen: &Path,
    rosen_arguments: &[OsString],
    matched: &ClassifiedGenerator,
    passthrough: &[String],
    held: bool,
) -> io::Result<ExitStatus> {
    let guard = begin_guard_blocking(
        &matched.endpoint,
        &matched.workspace_root,
        &matched.generator,
    )
    .map_err(io::Error::other)?;
    let status = if held {
        execute_generator_held(&matched.generator.execution, passthrough)
    } else {
        execute_generator_light(
            rosen,
            rosen_arguments,
            &matched.workspace_root,
            &matched.generator.execution,
            passthrough,
        )
    };
    let succeeded = status.as_ref().is_ok_and(ExitStatus::success);
    finish_guard_blocking(guard, succeeded).map_err(io::Error::other)?;
    status
}

fn execute_generator_light(
    rosen: &Path,
    rosen_arguments: &[OsString],
    workspace_root: &Path,
    execution: &GeneratorExecution,
    passthrough: &[String],
) -> io::Result<ExitStatus> {
    let targets = light_targets(workspace_root);
    let turn = light_turn(&targets, workspace_root, workspace_root, None)?;
    let target = turn.target();
    match execution {
        GeneratorExecution::Direct {
            env,
            program,
            arguments,
        } => {
            let mut command = light_command(
                rosen,
                rosen_arguments,
                OsStr::new(program),
                target,
                passthrough,
            )?;
            for (key, value) in env {
                command.env(key, value);
            }
            command.args(arguments).status()
        }
        GeneratorExecution::Shell(script) => {
            let (shell, flag) = generator_shell();
            light_command(
                rosen,
                rosen_arguments,
                OsStr::new(shell),
                target,
                passthrough,
            )?
            .args(flag)
            .arg(script)
            .status()
        }
    }
}

fn execute_generator_held(
    execution: &GeneratorExecution,
    passthrough: &[String],
) -> io::Result<ExitStatus> {
    let path = toolchain_path()?;
    match execution {
        GeneratorExecution::Direct {
            env,
            program,
            arguments,
        } => {
            let resolved = resolve_utility_program(OsStr::new(program), &path);
            let mut command = background_command(resolved);
            filter_environment(&mut command, passthrough);
            for (key, value) in env {
                command.env(key, value);
            }
            command
                .args(arguments)
                .env("PATH", path)
                .env_remove(BLOCK_VCS_ENV)
                .status()
        }
        GeneratorExecution::Shell(script) => {
            let (shell, flag) = generator_shell();
            let mut command = background_command(shell);
            filter_environment(&mut command, passthrough);
            command
                .args(flag)
                .arg(script)
                .env("PATH", path)
                .env_remove(BLOCK_VCS_ENV)
                .status()
        }
    }
}

fn generator_shell() -> (&'static str, &'static [&'static str]) {
    if cfg!(windows) {
        ("cmd", &["/D", "/C"])
    } else {
        ("sh", &["-c"])
    }
}

fn ordinary_command(
    program: &Path,
    arguments: &[OsString],
    target_program: Option<&OsString>,
    path: &OsStr,
    passthrough: &[String],
) -> io::Result<Command> {
    let is_lake_run = target_program
        .and_then(|value| value.to_str())
        .is_some_and(crate::lean_cache::is_lake);
    let mut command = hidden_command(program);
    filter_environment(&mut command, passthrough);
    command.args(arguments).env_remove(HELD_ENV);
    if !is_lake_run {
        // Drop Velocity's agent shims once the top-level command passed policy so child
        // tests and tools (e.g. Go tests invoking `git` in temp dirs or
        // `git check-ref-format`) reach host tools (#563). `lake` keeps its shim so
        // `lean_cache::run_lake` unpacks Mathlib inside the turn before stripping shims.
        command
            .env("PATH", toolchain_path_from(path)?)
            .env_remove(BLOCK_VCS_ENV);
    }
    Ok(command)
}

/// Whether this process already runs inside a turn that Velocity or `rosen run` holds. A
/// queued light run holds its project's light-run lock and a machine slot; a managed
/// command under it that waited for its own turn would wait on that ancestor forever (#548).
fn inside_held_turn() -> bool {
    std::env::var_os(HELD_ENV).is_some()
}

/// Runs the real `name` inside a held turn. It is resolved here, skipping managed command
/// folders, so a `PATH` that still names this shim's folder cannot start the shim again.
fn run_held(name: &str, arguments: &[OsString], passthrough: &[String]) -> io::Result<ExitStatus> {
    let path = toolchain_path()?;
    let program = real_program(name, &path).ok_or_else(|| {
        io::Error::other(format!(
            "Could not find {name} outside Velocity's managed commands"
        ))
    })?;
    let mut command = background_command(program);
    filter_environment(&mut command, passthrough);
    command
        .args(arguments)
        .env("PATH", path)
        .env_remove(BLOCK_VCS_ENV)
        .status()
}

/// The first `name` executable on `path` outside a managed command folder, falling back
/// to Cargo's bin folder for Rust tools when `PATH` omits it.
pub(crate) fn real_program(name: &str, path: &OsStr) -> Option<PathBuf> {
    let file = executable_file(name);
    std::env::split_paths(path)
        .filter(|folder| !is_command_folder(folder))
        .map(|folder| folder.join(&file))
        .find(|candidate| candidate.is_file())
        .or_else(|| cargo_bin_program(name, &file))
}

fn cargo_bin_program(name: &str, file: &str) -> Option<PathBuf> {
    if !RUST_COMMANDS.contains(&name) {
        return None;
    }
    let bin_folder = std::env::var_os("CARGO_HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join("bin"))
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".cargo").join("bin"))
        })?;
    if is_command_folder(&bin_folder) {
        return None;
    }
    let candidate = bin_folder.join(file);
    candidate.is_file().then_some(candidate)
}

fn run_utility(
    program: &OsStr,
    arguments: &[OsString],
    passthrough: &[String],
) -> io::Result<ExitStatus> {
    let _slot = light_slot(Instant::now(), DEFAULT_WAIT_TIMEOUT)?;
    let path = toolchain_path()?;
    let resolved = resolve_utility_program(program, &path);
    let mut command = background_command(resolved);
    filter_environment(&mut command, passthrough);
    command
        .args(arguments)
        .env("PATH", path)
        .env_remove(BLOCK_VCS_ENV)
        .status()
}

fn resolve_utility_program(program: &OsStr, path: &OsStr) -> PathBuf {
    let candidate = Path::new(program);
    if candidate.components().count() == 1
        && let Some(text) = program.to_str()
        && let Some(found) = real_program(&executable_name(text), path)
    {
        return found;
    }
    candidate.to_path_buf()
}

fn run_light(
    rosen: &Path,
    rosen_arguments: &[OsString],
    program: &OsStr,
    arguments: &[OsString],
    passthrough: &[String],
) -> io::Result<ExitStatus> {
    let directory = std::env::current_dir()?;
    let root = directory
        .ancestors()
        .find(|path| path.join(".rosen").exists() || path.join(".git").exists())
        .unwrap_or(&directory);
    let scope = cli_endpoint_for_run(rosen_arguments).and_then(|endpoint| {
        crate::light_target::inspect_workspace_scope_blocking(&endpoint, root)
    });
    let targets = light_targets(root);
    let turn = light_turn(&targets, root, &directory, scope.as_ref())?;
    light_command(rosen, rosen_arguments, program, turn.target(), passthrough)?
        .args(arguments)
        .status()
}

/// Velocity's own light run (#503): `program` in `directory`, building into the first free
/// target folder in `targets` (preferring one matching `directory`), with what it printed.
/// It waits and runs like an agent's (blocking).
pub(crate) fn light_output(
    directory: &Path,
    targets: &[PathBuf],
    program: &str,
    arguments: &[&str],
) -> io::Result<Output> {
    let rosen = crate::server::program_folder("rosen")
        .map(|folder| folder.join(executable_file("rosen")))
        .ok_or_else(|| io::Error::other("the rosen program was not found"))?;
    let turn = light_turn(targets, directory, directory, None)?;
    let passthrough = in_process_passthrough();
    light_command(
        &rosen,
        &[],
        OsStr::new(program),
        turn.target(),
        &passthrough,
    )?
    .args(arguments)
    .current_dir(directory)
    .stdin(Stdio::null())
    .output()
}

/// What a light run holds until it ends: one of its project's target folders and a machine slot.
struct LightTurn {
    _project: File,
    _slot: File,
    target: PathBuf,
}

impl LightTurn {
    fn target(&self) -> &Path {
        &self.target
    }
}

/// Waits for a free target folder in `targets` (preferring one warm for `workspace_root`),
/// a slot and memory, in that order, then prepares that target folder for `workspace_root`.
fn light_turn(
    targets: &[PathBuf],
    workspace_root: &Path,
    directory: &Path,
    scope: Option<&crate::light_target::WorkspaceScope>,
) -> io::Result<LightTurn> {
    let started = Instant::now();
    let identity = crate::light_target::workspace_identity(workspace_root);
    // Cargo locks a target folder for a whole build, so runs that share one wait here
    // rather than in a machine slot.
    let (target, project) =
        lock_light_target_with_timeout(targets, &identity, started, DEFAULT_WAIT_TIMEOUT)?;
    let slot = light_slot(started, DEFAULT_WAIT_TIMEOUT)?;
    wait_for_memory(started, DEFAULT_WAIT_TIMEOUT)?;
    crate::light_target::prepare_with_scope(&target, workspace_root, directory, scope)?;
    Ok(LightTurn {
        _project: project,
        _slot: slot,
        target,
    })
}

/// `rosen run -- <program>` set up as a light run; the caller adds the arguments.
fn light_command(
    rosen: &Path,
    rosen_arguments: &[OsString],
    program: &OsStr,
    target: &Path,
    passthrough: &[String],
) -> io::Result<Command> {
    // No Rosen turn: its admission charges every running turn its full cap (6 GiB for
    // an integration build), which kept two-job checks waiting while gigabytes sat free.
    // A light run starts when its own memory fits above Rosen's reserve; the run below
    // still executes in a job capped at a build turn's memory.
    let mut command = background_command(rosen);
    filter_environment(&mut command, passthrough);
    command
        .args(rosen_arguments)
        .args(["run", "--class", RunClass::Build.name(), "--"])
        .arg(program)
        .env("PATH", toolchain_path()?)
        .env_remove(BLOCK_VCS_ENV)
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

pub(crate) fn toolchain_path() -> io::Result<OsString> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    toolchain_path_from(&path)
}

/// Target folder pool for a light run in `root`: up to `LIGHT_SLOTS` pooled folders beside
/// a Rosen project's workspaces so concurrent agents on the same project can run in
/// parallel while keeping registry and git dependencies warm and invalidating local crate
/// fingerprints only when a folder switches workspaces (#527, #682). Elsewhere the
/// workspace keeps its own single target folder.
pub(crate) fn light_targets(root: &Path) -> Vec<PathBuf> {
    match root.parent() {
        Some(project) if root.join(".rosen").exists() => project_targets(project),
        _ => vec![root.join("target").join("agent-check")],
    }
}

/// The primary shared light-run target folder for the Rosen project whose workspaces lie
/// in `project_root`.
pub fn project_target(project_root: &Path) -> PathBuf {
    project_root.join(PROJECT_TARGET)
}

/// The pool of up to `LIGHT_SLOTS` shared light-run target folders beside a Rosen
/// project's workspaces (#682). Slot 0 keeps `.velocity-agent-target`.
pub fn project_targets(project_root: &Path) -> Vec<PathBuf> {
    (0..LIGHT_SLOTS)
        .map(|slot| project_root.join(project_target_name(slot)))
        .collect()
}

fn project_target_name(slot: usize) -> String {
    match slot {
        0 => PROJECT_TARGET.to_owned(),
        _ => format!("{PROJECT_TARGET}-{slot}"),
    }
}

fn wait_for_memory(started: Instant, timeout: Duration) -> io::Result<()> {
    let queue = RunQueue::machine()?;
    wait_for_light_resource(
        started,
        timeout,
        "waiting for 3.0 GiB memory headroom above Rosen's reserve",
        || Ok(queue.memory_headroom().fits(LIGHT_MEMORY).then_some(())),
    )
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

fn classify_run(
    all_arguments: &[OsString],
    arguments: &[OsString],
) -> Result<ClassifiedRun, &'static str> {
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
                return Ok(ClassifiedRun {
                    program_at: index,
                    run: AgentRun::Ordinary,
                    registered: None,
                    required_test: None,
                });
            }
            _ => break,
        }
    }
    let Some(program) = arguments.get(index).and_then(|value| value.to_str()) else {
        return Ok(ClassifiedRun {
            program_at: index,
            run: AgentRun::Ordinary,
            registered: None,
            required_test: None,
        });
    };
    let policy_args = policy_arguments(&arguments[index + 1..]);
    let block_vcs = vcs_blocked_in_process();
    if let Some((endpoint, workspace_root)) = run_workspace_context(all_arguments) {
        let required_tests = if executable_name(program) == "cargo"
            && policy_args.first().map(String::as_str) == Some("test")
        {
            crate::preflight::load_required_tests_for_workspace_blocking(&endpoint, &workspace_root)
        } else {
            None
        };
        let is_required_test = required_tests
            .as_ref()
            .is_some_and(|record| record.allows_cargo_test(&policy_args));
        let commands =
            registered_generator::load_base_commands_blocking(&endpoint).unwrap_or_default();
        let (run, matched) = policy_with_workspace_overrides(
            program,
            &policy_args,
            block_vcs,
            true,
            &commands,
            required_tests.as_ref(),
            &|key| std::env::var(key).ok(),
        )?;
        let required_test = is_required_test.then_some(ClassifiedRequiredTest {
            endpoint: endpoint.clone(),
            workspace_root: workspace_root.clone(),
            arguments: policy_args,
        });
        return Ok(ClassifiedRun {
            program_at: index,
            run,
            registered: matched.map(|generator| ClassifiedGenerator {
                endpoint,
                workspace_root,
                generator,
            }),
            required_test,
        });
    }
    policy_with_vcs_blocked(program, &policy_args, block_vcs).map(|run| ClassifiedRun {
        program_at: index,
        run,
        registered: None,
        required_test: None,
    })
}

fn run_workspace_context(arguments: &[OsString]) -> Option<(String, PathBuf)> {
    let endpoint = cli_endpoint_for_run(arguments)?;
    let cwd = std::env::current_dir().ok()?;
    let root = WorkspaceMarker::find_root(&cwd)?;
    registered_generator::is_workspace_root(&cwd, &root).then_some((endpoint, root))
}

fn cli_endpoint_for_run(arguments: &[OsString]) -> Option<String> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).and_then(|value| value.to_str()) {
        if argument == "--" {
            break;
        }
        if argument == "--endpoint" {
            return arguments
                .get(index + 1)
                .and_then(|value| value.to_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned);
        }
        if let Some(value) = argument.strip_prefix("--endpoint=")
            && !value.trim().is_empty()
        {
            return Some(value.to_owned());
        }
        index += 1;
    }
    std::env::var("ROSEN_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn policy_arguments(arguments: &[OsString]) -> Vec<String> {
    arguments
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect()
}

/// Tries to take the first free machine-wide light slot without blocking.
pub(crate) fn try_light_slot() -> io::Result<Option<File>> {
    try_light_slot_in(&std::env::temp_dir())
}

fn try_light_slot_in(folder: &Path) -> io::Result<Option<File>> {
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

/// Takes the first free machine-wide light slot, timing out instead of blocking forever.
/// Slot 0 keeps the single slot's old name, so older shims still count against it (#548).
fn light_slot(started: Instant, timeout: Duration) -> io::Result<File> {
    light_slot_in(&std::env::temp_dir(), started, timeout)
}

fn light_slot_in(folder: &Path, started: Instant, timeout: Duration) -> io::Result<File> {
    wait_for_light_resource(
        started,
        timeout,
        &format!("waiting for a free light Rust slot ({LIGHT_SLOTS} slots)"),
        || try_light_slot_in(folder),
    )
}

pub(crate) fn try_lock_file(path: &Path) -> io::Result<Option<File>> {
    let file = lock_file(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Ranks `targets` by workspace affinity (matching `.velocity-last-workspace` first, then
/// unclaimed, then least recently used) and locks the first free target without blocking
/// (#682).
pub(crate) fn try_lock_light_target(
    targets: &[PathBuf],
    workspace_identity: &str,
) -> io::Result<Option<(PathBuf, File)>> {
    let ranked = crate::light_target::rank_targets(targets, workspace_identity);
    for target in ranked {
        std::fs::create_dir_all(&target)?;
        if let Some(lock) = try_lock_file(&target.join(LIGHT_RUN_LOCK))? {
            return Ok(Some((target, lock)));
        }
    }
    Ok(None)
}

/// Counts how many target directories in `targets` currently have a held light-run lock
/// without blocking or creating missing folders.
pub(crate) fn busy_light_targets(targets: &[PathBuf]) -> usize {
    targets
        .iter()
        .filter(|target| {
            let lock = target.join(LIGHT_RUN_LOCK);
            lock.exists() && matches!(try_lock_file(&lock), Ok(None))
        })
        .count()
}

pub(crate) fn light_target_wait_reason(targets: &[PathBuf]) -> String {
    match targets {
        [single] => format!(
            "waiting for the project's shared Rust target lock ({})",
            single.join(LIGHT_RUN_LOCK).display()
        ),
        _ => format!(
            "waiting for a free project shared Rust target lock ({} targets)",
            targets.len()
        ),
    }
}

fn lock_light_target_with_timeout(
    targets: &[PathBuf],
    workspace_identity: &str,
    started: Instant,
    timeout: Duration,
) -> io::Result<(PathBuf, File)> {
    if targets.is_empty() {
        return Err(io::Error::other(
            "no light Rust target directory configured",
        ));
    }
    let reason = light_target_wait_reason(targets);
    wait_for_light_resource(started, timeout, &reason, || {
        try_lock_light_target(targets, workspace_identity)
    })
}

fn wait_for_light_resource<T>(
    started: Instant,
    timeout: Duration,
    reason: &str,
    mut attempt: impl FnMut() -> io::Result<Option<T>>,
) -> io::Result<T> {
    loop {
        if let Some(value) = attempt()? {
            return Ok(value);
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "Timed out after {} {reason}. Another light Rust run may still hold it.",
                    format_wait_duration(elapsed)
                ),
            ));
        }
        let remaining = timeout.saturating_sub(elapsed);
        std::thread::sleep(POLL.min(remaining));
    }
}

fn format_wait_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else if seconds > 0 {
        format!("{seconds}s")
    } else {
        format!("{}ms", duration.as_millis())
    }
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
    fn held_turn_bypasses_light_locks() {
        // Risk (#548): a cargo shim reached inside a held turn must never take a light
        // lock its own ancestor already holds, while still refusing disallowed commands.
        let check = ["check", "-p", "velocity-app"].map(std::ffi::OsString::from);
        let fmt = ["fmt", "--check"].map(std::ffi::OsString::from);
        let metadata = ["metadata", "--format-version", "1"].map(std::ffi::OsString::from);
        let test = ["test"].map(std::ffi::OsString::from);
        assert_eq!(
            super::direct_action(&check, true),
            Ok(super::DirectAction::Held)
        );
        assert_eq!(
            super::direct_action(&fmt, true),
            Ok(super::DirectAction::Held)
        );
        assert_eq!(
            super::direct_action(&metadata, true),
            Ok(super::DirectAction::Held)
        );
        assert!(super::direct_action(&test, true).is_err());
        assert_eq!(
            super::direct_action(&check, false),
            Ok(super::DirectAction::Light)
        );
        assert_eq!(
            super::direct_action(&fmt, false),
            Ok(super::DirectAction::Utility)
        );
    }

    #[test]
    fn locked_times_out_when_all_pooled_targets_are_held() -> std::io::Result<()> {
        // Risk (#548, #682): the shim's project target lock wait must allow up to
        // LIGHT_SLOTS concurrent runs on the same project and time out with a clear
        // message instead of blocking forever when every pooled target is busy.
        let project = tempfile::tempdir()?;
        let workspace_a = project.path().join("issue-a");
        let workspace_b = project.path().join("issue-b");
        std::fs::create_dir_all(workspace_a.join(".rosen"))?;
        std::fs::create_dir_all(workspace_b.join(".rosen"))?;

        let targets = super::light_targets(&workspace_a);
        assert_eq!(targets.len(), super::LIGHT_SLOTS);
        assert_eq!(
            targets[0].file_name().and_then(|name| name.to_str()),
            Some(".velocity-agent-target")
        );
        assert_eq!(
            targets[1].file_name().and_then(|name| name.to_str()),
            Some(".velocity-agent-target-1")
        );

        let identity_a = crate::light_target::workspace_identity(&workspace_a);
        let identity_b = crate::light_target::workspace_identity(&workspace_b);
        let (target_a, holder_a) = super::try_lock_light_target(&targets, &identity_a)?
            .ok_or_else(|| std::io::Error::other("expected first pooled target"))?;
        let (target_b, holder_b) = super::try_lock_light_target(&targets, &identity_b)?
            .ok_or_else(|| std::io::Error::other("expected second pooled target concurrently"))?;
        assert_ne!(target_a, target_b);
        assert_eq!(super::busy_light_targets(&targets), super::LIGHT_SLOTS);

        let error = match super::lock_light_target_with_timeout(
            &targets,
            &identity_a,
            std::time::Instant::now(),
            std::time::Duration::from_millis(60),
        ) {
            Ok(_) => {
                return Err(std::io::Error::other(
                    "expected timeout while all pooled targets are held",
                ));
            }
            Err(error) => error,
        };
        drop((holder_a, holder_b));
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            error.to_string().contains("shared Rust target lock"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn target_pool_prefers_workspace_affinity_and_falls_back_when_busy() -> std::io::Result<()> {
        // Risk (#682): back-to-back runs from the same workspace must prefer the pooled
        // target already warmed for it (keeping local fingerprints intact) while falling
        // back to another free target whenever the affine target is locked.
        let project = tempfile::tempdir()?;
        let workspace_a = project.path().join("issue-a");
        let workspace_b = project.path().join("issue-b");
        std::fs::create_dir_all(workspace_a.join(".rosen"))?;
        std::fs::create_dir_all(workspace_b.join(".rosen"))?;

        let targets = super::light_targets(&workspace_a);
        let identity_a = crate::light_target::workspace_identity(&workspace_a);
        let identity_b = crate::light_target::workspace_identity(&workspace_b);
        std::fs::create_dir_all(&targets[0])?;
        std::fs::create_dir_all(&targets[1])?;
        std::fs::write(targets[0].join(".velocity-last-workspace"), &identity_a)?;
        std::fs::write(targets[1].join(".velocity-last-workspace"), &identity_b)?;

        let (chosen_b, lock_b) = super::try_lock_light_target(&targets, &identity_b)?
            .ok_or_else(|| std::io::Error::other("expected affine target for workspace B"))?;
        assert_eq!(chosen_b, targets[1]);

        // While workspace B holds targets[1], another request with B's identity falls
        // back to targets[0] instead of blocking.
        let (fallback, _lock_fallback) = super::try_lock_light_target(&targets, &identity_b)?
            .ok_or_else(|| std::io::Error::other("expected fallback target when affine is busy"))?;
        assert_eq!(fallback, targets[0]);
        drop(lock_b);
        Ok(())
    }

    #[test]
    fn light_slot_times_out_when_all_slots_are_held() -> std::io::Result<()> {
        // Risk (#548): the shim's light-slot wait must time out with a clear message
        // instead of blocking forever when all slots are busy.
        let dir = tempfile::tempdir()?;
        let mut holders = Vec::new();
        for _ in 0..super::LIGHT_SLOTS {
            let slot = super::try_light_slot_in(dir.path())?
                .ok_or_else(|| std::io::Error::other("expected free slot"))?;
            holders.push(slot);
        }
        let error = match super::light_slot_in(
            dir.path(),
            std::time::Instant::now(),
            std::time::Duration::from_millis(60),
        ) {
            Ok(_) => {
                return Err(std::io::Error::other(
                    "expected timeout when slots are full",
                ));
            }
            Err(error) => error,
        };
        drop(holders);
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("light Rust slot"), "{error}");
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

    #[test]
    fn cargo_export_bindings_is_allowed_only_when_managed() {
        // Risk (#559): queued `pnpm -C ui bindings` runs `cargo run --manifest-path ../Cargo.toml
        // -p velocity-app --bin export-bindings`, which must be permitted under managed/held runs
        // while unqueued shell invocations and arbitrary `cargo run` binaries stay refused.
        let allowed: &[&[&str]] = &[
            &[
                "run",
                "--manifest-path",
                "../Cargo.toml",
                "-p",
                "velocity-app",
                "--bin",
                "export-bindings",
            ],
            &[
                "run",
                "--manifest-path",
                "src-tauri/Cargo.toml",
                "--bin",
                "export-bindings",
                "--",
                "--check",
            ],
            &[
                "run",
                "--locked",
                "-p",
                "velocity-app",
                "--bin",
                "export-bindings",
                "--",
                "--check",
            ],
            &[
                "run",
                "--manifest-path=../Cargo.toml",
                "--package=velocity-app",
                "--bin=export-bindings",
            ],
            &["run", "--bin", "export-bindings"],
        ];
        for arguments in allowed {
            let arguments: Vec<String> = arguments.iter().map(|&arg| arg.to_owned()).collect();
            let os_arguments: Vec<std::ffi::OsString> =
                arguments.iter().map(std::ffi::OsString::from).collect();
            assert_eq!(
                super::cargo_policy(&arguments, true),
                Ok(AgentRun::Bindings),
                "Expected managed policy to allow {arguments:?}"
            );
            assert_eq!(
                policy("cargo", &arguments),
                Ok(AgentRun::Bindings),
                "Expected queue policy to allow {arguments:?}"
            );
            assert_eq!(
                super::direct_action(&os_arguments, true),
                Ok(super::DirectAction::Held),
                "Expected held cargo shim to run {arguments:?}"
            );
            assert!(
                super::cargo_policy(&arguments, false).is_err(),
                "Expected unmanaged shell policy to refuse {arguments:?}"
            );
            assert!(
                super::direct_action(&os_arguments, false).is_err(),
                "Expected unqueued cargo shim to refuse {arguments:?}"
            );
        }

        let refused: &[&[&str]] = &[
            &["run", "-p", "velocity-app"],
            &["run", "-p", "velocity-app", "--bin", "velocity-app"],
            &[
                "run",
                "--manifest-path",
                "../Cargo.toml",
                "-p",
                "velocity-app",
                "--bin",
                "export-bindings",
                "--release",
            ],
            &["run", "--bin", "export-bindings", "--", "--unexpected"],
            &["build", "-p", "velocity-app", "--bin", "export-bindings"],
        ];
        for arguments in refused {
            let arguments: Vec<String> = arguments.iter().map(|&arg| arg.to_owned()).collect();
            assert!(
                super::cargo_policy(&arguments, true).is_err(),
                "Expected managed policy to refuse {arguments:?}"
            );
        }
    }

    #[test]
    fn git_and_gh_refusals_are_single_line_with_alternatives() {
        // Risk (#533): agents in a Rosen workspace must have git/gh refused with a crisp
        // one-line explanation naming the allowed alternative.
        let Some(git) = super::vcs_refusal_for("git") else {
            panic!("git must have a refusal");
        };
        assert!(!git.contains('\n'));
        assert!(git.contains("rosen"));
        let Some(gh) = super::vcs_refusal_for("gh") else {
            panic!("gh must have a refusal");
        };
        assert!(!gh.contains('\n'));
        assert!(gh.contains("AIRC") || gh.contains("issue_"));
        assert!(super::vcs_refusal_for("cargo").is_none());
    }

    #[test]
    fn policy_with_vcs_blocked_enforces_rosen_vcs_refusal() {
        // Risk (#563): queue_run runs inside Velocity where process env/cwd are not the agent
        // workspace, so policy must take the session's Rosen state explicitly to refuse
        // direct git/gh while still admitting ordinary build/test tools such as `go`.
        let status = vec!["status".to_owned()];
        let go_test = vec![
            "-C".to_owned(),
            "server".to_owned(),
            "test".to_owned(),
            ".".to_owned(),
        ];
        assert!(super::policy_with_vcs_blocked("git", &status, true).is_err());
        assert!(super::policy_with_vcs_blocked("gh", &status, true).is_err());
        assert_eq!(
            super::policy_with_vcs_blocked("go", &go_test, true),
            Ok(AgentRun::Ordinary)
        );
        assert_eq!(
            super::policy_with_vcs_blocked("git", &status, false),
            Ok(AgentRun::Ordinary)
        );
    }

    #[test]
    fn ordinary_rosen_run_drops_agent_shims_and_vcs_block() -> std::io::Result<()> {
        // Risk (#563): foreground `rosen run -- go -C server test .` must drop Velocity's
        // managed shim folder and VELOCITY_BLOCK_GIT_GH so child Go tests invoking `git`
        // reach the host binary, while `rosen run -- lake build` keeps its cache shim.
        let shim = tempfile::tempdir()?;
        let toolchain = tempfile::tempdir()?;
        let git = super::executable_file("git");
        std::fs::write(shim.path().join(super::APP_RECORD), b"velocity")?;
        std::fs::write(shim.path().join(&git), b"shim")?;
        std::fs::write(toolchain.path().join(&git), b"real")?;
        let path =
            std::env::join_paths([shim.path(), toolchain.path()]).map_err(std::io::Error::other)?;
        let rosen = toolchain.path().join(super::executable_file("rosen"));

        let go_args =
            ["run", "--", "go", "-C", "server", "test", "."].map(std::ffi::OsString::from);
        let go_cmd = super::ordinary_command(&rosen, &go_args, Some(&go_args[2]), &path, &[])?;
        let child_path = go_cmd
            .get_envs()
            .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
            .and_then(|(_, value)| value.map(std::ffi::OsStr::to_owned))
            .ok_or_else(|| std::io::Error::other("ordinary command missing PATH"))?;
        let entries: Vec<std::path::PathBuf> = std::env::split_paths(&child_path).collect();
        assert!(
            !entries.contains(&shim.path().to_path_buf()),
            "shim survived on PATH: {entries:?}"
        );
        assert_eq!(
            super::real_program("git", &child_path),
            Some(toolchain.path().join(&git))
        );
        assert!(
            go_cmd
                .get_envs()
                .any(|(name, value)| name == super::BLOCK_VCS_ENV && value.is_none())
        );

        let lake_args = ["run", "--", "lake", "build"].map(std::ffi::OsString::from);
        let lake_cmd =
            super::ordinary_command(&rosen, &lake_args, Some(&lake_args[2]), &path, &[])?;
        assert!(
            !lake_cmd
                .get_envs()
                .any(|(name, _)| name.eq_ignore_ascii_case("PATH")),
            "lake run should keep its shim PATH so lean_cache::run_lake prepares Mathlib"
        );
        Ok(())
    }

    // Risk (#582): agents may queue exact `[generated.commands]` entries as RegisteredGenerator
    // while unregistered commands (including plain `cargo test` or `env cargo test`) stay refused
    // with the message mentioning registered generators.
    #[test]
    fn registered_generator_policy_admits_exact_command_and_refuses_unregistered() {
        let mut commands = std::collections::BTreeMap::new();
        let Ok(pattern) = rosen_core::PathPattern::new("fixtures/collapse/**") else {
            panic!("valid pattern");
        };
        commands.insert(
            pattern,
            "UPDATE_FIXTURES=1 cargo test -p rosen-server --lib collapse_fixtures".to_owned(),
        );
        let no_env = |_: &str| None;
        let with_env = |key: &str| (key == "UPDATE_FIXTURES").then(|| "1".to_owned());

        let cargo_args: Vec<String> = ["test", "-p", "rosen-server", "--lib", "collapse_fixtures"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let prefixed_args: Vec<String> = [
            "cargo",
            "test",
            "-p",
            "rosen-server",
            "--lib",
            "collapse_fixtures",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();

        let matched = super::policy_with_registered_generators(
            "UPDATE_FIXTURES=1",
            &prefixed_args,
            true,
            true,
            &commands,
            &no_env,
        );
        assert!(matches!(
            matched,
            Ok((AgentRun::RegisteredGenerator, Some(_)))
        ));

        let matched_via_env = super::policy_with_registered_generators(
            "cargo",
            &cargo_args,
            true,
            true,
            &commands,
            &with_env,
        );
        assert!(matches!(
            matched_via_env,
            Ok((AgentRun::RegisteredGenerator, Some(_)))
        ));

        // Plain `cargo test` and `cargo test ...` without `UPDATE_FIXTURES=1` are refused.
        let refused_plain = super::policy_with_registered_generators(
            "cargo",
            &["test".to_owned()],
            true,
            true,
            &commands,
            &no_env,
        );
        assert!(refused_plain.is_err());
        if let Err(message) = refused_plain {
            assert!(
                message.contains("[generated.commands]"),
                "refusal should mention registered generators: {message}"
            );
        }

        let refused_without_env = super::policy_with_registered_generators(
            "cargo",
            &cargo_args,
            true,
            true,
            &commands,
            &no_env,
        );
        assert!(refused_without_env.is_err());

        // Unregistered `env cargo test` or `FOO=1 cargo test` cannot bypass the policy.
        let refused_wrapped = super::policy_with_registered_generators(
            "env",
            &["FOO=1".to_owned(), "cargo".to_owned(), "test".to_owned()],
            true,
            true,
            &commands,
            &no_env,
        );
        assert!(refused_wrapped.is_err());
        let refused_assign = super::policy_with_registered_generators(
            "FOO=1",
            &["cargo".to_owned(), "test".to_owned()],
            true,
            true,
            &commands,
            &no_env,
        );
        assert!(refused_assign.is_err());

        // Registered generator is refused when not running at workspace root.
        let refused_subdir = super::policy_with_registered_generators(
            "UPDATE_FIXTURES=1",
            &prefixed_args,
            true,
            false,
            &commands,
            &no_env,
        );
        assert!(refused_subdir.is_err());
    }
}
