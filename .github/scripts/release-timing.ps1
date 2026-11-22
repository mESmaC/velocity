# Records where a release's time went (#434): how long after the trigger each job of the
# run started, how long it took and its longest steps, and the time from trigger to
# published. Publication is complete once both the release downloads and the updater
# manifest have been published (#641), so post-publication reporting never shifts the
# elapsed time in live runs or offline reports. The release workflow runs this as its
# last step, so the table lands in the run's summary and every release carries its own
# breakdown. A `-beta` release that took longer than -BetaTargetMinutes gets a warning on
# the run.
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

# Both steps in the Publish job must succeed before downloads and the updater manifest
# are live (#641).
$PublicationSteps = @('Publish the release', 'Publish the update manifest')

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

# Returns the UTC timestamp when every required publication step succeeded, or $null if
# publication has not finished. Uses the same endpoint in live runs and offline reports.
function Get-PublishedAt($Jobs) {
    $candidates = @($Jobs | Where-Object { $_.name -ceq 'Publish' })
    if ($candidates.Count -eq 0) { $candidates = @($Jobs) }
    foreach ($job in $candidates) {
        $steps = @($job.steps)
        if ($steps.Count -eq 0) { continue }
        $completed = foreach ($stepName in $PublicationSteps) {
            $match = $steps |
                Where-Object { $_.name -ceq $stepName -and $_.conclusion -ceq 'success' -and $_.completed_at } |
                Sort-Object { ConvertTo-Utc $_.completed_at } |
                Select-Object -Last 1
            if ($match) { ConvertTo-Utc $match.completed_at }
        }
        $completed = @($completed)
        if ($completed.Count -eq $PublicationSteps.Count) {
            return ($completed | Sort-Object | Select-Object -Last 1)
        }
    }
    return $null
}

$run = Get-GitHub "repos/$Repository/actions/runs/$RunId"
$jobs = @((Get-GitHub "repos/$Repository/actions/runs/$RunId/jobs?per_page=100").jobs)
$trigger = ConvertTo-Utc $run.created_at
$now = if ($run.status -ceq 'completed' -and $run.updated_at) { ConvertTo-Utc $run.updated_at } else { [DateTime]::UtcNow }
$publishedAt = Get-PublishedAt $jobs
$total = if ($null -ne $publishedAt) { $publishedAt - $trigger } else { $null }

$gateSkipped = @($jobs | Where-Object { $_.name -clike 'Release gate *' -and $_.conclusion -ceq 'skipped' }).Count -gt 0
$path = if ($gateSkipped) { 'beta path, gate stages skipped' } else { 'full gate' }
$heading = if ($null -ne $total) {
    "### Release timing: $(Format-Span $total) from trigger to published"
} else {
    "### Release timing: not published ($(Format-Span ($now - $trigger)) elapsed)"
}
$label = if ($Tag) { "$Tag run $RunId" } else { "Run $RunId" }
$lines = @(
    $heading
    ''
    "$label, $path."
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
if ($beta -and $null -ne $total -and $total.TotalMinutes -gt $BetaTargetMinutes) {
    Write-Host "::warning title=Beta release over $BetaTargetMinutes minutes::$Tag took $(Format-Span $total) from trigger to published; the target is $BetaTargetMinutes minutes (#434)."
}
