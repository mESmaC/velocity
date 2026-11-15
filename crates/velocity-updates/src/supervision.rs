//! Fatal-exit recovery (#397). The launcher stays small and owns no agent state; each
//! window/backend runs in a child, whose ordinary startup recovers saved work.
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, ExitStatus};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CHILD_ARGUMENT: &str = "--velocity-supervised";
const STABLE_RUN: Duration = Duration::from_secs(5 * 60);
const MAX_RESTARTS: u32 = 5;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Runs the supervisor on a normal launch, returning only after its child exits for good.
/// MCP entry points must be dispatched before this, so an MCP server never opens a window.
/// `false` means this is the supervised child, which should start the application.
pub fn supervise() -> io::Result<bool> {
    if child_address()?.is_some() {
        return Ok(false);
    }
    let executable = std::env::current_exe()?;
    let arguments = application_arguments();
    let mut failures = 0;
    loop {
        let started = Instant::now();
        let reason = match run_child(&executable, &arguments) {
            Ok((_, true)) => return Ok(true),
            Ok((status, false)) => format!("Unexpected exit ({status})"),
            Err(error) => format!("Could not run Velocity: {error}"),
        };
        if started.elapsed() >= STABLE_RUN {
            failures = 0;
        }
        if failures >= MAX_RESTARTS {
            report(&format!(
                "Stopped recovery after {MAX_RESTARTS} short-lived restarts: {reason}"
            ));
            return Err(io::Error::other(
                "Velocity repeatedly exited during recovery",
            ));
        }
        let delay = Duration::from_secs(1 << failures);
        failures += 1;
        report(&format!("{reason}; restarting in {} s", delay.as_secs()));
        std::thread::sleep(delay);
    }
}

fn run_child(
    executable: &std::path::Path,
    arguments: &[OsString],
) -> io::Result<(ExitStatus, bool)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let mut child = super::portable::hidden_command(executable)
        .args(arguments)
        .arg(CHILD_ARGUMENT)
        .arg(listener.local_addr()?.to_string())
        .spawn()?;
    report(&format!("Started backend process {}", child.id()));
    let result = wait_for_exit(&mut child, &listener);
    if result.is_err() {
        // Never launch a replacement alongside a child whose wait or control channel failed.
        child.kill()?;
        child.wait()?;
    }
    result
}

fn wait_for_exit(child: &mut Child, listener: &TcpListener) -> io::Result<(ExitStatus, bool)> {
    let mut connection = None;
    let mut intentional = false;
    loop {
        read_intent(listener, &mut connection, &mut intentional)?;
        if let Some(status) = child.try_wait()? {
            // The final exit notification can arrive between the read and try_wait.
            read_intent(listener, &mut connection, &mut intentional)?;
            return Ok((status, intentional));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn read_intent(
    listener: &TcpListener,
    connection: &mut Option<TcpStream>,
    intentional: &mut bool,
) -> io::Result<()> {
    if connection.is_none() {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(true)?;
                *connection = Some(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
        }
    }
    let Some(stream) = connection else {
        return Ok(());
    };
    let mut buffer = [0; 32];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(length) => *intentional = buffer[length - 1] == 1,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

/// The child's explicit quit/update signal. An exit code alone cannot distinguish an
/// intentional close from e.g. TerminateProcess with exit code zero. The socket is not
/// inherited by agent processes, and no recovery flags enter their environments.
pub struct RecoveryConnection(Mutex<Option<TcpStream>>);

impl RecoveryConnection {
    pub fn connect() -> io::Result<Self> {
        let connection = child_address()?
            .map(|address| TcpStream::connect_timeout(&address, Duration::from_secs(5)))
            .transpose()?;
        if let Some(stream) = &connection {
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            stream.set_nodelay(true)?;
        }
        Ok(Self(Mutex::new(connection)))
    }

    /// Set before an irreversible exit, and clear if an installer fails and work resumes.
    pub fn set_intentional_exit(&self, intentional: bool) -> io::Result<()> {
        let mut connection = self
            .0
            .lock()
            .map_err(|_| io::Error::other("Recovery lock poisoned"))?;
        if let Some(stream) = connection.as_mut() {
            stream.write_all(&[u8::from(intentional)])?;
        }
        Ok(())
    }
}

fn child_address() -> io::Result<Option<SocketAddr>> {
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
        .filter(|address| address.ip() == Ipv4Addr::LOCALHOST && index + 2 == arguments.len());
    address
        .map(Some)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid recovery address"))
}

/// Updates start a fresh launcher, never another child of the old supervisor. Use these
/// arguments for portable relaunches and Tauri's installer restart environment.
pub fn application_arguments() -> Vec<OsString> {
    std::env::args_os()
        .skip(1)
        .take_while(|argument| argument != CHILD_ARGUMENT)
        .collect()
}

fn report(message: &str) {
    eprintln!("Velocity recovery: {message}");
    // Release copies have no console; retain evidence even when startup never reaches
    // Tauri's diagnostics. One file per launcher bounds each crash loop's output.
    let path = std::env::temp_dir().join(format!("velocity-recovery-{}.log", std::process::id()));
    if let Ok(mut log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(log, "{}: {message}", chrono::Local::now().to_rfc3339());
    }
}
