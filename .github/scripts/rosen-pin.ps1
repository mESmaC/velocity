# Prints the Rosen git URL and commit Velocity pins as step outputs `url` and `rev`.
# The workspace Cargo.toml holds the pin since #404; older tags kept it in the crate.
$pin = 'rosen-client = \{ git = "([^"]+)", rev = "([0-9a-f]{40})" \}'
$manifest = @('Cargo.toml', 'crates/velocity-rosen/Cargo.toml') |
    Where-Object { Test-Path $_ } |
    ForEach-Object { Get-Content -Raw $_ } |
    Where-Object { $_ -match $pin } |
    Select-Object -First 1
if (-not $manifest -or $manifest -notmatch $pin) {
    throw 'No pinned rosen-client in Cargo.toml or crates/velocity-rosen/Cargo.toml'
}
"url=$($Matches[1])" >> $env:GITHUB_OUTPUT
"rev=$($Matches[2])" >> $env:GITHUB_OUTPUT
Write-Host "Rosen pin: $($Matches[1]) @ $($Matches[2])"