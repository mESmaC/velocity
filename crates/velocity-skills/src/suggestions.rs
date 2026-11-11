//! Project scan skill suggestions (SPEC.md F3 step 1, F18 "Suggestions"): UI UX Pro Max when
//! the project has a UI, and Game dev skills (the router plus the detected engine's bundle)
//! when it has a game engine. Detection reads a list of project paths and a few small files,
//! so the same rules run over the working tree and over the tip of the default branch.

use std::collections::BTreeMap;
use std::path::Path;

use velocity_core::skills::SkillSuggestion;

use crate::library::run_git;

/// Deep enough for `ui/package.json` or `apps/web/package.json`, shallow enough that a scan
/// of a large repository stays quick.
const MAX_DEPTH: usize = 4;
/// Stops a walk of a huge checkout; suggestions only need the top of the tree.
const MAX_PATHS: usize = 20_000;
/// SwiftUI and Compose are found by reading source files; a few are enough.
const MAX_SOURCE_READS: usize = 200;
/// Manifests larger than this are not project manifests worth parsing.
const MAX_FILE_BYTES: u64 = 1_000_000;

/// Folders that hold dependencies, build output or other tools' state, never the project's
/// own UI or engine files. Velocity's own skill links are skipped so they never suggest.
const SKIPPED_FOLDERS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "vendor",
    "Pods",
    ".gradle",
    "Library",
    ".claude",
    ".agents",
    ".kimi-code",
    ".velocity",
];

const WEB_FRAMEWORKS: &[(&str, &str)] = &[
    ("@sveltejs/kit", "SvelteKit"),
    ("svelte", "Svelte"),
    ("next", "Next.js"),
    ("react-native", "React Native"),
    ("react", "React"),
    ("nuxt", "Nuxt"),
    ("vue", "Vue"),
    ("@angular/core", "Angular"),
    ("solid-js", "Solid"),
    ("preact", "Preact"),
    ("astro", "Astro"),
    ("@remix-run/react", "Remix"),
    ("lit", "Lit"),
    ("@builder.io/qwik", "Qwik"),
    ("ember-source", "Ember"),
];

const META_FRAMEWORKS: &[(&str, &str)] = &[
    ("SvelteKit", "Svelte"),
    ("Next.js", "React"),
    ("Remix", "React"),
    ("React Native", "React"),
    ("Nuxt", "Vue"),
];

/// The Game dev skills bundle for each web engine dependency (F18: Phaser, PixiJS, three.js).
const WEB_ENGINES: &[(&str, &str)] = &[
    ("phaser", "Phaser"),
    ("pixi.js", "PixiJS"),
    ("three", "three.js"),
];

/// Shown on a card whose pack the scan did not suggest, so you know what would.
pub const UI_HINT: &str =
    "No UI found · turns on when a web framework, Tailwind, SwiftUI, Flutter or Compose appears";
pub const ENGINE_HINT: &str = "No game engine found · turns on when project.godot, *.uproject, ProjectSettings/ or a Bevy, Phaser, PixiJS or three.js dependency appears";

/// Suggestions for the checkout at `root`, plus the committed tree at `tip` when given
/// (`origin/<default branch>` after a merge). A finding in either counts.
pub fn scan_project(root: &Path, tip: Option<&str>) -> Vec<SkillSuggestion> {
    let mut found = Vec::new();
    if let Some(reference) = tip
        && let Ok(paths) = git_tree_paths(root, reference)
    {
        found.extend(suggest(&paths, |path| git_file(root, reference, path)));
    }
    let paths = working_tree_paths(root);
    found.extend(suggest(&paths, |path| working_tree_file(root, path)));
    merge_suggestions(found)
}

/// `origin/<branch>` when the project has fetched it, without touching the network.
pub fn known_tip(root: &Path, branch: &str) -> Option<String> {
    let reference = format!("origin/{branch}");
    run_git(
        root,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
    )
    .ok()
    .map(|_| reference)
}

/// Fetches `branch` so a re-check after a merge sees the merged files even when the main
/// checkout has not pulled them yet. A project with no remote falls back to its checkout.
pub fn fetched_tip(root: &Path, branch: &str) -> Option<String> {
    // A failed fetch (offline, no remote) still leaves the last fetched tip usable.
    let _ = run_git(root, ["fetch", "--quiet", "origin", branch]);
    known_tip(root, branch)
}

/// The rules themselves: `paths` are relative, `/`-separated; `read` returns a file's text.
pub fn suggest(paths: &[String], read: impl Fn(&str) -> Option<String>) -> Vec<SkillSuggestion> {
    let manifests = PackageManifests::read(paths, &read);
    let mut suggestions = Vec::new();
    if let Some(reason) = ui_reason(paths, &manifests, &read) {
        suggestions.push(SkillSuggestion::new("uiux", vec!["uiux".into()], reason));
    }
    if let Some((bundle, reason)) = engine(paths, &manifests, &read) {
        let bundles = vec!["router".into(), bundle.into()];
        suggestions.push(SkillSuggestion::new("gamedev", bundles, reason));
    }
    suggestions
}

/// Every `package.json` in the scan with its dependency names, keyed by its folder.
struct PackageManifests {
    by_folder: BTreeMap<String, Vec<String>>,
}

impl PackageManifests {
    fn read(paths: &[String], read: &impl Fn(&str) -> Option<String>) -> Self {
        let by_folder = paths
            .iter()
            .filter(|path| file_name(path) == "package.json")
            .filter_map(|path| {
                let text = read(path)?;
                Some((folder_of(path), dependency_names(&text)))
            })
            .collect();
        Self { by_folder }
    }

    /// The first folder whose manifest depends on one of `names`, with the labels it matched.
    fn first_with(&self, names: &[(&str, &'static str)]) -> Option<(String, Vec<&'static str>)> {
        self.by_folder.iter().find_map(|(folder, dependencies)| {
            let labels: Vec<_> = names
                .iter()
                .filter(|(name, _)| dependencies.iter().any(|item| item == name))
                .map(|(_, label)| *label)
                .collect();
            (!labels.is_empty()).then(|| (folder.clone(), labels))
        })
    }

    fn has(&self, folder: &str, name: &str) -> bool {
        self.by_folder
            .get(folder)
            .is_some_and(|dependencies| dependencies.iter().any(|item| item == name))
    }
}

fn dependency_names(text: &str) -> Vec<String> {
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .filter_map(|section| manifest.get(section)?.as_object())
        .flat_map(|section| section.keys().cloned())
        .collect()
}

fn ui_reason(
    paths: &[String],
    manifests: &PackageManifests,
    read: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some((folder, mut labels)) = manifests.first_with(WEB_FRAMEWORKS) {
        // A meta-framework names its base: "SvelteKit", not "SvelteKit + Svelte".
        let bases: Vec<&str> = META_FRAMEWORKS
            .iter()
            .filter(|(meta, _)| labels.contains(meta))
            .map(|(_, base)| *base)
            .collect();
        labels.retain(|label| !bases.contains(label));
        if manifests.has(&folder, "tailwindcss") || has_tailwind_config(paths, &folder) {
            labels.push("Tailwind");
        }
        return Some(format!(
            "UI found: {} ({})",
            display_folder(&folder),
            labels.join(" + ")
        ));
    }
    if let Some(path) = paths.iter().find(|path| is_tailwind_config(path)) {
        return Some(format!("UI found: {path} (Tailwind)"));
    }
    native_ui_reason(paths, read)
}

/// A source file ending, the UI it reveals, and the test on its text.
type SourceCheck = (&'static str, &'static str, fn(&str) -> bool);

fn native_ui_reason(paths: &[String], read: &impl Fn(&str) -> Option<String>) -> Option<String> {
    let pubspec = paths.iter().find(|path| {
        file_name(path) == "pubspec.yaml" && read(path).is_some_and(|text| text.contains("flutter"))
    });
    if let Some(path) = pubspec {
        return Some(format!("UI found: {path} (Flutter)"));
    }
    let checks: [SourceCheck; 3] = [
        (".swift", "SwiftUI", |text| text.contains("import SwiftUI")),
        (".gradle", "Compose", uses_compose),
        (".kts", "Compose", uses_compose),
    ];
    let mut reads = 0;
    for path in paths {
        let Some((_, label, matches)) = checks.iter().find(|(end, ..)| path.ends_with(end)) else {
            continue;
        };
        reads += 1;
        if reads > MAX_SOURCE_READS {
            break;
        }
        if read(path).is_some_and(|text| matches(&text)) {
            return Some(format!("UI found: {path} ({label})"));
        }
    }
    None
}

fn uses_compose(text: &str) -> bool {
    text.contains("androidx.compose")
        || text.contains("org.jetbrains.compose")
        || text.contains("compose = true")
        || text.contains("compose true")
}

fn has_tailwind_config(paths: &[String], folder: &str) -> bool {
    paths
        .iter()
        .any(|path| is_tailwind_config(path) && folder_of(path) == folder)
}

fn is_tailwind_config(path: &str) -> bool {
    file_name(path).starts_with("tailwind.config.")
}

/// The one engine bundle to suggest (F18: engine bundles are exclusive), in the order the
/// spec lists the engines.
fn engine(
    paths: &[String],
    manifests: &PackageManifests,
    read: &impl Fn(&str) -> Option<String>,
) -> Option<(&'static str, String)> {
    if let Some(path) = paths.iter().find(|path| file_name(path) == "project.godot") {
        return Some(("godot", format!("Godot found: {path}")));
    }
    if let Some(path) = paths.iter().find(|path| path.ends_with(".uproject")) {
        return Some(("unreal", format!("Unreal found: {path}")));
    }
    if let Some(path) = paths.iter().find(|path| in_unity_settings(path)) {
        let folder = path.split("ProjectSettings/").next().unwrap_or_default();
        return Some(("unity", format!("Unity found: {folder}ProjectSettings/")));
    }
    if let Some((folder, labels)) = manifests.first_with(WEB_ENGINES) {
        let reason = format!(
            "{} found: {}package.json",
            labels.join(" + "),
            folder_prefix(&folder)
        );
        return Some(("web", reason));
    }
    let bevy = paths.iter().find(|path| {
        file_name(path) == "Cargo.toml" && read(path).is_some_and(|text| depends_on_bevy(&text))
    });
    bevy.map(|path| ("other", format!("Bevy found: {path}")))
}

fn in_unity_settings(path: &str) -> bool {
    path.starts_with("ProjectSettings/") || path.contains("/ProjectSettings/")
}

fn depends_on_bevy(text: &str) -> bool {
    text.lines().map(str::trim).any(|line| {
        line == "[dependencies.bevy]"
            || line
                .strip_prefix("bevy")
                .is_some_and(|rest| rest.trim_start().starts_with('=') || rest.starts_with('.'))
    })
}

/// Merges findings from the tip and the working tree: one suggestion per pack, the first
/// reason, and still only one engine bundle.
fn merge_suggestions(found: Vec<SkillSuggestion>) -> Vec<SkillSuggestion> {
    let mut merged: Vec<SkillSuggestion> = Vec::new();
    for suggestion in found {
        if !merged.iter().any(|item| item.pack == suggestion.pack) {
            merged.push(suggestion);
        }
    }
    merged
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn folder_of(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(folder, _)| folder.to_owned())
        .unwrap_or_default()
}

fn folder_prefix(folder: &str) -> String {
    if folder.is_empty() {
        String::new()
    } else {
        format!("{folder}/")
    }
}

fn display_folder(folder: &str) -> String {
    if folder.is_empty() {
        "the project root".into()
    } else {
        format!("{folder}/")
    }
}

fn skipped(path: &str) -> bool {
    path.split('/').count() > MAX_DEPTH + 1
        || path
            .split('/')
            .rev()
            .skip(1)
            .any(|part| SKIPPED_FOLDERS.contains(&part))
}

fn git_tree_paths(root: &Path, reference: &str) -> Result<Vec<String>, crate::SkillLibraryError> {
    let listing = run_git(root, ["ls-tree", "-r", "--name-only", "-z", reference])?;
    Ok(listing
        .split('\0')
        .filter(|path| !path.is_empty() && !skipped(path))
        .take(MAX_PATHS)
        .map(str::to_owned)
        .collect())
}

fn git_file(root: &Path, reference: &str, path: &str) -> Option<String> {
    run_git(root, ["show", &format!("{reference}:{path}")]).ok()
}

/// A breadth-first walk, so the shallow files that matter are listed before the cap.
fn working_tree_paths(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    let mut folders = vec![String::new()];
    let mut index = 0;
    while index < folders.len() && paths.len() < MAX_PATHS {
        let folder = folders[index].clone();
        index += 1;
        let Ok(entries) = std::fs::read_dir(root.join(&folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = format!("{}{name}", folder_prefix(&folder));
            // Junctions and symlinks are not followed: they may lead out of the project.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !SKIPPED_FOLDERS.contains(&name.as_str())
                    && relative.split('/').count() <= MAX_DEPTH
                {
                    folders.push(relative);
                }
            } else if kind.is_file() {
                paths.push(relative);
            }
        }
    }
    paths
}

fn working_tree_file(root: &Path, path: &str) -> Option<String> {
    let full = root.join(path);
    let size = std::fs::metadata(&full).ok()?.len();
    if size > MAX_FILE_BYTES {
        return None;
    }
    std::fs::read_to_string(full).ok()
}
