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
# Deterministic CSV writer: Export-Csv in Windows PowerShell 5.1 always quotes
# every field and emits CRLF + BOM, which rewrites all 300+ lines on every run.
# Quote only what RFC 4180 requires, use LF, and write without a BOM so a plain
# `git diff` shows just the cards that actually changed.
function ConvertTo-CsvField {
    param([AllowNull()][string]$Value)
    if ($null -eq $Value) { return '' }
    if ($Value -match '[",\r\n]') {
        return '"' + $Value.Replace('"', '""') + '"'
    }
    return $Value
}

$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('Name,Description,List,Labels,Due Date')
foreach ($card in $data.cards) {
    $checklist = $card.checklists[0]
    # Items are either a plain string or `{name, state}`; both must render as a
    # readable checklist line instead of PowerShell's `@{name=...}` dump.
    $items = $checklist.items | ForEach-Object {
        if ($_ -is [string]) {
            "- [ ] $_"
        }
        else {
            $mark = if ($_.state -eq 'complete') { 'x' } else { ' ' }
            "- [$mark] $($_.name)"
        }
    }
    $body = $card.desc + "`n`nChecklist ($($checklist.name)):`n" + ($items -join "`n")
    $fields = @(
        (ConvertTo-CsvField $card.name),
        (ConvertTo-CsvField $body),
        (ConvertTo-CsvField $card.list),
        (ConvertTo-CsvField ($card.labels -join ', ')),
        ''
    )
    $lines.Add($fields -join ',')
}

$content = ($lines -join "`n") + "`n"
[System.IO.File]::WriteAllText($CsvPath, $content, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "Wrote $($data.cards.Count) cards to $CsvPath"