# #434: time spent reporting after publication must not change the beta verdict.
$ErrorActionPreference = 'Stop'
Remove-Item Env:GITHUB_STEP_SUMMARY -ErrorAction SilentlyContinue

$trigger = [DateTime]::UtcNow.AddMinutes(-22)
$publication = $trigger.AddMinutes(19)
$tag = 'v0.1.0-beta.1'

function Invoke-RestMethod {
    param($Uri, $Headers, $MaximumRetryCount, $RetryIntervalSec)
    switch -Wildcard ($Uri) {
        '*/actions/runs/1' {
            return @{
                created_at = $trigger
                status = 'in_progress'
                run_attempt = 1
            }
        }
        '*/actions/runs/1/jobs?per_page=100' {
            return @{
                jobs = @(@{
                    name = 'Publish'
                    started_at = $trigger.AddMinutes(18)
                    steps = @(
                        @{ name = 'Publish the release'; started_at = $trigger.AddMinutes(18); completed_at = $publication.AddSeconds(-10); conclusion = 'success' }
                        @{ name = 'Publish the update manifest'; started_at = $publication.AddSeconds(-10); completed_at = $publication; conclusion = 'success' }
                        @{ name = 'Get the timing script'; started_at = $publication; completed_at = $trigger.AddMinutes(21); conclusion = 'success' }
                    )
                })
            }
        }
        "*/releases/tags/$tag" { return @{ published_at = $publication; tag_name = $tag; draft = $false } }
        default { throw "Unexpected API request: $Uri" }
    }
}

$timingScript = (Resolve-Path -LiteralPath '.github/scripts/release-timing.ps1').Path
$report = (& $timingScript -Repository 'example/velocity' -RunId '1' -Tag $tag 6>&1) -join [Environment]::NewLine
Write-Output $report
if ($report -notmatch '19 min 00 s from trigger to published' -or $report -match '::warning title=Beta release over') {
    Write-Output 'FAIL: both publication steps finished within 19 minutes. The report should say 19 min 00 s and issue no warning.'
    exit 1
}
