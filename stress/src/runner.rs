use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use crate::timing::Timings;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// The binary holding every Rust harness, built once per run (#499).
const HARNESS_BINARY: &str = "stress-harness";
/// Names the harness `stress-harness` runs; see `stress/harness/main.rs`.
const HARNESS_VARIABLE: &str = "VELOCITY_STRESS_HARNESS";
/// Hands the built binary to `.ps1` scripts, so `stress-harness.psm1` does not build it again.
const HARNESS_EXECUTABLE_VARIABLE: &str = "VELOCITY_STRESS_HARNESS_EXE";
/// The `stress-harness.psm1` function a `.ps1` script runs its harness with.
const HARNESS_HELPER: &str = "Invoke-StressHarness";

/// What a run has found so far: the steps that failed, and how long each step took (#434).
#[derive(Default)]
struct Outcome {
    failures: Vec<String>,
    timings: Timings,
}

impl Outcome {
    /// Records a step that started at `started` and prints its result.
    fn finish(&mut self, label: &str, started: Instant, result: &Result<bool>) {
        let passed = matches!(result, Ok(true));
        self.timings.record(label, started, passed);
        if passed {
            println!("PASS: {label}");
        } else {
            eprintln!("FAIL: {label}: {result:?}");
            self.failures.push(label.to_owned());
        }
    }
}

pub fn run(root: &Path, entries: &[PathBuf], whole_suite: bool) -> bool {
    let mut outcome = Outcome::default();
    let mut steps = entries.len();
    if whole_suite {
        workspace_checks(root, &mut outcome);
        steps += 2;
    }
    let mut harness = None;
    if entries.iter().any(|entry| uses_harness(entry)) {
        harness = build_harnesses(root, &mut outcome);
        steps += 1;
    }
    run_entries(root, entries, harness.as_deref(), &mut outcome);
    outcome.timings.report();
    report(steps, &outcome.failures)
}

/// Whether the entry runs a Rust harness: every `.rs` script, and each `.ps1` script that
/// calls the harness helper.
fn uses_harness(entry: &Path) -> bool {
    match entry.extension().and_then(|value| value.to_str()) {
        Some("rs") => true,
        Some("ps1") => fs::read_to_string(entry).is_ok_and(|text| text.contains(HARNESS_HELPER)),
        _ => false,
    }
}

/// Builds every Rust harness in one `cargo build`: one feature resolution, one target
/// folder and the root lockfile, instead of a standalone project per harness (#499).
fn build_harnesses(root: &Path, outcome: &mut Outcome) -> Option<PathBuf> {
    let label = "Build stress harnesses";
    println!("\n==> {label}");
    let started = Instant::now();
    let mut command = Command::new("cargo");
    command.args(["build", "--locked", "-p", "stress"]);
    command.args(["--features", "harnesses", "--bin", HARNESS_BINARY]);
    let executable = target_directory(root)
        .join("debug")
        .join(format!("{HARNESS_BINARY}{}", std::env::consts::EXE_SUFFIX));
    let result = status(root, &mut command).and_then(|built| {
        if !built {
            return Ok(false);
        }
        if !executable.is_file() {
            return Err(format!("the build left no {}", executable.display()).into());
        }
        Ok(true)
    });
    outcome.finish(label, started, &result);
    matches!(result, Ok(true)).then_some(executable)
}

/// The folder cargo builds into: `status` sets CARGO_TARGET_DIR to `<root>/target` when it
/// is unset, and cargo resolves a relative one from the working folder, `root`.
fn target_directory(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| root.join("target"), |folder| root.join(folder))
}

fn workspace_checks(root: &Path, outcome: &mut Outcome) {
    let mut tests = Command::new("cargo");
    tests.args(["test", "--workspace", "--locked"]);
    let mut assets = Command::new("pwsh");
    assets.args([
        "-NoProfile",
        "-Command",
        "& pnpm -C ui build; exit $LASTEXITCODE",
    ]);
    for (label, mut command) in [("Workspace tests", tests), ("Production UI assets", assets)] {
        println!("\n==> {label}");
        let started = Instant::now();
        let result = status(root, &mut command);
        outcome.finish(label, started, &result);
    }
}

fn run_entries(root: &Path, entries: &[PathBuf], harness: Option<&Path>, outcome: &mut Outcome) {
    for entry in entries {
        let label = entry
            .strip_prefix(root)
            .unwrap_or(entry)
            .display()
            .to_string();
        println!("\n==> {label}");
        let started = Instant::now();
        // A failed harness build fails each harness it would have run, without running
        // a binary left from an earlier build.
        let result: Result<bool> = if harness.is_none() && uses_harness(entry) {
            Err("the stress harnesses did not build".into())
        } else {
            execute(root, entry, harness)
        };
        outcome.finish(&label, started, &result);
    }
}

fn report(total: usize, failures: &[String]) -> bool {
    println!(
        "\n{} passed; {} failed",
        total - failures.len(),
        failures.len()
    );
    for failure in failures {
        eprintln!("  {failure}");
    }
    failures.is_empty()
}

fn execute(root: &Path, entry: &Path, harness: Option<&Path>) -> Result<bool> {
    let mut command = match entry.extension().and_then(|value| value.to_str()) {
        Some("ps1") => {
            let mut command = Command::new("pwsh");
            command.args(["-NoProfile", "-File"]).arg(entry);
            if let Some(harness) = harness {
                command.env(HARNESS_EXECUTABLE_VARIABLE, harness);
            }
            command
        }
        Some("py") => {
            let mut command = Command::new("python");
            command.arg(entry);
            command
        }
        Some("rs") => {
            let name = entry.file_stem().ok_or("Rust evidence has no name")?;
            let mut command = Command::new(harness.ok_or("the stress harnesses did not build")?);
            command.env(HARNESS_VARIABLE, name);
            command
        }
        _ => return Err(format!("{} is not a stress script", entry.display()).into()),
    };
    status(root, &mut command)
}

fn status(root: &Path, command: &mut Command) -> Result<bool> {
    command.current_dir(root).env("PYTHONUTF8", "1");
    // Vite invokes `net use`; keep Java's net.dll from shadowing Windows net.exe
    // when the aggregate command is run independently of check.ps1 (#44).
    if let (Some(system_root), Some(path)) =
        (std::env::var_os("SystemRoot"), std::env::var_os("PATH"))
    {
        let paths = std::iter::once(PathBuf::from(system_root).join("System32"))
            .chain(std::env::split_paths(&path));
        command.env("PATH", std::env::join_paths(paths)?);
    }
    if std::env::var_os("CARGO_TARGET_DIR").is_none() {
        command.env("CARGO_TARGET_DIR", root.join("target"));
    }
    let status = command.status()?;
    println!("Exit status: {status}");
    Ok(status.success())
}
