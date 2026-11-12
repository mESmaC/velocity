//! Plane 3: subsystems. A folder with a manifest (Cargo, npm, Go, Python) is a subsystem named
//! after its package; files outside every package belong to their top-level folder.

/// One subsystem.
#[derive(Clone, Debug)]
pub struct Subsystem {
    /// Folder relative to the root, with `/` separators; empty for the root.
    pub folder: String,
    /// The package name, or the folder.
    pub name: String,
    /// The name Rust code uses for it (`velocity-git` is `velocity_git`).
    pub crate_name: String,
    pub manifest: Option<String>,
}

const MANIFESTS: [&str; 4] = ["Cargo.toml", "package.json", "go.mod", "pyproject.toml"];

/// Whether `path` is a package manifest.
pub fn is_manifest(path: &str) -> bool {
    MANIFESTS.contains(&path.rsplit('/').next().unwrap_or(path))
}

/// The subsystems among `paths`, given each manifest's text.
pub fn detect(paths: &[String], manifest_text: impl Fn(&str) -> Option<String>) -> Vec<Subsystem> {
    let mut subsystems: Vec<Subsystem> = paths
        .iter()
        .filter_map(|path| {
            let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
            if !MANIFESTS.contains(&file) {
                return None;
            }
            let named = manifest_text(path).and_then(|text| package_name(file, &text));
            // A root manifest is a package only when it names one: a Cargo workspace root
            // with no `[package]` leaves its members to be the subsystems.
            if folder.is_empty() && named.is_none() {
                return None;
            }
            let name = named.unwrap_or_else(|| folder.to_owned());
            Some(Subsystem {
                folder: folder.to_owned(),
                crate_name: name.replace('-', "_"),
                name,
                manifest: Some(path.clone()),
            })
        })
        .collect();
    subsystems.sort_by(|left, right| left.folder.cmp(&right.folder));
    subsystems.dedup_by(|later, earlier| later.folder == earlier.folder);
    subsystems
}

/// The subsystem a path belongs to: its deepest package folder, else its top-level folder.
pub fn owner(subsystems: &[Subsystem], path: &str) -> (String, Option<usize>) {
    let package = subsystems
        .iter()
        .enumerate()
        .filter(|(_, subsystem)| {
            subsystem.folder.is_empty()
                || path
                    .strip_prefix(&subsystem.folder)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|(_, subsystem)| subsystem.folder.len());
    match package {
        Some((index, subsystem)) => (subsystem.folder.clone(), Some(index)),
        None => match path.split_once('/') {
            Some((top, _)) => (top.to_owned(), None),
            None => (String::new(), None),
        },
    }
}

fn package_name(file: &str, text: &str) -> Option<String> {
    let value = |line: &str, key: &str| {
        let rest = line.trim().strip_prefix(key)?.trim_start();
        let rest = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':'))?;
        Some(rest.trim().trim_matches([',', '"', '\'']).trim().to_owned())
    };
    match file {
        "go.mod" => text
            .lines()
            .find_map(|line| line.trim().strip_prefix("module "))
            .map(|module| {
                module
                    .trim()
                    .rsplit('/')
                    .next()
                    .unwrap_or(module)
                    .to_owned()
            }),
        "package.json" => text.lines().find_map(|line| value(line, "\"name\"")),
        // Cargo.toml and pyproject.toml: the first `name` in the `[package]` or `[project]`
        // table.
        _ => {
            let mut in_package = false;
            text.lines().find_map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with('[') {
                    in_package = trimmed == "[package]" || trimmed == "[project]";
                    return None;
                }
                if in_package {
                    value(trimmed, "name")
                } else {
                    None
                }
            })
        }
    }
    .filter(|name| !name.is_empty())
}

/// Whether `manifest` lists `dependency` among its dependencies (Cargo `name = ...` or
/// `name.workspace = true`, npm `"name": ...`).
pub fn depends(manifest: &str, dependency: &str) -> bool {
    manifest.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix(dependency).is_some_and(|rest| {
            let rest = rest.trim_start();
            rest.starts_with('=') || rest.starts_with(".workspace") || rest.starts_with(".path")
        }) || line.starts_with(&format!("\"{dependency}\":"))
    })
}
