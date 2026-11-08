[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repositoryDirectory = Split-Path -Parent $PSScriptRoot
$proofDirectory = Join-Path $repositoryDirectory 'proofs'

# Reuse the unchanged reviewer's CSS cascade/token reader and its HTML comparison.
. (Join-Path $PSScriptRoot 'pr2-titlebar-reference.ps1')

& lake --dir $proofDirectory build shell_appearance_reference
if ($LASTEXITCODE -ne 0) { throw 'The compiled Lean reference could not be built.' }
$executableName = 'shell_appearance_reference'
if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
    $executableName += '.exe'
}
$referenceExecutable = Join-Path $proofDirectory ".lake/build/bin/$executableName"
$referenceJson = & $referenceExecutable
if ($LASTEXITCODE -ne 0) { throw 'The compiled Lean reference failed.' }
$referenceStyle = ($referenceJson -join "`n") | ConvertFrom-Json
$propertyNames = @($referenceStyle.PSObject.Properties.Name | Sort-Object) -join ','
if ($propertyNames -ne 'background,color,width' -or
    $referenceStyle.background -isnot [string] -or
    $referenceStyle.color -isnot [string] -or
    ($referenceStyle.width -isnot [int] -and $referenceStyle.width -isnot [long])) {
    throw 'The Lean reference must return background/color strings and an integer width.'
}

# The executable fixes the input to Close/hover; the CSS reader selects that same state.
$modelStyle = @{
    background = Resolve-ColorValue $referenceStyle.background
    color = Resolve-ColorValue $referenceStyle.color
    width = [decimal]$referenceStyle.width
}
$applicationStyle = @{
    background = Resolve-ColorValue $actualBackground
    color = Resolve-ColorValue $actualColor
    width = $actualPixels
}
foreach ($property in @('background', 'color', 'width')) {
    Write-Output ('Close/hover {0}: Lean={1}; CSS={2}' -f
        $property, $modelStyle[$property], $applicationStyle[$property])
    if ($modelStyle[$property] -ne $applicationStyle[$property]) {
        throw "SPEC.md F17 reference mismatch for Close/hover property: $property"
    }
}
Write-Output 'Compiled Lean model and application CSS agree on the Close/hover regression input.'
