//! Game engine detection for the Game dev skills pack (SPEC.md F18): `project.godot`,
//! `*.uproject`, `ProjectSettings/`, or a Bevy, Phaser, PixiJS or three.js dependency picks the
//! engine bundle the router loads. Reading the project is kept apart from the pure
//! classification so the rules can be checked against plain file lists.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::bundles::GAMEDEV_ROUTER_BUNDLE;

/// What the scan found in a project's checkout: relative paths of the files and folders at its
/// root and one level down, and the names of its package and crate dependencies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectMarkers {
    pub entries: Vec<ProjectEntry>,
    pub package_dependencies: Vec<String>,
    pub crate_dependencies: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    /// Forward-slash path relative to the checkout.
    pub path: String,
    pub is_directory: bool,
}

impl ProjectEntry {
    pub fn file(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            is_directory: false,
        }
    }

    pub fn directory(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            is_directory: true,
        }
    }

    fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// The engine the scan found, and the bundles Game dev skills suggests for it.
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameEngineDetection {
    #[serde(default)]
    pub found: bool,
    /// Display name, e.g. `Godot`.
    #[serde(default)]
    pub engine: String,
    /// The engine's bundle in the Game dev skills pack.
    #[serde(default)]
    pub bundle_id: String,
    /// The file or dependency that gave the engine away, e.g. `project.godot`.
    #[serde(default)]
    pub evidence: String,
    /// The router plus the engine's bundle. Engine bundles are exclusive: only the detected
    /// engine's is suggested.
    #[serde(default)]
    pub suggested_bundles: Vec<String>,
    /// The line the pack's card shows.
    #[serde(default)]
    pub summary: String,
}

const NOT_FOUND_SUMMARY: &str = "No game engine found · turns on when project.godot, *.uproject, ProjectSettings/ or a Bevy, Phaser or three.js dependency appears";

// Checked in order: an engine's own project file is stronger evidence than a library
// dependency, which a web app may carry for one 3D widget.
type EntryRule = fn(&ProjectEntry) -> bool;

const FILE_RULES: &[(&str, &str, EntryRule)] = &[
    ("Godot", "godot", |entry| {
        !entry.is_directory && entry.name() == "project.godot"
    }),
    ("Unreal", "unreal", |entry| {
        !entry.is_directory && entry.name().to_ascii_lowercase().ends_with(".uproject")
    }),
    ("Unity", "unity", |entry| {
        entry.is_directory && entry.name() == "ProjectSettings"
    }),
];

const CRATE_RULES: &[(&str, &str, &str)] = &[("Bevy", "other", "bevy")];

const PACKAGE_RULES: &[(&str, &str, &str)] = &[
    ("Phaser", "web", "phaser"),
    ("PixiJS", "web", "pixi.js"),
    ("PixiJS", "web", "@pixi/"),
    ("three.js", "web", "three"),
];

/// Classifies a project from its markers. Pure: the same markers always give the same answer.
pub fn classify_game_engine(markers: &ProjectMarkers) -> GameEngineDetection {
    for (engine, bundle, matches) in FILE_RULES {
        if let Some(entry) = markers.entries.iter().find(|entry| matches(entry)) {
            let shown = if entry.is_directory {
                format!("{}/", entry.path)
            } else {
                entry.path.clone()
            };
            return found(engine, bundle, shown);
        }
    }
    for (engine, bundle, name) in CRATE_RULES {
        if markers
            .crate_dependencies
            .iter()
            .any(|dependency| dependency == name)
        {
            return found(engine, bundle, format!("{name} crate"));
        }
    }
    for (engine, bundle, name) in PACKAGE_RULES {
        let matched =
            markers
                .package_dependencies
                .iter()
                .find(|dependency| match name.strip_suffix('/') {
                    Some(scope) => dependency.starts_with(&format!("{scope}/")),
                    None => dependency.as_str() == *name,
                });
        if let Some(dependency) = matched {
            return found(engine, bundle, format!("{dependency} dependency"));
        }
    }
    GameEngineDetection {
        summary: NOT_FOUND_SUMMARY.to_owned(),
        ..GameEngineDetection::default()
    }
}

fn found(engine: &str, bundle: &str, evidence: String) -> GameEngineDetection {
    GameEngineDetection {
        found: true,
        engine: engine.to_owned(),
        bundle_id: bundle.to_owned(),
        summary: format!("{engine} found: {evidence}"),
        evidence,
        suggested_bundles: vec![GAMEDEV_ROUTER_BUNDLE.to_owned(), bundle.to_owned()],
    }
}

/// Scans a checkout and classifies it. A folder that can't be read counts as having no engine:
/// the result only drives a suggestion, never a gate.
pub fn detect_game_engine(checkout: &Path) -> GameEngineDetection {
    classify_game_engine(&read_project_markers(checkout))
}

/// Reads the checkout's root and the first level of its folders, skipping `.git`, dot-folders
/// and dependency or build folders, which hold other projects' markers.
pub fn read_project_markers(checkout: &Path) -> ProjectMarkers {
    let mut markers = ProjectMarkers::default();
    let root_entries = list_entries(checkout, "");
    for entry in &root_entries {
        if entry.is_directory && !skipped_folder(entry.name()) {
            let nested = list_entries(&checkout.join(&entry.path), &entry.path);
            read_manifests(&checkout.join(&entry.path), &mut markers);
            markers.entries.extend(nested);
        }
    }
    read_manifests(checkout, &mut markers);
    markers.entries.extend(root_entries);
    markers.entries.sort_by(|left, right| {
        let depth = |entry: &ProjectEntry| entry.path.matches('/').count();
        depth(left)
            .cmp(&depth(right))
            .then(left.path.cmp(&right.path))
    });
    markers
}

fn skipped_folder(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "node_modules" | "target" | "dist" | "build" | "vendor"
        )
}

fn list_entries(folder: &Path, prefix: &str) -> Vec<ProjectEntry> {
    let Ok(read) = fs::read_dir(folder) else {
        return Vec::new();
    };
    read.flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_directory = entry.file_type().ok()?.is_dir();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            Some(ProjectEntry { path, is_directory })
        })
        .collect()
}

fn read_manifests(folder: &Path, markers: &mut ProjectMarkers) {
    if let Ok(text) = fs::read_to_string(folder.join("package.json")) {
        markers
            .package_dependencies
            .extend(package_dependencies(&text));
    }
    if let Ok(text) = fs::read_to_string(folder.join("Cargo.toml")) {
        markers.crate_dependencies.extend(crate_dependencies(&text));
    }
}

/// Dependency names from a `package.json`'s `dependencies`, `devDependencies` and
/// `peerDependencies`. A file that doesn't parse has none.
pub fn package_dependencies(package_json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(package_json) else {
        return Vec::new();
    };
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .filter_map(|field| value.get(field).and_then(serde_json::Value::as_object))
        .flat_map(|map| map.keys().cloned())
        .collect()
}

/// Crate names a `Cargo.toml` depends on, from `name = ...` lines in dependency tables and
/// `[dependencies.name]` headers. A line scan is enough to spot an engine crate.
pub fn crate_dependencies(cargo_toml: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_dependencies = false;
    for line in cargo_toml.lines().map(str::trim) {
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            let header = header.trim();
            in_dependencies = header.ends_with("dependencies");
            if let Some((table, name)) = header.rsplit_once('.')
                && table.ends_with("dependencies")
            {
                names.push(name.trim().to_owned());
            }
            continue;
        }
        if in_dependencies && let Some((key, _)) = line.split_once('=') {
            let key = key.trim();
            if !key.is_empty() && !key.starts_with('#') {
                names.push(key.to_owned());
            }
        }
    }
    names
}
