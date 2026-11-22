<#
.SYNOPSIS
Builds a Velocity installer on this machine (#498, #593).

.DESCRIPTION
One command for a local release build, without the waits that made it slow:
- Rosen's binaries are built once per pinned commit in a per-revision build folder
  (#561), instead of reusing stale workspace artifacts across pins.
- The UI is built first; Vite exits by itself once it prints its result
  (ui/vite.config.ts explains the hang this used to work around, #44 and #500).
- The release profile is incremental locally (Cargo.toml), so a change to one
  crate rebuilds only what changed.

Without -Sign, downloads are unsigned and the updater manifest is never moved (#521).
With -Sign, downloads are signed with the Tauri updater key from the environment
(TAURI_SIGNING_PRIVATE_KEY) or Windows Credential Manager (target
`velocity:updater-signing-key`).
With -GraphIndexer, the standalone Linux x86-64 `velocity-graph` archive and
`.sha256` checksum are also built via WSL (or Docker) and copied to -OutDir.
With -PublishCollapse, the signed release, assets, `latest.json` and the rolling
`updater/latest.json` manifest are published directly to Collapse (#593).
With -Publish, an unsigned build is uploaded to GitHub as a draft prerelease
(`--draft --prerelease --latest=false`).

.EXAMPLE
pwsh -NoProfile -File scripts/release-local.ps1 -Version 0.1.0-beta.21

.EXAMPLE
pwsh -NoProfile -File scripts/release-local.ps1 -Version 0.1.0-beta.21 -Sign -GraphIndexer -PublishCollapse
#>
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$')]
    [string]$Version,
    # Where the installer, portable executable, signatures and checksums are copied.
    [string]$OutDir = '',
    # Commit recorded in the build metadata (40 hex). Defaults to git HEAD when available.
    [string]$Commit = '',
    # Sign downloads with the Tauri updater key held in Windows Credential Manager (or env).
    [switch]$Sign,
    # Also build the standalone Linux x86-64 velocity-graph archive and .sha256 via WSL/Docker.
    [switch]$GraphIndexer,
    # Publish a signed release (and rewrite rolling updater/latest.json) to Collapse (#593).
    [switch]$PublishCollapse,
    # Collapse repository (owner/name) for -PublishCollapse. Defaults to mESmaC/velocity.
    [string]$CollapseRepo = 'mESmaC/velocity',
    # Upload an unsigned local build to GitHub as a draft release (--latest=false).
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
$tauriCli = '@tauri-apps/cli@2.12.4'
$collapseApi = 'https://api.collapse.sh'

if ($PublishCollapse -and -not $Sign) {
    throw '-PublishCollapse requires -Sign so every published download is signed with the updater key.'
}

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

function Read-WindowsCredential([string]$TargetName) {
    if (-not ('VelocityWinCred' -as [type])) {
        Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class VelocityWinCred {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct CREDENTIAL {
        public uint Flags;
        public uint Type;
        public string TargetName;
        public string Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize;
        public IntPtr CredentialBlob;
        public uint Persist;
        public uint AttributeCount;
        public IntPtr Attributes;
        public string TargetAlias;
        public string UserName;
    }

    [DllImport("Advapi32.dll", EntryPoint = "CredReadW", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool CredRead(string target, uint type, uint reservedFlag, out IntPtr credentialPtr);

    [DllImport("Advapi32.dll", SetLastError = true)]
    private static extern void CredFree(IntPtr buffer);

    public static string ReadUtf8(string target) {
        IntPtr ptr;
        if (!CredRead(target, 1, 0, out ptr)) {
            return null;
        }
        try {
            var cred = (CREDENTIAL)Marshal.PtrToStructure(ptr, typeof(CREDENTIAL));
            if (cred.CredentialBlob == IntPtr.Zero || cred.CredentialBlobSize == 0) {
                return null;
            }
            var bytes = new byte[cred.CredentialBlobSize];
            Marshal.Copy(cred.CredentialBlob, bytes, 0, bytes.Length);
            var text = Encoding.UTF8.GetString(bytes);
            if (text.IndexOf('\0') >= 0) {
                text = Encoding.Unicode.GetString(bytes);
            }
            return text.TrimEnd('\0').Trim();
        } finally {
            CredFree(ptr);
        }
    }
}
"@
    }
    return [VelocityWinCred]::ReadUtf8($TargetName)
}

function Resolve-UpdaterSigningKey {
    if ($env:TAURI_SIGNING_PRIVATE_KEY) {
        return @{
            private_key = $env:TAURI_SIGNING_PRIVATE_KEY.Trim()
            password = if ($env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) { $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD } else { '' }
        }
    }
    $stored = Read-WindowsCredential 'velocity:updater-signing-key'
    if (-not $stored) {
        throw "No updater signing key found in TAURI_SIGNING_PRIVATE_KEY or Windows Credential Manager (target 'velocity:updater-signing-key')."
    }
    try {
        $parsed = $stored | ConvertFrom-Json -ErrorAction Stop
        if ($parsed.private_key) {
            return @{
                private_key = ([string]$parsed.private_key).Trim()
                password = if ($parsed.password) { [string]$parsed.password } else { '' }
            }
        }
    } catch {
        # Plain key text stored directly in Windows Credential Manager.
    }
    $password = Read-WindowsCredential 'velocity:updater-signing-key-password'
    return @{
        private_key = $stored.Trim()
        password = if ($password) { $password } else { '' }
    }
}

function Resolve-CollapseReleaseToken {
    if ($env:COLLAPSE_RELEASE_TOKEN) { return $env:COLLAPSE_RELEASE_TOKEN.Trim() }
    if ($env:COLLAPSE_TOKEN) { return $env:COLLAPSE_TOKEN.Trim() }
    $stored = Read-WindowsCredential 'velocity:collapse.sh:releases'
    if ($stored) {
        try {
            $parsed = $stored | ConvertFrom-Json -ErrorAction Stop
            if ($parsed.token) { return ([string]$parsed.token).Trim() }
        } catch {
            # Legacy plain token text stored directly in Windows Credential Manager.
        }
        return $stored.Trim()
    }
    throw "No Collapse release token found in COLLAPSE_RELEASE_TOKEN or Windows Credential Manager ('velocity:collapse.sh:releases')."
}

function Get-RosenBinaries {
    $pin = 'rosen-client = \{ git = "([^"]+)", rev = "([0-9a-f]{40})" \}'
    if ((Get-Content -Raw Cargo.toml) -notmatch $pin) { throw 'No pinned rosen-client in Cargo.toml' }
    $url, $rev = $Matches[1], $Matches[2]
    $install = Join-Path $target "rosen-bin\$rev"
    $bin = Join-Path $install 'bin'
    if ((Test-Path -LiteralPath (Join-Path $bin 'rosen-server.exe')) -and (Test-Path -LiteralPath (Join-Path $bin 'rosen.exe'))) {
        Write-Step "Rosen $($rev.Substring(0, 7)) already built"
        return $bin
    }
    Write-Step "Building Rosen $($rev.Substring(0, 7)) (once per pinned commit)"
    # Give each pinned commit its own target folder so Cargo never reuses stale
    # workspace-member artifacts (like rosen-protocol) across pins (#561).
    # Registry and git downloads remain shared via CARGO_HOME.
    $buildRoot = Join-Path $target 'rosen-build'
    $buildDir = Join-Path $buildRoot $rev
    if (Test-Path -LiteralPath $buildRoot -PathType Container) {
        Get-ChildItem -LiteralPath $buildRoot -Force |
            Where-Object { $_.Name -ne $rev } |
            ForEach-Object { Remove-Item -LiteralPath $_.FullName -Recurse -Force -ErrorAction SilentlyContinue }
    }
    $previous = $env:CARGO_TARGET_DIR
    $env:CARGO_TARGET_DIR = $buildDir
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

function Convert-ToWslPath([string]$WindowsPath) {
    $full = [System.IO.Path]::GetFullPath($WindowsPath)
    if ($full -match '^([A-Za-z]):\\(.*)$') {
        $drive = $Matches[1].ToLowerInvariant()
        $rest = $Matches[2] -replace '\\', '/'
        return "/mnt/$drive/$rest"
    }
    throw "Cannot map Windows path '$WindowsPath' into WSL"
}

function Build-GraphIndexer([string]$TargetVersion, [string]$Destination) {
    $archiveName = "velocity-graph-$TargetVersion-x86_64-unknown-linux-gnu.tar.gz"
    $archivePath = Join-Path $Destination $archiveName
    $shaPath = "$archivePath.sha256"
    $scriptPath = Join-Path $root '.github\scripts\graph-indexer.sh'
    if (-not (Test-Path -LiteralPath $scriptPath)) {
        throw "Missing packaging script: $scriptPath"
    }
    $wsl = Get-Command wsl -ErrorAction SilentlyContinue
    if ($wsl) {
        $distros = @((& wsl -l -q 2>$null) | ForEach-Object { ($_ -replace "`0", '').Trim() } | Where-Object { $_ -and $_ -notlike 'docker-desktop*' })
        foreach ($distro in $distros) {
            $hasCargo = & wsl -d $distro -- bash -lc 'command -v cargo >/dev/null 2>&1 || [ -x "$HOME/.cargo/bin/cargo" ]'
            if ($LASTEXITCODE -eq 0) {
                Write-Step "Building Linux velocity-graph archive via WSL ($distro)"
                $wslRoot = Convert-ToWslPath $root
                $wslOut = Convert-ToWslPath $Destination
                $wslTarget = '/tmp/velocity-graph-linux-target'
                $cmd = "export PATH=`"`$HOME/.cargo/bin:`$PATH`"; cd '$wslRoot' && CARGO_TARGET_DIR='$wslTarget' bash .github/scripts/graph-indexer.sh '$TargetVersion' '$wslOut'"
                & wsl -d $distro -- bash -lc $cmd
                if ($LASTEXITCODE -ne 0) { throw "WSL velocity-graph build failed (exit $LASTEXITCODE)" }
                if (-not (Test-Path -LiteralPath $archivePath) -or -not (Test-Path -LiteralPath $shaPath)) {
                    throw "WSL build did not produce $archiveName and its .sha256"
                }
                return
            }
        }
    }
    $docker = Get-Command docker -ErrorAction SilentlyContinue
    if ($docker) {
        Write-Step 'Building Linux velocity-graph archive via Docker'
        $mountOut = Join-Path $target "graph-indexer-out-$TargetVersion"
        New-Item -ItemType Directory -Force $mountOut | Out-Null
        $toolchain = (Get-Content -Raw (Join-Path $root 'rust-toolchain.toml'))
        $rustImage = if ($toolchain -match 'channel\s*=\s*"([^"]+)"') { "rust:$($Matches[1])-bookworm" } else { 'rust:1-bookworm' }
        & docker run --rm `
            -v "${root}:/src:ro" `
            -v "${mountOut}:/out" `
            -w /src `
            -e CARGO_TARGET_DIR=/tmp/target `
            $rustImage `
            bash .github/scripts/graph-indexer.sh $TargetVersion /out
        if ($LASTEXITCODE -ne 0) { throw "Docker velocity-graph build failed (exit $LASTEXITCODE)" }
        Copy-Item (Join-Path $mountOut $archiveName) $archivePath -Force
        Copy-Item (Join-Path $mountOut "$archiveName.sha256") $shaPath -Force
        return
    }
    throw 'Building the Linux velocity-graph archive on Windows requires WSL (with Rust installed) or Docker.'
}

function Invoke-SignFile([string]$FilePath, [string]$TargetVersion, [hashtable]$SigningKey) {
    $prevKey = $env:TAURI_SIGNING_PRIVATE_KEY
    $prevPass = $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD
    $env:TAURI_SIGNING_PRIVATE_KEY = $SigningKey.private_key
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $SigningKey.password
    try {
        if (Get-Command npx -ErrorAction SilentlyContinue) {
            npx -y $tauriCli signer sign --app-version $TargetVersion $FilePath
        } else {
            cargo tauri signer sign --app-version $TargetVersion $FilePath
        }
        if ($LASTEXITCODE -ne 0) { throw "Could not sign $(Split-Path -Leaf $FilePath)" }
    } finally {
        $env:TAURI_SIGNING_PRIVATE_KEY = $prevKey
        $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $prevPass
    }
}

function Publish-ToCollapse(
    [string]$CollapseRepository,
    [string]$Tag,
    [string]$TargetVersion,
    [string]$ReleaseCommit,
    [string]$Notes,
    [bool]$IsPrerelease,
    [string]$Destination
) {
    $parts = $CollapseRepository.Split('/', 2)
    if ($parts.Count -ne 2 -or -not $parts[0] -or -not $parts[1]) {
        throw "CollapseRepo '$CollapseRepository' must be owner/name"
    }
    $owner, $repo = $parts[0], $parts[1]
    $token = Resolve-CollapseReleaseToken
    $headers = @{ Authorization = "Bearer $token"; Accept = 'application/json' }
    $releasesUrl = "$collapseApi/api/repositories/$([uri]::EscapeDataString($owner))/$([uri]::EscapeDataString($repo))/releases"

    if ($ReleaseCommit -and (Test-Path (Join-Path $root '.git'))) {
        Write-Step "Ensuring git tag $Tag exists on Collapse ($CollapseRepository)"
        $mirrorUrl = "$collapseApi/$owner/$repo.git"
        git -c 'credential.https://api.collapse.sh.useHttpPath=true' -c 'http.https://api.collapse.sh/.proactiveAuth=basic' push $mirrorUrl "${ReleaseCommit}:refs/tags/${Tag}" 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Could not push tag $Tag to $mirrorUrl" }
    }

    Write-Step "Creating or updating Collapse release $Tag on $CollapseRepository"
    $payload = @{
        tag = $Tag
        name = "Velocity $TargetVersion"
        notes = $Notes
        prerelease = $IsPrerelease
        draft = $false
        rolling = $false
    } | ConvertTo-Json
    $existing = $null
    $resp = Invoke-WebRequest -UseBasicParsing -Uri "$releasesUrl/tags/$([uri]::EscapeDataString($Tag))" -Headers $headers -SkipHttpErrorCheck
    if ($resp.StatusCode -eq 200) { $existing = $resp.Content | ConvertFrom-Json }
    if ($existing) {
        $release = Invoke-RestMethod -Method Patch -Uri "$releasesUrl/$($existing.id)" -Headers $headers -ContentType 'application/json' -Body $payload
    } else {
        $release = Invoke-RestMethod -Method Post -Uri $releasesUrl -Headers $headers -ContentType 'application/json' -Body $payload
    }

    $uploadFiles = Get-ChildItem -LiteralPath $Destination -File | Where-Object { $_.Name -ne 'RELEASE-NOTES.md' }
    foreach ($file in $uploadFiles) {
        $existingAsset = $release.assets | Where-Object { $_.name -eq $file.Name } | Select-Object -First 1
        if ($existingAsset) {
            Invoke-RestMethod -Method Delete -Uri "$releasesUrl/$($release.id)/assets/$($existingAsset.id)" -Headers $headers | Out-Null
        }
        $mime = switch -Wildcard ($file.Name) {
            '*.json' { 'application/json' }
            '*.txt' { 'text/plain; charset=utf-8' }
            '*.sig' { 'text/plain; charset=utf-8' }
            '*.sha256' { 'text/plain; charset=utf-8' }
            '*.tar.gz' { 'application/gzip' }
            default { 'application/octet-stream' }
        }
        Write-Step "Uploading $($file.Name) to Collapse ($Tag)"
        Invoke-RestMethod -Method Put -Uri "$releasesUrl/$($release.id)/assets/$([uri]::EscapeDataString($file.Name))" -Headers $headers -ContentType $mime -InFile $file.FullName | Out-Null
    }

    $baseDownload = "$collapseApi/$([uri]::EscapeDataString($owner))/$([uri]::EscapeDataString($repo))/releases/download/$([uri]::EscapeDataString($Tag))"
    $setupName = "Velocity_${TargetVersion}_x64-setup.exe"
    $portableName = "Velocity_${TargetVersion}_x64-portable.exe"
    $setupEntry = @{ url = "$baseDownload/$setupName"; signature = (Get-Content -Raw (Join-Path $Destination "$setupName.sig")).Trim() }
    $platforms = [ordered]@{
        'windows-x86_64' = $setupEntry
        'windows-x86_64-nsis' = $setupEntry
        'windows-x86_64-portable' = @{ url = "$baseDownload/$portableName"; signature = (Get-Content -Raw (Join-Path $Destination "$portableName.sig")).Trim() }
    }
    $msiName = "Velocity_${TargetVersion}_x64_en-US.msi"
    if (Test-Path -LiteralPath (Join-Path $Destination $msiName)) {
        $platforms['windows-x86_64-msi'] = @{ url = "$baseDownload/$msiName"; signature = (Get-Content -Raw (Join-Path $Destination "$msiName.sig")).Trim() }
    }
    $manifestPath = Join-Path $Destination 'latest.json'
    [ordered]@{
        version = $TargetVersion
        notes = $Notes.Trim()
        pub_date = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
        platforms = $platforms
    } | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8NoBOM $manifestPath

    $existingManifest = $release.assets | Where-Object { $_.name -eq 'latest.json' } | Select-Object -First 1
    if ($existingManifest) {
        Invoke-RestMethod -Method Delete -Uri "$releasesUrl/$($release.id)/assets/$($existingManifest.id)" -Headers $headers | Out-Null
    }
    Invoke-RestMethod -Method Put -Uri "$releasesUrl/$($release.id)/assets/latest.json" -Headers $headers -ContentType 'application/json' -InFile $manifestPath | Out-Null

    Write-Step "Updating rolling updater/latest.json on Collapse ($CollapseRepository)"
    if ($ReleaseCommit -and (Test-Path (Join-Path $root '.git'))) {
        $mirrorUrl = "$collapseApi/$owner/$repo.git"
        git -c 'credential.https://api.collapse.sh.useHttpPath=true' -c 'http.https://api.collapse.sh/.proactiveAuth=basic' push $mirrorUrl "+${ReleaseCommit}:refs/tags/updater" 2>&1 | Out-Null
    }
    $updaterPayload = @{
        tag = 'updater'
        name = 'Update manifest'
        notes = 'Holds latest.json, which installed copies of Velocity read to find updates. Not a download.'
        prerelease = $true
        draft = $false
        rolling = $true
    } | ConvertTo-Json
    $updaterResp = Invoke-WebRequest -UseBasicParsing -Uri "$releasesUrl/tags/updater" -Headers $headers -SkipHttpErrorCheck
    if ($updaterResp.StatusCode -eq 200) {
        $updater = $updaterResp.Content | ConvertFrom-Json
        $updater = Invoke-RestMethod -Method Patch -Uri "$releasesUrl/$($updater.id)" -Headers $headers -ContentType 'application/json' -Body $updaterPayload
    } else {
        $updater = Invoke-RestMethod -Method Post -Uri $releasesUrl -Headers $headers -ContentType 'application/json' -Body $updaterPayload
    }
    Invoke-RestMethod -Method Put -Uri "$releasesUrl/$($updater.id)/assets/latest.json" -Headers $headers -ContentType 'application/json' -InFile $manifestPath | Out-Null
    Write-Step "Published $Tag and updated updater/latest.json on Collapse ($CollapseRepository)"
}

$signingKey = $null
if ($Sign) {
    $signingKey = Resolve-UpdaterSigningKey
}
$isPrerelease = $Version.Contains('-')

$rosen = Get-RosenBinaries
Build-Ui
Write-Step 'Building the app'
$buildCommit = $Commit.Trim().ToLowerInvariant()
if (-not $buildCommit -and $env:VELOCITY_BUILD_COMMIT) {
    $buildCommit = $env:VELOCITY_BUILD_COMMIT.Trim().ToLowerInvariant()
}
if (-not $buildCommit -and (Test-Path (Join-Path $root '.git'))) {
    $head = (git rev-parse HEAD 2>$null | Out-String).Trim().ToLowerInvariant()
    if ($LASTEXITCODE -eq 0 -and $head -cmatch '^[0-9a-f]{40}$') {
        $buildCommit = $head
    }
    $global:LASTEXITCODE = 0
}
if ($buildCommit) {
    if ($buildCommit -cnotmatch '^[0-9a-f]{40}$') {
        throw "Commit $buildCommit is not a 40-character hex commit ID"
    }
    $env:VELOCITY_BUILD_COMMIT = $buildCommit
}
$env:VELOCITY_ROSEN_SERVER_EXE = Join-Path $rosen 'rosen-server.exe'
$env:VELOCITY_ROSEN_CLI_EXE = Join-Path $rosen 'rosen.exe'
$config = Join-Path $target "release-local-$Version.json"
# The UI is already built; Tauri's own `pnpm build` would run from the wrong folder.
@{ version = $Version; build = @{ beforeBuildCommand = '' } } | ConvertTo-Json -Depth 3 | Set-Content -Encoding utf8 $config
$bundles = if ($Sign -and -not $isPrerelease) { 'nsis,msi' } else { 'nsis' }
cargo tauri build --ci --bundles $bundles --features bundled-rosen --config $config
$built = $LASTEXITCODE
# Tauri rewrites the manifest's line endings; drop that if nothing else changed.
if (Test-Path (Join-Path $root '.git')) {
    git diff --ignore-cr-at-eol --quiet -- src-tauri/Cargo.toml
    if ($LASTEXITCODE -eq 0) { git checkout -- src-tauri/Cargo.toml }
}
if ($built -ne 0) { throw "tauri build failed (exit $built)" }

Write-Step "Copying the downloads to $OutDir"
New-Item -ItemType Directory -Force $OutDir | Out-Null
$release = Join-Path $target 'release'
$setup = Get-ChildItem (Join-Path $release "bundle\nsis\*_${Version}_x64-setup.exe") | Select-Object -First 1
if (-not $setup) { throw "No installer for $Version in $release\bundle\nsis" }
$setupOut = Join-Path $OutDir "Velocity_${Version}_x64-setup.exe"
$portableOut = Join-Path $OutDir "Velocity_${Version}_x64-portable.exe"
Copy-Item $setup.FullName $setupOut -Force
Copy-Item (Join-Path $release 'velocity-app.exe') $portableOut -Force
$msiOut = $null
if ($Sign -and -not $isPrerelease) {
    $msi = Get-ChildItem (Join-Path $release "bundle\msi\*_${Version}_x64_en-US.msi") -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($msi) {
        $msiOut = Join-Path $OutDir "Velocity_${Version}_x64_en-US.msi"
        Copy-Item $msi.FullName $msiOut -Force
    }
}

$signTargets = @($setupOut, $portableOut)
if ($msiOut) { $signTargets += $msiOut }
if ($Sign) {
    Write-Step 'Signing the downloads with the updater key'
    foreach ($file in $signTargets) {
        Invoke-SignFile $file $Version $signingKey
    }
}

if ($GraphIndexer) {
    Build-GraphIndexer $Version $OutDir
}

Get-ChildItem $OutDir -File -Filter 'Velocity_*' -Exclude '*.sig' | ForEach-Object {
    "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)"
} | Set-Content -Encoding utf8 (Join-Path $OutDir 'SHA256SUMS.txt')

$notesPath = Join-Path $OutDir 'RELEASE-NOTES.md'
$tag = "v$Version"
if ($Sign) {
    $commitNote = if ($buildCommit) { " (commit ``$($buildCommit.Substring(0, 10))``)" } else { '' }
    $notes = "Velocity $Version$commitNote — signed release published to Collapse."
    $notes | Set-Content -Encoding utf8 $notesPath
} else {
    $title = "Velocity $Version (unsigned local build)"
    @"
<!-- velocity-unsigned-release -->
> [!WARNING]
> **Unsigned local build — not offered to installed copies.**
> Built locally with ``scripts/release-local.ps1`` outside a signed release cut.
> These downloads are not signed with the updater key, and the updater manifest (``updater/latest.json``) was **not** updated.
> Installed copies of Velocity will not be offered this version automatically.
<!-- /velocity-unsigned-release -->
"@ | Set-Content -Encoding utf8 $notesPath
}

Write-Step "Done: $OutDir"
if ($PublishCollapse) {
    Publish-ToCollapse $CollapseRepo $tag $Version $buildCommit (Get-Content -Raw $notesPath) $isPrerelease $OutDir
} elseif ($Sign) {
    Write-Host "Downloads are signed in $OutDir. Pass -PublishCollapse to publish to Collapse and update updater/latest.json." -ForegroundColor Green
} elseif ($Publish) {
    $assets = @(
        $setupOut,
        $portableOut,
        (Join-Path $OutDir 'SHA256SUMS.txt')
    )
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
    Write-Host "NOTE: Downloads are unsigned. The updater manifest (updater/latest.json) was NOT moved; installed copies will not be offered $Version." -ForegroundColor Yellow
}
