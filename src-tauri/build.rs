use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=VELOCITY_BUILD_COMMIT");
    let commit = std::env::var("VELOCITY_BUILD_COMMIT")
        .ok()
        .and_then(|value| normalize_commit(&value))
        .or_else(git_head_commit)
        .unwrap_or_default();
    println!("cargo:rustc-env=VELOCITY_BUILD_COMMIT={commit}");
    tauri_build::build();
}

fn normalize_commit(value: &str) -> Option<String> {
    let trimmed = value.trim().to_ascii_lowercase();
    let valid_len = (7..=40).contains(&trimmed.len());
    (valid_len && trimmed.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(trimmed)
}

/// Reads HEAD directly from `.git` when built inside a git checkout (without spawning
/// `git`, and emitting no `rerun-if-changed` entries in a Rosen workspace without `.git`).
fn git_head_commit() -> Option<String> {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    let root = manifest_dir.parent()?;
    let git_dir = resolve_git_dir(&root.join(".git"))?;
    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    let head = std::fs::read_to_string(&head_path).ok()?;
    let head = head.trim();
    if let Some(commit) = normalize_commit(head) {
        return Some(commit);
    }
    let reference = head.strip_prefix("ref:")?.trim();
    let common_dir = resolve_common_dir(&git_dir);
    read_git_ref(&git_dir, reference)
        .or_else(|| {
            common_dir
                .as_deref()
                .and_then(|dir| read_git_ref(dir, reference))
        })
        .or_else(|| read_packed_ref(&git_dir, reference))
        .or_else(|| {
            common_dir
                .as_deref()
                .and_then(|dir| read_packed_ref(dir, reference))
        })
}

fn resolve_git_dir(dot_git: &Path) -> Option<PathBuf> {
    if dot_git.is_dir() {
        return Some(dot_git.to_path_buf());
    }
    if !dot_git.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(dot_git).ok()?;
    let target = content.trim().strip_prefix("gitdir:")?.trim();
    let path = PathBuf::from(target);
    Some(if path.is_absolute() {
        path
    } else {
        dot_git.parent()?.join(path)
    })
}

fn resolve_common_dir(git_dir: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(git_dir.join("commondir")).ok()?;
    let target = PathBuf::from(content.trim());
    Some(if target.is_absolute() {
        target
    } else {
        git_dir.join(target)
    })
}

fn read_git_ref(dir: &Path, reference: &str) -> Option<String> {
    let path = dir.join(reference);
    if !path.is_file() {
        return None;
    }
    println!("cargo:rerun-if-changed={}", path.display());
    let content = std::fs::read_to_string(&path).ok()?;
    normalize_commit(&content)
}

fn read_packed_ref(dir: &Path, reference: &str) -> Option<String> {
    let path = dir.join("packed-refs");
    if !path.is_file() {
        return None;
    }
    println!("cargo:rerun-if-changed={}", path.display());
    let content = std::fs::read_to_string(&path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('^') {
            continue;
        }
        if let Some((commit, name)) = trimmed.split_once(' ')
            && name.trim() == reference
        {
            return normalize_commit(commit);
        }
    }
    None
}
