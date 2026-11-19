# Records where a release's time went (#434): how long after the trigger each job of the
# run started, how long it took and its longest steps, and the time from trigger to
# published. The release workflow runs it as its last step, so the table lands in the
# run's summary and every release carries its own breakdown. A `-beta` release that took
# longer than -BetaTargetMinutes gets a warning on the run.
#
# It only reads the run, through GitHub's REST API with GH_TOKEN when that is set, so it
# can be run by hand against any finished release:
#   pwsh -NoProfile -File .github/scripts/release-timing.ps1 -Repository owner/name -RunId 123
param(
    [Parameter(Mandatory)] [string] $Repository,
    [Parameter(Mandatory)] [string] $RunId,
    # The release tag. Only a `-beta` tag is held to the target.
    [string] $Tag = '',
    [int] $BetaTargetMinutes = 20,
    # Steps shorter than this are left out of a job's longest steps.
    [int] $StepFloorSeconds = 30
)
$ErrorActionPreference = 'Stop'

function Get-GitHub([string] $Path) {
    $headers = @{ Accept = 'application/vnd.github+json' }
    if ($env:GH_TOKEN) { $headers.Authorization = "Bearer $env:GH_TOKEN" }
    Invoke-RestMethod -Uri "https://api.github.com/$Path" -Headers $headers -MaximumRetryCount 2 -RetryIntervalSec 2
}

# The API's timestamps arrive as text or as dates, depending on the JSON reader.
function ConvertTo-Utc($Value) {
    if ($Value -is [string]) {
        return [DateTimeOffset]::Parse($Value, [Globalization.CultureInfo]::InvariantCulture, 'AssumeUniversal').UtcDateTime
    }
    return ([DateTime]$Value).ToUniversalTime()
}

function Format-Span([TimeSpan] $Span) {
    if ($Span.TotalSeconds -lt 60) { return '{0} s' -f [math]::Round($Span.TotalSeconds) }
    return '{0} min {1:00} s' -f [math]::Floor($Span.TotalMinutes), $Span.Seconds
}

# A job's steps of at least the floor, longest first, at most three.
function Get-LongestSteps($Job) {
    $timed = foreach ($step in $Job.steps) {
        if ($step.started_at -and $step.completed_at) {
            $took = (ConvertTo-Utc $step.completed_at) - (ConvertTo-Utc $step.started_at)
            if ($took.TotalSeconds -ge $StepFloorSeconds) { [pscustomobject]@{ Name = $step.name; Took = $took } }
        }
    }
    $longest = $timed | Sort-Object Took -Descending | Select-Object -First 3
    return ($longest | ForEach-Object { "$($_.Name) $(Format-Span $_.Took)" }) -join '; '
}

function Get-JobRow($Job, [DateTime] $Trigger, [DateTime] $Now) {
    if ($Job.conclusion -ceq 'skipped' -or -not $Job.started_at) {
        return "| $($Job.name) | | | skipped | |"
    }
    $started = ConvertTo-Utc $Job.started_at
    # The job running this script has no end yet; it is measured up to now.
    $ended = if ($Job.completed_at) { ConvertTo-Utc $Job.completed_at } else { $Now }
    $result = if ($Job.conclusion) { $Job.conclusion } else { 'running' }
    return "| $($Job.name) | $(Format-Span ($started - $Trigger)) | $(Format-Span ($ended - $started)) | $result | $(Get-LongestSteps $Job) |"
}

$run = Get-GitHub "repos/$Repository/actions/runs/$RunId"
$jobs = @((Get-GitHub "repos/$Repository/actions/runs/$RunId/jobs?per_page=100").jobs)
$trigger = ConvertTo-Utc $run.created_at
# A finished run is measured to its end, the running release to this step.
$now = if ($run.status -ceq 'completed') { ConvertTo-Utc $run.updated_at } else { [DateTime]::UtcNow }
$total = $now - $trigger

$gateSkipped = @($jobs | Where-Object { $_.name -clike 'Release gate *' -and $_.conclusion -ceq 'skipped' }).Count -gt 0
$path = if ($gateSkipped) { 'beta path, gate stages skipped' } else { 'full gate' }
$lines = @(
    "### Release timing: $(Format-Span $total) from trigger to published"
    ''
    "$Tag run $RunId, $path."
)
if ($run.run_attempt -gt 1) {
    $lines += "This is attempt $($run.run_attempt), so the time includes the wait before the re-run."
}
$lines += @('', '| Job | Started after | Took | Result | Longest steps |', '| --- | ---: | ---: | --- | --- |')
$lines += $jobs | Sort-Object { if ($_.started_at) { ConvertTo-Utc $_.started_at } else { $now } } |
    ForEach-Object { Get-JobRow $_ $trigger $now }
$lines += ''

$lines | ForEach-Object { Write-Host $_ }
if ($env:GITHUB_STEP_SUMMARY) { $lines >> $env:GITHUB_STEP_SUMMARY }

$beta = $Tag -cmatch '^v\d+\.\d+\.\d+-beta(\.|$)'
if ($beta -and $total.TotalMinutes -gt $BetaTargetMinutes) {
    Write-Host "::warning title=Beta release over $BetaTargetMinutes minutes::$Tag took $(Format-Span $total) from trigger to published; the target is $BetaTargetMinutes minutes (#434)."
}
