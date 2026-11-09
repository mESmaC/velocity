//! Worktrees settings read models and projections (SPEC.md F5, F13).

use crate::worktree_settings::{WorktreeDisplayStatus, WorktreePreferences, is_live};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

pub struct WorktreeContext {
    pub revision: u32,
    pub project_identifier: Option<u32>,
    pub project_name: String,
    pub repository_path: Option<String>,
    pub preferences: WorktreePreferences,
    pub owners: Vec<WorktreeOwner>,
    pub merged_owners: Vec<u32>,
}

#[derive(Clone)]
pub struct WorktreeOwner {
    pub identifier: u32,
    pub title: String,
    pub path: String,
    pub status: WorktreeDisplayStatus,
    pub fixing: bool,
}

#[derive(Clone)]
pub struct DriveInfo {
    pub root: String,
    pub name: String,
    pub kind: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub slow: bool,
}

#[derive(Clone)]
pub struct WorktreeSize {
    pub bytes: Option<u64>,
    pub error: Option<String>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeDriveView {
    pub root: String,
    pub name: String,
    pub kind: String,
    pub free_label: String,
    pub used_percent: u8,
    pub selected: bool,
    pub slow: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeRowTone {
    Working,
    PullRequest,
    Fixing,
    Failed,
    Closed,
    Duty,
    Deleted,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeRowView {
    pub owner_identifier: u32,
    pub owner: String,
    pub path: String,
    pub short_path: String,
    pub size: String,
    pub label: String,
    pub tone: WorktreeRowTone,
    pub warning: String,
    pub deleted: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeSettingsView {
    pub revision: u32,
    pub project_identifier: Option<u32>,
    pub project_name: String,
    pub drive: String,
    pub folder: String,
    pub root: String,
    pub effective_root: String,
    pub preview_root: String,
    pub example_path: String,
    pub separator: String,
    pub project_override: Option<String>,
    pub delete_local_branch: bool,
    pub share_build_cache: bool,
    pub copy_files: String,
    pub setup_command: String,
    pub ready_count: u32,
    pub drives: Vec<WorktreeDriveView>,
    pub rows: Vec<WorktreeRowView>,
    pub live_count: String,
    pub total_size: String,
    pub free_space: String,
    pub footer_drive: String,
    pub location_notice: String,
    pub can_prune: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
pub enum WorktreeSettingsEdit {
    Root(String),
    Drive(String),
    Folder(String),
    OverrideEnabled(bool),
    ProjectRoot(String),
    DeleteLocalBranch(bool),
    ShareBuildCache(bool),
    CopyFiles(String),
    SetupCommand(String),
    ReadyCount(u32),
}

pub fn split_location(root: &str) -> (String, String) {
    let path = Path::new(root);
    if let Some(Component::Prefix(prefix)) = path.components().next() {
        let drive = prefix.as_os_str().to_string_lossy().into_owned();
        let folder = root.get(drive.len()..).unwrap_or("\\").to_owned();
        return (drive, folder);
    }
    (String::new(), root.to_owned())
}

pub fn join_location(drive: &str, folder: &str) -> String {
    if drive.is_empty() {
        folder.to_owned()
    } else {
        format!("{drive}\\{}", folder.trim_start_matches(['\\', '/']))
    }
}

pub fn path_key(path: &str) -> String {
    if cfg!(windows) {
        let normalized = path.replace('/', "\\");
        let normalized = normalized
            .strip_prefix("\\\\?\\UNC\\")
            .map(|suffix| format!("\\\\{suffix}"))
            .unwrap_or_else(|| {
                normalized
                    .strip_prefix("\\\\?\\")
                    .unwrap_or(&normalized)
                    .to_owned()
            });
        normalized.to_ascii_lowercase()
    } else {
        path.to_owned()
    }
}

pub fn format_bytes(bytes: u128) -> String {
    for (unit, label) in [(1_000_000_000_000, "TB"), (1_000_000_000, "GB")] {
        if bytes >= unit {
            return format!("{}.{} {label}", bytes / unit, bytes % unit * 10 / unit);
        }
    }
    if bytes >= 1_000_000 {
        format!("{} MB", bytes / 1_000_000)
    } else if bytes >= 1_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{bytes} B")
    }
}

fn drive_view(drive: &DriveInfo, selected_root: &str) -> WorktreeDriveView {
    let used = drive.total_bytes.saturating_sub(drive.free_bytes);
    let percent = u128::from(used) * 100 / u128::from(drive.total_bytes.max(1));
    WorktreeDriveView {
        root: drive.root.clone(),
        name: drive.name.clone(),
        kind: drive.kind.clone(),
        free_label: format!("{} free", format_bytes(u128::from(drive.free_bytes))),
        used_percent: u8::try_from(percent.min(100)).unwrap_or(100),
        selected: path_key(selected_root).starts_with(&path_key(&drive.root)),
        slow: drive.slow,
    }
}

fn row_presentation(owner: &WorktreeOwner) -> (String, WorktreeRowTone) {
    match owner.status {
        WorktreeDisplayStatus::Working => ("working".into(), WorktreeRowTone::Working),
        WorktreeDisplayStatus::PullRequest(number) => (
            format!("PR #{number}"),
            if owner.fixing {
                WorktreeRowTone::Fixing
            } else {
                WorktreeRowTone::PullRequest
            },
        ),
        WorktreeDisplayStatus::Failed => ("failed · kept".into(), WorktreeRowTone::Failed),
        WorktreeDisplayStatus::Closed => ("closed · kept".into(), WorktreeRowTone::Closed),
        WorktreeDisplayStatus::Review => ("review".into(), WorktreeRowTone::Duty),
        WorktreeDisplayStatus::Sweep => ("sweep".into(), WorktreeRowTone::Duty),
        WorktreeDisplayStatus::Deleted => ("deleted · merged".into(), WorktreeRowTone::Deleted),
    }
}

pub fn row_view(owner: &WorktreeOwner, measurement: Option<&WorktreeSize>) -> WorktreeRowView {
    let (label, tone) = row_presentation(owner);
    let (drive, _) = split_location(&owner.path);
    let mut suffix: Vec<_> = Path::new(&owner.path)
        .components()
        .rev()
        .take(2)
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    suffix.reverse();
    WorktreeRowView {
        owner_identifier: owner.identifier,
        owner: owner.title.clone(),
        path: owner.path.clone(),
        short_path: format!("{drive}\\…\\{}", suffix.join("\\")),
        size: match measurement {
            Some(size) => size
                .bytes
                .map(|bytes| format_bytes(u128::from(bytes)))
                .unwrap_or_else(|| "unavailable".into()),
            None => "measuring…".into(),
        },
        label,
        tone,
        warning: measurement
            .and_then(|size| size.error.clone())
            .unwrap_or_default(),
        deleted: !is_live(owner.status),
    }
}

pub fn live_owners(context: &WorktreeContext) -> Vec<&WorktreeOwner> {
    let mut paths = BTreeSet::new();
    context
        .owners
        .iter()
        .filter(|owner| is_live(owner.status) && paths.insert(path_key(&owner.path)))
        .collect()
}

pub fn settings_view(
    context: &WorktreeContext,
    drives: &[DriveInfo],
    sizes: &BTreeMap<String, WorktreeSize>,
    deleted: &[WorktreeRowView],
    location_changed: bool,
) -> WorktreeSettingsView {
    let preferences = &context.preferences;
    let effective_root = effective_project_root(context);
    let live = live_owners(context);
    let mut rows: Vec<_> = live
        .iter()
        .map(|owner| row_view(owner, sizes.get(&path_key(&owner.path))))
        .collect();
    rows.extend_from_slice(deleted);
    let (drive, folder) = split_location(&preferences.root);
    WorktreeSettingsView {
        revision: 0,
        project_identifier: context.project_identifier,
        project_name: context.project_name.clone(),
        drive,
        folder,
        root: preferences.root.clone(),
        effective_root: effective_root.clone(),
        preview_root: preview_prefix(&preferences.root),
        example_path: example_path(context),
        separator: std::path::MAIN_SEPARATOR.to_string(),
        project_override: preferences.project_override.clone(),
        delete_local_branch: preferences.delete_local_branch,
        share_build_cache: preferences.share_build_cache,
        copy_files: preferences.copy_files.clone(),
        setup_command: preferences.setup_command.clone(),
        ready_count: preferences.ready_count,
        drives: drives
            .iter()
            .map(|info| drive_view(info, &preferences.root))
            .collect(),
        live_count: live.len().to_string(),
        total_size: total_size(&live, sizes),
        free_space: free_space(&effective_root, drives),
        footer_drive: footer_drive(&live, &effective_root),
        location_notice: location_notice(&effective_root, &live, location_changed),
        can_prune: context.repository_path.is_some(),
        rows,
    }
}

fn effective_project_root(context: &WorktreeContext) -> String {
    context
        .preferences
        .project_override
        .clone()
        .unwrap_or_else(|| {
            Path::new(&context.preferences.root)
                .join(&context.project_name)
                .to_string_lossy()
                .into_owned()
        })
}

fn preview_prefix(root: &str) -> String {
    format!(
        "{}{}",
        root.trim_end_matches(['\\', '/']),
        std::path::MAIN_SEPARATOR
    )
}

fn example_path(context: &WorktreeContext) -> String {
    Path::new(&context.preferences.root)
        .join(&context.project_name)
        .join("command-palette")
        .to_string_lossy()
        .into_owned()
}

fn location_notice(root: &str, owners: &[&WorktreeOwner], changed: bool) -> String {
    let destination = path_key(root).trim_end_matches(['\\', '/']).to_owned();
    let existing_elsewhere = owners.iter().any(|owner| {
        Path::new(&owner.path).parent().is_some_and(|parent| {
            path_key(&parent.to_string_lossy()).trim_end_matches(['\\', '/']) != destination
        })
    });
    if !changed || !existing_elsewhere {
        return String::new();
    }
    let count = owners.len();
    format!(
        "New worktrees go to {root}. The {count} that exist stay where they are until their PRs merge."
    )
}

fn total_size(owners: &[&WorktreeOwner], sizes: &BTreeMap<String, WorktreeSize>) -> String {
    // A finite owner slice of u64 measurements fits u128 on 32/64-bit hosts.
    let total = owners.iter().try_fold(0_u128, |total, owner| {
        sizes
            .get(&path_key(&owner.path))
            .and_then(|size| size.bytes)
            .map(|bytes| total + u128::from(bytes))
    });
    total
        .map(format_bytes)
        .unwrap_or_else(|| "size unavailable".into())
}

fn footer_drive(owners: &[&WorktreeOwner], fallback: &str) -> String {
    let drives: BTreeSet<_> = owners
        .iter()
        .map(|owner| split_location(&owner.path).0)
        .collect();
    if drives.len() > 1 {
        format!("{} drives", drives.len())
    } else {
        drives
            .into_iter()
            .next()
            .unwrap_or_else(|| split_location(fallback).0)
    }
}

fn free_space(root: &str, drives: &[DriveInfo]) -> String {
    drives
        .iter()
        .filter(|drive| path_key(root).starts_with(&path_key(&drive.root)))
        .max_by_key(|drive| drive.root.len())
        .map(|drive| {
            format!(
                "{} {} free",
                split_location(&drive.root).0,
                format_bytes(u128::from(drive.free_bytes))
            )
        })
        .unwrap_or_else(|| "Free space unavailable".into())
}
