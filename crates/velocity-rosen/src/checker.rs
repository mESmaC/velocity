//! Velocity's own checks on a Rosen change (SPEC.md F7 "Checks before merge"; Rosen R8 and
//! §9 "Build queue"): the project's pre-land commands, run on the change's merge result in
//! one folder per project that keeps its build output from one check to the next.
//!
//! A check that started from an empty folder would build everything every time. Here the
//! merge result is written beside the folder, and only the files that differ are moved in,
//! so the build after it compiles what changed since the last check.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rosen_client::Client;

use crate::duty::{self, MergeCheckout};
use crate::process::hidden_command;
use crate::workspace::session;
use crate::{ProjectServer, RosenError};

/// The Rosen identity Velocity submits receipts and lands under.
pub const CHECKER: &str = "velocity-checker";
/// The longest one pre-land command may run, as Rosen allows a prepare command.
const COMMAND_LIMIT: Duration = Duration::from_secs(60 * 60);
const POLL: Duration = Duration::from_millis(250);

/// A pre-land command that did not pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedCommand {
    pub command: String,
    /// `None` when the command ran past the limit and was stopped.
    pub exit_code: Option<i32>,
    /// What it printed, standard output and error together.
    pub output: String,
}

/// The commands the project's `rosen.toml` names for its pre-land check set, as the base
/// holds it. Empty when the project names none.
pub async fn pre_land_commands(server: &ProjectServer) -> Result<Vec<String>, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let config = client.project_config(None).await?.config;
    Ok(config.checks.pre_land.commands)
}

/// Writes `change`'s current merge result beside `folder`, ready for [`apply_staged`].
/// Nothing is staged when the merge conflicts. A folder Velocity did not check out is
/// never touched.
pub async fn stage_merge_result(
    server: &ProjectServer,
    change: &str,
    folder: &Path,
) -> Result<MergeCheckout, RosenError> {
    let staging = duty::claim_checkout(folder)?;
    let checkout = session(server, CHECKER)
        .await?
        .check_out_merge_result(change, &staging)
        .await?;
    Ok(MergeCheckout {
        head: checkout.merge.head.to_string(),
        base: checkout.merge.base.to_string(),
        conflicts: checkout.merge.conflicts,
    })
}

/// Brings `folder` to the staged merge result (blocking). A file whose bytes are already
/// there is left alone with its modified time, so the build tool sees it as unchanged; a
/// file the last result had and this one lacks is removed. Everything else in the folder
/// is build output and stays.
pub fn apply_staged(folder: &Path) -> Result<(), RosenError> {
    let staging = duty::staging_folder(folder);
    let list = file_list(folder);
    let files = files_under(&staging)?;
    let wanted: BTreeSet<&Path> = files.iter().map(PathBuf::as_path).collect();
    for gone in listed_files(&list)
        .iter()
        .filter(|path| !wanted.contains(path.as_path()))
    {
        // Already gone is what was wanted.
        let _ = std::fs::remove_file(folder.join(gone));
    }
    for file in &files {
        let target = folder.join(file);
        place(&staging.join(file), &target).map_err(|source| io_error("write", &target, source))?;
    }
    let lines: Vec<String> = files.iter().map(|file| slashed(file)).collect();
    std::fs::write(&list, lines.join("\n")).map_err(|source| io_error("write", &list, source))?;
    std::fs::remove_dir_all(&staging).map_err(|source| io_error("remove", &staging, source))
}

/// Runs `commands` in order in `folder` through the platform shell (blocking), stopping at
/// the first that fails. `None` when every command passed.
pub fn run_commands(
    folder: &Path,
    commands: &[String],
) -> Result<Option<FailedCommand>, RosenError> {
    let log = duty::sibling(folder, ".rosen-check.log");
    for command in commands {
        let exit_code = match run_one(folder, command, &log)? {
            Some(0) => continue,
            other => other,
        };
        let output = std::fs::read(&log).unwrap_or_default();
        return Ok(Some(FailedCommand {
            command: command.clone(),
            exit_code,
            output: String::from_utf8_lossy(&output).into_owned(),
        }));
    }
    Ok(None)
}

/// The command's exit status, or `None` when it was stopped for running too long. A
/// process ended by a signal counts as status 1.
fn run_one(folder: &Path, command: &str, log: &Path) -> Result<Option<i32>, RosenError> {
    let failed = |source| io_error("run a check in", folder, source);
    let output = File::create(log).map_err(|source| io_error("write", log, source))?;
    let errors = output
        .try_clone()
        .map_err(|source| io_error("write", log, source))?;
    let mut child = shell(command)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(errors)
        .spawn()
        .map_err(failed)?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(failed)? {
            return Ok(Some(status.code().unwrap_or(1)));
        }
        if started.elapsed() > COMMAND_LIMIT {
            stop_tree(&mut child);
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

/// `cmd /S /C "<command>"`: with `/S`, cmd drops the outer quotes and runs the rest as
/// written, so the command's own quotes survive.
#[cfg(windows)]
fn shell(command: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut shell = hidden_command("cmd");
    shell.raw_arg(format!("/S /C \"{command}\""));
    shell
}

#[cfg(not(windows))]
fn shell(command: &str) -> Command {
    let mut shell = hidden_command("sh");
    shell.args(["-c", command]);
    shell
}

/// Stops the command and what it started: a build left running would hold the folder's
/// files against the next check.
fn stop_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let _ = hidden_command("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Moves `source` to `target` unless `target` already holds the same bytes.
fn place(source: &Path, target: &Path) -> std::io::Result<()> {
    if same_bytes(source, target) {
        return Ok(());
    }
    if target.is_dir() {
        // The tree now has a file where it had a folder, or build output sat there.
        std::fs::remove_dir_all(target)?;
    }
    if let Some(parent) = target.parent() {
        make_folder(parent)?;
    }
    std::fs::rename(source, target)
}

/// Creates `folder`, removing a file that sits where it or one of its parents must go.
fn make_folder(folder: &Path) -> std::io::Result<()> {
    if std::fs::create_dir_all(folder).is_ok() {
        return Ok(());
    }
    if let Some(file) = folder.ancestors().find(|ancestor| ancestor.is_file()) {
        std::fs::remove_file(file)?;
    }
    std::fs::create_dir_all(folder)
}

fn same_bytes(left: &Path, right: &Path) -> bool {
    let size = |path: &Path| std::fs::metadata(path).ok().filter(|data| data.is_file());
    match (size(left), size(right)) {
        (Some(left_size), Some(right_size)) if left_size.len() == right_size.len() => {
            matches!((std::fs::read(left), std::fs::read(right)), (Ok(a), Ok(b)) if a == b)
        }
        _ => false,
    }
}

/// Every file under `root`, relative to it. Empty when `root` does not exist.
fn files_under(root: &Path) -> Result<Vec<PathBuf>, RosenError> {
    let mut files = Vec::new();
    let mut folders = vec![PathBuf::new()];
    while let Some(folder) = folders.pop() {
        let path = root.join(&folder);
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error("read", &path, source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| io_error("read", &path, source))?;
            let relative = folder.join(entry.file_name());
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                folders.push(relative);
            } else {
                files.push(relative);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Beside the folder, like its marker: the files the last merge result put in it.
fn file_list(folder: &Path) -> PathBuf {
    duty::sibling(folder, ".rosen-files")
}

fn listed_files(list: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(list)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn io_error(action: &'static str, path: &Path, source: std::io::Error) -> RosenError {
    RosenError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}
