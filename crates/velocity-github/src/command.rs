use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Absolute path to the real `gh` executable resolved from Velocity's own `PATH` (skipping
/// agent shim folders) and passed to Velocity's MCP child servers (#581).
pub const GH_PROGRAM_ENV: &str = "VELOCITY_GH";

/// Marker files inside Velocity's managed agent command folder (`velocity-rosen::agent_run`).
const AGENT_SHIM_MARKERS: &[&str] = &["velocity-program", "rosen-program"];

/// Agent-session variables removed from `gh` children that Velocity starts so the real `gh`
/// never inherits Rosen or VCS-refusal markers (#581).
const STRIPPED_GH_ENV_VARS: &[&str] = &["VELOCITY_BLOCK_GIT_GH", "ROSEN_CHANGE", "ROSEN_ENDPOINT"];

static APP_GH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Resolves the real `gh` executable once from this process's environment, skipping agent
/// shim folders, for passing to child MCP servers through [`GH_PROGRAM_ENV`] (#581).
pub fn app_gh_program() -> Option<&'static Path> {
    APP_GH.get_or_init(resolve_gh_program).as_deref()
}

/// Resolves the real `gh` executable from [`GH_PROGRAM_ENV`] or `PATH`, skipping Velocity's
/// agent shim folders (#581).
pub fn resolve_gh_program() -> Option<PathBuf> {
    resolve_gh_program_from(
        std::env::var_os(GH_PROGRAM_ENV).as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
}

/// Builds a hidden `Command` targeting the real `gh` executable with any agent shim folder
/// and Rosen/VCS-block variables removed from its environment (#581).
pub(crate) fn gh_command() -> Command {
    let program = resolve_gh_program()
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| OsString::from("gh"));
    let mut command = hidden_command(program);
    if let Some(clean_path) = cleaned_path_from(std::env::var_os("PATH").as_deref()) {
        command.env("PATH", clean_path);
    }
    for name in STRIPPED_GH_ENV_VARS {
        command.env_remove(name);
    }
    command
}

/// `gh <arguments>`, counted against the caller like every `GitHubCommand` call (SPEC.md
/// F6), so the request totals cover the browser, new-project and repository-list calls too.
#[track_caller]
pub(crate) fn gh(arguments: &[&str]) -> Command {
    crate::request_counter::record(
        std::panic::Location::caller(),
        crate::request_counter::operation(arguments, None),
    );
    let mut command = gh_command();
    command.args(arguments);
    // Velocity reads gh's output; prompts would block it and colour codes would
    // garble the errors it shows.
    command.env("GH_PROMPT_DISABLED", "1").env("NO_COLOR", "1");
    command
}

/// A `Command` that opens no console window. Velocity's release build is a windowed app, so on
/// Windows a child started without `CREATE_NO_WINDOW` gets a console window of its own that
/// flashes on screen (#232). Every process this crate starts is built here; clippy's
/// `disallowed_methods` (`clippy.toml`) rejects a bare `Command::new`.
#[allow(clippy::disallowed_methods)]
pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    // Only Windows sets creation flags on it.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn is_agent_shim_folder(folder: &Path) -> bool {
    AGENT_SHIM_MARKERS
        .iter()
        .any(|marker| folder.join(marker).is_file())
}

fn cleaned_path_from(path: Option<&OsStr>) -> Option<OsString> {
    let folders = std::env::split_paths(path?).filter(|folder| !is_agent_shim_folder(folder));
    std::env::join_paths(folders).ok()
}

fn resolve_gh_program_from(override_path: Option<&OsStr>, path: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(raw) = override_path.filter(|value| !value.is_empty()) {
        let candidate = PathBuf::from(raw);
        if candidate.is_file() && !candidate.parent().is_some_and(is_agent_shim_folder) {
            return Some(std::path::absolute(&candidate).unwrap_or(candidate));
        }
    }
    let path = path?;
    for folder in std::env::split_paths(path) {
        if is_agent_shim_folder(&folder) {
            continue;
        }
        if let Some(found) = find_gh_in_folder(&folder) {
            return Some(std::path::absolute(&found).unwrap_or(found));
        }
    }
    None
}

fn find_gh_in_folder(folder: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    const NAMES: &[&str] = &["gh.exe", "gh.cmd", "gh.bat"];
    #[cfg(not(windows))]
    const NAMES: &[&str] = &["gh"];

    NAMES
        .iter()
        .map(|name| folder.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_gh_skips_agent_shim_folder_and_prefers_valid_override() -> std::io::Result<()> {
        // Risk (#581): an inherited agent PATH puts Velocity's shim folder before the host
        // gh binary; resolution must skip shim folders and honor a non-shim VELOCITY_GH path.
        let shim = tempfile::tempdir()?;
        let host = tempfile::tempdir()?;
        let explicit = tempfile::tempdir()?;
        #[cfg(windows)]
        let file = "gh.cmd";
        #[cfg(not(windows))]
        let file = "gh";

        std::fs::write(shim.path().join("velocity-program"), b"velocity")?;
        std::fs::write(shim.path().join(file), b"shim")?;
        std::fs::write(host.path().join(file), b"host")?;
        std::fs::write(explicit.path().join(file), b"explicit")?;

        let path =
            std::env::join_paths([shim.path(), host.path()]).map_err(std::io::Error::other)?;
        assert_eq!(
            resolve_gh_program_from(None, Some(&path)),
            Some(std::path::absolute(host.path().join(file))?)
        );
        let explicit_gh = explicit.path().join(file);
        assert_eq!(
            resolve_gh_program_from(Some(explicit_gh.as_os_str()), Some(&path)),
            Some(std::path::absolute(&explicit_gh)?)
        );
        // A VELOCITY_GH pointing inside a shim folder must be ignored.
        let shim_gh = shim.path().join(file);
        assert_eq!(
            resolve_gh_program_from(Some(shim_gh.as_os_str()), Some(&path)),
            Some(std::path::absolute(host.path().join(file))?)
        );

        let cleaned = cleaned_path_from(Some(&path))
            .ok_or_else(|| std::io::Error::other("cleaned path missing"))?;
        let entries: Vec<PathBuf> = std::env::split_paths(&cleaned).collect();
        assert!(!entries.contains(&shim.path().to_path_buf()));
        assert!(entries.contains(&host.path().to_path_buf()));
        Ok(())
    }
}
