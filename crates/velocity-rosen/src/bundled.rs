//! Rosen's binaries built into a release of Velocity (the `bundled` feature). The portable
//! download is one executable, so the binaries ride inside it and are unpacked on first
//! use into a folder named by their content. An update then unpacks its own copy beside
//! the old one, and never has to overwrite a server that is still running.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Each binary's file name and bytes. The release workflow builds them from the Rosen
/// commit `Cargo.toml` pins and names them in these variables.
const BINARIES: [(&str, &[u8]); 2] = [
    (
        "rosen-server.exe",
        include_bytes!(env!("VELOCITY_ROSEN_SERVER_EXE")),
    ),
    ("rosen.exe", include_bytes!(env!("VELOCITY_ROSEN_CLI_EXE"))),
];

/// The folder holding the unpacked binaries, unpacking them the first time. `None` when
/// they can't be written, so the caller looks elsewhere.
pub(crate) fn folder() -> Option<PathBuf> {
    static UNPACKED: OnceLock<Option<PathBuf>> = OnceLock::new();
    UNPACKED.get_or_init(unpack).clone()
}

fn unpack() -> Option<PathBuf> {
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let folder = root
        .join("velocity-rosen")
        .join(format!("{:016x}", content_hash()));
    std::fs::create_dir_all(&folder).ok()?;
    for (name, bytes) in BINARIES {
        write_once(&folder.join(name), bytes).ok()?;
    }
    Some(folder)
}

/// Writes `bytes` to `path` unless a file of that size is already there. The bytes go to
/// a temporary file first and are renamed into place, so a crash or a second Velocity
/// unpacking at the same time never leaves half a binary under the final name.
fn write_once(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|found| found.len() == bytes.len() as u64) {
        return Ok(());
    }
    let partial = path.with_extension(format!("partial-{}", std::process::id()));
    std::fs::write(&partial, bytes)?;
    match std::fs::rename(&partial, path) {
        Ok(()) => Ok(()),
        // Another Velocity renamed its copy into place first, and it may be running.
        Err(_) if std::fs::metadata(path).is_ok_and(|found| found.len() == bytes.len() as u64) => {
            let _ = std::fs::remove_file(&partial);
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            Err(error)
        }
    }
}

/// FNV-1a over every binary, so a release with other binaries unpacks into a new folder.
fn content_hash() -> u64 {
    BINARIES
        .iter()
        .flat_map(|(_, bytes)| bytes.iter())
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        })
}
