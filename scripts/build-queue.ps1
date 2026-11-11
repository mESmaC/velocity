<#
.SYNOPSIS
Runs a heavy build command alone on this machine, after any build already running.

.DESCRIPTION
Several agents compiling at once each assume they have every core, so they all
finish late together: a build that takes 4 minutes alone takes 20 when five
run side by side. This script holds a machine-wide lock while its command runs,
so builds run one at a time at full speed and the first in line finishes first.

The lock is a named Windows mutex. Windows releases it when its holder exits,
even after a crash, so a dead build never blocks the queue. Waiters print who
holds the lock every minute.

Run cargo build, test, clippy and run through it. Never run `cargo tauri dev`
through it: the dev server runs until stopped and would hold the queue.

Dot-source it to get Invoke-InBuildQueue for use inside another script
(scripts/check.ps1 does this).

.EXAMPLE
pwsh -NoProfile -File scripts/build-queue.ps1 cargo build --workspace
#>

$script:BuildQueueMutexName = 'Global\velocity-build-queue'
$script:BuildQueueHolderFile = Join-Path $env:LOCALAPPDATA 'velocity\build-queue-holder.json'

function Get-BuildQueueHolder {
    try { Get-Content -LiteralPath $script:BuildQueueHolderFile -Raw | ConvertFrom-Json } catch { $null }
}

function Write-BuildQueueWait([datetime]$WaitStart) {
    $holder = Get-BuildQueueHolder
    $waited = [int]((Get-Date) - $WaitStart).TotalMinutes
    if ($holder -and (Get-Process -Id $holder.processId -ErrorAction SilentlyContinue)) {
        $held = [int]((Get-Date) - [datetime]$holder.startedAt).TotalMinutes
        Write-Host "Build queue: waiting ${waited}m. PID $($holder.processId) has held it ${held}m: $($holder.command) in $($holder.directory)" -ForegroundColor DarkYellow
    } else {
        Write-Host "Build queue: waiting ${waited}m for the current build to finish." -ForegroundColor DarkYellow
    }
}

# Runs $Command while holding the machine-wide build lock. The mutex is
# re-entrant on the same thread, so nested calls in one script never deadlock.
function Invoke-InBuildQueue([string]$Description, [scriptblock]$Command) {
    # A queued aggregate runner starts scripts that queue their own cargo calls.
    # They share the parent's lifetime and must not wait on its mutex again.
    $holder = Get-BuildQueueHolder
    if ($env:VELOCITY_BUILD_QUEUE_OWNER -and $holder -and
        "$($holder.processId)" -eq $env:VELOCITY_BUILD_QUEUE_OWNER -and
        (Get-Process -Id $holder.processId -ErrorAction SilentlyContinue)) {
        & $Command
        return
    }
    $mutex = [System.Threading.Mutex]::new($false, $script:BuildQueueMutexName)
    $waitStart = Get-Date
    $acquired = $false
    while (-not $acquired) {
        try {
            $acquired = $mutex.WaitOne([TimeSpan]::FromMinutes(1))
        } catch [System.Threading.AbandonedMutexException] {
            # The previous holder exited without releasing it; the lock is ours.
            $acquired = $true
        }
        if (-not $acquired) { Write-BuildQueueWait $waitStart }
    }
    try {
        $previousQueueOwner = $env:VELOCITY_BUILD_QUEUE_OWNER
        $env:VELOCITY_BUILD_QUEUE_OWNER = "$PID"
        $holderFolder = Split-Path -Parent $script:BuildQueueHolderFile
        New-Item -ItemType Directory -Force -Path $holderFolder | Out-Null
        @{ processId = $PID; command = $Description; directory = (Get-Location).Path; startedAt = (Get-Date).ToString('o') } |
            ConvertTo-Json | Set-Content -LiteralPath $script:BuildQueueHolderFile -Encoding utf8
        & $Command
    } finally {
        $env:VELOCITY_BUILD_QUEUE_OWNER = $previousQueueOwner
        Remove-Item -LiteralPath $script:BuildQueueHolderFile -ErrorAction SilentlyContinue
        $mutex.ReleaseMutex()
        $mutex.Dispose()
    }
}

# Run as a script: everything after the script name is the command to queue.
if ($MyInvocation.InvocationName -ne '.') {
    if ($args.Count -eq 0) {
        Write-Host 'Usage: pwsh -NoProfile -File scripts/build-queue.ps1 <command> [arguments]' -ForegroundColor Red
        exit 2
    }
    $commandName = $args[0]
    $commandArguments = @($args | Select-Object -Skip 1)
    $script:queuedExitCode = 0
    Invoke-InBuildQueue ($args -join ' ') {
        & $commandName @commandArguments
        $script:queuedExitCode = $LASTEXITCODE
    }
    exit $script:queuedExitCode
}
