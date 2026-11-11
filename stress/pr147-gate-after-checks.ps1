param([string]$Root = (Split-Path -Parent $PSScriptRoot))
$ErrorActionPreference = 'Stop'
# PR #147 review: the proof gate must accept a review worktree right after the local checks
# AGENTS.md requires (they regenerate the IPC bindings), for an ordinary PR with no Lean.
$root = (Resolve-Path $Root).Path
Push-Location $root
try {
    $head = (git rev-parse HEAD).Trim()
    $base = (git merge-base HEAD origin/master).Trim()
    "status before: [" + ((git status --porcelain --untracked-files=all) -join '; ') + "]"
    & pnpm -C ui bindings | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'pnpm -C ui bindings failed' }
    "status after bindings: [" + ((git status --porcelain --untracked-files=all) -join '; ') + "]"
    "content diff ignoring CR at EOL: [" + ((git diff --ignore-cr-at-eol --stat) -join '; ') + "]"
    $env:CARGO_TARGET_DIR = Join-Path $root 'target'
    & (Join-Path $root 'scripts/build-queue.ps1') cargo run --quiet -p velocity-github --example proof_gate -- $root $base $head
    $gate = $LASTEXITCODE
    git checkout -- ui/src/lib/bindings.ts
    if ($gate -ne 0) { "F17: proof gate refused a clean merge result after the required checks"; exit 1 }
    "proof gate passed"
} finally { Pop-Location }
