//! Starting child processes without a console window.

use std::ffi::OsStr;
use std::process::Command;

/// A `Command` that opens no console window. Velocity's release build is a windowed app, so on
/// Windows a child started without `CREATE_NO_WINDOW` gets a console window of its own that
/// flashes on screen (#232). Every process this crate starts is built here; clippy's
/// `disallowed_methods` (`clippy.toml`) rejects a bare `Command::new`.
#[allow(clippy::disallowed_methods)]
pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
