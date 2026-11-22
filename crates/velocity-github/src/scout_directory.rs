use std::io;
use std::path::{Path, PathBuf};

/// Win32's short-cwd ceiling for `CreateProcess` with room for relative paths.
#[cfg(windows)]
const WINDOWS_SHORT_PATH_LIMIT: usize = 240;

/// CreateProcess rejects a long explicit cwd even when Rust can read it, and
/// Windows tools such as Go's `//go:embed` reject `\\?\` verbatim paths even
/// when the path is short (#643). Strip the verbatim prefix when the path fits
/// under Win32's short-path ceiling, and otherwise use a private short junction
/// to preserve relative paths without changing process-wide cwd.
pub(crate) struct ReproductionDirectory {
    path: PathBuf,
    #[cfg(windows)]
    temporary: Option<tempfile::TempDir>,
}

impl ReproductionDirectory {
    pub fn new(directory: &Path) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let normal = strip_windows_verbatim(directory);
            if windows_wide_len(&normal) >= WINDOWS_SHORT_PATH_LIMIT {
                let temporary = tempfile::Builder::new()
                    .prefix("velocity-scout-")
                    .tempdir()?;
                let path = strip_windows_verbatim(&temporary.path().join("checkout"));
                junction::create(directory, &path)?;
                Ok(Self {
                    path,
                    temporary: Some(temporary),
                })
            } else {
                Ok(Self {
                    path: normal,
                    temporary: None,
                })
            }
        }
        #[cfg(not(windows))]
        Ok(Self {
            path: directory.to_owned(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn script_path(&self, directory: &Path, script: &Path) -> PathBuf {
        if let Ok(relative) = script.strip_prefix(directory) {
            return self.path.join(relative);
        }
        #[cfg(windows)]
        {
            let normal = strip_windows_verbatim(script);
            if windows_wide_len(&normal) < WINDOWS_SHORT_PATH_LIMIT {
                return normal;
            }
        }
        script.to_owned()
    }
}

#[cfg(any(windows, test))]
fn strip_windows_verbatim_str(text: &str) -> Option<String> {
    let rest = text.strip_prefix(r"\\?\")?;
    if let Some(unc) = rest
        .strip_prefix(r"UNC\")
        .or_else(|| rest.strip_prefix(r"unc\"))
    {
        return Some(format!(r"\\{unc}"));
    }
    let bytes = rest.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some(rest.to_owned());
    }
    None
}

#[cfg(windows)]
fn strip_windows_verbatim(path: &Path) -> PathBuf {
    path.to_str()
        .and_then(strip_windows_verbatim_str)
        .map_or_else(|| path.to_owned(), PathBuf::from)
}

#[cfg(windows)]
fn windows_wide_len(path: &Path) -> usize {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().count()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_windows_drive_and_unc_verbatim_prefixes_without_corrupting_volume_guids() {
        // Risk (#643): std::fs::canonicalize returns \\?\ paths on Windows, which breaks
        // Go's //go:embed pattern matching when passed as cwd or script path.
        assert_eq!(
            strip_windows_verbatim_str(r"\\?\Z:\velocity-work\collapse.sh\review-scout-1"),
            Some(r"Z:\velocity-work\collapse.sh\review-scout-1".to_owned())
        );
        assert_eq!(
            strip_windows_verbatim_str(r"\\?\UNC\server\share\repo"),
            Some(r"\\server\share\repo".to_owned())
        );
        assert_eq!(
            strip_windows_verbatim_str(r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\repo"),
            None
        );
        assert_eq!(
            strip_windows_verbatim_str(r"Z:\velocity-work\collapse.sh"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn reproduction_directory_strips_verbatim_prefix_for_short_canonical_checkout() -> io::Result<()>
    {
        // Risk (#643): short canonical Windows checkouts must reach reproduction child processes
        // without the \\?\ verbatim prefix so Go //go:embed resolves embedded SQL migrations.
        let workspace = tempfile::tempdir()?;
        let script = workspace.path().join("repro.py");
        std::fs::write(&script, b"raise SystemExit(1)\n")?;

        let canonical_dir = std::fs::canonicalize(workspace.path())?;
        let canonical_script = std::fs::canonicalize(&script)?;
        let working = ReproductionDirectory::new(&canonical_dir)?;
        let resolved_script = working.script_path(&canonical_dir, &canonical_script);

        assert!(!working.path().to_string_lossy().starts_with(r"\\?\"));
        assert!(!resolved_script.to_string_lossy().starts_with(r"\\?\"));
        assert!(resolved_script.is_file());
        Ok(())
    }
}
