# Risk: F6 cleanup must identify an owned worktree without conflating distinct
# Windows directory names. All destructive calls target this isolated fixture.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\pr50-unicode-worktree-cleanup'
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
$gitPath = (Join-Path $repositoryRoot 'crates\velocity-git').Replace('\', '/')
$manifest = @"
[package]
name = "pr50-unicode-worktree-cleanup"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-git = { path = "$gitPath" }
"@
$source = @'
use std::{fs, path::{Path, PathBuf}, process::{Command, exit}};
use velocity_git::remove_merged_worktree;

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git").current_dir(directory).args(arguments).output().expect("fixture git");
    assert!(output.status.success(), "git {arguments:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).expect("git output")
}

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn add(repository: &Path, path: &Path, branch: &str) {
    git(repository, &["worktree", "add", "-b", branch, path.to_str().expect("fixture path"), "master"]);
    fs::write(path.join("sentinel"), branch).expect("isolated sentinel");
}

fn main() {
    assert!(cfg!(windows), "This reproducer requires Windows filesystem semantics");
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    let repository = root.join("repository");
    fs::create_dir_all(&repository).expect("fixture repository");
    git(&repository, &["init", "--initial-branch", "master"]);
    git(&repository, &["-c", "user.name=PR Duty", "-c", "user.email=pr-duty@example.invalid",
        "commit", "--allow-empty", "-m", "isolated cleanup fixture"]);
    let first = root.join("case-\u{10400}").join("worktree with spaces");
    let second = root.join("case-\u{10428}").join("worktree with spaces");
    let ordinary = root.join("ordinary worktree");
    add(&repository, &first, "case-first");
    add(&repository, &second, "case-second");
    add(&repository, &ordinary, "ordinary-control");
    let canonical_root = fs::canonicalize(&root).expect("fixture root identity");
    let first_identity = fs::canonicalize(&first).expect("first identity");
    let second_identity = fs::canonicalize(&second).expect("second identity");
    assert!(first_identity.starts_with(&canonical_root) && second_identity.starts_with(&canonical_root));
    assert!(fs::canonicalize(&ordinary).expect("ordinary identity").starts_with(&canonical_root));
    let mut failures = report("native-unicode-directories-are-distinct", first_identity != second_identity,
        "U+10400 and U+10428 are different real linked worktrees");
    let listing = git(&repository, &["worktree", "list", "--porcelain", "-z"]);
    let first_position = listing.find("branch refs/heads/case-first\0").expect("first registration");
    let second_position = listing.find("branch refs/heads/case-second\0").expect("second registration");
    // Choose the later entry so the test does not depend on git's listing order.
    let (victim, branch, protected, protected_branch) = if first_position < second_position {
        (&second, "case-second", &first, "case-first")
    } else { (&first, "case-first", &second, "case-second") };
    failures += report("target-has-the-expected-branch", git(victim, &["branch", "--show-current"]).trim() == branch,
        "git independently confirms the intended owner");
    let result = remove_merged_worktree(&repository, victim, branch, false);
    failures += report("cleanup-removes-the-requested-unicode-worktree", result.is_ok() && !victim.exists(),
        format!("expected=Ok and removed, actual={result:?}, path_exists={}", victim.exists()));
    failures += report("other-unicode-worktree-survives", protected.exists()
        && fs::read_to_string(protected.join("sentinel")).expect("protected sentinel") == protected_branch,
        "the other worktree and its payload remain intact");
    let ordinary_result = remove_merged_worktree(&repository, &ordinary, "ordinary-control", true);
    failures += report("ordinary-cleanup-control", ordinary_result.is_ok() && !ordinary.exists(),
        format!("ordinary path cleanup={ordinary_result:?}"));
    println!("failures: {failures}");
    exit(i32::from(failures != 0));
}
'@
$encoding = New-Object Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'), $manifest, $encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\main.rs'), $source, $encoding)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
$env:CARGO_TARGET_DIR = Join-Path $repositoryRoot '.tools\pr50-target'
$fixtureRoot = [IO.Path]::GetFullPath((Join-Path $harnessRoot ('fixture-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))))
if (-not $fixtureRoot.StartsWith($harnessRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture path escaped this review checkout' }
cargo run --manifest-path (Join-Path $harnessRoot 'Cargo.toml') --offline -- $fixtureRoot
exit $LASTEXITCODE
