//! Finding the program behind a bare command name.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// Resolves a bare program name such as `pnpm` the way a Windows shell does: each folder on
/// `PATH`, trying each `PATHEXT` extension. The system itself finds only `.exe` files, so
/// without this `npm`, `pnpm`, `npx` and `yarn`, which install as `.cmd` shims, fail to start.
/// Given the full path of a `.cmd` or `.bat` file, Rust's standard library runs it through
/// `cmd.exe` and escapes its arguments safely (the BatBadBut fix).
///
/// The working folder is not searched: a repository could otherwise plant a `git.cmd` that
/// runs in place of your git.
#[cfg(windows)]
pub fn find_program(program: &OsStr) -> PathBuf {
    let Some(search_path) = search_path() else {
        return PathBuf::from(program);
    };
    find_program_on_path(program, &search_path)
}

/// Resolves a bare program name against `search_path`, trying each `PATHEXT` extension on
/// Windows so `.cmd` shims such as `pnpm` start cleanly without a shell.
#[cfg(windows)]
pub fn find_program_on_path(program: &OsStr, search_path: &OsStr) -> PathBuf {
    let bare = PathBuf::from(program);
    if bare.extension().is_some() || bare.components().count() > 1 {
        return bare;
    }
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    for folder in std::env::split_paths(search_path) {
        for extension in extensions
            .split(';')
            .filter(|extension| !extension.is_empty())
        {
            let mut name = program.to_owned();
            name.push(extension);
            let candidate = folder.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    bare
}

/// Resolves a bare program name against `search_path`, including folders discovered from
/// the user's login shell on Linux and macOS (#627).
#[cfg(not(windows))]
pub fn find_program(program: &OsStr) -> PathBuf {
    let Some(search_path) = search_path() else {
        return PathBuf::from(program);
    };
    find_program_on_path(program, &search_path)
}

#[cfg(not(windows))]
pub fn find_program_on_path(program: &OsStr, search_path: &OsStr) -> PathBuf {
    let bare = PathBuf::from(program);
    if bare.components().count() > 1 {
        return bare;
    }
    for folder in std::env::split_paths(search_path) {
        let candidate = folder.join(program);
        if is_executable_file(&candidate) {
            return candidate;
        }
    }
    bare
}

#[cfg(not(windows))]
fn is_executable_file(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Whether `program` would start: found on the search path (F15 Install checks for `npm`).
pub fn program_found(program: &str) -> bool {
    let Some(search_path) = search_path() else {
        return false;
    };
    #[cfg(windows)]
    {
        let extensions: Vec<String> = std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(str::to_owned)
            .collect();
        std::env::split_paths(&search_path).any(|folder| {
            extensions
                .iter()
                .any(|extension| folder.join(format!("{program}{extension}")).is_file())
        })
    }
    #[cfg(not(windows))]
    {
        std::env::split_paths(&search_path).any(|folder| is_executable_file(&folder.join(program)))
    }
}

/// Velocity's own `PATH`, followed by the folders installers added since Velocity started.
/// An agent installed while Velocity runs (F15 Install, or by you in a terminal) puts its
/// folder on your saved `PATH`, which a running process never sees: without these, its card
/// would say "isn't installed" until a restart, and an npm shim would not find a freshly
/// installed `node`.
pub fn search_path() -> Option<OsString> {
    let current = std::env::var_os("PATH");
    let path = merged_search_path(current.clone(), &installed_folders()).or(current);
    super::run_queue::search_path(path)
}

/// `current` followed by every folder of `extra` it doesn't already hold, or `None` when it
/// already holds them all.
fn merged_search_path(current: Option<OsString>, extra: &[PathBuf]) -> Option<OsString> {
    let mut folders: Vec<PathBuf> = current
        .as_deref()
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    let before = folders.len();
    for folder in extra {
        let key = folder_key(folder);
        if !key.is_empty() && !folders.iter().any(|known| folder_key(known) == key) {
            folders.push(folder.clone());
        }
    }
    if folders.len() == before {
        return None;
    }
    std::env::join_paths(folders).ok()
}

/// Folders compare without a trailing separator, and without case on Windows.
fn folder_key(folder: &std::path::Path) -> String {
    let text = folder.to_string_lossy();
    let trimmed = text.trim_end_matches(['\\', '/']);
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// Windows keeps your `PATH` in the registry: the machine's, then yours, as a new terminal
/// would build it.
#[cfg(windows)]
fn installed_folders() -> Vec<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    const SOURCES: [(winreg::HKEY, &str); 2] = [
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
        (HKEY_CURRENT_USER, "Environment"),
    ];
    let mut folders = Vec::new();
    for (root, key) in SOURCES {
        let Ok(value) = RegKey::predef(root)
            .open_subkey(key)
            .and_then(|key| key.get_value::<String, _>("Path"))
        else {
            continue;
        };
        let expanded = expand_variables(&value, |name| std::env::var(name).ok());
        folders.extend(std::env::split_paths(&expanded));
    }
    folders
}

/// Folders from the user's login shell `PATH` (read once at startup, since a desktop-
/// launched app skips shell startup files where nvm, fnm or volta add Node/npm), plus
/// the providers' `sh` installer folders (`~/.local/bin`, `~/.kimi-code/bin`) (#627).
#[cfg(not(windows))]
fn installed_folders() -> Vec<PathBuf> {
    let mut folders = login_shell_folders().to_vec();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        folders.push(home.join(".local").join("bin"));
        folders.push(home.join(".kimi-code").join("bin"));
    }
    folders
}

#[cfg(not(windows))]
const PATH_START_MARKER: &str = "__VELOCITY_PATH_START__";
#[cfg(not(windows))]
const PATH_END_MARKER: &str = "__VELOCITY_PATH_END__";
#[cfg(not(windows))]
const LOGIN_SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(not(windows))]
fn login_shell_folders() -> &'static [PathBuf] {
    static FOLDERS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    FOLDERS.get_or_init(probe_login_shell_path)
}

#[cfg(not(windows))]
fn probe_login_shell_path() -> Vec<PathBuf> {
    let shell = std::env::var("SHELL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_owned());
    let is_fish = std::path::Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("fish");
    if is_fish {
        let script =
            format!("printf '{PATH_START_MARKER}%s{PATH_END_MARKER}' (string join ':' $PATH)");
        return run_shell_path_probe(&shell, &["-l", "-c", &script]).unwrap_or_default();
    }
    let script = format!("printf '{PATH_START_MARKER}%s{PATH_END_MARKER}' \"$PATH\"");
    // Interactive login loads `.bashrc`/`.zshrc` (where nvm, fnm and volta live on Ubuntu
    // and macOS); fall back to non-interactive login if an interactive rc file fails.
    run_shell_path_probe(&shell, &["-i", "-l", "-c", &script])
        .or_else(|| run_shell_path_probe(&shell, &["-l", "-c", &script]))
        .unwrap_or_default()
}

/// Runs `shell` with `args` bounded by [`LOGIN_SHELL_TIMEOUT`] so a misbehaving startup
/// script can never block app launch. Unix has no console window to suppress.
#[cfg(not(windows))]
#[allow(clippy::disallowed_methods)]
fn run_shell_path_probe(shell: &str, args: &[&str]) -> Option<Vec<PathBuf>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new(shell)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < LOGIN_SHELL_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    }?;
    let bytes = reader.join().ok()?;
    if !status.success() {
        return None;
    }
    extract_marked_path(&String::from_utf8_lossy(&bytes))
}

#[cfg(not(windows))]
fn extract_marked_path(output: &str) -> Option<Vec<PathBuf>> {
    let (_, after_start) = output.split_once(PATH_START_MARKER)?;
    let (raw, _) = after_start.split_once(PATH_END_MARKER)?;
    Some(
        std::env::split_paths(OsStr::new(raw))
            .filter(|folder| !folder.as_os_str().is_empty())
            .collect(),
    )
}

/// Expands `%NAME%` the way Windows expands a saved `PATH`; an unknown name stays as written.
#[cfg_attr(not(windows), allow(dead_code))]
fn expand_variables(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match lookup(name) {
                    Some(value) => expanded.push_str(&value),
                    None => {
                        expanded.push('%');
                        expanded.push_str(name);
                        expanded.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                expanded.push('%');
                rest = after;
            }
        }
    }
    expanded.push_str(rest);
    expanded
}
