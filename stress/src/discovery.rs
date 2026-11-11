use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn discover(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    visit(&root.join("stress"), true, &mut entries)?;
    entries.sort();
    Ok(entries)
}

/// The discovered scripts each name picks: an exact file name, a file name without its
/// extension, or a path relative to the repository root.
pub fn select(
    root: &Path,
    entries: Vec<PathBuf>,
    names: &[String],
) -> Result<Vec<PathBuf>, String> {
    let picks = |entry: &PathBuf, name: &str| {
        let relative = entry.strip_prefix(root).unwrap_or(entry);
        let file = entry.file_name().and_then(|value| value.to_str());
        let stem = entry.file_stem().and_then(|value| value.to_str());
        file == Some(name)
            || stem == Some(name)
            || relative == Path::new(name)
            // A packaged script (`stress/<name>/Cargo.toml`) is named by its folder.
            || file == Some("Cargo.toml")
                && relative
                    .parent()
                .and_then(Path::file_name)
                .and_then(|value| value.to_str())
                == Some(name)
    };
    for name in names {
        if !entries.iter().any(|entry| picks(entry, name)) {
            return Err(format!("No stress script named {name}; see --list"));
        }
    }
    Ok(entries
        .into_iter()
        .filter(|entry| names.iter().any(|name| picks(entry, name)))
        .collect())
}

fn visit(directory: &Path, top: bool, entries: &mut Vec<PathBuf>) -> io::Result<()> {
    if !top && directory.join("Cargo.toml").is_file() {
        entries.push(directory.join("Cargo.toml"));
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if entry.file_type()?.is_dir() {
            if !matches!(
                name.to_str(),
                Some("src" | "target" | "__pycache__" | ".git")
            ) {
                visit(&path, false, entries)?;
            }
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("ps1" | "py" | "rs")
        ) {
            entries.push(path);
        }
    }
    Ok(())
}
