$ErrorActionPreference = 'Stop'
# Source guard plus a real Windows sharing-violation fixture. No app data is used.
$driver = Get-Content -Raw -LiteralPath 'src-tauri/src/thread_menu.rs'
$hub = Get-Content -Raw -LiteralPath 'crates/velocity-browser/src/hub.rs'
$deletion = [regex]::Match($driver, '(?s)async fn land_thread\b.*?(?=\r?\n(?:pub(?:\([^)]*\))? )?async fn )').Value
if (-not $deletion) { throw 'Could not locate land_thread' }
$discardsFailedCleanup = $deletion -match '(?s)if let Err\(error\)\s*=\s*crate::browser::delete\(application, key, thread\.browser_location\.as_deref\(\)\)\.await\s*\{\s*problems\.push\(format!\(\s*"its browser profile could not be removed \(\{error\}\)"\s*\)\);\s*\}.*?\.remove_deleted_thread\(key\.0, key\.1, None\)'
$retiresBeforeRemoval = $hub -match '(?s)pub async fn delete\b.*?entries\.remove\(&key\).*?Some\(profile\) => remove_profile\(profile\)\.await'
$boundedRemoval = $hub -match '(?s)async fn remove_profile\b.*?0\.\.10.*?attempt == 9 => return Err'
if (-not ($discardsFailedCleanup -and $retiresBeforeRemoval -and $boundedRemoval)) {
    Write-Output 'The original discard-on-failure source path is absent.'
    exit 0
}

$parent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$fixture = [IO.Path]::GetFullPath([IO.Path]::Combine($parent, "velocity-488-cleanup-$([Guid]::NewGuid())"))
if ([IO.Path]::GetDirectoryName($fixture) -ne $parent.TrimEnd('\', '/')) {
    throw 'The temporary profile is outside the intended directory'
}
$marker = [IO.Path]::Combine($fixture, 'profile-marker')
[void][IO.Directory]::CreateDirectory($fixture)
try {
    $lease = [IO.File]::Open($marker, 'CreateNew', 'ReadWrite', 'None')
    try {
        foreach ($attempt in 1..10) {
            try { [IO.Directory]::Delete($fixture, $true) }
            catch [IO.IOException] { }
            if (-not [IO.File]::Exists($marker)) { throw 'The exclusive handle did not retain the marker' }
            if ($attempt -lt 10) { Start-Sleep -Milliseconds 250 }
        }
    } finally {
        $lease.Dispose()
    }
    if (-not [IO.File]::Exists($marker)) { throw 'The profile did not survive the bounded removal window' }
    Write-Output 'A Windows sharing violation kept the fixture profile on disk through ten removal attempts.'
    Write-Output 'FAIL: BrowserHub drops the entry, and land_thread removes the saved browser_location after reporting the deletion failure.'
    exit 1
} finally {
    if ([IO.Directory]::Exists($fixture)) { [IO.Directory]::Delete($fixture, $true) }
}
