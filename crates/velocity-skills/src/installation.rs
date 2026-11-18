//! Resumable installation of an unregistered pack. Only publication changes the registry.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use process_wrap::tokio::{CommandWrap, KillOnDrop};

use crate::library::{
    discover_skills, materialize_symlink_pointers, resolve_commit, run_git, strip_verbatim_prefix,
};
use crate::{InstalledSkillPack, SkillLibrary, SkillLibraryError, resolve_pack_source};

#[derive(Clone, Copy, Debug)]
pub enum SkillPackInstallStep {
    Cloning,
    CheckingOut,
    Pinning,
    Discovering,
    Registering,
}

impl SkillLibrary {
    /// Installs once, using the remote tip for a new pack when no initial pin was supplied.
    pub fn install_pack(
        &self,
        pack_or_repo: &str,
        source_override: Option<&str>,
        initial_pin: Option<&str>,
    ) -> Result<InstalledSkillPack, SkillLibraryError> {
        self.install_pack_with_progress(pack_or_repo, source_override, initial_pin, |_| {})
    }

    pub fn install_pack_with_progress(
        &self,
        pack_or_repo: &str,
        source_override: Option<&str>,
        initial_pin: Option<&str>,
        mut progress: impl FnMut(SkillPackInstallStep),
    ) -> Result<InstalledSkillPack, SkillLibraryError> {
        let (catalog, remote) = resolve_pack_source(pack_or_repo, source_override)?;
        let _lease = self.lock_installation(&catalog.id)?;
        match self.installed_pack(&catalog.id) {
            Ok(pack) if pack.repository.eq_ignore_ascii_case(&catalog.repository) => {
                return Ok(pack);
            }
            Ok(_) => return Err(SkillLibraryError::PackIdentifierInUse(catalog.id)),
            Err(SkillLibraryError::PackNotInstalled(_)) => {}
            Err(error) => return Err(error),
        }
        let initial_pin = initial_pin.map(str::trim).filter(|pin| !pin.is_empty());
        let destination = self.pack_directory(&catalog.id);
        let staging = self.pack_directory(&format!(".install-{}", catalog.id));
        progress(SkillPackInstallStep::Cloning);
        let checkout = prepare_checkout(self.root(), &destination, &staging, &remote, initial_pin)?;
        progress(SkillPackInstallStep::CheckingOut);
        let target = resolve_commit(&checkout, initial_pin.unwrap_or("HEAD"))?;
        checkout_commit(&checkout, &target)?;
        if checkout != destination {
            fs::rename(&checkout, &destination)
                .map_err(|source| file_error(&destination, source))?;
        }
        progress(SkillPackInstallStep::Pinning);
        let pinned_commit = resolve_commit(&destination, "HEAD")?;
        progress(SkillPackInstallStep::Discovering);
        // Junction targets are absolute on Windows, so create them at the final path.
        materialize_symlink_pointers(&destination)?;
        let skills = discover_skills(&destination, &catalog.name)?;
        let installed =
            InstalledSkillPack::from_checkout(catalog, remote, pinned_commit, &destination, skills);
        progress(SkillPackInstallStep::Registering);
        self.upsert_installed(installed.clone())?;
        Ok(installed)
    }

    pub(crate) fn lock_installation(&self, pack: &str) -> Result<File, SkillLibraryError> {
        let path = self.pack_directory(&format!(".install-{pack}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|source| file_error(&path, source))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                Err(SkillLibraryError::InstallInProgress(pack.to_owned()))
            }
            Err(std::fs::TryLockError::Error(source)) => Err(file_error(&path, source)),
        }
    }
}

fn prepare_checkout(
    library_root: &Path,
    destination: &Path,
    staging: &Path,
    remote: &str,
    initial_pin: Option<&str>,
) -> Result<PathBuf, SkillLibraryError> {
    // An older install could have stopped at the final path before it wrote the registry.
    for folder in [destination, staging] {
        let reusable = folder.join(".git").is_dir()
            && run_git(folder, ["config", "--local", "--get", "remote.origin.url"])
                .is_ok_and(|url| url.trim_end() == remote)
            && resolve_commit(folder, initial_pin.unwrap_or("HEAD")).is_ok();
        if reusable {
            return Ok(folder.to_path_buf());
        }
        match fs::remove_dir_all(folder) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(file_error(folder, source)),
        }
    }
    let mut arguments = vec![
        "clone",
        "--no-checkout",
        "--config",
        "core.symlinks=true",
        "--config",
        "core.longpaths=true",
    ];
    // Explicit revisions may be older or on another branch. Keep their full ref history.
    if initial_pin.is_none() {
        arguments.extend(["--depth=1", "--single-branch"]);
    }
    let destination_argument = strip_verbatim_prefix(staging);
    arguments.extend(["--", remote, &destination_argument]);
    run_install_git(library_root, &arguments)?;
    Ok(staging.to_owned())
}

fn checkout_commit(folder: &Path, commit: &str) -> Result<(), SkillLibraryError> {
    // Git can leave internal lock files behind. The pack lease excludes other installers.
    for name in ["HEAD.lock", "index.lock", "config.lock", "shallow.lock"] {
        let lock = folder.join(".git").join(name);
        match fs::remove_file(&lock) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(file_error(&lock, source)),
        }
    }
    run_git(
        folder,
        [
            "config",
            "--local",
            "--replace-all",
            "core.symlinks",
            "true",
        ],
    )?;
    #[cfg(windows)]
    crate::library::remove_pack_junctions(folder)?;
    crate::library::checkout_with_symlinks(commit, |arguments| run_install_git(folder, arguments))
}

// The Windows job wrapper replaces the flags set by hidden_command, so repeat CREATE_NO_WINDOW.
#[allow(clippy::disallowed_types)]
fn run_install_git(folder: &Path, arguments: &[&str]) -> Result<(), SkillLibraryError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| file_error(folder, source))?;
    let operation = if arguments.starts_with(&["clone"]) {
        "clone"
    } else {
        "checkout"
    };
    runtime.block_on(async {
        let mut command = tokio::process::Command::from(crate::process::hidden_command("git"));
        command
            .current_dir(folder)
            .args(["-c", "core.longpaths=true"])
            .args(arguments)
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut command = CommandWrap::from(command);
        command.wrap(KillOnDrop);
        #[cfg(windows)]
        {
            use process_wrap::tokio::{CreationFlags, JobObject};
            use windows::Win32::System::Threading::CREATE_NO_WINDOW;
            command
                .wrap(CreationFlags(CREATE_NO_WINDOW))
                .wrap(JobObject);
        }
        #[cfg(unix)]
        command.wrap(process_wrap::tokio::ProcessGroup::leader());
        let child = command
            .spawn()
            .map_err(|source| file_error(folder, source))?;
        git_result(operation, Box::into_pin(child.wait_with_output())).await
    })
}

async fn git_result(
    operation: &str,
    output: impl Future<Output = io::Result<std::process::Output>>,
) -> Result<(), SkillLibraryError> {
    let stderr = match tokio::time::timeout(Duration::from_secs(5 * 60), output).await {
        Ok(Ok(output)) if output.status.success() => return Ok(()),
        Ok(Ok(output)) => String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        Ok(Err(error)) => error.to_string(),
        Err(_) => format!("skill pack {operation} timed out after 5 minutes"),
    };
    Err(SkillLibraryError::GitCommand {
        command: format!("git {operation}"),
        stderr,
    })
}

fn file_error(path: &Path, source: io::Error) -> SkillLibraryError {
    SkillLibraryError::Io {
        path: path.to_path_buf(),
        source,
    }
}
