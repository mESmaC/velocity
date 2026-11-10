# Risk: Windows per-directory case sensitivity (fsutil setCaseSensitiveInfo, set by WSL on
# folders it creates) keeps `Api` and `api` as two directories. Two projects whose names
# differ only in ASCII case then own two distinct worktrees under one root (F5 path rule
# <root>\<project>\<branch>). ASCII folding must not merge them into one inventory row,
# one size-cache entry or one footer count. No agent/worktree is deleted.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixtureRoot = Join-Path $repositoryRoot ('.tools\pr22-case-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$harnessRoot = Join-Path $fixtureRoot 'harness'
# Setting the attribute needs write-attributes rights the repo folder may not grant, so the
# worktree root lives in the user's temp folder.
$worktreeRoot = Join-Path ([IO.Path]::GetTempPath()) ('pr22-case-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
New-Item -ItemType Directory -Path $worktreeRoot -Force | Out-Null
fsutil file setCaseSensitiveInfo $worktreeRoot enable | Out-Host
# fsutil exits 0 even when it is denied, so confirm the attribute instead.
if (-not ((fsutil file queryCaseSensitiveInfo $worktreeRoot) -match 'enabled')) {
    Write-Output 'SKIP: per-directory case sensitivity unavailable'; exit 2
}
$corePath = (Join-Path $repositoryRoot 'crates\velocity-core').Replace('\', '/')
$gitPath = (Join-Path $repositoryRoot 'crates\velocity-git').Replace('\', '/')
$manifest = @"
[package]
name = "pr22-case-sensitive-directories"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-core = { path = "$corePath" }
velocity-git = { path = "$gitPath" }
"@
$source = @'
use std::{fs, path::{Path, PathBuf}, process::exit, sync::atomic::AtomicBool};
use velocity_core::{worktree_settings::{WorktreeDisplayStatus, WorktreePreferences},
    worktree_view::{WorktreeContext, WorktreeOwner, footer_summary}};
use velocity_git::inventory::{WorktreeInventory, measure_worktree};

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn owner(identifier: u32, path: &Path) -> WorktreeOwner {
    WorktreeOwner { identifier, title: format!("thread-{identifier}"),
        path: path.to_string_lossy().into_owned(), status: WorktreeDisplayStatus::Working, fixing: false }
}

fn context(root: &Path, upper: &Path, lower: &Path) -> WorktreeContext {
    WorktreeContext { revision: 0, project_identifier: Some(1), project_name: "Api".into(),
        repository_path: None, merged_owners: vec![], owners: vec![owner(1, upper), owner(2, lower)],
        preferences: WorktreePreferences { root: root.to_string_lossy().into_owned(),
            project_override: None, delete_local_branch: true, share_build_cache: true,
            copy_files: String::new(), setup_command: String::new(), ready_count: 4 } }
}

fn check(root: &Path) -> usize {
    let upper = root.join("Api").join("command-palette");
    let lower = root.join("api").join("command-palette");
    fs::create_dir_all(&upper).expect("first project worktree");
    fs::create_dir_all(&lower).expect("second project worktree");
    fs::write(upper.join("payload"), [0_u8; 7]).expect("first payload");
    fs::write(lower.join("payload"), [0_u8; 3]).expect("second payload");
    let distinct = fs::canonicalize(&upper).expect("canonical upper") != fs::canonicalize(&lower).expect("canonical lower")
        && fs::read(upper.join("payload")).expect("read upper").len() == 7
        && fs::read(lower.join("payload")).expect("read lower").len() == 3;
    let mut failures = report("native-directories-are-distinct", distinct, "separate canonical paths and 7/3-byte payloads");
    if !distinct { return failures; }
    let context = context(root, &upper, &lower);
    let mut inventory = WorktreeInventory::default();
    for path in [&upper, &lower] {
        inventory.update_size(path.to_string_lossy().into_owned(), measure_worktree(path, &AtomicBool::new(false)));
    }
    let view = inventory.snapshot(&context);
    failures += report("both-worktrees-retain-inventory-rows", view.live_count == "2", format!("expected=2, actual={}", view.live_count));
    failures += report("both-worktrees-retain-total-bytes", view.total_size == "10 B", format!("expected=10 B, actual={}", view.total_size));
    let (count, _) = footer_summary(&context.owners, &context.preferences.root);
    failures += report("both-worktrees-retain-footer-count", count == 2, format!("expected=2, actual={count}"));
    failures
}

fn main() {
    assert!(cfg!(windows), "This reproduction requires Windows filesystem semantics");
    let root = PathBuf::from(std::env::args().nth(1).expect("case-sensitive worktree root"));
    let failures = check(&root);
    println!("failures: {failures}");
    exit(i32::from(failures != 0));
}
'@
$encoding = New-Object Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'), $manifest, $encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\main.rs'), $source, $encoding)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
$env:CARGO_TARGET_DIR = Join-Path $repositoryRoot '.tools\pr22-target'
cargo run --manifest-path (Join-Path $harnessRoot 'Cargo.toml') --offline -- $worktreeRoot
exit $LASTEXITCODE
