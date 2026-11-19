use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn discover(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    visit(&root.join("stress"), &mut entries)?;
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
        file == Some(name) || stem == Some(name) || relative == Path::new(name)
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

fn visit(directory: &Path, entries: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if entry.file_type()?.is_dir() {
            // `harness` holds the harness binary and the Rust sources `.ps1` scripts run,
            // which are not scripts themselves (#499).
            if !matches!(
                name.to_str(),
                Some("src" | "harness" | "target" | "__pycache__" | ".git")
            ) {
                visit(&path, entries)?;
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
