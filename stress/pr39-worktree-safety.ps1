param([string]$TargetDirectory = $env:CARGO_TARGET_DIR)

# Risk: worktree creation failures must report long paths and preserve branches owned by others.
# The shim injects one deterministic concurrent branch creation, then delegates to real Git.
# All repositories, branches and worktrees used here are fresh fixtures under this checkout.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not $TargetDirectory) { $TargetDirectory = Join-Path $repositoryRoot '.tools\pr39-cargo-target' }
$fixtureRoot = Join-Path $repositoryRoot ('.tools\pr39-safety-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$shimDirectory = Join-Path $fixtureRoot 'shim'
New-Item -ItemType Directory -Path $shimDirectory -Force | Out-Null
$realGit = (Get-Command git.exe -ErrorAction Stop).Source
$encoding = New-Object Text.UTF8Encoding($false)
$proxySource = @'
use std::{env, process::{Command, exit}};

fn main() {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let mode = env::var("PR39_MODE").unwrap_or_default();
    let real_git = env::var("PR39_REAL_GIT").expect("real Git path");
    if mode == "probe-error" && arguments.iter().any(|item| item == "show-ref") {
        eprintln!("fatal: injected show-ref I/O failure");
        exit(128);
    }
    let adding = arguments.windows(2).any(|pair| pair == ["worktree", "add"]);
    if mode == "race" && adding {
        let branch = env::var("PR39_BRANCH").expect("race branch");
        let tip = env::var("PR39_TIP").expect("race tip");
        let status = Command::new(&real_git).args(["branch", &branch, &tip])
            .status().expect("concurrent branch creation");
        assert!(status.success());
        eprintln!("injected concurrent branch creation: {branch} -> {tip}");
    }
    let status = Command::new(&real_git).args(arguments).status().expect("real Git");
    exit(status.code().unwrap_or(127));
}
'@
$harnessSource = @'
use std::{env, ffi::OsString, fs, path::{Path, PathBuf}, process::{Command, Output, exit}};
use velocity_git::{WorktreeError, WorktreeSettings, WorktreeSetupRequest,
    execute_worktree_setup, execute_git_command, GitCommand};

fn git(real_git: &str, repository: &Path, arguments: &[&str]) -> Output {
    Command::new(real_git).current_dir(repository).args(arguments).output().expect("Git spawn")
}

fn checked_git(real_git: &str, repository: &Path, arguments: &[&str]) -> String {
    let output = git(real_git, repository, arguments);
    assert!(output.status.success(), "Git {:?}: {}", arguments, String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn initialize(real_git: &str, root: &Path) -> PathBuf {
    let repository = root.join("repository with spaces");
    fs::create_dir_all(&repository).expect("fixture root");
    checked_git(real_git, &repository, &["init", "--initial-branch", "master"]);
    checked_git(real_git, &repository, &["config", "user.name", "Velocity stress fixture"]);
    checked_git(real_git, &repository, &["config", "user.email", "fixture@example.invalid"]);
    checked_git(real_git, &repository, &["commit", "--allow-empty", "-m", "base fixture"]);
    repository
}

fn request(repository: &Path, worktree: PathBuf, branch: Option<String>) -> WorktreeSetupRequest {
    WorktreeSetupRequest {
        repository_root: repository.to_owned(), worktree_path: worktree,
        branch_name: branch, base_ref: "master".into(),
        settings: WorktreeSettings { delete_branch_on_merge: true, share_build_cache: false,
            copy_untracked_files: false, run_setup_command: false },
        untracked_files: String::new(), setup_command: String::new(),
        shared_cargo_target_dir: repository.join("unused-build-cache"),
    }
}

fn path_with_length(root: &Path, length: usize) -> PathBuf {
    let mut path = root.to_owned();
    while length - path.to_string_lossy().chars().count() > 41 {
        path.push("x".repeat(40));
    }
    let remaining = length - path.to_string_lossy().chars().count();
    assert!(remaining > 1);
    path.push("y".repeat(remaining - 1));
    assert_eq!(path.to_string_lossy().chars().count(), length);
    path
}

fn report(label: &str, passed: bool, detail: &str) -> usize {
    println!("{} {label}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn long_paths(real_git: &str, root: &Path, repository: &Path) -> usize {
    let mut failures = 0;
    let mut mapped_git_failures = 0;
    for length in [200, 212, 215, 216, 220, 240, 254, 255, 260, 300] {
        let path = path_with_length(&root.join("deep paths"), length);
        let branch = format!("vel/length-{length}");
        let result = execute_worktree_setup(&request(repository, path.clone(), Some(branch.clone())));
        let reference = format!("refs/heads/{branch}");
        let outcome = match result {
            Ok(created) => {
                let ready = created.agent_may_start;
                execute_git_command(repository, &GitCommand::RemoveWorktree(path.clone())).expect("fixture worktree cleanup");
                execute_git_command(repository, &GitCommand::DeleteBranch(branch.clone())).expect("fixture branch cleanup");
                failures += report(&format!("length-{length}"), ready, "created and cleaned");
                continue;
            }
            Err(WorktreeError::PathTooLong { .. }) => {
                if length <= 254 { mapped_git_failures += 1; }
                true
            }
            Err(error) => { println!("unexpected error: {error:?}"); false }
        };
        let absent = git(real_git, repository, &["show-ref", "--verify", "--quiet", &reference]).status.code() == Some(1);
        let listing = checked_git(real_git, repository, &["worktree", "list", "--porcelain"]);
        let clean = !listing.replace('\\', "/").contains(&path.to_string_lossy().replace('\\', "/"));
        failures += report(&format!("length-{length}"), outcome && absent && clean,
            &format!("typed_path_error={outcome}, no_branch={absent}, no_worktree_entry={clean}"));
    }
    failures + report("real-git-length-limit-exercised", mapped_git_failures > 0,
        &format!("mapped failures below pre-check limit: {mapped_git_failures}"))
}

fn detached_long_path(real_git: &str, root: &Path, repository: &Path) -> usize {
    let path = path_with_length(&root.join("detached paths"), 240);
    let result = execute_worktree_setup(&request(repository, path.clone(), None));
    if result.is_ok() {
        execute_git_command(repository, &GitCommand::RemoveWorktree(path)).expect("detached cleanup");
        return report("detached-long-path", true, "Git accepted path and cleanup passed");
    }
    let typed = matches!(result, Err(WorktreeError::PathTooLong { .. }));
    let listing = checked_git(real_git, repository, &["worktree", "list", "--porcelain"]);
    let clean = !listing.replace('\\', "/").contains(&path.to_string_lossy().replace('\\', "/"));
    report("detached-long-path", typed && clean, &format!("typed_path_error={typed}, no_worktree_entry={clean}"))
}

fn guarded_branch(real_git: &str, root: &Path, repository: &Path, mode: &str) -> usize {
    let branch = format!("vel/guard-{mode}");
    let tip = checked_git(real_git, repository, &["rev-parse", "HEAD"]);
    if mode != "race" { checked_git(real_git, repository, &["branch", &branch, &tip]); }
    let result = execute_worktree_setup(&request(repository, root.join(format!("worktree-{mode}")), Some(branch.clone())));
    let reference = format!("refs/heads/{branch}");
    let output = git(real_git, repository, &["rev-parse", "--verify", &reference]);
    let preserved = output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == tip;
    report(mode, result.is_err() && preserved,
        &format!("creation_failed={}, branch_preserved={preserved}, result={result:?}", result.is_err()))
}

fn child(real_git: &str, root: &Path, shim: &Path, repository: &Path, mode: &str) -> usize {
    let current_path = env::var_os("PATH").unwrap_or_default();
    let paths = env::split_paths(&current_path);
    let updated_path = env::join_paths(std::iter::once(shim.to_owned()).chain(paths)).expect("fixture PATH");
    let branch = format!("vel/guard-{mode}");
    let tip = checked_git(real_git, repository, &["rev-parse", "HEAD"]);
    let status = Command::new(env::current_exe().expect("harness path"))
        .args([OsString::from("--child"), root.as_os_str().into(), repository.as_os_str().into(), mode.into()])
        .env("PATH", updated_path).env("PR39_REAL_GIT", real_git).env("PR39_MODE", mode)
        .env("PR39_BRANCH", branch).env("PR39_TIP", tip).status().expect("fixture child");
    usize::from(!status.success())
}

fn main() {
    let arguments: Vec<String> = env::args().collect();
    if arguments.get(1).map(String::as_str) == Some("--child") {
        let real_git = env::var("PR39_REAL_GIT").expect("real Git");
        let failures = guarded_branch(&real_git, Path::new(&arguments[2]), Path::new(&arguments[3]), &arguments[4]);
        exit(i32::from(failures != 0));
    }
    let root = PathBuf::from(&arguments[1]);
    let real_git = &arguments[2];
    let shim = Path::new(&arguments[3]);
    let repository = initialize(real_git, &root);
    let mut failures = long_paths(real_git, &root, &repository);
    failures += detached_long_path(real_git, &root, &repository);
    for mode in ["existing", "probe-error", "race"] {
        failures += child(real_git, &root, shim, &repository, mode);
    }
    println!("failures: {failures}");
    exit(i32::from(failures != 0));
}
'@
$proxyPath = Join-Path $fixtureRoot 'git-proxy.rs'
$harnessPath = Join-Path $fixtureRoot 'worktree-safety.rs'
[IO.File]::WriteAllText($proxyPath, $proxySource, $encoding)
[IO.File]::WriteAllText($harnessPath, $harnessSource, $encoding)
$env:CARGO_TARGET_DIR = $TargetDirectory
cargo build --manifest-path (Join-Path $repositoryRoot 'Cargo.toml') -p velocity-git --locked
if ($LASTEXITCODE -ne 0) { throw 'Could not build actual velocity-git' }
rustc --edition=2024 $proxyPath -o (Join-Path $shimDirectory 'git.exe')
if ($LASTEXITCODE -ne 0) { throw 'Could not build Git fault injector' }
$libraryPath = Join-Path $TargetDirectory 'debug\libvelocity_git.rlib'
$dependencyPath = Join-Path $TargetDirectory 'debug\deps'
$harnessExecutable = Join-Path $fixtureRoot 'worktree-safety.exe'
rustc --edition=2024 $harnessPath --extern "velocity_git=$libraryPath" -L "dependency=$dependencyPath" -o $harnessExecutable
if ($LASTEXITCODE -ne 0) { throw 'Could not compile worktree stress harness' }
Write-Output "Fixture root: $fixtureRoot"
& $harnessExecutable $fixtureRoot $realGit $shimDirectory
exit $LASTEXITCODE
