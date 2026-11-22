//! Fatal-exit recovery (#397, #543). The launcher stays small and owns no agent state; each
//! window/backend runs in a child, whose ordinary startup recovers saved work.
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CHILD_ARGUMENT: &str = "--velocity-supervised";
const STABLE_RUN: Duration = Duration::from_secs(5 * 60);
const MAX_RESTARTS: u32 = 5;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ACK_TIMEOUT: Duration = Duration::from_secs(1);

struct ChildTarget {
    address: SocketAddr,
    launcher_pid: u32,
}

struct ChildExit {
    pid: u32,
    status: ExitStatus,
    socket_intent: bool,
    marker_intent: bool,
    channel: &'static str,
}

impl ChildExit {
    fn intentional(&self) -> bool {
        self.socket_intent || self.marker_intent
    }

    fn clean_exit_without_intent(&self) -> bool {
        !self.intentional() && self.status.code() == Some(0)
    }

    fn unexpected_reason(&self) -> String {
        if self.status.code() == Some(0) {
            format!(
                "Unexpected clean exit (exit code: 0) from backend process {} with no intentional-exit signal (recovery socket: {}, intent marker: absent)",
                self.pid, self.channel
            )
        } else {
            format!(
                "Unexpected exit ({}) from backend process {}",
                self.status, self.pid
            )
        }
    }
}

enum Channel {
    Listening,
    Connected(TcpStream),
    Closed(&'static str),
}

impl Channel {
    fn summary(&self) -> &'static str {
        match self {
            Self::Listening => "never connected",
            Self::Connected(_) => "connected",
            Self::Closed(reason) => reason,
        }
    }
}

/// Runs the supervisor on a normal launch, returning only after its child exits for good.
/// MCP entry points must be dispatched before this, so an MCP server never opens a window.
/// `false` means this is the supervised child, which should start the application.
pub fn supervise() -> io::Result<bool> {
    supervise_with_local_rollback(None, None)
}

/// Runs the supervisor like [`supervise`], additionally rolling back to the previous local
/// build and marking the new commit as failed when a freshly restarted local build exits
/// unexpectedly during startup or its health window (#609).
pub fn supervise_with_local_rollback(
    app_data: Option<&Path>,
    running_commit: Option<&str>,
) -> io::Result<bool> {
    if child_target()?.is_some() {
        return Ok(false);
    }
    let executable = std::env::current_exe()?;
    let arguments = application_arguments();
    let launcher_pid = std::process::id();
    let notice_path = restart_notice_path(launcher_pid);
    let _ = remove_file_if_present(&notice_path);
    let mut failures = 0;
    let mut first_attempt = true;
    loop {
        let started = Instant::now();
        let (reason, clean_without_intent) = match run_child(
            &executable,
            &arguments,
            launcher_pid,
            first_attempt,
        ) {
            Ok(exit) if exit.intentional() => {
                if !exit.socket_intent && exit.marker_intent {
                    report(&format!(
                        "Intentional exit of backend process {} confirmed by marker file (recovery socket: {})",
                        exit.pid, exit.channel
                    ));
                }
                let _ = remove_file_if_present(&notice_path);
                return Ok(true);
            }
            Ok(exit) => (exit.unexpected_reason(), exit.clean_exit_without_intent()),
            Err(error) => (format!("Could not run Velocity: {error}"), false),
        };
        if !cfg!(debug_assertions)
            && let Some(dir) = app_data
            && maybe_rollback_pending_restart(&executable, dir, running_commit, &reason)
        {
            let _ = remove_file_if_present(&notice_path);
            return Ok(true);
        }
        first_attempt = false;
        if started.elapsed() >= STABLE_RUN {
            failures = 0;
        }
        if failures >= MAX_RESTARTS {
            let _ = remove_file_if_present(&notice_path);
            report(&format!(
                "Stopped recovery after {MAX_RESTARTS} short-lived restarts: {reason}"
            ));
            return Err(io::Error::other(
                "Velocity repeatedly exited during recovery",
            ));
        }
        let delay = Duration::from_secs(1 << failures);
        failures += 1;
        let log_line = format!("{reason}; restarting in {} s", delay.as_secs());
        report(&log_line);
        if clean_without_intent {
            write_restart_notice(
                &notice_path,
                &format!(
                    "{log_line}. Recovery log: {}",
                    recovery_log_path(launcher_pid).display()
                ),
            );
        }
        std::thread::sleep(delay);
    }
}

fn maybe_rollback_pending_restart(
    executable: &Path,
    app_data: &Path,
    running_commit: Option<&str>,
    reason: &str,
) -> bool {
    match rollback_pending_restart_with(
        executable,
        app_data,
        running_commit,
        reason,
        super::portable::relaunch_replaced,
    ) {
        Ok(rolled_back) => rolled_back,
        Err(error) => {
            report(&format!(
                "Could not roll back failed local-build restart: {error}"
            ));
            false
        }
    }
}

fn rollback_pending_restart_with<F>(
    executable: &Path,
    app_data: &Path,
    running_commit: Option<&str>,
    reason: &str,
    relaunch_fn: F,
) -> Result<bool, String>
where
    F: FnOnce(&Path, &Path) -> io::Result<()>,
{
    let builds = super::local_build::LocalBuilds::new(app_data);
    let Some(pending) = builds.pending_restart() else {
        return Ok(false);
    };
    if !pending.matches_running(running_commit)
        || pending.is_stale(super::local_build::unix_seconds())
    {
        return Ok(false);
    }
    let bytes = builds
        .read_rollback_executable(&pending)
        .map_err(|error| error.to_string())?;
    let failure = super::local_build::FailedBuild {
        commit: pending.commit.clone(),
        reason: format!("Restarted build failed during startup: {reason}"),
    };
    builds
        .mark_build_failed(&failure)
        .map_err(|error| error.to_string())?;
    builds.clear_pending_restart();
    let _ = super::process_tree::stop_before_swap(executable, Duration::from_secs(5));
    let previous =
        super::portable::replace_running(executable, &bytes).map_err(|error| error.to_string())?;
    if let Err(error) = relaunch_fn(executable, &previous) {
        let _ = super::portable::restore_previous(executable, &previous);
        return Err(format!(
            "Could not launch rolled-back previous executable: {error}"
        ));
    }
    report(&format!(
        "Rolled back failed local build {} to previous build: {reason}",
        super::local_build::short_commit(&pending.commit)
    ));
    Ok(true)
}

fn run_child(
    executable: &Path,
    arguments: &[OsString],
    launcher_pid: u32,
    first_attempt: bool,
) -> io::Result<ChildExit> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let marker_path = intent_marker_path(launcher_pid);
    let _ = remove_file_if_present(&marker_path);
    let mut command = super::portable::hidden_command(executable);
    if !first_attempt {
        super::portable::clear_replaced_env(&mut command);
    }
    let mut child = command
        .args(arguments)
        .arg(CHILD_ARGUMENT)
        .arg(listener.local_addr()?.to_string())
        .arg(launcher_pid.to_string())
        .spawn()?;
    report(&format!("Started backend process {}", child.id()));
    let result = wait_for_exit(&mut child, &listener, &marker_path);
    if result.is_err() {
        let _ = remove_file_if_present(&marker_path);
        // Never launch a replacement alongside a child whose wait or control channel failed.
        if child.try_wait()?.is_none() {
            child.kill()?;
            child.wait()?;
        }
    }
    result
}

fn wait_for_exit(
    child: &mut Child,
    listener: &TcpListener,
    marker_path: &Path,
) -> io::Result<ChildExit> {
    let mut channel = Channel::Listening;
    let mut intentional = false;
    loop {
        read_intent(listener, &mut channel, &mut intentional)?;
        if let Some(status) = child.try_wait()? {
            // The final exit notification can arrive between the read and try_wait.
            drain_intent(listener, &mut channel, &mut intentional)?;
            let marker_intent = intent_marker_matches(marker_path, child.id());
            let _ = remove_file_if_present(marker_path);
            return Ok(ChildExit {
                pid: child.id(),
                status,
                socket_intent: intentional,
                marker_intent,
                channel: channel.summary(),
            });
        }
        match channel {
            Channel::Listening => std::thread::sleep(ACCEPT_POLL_INTERVAL),
            Channel::Closed(_) => std::thread::sleep(POLL_INTERVAL),
            // `read_intent` blocks for up to `POLL_INTERVAL` while connected.
            Channel::Connected(_) => {}
        }
    }
}

fn accept_child(
    listener: &TcpListener,
    channel: &mut Channel,
    nonblocking: bool,
) -> io::Result<()> {
    if !matches!(channel, Channel::Listening) {
        return Ok(());
    }
    match listener.accept() {
        Ok((stream, _)) => {
            stream.set_nodelay(true)?;
            stream.set_nonblocking(nonblocking)?;
            if !nonblocking {
                stream.set_read_timeout(Some(POLL_INTERVAL))?;
                stream.set_write_timeout(Some(ACK_TIMEOUT))?;
            }
            *channel = Channel::Connected(stream);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Err(error) => Err(error),
    }
}

fn read_intent(
    listener: &TcpListener,
    channel: &mut Channel,
    intentional: &mut bool,
) -> io::Result<()> {
    accept_child(listener, channel, false)?;
    let Channel::Connected(stream) = channel else {
        return Ok(());
    };
    let mut buffer = [0u8; 32];
    match stream.read(&mut buffer) {
        Ok(0) => {
            *channel = Channel::Closed("closed cleanly");
            Ok(())
        }
        Ok(length) => {
            if let Some(reason) = apply_intent_bytes(stream, &buffer[..length], intentional)? {
                *channel = Channel::Closed(reason);
            }
            Ok(())
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(())
        }
        Err(error) if is_disconnect_error(&error) => {
            // Keep any intent already received when the child closes or resets the socket.
            *channel = Channel::Closed("reset");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn drain_intent(
    listener: &TcpListener,
    channel: &mut Channel,
    intentional: &mut bool,
) -> io::Result<()> {
    accept_child(listener, channel, true)?;
    let Channel::Connected(stream) = channel else {
        return Ok(());
    };
    stream.set_nonblocking(true)?;
    let mut buffer = [0u8; 32];
    loop {
        let Channel::Connected(stream) = channel else {
            return Ok(());
        };
        match stream.read(&mut buffer) {
            Ok(0) => {
                *channel = Channel::Closed("closed cleanly");
                return Ok(());
            }
            Ok(length) => {
                if let Some(reason) = apply_intent_bytes(stream, &buffer[..length], intentional)? {
                    *channel = Channel::Closed(reason);
                    return Ok(());
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if is_disconnect_error(&error) => {
                *channel = Channel::Closed("reset");
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
}

fn apply_intent_bytes(
    stream: &mut TcpStream,
    bytes: &[u8],
    intentional: &mut bool,
) -> io::Result<Option<&'static str>> {
    for &byte in bytes {
        *intentional = decode_intent(byte);
    }
    match stream.write_all(bytes) {
        Ok(()) => Ok(None),
        Err(error) if is_disconnect_error(&error) => {
            // The intent was already recorded; never fail recovery just because the peer closed.
            Ok(Some("reset while acknowledging"))
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn is_disconnect_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
    )
}

fn encode_intent(sequence: u8, intentional: bool) -> u8 {
    ((sequence & 0x7f) << 1) | u8::from(intentional)
}

fn decode_intent(byte: u8) -> bool {
    (byte & 1) == 1
}

struct ConnectionState {
    stream: Option<TcpStream>,
    sequence: u8,
}

/// The child's explicit quit/update signal. An exit code alone cannot distinguish an
/// intentional close from e.g. TerminateProcess with exit code zero. The socket is not
/// inherited by agent processes, and no recovery flags enter their environments.
pub struct RecoveryConnection {
    state: Mutex<ConnectionState>,
    launcher_pid: Option<u32>,
    child_pid: u32,
}

impl RecoveryConnection {
    pub fn connect() -> io::Result<Self> {
        let child_pid = std::process::id();
        let Some(target) = child_target()? else {
            return Ok(Self {
                state: Mutex::new(ConnectionState {
                    stream: None,
                    sequence: 0,
                }),
                launcher_pid: None,
                child_pid,
            });
        };
        let stream = TcpStream::connect_timeout(&target.address, CONNECT_TIMEOUT)?;
        stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
        stream.set_nodelay(true)?;
        Ok(Self {
            state: Mutex::new(ConnectionState {
                stream: Some(stream),
                sequence: 0,
            }),
            launcher_pid: Some(target.launcher_pid),
            child_pid,
        })
    }

    /// Set before an exit that may still be cancelled (single-instance handoff or an
    /// installer/update swap), and clear if work resumes. Keeps the socket open for updates.
    pub fn set_intentional_exit(&self, intentional: bool) -> io::Result<()> {
        let marker_result = self.update_intent_marker(intentional);
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Recovery lock poisoned"))?;
        let ConnectionState { stream, sequence } = &mut *state;
        if let Some(stream) = stream.as_mut() {
            send_and_acknowledge(stream, sequence, intentional, ACK_TIMEOUT)?;
        }
        if let Err(error) = marker_result {
            eprintln!("Velocity recovery: could not update intent marker: {error}");
        }
        Ok(())
    }

    /// Signals an irreversible exit (such as `RunEvent::Exit`), waits for supervisor
    /// acknowledgement, and shuts down the write half cleanly so Windows sends FIN instead
    /// of discarding buffered data with RST on process exit.
    pub fn finish_intentional_exit(&self) -> io::Result<()> {
        let marker_result = self.update_intent_marker(true);
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Recovery lock poisoned"))?;
        let socket_result = if let Some(mut stream) = state.stream.take() {
            let ack_result =
                send_and_acknowledge(&mut stream, &mut state.sequence, true, ACK_TIMEOUT);
            let shutdown_result = stream.shutdown(Shutdown::Write);
            drop(stream);
            ack_result.and(shutdown_result)
        } else {
            Ok(())
        };
        socket_result.and(marker_result)
    }

    /// Returns and clears any unannounced clean-exit restart notice recorded by the
    /// supervisor for this launcher, so the restarted instance can record it in `velocity.log`.
    pub fn take_restart_notice(&self) -> Option<String> {
        let launcher_pid = self.launcher_pid?;
        take_restart_notice_from(&restart_notice_path(launcher_pid))
    }

    fn update_intent_marker(&self, intentional: bool) -> io::Result<()> {
        let Some(launcher_pid) = self.launcher_pid else {
            return Ok(());
        };
        let path = intent_marker_path(launcher_pid);
        if intentional {
            write_intent_marker(&path, self.child_pid)
        } else {
            remove_file_if_present(&path)
        }
    }
}

fn send_and_acknowledge(
    stream: &mut TcpStream,
    sequence: &mut u8,
    intentional: bool,
    timeout: Duration,
) -> io::Result<()> {
    let token = encode_intent(*sequence, intentional);
    *sequence = sequence.wrapping_add(1);
    stream.write_all(&[token])?;
    let _ = stream.flush();
    wait_for_ack(stream, token, timeout)
}

fn wait_for_ack(stream: &mut TcpStream, expected: u8, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut buffer = [0u8; 32];
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(ack_timeout_error());
        };
        if remaining.is_zero() {
            return Err(ack_timeout_error());
        }
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(&mut buffer) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Supervisor closed the recovery channel before acknowledging exit intent",
                ));
            }
            Ok(length) => {
                if buffer[..length].contains(&expected) {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(ack_timeout_error());
            }
            Err(error) => return Err(error),
        }
    }
}

fn ack_timeout_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "Timed out waiting for supervisor exit-intent acknowledgement",
    )
}

fn child_target() -> io::Result<Option<ChildTarget>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let Some(index) = arguments
        .iter()
        .position(|argument| argument == CHILD_ARGUMENT)
    else {
        return Ok(None);
    };
    let address = arguments
        .get(index + 1)
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<SocketAddr>().ok())
        .filter(|address| address.ip() == Ipv4Addr::LOCALHOST);
    let launcher_pid = arguments
        .get(index + 2)
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|pid| *pid > 0);
    match (address, launcher_pid) {
        (Some(address), Some(launcher_pid)) if index + 3 == arguments.len() => {
            Ok(Some(ChildTarget {
                address,
                launcher_pid,
            }))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid recovery target",
        )),
    }
}

/// Updates start a fresh launcher, never another child of the old supervisor. Use these
/// arguments for portable relaunches and Tauri's installer restart environment.
pub fn application_arguments() -> Vec<OsString> {
    std::env::args_os()
        .skip(1)
        .take_while(|argument| argument != CHILD_ARGUMENT)
        .collect()
}

/// The supervisor launcher's PID when this process is a supervised backend child (#605).
pub(crate) fn launcher_pid() -> Option<u32> {
    child_target()
        .ok()
        .flatten()
        .map(|target| target.launcher_pid)
}

fn intent_marker_path(launcher_pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("velocity-recovery-{launcher_pid}.intent"))
}

fn restart_notice_path(launcher_pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("velocity-recovery-{launcher_pid}.restart"))
}

fn recovery_log_path(launcher_pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("velocity-recovery-{launcher_pid}.log"))
}

fn write_intent_marker(path: &Path, child_pid: u32) -> io::Result<()> {
    std::fs::write(path, format!("{child_pid}\n"))
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn intent_marker_matches(path: &Path, child_pid: u32) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| contents.trim().parse::<u32>().ok())
        .is_some_and(|recorded| recorded == child_pid)
}

fn write_restart_notice(path: &Path, message: &str) {
    let _ = std::fs::write(path, message);
}

fn take_restart_notice_from(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let _ = remove_file_if_present(path);
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn report(message: &str) {
    eprintln!("Velocity recovery: {message}");
    // Release copies have no console; retain evidence even when startup never reaches
    // Tauri's diagnostics. One file per launcher bounds each crash loop's output.
    let path = recovery_log_path(std::process::id());
    if let Ok(mut log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(log, "{}: {message}", chrono::Local::now().to_rfc3339());
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::process::Stdio;
    use std::sync::mpsc;

    use super::*;

    fn connect_for_test(
        address: SocketAddr,
        launcher_pid: u32,
        child_pid: u32,
    ) -> io::Result<RecoveryConnection> {
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
        stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
        stream.set_nodelay(true)?;
        Ok(RecoveryConnection {
            state: Mutex::new(ConnectionState {
                stream: Some(stream),
                sequence: 0,
            }),
            launcher_pid: Some(launcher_pid),
            child_pid,
        })
    }

    fn spawn_waiting_child() -> io::Result<Child> {
        // Blocks on stdin without spawning grandchildren; exits with code 0 when stdin closes.
        #[cfg(windows)]
        let program = std::env::var_os("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32").join("sort.exe"))
            .unwrap_or_else(|| PathBuf::from("sort.exe"));
        #[cfg(not(windows))]
        let program = PathBuf::from("cat");
        super::super::portable::hidden_command(program)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }

    // Risk (#543): Windows RST on process exit can discard an unacknowledged intent byte
    // before the supervisor reads it, causing a deliberate window close to relaunch Velocity.
    #[test]
    fn deliberate_close_is_acknowledged_twenty_times_without_relaunch() -> Result<(), Box<dyn Error>>
    {
        let base_launcher = std::process::id().saturating_add(100_000);
        for round in 0..20u32 {
            let launcher_pid = base_launcher + round;
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
            listener.set_nonblocking(true)?;
            let address = listener.local_addr()?;
            let marker_path = intent_marker_path(launcher_pid);
            let _ = remove_file_if_present(&marker_path);

            let mut child = spawn_waiting_child()?;
            let child_pid = child.id();
            let stdin = child.stdin.take();
            let (release_tx, release_rx) = mpsc::channel();
            let handle = std::thread::spawn(move || -> io::Result<()> {
                let connection = connect_for_test(address, launcher_pid, child_pid)?;
                // Mirror startup single-instance handoff -> setup -> RunEvent::Exit.
                connection.set_intentional_exit(true)?;
                connection.set_intentional_exit(false)?;
                connection.finish_intentional_exit()?;
                let _ = release_tx.send(());
                Ok(())
            });
            let monitor = {
                let marker_for_wait = marker_path.clone();
                std::thread::spawn(move || wait_for_exit(&mut child, &listener, &marker_for_wait))
            };
            release_rx.recv_timeout(Duration::from_secs(5))?;
            drop(stdin);

            handle
                .join()
                .map_err(|_| io::Error::other("worker thread panicked"))??;
            let exit = monitor
                .join()
                .map_err(|_| io::Error::other("monitor thread panicked"))??;
            assert!(exit.intentional(), "round {round} missed intentional exit");
            assert!(exit.socket_intent, "round {round} missed socket intent");
            assert!(exit.marker_intent, "round {round} missed marker intent");
            assert!(!marker_path.exists(), "round {round} leaked marker file");
        }
        Ok(())
    }

    // Risk (#543): a process killed externally (e.g. Task Manager) after setup must still
    // trigger recovery instead of looking intentional.
    #[test]
    fn killed_child_without_intent_triggers_recovery() -> Result<(), Box<dyn Error>> {
        let launcher_pid = std::process::id().saturating_add(200_000);
        let marker_path = intent_marker_path(launcher_pid);
        let _ = remove_file_if_present(&marker_path);

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let mut child = spawn_waiting_child()?;
        let child_pid = child.id();
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || -> io::Result<RecoveryConnection> {
            let connection = connect_for_test(address, launcher_pid, child_pid)?;
            connection.set_intentional_exit(true)?;
            connection.set_intentional_exit(false)?;
            let _ = ready_tx.send(());
            Ok(connection)
        });
        let kill_marker = marker_path.clone();
        let (kill_tx, kill_rx) = mpsc::channel();
        let monitor = std::thread::spawn(move || -> io::Result<ChildExit> {
            let mut channel = Channel::Listening;
            let mut intentional = false;
            loop {
                read_intent(&listener, &mut channel, &mut intentional)?;
                if kill_rx.try_recv().is_ok() {
                    let _ = child.kill();
                }
                if let Some(status) = child.try_wait()? {
                    drain_intent(&listener, &mut channel, &mut intentional)?;
                    let marker_intent = intent_marker_matches(&kill_marker, child.id());
                    let _ = remove_file_if_present(&kill_marker);
                    return Ok(ChildExit {
                        pid: child.id(),
                        status,
                        socket_intent: intentional,
                        marker_intent,
                        channel: channel.summary(),
                    });
                }
                if !matches!(channel, Channel::Connected(_)) {
                    std::thread::sleep(ACCEPT_POLL_INTERVAL);
                }
            }
        });
        ready_rx.recv_timeout(Duration::from_secs(5))?;
        assert!(!marker_path.exists(), "setup did not clear intent marker");
        let _held_connection = worker
            .join()
            .map_err(|_| io::Error::other("worker thread panicked"))??;
        let _ = kill_tx.send(());
        let killed = monitor
            .join()
            .map_err(|_| io::Error::other("monitor thread panicked"))??;
        assert!(!killed.intentional(), "killed child looked intentional");
        Ok(())
    }

    // Risk (#543): an unannounced clean code-0 exit must never relaunch silently; it must
    // log why it restarted and leave a one-shot notice for the next instance's velocity.log.
    #[test]
    fn unannounced_code_zero_exit_records_restart_notice() -> Result<(), Box<dyn Error>> {
        let launcher_pid = std::process::id().saturating_add(250_000);
        let marker_path = intent_marker_path(launcher_pid);
        let notice_path = restart_notice_path(launcher_pid);
        let _ = remove_file_if_present(&marker_path);
        let _ = remove_file_if_present(&notice_path);

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let mut clean_child = spawn_waiting_child()?;
        drop(clean_child.stdin.take());
        let clean_exit = wait_for_exit(&mut clean_child, &listener, &marker_path)?;
        assert!(!clean_exit.intentional());
        assert!(clean_exit.clean_exit_without_intent());
        let reason = clean_exit.unexpected_reason();
        assert!(reason.contains("exit code: 0"));
        assert!(reason.contains("no intentional-exit signal"));
        write_restart_notice(&notice_path, &reason);
        assert_eq!(take_restart_notice_from(&notice_path), Some(reason));
        assert_eq!(take_restart_notice_from(&notice_path), None);
        Ok(())
    }

    // Risk (#543): read_intent and drain_intent must keep an already-received intentional
    // exit signal when the peer closes or resets the socket without calling shutdown(Write).
    #[test]
    fn connection_close_or_reset_preserves_received_intent() -> Result<(), Box<dyn Error>> {
        let launcher_pid = std::process::id().saturating_add(275_000);
        let marker_path = intent_marker_path(launcher_pid);
        let _ = remove_file_if_present(&marker_path);

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let worker = std::thread::spawn(move || -> io::Result<()> {
            let connection = connect_for_test(address, launcher_pid, std::process::id())?;
            connection.set_intentional_exit(true)?;
            // Drop without shutdown(Write) to mimic single-instance/installer exit.
            drop(connection);
            Ok(())
        });

        let mut channel = Channel::Listening;
        let mut intentional = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(channel, Channel::Closed(_)) && Instant::now() < deadline {
            read_intent(&listener, &mut channel, &mut intentional)?;
            if !matches!(channel, Channel::Connected(_)) {
                std::thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
        drain_intent(&listener, &mut channel, &mut intentional)?;
        worker
            .join()
            .map_err(|_| io::Error::other("worker thread panicked"))??;
        let _ = remove_file_if_present(&marker_path);
        assert!(intentional, "intent was lost after peer disconnect");
        Ok(())
    }

    // Risk (#543): if the socket times out or resets, the durable marker file must still
    // prevent a spurious relaunch.
    #[test]
    fn durable_marker_survives_ack_timeout_and_confirms_exit() -> Result<(), Box<dyn Error>> {
        let launcher_pid = std::process::id().saturating_add(300_000);
        let marker_path = intent_marker_path(launcher_pid);
        let _ = remove_file_if_present(&marker_path);

        // Listener never accepts or acknowledges, so finish_intentional_exit must time out
        // within ~1 s rather than hanging, while still writing the durable marker file.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let connection = connect_for_test(address, launcher_pid, std::process::id())?;
        let started = Instant::now();
        let error = connection
            .finish_intentional_exit()
            .err()
            .ok_or("expected acknowledgement timeout")?;
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "ack wait hung for {:?}",
            started.elapsed()
        );
        assert!(
            intent_marker_matches(&marker_path, std::process::id()),
            "durable marker was not written before timeout"
        );

        // A child exiting with only the durable marker set is still recognized as intentional.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let mut child = spawn_waiting_child()?;
        write_intent_marker(&marker_path, child.id())?;
        drop(child.stdin.take());
        let exit = wait_for_exit(&mut child, &listener, &marker_path)?;
        assert!(exit.intentional());
        assert!(!exit.socket_intent);
        assert!(exit.marker_intent);
        assert!(!marker_path.exists());
        Ok(())
    }

    // Risk (#609): if a newly restarted local build crashes during startup, the supervisor
    // must restore the previous build's executable, mark the new commit failed in
    // failed.json, clear pending-restart.json, and relaunch the previous build.
    #[test]
    fn startup_crash_rolls_back_pending_local_build_and_marks_commit_failed()
    -> Result<(), Box<dyn Error>> {
        let scratch = std::env::temp_dir().join(format!(
            "velocity-supervision-rollback-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch)?;
        let app_data = scratch.join("app-data");
        let executable = scratch.join("velocity-app.exe");
        std::fs::write(&executable, b"previous-good-binary")?;

        let builds = super::super::local_build::LocalBuilds::new(&app_data);
        let bad_commit = "786d72e".to_owned() + &"a".repeat(33);
        let version = super::super::local_build::local_version("0.1.0-beta.22", &bad_commit);
        let folder = builds.prepare(&bad_commit)?;
        std::fs::write(
            folder.join(format!("Velocity_{version}_x64-portable.exe")),
            b"crashing-new-binary",
        )?;
        let _ = builds.finish(&bad_commit, &version)?;
        let _ = builds.arm_pending_restart(&bad_commit, Some("b9169e5"), Some(&executable))?;
        // Simulate the swap into the crashing new build.
        std::fs::write(&executable, b"crashing-new-binary")?;

        let mut relaunched = false;
        let rolled_back = rollback_pending_restart_with(
            &executable,
            &app_data,
            Some(&bad_commit),
            "Unexpected exit (exit code: 101)",
            |exe, _prev| {
                assert_eq!(std::fs::read(exe)?, b"previous-good-binary");
                relaunched = true;
                Ok(())
            },
        )
        .map_err(io::Error::other)?;
        assert!(rolled_back);
        assert!(relaunched);
        assert_eq!(std::fs::read(&executable)?, b"previous-good-binary");
        assert!(builds.pending_restart().is_none());
        assert!(builds.list().is_empty());
        let failure = builds.failure().ok_or("failed.json was not recorded")?;
        assert_eq!(failure.commit, bad_commit);
        assert!(failure.reason.contains("exit code: 101"));

        let _ = std::fs::remove_dir_all(&scratch);
        Ok(())
    }
}
