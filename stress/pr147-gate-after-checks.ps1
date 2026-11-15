param([string]$Root = (Split-Path -Parent $PSScriptRoot))
$ErrorActionPreference = 'Stop'
# PR #147 review: the proof gate must accept a review worktree right after the local checks
# AGENTS.md requires (they regenerate the IPC bindings), for an ordinary PR with no Lean.
# Updated for Rosen merge trees (#385): Git's gate runs in an isolated CRLF fixture
# seeded from these reviewed sources; the real bindings generator still runs unchanged.
$root = (Resolve-Path $Root).Path
Import-Module (Join-Path $PSScriptRoot 'git-fixture.psm1') -Force
$paths = @('.gitattributes', 'ui/src/lib/bindings.ts') +
    @(Get-ChildItem -LiteralPath (Join-Path $root 'proofs') -Force |
        Where-Object { $_.Name -notin @('.lake', '.lake-wsl', 'generated-llbc') } |
        ForEach-Object {
            if ($_.PSIsContainer) { Get-ChildItem -LiteralPath $_.FullName -Recurse -File }
            else { $_ }
        } |
        ForEach-Object { $_.FullName.Substring($root.Length + 1) })
$fixture = New-StressGitFixture $root $paths
$bindings = Join-Path $root 'ui/src/lib/bindings.ts'
$originalBindings = [IO.File]::ReadAllBytes($bindings)
Push-Location $root
try {
    $base = (Invoke-FixtureGit $fixture @('rev-parse', 'HEAD')).Trim()
    Set-Content -LiteralPath (Join-Path $fixture 'ordinary.txt') -Value 'An ordinary PR with no Lean change'
    Invoke-FixtureGit $fixture @('add', 'ordinary.txt')
    Invoke-FixtureGit $fixture @('-c', 'user.name=stress', '-c', 'user.email=stress@example.invalid', 'commit', '--quiet', '-m', 'Ordinary change without Lean')
    $head = (Invoke-FixtureGit $fixture @('rev-parse', 'HEAD')).Trim()
    "status before: [" + ((Invoke-FixtureGit $fixture @('status', '--porcelain', '--untracked-files=all')) -join '; ') + "]"
    & pnpm -C ui bindings | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'pnpm -C ui bindings failed' }
    Copy-Item -LiteralPath $bindings -Destination (Join-Path $fixture 'ui/src/lib/bindings.ts') -Force
    "status after bindings: [" + ((Invoke-FixtureGit $fixture @('status', '--porcelain', '--untracked-files=all')) -join '; ') + "]"
    "content diff ignoring CR at EOL: [" + ((Invoke-FixtureGit $fixture @('diff', '--ignore-cr-at-eol', '--stat')) -join '; ') + "]"
    $env:CARGO_TARGET_DIR = Join-Path $root 'target'
    & (Join-Path $root 'scripts/build-queue.ps1') cargo run --quiet --locked -p velocity-github --example proof_gate -- $fixture $base $head
    $gate = $LASTEXITCODE
    if ($gate -ne 0) { "F17: proof gate refused a clean merge result after the required checks"; exit 1 }
    "proof gate passed"
} finally {
    [IO.File]::WriteAllBytes($bindings, $originalBindings)
    Pop-Location
    Remove-StressGitFixture $fixture
}
