# Risk (#293): on Windows, `git worktree remove --force` stops at a file another process
# holds open after it has already deleted the worktree's `.git` file and registration.
# F6 merged cleanup then refused the leftover folder as "not registered" on every pass, so
# a merged thread's worktree was never removed. Cleanup must fail while the file is held,
# remove the folder once it is released, and still refuse an unregistered folder that
# holds a `.git` entry. All destructive calls target this isolated fixture.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\issue-293-merged-leftover'
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
$gitPath = (Join-Path $repositoryRoot 'crates\velocity-git').Replace('\', '/')
$manifest = @"
[package]
name = "issue-293-merged-leftover"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-git = { path = "$gitPath" }
"@
$source = @'
use std::{fs, os::windows::fs::OpenOptionsExt, path::{Path, PathBuf}, process::{Command, exit}};
use velocity_git::remove_merged_worktree;

const IDENTITY: [&str; 4] = ["-c", "user.name=PR Duty", "-c", "user.email=pr-duty@example.invalid"];

fn git(directory: &Path, arguments: &[&str]) -> (bool, String) {
    let output = Command::new("git").current_dir(directory).args(arguments).output().expect("fixture git");
    (output.status.success(), String::from_utf8_lossy(&output.stdout).into_owned())
}

fn must(directory: &Path, arguments: &[&str]) -> String {
    let (success, output) = git(directory, arguments);
    assert!(success, "git {arguments:?} failed");
    output
}

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

/// A merged thread's worktree: its branch committed and pushed, plus a nested folder.
fn add(repository: &Path, path: &Path, branch: &str) {
    must(repository, &["worktree", "add", "-b", branch, path.to_str().expect("fixture path"), "master"]);
    fs::create_dir_all(path.join("held")).expect("held folder");
    fs::write(path.join("held").join("open.txt"), branch).expect("held file");
    must(path, &["add", "."]);
    must(path, &[&IDENTITY[..], &["commit", "-m", "work"]].concat());
    must(path, &["push", "origin", branch]);
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    let repository = root.join("repository");
    fs::create_dir_all(&repository).expect("fixture repository");
    must(&repository, &["init", "--initial-branch", "master"]);
    must(&repository, &[&IDENTITY[..], &["commit", "--allow-empty", "-m", "fixture"]].concat());
    let remote = root.join("remote.git");
    must(&root, &["init", "--bare", remote.to_str().expect("remote path")]);
    must(&repository, &["remote", "add", "origin", remote.to_str().expect("remote path")]);
    must(&repository, &["push", "origin", "master"]);
    let worktree = root.join("thread worktree");
    add(&repository, &worktree, "merged-thread");

    // An exclusive handle stands in for an editor or build tool holding the file.
    let held = fs::OpenOptions::new().read(true).share_mode(0)
        .open(worktree.join("held").join("open.txt")).expect("hold file");
    let (removed, _) = git(&repository, &["worktree", "remove", "--force", worktree.to_str().expect("path")]);
    let listing = must(&repository, &["worktree", "list", "--porcelain"]);
    let mut failures = report("git-remove-is-interrupted",
        !removed && worktree.exists() && !worktree.join(".git").exists() && !listing.contains("merged-thread"),
        format!("git succeeded={removed}, folder={}, .git={}", worktree.exists(), worktree.join(".git").exists()));
    let while_held = remove_merged_worktree(&repository, &worktree, "merged-thread", true);
    failures += report("cleanup-fails-while-file-is-held", while_held.is_err() && worktree.exists(),
        format!("{while_held:?}"));
    drop(held);
    let released = remove_merged_worktree(&repository, &worktree, "merged-thread", true);
    let (branch_left, _) = git(&repository, &["show-ref", "--verify", "refs/heads/merged-thread"]);
    failures += report("cleanup-recovers-once-released", released.is_ok() && !worktree.exists() && !branch_left,
        format!("{released:?}, folder={}, branch={branch_left}", worktree.exists()));

    // Control: an unregistered folder with a `.git` entry anywhere below it is kept.
    let foreign = root.join("foreign folder");
    fs::create_dir_all(foreign.join("nested")).expect("foreign folder");
    must(&foreign.join("nested"), &["init"]);
    let foreign_result = remove_merged_worktree(&repository, &foreign, "merged-thread", true);
    failures += report("folder-with-git-entry-is-kept", foreign_result.is_err() && foreign.join("nested").join(".git").exists(),
        format!("{foreign_result:?}"));
    println!("failures: {failures}");
    exit(i32::from(failures != 0));
}
'@
$encoding = New-Object Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'), $manifest, $encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\main.rs'), $source, $encoding)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $repositoryRoot 'target' }
$fixtureRoot = [IO.Path]::GetFullPath((Join-Path $harnessRoot ('fixture-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))))
if (-not $fixtureRoot.StartsWith($harnessRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture path escaped this checkout' }
cargo run --manifest-path (Join-Path $harnessRoot 'Cargo.toml') --offline -- $fixtureRoot
exit $LASTEXITCODE
