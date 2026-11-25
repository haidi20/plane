#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$BoardId,
    [string]$JsonPath,
    [switch]$DryRun,
    [switch]$Prune,
    [switch]$RemoveStaleDoD
)
$ErrorActionPreference = 'Stop'
if (-not $JsonPath) { $JsonPath = Join-Path $PSScriptRoot 'plane-rust-cards.json' }

# Credentials come from the environment first, then from the gitignored
# `.mcp.json` at the repo root (same file the Trello MCP server uses). The
# board id may also live there, so importing needs no extra flags.
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$mcpPath = Join-Path $repoRoot '.mcp.json'
$key = $env:TRELLO_KEY
$token = $env:TRELLO_TOKEN
if (Test-Path -LiteralPath $mcpPath) {
    $mcpEnv = (Get-Content -Raw -LiteralPath $mcpPath | ConvertFrom-Json).mcpServers.trello.env
    if (-not $key) { $key = $mcpEnv.TRELLO_API_KEY }
    if (-not $token) { $token = $mcpEnv.TRELLO_TOKEN }
    if (-not $BoardId -and $mcpEnv.TRELLO_BOARD_ID) { $BoardId = $mcpEnv.TRELLO_BOARD_ID }
}
if (-not $key -or -not $token) {
    throw 'Isi TRELLO_KEY/TRELLO_TOKEN di env atau .mcp.json (lihat .mcp.json.example). Jangan simpan token di file yang di-track git.'
}
if (-not $BoardId) { throw 'BoardId kosong: kirim -BoardId atau isi TRELLO_BOARD_ID di .mcp.json.' }
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

$legacyHttpDoD = @(
    'Kontrak endpoint tercatat di `contracts/` (method, path, request, response, status, header).'
    '`route-owner` hijau: setiap endpoint punya tepat satu owner, status jujur, dan tidak ada route `rust` yang belum dilayani.'
    '`wire-parity` hijau terhadap Django pada data nyata (bukan mock/fixture).'
    'Case nyata ditambahkan ke suite `api-e2e`; logika domain internal tetap punya unit test.'
)
$legacyLiveDoD = @(
    'Handshake, reconnect, dan konflik diuji dengan apps/live nyata.'
    'Handshake dan reconnect diuji nyata.'
    'Konflik versi ditangani sama.'
    'Tidak ada endpoint live yang terlewat dari daftar.'
    'Tidak ada endpoint live yang terlewat.'
)
$httpProfiles = @('http', 'http-write', 'http-live')
$validProfiles = @('http', 'http-write', 'http-live', 'data', 'spike', 'decision', 'infra', 'dev', 'migration', 'job', 'mvp', 'cutover', 'sizing', 'decom', 'ops')
$legacySet = @{}
foreach ($legacy in $legacyHttpDoD) { $legacySet[$legacy] = $true }
$legacyLiveSet = @{}
foreach ($legacy in $legacyLiveDoD) { $legacyLiveSet[$legacy] = $true }
foreach ($card in $data.cards) {
    if (-not $card.profile -or $validProfiles -notcontains $card.profile) {
        throw "Card has no valid acceptance profile: $($card.name)"
    }
    if ($httpProfiles -notcontains $card.profile) {
        $names = @($card.checklists[0].items | ForEach-Object { if ($_ -is [string]) { $_ } else { $_.name } })
        if ($names | Where-Object { $legacySet.ContainsKey($_) }) {
            throw "Non-HTTP card still contains an HTTP gate: $($card.name)"
        }
    }
    if ($card.profile -eq 'http-live') {
        $names = @($card.checklists[0].items | ForEach-Object { if ($_ -is [string]) { $_ } else { $_.name } })
        if ($names | Where-Object { $legacyLiveSet.ContainsKey($_) }) {
            throw "Live HTTP card still contains a WebSocket/Yjs gate: $($card.name)"
        }
    }
}
# `-BoardId` accepts the short link from the board URL, but POST /labels needs
# the 24-char board id. Resolve once and use the id for every board-scoped call.
$board = Invoke-Trello GET "/boards/$BoardId" @{ fields = 'id,name' }
$boardId = $board.id
Write-Host "Board id: $boardId ($($board.name))"

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
                $created = Invoke-Trello POST "/boards/$boardId/labels" @{ name = $l.name; color = $l.color }
                $labels[$l.name] = $created.id
                Write-Host "label + $($l.name)"
            }
            catch {
                Write-Warning "label skipped: $($l.name) ($($_.Exception.Message))"
            }
        }
    }
}

$boardCards = Invoke-Trello GET "/boards/$BoardId/cards" @{ fields = 'name,desc,idList,idLabels'; limit = '1000' }
$existing = @{}
foreach ($c in $boardCards) { $existing[$c.name] = [pscustomobject]@{ id = $c.id; desc = $c.desc; idList = $c.idList; labelIds = @($c.idLabels) } }

# A renamed card is matched by its `aliases` entry, renamed in place, and then
# re-homed, so renaming the JSON never creates a duplicate card on the board.
$renamedCount = 0
foreach ($card in $data.cards) {
    if ($existing.ContainsKey($card.name)) { continue }
    $alias = @($card.aliases) | Where-Object { $_ -and $existing.ContainsKey($_) } | Select-Object -First 1
    if (-not $alias) { continue }
    if ($DryRun) {
        Write-Host "[dry] card rename: $alias -> $($card.name)"
        $existing[$card.name] = $existing[$alias]
        $renamedCount++
        continue
    }
    $aliasId = $existing[$alias].id
    Invoke-Trello PUT "/cards/$aliasId" @{ name = $card.name } | Out-Null
    $existing[$card.name] = [pscustomobject]@{ id = $aliasId; desc = $existing[$alias].desc; idList = $existing[$alias].idList; labelIds = @($existing[$alias].labelIds) }
    $existing.Remove($alias)
    $renamedCount++
    Write-Host "card rename: $alias -> $($card.name)"
    Start-Sleep -Milliseconds 120
}

$createdCount = 0; $skipped = 0; $moved = 0; $labelled = 0; $itemCount = 0; $itemStateCount = 0
$descUpdated = 0; $extraItemCount = 0; $extraCardCount = 0; $staleDoDRemoved = 0
$jsonCardNames = @{}
foreach ($card in $data.cards) { $jsonCardNames[$card.name] = $true }
foreach ($card in $data.cards) {
    if ($existing.ContainsKey($card.name)) {
        $cardId = $existing[$card.name].id
        $skipped++

        # JSON descriptions are authoritative. Without this, a card keeps an
        # old checklist summary forever even after the source-of-truth changes.
        if ($existing[$card.name].desc -ne $card.desc) {
            $descUpdated++
            if ($DryRun) { Write-Host "[dry] card desc update: $($card.name)" }
            else {
                Invoke-Trello PUT "/cards/$cardId" @{ desc = $card.desc } | Out-Null
                Write-Host "card desc update: $($card.name)"
                Start-Sleep -Milliseconds 120
            }
        }

        # A card that already exists is re-homed to the list this JSON says it
        # belongs to (for example `progress`), so board state survives re-import.
        $targetList = $lists[$card.list]
        if ($targetList -and $existing[$card.name].idList -ne $targetList) {
            if ($DryRun) { Write-Host "[dry] card -> $($card.list): $($card.name)" }
            else {
                Invoke-Trello PUT "/cards/$cardId" @{ idList = $targetList } | Out-Null
                $moved++
                Write-Host "card -> $($card.list): $($card.name)"
                Start-Sleep -Milliseconds 120
            }
        }

        # Labels are idempotent too: add what is missing, never remove what a
        # human added by hand on the board.
        $currentIds = @($existing[$card.name].labelIds)
        $wantedIds = @()
        foreach ($ln in $card.labels) { if ($labels.ContainsKey($ln) -and $currentIds -notcontains $labels[$ln]) { $wantedIds += $labels[$ln] } }
        if ($wantedIds.Count -gt 0) {
            if ($DryRun) { Write-Host "[dry] card labels +$($wantedIds.Count): $($card.name)" }
            else {
                Invoke-Trello PUT "/cards/$cardId" @{ idLabels = (($currentIds + $wantedIds) -join ',') } | Out-Null
                $labelled++
                Write-Host "card labels +$($wantedIds.Count): $($card.name)"
                Start-Sleep -Milliseconds 120
            }
        }
    }
    else {
        if ($DryRun) {
            Write-Host "[dry] card + $($card.name)"
            foreach ($checklist in $card.checklists) {
                Write-Host "[dry] checklist +$($checklist.items.Count): $($card.name) / $($checklist.name)"
                $itemCount += $checklist.items.Count
            }
            continue
        }
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

    # Existing cards and live imports have an id. A brand-new dry-run card was
    # already reported above and never reaches this fetch.
    $cl = Invoke-Trello GET "/cards/$cardId/checklists" @{ fields = 'name' }
    foreach ($checklist in $card.checklists) {
        $found = $cl | Where-Object { $_.name -eq $checklist.name } | Select-Object -First 1
        if (-not $found) {
            if ($DryRun) {
                Write-Host "[dry] checklist new: $($card.name) / $($checklist.name)"
                foreach ($entry in $checklist.items) {
                    $name = if ($entry -is [string]) { $entry } else { $entry.name }
                    Write-Host "[dry] checkitem +: $name"
                    $itemCount++
                }
                continue
            }
            $found = Invoke-Trello POST "/cards/$cardId/checklists" @{ name = $checklist.name; pos = 'bottom' }
        }
        # Checklist items carry an optional `state` ("complete"/"incomplete").
        # A plain string means "item must exist" and leaves any manual tick
        # alone; an object with `state` is authoritative, so the board matches
        # the JSON after every import.
        $usedItemIds = @{}
        $liveItems = @()
        if ($found.id) {
            # PowerShell 5.1 wraps a JSON array returned by a function into a
            # single object when it is collected with @() at the call site, so
            # collect through a variable instead.
            $fetchedItems = Invoke-Trello GET "/checklists/$($found.id)/checkItems" @{ fields = 'name,state' }
            $liveItems = @($fetchedItems)
        }
        foreach ($entry in $checklist.items) {
            if ($entry -is [string]) {
                $item = $entry
                $state = $null
            }
            else {
                $item = $entry.name
                $state = $entry.state
            }
            $existingItem = $liveItems | Where-Object { $_.name -eq $item -and -not $usedItemIds.ContainsKey($_.id) } | Select-Object -First 1
            if (-not $existingItem) {
                $body = @{ name = $item; pos = 'bottom' }
                if ($state) { $body.state = $state }
                if ($DryRun) { Write-Host "[dry] checkitem + ${state}: $item" }
                else {
                    $created = Invoke-Trello POST "/checklists/$($found.id)/checkItems" $body
                    $liveItems += $created
                    $existingItem = $created
                    Write-Host "checkitem + ${state}: $item"
                }
                $itemCount++
            }
            if ($existingItem.id) { $usedItemIds[$existingItem.id] = $true }
            if ($existingItem -and $state -and $existingItem.state -ne $state) {
                if ($DryRun) { Write-Host "[dry] checkitem ${state}: $item" }
                else {
                    Invoke-Trello PUT "/cards/$cardId/checkItem/$($existingItem.id)" @{ state = $state } | Out-Null
                    $itemStateCount++
                    Write-Host "checkitem $state`: $item"
                }
            }
        }
        $unmatchedItems = @($liveItems | Where-Object { -not $usedItemIds.ContainsKey($_.id) })
        foreach ($entry in $unmatchedItems) {
            $isStaleDoD = $RemoveStaleDoD -and $httpProfiles -notcontains $card.profile -and $legacySet.ContainsKey($entry.name)
            $isStaleLiveDoD = $RemoveStaleDoD -and $card.profile -eq 'http-live' -and $legacyLiveSet.ContainsKey($entry.name)
            if (-not $isStaleDoD -and -not $isStaleLiveDoD) {
                $extraItemCount++
                Write-Host "[extra] live checkitem not in JSON: $($card.name) / $($checklist.name) / $($entry.name)"
            }
            if ($Prune -or $isStaleDoD -or $isStaleLiveDoD) {
                if ($found.id) {
                    if ($DryRun) {
                        $kind = if ($isStaleDoD -or $isStaleLiveDoD) { 'stale DoD ' } else { '' }
                        Write-Host "[dry] DELETE ${kind}/checkItems/$($entry.id): $($entry.name)"
                        if ($isStaleDoD -or $isStaleLiveDoD) { $staleDoDRemoved += 1 }
                    }
                    else {
                        Invoke-Trello DELETE "/cards/$cardId/checkItem/$($entry.id)" | Out-Null
                        Write-Host "checkitem pruned: $($entry.name)"
                        if ($isStaleDoD -or $isStaleLiveDoD) { $staleDoDRemoved += 1 }
                        Start-Sleep -Milliseconds 120
                    }
                }
            }
        }
        Start-Sleep -Milliseconds 130
    }
}

$extraCards = @($boardCards | Where-Object { -not $jsonCardNames.ContainsKey($_.name) })
$extraCardCount = $extraCards.Count
foreach ($extra in $extraCards) {
    Write-Host "[extra] live card not in JSON: $($extra.name) [$($extra.idList)]"
}

Write-Host "Selesai: created=$createdCount renamed=$renamedCount skipped=$skipped moved=$moved labelled=$labelled descUpdated=$descUpdated newCheckItems=$itemCount checkItemStates=$itemStateCount staleDoDRemoved=$staleDoDRemoved extraCheckItems=$extraItemCount extraCards=$extraCardCount"
