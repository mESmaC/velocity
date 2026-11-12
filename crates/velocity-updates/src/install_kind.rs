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
}

impl InstallKind {
    /// The suffix of this kind's entry in the release manifest's `platforms`, after the
    /// `{os}-{arch}` base (`windows-x86_64-portable`).
    pub fn manifest_suffix(self) -> &'static str {
        match self {
            Self::Nsis => "nsis",
            Self::Msi => "msi",
            Self::Portable => "portable",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Nsis => "Installed with setup",
            Self::Msi => "Installed with MSI",
            Self::Portable => "Portable",
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

/// Reads the executable's folder and the Program Files locations from the environment.
pub fn detect(executable: &Path) -> InstallKind {
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

/// Windows paths compare case-insensitively, with either slash and with or without the
/// extended-length prefix.
fn comparable(path: &str) -> String {
    let path = path.replace('/', "\\");
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    path.trim_end_matches('\\').to_lowercase()
}
