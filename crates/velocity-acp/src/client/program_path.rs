//! Finding the program behind a bare command name.

use std::ffi::OsStr;
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
    let Some(search_path) = std::env::var_os("PATH") else {
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

/// Elsewhere the system's own `PATH` search already finds every executable.
#[cfg(not(windows))]
pub fn find_program(program: &OsStr) -> PathBuf {
    PathBuf::from(program)
}
