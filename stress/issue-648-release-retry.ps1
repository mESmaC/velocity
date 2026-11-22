param([string]$WorkflowPath = (Join-Path $PSScriptRoot '../.github/workflows/release.yml'))

$ErrorActionPreference = 'Stop'
$workflow = Get-Content -Raw -LiteralPath $WorkflowPath
$step = [regex]::Match($workflow, '(?m)^      - name: Publish the release\r?\n        shell: pwsh\r?\n        run: \|\r?\n((?:          [^\r\n]*\r?\n|\r?\n)+)')
if (!$step.Success) { throw 'Could not locate the PowerShell Publish the release step.' }
$publish = [scriptblock]::Create(($step.Groups[1].Value -replace '(?m)^          ', ''))

$env:TAG = 'v0.1.0-beta.17'
$env:VERSION = $env:TAG.TrimStart('v')
$env:PRERELEASE = 'true'
$archive = "velocity-graph-$env:VERSION-x86_64-unknown-linux-gnu.tar.gz"
$checksum = "$archive.sha256"

# Model the release asset commands in memory. No GitHub credentials or requests are used.
function gh {
    $global:LASTEXITCODE = 0
    if ($args[0] -ne 'release' -or $args[2] -ne $env:TAG) {
        throw "Unexpected command: gh $args"
    }
    switch ($args[1]) {
        'view' {
            if ($args -contains '.assets[].name') { $script:remoteAssets.Keys }
            if ($args -contains '.body') { 'Existing release notes' }
        }
        'download' {
            $patternIndex = [Array]::IndexOf($args, '--pattern')
            if ($patternIndex -lt 0) { throw "Expected a download pattern: gh $args" }
            $name = $args[$patternIndex + 1]
            if (!$script:remoteAssets.ContainsKey($name)) { throw "Asset $name does not exist." }
            $directoryIndex = [Array]::IndexOf($args, '--dir')
            $destination = if ($directoryIndex -lt 0) { $name } else {
                Join-Path $args[$directoryIndex + 1] $name
            }
            Set-Content -LiteralPath $destination -Value $script:remoteAssets[$name] -NoNewline
        }
        'upload' {
            $paths = foreach ($argument in $args[3..($args.Length - 1)]) { $argument }
            foreach ($localPath in ($paths | Where-Object { $_ -notlike '--*' })) {
                $name = Split-Path -Leaf $localPath
                if ($script:remoteAssets.ContainsKey($name) -and $args -notcontains '--clobber') {
                    $global:LASTEXITCODE = 1
                    return
                }
                $script:remoteAssets[$name] = Get-Content -Raw -LiteralPath $localPath
            }
        }
        'edit' {}
        default { throw "Unexpected command: gh $args" }
    }
}

function Get-Sha256([string]$text) {
    $bytes = [Text.Encoding]::UTF8.GetBytes($text)
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
}

$work = [IO.Directory]::CreateTempSubdirectory('velocity-release-retry-').FullName
$dist = Join-Path $work 'dist'
$null = New-Item -ItemType Directory -Path $dist
Push-Location $work
try {
    # A previous attempt completed the archive upload and stopped before publishing the checksum.
    $original = 'archive bytes published by the first run'
    $originalDigest = Get-Sha256 $original
    $script:remoteAssets = @{ $archive = $original }
    $rebuilt = 'different archive bytes from a rebuild of the same tag'
    Set-Content -LiteralPath (Join-Path $dist $archive) -Value $rebuilt -NoNewline
    Set-Content -LiteralPath (Join-Path $dist $checksum) -Value "$(Get-Sha256 $rebuilt)  $archive"

    & $publish

    if ($script:remoteAssets[$archive] -cne $original) {
        throw "Release retry replaced $archive. Expected SHA-256 $originalDigest but got $(Get-Sha256 $script:remoteAssets[$archive])."
    }
    if (!$script:remoteAssets.ContainsKey($checksum) -or $script:remoteAssets[$checksum].Trim() -cne "$originalDigest  $archive") {
        throw 'The retry did not publish the checksum of the original archive.'
    }
} finally {
    Pop-Location
    $resolvedWork = (Resolve-Path -LiteralPath $work).ProviderPath
    $tempRoot = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetTempPath())
    if ($resolvedWork -eq $work -and (Split-Path -Parent $resolvedWork) -eq $tempRoot) {
        Remove-Item -LiteralPath $resolvedWork -Recurse -Force
    }
}
