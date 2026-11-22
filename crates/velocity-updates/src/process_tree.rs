//! Process-tree cleanup for portable updates and local-build restarts (#605).
//!
//! Stops orphaned `velocity-app.exe` child helpers (graph-project, MCP servers) whose
//! parent Velocity process is gone, and stops any lingering previous-copy process trees
//! and helper children before swapping the executable on Windows.
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Subcommands spawned by Velocity as helper children (`src-tauri/src/main.rs`).
/// Never includes the launcher, `--velocity-supervised` main window, or `machine` worker.
const CHILD_HELPER_SWITCHES: &[&str] = &[
    "--graph-project",
    "--graph-mcp",
    "--graph-warm",
    "--airc-mcp",
    "--scout-mcp",
    "--queued-run-mcp",
    "--rosen-mcp",
    "--browser-mcp",
    "--review-swarm-mcp",
    "--parked-review-mcp",
    "--velocity-agent-command",
    "app-server",
];

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Result of stopping helper or previous-copy process trees.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StopSummary {
    pub stopped: Vec<u32>,
    pub still_running: Vec<u32>,
}

/// Stops `velocity-app.exe` child helpers (current or `.old*` in the same folder as
/// `executable`) whose parent Velocity process is gone (#605).
pub fn stop_orphaned_helpers(executable: &Path, timeout: Duration) -> StopSummary {
    let Some(mut system) = refresh_all_processes() else {
        return StopSummary::default();
    };
    let protected = self_and_ancestors(&system, Pid::from_u32(std::process::id()));
    let roots: Vec<Pid> = system
        .processes()
        .values()
        .filter(|process| !protected.contains(&process.pid()))
        .filter(|process| {
            process
                .exe()
                .is_some_and(|exe| is_same_install_image(executable, exe))
        })
        .filter(|process| is_child_helper(process.cmd()))
        .filter(|process| !has_live_velocity_host(&system, executable, process.pid()))
        .map(Process::pid)
        .collect();
    kill_and_wait_trees(&mut system, roots, &protected, timeout)
}

/// Before swapping `executable`, kills and waits for every lingering process tree running
/// from a previous `.old*` image in the same folder, every orphaned helper, and this
/// copy's own spawned helper children (`--graph-project`, MCP servers) so nothing keeps
/// either the previous or newly moved-aside image locked (#605).
pub fn stop_before_swap(executable: &Path, timeout: Duration) -> StopSummary {
    let Some(mut system) = refresh_all_processes() else {
        return StopSummary::default();
    };
    let current = Pid::from_u32(std::process::id());
    let protected = self_and_ancestors(&system, current);
    let own_root = own_velocity_root(&system, executable, current);
    let descendants = collect_descendants(&system, own_root);
    let roots: Vec<Pid> = system
        .processes()
        .values()
        .filter(|process| !protected.contains(&process.pid()))
        .filter(|process| should_stop_before_swap(&system, executable, &descendants, process))
        .map(Process::pid)
        .collect();
    kill_and_wait_trees(&mut system, roots, &protected, timeout)
}

/// Stops any processes still running from the moved-aside `image` path (excluding this
/// process and its ancestors) so `wait_for_previous` can remove the file promptly (#605).
pub(crate) fn stop_processes_using_image(image: &Path, timeout: Duration) -> StopSummary {
    let Some(mut system) = refresh_all_processes() else {
        return StopSummary::default();
    };
    let protected = self_and_ancestors(&system, Pid::from_u32(std::process::id()));
    let wanted = comparable_path(image);
    let roots: Vec<Pid> = system
        .processes()
        .values()
        .filter(|process| !protected.contains(&process.pid()))
        .filter(|process| {
            process
                .exe()
                .is_some_and(|exe| comparable_path(exe) == wanted)
        })
        .map(Process::pid)
        .collect();
    kill_and_wait_trees(&mut system, roots, &protected, timeout)
}

/// `true` while any PID in `pids` is still running.
pub(crate) fn any_pid_alive(pids: &[u32]) -> bool {
    if pids.is_empty() {
        return false;
    }
    let Ok(mut system) = System::new() else {
        return false;
    };
    let targets: Vec<Pid> = pids.iter().copied().map(Pid::from_u32).collect();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&targets),
        true,
        ProcessRefreshKind::nothing(),
    );
    targets.iter().any(|pid| system.process(*pid).is_some())
}

fn should_stop_before_swap(
    system: &System,
    executable: &Path,
    own_descendants: &HashSet<Pid>,
    process: &Process,
) -> bool {
    let Some(exe) = process.exe() else {
        return false;
    };
    if is_same_install_previous(executable, exe) {
        return true;
    }
    if !is_same_install_current(executable, exe) || !is_child_helper(process.cmd()) {
        return false;
    }
    own_descendants.contains(&process.pid())
        || !has_live_velocity_host(system, executable, process.pid())
}

fn refresh_all_processes() -> Option<System> {
    let mut system = System::new().ok()?;
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always),
    );
    Some(system)
}

fn self_and_ancestors(system: &System, start: Pid) -> HashSet<Pid> {
    let mut protected = HashSet::from([start]);
    let mut cursor = start;
    while let Some(parent) = valid_parent(system, cursor) {
        if !protected.insert(parent) {
            break;
        }
        cursor = parent;
    }
    protected
}

fn own_velocity_root(system: &System, executable: &Path, current: Pid) -> Pid {
    let mut root = current;
    let mut seen = HashSet::from([root]);
    while let Some(parent) = valid_parent(system, root) {
        if !seen.insert(parent) {
            break;
        }
        let same_host = system.process(parent).is_some_and(|process| {
            process
                .exe()
                .is_some_and(|exe| is_same_install_image(executable, exe))
                && !is_child_helper(process.cmd())
        });
        if !same_host {
            break;
        }
        root = parent;
    }
    root
}

fn has_live_velocity_host(system: &System, executable: &Path, start: Pid) -> bool {
    let mut seen = HashSet::from([start]);
    let mut cursor = start;
    while let Some(parent) = valid_parent(system, cursor) {
        if !seen.insert(parent) {
            break;
        }
        if let Some(process) = system.process(parent)
            && process
                .exe()
                .is_some_and(|exe| is_same_install_current(executable, exe))
            && !is_child_helper(process.cmd())
        {
            return true;
        }
        cursor = parent;
    }
    false
}

/// Returns `child`'s parent PID only when the parent still exists and started no later
/// than `child` (guarding against Windows PID reuse).
fn valid_parent(system: &System, child: Pid) -> Option<Pid> {
    let process = system.process(child)?;
    let parent_pid = process.parent()?;
    let parent = system.process(parent_pid)?;
    (parent.start_time() <= process.start_time()).then_some(parent_pid)
}

fn children_map(system: &System) -> HashMap<Pid, Vec<Pid>> {
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for process in system.processes().values() {
        if let Some(parent) = valid_parent(system, process.pid()) {
            children.entry(parent).or_default().push(process.pid());
        }
    }
    children
}

fn collect_descendants(system: &System, root: Pid) -> HashSet<Pid> {
    let map = children_map(system);
    let mut queue = vec![root];
    let mut seen = HashSet::from([root]);
    let mut index = 0;
    while let Some(&pid) = queue.get(index) {
        if let Some(kids) = map.get(&pid) {
            for &kid in kids {
                if seen.insert(kid) {
                    queue.push(kid);
                }
            }
        }
        index += 1;
    }
    seen.remove(&root);
    seen
}

fn kill_and_wait_trees(
    system: &mut System,
    roots: Vec<Pid>,
    protected: &HashSet<Pid>,
    timeout: Duration,
) -> StopSummary {
    if roots.is_empty() {
        return StopSummary::default();
    }
    let watched = collect_watched_pids(system, &roots, protected);
    terminate_trees(system, &roots, &watched);
    wait_until_exited(system, watched, timeout)
}

fn collect_watched_pids(
    system: &System,
    roots: &[Pid],
    protected: &HashSet<Pid>,
) -> HashMap<Pid, u64> {
    let map = children_map(system);
    let mut watched = HashMap::new();
    let mut queue: Vec<Pid> = roots
        .iter()
        .copied()
        .filter(|pid| !protected.contains(pid))
        .collect();
    let mut index = 0;
    while let Some(&pid) = queue.get(index) {
        if let Some(process) = system.process(pid)
            && watched.insert(pid, process.start_time()).is_none()
            && let Some(kids) = map.get(&pid)
        {
            queue.extend(kids.iter().copied().filter(|kid| !protected.contains(kid)));
        }
        index += 1;
    }
    watched
}

fn terminate_trees(system: &System, roots: &[Pid], watched: &HashMap<Pid, u64>) {
    #[cfg(windows)]
    for root in roots {
        let _ = super::portable::hidden_command("taskkill")
            .args(["/T", "/F", "/PID", &root.as_u32().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    let _ = roots;
    for &pid in watched.keys() {
        if let Some(process) = system.process(pid) {
            let _ = process.kill();
        }
    }
}

fn wait_until_exited(
    system: &mut System,
    watched: HashMap<Pid, u64>,
    timeout: Duration,
) -> StopSummary {
    let started = Instant::now();
    let mut remaining = watched.clone();
    loop {
        let pids: Vec<Pid> = remaining.keys().copied().collect();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::nothing(),
        );
        remaining.retain(|pid, start_time| {
            system
                .process(*pid)
                .is_some_and(|process| process.start_time() == *start_time)
        });
        if remaining.is_empty() || started.elapsed() >= timeout {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    let mut stopped: Vec<u32> = watched
        .keys()
        .filter(|pid| !remaining.contains_key(*pid))
        .map(|pid| pid.as_u32())
        .collect();
    let mut still_running: Vec<u32> = remaining.keys().map(|pid| pid.as_u32()).collect();
    stopped.sort_unstable();
    still_running.sort_unstable();
    StopSummary {
        stopped,
        still_running,
    }
}

pub(crate) fn is_previous_sidecar_name(executable_name: &str, candidate_name: &str) -> bool {
    let prefix = format!("{executable_name}.old");
    if cfg!(windows) {
        candidate_name
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    } else {
        candidate_name.starts_with(&prefix)
    }
}

fn is_same_install_previous(executable: &Path, candidate: &Path) -> bool {
    let (Some(exe_dir), Some(cand_dir)) = (executable.parent(), candidate.parent()) else {
        return false;
    };
    if comparable_path(exe_dir) != comparable_path(cand_dir) {
        return false;
    }
    let (Some(exe_name), Some(cand_name)) = (
        executable.file_name().and_then(|n| n.to_str()),
        candidate.file_name().and_then(|n| n.to_str()),
    ) else {
        return false;
    };
    is_previous_sidecar_name(exe_name, cand_name)
}

fn is_same_install_current(executable: &Path, candidate: &Path) -> bool {
    comparable_path(executable) == comparable_path(candidate)
}

fn is_same_install_image(executable: &Path, candidate: &Path) -> bool {
    is_same_install_current(executable, candidate)
        || is_same_install_previous(executable, candidate)
}

fn is_child_helper(cmd: &[OsString]) -> bool {
    cmd.get(1)
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| CHILD_HELPER_SWITCHES.contains(&arg))
}

fn comparable_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if cfg!(windows) {
        let normalized = raw.replace('/', "\\");
        let stripped = normalized.strip_prefix(r"\\?\").unwrap_or(&normalized);
        stripped.trim_end_matches('\\').to_ascii_lowercase()
    } else {
        raw.trim_end_matches('/').to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_previous_sidecars_and_install_images_within_same_folder() {
        let exe = Path::new(r"C:\Users\owner\Velocity\velocity-app.exe");
        let legacy_old = Path::new(r"C:\Users\owner\Velocity\velocity-app.exe.old");
        let stamped_old = Path::new(r"C:\Users\owner\Velocity\velocity-app.exe.old-1763780000123");
        let dev_build = Path::new(r"Z:\velocity\target\debug\velocity-app.exe.old");

        assert!(is_same_install_previous(exe, legacy_old));
        assert!(is_same_install_previous(exe, stamped_old));
        assert!(!is_same_install_previous(exe, exe));
        assert!(!is_same_install_previous(exe, dev_build));
        assert!(is_same_install_image(exe, exe));
        assert!(is_same_install_image(exe, stamped_old));
        assert!(!is_same_install_image(exe, dev_build));
    }

    #[test]
    fn classifies_helper_switches_without_matching_main_or_machine_hosts() {
        let helper =
            |switch: &str| vec![OsString::from("velocity-app.exe"), OsString::from(switch)];
        assert!(is_child_helper(&helper("--graph-project")));
        assert!(is_child_helper(&helper("--graph-mcp")));
        assert!(is_child_helper(&helper("--airc-mcp")));
        assert!(is_child_helper(&helper("--rosen-mcp")));
        assert!(is_child_helper(&helper("--queued-run-mcp")));
        assert!(is_child_helper(&helper("--browser-mcp")));
        assert!(!is_child_helper(&helper("--velocity-supervised")));
        assert!(!is_child_helper(&helper("machine")));
        assert!(!is_child_helper(&[OsString::from("velocity-app.exe")]));
    }
}
