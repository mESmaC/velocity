use std::process::Command;

/// `git` with long paths on: Git for Windows refuses paths over 260 characters
/// without `core.longpaths`.
pub(crate) fn git() -> Command {
    let mut command = Command::new("git");
    command.args(["-c", "core.longpaths=true"]);
    hide_console_window(&mut command);
    command
}

/// Velocity is a GUI app; without this flag every git call flashes a console window.
#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}
