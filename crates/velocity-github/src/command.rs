use std::process::Command;

pub(crate) fn gh() -> Command {
    let mut command = Command::new("gh");
    // Velocity reads gh's output; prompts would block it and colour codes would
    // garble the errors it shows.
    command.env("GH_PROMPT_DISABLED", "1").env("NO_COLOR", "1");
    hide_console_window(&mut command);
    command
}

/// Velocity is a GUI app; without this flag every gh call flashes a console window.
#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

/// `git -C <checkout>` for the watcher's commit checks. The checkout goes in `-C`, not the
/// working directory, because CreateProcess rejects working directories over 260 characters.
pub(crate) fn git_in(checkout: &std::path::Path) -> Command {
    let mut command = Command::new("git");
    command
        .args(["-c", "core.longpaths=true", "-C"])
        .arg(checkout);
    hide_console_window(&mut command);
    command
}
