#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$JsonPath,
    [string]$CsvPath
)
$ErrorActionPreference = 'Stop'
if (-not $JsonPath) { $JsonPath = Join-Path $PSScriptRoot 'plane-rust-cards.json' }
if (-not $CsvPath) { $CsvPath = Join-Path $PSScriptRoot 'plane-rust-cards.csv' }
$data = Get-Content -Raw -LiteralPath $JsonPath | ConvertFrom-Json
$rows = foreach ($card in $data.cards) {
    $checklist = $card.checklists[0]
    $body = $card.desc + "`n`nChecklist ($($checklist.name)):`n" +
        (($checklist.items | ForEach-Object { "- [ ] $_" }) -join "`n")
    [pscustomobject]@{
        Name        = $card.name
        Description = $body
        List        = $card.list
        Labels      = ($card.labels -join ', ')
        'Due Date'  = ''
    }
}
$rows | Export-Csv -LiteralPath $CsvPath -NoTypeInformation -Encoding UTF8
Write-Host "Wrote $($rows.Count) cards to $CsvPath"