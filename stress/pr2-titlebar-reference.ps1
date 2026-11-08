[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repositoryDirectory = Split-Path -Parent $PSScriptRoot
$reference = Get-Content -Raw (Join-Path $repositoryDirectory 'design/main-window.html')
$stylesheet = Get-Content -Raw (Join-Path $repositoryDirectory 'ui/src/lib/theme/shell.css')
$themeDirectory = Join-Path $repositoryDirectory 'ui/src/lib/theme'
# Include semantic tokens if #12 authorizes a supplemental theme file.
$tokens = (Get-ChildItem -LiteralPath $themeDirectory -Filter '*.css' -File |
    Sort-Object Name | ForEach-Object { Get-Content -Raw -LiteralPath $_.FullName }) -join "`n"
$pixelTokens = @{}
$rawTokens = @{}
foreach ($token in [regex]::Matches($tokens, '(--[\w-]+)\s*:\s*([^;]+);')) {
    $rawTokens[$token.Groups[1].Value] = $token.Groups[2].Value.Trim()
}
foreach ($token in [regex]::Matches($tokens, '(--[\w-]+)\s*:\s*([\d.]+)px')) {
    $pixelTokens[$token.Groups[1].Value] = [decimal]$token.Groups[2].Value
}

function Resolve-PixelValue {
    param([string] $Value)
    $resolved = [regex]::Replace($Value, 'var\((--[\w-]+)\)', {
        param($match)
        if (-not $pixelTokens.ContainsKey($match.Groups[1].Value)) {
            throw "Unknown pixel token: $($match.Groups[1].Value)"
        }
        return "$($pixelTokens[$match.Groups[1].Value])px"
    })
    if ($resolved -match '^\s*([\d.]+)px\s*$') { return [decimal]$Matches[1] }
    if ($resolved -match '^calc\(\s*([\d.]+)px\s*\+\s*([\d.]+)px\s*\)$') {
        return [decimal]$Matches[1] + [decimal]$Matches[2]
    }
    throw "Unsupported pixel expression: $resolved"
}

function Get-LastProperty {
    param([string] $Stylesheet, [string] $Selector, [string] $Property)
    $value = $null
    foreach ($rule in [regex]::Matches($Stylesheet, [regex]::Escape($Selector) + '\s*\{([^}]+)\}')) {
        foreach ($declaration in [regex]::Matches($rule.Groups[1].Value, '(?:^|;)\s*' + $Property + '\s*:\s*([^;]+)')) {
            $value = $declaration.Groups[1].Value.Trim()
        }
    }
    return $value
}

function Get-CloseHoverProperty {
    param([string] $Stylesheet, [string] $GenericSelector, [string] $CloseSelector, [string] $Property)
    # The close selector has higher specificity; a later generic rule cannot override it.
    $specificValue = Get-LastProperty $Stylesheet $CloseSelector $Property
    if ($null -ne $specificValue) { return $specificValue }
    return Get-LastProperty $Stylesheet $GenericSelector $Property
}

function Resolve-ColorValue {
    param([string] $Value)
    for ($depth = 0; $depth -lt 16 -and $Value -match '^var\((--[\w-]+)\)$'; $depth++) {
        $tokenName = $Matches[1]
        if (-not $rawTokens.ContainsKey($tokenName)) { throw "Unknown color token: $tokenName" }
        $Value = $rawTokens[$tokenName]
    }
    $normalized = [regex]::Replace($Value.ToLowerInvariant(), '\s+', '')
    if ($normalized -match '^#([0-9a-f])([0-9a-f])([0-9a-f])$') {
        return '#' + $Matches[1] + $Matches[1] + $Matches[2] + $Matches[2] + $Matches[3] + $Matches[3]
    }
    return $normalized
}

$referenceWidth = Get-LastProperty $reference '.win-controls button' 'width'
$actualWidth = Get-LastProperty $stylesheet '.window-controls button' 'width'
$expectedPixels = Resolve-PixelValue $referenceWidth
$actualPixels = Resolve-PixelValue $actualWidth
Write-Output "Reference control width: $expectedPixels px"
Write-Output "Application control width: $actualPixels px ($actualWidth)"
if ($actualPixels -ne $expectedPixels) {
    throw 'SPEC.md F1 reference mismatch: every native title-bar control has the wrong width.'
}
Write-Output 'Control width matches the final reference override.'

$expectedBackground = Get-CloseHoverProperty $reference '.win-controls button:hover' '.win-controls button.close:hover' 'background'
$actualBackground = Get-CloseHoverProperty $stylesheet '.window-controls button:hover' '.window-controls .close:hover' 'background'
$expectedColor = Get-CloseHoverProperty $reference '.win-controls button:hover' '.win-controls button.close:hover' 'color'
$actualColor = Get-CloseHoverProperty $stylesheet '.window-controls button:hover' '.window-controls .close:hover' 'color'
Write-Output "Reference Close hover: background=$expectedBackground; color=$expectedColor"
Write-Output "Application Close hover: background=$actualBackground; color=$actualColor"
if ((Resolve-ColorValue $expectedBackground) -ne (Resolve-ColorValue $actualBackground) -or
    (Resolve-ColorValue $expectedColor) -ne (Resolve-ColorValue $actualColor)) {
    throw 'SPEC.md F1 reference mismatch: Close hover background and glyph colors differ.'
}
