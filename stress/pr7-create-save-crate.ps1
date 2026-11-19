# Same failed-save and retry assertions as pr7-create-save.ps1, adapted to the
# extracted velocity-new-project crate. The original script remains unchanged.
# Calls the real executor with real store/git/filesystem code; only event delivery
# is a no-op. Does not exercise the native UI or IPC serialization/transport.
# Adapted for #499: the harness is stress/harness/sources/pr7-create-save-crate.rs, built with
# every other Rust harness into the stress crate's stress-harness binary instead of a Cargo
# project this script wrote into the fixture. Its source, assertions, fixture folder and the
# fixture Rosen server cleanup are unchanged.
param()
$ErrorActionPreference = 'Stop'
$repository = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixture = Join-Path $repository ('.tools\pr7-create-save-crate\' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture -Force | Out-Null
Import-Module (Join-Path $PSScriptRoot 'stress-harness.psm1') -Force
Write-Output "Fixture: $fixture"
try {
    Invoke-StressHarness pr7-create-save-crate $fixture
    $harnessExitCode = $LASTEXITCODE
} finally {
    if ($IsWindows) {
        # The server uses a canonical path, which can differ from the workspace drive.
        # Match the unique fixture directory and the rest of the quoted data-dir argument.
        $dataDirectorySuffix = (Split-Path -Leaf $fixture) + '\new project\.rosen\server'
        $dataDirectoryArgument = '\s--data-dir\s+"[^"]*\\' + [regex]::Escape($dataDirectorySuffix) + '"(?=\s|$)'
        $fixtureServers = Get-CimInstance -ClassName Win32_Process -Filter "Name = 'rosen-server.exe'" |
            Where-Object { $_.CommandLine -match $dataDirectoryArgument }
        foreach ($fixtureServer in $fixtureServers) {
            $server = Get-Process -Id $fixtureServer.ProcessId -ErrorAction SilentlyContinue
            if ($null -eq $server) {
                continue
            }
            try {
                Stop-Process -InputObject $server -Force
            } catch {
                if (-not $server.HasExited) {
                    throw
                }
            }
            if (-not $server.WaitForExit(5000)) {
                throw "Fixture Rosen server $($server.Id) did not exit for $fixture"
            }
            Write-Output "Stopped fixture Rosen server $($server.Id)"
        }
    }
}
exit $harnessExitCode
