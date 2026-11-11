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
