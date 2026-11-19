# Prints the Rosen git URL and commit Velocity pins as step outputs `url` and `rev`, and
# the Rosen release tag built from that commit as `release` (#433), empty when none is pinned.
# The workspace Cargo.toml holds the pin since #404; older tags kept it in the crate and
# pin no release.
$pin = 'rosen-client = \{ git = "([^"]+)", rev = "([0-9a-f]{40})" \}'
$manifest = @('Cargo.toml', 'crates/velocity-rosen/Cargo.toml') |
    Where-Object { Test-Path $_ } |
    ForEach-Object { Get-Content -Raw $_ } |
    Where-Object { $_ -match $pin } |
    Select-Object -First 1
if (-not $manifest -or $manifest -notmatch $pin) {
    throw 'No pinned rosen-client in Cargo.toml or crates/velocity-rosen/Cargo.toml'
}
$url = $Matches[1]
$rev = $Matches[2]
# The `release` key of the `[workspace.metadata.rosen]` table, read up to the next table.
$release = ''
if ($manifest -match '(?ms)^\[workspace\.metadata\.rosen\]\s*$(.*?)(^\[|\z)') {
    if ($Matches[1] -match '(?m)^release\s*=\s*"([^"]*)"') { $release = $Matches[1] }
}
if ($release -and $release -notmatch '^v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') {
    throw "Rosen release '$release' in Cargo.toml is not a v<semver> tag"
}
"url=$url" >> $env:GITHUB_OUTPUT
"rev=$rev" >> $env:GITHUB_OUTPUT
"release=$release" >> $env:GITHUB_OUTPUT
Write-Host "Rosen pin: $url @ $rev, release '$release'"
