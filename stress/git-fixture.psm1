# Git-specific regressions use reviewed source files, without requiring the outer
# review tree to have Git metadata or borrowing a different checkout's revision.
function Invoke-FixtureGit([string]$Fixture, [string[]]$Arguments) {
    $output = git -C $Fixture @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Fixture git $Arguments failed" }
    return $output
}

function New-StressGitFixture([string]$Source, [string[]]$Paths) {
    $fixture = Join-Path ([IO.Path]::GetTempPath()) ("velocity git fixture " + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $fixture | Out-Null
    try {
        foreach ($relative in $Paths) {
            $destination = [IO.Path]::GetFullPath((Join-Path $fixture $relative))
            if (-not $destination.StartsWith($fixture + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
                throw "Fixture path escaped its directory: $relative"
            }
            New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
            Copy-Item -LiteralPath (Join-Path $Source $relative) -Destination $destination
        }
        Invoke-FixtureGit $fixture @('init', '--quiet', '-b', 'master') | Out-Null
        Invoke-FixtureGit $fixture @('config', 'core.autocrlf', 'true')
        Invoke-FixtureGit $fixture @('config', 'core.longpaths', 'true')
        Invoke-FixtureGit $fixture @('add', '--all')
        Invoke-FixtureGit $fixture @('-c', 'user.name=stress', '-c', 'user.email=stress@example.invalid', 'commit', '--quiet', '-m', 'Reviewed sources under test')
        # Force the same CRLF checkout that the old autocrlf shared clone supplied.
        Invoke-FixtureGit $fixture @('checkout-index', '--all', '--force')
        return $fixture
    } catch {
        Remove-StressGitFixture $fixture
        throw
    }
}

function Remove-StressGitFixture([string]$Fixture) {
    $temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $resolved = [IO.Path]::GetFullPath($Fixture)
    if (-not $resolved.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or
        -not ([IO.Path]::GetFileName($resolved)).StartsWith('velocity git fixture ')) {
        throw "Refusing to remove a path outside the temporary Git fixtures: $resolved"
    }
    if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}

Export-ModuleMember -Function New-StressGitFixture, Remove-StressGitFixture, Invoke-FixtureGit
