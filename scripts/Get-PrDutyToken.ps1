<#
.SYNOPSIS
Prints a GitHub installation token for the PR duty GitHub App.

.DESCRIPTION
PR duty agents run `gh` with this token so their reviews and merges come from
the app's bot account, not the shared login that opened the PR (#42). A shared
login cannot approve its own PRs; the bot can.

The token lasts one hour. It is cached under %LOCALAPPDATA%\velocity and reused
while it has more than ten minutes left, so agents can call this before every
`gh` command. Nothing secret lives in the repo: the App ID, Installation ID and
private key path come from the environment or the parameters.

.EXAMPLE
$env:GH_TOKEN = pwsh -NoProfile -File scripts/Get-PrDutyToken.ps1; gh pr review 43 --approve
#>
param(
    [string]$AppId = $env:VELOCITY_PR_DUTY_APP_ID,
    [string]$InstallationId = $env:VELOCITY_PR_DUTY_INSTALLATION_ID,
    [string]$KeyPath = $env:VELOCITY_PR_DUTY_KEY
)
$ErrorActionPreference = 'Stop'

foreach ($setting in @(
        @{ Name = 'VELOCITY_PR_DUTY_APP_ID'; Value = $AppId },
        @{ Name = 'VELOCITY_PR_DUTY_INSTALLATION_ID'; Value = $InstallationId },
        @{ Name = 'VELOCITY_PR_DUTY_KEY'; Value = $KeyPath })) {
    if ([string]::IsNullOrWhiteSpace($setting.Value)) {
        throw "Set $($setting.Name) (see scripts/Get-PrDutyToken.ps1)."
    }
}

$cacheFolder = Join-Path $env:LOCALAPPDATA 'velocity'
$cachePath = Join-Path $cacheFolder "pr-duty-token-$InstallationId.json"
if (Test-Path -LiteralPath $cachePath) {
    $cached = Get-Content -Raw -LiteralPath $cachePath | ConvertFrom-Json
    if ([DateTimeOffset]::Parse($cached.expires_at) -gt [DateTimeOffset]::UtcNow.AddMinutes(10)) {
        return $cached.token
    }
}

function ConvertTo-Base64Url([byte[]]$Bytes) {
    [Convert]::ToBase64String($Bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_')
}

# GitHub App JWT: RS256, issued a minute early to absorb clock skew, max 10 minutes.
$now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
$header = ConvertTo-Base64Url ([Text.Encoding]::UTF8.GetBytes('{"alg":"RS256","typ":"JWT"}'))
$claims = @{ iat = $now - 60; exp = $now + 540; iss = $AppId } | ConvertTo-Json -Compress
$payload = ConvertTo-Base64Url ([Text.Encoding]::UTF8.GetBytes($claims))
$rsa = [Security.Cryptography.RSA]::Create()
try {
    $rsa.ImportFromPem((Get-Content -Raw -LiteralPath $KeyPath))
    $signature = $rsa.SignData(
        [Text.Encoding]::UTF8.GetBytes("$header.$payload"),
        [Security.Cryptography.HashAlgorithmName]::SHA256,
        [Security.Cryptography.RSASignaturePadding]::Pkcs1)
} finally {
    $rsa.Dispose()
}
$jwt = "$header.$payload.$(ConvertTo-Base64Url $signature)"

$response = Invoke-RestMethod -Method Post `
    -Uri "https://api.github.com/app/installations/$InstallationId/access_tokens" `
    -Headers @{
        Authorization          = "Bearer $jwt"
        Accept                 = 'application/vnd.github+json'
        'X-GitHub-Api-Version' = '2022-11-28'
    }

New-Item -ItemType Directory -Force -Path $cacheFolder | Out-Null
@{ token = $response.token; expires_at = $response.expires_at } |
    ConvertTo-Json -Compress |
    Set-Content -LiteralPath $cachePath -Encoding utf8NoBOM
$response.token
