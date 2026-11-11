# Risk (#213): the build queue let later builds overtake earlier waiters, so under
# swarm load some builds waited 45 minutes. Every agent's build goes through it,
# so it must hand the lock over in arrival order (by priority, SPEC.md F5), never
# run two builds at once, and never stall when a holder or a waiter is killed.
# It runs on its own queue instance, so it never touches the swarm's real queue.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$queueScript = Join-Path $repositoryRoot 'scripts\build-queue.ps1'
$instance = "stress-213-$PID"
$env:VELOCITY_BUILD_QUEUE_INSTANCE = $instance
# The stress runner holds the real queue; this test's queue is a different one.
Remove-Item Env:VELOCITY_BUILD_QUEUE_OWNER -ErrorAction SilentlyContinue
$queueFolder = Join-Path $env:LOCALAPPDATA "velocity\build-queue-$instance"
$work = Join-Path ([IO.Path]::GetTempPath()) "velocity-$instance"
New-Item -ItemType Directory -Force -Path $work | Out-Null
$log = Join-Path $work 'order.log'
# The queued "build": log its start, hold the lock for a while, log its end.
$worker = Join-Path $work 'worker.cmd'
Set-Content -LiteralPath $worker -Encoding ascii -Value @(
    '@echo start %1>>"%2"', '@ping -n %3 127.0.0.1 >nul', '@echo end %1>>"%2"')
$started = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()

function Get-Tickets { @(Get-ChildItem -LiteralPath $queueFolder -Filter '*.ticket' -ErrorAction SilentlyContinue) }
function Get-Held { @(Get-ChildItem -LiteralPath $queueFolder -Filter '*.held' -ErrorAction SilentlyContinue) }

function Wait-Until([scriptblock]$Condition, [string]$What, [int]$Seconds = 60) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while (-not (& $Condition)) {
        if ((Get-Date) -gt $deadline) { throw "Timed out waiting for $What" }
        Start-Sleep -Milliseconds 50
    }
}

# Starts one queued build and returns once its ticket is in line (or it holds
# the lock), so the arrival order is exactly the order of these calls.
function Start-QueuedBuild([string]$Name, [int]$Seconds = 1, [string]$Priority = 'normal') {
    $arguments = @('-NoProfile', '-File', "`"$queueScript`"", '-Priority', $Priority, 'cmd', '/c', "`"$worker`"", $Name, "`"$log`"", ($Seconds + 1))
    $process = Start-Process pwsh -ArgumentList $arguments -PassThru -WindowStyle Hidden
    $started.Add($process)
    # Ticket names end in the waiter's PID; held files start with it.
    $joined = { $process.HasExited -or
        (Get-Tickets | Where-Object Name -Like "*-$($process.Id).ticket") -or
        (Get-Held | Where-Object Name -Like "$($process.Id)-*") }
    Wait-Until $joined "$Name to join the queue"
    return $process
}

# Stops a process this test started, with the build it runs.
function Stop-Started([System.Diagnostics.Process]$Process) {
    & taskkill.exe /PID $Process.Id /T /F | Out-Null
    $Process.WaitForExit()
}

function Assert-Order([string[]]$Expected, [string]$Case) {
    $lines = @(Get-Content -LiteralPath $log | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    $expectedLines = @($Expected | ForEach-Object { "start $_"; "end $_" })
    if (($lines -join '|') -ne ($expectedLines -join '|')) {
        throw "$Case`: expected $($expectedLines -join ', ') but got $($lines -join ', ')"
    }
    Write-Host "PASS $Case`: $($Expected -join ' -> ')"
}

function Wait-Drained([System.Diagnostics.Process[]]$Processes, [int]$Seconds = 120) {
    foreach ($process in $Processes) {
        if (-not $process.WaitForExit($Seconds * 1000)) { throw "Build PID $($process.Id) did not finish: the queue stalled" }
    }
    Wait-Until { (Get-Tickets).Count -eq 0 -and (Get-Held).Count -eq 0 } 'the queue folder to empty' 10
}

function Reset-Log { Remove-Item -LiteralPath $log -ErrorAction SilentlyContinue }

try {
    # 1. Twelve waiters arrive one after another behind a holder: they run in arrival order, one at a time.
    Reset-Log
    $holder = Start-QueuedBuild 'holder' 20
    $waiters = foreach ($index in 1..12) { Start-QueuedBuild "w$index" 0 }
    Wait-Drained (@($holder) + $waiters)
    Assert-Order (@('holder') + (1..12 | ForEach-Object { "w$_" })) 'arrival order'

    # 2. A waiter at the head of the line and one in the middle are killed, then the holder is killed mid-build.
    Reset-Log
    $holder = Start-QueuedBuild 'stuck' 120
    Wait-Until { Test-Path -LiteralPath $log } 'the holder to start'
    $headWaiter = Start-QueuedBuild 'a' 0
    $b = Start-QueuedBuild 'b' 0
    $middleWaiter = Start-QueuedBuild 'c' 0
    $d = Start-QueuedBuild 'd' 0
    Stop-Started $headWaiter
    Stop-Started $middleWaiter
    Wait-Until { (Get-Tickets).Count -eq 2 } 'killed waiters to leave the line' 10
    $killedAt = Get-Date
    Stop-Started $holder
    Wait-Drained @($b, $d) 30
    $lines = @(Get-Content -LiteralPath $log | ForEach-Object { $_.Trim() })
    if ($lines[0] -ne 'start stuck' -or $lines -contains 'end stuck') { throw "killed holder: unexpected log $($lines -join ', ')" }
    Set-Content -LiteralPath $log -Value ($lines | Select-Object -Skip 1)
    Assert-Order @('b', 'd') 'killed holder and waiters'
    Write-Host "  queue moved on $([int]((Get-Date) - $killedAt).TotalSeconds)s after the holder was killed"

    # 3. Priority: landing checks, then ready checks, then the rest, first come, first served within each.
    Reset-Log
    $holder = Start-QueuedBuild 'busy' 15
    $queued = @(
        Start-QueuedBuild 'n1' 0 'normal'
        Start-QueuedBuild 'n2' 0 'normal'
        Start-QueuedBuild 'r1' 0 'ready'
        Start-QueuedBuild 'l1' 0 'landing'
        Start-QueuedBuild 'n3' 0 'normal'
        Start-QueuedBuild 'r2' 0 'ready'
        Start-QueuedBuild 'l2' 0 'landing'
    )
    Wait-Drained (@($holder) + $queued)
    Assert-Order @('busy', 'l1', 'l2', 'r1', 'r2', 'n1', 'n2', 'n3') 'priority order'
    Write-Host 'All build queue stress cases passed.'
} finally {
    foreach ($process in $started) {
        if (-not $process.HasExited) { Stop-Started $process }
    }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $queueFolder -Recurse -Force -ErrorAction SilentlyContinue
}
