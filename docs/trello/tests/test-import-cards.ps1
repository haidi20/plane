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

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-False([bool]$Condition, [string]$Message) {
    if ($Condition) { throw $Message }
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

$helperStart = $source.IndexOf('function ConvertTo-ComparisonText(', [System.StringComparison]::Ordinal)
$helperEnd = $source.IndexOf('function Invoke-Trello', $helperStart, [System.StringComparison]::Ordinal)
if ($helperStart -lt 0 -or $helperEnd -lt 0) { throw 'helper definitions not found' }
. ([scriptblock]::Create($source.Substring($helperStart, $helperEnd - $helperStart)))

$legacyHttpSamples = @(
    '`route-parity` hijau: route punya tepat satu owner, tidak ada route dobel atau hilang.'
    '`route-parity` hijau: route punya tepat satu owner, tidak ada route dobel atau hilang. [belum: 389 route Django masih tanpa handler di Rust (route_parity --allow-missing)]'
    'Case nyata ditambahkan ke suite `api-e2e`; tidak menambah unit/service test.'
    'Case nyata ditambahkan ke suite `api-e2e`; tidak menambah unit/service test. [via `cargo test --test error_golden`, HTTP nyata ke server axum]'
    '`wire-parity` hijau terhadap Django pada data nyata (bukan mock/fixture).'
    'PR kecil (< 400 baris diff produk), CI hijau, review selesai, runbook diperbarui.'
    'Semua error path (400/401/403/404/405/413/415/429) diuji nyata.'
)
foreach ($sample in $legacyHttpSamples) {
    Assert-True (Test-LegacyHttpGate $sample) "legacy HTTP gate not detected: $sample"
}
Assert-False (Test-LegacyHttpGate 'Tidak ada route `owner=django`; `route-parity` penuh hijau dan `plane-api` melayani seluruh endpoint.') 'current DECOM acceptance was treated as a legacy HTTP gate'
Assert-False (Test-LegacyHttpGate 'Endpoint current user dan live-access diuji lewat HTTP API boundary dengan cookie user yang sama; tidak ada header internal tambahan yang dilewatkan apps/live.') 'current LIVE acceptance was treated as a legacy HTTP gate'

$legacyLiveSamples = @(
    'Handshake, reconnect, dan konflik diuji dengan apps/live nyata.'
    'Handshake dan reconnect diuji nyata.'
    'Konflik versi ditangani sama.'
    'Tidak ada endpoint live yang terlewat dari daftar.'
    'Tidak ada endpoint live yang terlewat dari inventaris.'
    'Tidak ada endpoint live yang terlewat.'
)
foreach ($sample in $legacyLiveSamples) {
    Assert-True (Test-LegacyLiveGate $sample) "legacy LIVE gate not detected: $sample"
}
Assert-False (Test-LegacyLiveGate 'Handshake/WebSocket/Yjs, reconnect, dan konflik versi adalah scope Fase 9 live-parity dan tidak diklaim selesai oleh kartu ini.') 'current Fase 9 boundary was treated as a legacy LIVE gate'

$legacyProfileSamples = @(
    [pscustomobject]@{ Profile = 'decom'; Item = 'Tidak menghapus file migrasi Django atau lockfile.' }
    [pscustomobject]@{ Profile = 'decom'; Item = 'Tidak menghapus models/settings/migrasi.' }
    [pscustomobject]@{ Profile = 'decom'; Item = 'CI Python non-migrasi dihapus.' }
    [pscustomobject]@{ Profile = 'decom'; Item = 'Job generate-migrations + drift hijau.' }
    [pscustomobject]@{ Profile = 'decom'; Item = '`plane-migrate` adalah satu-satunya runner migrasi production; file migrasi Django historis dibekukan dengan checksum, tetap arsip read-only, dan `schema-parity`/`ledger-parity`/`data-parity` hijau sebelum runtime lama dihapus.' }
    [pscustomobject]@{ Profile = 'spike'; Item = 'Jalankan get_resolver() dan bandingkan jumlah dengan 396+ endpoint; catat selisih.' }
    [pscustomobject]@{ Profile = 'spike'; Item = 'Hitung converter uuid (372) dan str (311); pastikan tidak ada re_path tersembunyi.' }
    [pscustomobject]@{ Profile = 'decision'; Item = 'Keputusan ditandatangani dan dampak ke estimasi/tim diperbarui.' }
    [pscustomobject]@{ Profile = 'decision'; Item = 'Keputusan ditandatangani dan dampak ke estimasi/tim diperbarui; backend production Rust-only (`plane-api`/`plane-worker`/`plane-migrate`), Django/Celery/RabbitMQ dekomisi di Fase 8, dan `apps/live` tetap Node sampai Fase 9.' }
    [pscustomobject]@{ Profile = 'data'; Item = 'Query timeout dan retry terdefinisi.' }
    [pscustomobject]@{ Profile = 'sizing'; Item = 'Mode sandbox tidak mengganggu produksi.' }
)
foreach ($sample in $legacyProfileSamples) {
    Assert-True (Test-LegacyDoD $sample.Profile $sample.Item) "legacy $($sample.Profile) DoD not detected: $($sample.Item)"
}
Assert-False (Test-LegacyDoD 'decom' '`plane-migrate` adalah satu-satunya runner migrasi production; source dan file migrasi Django historis dibekukan dengan checksum, tetap arsip read-only, dan `schema-parity`/`ledger-parity`/`data-parity` hijau sebelum runtime lama dihentikan.') 'current DECOM runtime evidence was treated as stale'
Assert-False (Test-LegacyDoD 'spike' 'Hitung converter uuid (366) dan str (311); pastikan tidak ada re_path tersembunyi.') 'current SPIKE measurement was treated as stale'
Assert-False (Test-LegacyDoD 'decision' 'Keputusan ditandatangani dan dampak ke estimasi/tim diperbarui; backend production Rust-only (`plane-api`/`plane-worker`/`plane-migrate`), runtime Django/Celery/RabbitMQ dihentikan di Fase 8 sementara source `apps/api/**` tetap arsip read-only, dan `apps/live` tetap Node sampai Fase 9.') 'current DECISION acceptance was treated as stale'
Assert-False (Test-LegacyDoD 'data' 'Query timeout dan retry terdefinisi; startup gagal bila `ENABLE_READ_REPLICA=1`.') 'current DATA acceptance was treated as stale'
Assert-False (Test-LegacyDoD 'sizing' 'Sandbox dan produksi dipisah atau dijadwalkan; satu lingkungan berat per host.') 'current SIZING acceptance was treated as stale'
Assert-False (Test-LegacyDoD 'unknown' 'Query timeout dan retry terdefinisi.') 'unknown profile matched a legacy DoD'

# Regression: PowerShell 5.1 defaults Get-Content to ANSI, which turns UTF-8
# sequences like "SS" into "mojibake" and bakes them into the generated CSV.
$exportPath = Join-Path $repoRoot 'docs/trello/export-csv.ps1'
$exportSource = Get-Content -Raw -Encoding UTF8 -LiteralPath $exportPath
if ($exportSource -match "Get-Content -Raw -LiteralPath") { throw "export-csv.ps1 reads JSON without -Encoding UTF8" }
if ($source -match "Get-Content -Raw -LiteralPath") { throw "import-cards.ps1 reads JSON without -Encoding UTF8" }

# The committed CSV must not contain mojibake for characters used in the JSON.
$csvPath = Join-Path $repoRoot 'docs/trello/plane-rust-cards.csv'
$csvText = Get-Content -Raw -Encoding UTF8 -LiteralPath $csvPath
$mojibake = [string]([char]0xC2) + [string]([char]0xA7)
if ($csvText.Contains($mojibake)) { throw "plane-rust-cards.csv contains mojibake; regenerate with export-csv.ps1" }

Write-Host 'PASS import-cards.ps1 contract checks'
