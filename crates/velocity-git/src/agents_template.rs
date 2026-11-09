//! Non-destructive installation of the default project instructions (F11).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, thiserror::Error)]
#[error("could not install default AGENTS.md in {folder}: {source}")]
pub struct AgentsTemplateError {
    folder: PathBuf,
    #[source]
    source: io::Error,
}

/// Returns true only for a newly installed file. Existing files, directories and
/// symbolic links are left alone, including empty files and dangling links.
pub fn install_default_agents(
    folder: &Path,
    project_name: &str,
) -> Result<bool, AgentsTemplateError> {
    install(folder, project_name).map_err(|source| AgentsTemplateError {
        folder: folder.to_owned(),
        source,
    })
}

fn install(folder: &Path, project_name: &str) -> io::Result<bool> {
    // Canonicalization supplies the extended Windows prefix for long-path I/O.
    let folder = fs::canonicalize(folder)?;
    let destination = folder.join("AGENTS.md");
    if exists(&destination)? {
        return Ok(false);
    }
    let (temporary, mut file) = temporary_file(&folder)?;
    file.write_all(
        velocity_core::agents_template::default_agents_markdown(project_name).as_bytes(),
    )?;
    file.sync_all()?;
    drop(file);
    // A hard link publishes complete bytes atomically without replacing a file
    // another caller created after the initial check. Both paths share a volume.
    match fs::hard_link(&temporary.0, &destination) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn temporary_file(folder: &Path) -> io::Result<(TemporaryFile, fs::File)> {
    loop {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let path = folder.join(format!(
            ".velocity-agents-{}-{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((TemporaryFile(path), file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
