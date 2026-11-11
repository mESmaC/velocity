<#
.SYNOPSIS
Runs a heavy build command alone on this machine, after the builds queued before it.

.DESCRIPTION
Several agents compiling at once each assume they have every core, so they all
finish late together: a build that takes 4 minutes alone takes 20 when five
run side by side. This script holds a machine-wide lock while its command runs,
so builds run one at a time at full speed.

The queue is first come, first served (#213). Each waiter takes a ticket: a
file in the queue folder, named by its priority and its place in line, that the
waiter holds open with delete-on-close. Windows closes the handle when the
process exits, even after a crash or a kill, so a dead waiter's ticket vanishes
and never blocks the line. Only the waiter at the head of the line asks for the
lock, a named Windows mutex, which Windows also releases when its holder dies.

Priority (SPEC.md F5): landing checks go first, then checks before a PR is
marked ready, then everything else. First come, first served within each.
Pass -Priority landing|ready|normal before the command; the default is normal.

Waiters print their place in line and who holds the lock every minute.

Run cargo build, test, clippy and run through it. Never run `cargo tauri dev`
through it: the dev server runs until stopped and would hold the queue.

Dot-source it to get Invoke-InBuildQueue for use inside another script
(scripts/check.ps1 does this).

.EXAMPLE
pwsh -NoProfile -File scripts/build-queue.ps1 cargo build --workspace

.EXAMPLE
pwsh -NoProfile -File scripts/build-queue.ps1 -Priority landing cargo build --workspace
#>

# VELOCITY_BUILD_QUEUE_INSTANCE gives a separate queue, so the stress test never
# touches the queue the swarm's real builds wait in.
$script:BuildQueueSuffix = if ($env:VELOCITY_BUILD_QUEUE_INSTANCE) { "-$env:VELOCITY_BUILD_QUEUE_INSTANCE" } else { '' }
$script:BuildQueueMutexName = "Global\velocity-build-queue$script:BuildQueueSuffix"
# Held only while a ticket is numbered, so tickets number in the order they were taken.
$script:BuildQueueRegisterMutexName = "Global\velocity-build-queue-register$script:BuildQueueSuffix"
$script:BuildQueueFolder = Join-Path $env:LOCALAPPDATA "velocity\build-queue$script:BuildQueueSuffix"
$script:BuildQueuePriorities = @{ landing = 0; ready = 1; normal = 2 }
$script:BuildQueueHeld = $false

# Waits for a mutex, treating one its dead holder abandoned as acquired.
function Wait-BuildQueueMutex([System.Threading.Mutex]$Mutex, [TimeSpan]$Timeout) {
    try { return $Mutex.WaitOne($Timeout) } catch [System.Threading.AbandonedMutexException] { return $true }
}

# Opens a file that Windows deletes when this process closes it or dies.
function New-BuildQueueFile([string]$Path) {
    $share = [System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete
    return [System.IO.FileStream]::new($Path, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::ReadWrite,
        $share, 4096, [System.IO.FileOptions]::DeleteOnClose)
}

# A ticket name sorts into queue order: priority, then sequence, then process.
# Sequence is the time of registration in ticks, taken under the register mutex
# and kept strictly increasing, so a later ticket never sorts ahead of an earlier one.
function New-BuildQueueTicket([string]$Priority, [string]$Description) {
    New-Item -ItemType Directory -Force -Path $script:BuildQueueFolder | Out-Null
    $register = [System.Threading.Mutex]::new($false, $script:BuildQueueRegisterMutexName)
    [void](Wait-BuildQueueMutex $register ([TimeSpan]::FromMinutes(5)))
    try {
        $sequence = [DateTime]::UtcNow.Ticks
        foreach ($ticket in Get-BuildQueueTickets) {
            if ($ticket.sequence -ge $sequence) { $sequence = $ticket.sequence + 1 }
        }
        $name = '{0}-{1:D19}-{2}.ticket' -f $script:BuildQueuePriorities[$Priority], $sequence, $PID
        $stream = New-BuildQueueFile (Join-Path $script:BuildQueueFolder $name)
    } finally {
        $register.ReleaseMutex()
        $register.Dispose()
    }
    $ticket = @{ name = $name; stream = $stream; priority = $Priority; command = $Description
        directory = (Get-Location).Path; arrivedAt = (Get-Date).ToString('o'); acquiredAt = $null
    }
    Write-BuildQueueTicketDetails $ticket
    return $ticket
}

# A file's content is for display only: the queue order comes from ticket names.
function Write-BuildQueueTicketDetails([hashtable]$Ticket, [System.IO.FileStream]$Stream = $Ticket.stream) {
    $details = [ordered]@{ processId = $PID; priority = $Ticket.priority; command = $Ticket.command
        directory = $Ticket.directory; arrivedAt = $Ticket.arrivedAt; acquiredAt = $Ticket.acquiredAt
    } | ConvertTo-Json -Compress
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($details)
    $Stream.SetLength(0)
    $Stream.Write($bytes, 0, $bytes.Length)
    $Stream.Flush()
}

# The holder leaves the line once it has the lock and marks itself in a held
# file instead, so a landing check that arrives during its build can take the
# head of the line without looking like the holder.
function Get-BuildQueueHolder {
    $files = try { [System.IO.Directory]::GetFiles($script:BuildQueueFolder, '*.held') } catch { @() }
    foreach ($file in $files) {
        $details = Read-BuildQueueTicketDetails $file
        if ($details -and (Get-Process -Id $details.processId -ErrorAction SilentlyContinue)) { return $details }
    }
    return $null
}

# Live tickets in queue order. A ticket whose process has died is skipped even if
# its file lingers, so a dead waiter never holds up the line.
function Get-BuildQueueTickets {
    $files = try { [System.IO.Directory]::GetFiles($script:BuildQueueFolder, '*.ticket') } catch { @() }
    $tickets = foreach ($file in $files) {
        $name = [System.IO.Path]::GetFileName($file)
        if ($name -notmatch '^(\d)-(\d{19})-(\d+)\.ticket$') { continue }
        $processId = [int]$Matches[3]
        if (-not (Get-Process -Id $processId -ErrorAction SilentlyContinue)) { continue }
        [pscustomobject]@{ name = $name; path = $file; priority = [int]$Matches[1]; sequence = [long]$Matches[2]; processId = $processId }
    }
    return @($tickets | Sort-Object name)
}

function Read-BuildQueueTicketDetails([string]$Path) {
    try {
        $share = [System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete
        $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, $share)
        try { [System.IO.StreamReader]::new($stream).ReadToEnd() | ConvertFrom-Json } finally { $stream.Dispose() }
    } catch { $null }
}

function Write-BuildQueueWait([datetime]$WaitStart, [object[]]$Tickets, [string]$OwnName) {
    $waited = [int]((Get-Date) - $WaitStart).TotalMinutes
    $place = [array]::IndexOf(@($Tickets | ForEach-Object name), $OwnName) + 1
    $line = "Build queue: waiting ${waited}m, place $place of $($Tickets.Count)."
    $holder = Get-BuildQueueHolder
    if ($holder) {
        $held = [int]((Get-Date) - [datetime]$holder.acquiredAt).TotalMinutes
        $line += " PID $($holder.processId) has held it ${held}m: $($holder.command) in $($holder.directory)"
    } else {
        $line += ' Waiting for the current build to finish.'
    }
    Write-Host $line -ForegroundColor DarkYellow
}

# Waits until $Ticket is at the head of the line and holds the build mutex.
# Only the head asks for the mutex. It asks in short slices and looks at the line
# between them, so a landing check that arrives meanwhile still goes first.
function Wait-BuildQueueTurn([hashtable]$Ticket, [System.Threading.Mutex]$Mutex) {
    $waitStart = Get-Date
    $nextReport = $waitStart.AddMinutes(1)
    while ($true) {
        $tickets = Get-BuildQueueTickets
        if ($tickets.Count -gt 0 -and $tickets[0].name -eq $Ticket.name) {
            if (Wait-BuildQueueMutex $Mutex ([TimeSpan]::FromMilliseconds(500))) { return }
        } else {
            Start-Sleep -Milliseconds 250
        }
        if ((Get-Date) -ge $nextReport) {
            Write-BuildQueueWait $waitStart $tickets $Ticket.name
            $nextReport = $nextReport.AddMinutes(1)
        }
    }
}

# A queued aggregate runner starts scripts that queue their own cargo calls.
# They share the holder's lifetime and must not queue behind it again.
function Test-BuildQueueHeldHere {
    if ($script:BuildQueueHeld) { return $true }
    $owner = $env:VELOCITY_BUILD_QUEUE_OWNER
    if (-not $owner) { return $false }
    $holder = Get-BuildQueueHolder
    return [bool]($holder -and "$($holder.processId)" -eq $owner)
}

# Runs $Command while holding the machine-wide build lock, after every waiter
# that arrived earlier at the same or a higher priority.
function Invoke-InBuildQueue([string]$Description, [scriptblock]$Command, [string]$Priority = 'normal') {
    if (-not $script:BuildQueuePriorities.ContainsKey($Priority)) {
        throw "Unknown build queue priority '$Priority'. Use landing, ready or normal."
    }
    if (Test-BuildQueueHeldHere) {
        & $Command
        return
    }
    $mutex = [System.Threading.Mutex]::new($false, $script:BuildQueueMutexName)
    $ticket = New-BuildQueueTicket $Priority $Description
    $acquired = $false
    $heldStream = $null
    $previousQueueOwner = $env:VELOCITY_BUILD_QUEUE_OWNER
    try {
        Wait-BuildQueueTurn $ticket $mutex
        $acquired = $true
        $ticket.acquiredAt = (Get-Date).ToString('o')
        $heldStream = New-BuildQueueFile (Join-Path $script:BuildQueueFolder "$PID-$([guid]::NewGuid().ToString('N')).held")
        Write-BuildQueueTicketDetails $ticket $heldStream
        # Leave the line: the next waiter now waits on the lock itself.
        $ticket.stream.Dispose()
        $script:BuildQueueHeld = $true
        $env:VELOCITY_BUILD_QUEUE_OWNER = "$PID"
        & $Command
    } finally {
        $env:VELOCITY_BUILD_QUEUE_OWNER = $previousQueueOwner
        $script:BuildQueueHeld = $false
        if ($acquired) { $mutex.ReleaseMutex() }
        $mutex.Dispose()
        $ticket.stream.Dispose()
        if ($heldStream) { $heldStream.Dispose() }
    }
}

# Run as a script: an optional -Priority, then the command to queue.
if ($MyInvocation.InvocationName -ne '.') {
    $queueArguments = @($args)
    $queuePriority = 'normal'
    if ($queueArguments.Count -ge 2 -and $queueArguments[0] -eq '-Priority') {
        $queuePriority = "$($queueArguments[1])"
        $queueArguments = @($queueArguments | Select-Object -Skip 2)
    }
    if ($queueArguments.Count -eq 0 -or -not $script:BuildQueuePriorities.ContainsKey($queuePriority)) {
        Write-Host 'Usage: pwsh -NoProfile -File scripts/build-queue.ps1 [-Priority landing|ready|normal] <command> [arguments]' -ForegroundColor Red
        exit 2
    }
    $commandName = $queueArguments[0]
    $commandArguments = @($queueArguments | Select-Object -Skip 1)
    $script:queuedExitCode = 0
    Invoke-InBuildQueue ($queueArguments -join ' ') {
        & $commandName @commandArguments
        $script:queuedExitCode = $LASTEXITCODE
    } $queuePriority
    exit $script:queuedExitCode
}
