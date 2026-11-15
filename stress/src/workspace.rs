use std::{
    io,
    path::{Path, PathBuf},
};

/// Warm-pool binaries retain their template's compile-time manifest directory.
/// Only the caller's tree can identify the merge result being checked (#388).
pub fn root() -> io::Result<PathBuf> {
    let directory = std::env::current_dir()?;
    directory
        .ancestors()
        .find(|path| is_workspace(path))
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Run stress from the Velocity workspace or one of its subdirectories",
            )
        })
}

fn is_workspace(path: &Path) -> bool {
    path.join("Cargo.toml").is_file()
        && path.join("stress/src/main.rs").is_file()
        && path.join("scripts/build-queue.ps1").is_file()
        && path.join("crates").is_dir()
}
