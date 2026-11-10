//! Keep Git for Windows bookkeeping below its internal MAX_PATH buffers.

#[cfg(windows)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) struct CheckoutPath {
    path: PathBuf,
    #[cfg(windows)]
    _short: Option<ShortCheckout>,
}

impl CheckoutPath {
    pub(crate) fn new(directory: &Path) -> io::Result<Self> {
        #[cfg(windows)]
        let short = ShortCheckout::new(directory)?;
        #[cfg(windows)]
        let path = short
            .as_ref()
            .map_or(directory, |short| short.link.as_path());
        #[cfg(not(windows))]
        let path = directory;
        Ok(Self {
            path: path.to_path_buf(),
            #[cfg(windows)]
            _short: short,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(windows)]
struct ShortCheckout {
    folder: PathBuf,
    link: PathBuf,
}

#[cfg(windows)]
impl ShortCheckout {
    fn new(directory: &Path) -> io::Result<Option<Self>> {
        // Git's chdir/$GIT_DIR buffers have their own MAX_PATH limit even with core.longpaths.
        // A short junction keeps Git's bookkeeping paths short without moving the checkout.
        if directory.as_os_str().len() < 200 {
            return Ok(None);
        }
        let temporary = tempfile::tempdir()?;
        let link = temporary.path().join("checkout");
        junction::create(fs::canonicalize(directory)?, &link)?;
        Ok(Some(Self {
            folder: temporary.keep(),
            link,
        }))
    }
}

#[cfg(windows)]
impl Drop for ShortCheckout {
    fn drop(&mut self) {
        // Remove the reparse point only. Never traverse the checkout during cleanup.
        if junction::delete(&self.link).is_ok() {
            let _ = fs::remove_dir(&self.link);
            let _ = fs::remove_dir(&self.folder);
        }
    }
}
