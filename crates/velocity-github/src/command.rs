use std::ffi::OsStr;
use std::process::Command;

pub(crate) fn gh() -> Command {
    let mut command = hidden_command("gh");
    // Velocity reads gh's output; prompts would block it and colour codes would
    // garble the errors it shows.
    command.env("GH_PROMPT_DISABLED", "1").env("NO_COLOR", "1");
    command
}

/// `git -C <checkout>` for the watcher's commit checks. The checkout goes in `-C`, not the
/// working directory, because CreateProcess rejects working directories over 260 characters.
pub(crate) fn git_in(checkout: &std::path::Path) -> Command {
    let mut command = hidden_command("git");
    command
        .args(["-c", "core.longpaths=true", "-C"])
        .arg(checkout);
    command
}

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
