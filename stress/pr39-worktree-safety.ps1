# Risk: worktree creation failures must report long paths and preserve branches owned by others.
# The shim injects one deterministic concurrent branch creation, then delegates to real Git.
# All repositories, branches and worktrees used here are fresh fixtures under this checkout.
# Updated for #230: requests are built through velocity-git's constructors, and the long-path
# helper no longer depends on the checkout path's length.
# Adapted for #499: the harness is stress/harness/sources/pr39-worktree-safety.rs, built with every
# other Rust harness into the stress crate's stress-harness binary in the root workspace, instead
# of rustc linking it against a separate velocity-git build; -TargetDirectory placed that build
# and is gone. The Git fault injector below is still compiled alone with rustc: it uses only std
# and must exist as git.exe on the fixture PATH. Sources and assertions are unchanged.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
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
$proxyPath = Join-Path $fixtureRoot 'git-proxy.rs'
[IO.File]::WriteAllText($proxyPath, $proxySource, $encoding)
rustc --edition=2024 $proxyPath -o (Join-Path $shimDirectory 'git.exe')
if ($LASTEXITCODE -ne 0) { throw 'Could not build Git fault injector' }
Write-Output "Fixture root: $fixtureRoot"
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Invoke-StressHarness pr39-worktree-safety -Arguments @($fixtureRoot, $realGit, $shimDirectory)
exit $LASTEXITCODE
