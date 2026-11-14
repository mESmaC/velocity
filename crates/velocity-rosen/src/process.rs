//! Every process this crate starts is built here; clippy's `disallowed_methods`
//! (`clippy.toml`) rejects a bare `Command::new` and `creation_flags` elsewhere.

use std::ffi::OsStr;
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A `Command` that opens no console window: Velocity's release build is a windowed app,
/// so a child started without `CREATE_NO_WINDOW` flashes a console on screen (#232).
#[allow(clippy::disallowed_methods)]
pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// A project's Rosen server, which outlives the call that starts it. It gets a hidden
/// console of its own rather than none: every `git` it runs then shares that console,
/// where a server with no console makes each git open a new window, and git sometimes
/// failed to start that way. Leaving Velocity's job object lets it keep serving agents'
/// watchers across an app restart; some job objects forbid that, so callers retry with
/// `leave_job` off.
#[allow(clippy::disallowed_methods)]
pub(crate) fn server_command(program: impl AsRef<OsStr>, leave_job: bool) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let breakaway = if leave_job {
            CREATE_BREAKAWAY_FROM_JOB
        } else {
            0
        };
        // Repeats CREATE_NO_WINDOW: creation flags replace the ones set before (#276).
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | breakaway);
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt;
        let _ = leave_job;
        command.process_group(0);
    }
    command
}
