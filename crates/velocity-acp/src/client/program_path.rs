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
    let bare = PathBuf::from(program);
    if bare.extension().is_some() || bare.components().count() > 1 {
        return bare;
    }
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    let Some(search_path) = search_path() else {
        return bare;
    };
    for folder in std::env::split_paths(&search_path) {
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

/// Elsewhere the system's own `PATH` search already finds every executable. A command whose
/// `PATH` is set searches that one, which `spawn_owned_command` extends (`search_path`).
#[cfg(not(windows))]
pub fn find_program(program: &OsStr) -> PathBuf {
    PathBuf::from(program)
}

/// Whether `program` would start: found on the search path (F15 Install checks for `npm`).
pub fn program_found(program: &str) -> bool {
    let Some(search_path) = search_path() else {
        return false;
    };
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&search_path).any(|folder| {
        extensions
            .iter()
            .any(|extension| folder.join(format!("{program}{extension}")).is_file())
    })
}

/// Velocity's own `PATH`, followed by the folders installers added since Velocity started.
/// An agent installed while Velocity runs (F15 Install, or by you in a terminal) puts its
/// folder on your saved `PATH`, which a running process never sees: without these, its card
/// would say "isn't installed" until a restart, and an npm shim would not find a freshly
/// installed `node`.
pub fn search_path() -> Option<OsString> {
    let current = std::env::var_os("PATH");
    merged_search_path(current.clone(), &installed_folders()).or(current)
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

/// The folders the providers' own `sh` installers put agents in, which a desktop app's
/// `PATH` often lacks (F15 Install): `~/.local/bin` (Antigravity) and `~/.kimi-code/bin`.
#[cfg(not(windows))]
fn installed_folders() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    vec![
        home.join(".local").join("bin"),
        home.join(".kimi-code").join("bin"),
    ]
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
