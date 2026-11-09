use std::io;
use std::path::{Path, PathBuf};

/// CreateProcess rejects a long explicit cwd even when Rust can read it. A
/// private short junction preserves the checkout's relative paths without
/// changing the application's process-wide current directory.
pub(crate) struct ReproductionDirectory {
    path: PathBuf,
    #[cfg(windows)]
    temporary: Option<tempfile::TempDir>,
}

impl ReproductionDirectory {
    pub fn new(directory: &Path) -> io::Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            if directory.as_os_str().encode_wide().count() >= 240 {
                let temporary = tempfile::Builder::new()
                    .prefix("velocity-scout-")
                    .tempdir()?;
                let path = temporary.path().join("checkout");
                junction::create(directory, &path)?;
                return Ok(Self {
                    path,
                    temporary: Some(temporary),
                });
            }
        }
        Ok(Self {
            path: directory.to_owned(),
            #[cfg(windows)]
            temporary: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn script_path(&self, directory: &Path, script: &Path) -> PathBuf {
        script
            .strip_prefix(directory)
            .map(|relative| self.path.join(relative))
            .unwrap_or_else(|_| script.to_owned())
    }
}

#[cfg(windows)]
impl Drop for ReproductionDirectory {
    fn drop(&mut self) {
        if self.temporary.is_some() {
            // delete removes only this reparse point, never its target. The
            // TempDir then removes its private empty container.
            let _ = junction::delete(&self.path);
        }
    }
}
