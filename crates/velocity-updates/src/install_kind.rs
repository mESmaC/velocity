use std::path::Path;

/// How the running copy was installed, which decides the update package it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum InstallKind {
    /// The NSIS setup: an `uninstall.exe` sits next to the executable.
    Nsis,
    /// The MSI: installed under Program Files with no NSIS uninstaller.
    Msi,
    /// The portable executable, run from wherever it was saved.
    Portable,
    /// A Linux AppImage: the AppImage runtime sets the `APPIMAGE` environment variable.
    AppImage,
    /// A Linux Debian package installed under `/usr/bin` or `/usr/lib`.
    Deb,
}

impl InstallKind {
    /// The suffix of this kind's entry in the release manifest's `platforms`, after the
    /// `{os}-{arch}` base (`windows-x86_64-portable`, `linux-x86_64-appimage`).
    pub fn manifest_suffix(self) -> &'static str {
        match self {
            Self::Nsis => "nsis",
            Self::Msi => "msi",
            Self::Portable => "portable",
            Self::AppImage => "appimage",
            Self::Deb => "deb",
        }
    }

    /// The `platforms` key in `latest.json` checked for this install kind. Debian
    /// packages cannot self-update, so they check the base `{os}-{arch}` entry to detect
    /// newer releases and then direct the user to their package manager or `.deb` download.
    pub fn manifest_target(self, base: &str) -> String {
        match self {
            Self::Deb => base.to_owned(),
            _ => format!("{base}-{}", self.manifest_suffix()),
        }
    }

    /// Whether Velocity can download and apply an update itself for this install kind.
    pub fn supports_in_app_update(self) -> bool {
        !matches!(self, Self::Deb)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Nsis => "Installed with setup",
            Self::Msi => "Installed with MSI",
            Self::Portable => "Portable",
            Self::AppImage => "AppImage",
            Self::Deb => "Installed with .deb",
        }
    }
}

/// The NSIS uninstaller wins, because a per-machine NSIS install also lives under Program
/// Files. Anything else outside Program Files is a portable copy.
pub fn classify(has_nsis_uninstaller: bool, under_program_files: bool) -> InstallKind {
    if has_nsis_uninstaller {
        InstallKind::Nsis
    } else if under_program_files {
        InstallKind::Msi
    } else {
        InstallKind::Portable
    }
}

/// Linux classification: an `APPIMAGE` environment variable always identifies an AppImage;
/// an executable installed under `/usr/bin` or `/usr/lib` is a system `.deb` package;
/// anything else (such as a local build) falls back to Portable.
pub fn classify_linux(has_appimage_env: bool, executable: &Path) -> InstallKind {
    if has_appimage_env {
        InstallKind::AppImage
    } else if under_linux_system_dir(executable) {
        InstallKind::Deb
    } else {
        InstallKind::Portable
    }
}

fn under_linux_system_dir(executable: &Path) -> bool {
    let text = executable.to_string_lossy();
    text.starts_with("/usr/bin/") || text.starts_with("/usr/lib/")
}

/// Reads the executable's location and platform environment variables to classify this copy.
pub fn detect(executable: &Path) -> InstallKind {
    #[cfg(target_os = "linux")]
    {
        let has_appimage = std::env::var_os("APPIMAGE").is_some_and(|value| !value.is_empty());
        classify_linux(has_appimage, executable)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let folder = executable.parent();
        let has_uninstaller = folder.is_some_and(|folder| folder.join("uninstall.exe").is_file());
        let roots: Vec<String> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .collect();
        let under_program_files = folder.is_some_and(|folder| {
            let folder = comparable(&folder.to_string_lossy());
            roots
                .iter()
                .map(|root| comparable(root))
                .any(|root| !root.is_empty() && folder.starts_with(&format!("{root}\\")))
        });
        classify(has_uninstaller, under_program_files)
    }
}

/// Windows paths compare case-insensitively, with either slash and with or without the
/// extended-length prefix.
#[cfg(not(target_os = "linux"))]
fn comparable(path: &str) -> String {
    let path = path.replace('/', "\\");
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    path.trim_end_matches('\\').to_lowercase()
}
