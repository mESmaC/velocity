<#
.SYNOPSIS
Holds a turn of Rosen's run queue for scripts/build-queue.ps1.

.DESCRIPTION
`rosen run` starts this script and keeps its turn while it runs. It marks the
turn as started, then waits until build-queue.ps1 asks it to end or the owning
process exits, so a crashed check never keeps the turn. -For names the work
in the queue's line; nothing reads it.
#>
param(
    [Parameter(Mandatory)][string]$Started,
    [Parameter(Mandatory)][string]$Release,
    [Parameter(Mandatory)][int]$Owner,
    [string]$For = ''
)
New-Item -ItemType File -Path $Started -Force | Out-Null
while (-not (Test-Path -LiteralPath $Release) -and (Get-Process -Id $Owner -ErrorAction SilentlyContinue)) {
    Start-Sleep -Milliseconds 200
}
