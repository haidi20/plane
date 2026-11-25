#Requires -Version 5.1
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))
$scriptPath = Join-Path $repoRoot 'docs/trello/import-cards.ps1'
$source = Get-Content -Raw -LiteralPath $scriptPath

function Assert-Contains([string]$Pattern) {
    if ($source -notmatch $Pattern) { throw "expected pattern: $Pattern" }
}

function Assert-NotContains([string]$Pattern) {
    if ($source -match $Pattern) { throw "unexpected pattern: $Pattern" }
}

Assert-Contains '\[switch\]\$Prune'
Assert-Contains '\[switch\]\$RemoveStaleDoD'
Assert-Contains 'extraCheckItems'
Assert-Contains 'DELETE "/cards/\$cardId/checkItem/\$\(\$entry\.id\)"'
Assert-NotContains 'DELETE "/checkItems/'
Assert-Contains 'live checkitem not in JSON'
Assert-Contains 'if \(\$Prune -or \$isStaleDoD -or \$isStaleLiveDoD\)'
Assert-Contains '\$Prune'
Assert-Contains 'extraCheckItems='
Assert-Contains 'unmatchedItems'
Assert-Contains 'usedItemIds'
Assert-Contains 'staleDoDRemoved'
Assert-Contains 'legacyLiveDoD'
Assert-Contains '\$httpProfiles -notcontains \$card\.profile'
Assert-Contains '\$card\.profile -eq ''http-live'''
Assert-Contains '\$staleDoDRemoved \+= 1'
Assert-NotContains '\$existingItems\['

Write-Host 'PASS import-cards.ps1 contract checks'
