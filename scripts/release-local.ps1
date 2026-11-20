<#
.SYNOPSIS
Builds a Velocity installer on this machine (#498).

.DESCRIPTION
One command for a local release build, without the waits that made it slow:
- Rosen's binaries are built once per pinned commit and kept under the target
  folder, instead of a cold `cargo install` into a throwaway folder every time.
- The UI is built first; Vite exits by itself once it prints its result
  (ui/vite.config.ts explains the hang this used to work around, #44 and #500).
- The release profile is incremental locally (Cargo.toml), so a change to one
  crate rebuilds only what changed.
The installer, portable executable, checksums and an unsigned release notice
(`RELEASE-NOTES.md`) are copied to -OutDir.
The downloads are not signed with the updater key, which only CI holds, and
the updater manifest (`updater/latest.json`) is never moved here (#521).
With -Publish, the downloads are uploaded to GitHub as a draft prerelease
(`--draft --prerelease --latest=false`) labelled as an unsigned local build
so it never looks like the current release to installed copies.
The Linux code graph indexer Collapse pins is not built here.

.EXAMPLE
pwsh -NoProfile -File scripts/release-local.ps1 -Version 0.1.0-beta.13

.EXAMPLE
pwsh -NoProfile -File scripts/release-local.ps1 -Version 0.1.0-beta.13 -Publish
#>
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$')]
    [string]$Version,
    # Where the installer, portable executable and checksums are copied.
    [string]$OutDir = '',
    # Upload the local build to GitHub as a draft unsigned release (--latest=false).
    [switch]$Publish,
    # GitHub repository (owner/name) for -Publish. Empty uses the checkout's default.
    [string]$Repository = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
if (-not $OutDir) { $OutDir = Join-Path $target "release-local\$Version" }
$started = Get-Date
# A release build at opt-level 3 uses up to about 1.5 GB of commit charge per job. Running
# every core at once next to working agents exhausted the commit limit on 2026-11-19
# (allocation failures and compiler crashes, the same pressure behind earlier bluescreens).
# Size the job count to the free commit charge, keeping 4 GB back, at most half the cores.
if (-not $env:CARGO_BUILD_JOBS) {
    $os = Get-CimInstance Win32_OperatingSystem
    $freeCommitGb = $os.FreeVirtualMemory / 1MB
    $byMemory = [math]::Floor(($freeCommitGb - 4) / 1.5)
    $byCores = [math]::Max(1, [math]::Floor([Environment]::ProcessorCount / 2))
    $env:CARGO_BUILD_JOBS = [string][math]::Max(1, [math]::Min($byMemory, $byCores))
    Write-Host "Building with $env:CARGO_BUILD_JOBS jobs ($([math]::Round($freeCommitGb, 1)) GB commit free)"
}

function Write-Step([string]$Text) {
    $elapsed = [int]((Get-Date) - $started).TotalSeconds
    Write-Host "==> [$elapsed s] $Text" -ForegroundColor Cyan
}

function Get-RosenBinaries {
    $pin = 'rosen-client = \{ git = "([^"]+)", rev = "([0-9a-f]{40})" \}'
    if ((Get-Content -Raw Cargo.toml) -notmatch $pin) { throw 'No pinned rosen-client in Cargo.toml' }
    $url, $rev = $Matches[1], $Matches[2]
    $install = Join-Path $target "rosen-bin\$rev"
    $bin = Join-Path $install 'bin'
    if (Test-Path (Join-Path $bin 'rosen-server.exe')) {
        Write-Step "Rosen $($rev.Substring(0, 7)) already built"
        return $bin
    }
    Write-Step "Building Rosen $($rev.Substring(0, 7)) (once per pinned commit)"
    # A kept target folder makes the next pin's build reuse unchanged dependencies.
    $previous = $env:CARGO_TARGET_DIR
    $env:CARGO_TARGET_DIR = Join-Path $target 'rosen-build'
    try {
        cargo install --locked --git $url --rev $rev --root $install rosen-server rosen-cli
        if ($LASTEXITCODE -ne 0) { throw 'Could not build Rosen' }
    } finally {
        $env:CARGO_TARGET_DIR = $previous
    }
    $bin
}

function Build-Ui {
    Write-Step 'Building the UI'
    pnpm -C ui build
    if ($LASTEXITCODE -ne 0) { throw "UI build failed (exit $LASTEXITCODE)" }
}

$rosen = Get-RosenBinaries
Build-Ui
Write-Step 'Building the app'
$env:VELOCITY_ROSEN_SERVER_EXE = Join-Path $rosen 'rosen-server.exe'
$env:VELOCITY_ROSEN_CLI_EXE = Join-Path $rosen 'rosen.exe'
$config = Join-Path $target "release-local-$Version.json"
# The UI is already built; Tauri's own `pnpm build` would run from the wrong folder.
@{ version = $Version; build = @{ beforeBuildCommand = '' } } | ConvertTo-Json -Depth 3 | Set-Content -Encoding utf8 $config
cargo tauri build --ci --bundles nsis --features bundled-rosen --config $config
$built = $LASTEXITCODE
# Tauri rewrites the manifest's line endings; drop that if nothing else changed.
git diff --ignore-cr-at-eol --quiet -- src-tauri/Cargo.toml
if ($LASTEXITCODE -eq 0) { git checkout -- src-tauri/Cargo.toml }
if ($built -ne 0) { throw "tauri build failed (exit $built)" }

Write-Step "Copying the downloads to $OutDir"
New-Item -ItemType Directory -Force $OutDir | Out-Null
$release = Join-Path $target 'release'
$setup = Get-ChildItem (Join-Path $release "bundle\nsis\*_${Version}_x64-setup.exe") | Select-Object -First 1
if (-not $setup) { throw "No installer for $Version in $release\bundle\nsis" }
Copy-Item $setup.FullName (Join-Path $OutDir "Velocity_${Version}_x64-setup.exe") -Force
Copy-Item (Join-Path $release 'velocity-app.exe') (Join-Path $OutDir "Velocity_${Version}_x64-portable.exe") -Force
Get-ChildItem $OutDir -File -Filter 'Velocity_*' | ForEach-Object {
    "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)"
} | Set-Content -Encoding utf8 (Join-Path $OutDir 'SHA256SUMS.txt')

$notesPath = Join-Path $OutDir 'RELEASE-NOTES.md'
$tag = "v$Version"
$title = "Velocity $Version (unsigned local build)"
@"
<!-- velocity-unsigned-release -->
> [!WARNING]
> **Unsigned local build — not offered to installed copies.**
> Built locally with ``scripts/release-local.ps1`` outside the signed ``release.yml`` pipeline.
> These downloads are not signed with the updater key, and the updater manifest (``updater/latest.json``) was **not** updated.
> Installed copies of Velocity will not be offered this version automatically.
<!-- /velocity-unsigned-release -->
"@ | Set-Content -Encoding utf8 $notesPath

$assets = @(
    (Join-Path $OutDir "Velocity_${Version}_x64-setup.exe"),
    (Join-Path $OutDir "Velocity_${Version}_x64-portable.exe"),
    (Join-Path $OutDir 'SHA256SUMS.txt')
)

Write-Step "Done: $OutDir"
Write-Host "NOTE: Downloads are unsigned. The updater manifest (updater/latest.json) was NOT moved; installed copies will not be offered $Version." -ForegroundColor Yellow

if ($Publish) {
    $repoArgs = if ($Repository) { @('--repo', $Repository) } else { @() }
    Write-Step "Uploading $tag as a draft unsigned release"
    gh release view $tag @repoArgs *> $null
    if ($LASTEXITCODE -eq 0) {
        gh release edit $tag @repoArgs --draft --prerelease --latest=false --title $title --notes-file $notesPath
        if ($LASTEXITCODE -ne 0) { throw "Could not update draft release $tag" }
        gh release upload $tag @repoArgs @assets --clobber
        if ($LASTEXITCODE -ne 0) { throw "Could not upload local downloads to $tag" }
    } else {
        gh release create $tag @repoArgs --draft --prerelease --latest=false --title $title --notes-file $notesPath @assets
        if ($LASTEXITCODE -ne 0) { throw "Could not create draft release $tag" }
    }
    Write-Host "Draft release $tag updated ($title). The updater manifest (updater/latest.json) was NOT moved." -ForegroundColor Yellow
} else {
    Write-Host "To upload as a draft without looking like the current release:"
    Write-Host "  pwsh -NoProfile -File scripts/release-local.ps1 -Version $Version -Publish"
    Write-Host "To publish a signed release that updates updater/latest.json for installed copies, use Cut release in Velocity or .github/workflows/release.yml."
}
