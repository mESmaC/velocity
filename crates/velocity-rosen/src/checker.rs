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

use rosen_client::run_queue::{self, RunClass, RunQueue, Turn};

use crate::duty::{self, MergeCheckout};
use crate::policy::{CheckPolicy, policy_at};
use crate::process::{background_command, hidden_command};
use crate::workspace::session;
use crate::{ProjectServer, RosenError};

/// The Rosen identity Velocity submits receipts and lands under.
pub const CHECKER: &str = "velocity-checker";
/// The longest one pre-land command may run, as Rosen allows a prepare command.
const COMMAND_LIMIT: Duration = Duration::from_secs(60 * 60);
const POLL: Duration = Duration::from_millis(250);

/// How much of the machine a run of commands may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    /// A change waits to land on this run.
    Landing,
    /// Nothing waits on this run: it runs at low process priority, and waits its turn
    /// behind the checks of a land and agents' own builds.
    Background,
}

/// A check command that did not pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedCommand {
    pub command: String,
    /// `None` when the command ran past the limit and was stopped.
    pub exit_code: Option<i32>,
    /// What it printed, standard output and error together.
    pub output: String,
}

/// A merge result staged beside the check folder, with the policy its base holds.
#[derive(Clone, Debug)]
pub struct StagedMerge {
    pub merge: MergeCheckout,
    /// Read at `merge.base`, the base the check runs on: its commands are the ones to run
    /// and its check set the one the receipt names, whatever lands meanwhile.
    pub policy: CheckPolicy,
}

/// Writes `change`'s current merge result beside `folder`, ready for [`apply_staged`], and
/// reads the policy at its base. Nothing is staged when the merge conflicts. A folder
/// Velocity did not check out is never touched.
pub async fn stage_merge_result(
    server: &ProjectServer,
    change: &str,
    folder: &Path,
) -> Result<StagedMerge, RosenError> {
    let staging = duty::claim_checkout(folder)?;
    let checkout = session(server, CHECKER)
        .await?
        .check_out_merge_result(change, &staging)
        .await?;
    let merge = MergeCheckout {
        head: checkout.merge.head.to_string(),
        base: checkout.merge.base.to_string(),
        conflicts: checkout.merge.conflicts,
    };
    let policy = policy_at(server, Some(&merge.base)).await?;
    Ok(StagedMerge { merge, policy })
}

/// Brings `folder` to exactly the staged merge result (blocking). A file whose bytes are
/// already there is left alone with its modified time, so the build tool sees it as
/// unchanged; a file the last result had and this one lacks is removed. Everything else in
/// the folder is build output and stays.
///
/// A check certifies the folder's sources, so they must be the merge result's and nothing
/// more: a stale file the build still finds (a deleted module, say) can make a tree that
/// fails to build pass. Any file that can't be removed stops the check, and stays listed
/// so the next check removes it.
pub fn apply_staged(folder: &Path) -> Result<(), RosenError> {
    let staging = duty::staging_folder(folder);
    let list = file_list(folder);
    let files = files_under(&staging)?;
    let listed = match listed_files(&list)? {
        Some(listed) => listed,
        // Nothing says which files are the last result's and which are build output, so
        // none of it can be trusted: one cold build is the price.
        None => {
            remove_entries(folder)?;
            Vec::new()
        }
    };
    // Every file this run may leave in the folder is listed before the folder changes, so a
    // run cut off part way still removes the ones it should have next time.
    let tracked: BTreeSet<&Path> = listed.iter().chain(&files).map(PathBuf::as_path).collect();
    write_list(folder, tracked)?;
    let wanted: BTreeSet<&Path> = files.iter().map(PathBuf::as_path).collect();
    for gone in listed
        .iter()
        .filter(|path| !wanted.contains(path.as_path()))
    {
        remove_tracked(&folder.join(gone))?;
    }
    for file in &files {
        let target = folder.join(file);
        place(&staging.join(file), &target).map_err(|source| io_error("write", &target, source))?;
    }
    write_list(folder, files.iter().map(PathBuf::as_path))?;
    std::fs::remove_dir_all(&staging).map_err(|source| io_error("remove", &staging, source))
}

/// Removes a file the last merge result had. Only a file that is already gone counts as
/// removed: one held open or refused is still in the folder for the build to find.
fn remove_tracked(path: &Path) -> Result<(), RosenError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("remove", path, source)),
    }
}

/// Empties `folder`, keeping the folder itself.
fn remove_entries(folder: &Path) -> Result<(), RosenError> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error("read", folder, source)),
    };
    for entry in entries {
        let entry = entry.map_err(|source| io_error("read", folder, source))?;
        let path = entry.path();
        let removed = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.map_err(|source| io_error("remove", &path, source))?;
    }
    Ok(())
}

/// Runs `commands` in order in `folder` through the platform shell (blocking), stopping at
/// the first that fails. `None` when every command passed.
///
/// The whole run takes one turn in the machine's run queue (Rosen R3 "Run queue"), so the
/// checks of several projects, agents' own builds and the warm pools' prepares take turns
/// instead of all compiling at once. Checks a land waits on are first in that line. The
/// commands are told they run beneath that turn (`ROSEN_RUN_HELD`), so a `rosen run` or
/// build-queue step inside them runs at once instead of waiting behind its own check.
pub fn run_commands(
    folder: &Path,
    commands: &[String],
    priority: Priority,
) -> Result<Option<FailedCommand>, RosenError> {
    let log = duty::sibling(folder, ".rosen-check.log");
    let turn = turn(folder, priority);
    let held = turn.as_ref().map(|_| class_of(priority));
    for command in commands {
        let exit_code = match run_one(folder, command, &log, (priority, held))? {
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

/// Waits for this run's turn. `None` when the queue can't be used: it only schedules, so
/// the checks then run without waiting.
fn turn(folder: &Path, priority: Priority) -> Option<Turn> {
    let class = class_of(priority);
    let note = format!("{} checks in {}", class.name(), folder.display());
    RunQueue::machine().ok()?.turn(class, &note).ok()
}

fn class_of(priority: Priority) -> RunClass {
    match priority {
        Priority::Landing => RunClass::Landing,
        Priority::Background => RunClass::Verify,
    }
}

/// The command's exit status, or `None` when it was stopped for running too long. A
/// process ended by a signal counts as status 1. `held` is the run queue turn the command
/// runs beneath, if one was taken.
fn run_one(
    folder: &Path,
    command: &str,
    log: &Path,
    (priority, held): (Priority, Option<RunClass>),
) -> Result<Option<i32>, RosenError> {
    let failed = |source| io_error("run a check in", folder, source);
    let output = File::create(log).map_err(|source| io_error("write", log, source))?;
    let errors = output
        .try_clone()
        .map_err(|source| io_error("write", log, source))?;
    let mut shell = shell(command, priority);
    if let Some(class) = held {
        shell.env(run_queue::HELD_ENV, class.name());
    }
    let mut child = shell
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
fn shell(command: &str, priority: Priority) -> Command {
    use std::os::windows::process::CommandExt;
    let mut shell = shell_program("cmd", priority);
    shell.raw_arg(format!("/S /C \"{command}\""));
    shell
}

#[cfg(not(windows))]
fn shell(command: &str, priority: Priority) -> Command {
    let mut shell = shell_program("sh", priority);
    shell.args(["-c", command]);
    shell
}

fn shell_program(program: &str, priority: Priority) -> Command {
    match priority {
        Priority::Landing => hidden_command(program),
        Priority::Background => background_command(program),
    }
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

/// The files the list names, or `None` when there is no list.
fn listed_files(list: &Path) -> Result<Option<Vec<PathBuf>>, RosenError> {
    let text = match std::fs::read_to_string(list) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error("read", list, source)),
    };
    Ok(Some(
        text.lines()
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect(),
    ))
}

/// Replaces the folder's file list in one rename, so a run cut off while writing it
/// leaves the whole old list or the whole new one.
fn write_list<'a>(
    folder: &Path,
    files: impl IntoIterator<Item = &'a Path>,
) -> Result<(), RosenError> {
    let list = file_list(folder);
    let next = duty::sibling(folder, ".rosen-files.next");
    let lines: Vec<String> = files.into_iter().map(slashed).collect();
    std::fs::write(&next, lines.join("\n")).map_err(|source| io_error("write", &next, source))?;
    std::fs::rename(&next, &list).map_err(|source| io_error("write", &list, source))
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

#[cfg(all(test, windows))]
mod tests {
    use std::error::Error;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    use super::*;

    /// Read and write sharing, but no delete sharing: what an editor or a build holding
    /// the file does, and what makes Windows refuse to remove it.
    const SHARE_READ_WRITE: u32 = 0x1 | 0x2;

    // Risk (audit of 59be908): a file the last merge result had and this one dropped was
    // held open, so it could not be removed. The check went on with it in the folder and
    // forgot it, and a module the change deleted still compiled: the receipt certified a
    // tree that does not build. The check must stop, and the file stay listed for the next.
    #[test]
    fn a_dropped_file_that_cannot_be_removed_stops_the_check_and_stays_listed()
    -> Result<(), Box<dyn Error>> {
        let scratch = std::env::temp_dir().join(format!("velocity-apply-{}", std::process::id()));
        let folder = scratch.join("pre-land-checks");
        let staging = duty::staging_folder(&folder);
        std::fs::create_dir_all(&folder)?;
        std::fs::create_dir_all(&staging)?;
        let main = "mod deleted_module; fn main() { deleted_module::works(); }\n";
        let dropped = folder.join("deleted_module.rs");
        std::fs::write(&dropped, "pub fn works() {}\n")?;
        std::fs::write(folder.join("main.rs"), main)?;
        std::fs::write(staging.join("main.rs"), main)?;
        std::fs::write(file_list(&folder), "deleted_module.rs\nmain.rs")?;

        let held = OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ_WRITE)
            .open(&dropped)?;
        let refused = apply_staged(&folder);
        let listed_while_held = std::fs::read_to_string(file_list(&folder))?;
        drop(held);
        let applied = apply_staged(&folder);
        let listed = std::fs::read_to_string(file_list(&folder))?;
        let survived = dropped.exists();
        std::fs::remove_dir_all(&scratch)?;

        assert!(refused.is_err(), "a held file let the check go on");
        assert!(
            listed_while_held
                .lines()
                .any(|line| line == "deleted_module.rs")
        );
        applied?;
        assert!(!survived, "the next check left the dropped file");
        assert_eq!(listed, "main.rs");
        Ok(())
    }
}
