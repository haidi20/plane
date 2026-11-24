#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$BoardId,
    [string]$JsonPath,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
if (-not $JsonPath) { $JsonPath = Join-Path $PSScriptRoot 'plane-rust-cards.json' }
$key = $env:TRELLO_KEY
$token = $env:TRELLO_TOKEN
if (-not $key -or -not $token) {
    throw 'Set TRELLO_KEY dan TRELLO_TOKEN lebih dulu. Jangan simpan token di repo.'
}
$Base = 'https://api.trello.com/1'

function Invoke-Trello {
    param([string]$Method, [string]$Path, [hashtable]$Params = @{})
    $pairs = @("key=$([uri]::EscapeDataString($key))", "token=$([uri]::EscapeDataString($token))")
    foreach ($k in $Params.Keys) {
        $pairs += "$([uri]::EscapeDataString($k))=$([uri]::EscapeDataString([string]$Params[$k]))"
    }
    $uri = "$Base$Path`?" + ($pairs -join '&')
    for ($attempt = 1; $attempt -le 4; $attempt++) {
        try {
            return Invoke-RestMethod -Method $Method -Uri $uri
        }
        catch {
            $code = $_.Exception.Response.StatusCode.value__
            if ($code -eq 429 -and $attempt -lt 4) {
                Start-Sleep -Seconds (2 * $attempt)
                continue
            }
            throw
        }
    }
}

$data = Get-Content -Raw -LiteralPath $JsonPath | ConvertFrom-Json
Write-Host "Board: $($data.board) | cards: $($data.cards.Count) | lists: $($data.lists.Count)"

$lists = @{}
foreach ($l in (Invoke-Trello GET "/boards/$BoardId/lists" @{ fields = 'name' })) { $lists[$l.name] = $l.id }
foreach ($name in $data.lists) {
    if (-not $lists.ContainsKey($name)) {
        if ($DryRun) { Write-Host "[dry] list + $name" }
        else {
            $created = Invoke-Trello POST "/boards/$BoardId/lists" @{ name = $name; pos = 'bottom' }
            $lists[$name] = $created.id
            Write-Host "list + $name"
        }
    }
}

$labels = @{}
$boardLabels = Invoke-Trello GET "/boards/$BoardId/labels" @{ fields = 'name,color'; limit = '1000' }
foreach ($l in $boardLabels) { if ($l.name) { $labels[$l.name] = $l.id } }
foreach ($l in $data.labels) {
    if (-not $labels.ContainsKey($l.name)) {
        if ($DryRun) { Write-Host "[dry] label + $($l.name)" }
        else {
            try {
                $created = Invoke-Trello POST '/labels' @{ idBoard = $BoardId; name = $l.name; color = $l.color }
                $labels[$l.name] = $created.id
                Write-Host "label + $($l.name)"
            }
            catch {
                Write-Warning "label skipped: $($l.name) ($($_.Exception.Message))"
            }
        }
    }
}

$existing = @{}
foreach ($c in (Invoke-Trello GET "/boards/$BoardId/cards" @{ fields = 'name'; limit = '1000' })) { $existing[$c.name] = $c.id }

$createdCount = 0; $skipped = 0; $itemCount = 0
foreach ($card in $data.cards) {
    if ($existing.ContainsKey($card.name)) {
        $cardId = $existing[$card.name]
        $skipped++
    }
    else {
        if ($DryRun) { Write-Host "[dry] card + $($card.name)"; continue }
        $ids = @()
        foreach ($ln in $card.labels) { if ($labels.ContainsKey($ln)) { $ids += $labels[$ln] } }
        $body = @{ idList = $lists[$card.list]; name = $card.name; desc = $card.desc; pos = 'bottom' }
        if ($ids.Count -gt 0) { $body.idLabels = ($ids -join ',') }
        $new = Invoke-Trello POST '/cards' $body
        $cardId = $new.id
        $createdCount++
        Write-Host "card + $($card.name)"
        Start-Sleep -Milliseconds 120
    }

    if ($DryRun) { continue }

    $cl = Invoke-Trello GET "/cards/$cardId/checklists" @{ fields = 'name' }
    foreach ($checklist in $card.checklists) {
        $found = $cl | Where-Object { $_.name -eq $checklist.name } | Select-Object -First 1
        if (-not $found) {
            $found = Invoke-Trello POST "/cards/$cardId/checklists" @{ name = $checklist.name; pos = 'bottom' }
        }
        $existingItems = @()
        if ($found.id) {
            $items = Invoke-Trello GET "/checklists/$($found.id)/checkItems" @{ fields = 'name' }
            $existingItems = @($items | ForEach-Object { $_.name })
        }
        foreach ($item in $checklist.items) {
            if ($existingItems -notcontains $item) {
                Invoke-Trello POST "/checklists/$($found.id)/checkItems" @{ name = $item; pos = 'bottom' } | Out-Null
                $itemCount++
            }
        }
        Start-Sleep -Milliseconds 130
    }
}
Write-Host "Selesai: created=$createdCount skipped=$skipped newCheckItems=$itemCount"