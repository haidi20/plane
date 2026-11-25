# Migrasi Backend Plane: Django -> Rust (Full BE)

> **Konteks produk**: Plane adalah aplikasi **project management** — workspace, project, work item/issue, cycle, module, state, label, page, dan intake. Istilah "project" di dokumen ini selalu berarti project Plane; "proyek migrasi" berarti penggantian backend Django dengan Rust.
> Status: **spike Fase 0 berjalan** (baseline kontrak dan sebagian kecil handler Rust sudah ada; belum ada cutover).
> **End state:** backend produksi **hanya Rust** (`plane-api` + `plane-worker` + `plane-migrate` setelah Fase 8, lalu `plane-live` setelah cutover Fase 9 selesai). Tidak boleh ada service Django, Celery, RabbitMQ, atau server live Node/Hocuspocus yang berjalan di produksi. Django (sampai Fase 8) dan `apps/live` Node (sampai cutover Fase 9 selesai) hanya berfungsi sebagai oracle paritas/rollback; keduanya bukan backend cadangan permanen. Source `apps/api/**` dan `apps/live/**` tetap disimpan sebagai arsip read-only, tetapi tidak dibangun, tidak dijalankan, dan tidak menerima perubahan produk baru setelah runtime-nya dihentikan.
> Target: seluruh backend `apps/api` (Django 5.2 + DRF) diganti implementasi Rust, **tanpa** mengubah frontend (`apps/web`, `apps/admin`, `apps/space`) dan **tanpa** mengubah skema database yang sudah ada.
> Dokumen ini adalah baseline kerja: ruang lingkup, arsitektur target, peta modul, kontrak yang wajib identik, rencana fase, cutover, risiko, dan estimasi.

---

## 0. Batasan Keras (tidak dinegosiasikan)

1. **Baseline migrasi dicerminkan sekali, lalu dimiliki Rust.** Baseline historis mencakup 125 migrasi app `db` (`apps/api/plane/db/migrations`), 6 migrasi app `license`, dan migrasi app pihak ketiga yang ikut terpasang (`django.contrib.auth`, `django.contrib.contenttypes`, `django.contrib.sessions`, `django_celery_beat`). Setelah `schema-parity`, `ledger-parity`, dan `data-parity` membuktikan manifest/SQL Rust identik, ownership migrasi baru pindah ke Rust. Django tidak lagi menjadi source of truth runtime, dan migrasi historis tidak boleh digabung, di-squash, dihapus, atau diubah tanpa proses audit.
2. **Verifikasi = selisih skema nol.** Dua database kosong: satu dimigrasi Django, satu dimigrasi Rust. `pg_dump --schema-only` (dinormalisasi) harus **identik**. Ini gate utama, bukan asumsi.
3. **Database tetap PostgreSQL.** Sama seperti sekarang (`postgres:15.7-alpine`), memakai `JSONB`, `ILIKE`, `= ANY($1)`, `DISTINCT ON`, window function, partial index. Tidak ada SQLite/MySQL.
4. **Ledger migrasi tetap `django_migrations`.** Rust migration runner menulis baris `(app, name, applied)` yang sama persis, sehingga Django dan Rust bisa bergantian menjalankan/inspeksi migrasi tanpa re-apply atau "hilang riwayat". Tidak ada tabel ledger kedua.
5. **Data migration wajib diport.** Ada **60 `RunPython`** di migrasi Plane (0 `RunSQL`, 0 `SeparateDatabaseAndState`). Semuanya harus punya padanan Rust yang menghasilkan efek data identik pada posisi urutan yang sama — tidak boleh di-skip.
6. **Komentar wajib di sisi Rust.** Setiap file migrasi Rust wajib punya header (sumber Django, tujuan, tabel/kolom terdampak, reversibilitas) dan komentar inline per statement. Bila file Django sudah berkomentar, salin/gabungkan; bila belum, tulis penjelasan baru di Rust. **File Django tidak diedit** untuk menambah komentar.
7. **File Django tidak disentuh selama transisi.** `apps/api/plane/db/migrations/**`, `apps/api/plane/license/migrations/**`, dan `apps/api/migrations_lockfile.txt` tidak diedit. Setelah baseline dibekukan, file tersebut hanya menjadi referensi/arsip read-only atau input generator offline; bukan runtime dan bukan tempat menambah migrasi baru.
8. **Tidak ada DDL di luar folder migrasi.** Handler, repository, job, dan tool tidak boleh memuat `CREATE/ALTER/DROP/TRUNCATE` maupun `sqlx migrate`. Satu-satunya jalur perubahan skema adalah file migrasi hasil generate.
9. **URL surface beku 100%.** Path, method, urutan segmen, trailing slash, redirect, query key, header, dan nama field body tidak berubah. Tidak ada prefix baru (`/api/v2`, `/internal`, `/rust`), tidak ada rename "gaya Rust", dan tidak ada endpoint yang dihapus walau terlihat tidak terpakai. Router Rust adalah cermin `urls.py`; gate `route-owner` wajib hijau sejak Fase 0 sebagai checker lokal, sedangkan `route-parity` penuh wajib hijau sebelum Fase 8.
10. **Request dan response identik 100%.** Status, header (termasuk `Content-Type`, `Set-Cookie`, CORS, rate limit, security header), key set, urutan field, tipe nilai, `null` vs hilang vs `""`, format angka/waktu, pesan error, dan body non-JSON harus sama. Hanya nilai volatil (ID/timestamp/token/signature) dan byte kompresi yang dinormalisasi harness; lihat 8.7.
11. **Zero perubahan kode frontend.** `apps/web`, `apps/admin`, `apps/space` (dan `apps/live`) tidak boleh punya diff: tidak ada penyesuaian URL builder, tipe response, store, env, dependency, atau test produk. Jika FE perlu diubah agar jalan, itu bug paritas backend dan wajib diperbaiki di Rust (gate `fe-unchanged`, lihat 12.8).
12. **Bukan multi-tenant.** Plane adalah satu deployment, satu database, satu schema. Isolasi data memakai `workspace_id` (FK) + permission check, bukan schema/DB per pelanggan. Rust tidak boleh menambah tenant resolver, `search_path` switching, tabel `tenant`, atau database per tenant. Istilah "tenant" pada kode/test lama berarti workspace.
13. **Tidak ada fallback permanen ke Django.** Setelah Fase 8, tidak boleh ada route dengan `owner=django`, service Django, Celery worker/beat, atau RabbitMQ. Kegagalan `plane-api` tidak boleh diselamatkan dengan menghidupkan kembali Django; rollback hanya ke image Rust sebelumnya atau, selama jendela cutover yang disetujui, ke rilis Django yang masih dipertahankan.
14. **Source code Django tidak dihapus.** Setelah Fase 8, `apps/api/**` tetap berada di repository sebagai arsip read-only. Yang didekomisi adalah runtime, service/container, image produksi, CI permanen, dan dependency Python. Source Django tidak dibangun, tidak dijalankan, tidak menjadi fallback, dan tidak menerima migrasi baru. Penghapusan source membutuhkan permintaan eksplisit dan PR terpisah.

---

## 1. Ringkasan Eksekutif

Mengganti Django dengan Rust bukan "menulis ulang CRUD". Yang mahal justru **paritas perilaku**: format error, pagination cursor, format waktu, sesi login, urutan query, soft-delete, efek samping berupa background job, dan integrasi realtime (`apps/live`).

Kesimpulan perencanaan:

1. **Skema tetap sama, di PostgreSQL.** Rust memiliki baseline migrasi cermin 1:1 dari Django (125 `plane.db` + 6 `plane.license` + migrasi app pihak ketiga), dan `pg_dump` harus identik. DB dipakai bersama hanya selama transisi agar cutover tidak memindahkan data; setelah Fase 8, `plane-migrate` adalah satu-satunya runner migrasi produksi dan Rust memiliki migrasi baru.
2. **Kontrak HTTP dan URL dibekukan.** Frontend, `apps/live`, API client, dan consumer webhook tidak boleh perlu mengubah satu baris pun: path, method, trailing slash, status 404/405, query key, header, **request body, dan response body** harus identik. Setiap perubahan perilaku harus lolos rekaman `wire-parity` dari API Django nyata (lihat 8.0 dan 8.7). Tidak boleh ada PR di `apps/web`, `apps/admin`, atau `apps/space`.
3. **Migrasi bertahap per modul**, bukan big-bang: read-only dulu, lalu write, lalu job, lalu auth, terakhir admin/public.
4. **`apps/live` dimigrasi ke Rust, bukan ditinggalkan di Node.** Selama fase 0-8 dan jendela cutover Fase 9, server Node/Hocuspocus tetap menjadi oracle/rollback dan source-nya tidak boleh diubah sampai cutover selesai; Rust menyediakan endpoint yang dipanggilnya. Fase 9 menggantinya dengan binary `plane-live` Rust yang memakai protokol WebSocket/Yjs identik. Setelah cutover Fase 9 selesai, tidak ada runtime atau container Node untuk server live. Source `apps/live/**` tetap arsip read-only. Frontend SSR `apps/space` adalah runtime Node terpisah dan tidak termasuk klaim "nol Node" ini.
5. Perkiraan realistis: **6-8 bulan untuk 4-5 engineer** (detail di 16), dengan risiko terbesar di auth/session, filter DSL, activity/versioning, dan notification pipeline.

---

## 2. Baseline: Apa yang Ada Sekarang

Diukur langsung dari repo ini (`apps/api`, tanpa folder `migrations`):

| Metrik                         | Nilai                                                                                                                |
| ------------------------------ | -------------------------------------------------------------------------------------------------------------------- |
| File Python                    | 651                                                                                                                  |
| Baris Python (non-migration)   | 87.257                                                                                                               |
| File model / jumlah model      | 31 file, ±80 model                                                                                                   |
| Migrasi Django (sumber cermin) | 125 app `db` + 6 app `license`                                                                                       |
| `RunPython` (data migration)   | 60                                                                                                                   |
| `AddIndex` / `AddConstraint`   | 26 / 48                                                                                                              |
| File view / kelas view         | 120 file, 218 kelas                                                                                                  |
| File serializer                | 64                                                                                                                   |
| Endpoint (semua pintu)         | 722 (391 route; `/api` 442, `/api/v1` 178, `/api/public` 42, `/api/instances` 21, `/auth` 37, `/` + `/robots.txt` 2) |
| Endpoint `/api/public` (space) | 42 (25 route)                                                                                                        |
| Endpoint `/auth`               | 37 (37 route)                                                                                                        |
| Modul background task          | 31                                                                                                                   |
| Jadwal Celery Beat             | 13                                                                                                                   |
| File test                      | 176                                                                                                                  |

### 2.1 Permukaan API (6 pintu)

| Prefix                | Router                      | Auth                               | Dipakai oleh                      |
| --------------------- | --------------------------- | ---------------------------------- | --------------------------------- |
| `/api/`               | `plane/app/urls`            | Session cookie                     | Web app, live server              |
| `/api/v1/`            | `plane/api/urls`            | API key (`X-Api-Key`)              | Public API / integrasi            |
| `/api/public/`        | `plane/space/urls`          | `AllowAny` + deploy-board anchor   | Space publik & form intake        |
| `/api/instances/`     | `plane/license/urls`        | Admin session (`admin-session-id`) | God mode / admin                  |
| `/auth/`              | `plane/authentication/urls` | Campuran                           | Login, OAuth, magic code, signout |
| `/` dan `/robots.txt` | `plane/web/urls`            | –                                  | Health check `{"status":"OK"}`    |

Endpoint `/api/schema/` (drf-spectacular) aktif bila `ENABLE_DRF_SPECTACULAR` → **ini aset utama** untuk peta route dan pembanding kontrak.

### 2.2 Pembagian modul Django

```
plane/api      -> REST v1 (API key), 14 file urls
plane/app      -> REST v2/web (session), 21 file urls
plane/space    -> public API (publish board, intake)
plane/web      -> health + robots
plane/authentication -> session, oauth, credential, magic code
plane/license  -> instance admin, telemetry
plane/bgtasks  -> Celery tasks (31 modul)
plane/db       -> models + manager/mixin (soft delete, audit)
plane/utils    -> filter DSL, exporter/importer, search, security (SSRF), pagination
plane/middleware, plane/settings -> request logging, body limit, settings modular
```

### 2.3 Integrasi di luar HTTP

- **Realtime saat ini**: `apps/live` (HocusPocus/Yjs) memanggil API dengan **cookie user yang diteruskan**. Endpoint Django yang dipakai: current user, akses halaman, load/save page, mentions, dan redirect asset untuk PDF. Header `live-server-secret-key` adalah middleware internal server live dan tidak diteruskan ke API. Selama fase 0-8 dan jendela cutover Fase 9, Node tetap menjadi oracle/rollback. Setelah cutover Fase 9 selesai, server yang sama secara protokol dijalankan oleh binary Rust `plane-live`; tidak ada service live Node lagi.
- **Storage**: S3/MinIO, presigned POST + presigned GET, `asset_id` sebagai kunci validasi upload.
- **Queue saat ini**: RabbitMQ + Celery (`worker`, `beat-worker`), Redis sebagai lock/cache.
- **Queue target Rust**: Redis Streams untuk worker Rust. RabbitMQ tetap wajib selama service Django/Celery masih hidup; memindahkan broker saja tanpa memindahkan worker/beat tidak menghasilkan arsitektur yang lebih ringan.
- **Email**: SMTP via template Django (`apps/api/templates`).
- **Read replica**: `ENABLE_READ_REPLICA` + `DATABASE_READ_REPLICA_URL` (+ middleware `db_routing`).
- **Telemetry**: `scout-apm`, push metrik instance berkala (job Celery).

---

## 3. Ruang Lingkup & Non-Goals

### In scope

- Semua 6 pintu API di atas, termasuk SSE/streaming bila ada (cek per endpoint).
- Auth: email/password, magic code, OAuth Google/GitHub/GitLab/Gitea, session, API key, signout.
- Izin: workspace role (owner/admin/member/guest), project role, page permission.
- Semua CRUD domain: workspace, project, issue/work item, cycle, module, state, label, estimate, intake, page, view, sticky, favorite, notification, webhook, asset, API token, recent visit, draft.
- Background job (31 modul) + 13 jadwal beat + job ad-hoc (export, import, seed, dummy data).
- Exporter (CSV/XLSX/JSON) dan analytics aggregation.
- Public space API + instance admin API.

### Out of scope (fase 0-8)

- Mengganti `apps/live` sebelum cutover backend selesai. Server live tetap Node/Hocuspocus selama fase 0-8 dan jendela cutover Fase 9 sebagai oracle/rollback, dan source-nya tidak boleh diubah sampai cutover selesai. Migrasi server WebSocket/Yjs ke Rust `plane-live` masuk Fase 9 (13.10); Node baru dihentikan setelah cutover Fase 9 selesai, dan target akhirnya tidak ada runtime Node untuk live.
- Mengganti frontend/stack UI.
- Menyentuh kode frontend (`apps/web`, `apps/admin`, `apps/space`): URL builder, tipe response, store, env, dependency, atau test produk. Artefak baru hanya boleh berupa tooling test di workspace terpisah.
- Mengubah skema database, nama tabel, atau tipe kolom.
- Mengubah, menambah, atau menghapus URL/method; membuat versi API baru; mengganti nama query/header/field.
- Menambah fitur produk baru di tengah migrasi (freeze fitur backend selama fase 4-6).
- Migrasi data historis (tidak perlu, DB dipakai bersama).

---

## 4. Arsitektur Target

### 4.1 Bentuk deployment

```
                       +-----------------------------+
   browser / live ---> |  Caddy / ingress (existing)  |
                       +--------------+--------------+
                                      |
                     +----------------v-----------------+
                     |  plane-api (Rust, axum) :8000    |
                     |  - HTTP handler + middleware     |
                     |  - session/API-key auth          |
                     |  - rate limit (Redis)            |
                     +---+----------+-----------+-------+
                         |          |           |
              Postgres 15 |     Redis/Valkey   |  S3/MinIO
                         |          |           |
              +----------v--+  +----v-----+  +--v---------+
              | plane-worker|  | scheduler|  | asset flow |
              | (jobs)      |  | (cron)   |  | presigned  |
              +-------------+  +----------+  +------------+
```

Satu binary `plane-api` + satu binary `plane-worker` (bisa satu crate, dua entrypoint), menggantikan service `api`, `worker`, `beat-worker` di `docker-compose*.yml`.

### 4.2 Struktur folder (kondisi saat ini + target)

Struktur aktual dan artefak yang sudah ada:

```
plane/
├─ apps/
│  ├─ api/                         # Django historis: oracle + sumber baseline; bukan runtime
│  │  ├─ plane/db/migrations/      # 125 migrasi, TIDAK disentuh
│  │  ├─ plane/license/migrations/ # 6 migrasi, TIDAK disentuh
│  │  ├─ plane/db/models/          # referensi historis; tidak dipakai runtime setelah Fase 8
│  │  ├─ plane/settings/, manage.py
│  │  ├─ plane/utils/              # hanya yang di-import RunPython
│  │  └─ pyproject.toml, uv.lock
│  ├─ api-rs/                      # backend Rust (detail di bawah)
│  ├─ web/ admin/ space/           # FE: nol diff
│  ├─ live/                        # Node/Hocuspocus fase 0-8 + cutover Fase 9; arsip setelah plane-live Rust
│  └─ proxy/                       # Caddy existing; hanya config route
├─ packages/                       # shared FE: nol diff
├─ contracts/                      # sumber kebenaran kontrak (root, dua toolchain)
│  ├─ routes.json                  # method + path + converter + urutan
│  ├─ endpoints.csv
│  ├─ openapi-django.json
│  ├─ route-owners.json            # ownership per endpoint (rust/django)
│  ├─ endpoint-status.json         # green/red/waived per endpoint
│  ├─ cases/                       # definisi request per endpoint/kasus
│  ├─ golden/<case>/               # rekaman wire dari Django nyata
│  ├─ tools/                       # dump_routes, route_parity, capture-*
│  └─ tests/                       # test kontrak checked-in
├─ tests/browser/                  # workspace terpisah: Playwright smoke FE
├─ deployments/, docs/, docker-compose*.yml
```

Detail `apps/api-rs/`:

```
apps/api-rs/
├─ Cargo.toml / Cargo.lock         # Cargo workspace sendiri (bukan workspace pnpm)
├─ rust-toolchain.toml
├─ rustfmt.toml, clippy.toml      # deny.toml belum ada
├─ crates/
│  ├─ domain/                      # entity, aturan bisnis, filter DSL AST (tanpa axum/sqlx)
│  ├─ db/                          # repository sqlx, query builder, pagination, soft delete
│  ├─ api/                         # bin plane-api: router, middleware, DTO, handler
│  │  └─ tests/{wire_golden,error_golden}.rs
│  ├─ worker/                      # bin plane-worker: jobs, scheduler, streams
│  ├─ migrate/                     # bin plane-migrate: runner, ledger, generator
│  ├─ live/                        # bin plane-live: WebSocket + Yjs (cutover Fase 9)
│  └─ pkg/                         # config, error, time, auth crypto, s3, mail, redis, telemetry
│     └─ tests/serializer_golden.rs
├─ contracts/routes.rust.json      # manifest route Rust (sudah ada, git-tracked)
├─ migrations/                     # DIRENCANAKAN: manifest.json + db/license SQL
├─ tests/                          # DIRENCANAKAN: api_e2e.rs, wire/, state/
└─ scripts/                        # DIRENCANAKAN: record-wire.sh, verify-migrations.sh
```

Catatan struktur:

- `contracts/` di root karena dipakai dua toolchain (Django recorder + Rust harness); `apps/api-rs` merujuk `../../contracts`.
- `crates/api/tests/wire_golden.rs` sudah menguji `/` dan `/robots.txt` lewat TCP; `error_golden.rs` dan `pkg/tests/serializer_golden.rs` adalah unit/golden test internal yang sudah ada. Harness e2e HTTP penuh (`tests/api_e2e.rs`, `tests/wire/`, `tests/state/`) masih direncanakan.
- `contracts/params.json` belum ada; `param-parity` masih gate target, bukan gate yang sudah berjalan di repo ini.
- Setelah Fase 8, tidak ada runtime Django. Seluruh source `apps/api/**` tetap disimpan sebagai arsip read-only, termasuk `urls/`, `views/`, `serializers/`, `bgtasks/`, `authentication/`, `celery.py`, models, settings, migrations, dan test Python. Source tersebut tidak dibangun, tidak dijalankan, tidak menjadi fallback, dan tidak menerima migrasi baru. Service `api`/`worker`/`beat-worker` dan image produksi tidak membawa Python atau dependency Python.
- `apps/api-rs` tidak didaftarkan di `pnpm-workspace.yaml`; jangan campur lockfile Node dan Cargo. Setelah cutover Fase 9 selesai, `crates/live` menghasilkan binary `plane-live`; tidak ada service live dari `apps/live`.
- `crates/live` memakai `pkg`, `db`, dan `domain`; implementasi CRDT memakai crate `yrs` (protokol Yjs), bukan encoder/decoder Yjs buatan sendiri. `apps/live/**` adalah oracle protokol dan tetap arsip read-only setelah cutover.
- `tests/browser` workspace terpisah supaya `apps/web`, `apps/admin`, `apps/space` tetap nol diff.

Alasan pemisahan crate: `domain` dan `db` dipakai bersama oleh `api` dan `worker`, sehingga aturan bisnis tidak terduplikasi (sumber divergensi paling umum saat menulis ulang backend). Perilaku endpoint dibuktikan di level HTTP; logika domain internal tetap wajib punya unit test.

### 4.3 Pilihan crate

| Kebutuhan       | Crate                                                           | Catatan                                                                                                                                     |
| --------------- | --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| HTTP server     | `axum` 0.8 + `tower` + `tower-http`                             | trace, cors, compression (gzip), timeout, body limit                                                                                        |
| Runtime         | `tokio`                                                         | multi-thread                                                                                                                                |
| DB              | `sqlx` 0.8 (`postgres`, `uuid`, `json`, `chrono`)               | PostgreSQL saja; runtime query (`query_as`), bukan makro, supaya build tak butuh DB                                                         |
| Serialize       | `serde`, `serde_json` (`preserve_order`)                        | urutan field = DRF; format waktu/angka kustom (7.4, 8.7)                                                                                    |
| Parsing request | parser sendiri di atas `serde_json` + `multer`                  | padanan DRF (415, unknown field diabaikan); jangan `Json<T>` axum (8.7)                                                                     |
| Validasi        | `validator`                                                     | padanan serializer validation                                                                                                               |
| OpenAPI         | `utoipa` + `utoipa-swagger-ui`                                  | dibandingkan dengan schema drf-spectacular                                                                                                  |
| Config          | `figment` (env + file)                                          | nama env var wajib sama dengan Django                                                                                                       |
| Logging         | `tracing`, `tracing-subscriber` (JSON)                          | samakan field dengan request logger                                                                                                         |
| Metrics/APM     | `metrics` + `metrics-exporter-prometheus`, `opentelemetry-otlp` | pengganti scout-apm                                                                                                                         |
| Redis           | `redis` 0.27 (`tokio-comp`, `streams`) + `deadpool-redis`       | cache, lock, rate limit, Streams untuk job berat (11.6)                                                                                     |
| Job queue       | `apalis` + `apalis-redis` (atau consumer Streams sendiri)       | padanan Celery; retry/ack/DLQ harus eksplisit (11.6)                                                                                        |
| Migrasi         | runner + generator sendiri                                      | baseline historis dari `sqlmigrate` + port `RunPython`; setelah freeze migrasi baru dimiliki Rust. Ledger `django_migrations` dipertahankan |
| Cron            | `tokio-cron-scheduler`                                          | 13 jadwal (bagian 8)                                                                                                                        |
| S3              | `aws-sdk-s3`                                                    | presigned POST/GET, copy, delete                                                                                                            |
| Email           | `lettre` + `minijinja`                                          | template HTML lama dipakai ulang                                                                                                            |
| HTTP client     | `reqwest` (rustls)                                              | webhook, OAuth, unfurl link                                                                                                                 |
| HTML            | `ammonia` (sanitize), `scraper`/`lol_html`                      | padanan bleach/BeautifulSoup                                                                                                                |
| XLSX            | `rust_xlsxwriter` (tulis), `calamine` (baca)                    | export/import                                                                                                                               |
| CSV             | `csv`                                                           | export/import                                                                                                                               |
| Password        | `pbkdf2`, `sha2`, `hmac`, `base64`, `rand`                      | wajib kompatibel Django (7.1)                                                                                                               |
| Cookie          | `axum-extra` cookie                                             | nama cookie & atribut sama                                                                                                                  |
| UUID            | `uuid`                                                          | tetap v4                                                                                                                                    |
| Waktu           | `chrono` + `chrono-tz`                                          | format serialisasi kustom (7.4); offset dari timezone pengguna                                                                              |
| Error           | `thiserror` (+ `anyhow` hanya di binary)                        | mapping ke bentuk DRF                                                                                                                       |
| Test            | `nextest` (runner) + `reqwest` (client) + compose stack         | hanya e2e HTTP nyata: Postgres/Valkey/MinIO/SMTP nyata; tanpa mock (12)                                                                     |
| Lint            | `clippy -D warnings`, `rustfmt`, `cargo-deny`, `cargo-audit`    | pengganti oxlint/ruff                                                                                                                       |

### 4.4 Yang sengaja TIDAK dipakai

- **ORM penuh (`diesel`, `sea-orm`)**: query kompleks (filter DSL, grouping, cursor pagination) lebih aman ditulis SQL eksplisit agar hasil identik dengan Django. ORM menambah lapisan yang sulit diaudit saat paritas.
- **`sqlx::query!` compile-time**: build image tidak perlu akses DB; gunakan `query_as` + test integrasi terhadap snapshot skema.
- **Perubahan skema/kolom ad-hoc dari sisi Rust**: dilarang. Baseline historis dicerminkan dari Django sekali; setelah freeze, migrasi baru ditulis/di-generate di Rust melalui pipeline migrasi (lihat 7.1). Tidak ada DDL di kode aplikasi.

---

## 5. Prinsip Kompatibilitas (Aturan Main)

1. **DB beku (PostgreSQL).** Baseline historis dicerminkan 1:1 dari file Django; setelah baseline parity dibekukan, migrasi baru dimiliki Rust dan masuk melalui pipeline migrasi. Tidak ada DDL di handler/repository/job. Lihat 7.1 dan 14.3.
2. **Kontrak HTTP beku.** Perubahan bentuk respons = bug, diverifikasi `wire-parity` pada API nyata.
3. **Satu sumber aturan bisnis.** Validasi dipakai bersama oleh HTTP dan job, ditaruh di `domain`.
4. **Tidak ada "sekalian refactor".** Tidak memperbaiki inkonsistensi API, tidak menambah field, tidak mengganti nama. Perbaikan masuk backlog terpisah.
5. **Paritas dulu, performa kemudian.**
6. **Setiap modul wajib lulus tes pembanding** sebelum menerima traffic.
7. **Satu route, satu owner (selama transisi).** Caddy hanya merutekan endpoint yang sudah terdaftar di `contracts/routes.json`; tidak ada endpoint yang dilayani dua backend.

---

## 6. Peta Modul: Django -> Rust

| Django                     | Isi                           | Padanan Rust                        | Catatan                            |
| -------------------------- | ----------------------------- | ----------------------------------- | ---------------------------------- |
| `plane/db/models`          | ±80 model, soft delete, audit | `crates/domain` + `crates/db`       | tabel & kolom apa adanya           |
| `plane/app/views/*`        | 218 kelas view (web API)      | `crates/api/src/routes/*`           | per modul                          |
| `plane/api/views/*`        | API v1 (API key)              | `crates/api/src/routes/v1/*`        | kontrak publik, prioritas          |
| `plane/space`              | public API                    | `crates/api/src/routes/public/*`    | `AllowAny`, anti-abuse             |
| `plane/license`            | instance admin                | `crates/api/src/routes/instances/*` | cookie `admin-session-id`          |
| `plane/authentication`     | session/oauth/magic           | `crates/pkg/auth`                   | paling sensitif                    |
| `plane/bgtasks`            | 31 modul task                 | `crates/worker/jobs/*`              | idempotensi wajib                  |
| `plane/utils/filters`      | DSL filter (1.086 baris)      | `crates/db/src/filter`              | port 1:1 + tabel uji               |
| `plane/utils/exporters`    | CSV/XLSX/JSON                 | `crates/api/src/export`             | urutan kolom harus sama            |
| `plane/utils/porters`      | schema export/import          | `crates/domain/src/porters`         |                                    |
| `plane/utils/paginator`    | cursor & grouping             | `crates/api/src/pagination`         | format cursor `v:offset:is_prev`   |
| `plane/utils/issue_search` | `icontains` search            | `crates/db/src/search`              | `ILIKE` + ekstraksi angka          |
| `plane/utils/url_security` | SSRF pinning                  | `crates/pkg/net`                    | keamanan, bukan opsional           |
| `plane/middleware`         | body limit, request log       | `crates/api/src/middleware`         | termasuk tulis `api_activity_logs` |
| `plane/settings/*`         | env & config                  | `crates/pkg/config`                 | nama env var dipertahankan         |

### 6.1 Urutan port per modul

Read-only dulu (bisa shadow traffic tanpa risiko), lalu write, lalu job:

1. workspace, member, project (GET)
2. issue/work item (GET + filter + pagination + grouping)
3. cycle, module, state, label, estimate, view, intake (GET)
4. page (GET) + asset read/presigned
5. write path: issue, project, cycle, module, page
6. asset upload/duplikasi/restore
7. notification/webhook/activity/versioning job
8. auth & session
9. API v1 (API key), public space, instance admin

---

## 7. Layer Data

### 7.1 Migrasi: baseline historis Django, ownership baru di Rust (PostgreSQL)

**Baseline historis** berasal dari file migrasi Django yang dibekukan **sekali** menjadi snapshot yang dapat diaudit. Rust mencerminkannya untuk membentuk baseline yang dapat dieksekusi. Setelah manifest dan SQL Rust lolos `schema-parity`, `ledger-parity`, dan `data-parity`, Rust menjadi executable migration source; file Django hanya menjadi arsip read-only dan checksum, tidak dijalankan di production.

Fakta baseline yang harus selalu cocok (target gate `migration-drift`; belum ada workflow Rust): 125 migrasi `plane.db`, 6 migrasi `plane.license`, 60 `RunPython` (di 33 file, semuanya app `db`), 0 `RunSQL`, 0 `SeparateDatabaseAndState`, plus migrasi `django.contrib.auth`, `django.contrib.contenttypes`, `django.contrib.sessions`, dan `django_celery_beat` sesuai `showmigrations --plan`.

#### Struktur

```
apps/api-rs/migrations/
  manifest.json                  # urutan final: app_label, name, sha256 sumber Django
  db/0001_initial.sql            # 125 file, satu per migrasi plane.db
  db/0035_auto_20230704_2225.sql
  ...
  license/0006_instance_is_current_version_deprecated.sql  # 6 file
  auth/0001_initial.sql          # dari django.contrib.auth
  contenttypes/0001_initial.sql
  sessions/0001_initial.sql
  celery_beat/0001_initial.sql   # django_celery_beat
```

- Urutan baseline = hasil `manage.py showmigrations --plan` saat baseline dibekukan (bukan urutan nomor file per app saja).
- Nama file baseline mengikuti `app/name` Django supaya pemetaan 1:1 mudah diaudit.
- `manifest.json` menyimpan `sha256` file Django sumber untuk baseline. Setelah freeze, perubahan file historis tanpa proses audit membuat `migration-drift` gagal; migrasi baru tidak ditambahkan ke sana.

#### Pipeline generate

1. **Sekali, saat membentuk baseline:** `uv run python manage.py showmigrations --plan` -> manifest urutan lengkap (semua app).
2. **Sekali, saat membentuk baseline:** untuk setiap migrasi historis, `uv run python manage.py sqlmigrate <app> <name>` -> SQL DDL.
3. Untuk setiap `RunPython` historis: ekstrak fungsi + `apps.get_model(...)` yang dipakai, lalu port ke SQL/Rust pada posisi yang sama.
4. Normalisasi baseline: buang `BEGIN;`/`COMMIT;` (runner yang mengatur transaksi), samakan spasi, pertahankan urutan statement dan nama constraint/index apa adanya.
5. Tambahkan komentar wajib:
   - Header: `-- source: apps/api/plane/db/migrations/0125_reset_project_deploy_board_identity.py`
   - `-- purpose:` ringkasan tujuan migrasi
   - `-- affected:` daftar `table.column` / `table` yang berubah
   - `-- data:` ada/tidak ada data migration, dan tabel yang disentuh
   - `-- reversibility:` forward-only atau reversible
   - Komentar inline untuk setiap `CREATE/ALTER/DROP/INSERT/UPDATE/DELETE`.
6. Tulis checksum baseline ke `manifest.json` dan bekukan manifest.

Setelah baseline dibekukan:

- Migrasi baru ditulis atau di-generate di `apps/api-rs/migrations/` dan dimiliki pipeline Rust.
- Generator Django dipensiunkan dari CI produksi. Bila masih diperlukan untuk audit, ia hanya boleh menjadi tool offline dengan input snapshot dan tidak menjadi dependency build/runtime.
- `manifest.json` tetap menjadi ledger urutan eksekusi; penambahan file baru harus deterministik dan lolos gate yang sama.

#### Runner (`plane-migrate`)

- Binary terpisah (bukan ikut start API) agar deploy bisa `migrate -> api -> worker`.
- Ambil `pg_advisory_lock` sebelum mulai (mencegah dua instance migrasi bersamaan).
- Jalankan file sesuai manifest, satu transaksi per migrasi (sama seperti Django `atomic=True` default).
- Setelah sukses, `INSERT INTO django_migrations (app, name, applied) VALUES ($1,$2,now())`, dengan `ON CONFLICT DO NOTHING`/cek prasyarat.
- Bila baris ledger sudah ada, skip (idempoten).
- Mode `--plan` (dry run) mencetak migrasi yang akan jalan; mode `--check` memastikan ledger dan DB sinkron.

#### Data migration (`RunPython`)

- Dijalankan pada posisi urutan yang sama, terhadap kolom yang **ada pada titik itu** (bukan skema akhir).
- Logika yang memakai historical model diport menjadi SQL eksplisit; bila terlalu kompleks, menjadi fungsi Rust yang menerima koneksi + membaca/menulis tabel saat itu.
- Wajib punya uji: fixture sebelum migrasi -> jalankan -> snapshot tabel terdampak -> bandingkan dengan hasil Django.
- `reverse_code` (`RunPython.noop`) dicatat; migrasi yang tidak punya reverse ditandai forward-only.
- Perbandingan data: dump semua kolom tabel terdampak, urut primary key, `NULL` dibedakan dari string kosong; bandingkan hash per tabel lalu tampilkan baris pertama yang beda. Bandingkan setelah **setiap** data migration, bukan hanya di akhir.
- `RunPython` yang memakai `apps.get_model` harus membaca kolom yang ada pada titik waktu itu; jangan memakai model final. Ini penyebab paling umum urutan migrasi terlihat benar tetapi data akhirnya beda.

#### Gate paritas

| Gate                     | Isi                                                                                                                                                           |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `migration-drift`        | `sha256` snapshot baseline historis di `manifest.json` cocok; jumlah & nama migrasi sama. File Django bukan input runtime dan tidak boleh berubah tanpa audit |
| `schema-parity-fresh`    | DB kosong: Django vs Rust -> `pg_dump --schema-only` dinormalisasi = identik                                                                                  |
| `schema-parity-stepwise` | Setelah **setiap** migrasi (urutan manifest), skema Django vs Rust dibandingkan; menangkap salah urutan/ketergantungan yang tidak terlihat di skema akhir     |
| `schema-parity-upgrade`  | snapshot rilis lama -> head, dua jalur, hasil identik                                                                                                         |
| `ledger-parity`          | isi `django_migrations` sama (app, name, urutan)                                                                                                              |
| `data-parity`            | untuk tiap data migration: snapshot tabel terdampak sama                                                                                                      |
| `comment-coverage`       | 100% file Rust punya header + komentar per statement                                                                                                          |

#### Larangan

- Mengedit file migrasi Django (termasuk menambah komentar di sana).
- Menambah kolom/index "khusus Rust".
- Menjalankan `sqlx migrate`/`refinery` dengan format migrasi sendiri.
- Memakai tabel ledger selain `django_migrations`.

### 7.2 Soft delete & audit

Django memakai `deleted_at` + `SoftDeletionManager` dan `AuditModel` (`created_by`, `updated_by`, `created_at`, `updated_at`). Rust wajib:

- Semua query default `deleted_at IS NULL`.
- Delete = set `deleted_at` + cascade soft delete sesuai `soft_delete_related_objects` (daftar relasi eksplisit per model, bukan refleksi).
- `created_by`/`updated_by` diisi dari aktor request (padanan `crum` middleware): `Actor { user_id, api_token_id, source }` di-inject ke repository, bukan global thread-local.
- **Verifikasi DDL**: `created_at`/`updated_at` di Django diisi aplikasi (`auto_now`), belum tentu punya default DB. Bila tidak ada default, Rust harus set sendiri `Utc::now()`.

### 7.3 Relasi & tabel pivot

M2M Django memakai tabel pivot eksplisit (`issue_assignees`, `issue_labels`, `module_issues`, `cycle_issues`, `page_labels`, dst.) dengan kolom audit. Rust menulis ke tabel pivot yang sama; jangan buat tabel baru.

### 7.4 Tipe & format nilai

Diukur dari DRF asli, bukan dari dokumentasi: `contracts/tools/capture_serializer.py`
menulis `contracts/golden/serializer-*` memakai `Serializer` + `JSONRenderer`
(tanpa DB, tanpa Docker), lalu `crates/pkg/src/python_repr.rs` dan
`crates/pkg/src/json.rs` direproduksi byte-per-byte oleh unit test, lalu perilaku HTTP-nya diverifikasi ulang lewat wire golden.

| Hal                     | Perilaku Django/DRF (terukur)                                                                                                                                     | Aksi Rust                                                      |
| ----------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| UUID                    | string lowercase                                                                                                                                                  | `uuid::Uuid` -> Display                                        |
| Waktu                   | `datetime.isoformat()`: `%Y-%m-%dT%H:%M:%S`, mikrodetik **hanya bila != 0** (6 digit), lalu `+00:00` diganti `Z`; offset lain mengikuti timezone aktif (`+07:00`) | `python_repr::iso8601` dengan `chrono_tz`; jangan hardcode `Z` |
| `DateField`             | `YYYY-MM-DD`                                                                                                                                                      | `python_repr::iso8601_date` (`chrono::NaiveDate`)              |
| JSONB                   | objek/array apa adanya                                                                                                                                            | `serde_json::Value` (parse saja)                               |
| Boolean/angka           | tidak dikonversi                                                                                                                                                  | jangan pakai `String`                                          |
| `null` vs `""`          | nullable -> `null`; `""` hanya bila datanya string kosong                                                                                                         | uji per field, jangan "rapikan"                                |
| Enum integer            | angka (`0/1/2`)                                                                                                                                                   | `#[repr(i16)]` + mapping eksplisit                             |
| Decimal                 | **tidak ada `DecimalField`** di `apps/api/plane` saat capture; aturan DRF `COERCE_DECIMAL_TO_STRING=True` tetap dicatat untuk field baru                          | jangan tambah encoder Decimal sebelum ada field nyata          |
| Float (`FloatField`)    | `repr` Python, bukan `ryu`: `65535.0`, `1e+16`, `1e-07`, `-0.0`, `1.7976931348623157e+308`                                                                        | `python_repr::repr_float`, diuji terhadap 218 kasus `repr()`   |
| `NaN`/`Infinity`        | `json.dumps(allow_nan=False)` -> `ValueError` -> **500**, bukan `null`                                                                                            | `JsonError::NonFinite` -> 500                                  |
| Urutan field JSON       | urutan serializer/dict                                                                                                                                            | struct field order + `serde_json` `preserve_order`             |
| Escaping DRF            | UTF-8 mentah, tetapi `U+2028`/`U+2029` tetap `\u2028`/`\u2029`; `0x7f` mentah                                                                                     | `Style::Drf`                                                   |
| Escaping `JsonResponse` | ber-spasi setelah `:`/`,`; semua > `0x7e` dan `0x7f` menjadi `\uXXXX` (surrogate pair untuk astral)                                                               | `Style::Django`                                                |

Catatan implementasi:

- Encoder JSON ditulis sendiri di `crates/pkg/src/json.rs` (`serde::Serialize` walker). `serde_json` hanya untuk **parsing**; `serde_json::ser` tidak dipakai karena tiga perilaku berbeda (float, `NaN`, `U+2028`).
- `serde_json` wajib mengaktifkan fitur `float_roundtrip`; tanpa itu `212153.88871970866` ter-parse menjadi `...863` dan input JSONB/angka berubah sebelum diserialisasi.
- API: `compact_json`/`spaced_json` mengembalikan `Result<_, JsonError>`; `JsonResponse::{django,drf}` tetap pembungkus bertipe.

### 7.5 Read replica: tidak dipakai

Keputusan (18.4 #6): read replica **tidak dipakai** di produksi, jadi Rust hanya punya satu pool RW.

- Django mendukung `ENABLE_READ_REPLICA` + `DATABASE_READ_REPLICA_URL`, tetapi produksi berjalan dengan nilai `0`; perilaku default itulah yang wajib identik.
- Jangan menambah pool `ro`, router, atau cache per-replica di Rust.
- Fail-fast: bila `ENABLE_READ_REPLICA=1` terdeteksi saat startup, proses berhenti dengan pesan jelas (`unsupported: read replica not implemented`). Ini mencegah divergensi diam-diam, bukan mengabaikan config.
- Semua query, termasuk GET, memakai koneksi RW sehingga read-after-write konsisten.
- Bila replica dibutuhkan kelak, itu PR terpisah dengan gate `wire-parity` tersendiri.

### 7.6 Performa query

- Port `select_related`/`prefetch_related` menjadi JOIN atau batch `WHERE id = ANY($1)`, bukan N+1.
- Cursor pagination butuh index yang sama; jangan tambah index tanpa analisa (DB dipakai bersama).
- Hot path: list issue dengan filter kompleks + grouping. Ukur dengan snapshot data produksi; target p95 setara atau lebih baik.

### 7.7 Bukan multi-tenant: satu workspace boundary

Plane adalah **satu deployment, satu database, satu schema**. Tidak ada `django-tenants`, tidak ada schema/DB per pelanggan, dan tidak ada `search_path` switching. Fakta ini berasal dari implementasi Django yang menjadi oracle migrasi, bukan alasan untuk mempertahankan runtime Django.

Bukti di kode:

- `apps/api/plane/settings/common.py` hanya memakai `DATABASE_ROUTERS = ["plane.utils.core.dbrouters.ReadReplicaRouter"]` - itu read replica, bukan tenant router.
- `apps/api/plane/db/models/workspace.py`: `WorkspaceBaseModel.workspace = models.ForeignKey("db.Workspace", ...)`; semua model domain mewarisi ini.
- Tidak ada dependency `django-tenants`/`tenant-schemas` di `apps/api/pyproject.toml`, dan tidak ada middleware tenant.
- Istilah "tenant" di komentar/test (`cross-tenant IDOR`) berarti **workspace**, bukan schema terpisah.

Konsekuensi untuk Rust:

- Satu schema `public`, satu set migrasi; jangan menambah tabel `tenant` atau kolom `schema_name`.
- Isolasi = filter `workspace_id`/`project_id` + permission check di setiap query; bukan route/middleware tenant.
- Tidak ada `SET search_path`, pool per tenant, atau cache key per tenant.
- Sandbox dan produksi adalah **deployment terpisah** (DB/Redis/S3 sendiri), bukan tenant di instance yang sama.

---

## 8. Kontrak HTTP yang Wajib Identik

Ini daftar hal yang biasanya "kelihatan sepele" tapi mematahkan frontend. Semua item di bawah harus punya case `wire-parity` pada API nyata.

### 8.0 URL surface: nol perubahan

Tugas ini mengganti **implementasi**, bukan API. Semua URL yang ada hari ini adalah kontrak publik: frontend (`apps/web`, `apps/admin`, `apps/space`), `apps/live`, integrasi API key, consumer webhook, dan deployment pelanggan. Tidak ada satu pun yang boleh berubah.

| Root                  | Contoh path (harus identik)                      | Catatan                                                                                      |
| --------------------- | ------------------------------------------------ | -------------------------------------------------------------------------------------------- |
| `/api/`               | `/api/workspaces/<slug>/projects/<uuid>/issues/` | session cookie; termasuk `assets/v2/*` dan `.../v2/issues/` (literal `v2`, bukan versi baru) |
| `/api/v1/`            | `/api/v1/workspaces/<slug>/projects/`            | API key                                                                                      |
| `/api/public/`        | `/api/public/workspaces/<slug>/...`              | `AllowAny` + deploy board                                                                    |
| `/api/instances/`     | `/api/instances/...`                             | admin cookie                                                                                 |
| `/auth/`              | `/auth/google/`, callback OAuth                  | redirect 302 + `Location` identik                                                            |
| `/` dan `/robots.txt` | health `{"status":"OK"}`, teks robots            |                                                                                              |

Daftar di atas bukan sumber kebenaran. Sumber kebenaran adalah `contracts/routes.json` hasil ekstraksi `get_resolver()`, bukan ingatan atau pembacaan manual.

**Aturan:**

1. **Path literal identik**: plural/singular, `v2`, `bulk`, `restore`, `duplicate-assets`, `file-assets`, dan trailing slash. Tidak ada normalisasi case, penghapusan slash, atau penambahan prefix.
2. **Method per path identik**: method yang tidak didukung → 405 dengan body dan header `Allow` yang sama.
3. **Converter identik**: repo hanya memakai `<str:...>` dan `<uuid:...>`. `<str>` = satu segmen tanpa `/` (titik dan huruf besar diizinkan, case-sensitive); `<uuid>` = UUID kanonik. Nilai yang tidak cocok → **404 (route tidak match)**, bukan 400 hasil parsing handler. Jangan menangkap `:pk` sebagai string lalu parse di handler.
4. **Urutan route identik**: Django mencocokkan dari atas. Contoh: `/api/workspaces/file-assets/...` harus menang sebelum `/api/workspaces/<str:slug>/...`. Jika router Rust tidak bisa menyatakan urutan itu, pakai dispatcher eksplisit; jangan mengandalkan "static menang" tanpa tes.
5. **Trailing slash identik** (lihat 8.6): jangan memperbaiki path di router; status 301/404 harus mengikuti Django.
6. **Query key identik**: nama, format array (`?state=a,b` vs repeated), boolean (`true`/`false`), dan string kosong. Tidak ada rename ke camelCase.
7. **Header identik**: `X-Api-Key`, `X-RateLimit-Remaining/Reset`, `X-Plane-Delivery/Event/Signature`, `Origin`, `Cookie` (`session-id`, `admin-session-id`). Header internal server live tidak termasuk kontrak API Django dan tidak boleh ditambahkan ke request keluar `apps/live`.
8. **Redirect identik**: OAuth/magic-link mengembalikan status dan `Location` yang sama (termasuk query dan fragmen). Jangan mengganti 302→307 atau menambah slash pada redirect.
9. **Nama route** (`name=` di `urlpatterns`) dipertahankan bila dipakai `reverse`/template email; URL di email harus sama.

**Gate `route-owner` dan `route-parity`:**

- Generator mengekstrak `(method, path_template, converter, name, urutan)` dari Django `get_resolver()` untuk matriks env produksi (`ENABLE_DRF_SPECTACULAR` on/off, `DEBUG=false`) → `contracts/routes.json`.
- `contracts/route-owners.json` dan `contracts/endpoint-status.json` adalah sumber ownership/status per endpoint selama transisi. `green` hanya sah bila manifest Rust benar-benar menyajikan route tersebut; `waived` wajib punya alasan. Setelah Fase 8, owner `django` harus nol.
- `route-owner` memeriksa: setiap endpoint punya tepat satu owner, route `rust` benar-benar dilayani, route `django` tidak dilayani Rust, dan tidak ada klaim `green` palsu. Saat ini checker ini masih berupa tool kontrak lokal; belum menjadi required check CI.
- `route-parity` membandingkan dua arah seluruh route Rust dan Django. Selama port berjalan, `--allow-missing` boleh dipakai untuk route yang belum diport; ownership/status tetap diperiksa oleh checker lokal sampai workflow Rust dibuat.
- Setelah Fase 8, `route-parity` penuh wajib hijau: tidak boleh ada route hilang **maupun** route ekstra di Rust. Diff non-kosong = PR gagal.
- Smoke test per route tanpa auth: status/body/header harus sama (401/404/405), plus uji `OPTIONS`/`HEAD` dan trailing slash.
- Route baru dari Django tidak boleh masuk produksi sebelum muncul di manifest Rust.

**Status terukur (baseline Fase 0, `contracts/routes.json`):**

| Metrik                             | Nilai                                                                                                        |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Route / endpoint                   | 391 / 722                                                                                                    |
| `unknown_method_routes`            | 0                                                                                                            |
| `wildcard_method_routes`           | 2 (`/`, `/robots.txt`)                                                                                       |
| `is_regex_path` (`re_path`)        | 8 (invitations + stickies di `/api/v1/`)                                                                     |
| Converter                          | `uuid` 366, `str` 311, `regex` 8, `drf_format_suffix` 2                                                      |
| Duplikat yang memang ada di Django | 2 endpoint, `api/v1/workspaces/<str:slug>/` dan `.../<drf_format_suffix:format>` (order 345/351 dan 346/352) |

Dua duplikat itu bukan bug generator: `plane/api/urls.py` meng-`include(DefaultRouter.urls)` dari `invite.py` **dan** `sticky.py`, jadi `APIRootView` terdaftar dua kali. Rust wajib mereproduksi jumlah duplikat yang sama, bukan menghapusnya (`route_parity.py` membandingkan himpunan duplikat, bukan sekadar "nol duplikat").

`/api/workspaces/` mengembalikan **400** `{"error": "The required key does not exist."}` (`KeyError` `slug` di decorator permission), bukan 200 atau 404. Ini kontrak dan sudah masuk `contracts/golden/workspaces-root/`.

Perintah pembanding:

```bash
cd apps/api-rs
cargo run -q -p plane-api --bin route-manifest -- --out contracts/routes.rust.json
cd ../.. && python contracts/tools/route_parity.py \
  --rust apps/api-rs/contracts/routes.rust.json [--check-order] [--allow-missing]
```

`--allow-missing` dipakai selama port berjalan: route yang belum dideklarasikan di `plane-domain/src/route.rs` diabaikan, tetapi ownership/status, converter, method, trailing slash, dan duplikat tetap harus identik. Status per endpoint disimpan di `contracts/endpoint-status.json`; `green` hanya sah jika manifest Rust benar-benar menyajikan route tersebut.

Status saat ini harus dibaca per populasi:

- `contracts/endpoints.csv`: 722 baris endpoint, 2 `green` + 720 `red`. Dua baris tambahan berasal dari duplikat `APIRootView`.
- `contracts/endpoint-status.json`: 720 key unik, 2 `green` + 718 `red`.
- `contracts/route-owners.json`: 720 key unik, 2 `rust` + 718 `django`.
- `route_parity.py --allow-missing`: 2 route Rust, 718 `missing`, 0 `extra`, 0 error owner/status.

Jadi "hijau" pada gate `--allow-missing` hanya berarti tidak ada route/owner/status yang melanggar selama port berjalan; itu **bukan full route parity**. Full parity baru boleh diklaim setelah 718 endpoint/key yang tersisa dilayani Rust dan `--allow-missing` tidak dipakai lagi. Angka ini diverifikasi oleh `contracts/tests/test_route_parity.py`.

---

### 8.1 Autentikasi & cookie

| Item           | Nilai saat ini                                                                                                         |
| -------------- | ---------------------------------------------------------------------------------------------------------------------- |
| Cookie session | `session-id` (default), `HttpOnly`, `SameSite` mengikuti setting, `Secure` bila HTTPS                                  |
| Cookie admin   | `admin-session-id` untuk path `/api/instances/`                                                                        |
| Masa berlaku   | `SESSION_COOKIE_AGE` default 604800 detik                                                                              |
| Session store  | tabel `sessions` (`session_key`, `session_data`, `expire_date`, `device_info`, `user_id`)                              |
| API key        | header `X-Api-Key`, token plaintext di `api_tokens.token`                                                              |
| CSP/CSRF       | tidak memakai token CSRF; **write** wajib `Origin` di allowlist (own origin / `CORS_ALLOWED_ORIGINS` / base URL Plane) |

**Keputusan (dikunci di 18.4 #1): Opsi A.**

- Opsi A: implementasi encoding/penandatanganan sesi Django di Rust sehingga sesi lama tetap valid, dan Django/Rust bisa membaca sesi satu sama lain selama masa transisi. Spike Fase 0 harus membuktikan round-trip dua arah.
- Opsi B hanya fallback bila Opsi A terbukti mustahil: format sesi sendiri -> semua user logout sekali saat cutover (dan admin logout). Lebih cepat, tapi harus diumumkan.

Apa pun pilihannya, `sessions` harus tetap satu tabel agar live server (yang meneruskan cookie) tetap jalan.

### 8.2 Password

Django menyimpan `pbkdf2_sha256$<iterations>$<salt>$<base64(sha256(...))>`. Rust **wajib** memverifikasi format ini (iterasi dibaca dari hash, bukan hardcode) agar login lama tidak pecah. Hash baru tetap `pbkdf2_sha256` dengan parameter Django saat ini; opsi upgrade ke Argon2 hanya setelah cutover penuh.

### 8.3 Bentuk error

| Kondisi                     | Status | Body                                                                                                                                          |
| --------------------------- | ------ | --------------------------------------------------------------------------------------------------------------------------------------------- |
| Tidak login                 | 401    | `{"detail": "Authentication credentials were not provided."}`                                                                                 |
| Validasi field              | 400    | `{"field": ["pesan"]}` (atau `{"non_field_errors": [...]}`)                                                                                   |
| Tidak berizin               | 403    | `{"detail": "..."}`                                                                                                                           |
| Tidak ada route cocok       | 404    | `{"error": "Page not found."}` (`JsonResponse`, ber-spasi)                                                                                    |
| Objek tidak ditemukan (DRF) | 404    | `{"detail": "Not found."}`                                                                                                                    |
| Rate limit                  | 429    | envelope auth: `{"error_code": ..., "error_message": ...}`                                                                                    |
| Body terlalu besar          | 413    | `{"error": "REQUEST_BODY_TOO_LARGE", "detail": "The size of the request body exceeds the maximum allowed size."}` (`JsonResponse`, ber-spasi) |
| Media type tidak didukung   | 415    | `{"detail": "Unsupported media type \"...\" in request."}`                                                                                    |

`auth_exception_handler` mengubah `NotAuthenticated` menjadi 401 dan `Throttled` menjadi envelope error khusus. Port kedua perilaku ini. Perhatikan perbedaan format: respons DRF compact, respons `JsonResponse` (health, 404 resolver, 413) ber-spasi; keduanya kontrak (8.7).

**Byte terukur** (Django 5.2.17 + DRF 3.18.3, direkam tanpa Docker oleh `contracts/tools/capture_errors.py` ke `contracts/golden/error-*`):

| Case                        | Status | `Content-Type`     | Body (persis)                                                                                                     |
| --------------------------- | ------ | ------------------ | ----------------------------------------------------------------------------------------------------------------- |
| Tidak login                 | 401    | `application/json` | `{"detail":"Authentication credentials were not provided."}`                                                      |
| Tidak berizin               | 403    | `application/json` | `{"detail":"You do not have permission to perform this action."}`                                                 |
| DRF `Http404`               | 404    | `application/json` | `{"detail":"Not found."}`                                                                                         |
| Route tidak cocok           | 404    | `application/json` | `{"error": "Page not found."}`                                                                                    |
| Metode salah                | 405    | `application/json` | `{"detail":"Method \"DELETE\" not allowed."}`                                                                     |
| Field tidak valid           | 400    | `application/json` | `{"name":["This field is required."]}`                                                                            |
| `validate()` serializer     | 400    | `application/json` | `{"non_field_errors":["Something went wrong."]}`                                                                  |
| `ValidationError` dari view | 400    | `application/json` | `["Something went wrong."]`                                                                                       |
| Media type salah            | 415    | `application/json` | `{"detail":"Unsupported media type \"text/plain\" in request."}`                                                  |
| Rate limit                  | 429    | `application/json` | `{"error_code":5900,"error_message":"RATE_LIMIT_EXCEEDED"}`                                                       |
| Body > 5 MiB                | 413    | `application/json` | `{"error": "REQUEST_BODY_TOO_LARGE", "detail": "The size of the request body exceeds the maximum allowed size."}` |

Tiga koreksi yang wajib diikuti:

- **`Allow` ada di semua respons DRF**, bukan hanya 405. `APIView.default_response_headers` selalu menyetelnya, jadi 401/403/404/415/429 pun membawa `Allow: <verb view itu>`. Nilainya per view, bukan konstanta.
- **413 dipicu `DATA_UPLOAD_MAX_MEMORY_SIZE`, bukan `FILE_SIZE_LIMIT`.** `plane/settings/common.py` menyambungkan `DATA_UPLOAD_MAX_MEMORY_SIZE` dari env `FILE_SIZE_LIMIT` (default 5 MiB), jadi ambangnya 5 MiB, tetapi nama setting yang dibaca middleware adalah `DATA_UPLOAD_MAX_MEMORY_SIZE`.

- **Escaping non-ASCII berbeda.** `JsonResponse` memakai `json.dumps` default (`ensure_ascii=True`), jadi menulis `"Ren\u00e9"` dan surrogate pair `"\ud83d\ude00"`. DRF `JSONRenderer` mengirim UTF-8 mentah (`"Ren\u00e9"` sebagai byte `c3 a9`). Jangan memakai satu encoder untuk keduanya.

#### Envelope `BaseAPIView.handle_exception`

Selain pemetaan DRF di atas, `plane/api/views/base.py` menangkap lima keluarga exception Python yang lolos dari DRF. Keluarannya memakai JSON compact DRF (bukan `JsonResponse`), jadi jangan dirender lewat jalur 404/413. Diukur dengan `contracts/tools/capture_errors.py` dan dibekukan sebagai `contracts/golden/envelope-*`:

| Kondisi                                                 | Status  | Body (persis)                                               |
| ------------------------------------------------------- | ------- | ----------------------------------------------------------- |
| `ParseError` dari `initial()` (body bukan object/array) | 400     | `{"detail":"Request body must be a JSON object or array."}` |
| `IntegrityError`                                        | 400     | `{"error":"The payload is not valid"}`                      |
| Django `ValidationError`                                | 400     | `{"error":"Please provide valid detail"}`                   |
| `ObjectDoesNotExist` di `BaseAPIView`                   | 404     | `{"error":"The requested resource does not exist."}`        |
| `ObjectDoesNotExist` di `BaseViewSet`                   | 404     | `{"error":"The required object does not exist."}`           |
| `KeyError` (mis. slug workspace tidak ada)              | 400     | `{"error":"The required key does not exist."}`              |
| Exception lain                                          | 500     | `{"error":"Something went wrong please try again later"}`   |
| `APIException("pesan", 400)`                            | **500** | `{"detail":"Please check the view"}`                        |

Yang wajib diikuti:

- **Dua handler berbeda untuk objek yang hilang.** `BaseAPIView` menulis `"The requested resource does not exist."`, `BaseViewSet` menulis `"The required object does not exist."`. Di Rust pilih variannya secara eksplisit (`ApiError::ResourceMissing` vs `ApiError::ObjectMissing`); jangan disatukan.
- **Argumen kedua `APIException` adalah `code`, bukan status.** `APIException("Please check the view", 400)` tetap menghasilkan **500**, dan karena DRF sudah memetakannya, body-nya ber-shaped `{"detail": ...}` (bukan envelope `{"error": ...}`). Jangan "memperbaiki" ini; klien yang ada sudah melihat 500.
- **`Allow` tetap ada** di semua respons ini (sama seperti error DRF lain), nilainya per view.
- Kalimat envelope adalah teks tetap. Exception asli hanya di-log (`log_exception`), tidak pernah masuk body; jangan menambahkan `str(exc)` agar tidak membocorkan detail internal.
- **Logging berbeda, respons sama.** `BaseViewSet` menulis `logger.warning`/`logger.error` dengan `error_code` (`VALIDATION_ERROR`, `OBJECT_DOES_NOT_EXIST`, `KEY_ERROR`) untuk tiga keluarga itu, sedangkan `BaseAPIView` hanya memanggil `log_exception`. Body dan status identik, jadi port Rust boleh memakai satu jalur render; yang tidak boleh hilang adalah level log saat observability diport.

### 8.4 Pagination

Envelope cursor (dipakai hampir semua list):

```json
{
  "total_count": 123,
  "next_cursor": "50:1:0",
  "prev_cursor": "50:0:1",
  "next_page_results": true,
  "prev_page_results": false,
  "total_results": 123,
  "results": []
}
```

- Cursor = `"<limit>:<page>:<is_prev>"`.
- Ada varian grouping (group by / subgroup by) yang membungkus `results` menjadi map. Port `plane/utils/paginator.py` apa adanya; jangan "menyederhanakan".
- `PAGINATOR_MAX_LIMIT = 1000`.

### 8.5 Query parameter & filter

- Filter DSL di `plane/utils/filters` (1.086 baris: `converters.py`, `filterset.py`, `filter_backend.py`) menerjemahkan struktur filter (AND/OR bertingkat, operator `exact`, `in`, `contains`, `gt/lt`, `range`, relasi) menjadi query.
- Nilai filter disimpan di JSON `views.filters` dan dipakai ulang oleh saved view. **Struktur JSON tidak boleh berubah.**
- Ordering memakai allowlist (`ISSUE_GROUP_BY_ALLOWLIST`) — pertahankan allowlist itu persis untuk mencegah injection.

#### Peta parameter per route

"Nama param sama" tidak cukup. Untuk setiap route di `contracts/routes.json`, `contracts/params.json` mencatat: nama, lokasi (path/query/header/body), tipe, wajib/opsional, default, nilai yang diterima, dan perilaku saat invalid. Sumber: `drf-spectacular` + introspeksi `filterset_class`/`serializer_class`/`get_queryset`, lalu diverifikasi request nyata ke Django.

Yang wajib sama per parameter:

- Nama dan case (`order_by` tetap `order_by`, bukan `orderBy`).
- Duplikat key: `QueryDict.get` memakai nilai **terakhir**; jangan pakai yang pertama.
- Nilai kosong (`?limit=`): diperlakukan sama seperti Django (default atau error), bukan "diabaikan".
- Key tak dikenal: diabaikan, bukan 400.
- Tipe invalid: sama-sama 400 dengan pesan DRF, atau sama-sama diabaikan; jangan diperbaiki.
- Boolean hanya menerima bentuk yang diterima DRF (`true`/`false`/`1`/`0`); selain itu perilakunya identik.
- Array/list: `?state=a,b` vs `?state=a&state=b` mengikuti parser Django, termasuk urutan hasil.
- Filter JSON (`filters`, `filters_data`, `views.filters`): struktur, validasi, dan operator sama.
- Default dan batas: `limit`, `PAGINATOR_MAX_LIMIT=1000`, format `cursor` `v:offset:is_prev`.
- `order_by`/`group_by`/`sub_group_by` hanya menerima nilai allowlist; di luar itu diperlakukan sama.
- Tanggal/waktu: format diterima, timezone `UTC`, dan hasil parsing sama.
- `expand`/`fields`/select-related bila ada: himpunan nilai dan efek ke payload sama.

**Gate `param-parity`:** `contracts/params.json` di-generate di CI; PR gagal bila ada parameter tak tercatat atau parameter ekstra di Rust. Matriks kasus per route: tanpa param, semua default, tiap param valid, tiap param invalid, duplikat, kosong, dan kombinasi filter saved view nyata. Semua dikirim sebagai request nyata; selisih = gagal.

### 8.6 Lain-lain

- Trailing slash: **wajib sama**. Django `APPEND_SLASH=True` + `CommonMiddleware`: path tanpa `/` yang hanya cocok dengan tambahan slash → 301 untuk method selain `POST`/`PUT`/`PATCH`; tiga method itu tidak di-redirect. Jangan memperbaiki path di router Rust. Uji GET/HEAD/POST/PUT/PATCH/DELETE.
- GZip: `GZipMiddleware` aktif → `tower-http` compression.
- Header rate limit API key: `X-RateLimit-Remaining`, `X-RateLimit-Reset`.
- Batas body: `FILE_SIZE_LIMIT` (default 5 MB) → 413.
- Health: `GET /` -> `{"status": "OK"}`, `GET /robots.txt` -> teks disallow.
- `handler404` kustom (`plane.app.views.error_404`) — pastikan bentuk body sama.
- CORS: origin dari env, `allow_credentials=true`, header tambahan `X-Api-Key`.

---

### 8.7 Request & response: identik 100%

Bukan hanya "bentuk JSON". Yang diuji adalah **pasangan request→response**: status, header, dan byte body.

**Request yang wajib sama**

- Content-Type dan parser: `application/json`, `application/x-www-form-urlencoded`, `multipart/form-data` sesuai default DRF; tipe lain → 415 dengan pesan DRF. Jangan memakai extractor `Json<T>` axum apa adanya: status dan pesan error-nya berbeda.
- Field tak dikenal **diabaikan** (perilaku DRF), bukan 400 karena `deny_unknown_fields`.
- Coercion DRF: string→int/float/bool/UUID/date, nilai `"true"/"false"/"1"/"0"`, angka berbentuk string; `PATCH` parsial vs `PUT` yang mewajibkan field lengkap.
- Query: duplikat key, nilai kosong, array `a,b`, boolean, dan percent-decoding diproses sama.
- Body melebihi `FILE_SIZE_LIMIT` → 413 dengan body middleware yang sama.
- Header auth/cookie/API key/`Origin` dibaca dengan aturan yang sama.

**Kontrak parser request (Fase 0, sudah diimplementasikan)**

Contoh ada di `apps/api-rs/crates/pkg/src/parser/`; oracle adalah DRF 3.18.3 + Django 5.2.17 di venv lokal (tanpa Docker). Urutan keputusan **wajib** dipertahankan karena terlihat di wire:

1. **`Content-Length`** dibaca dengan `int()` CPython, bukan `parse::<i64>()`: strip 25 dari 29 codepoint `isspace()` (`U+001C..U+001F` tidak di-strip), satu tanda `+`/`-`, `_` tunggal di antara digit, digit Unicode `Nd`, maksimum 4300 digit. Nilai yang gagal di-`int()` menjadi `0`. `0` berarti stream `None` → 200 dengan body kosong **sebelum** negosiasi, jadi `text/plain` kosong bukan 415 melainkan `{}`.
2. **Media type > 256 byte** → 415 (negosiator DRF tidak menemukan parser), termasuk `application/json; ...` yang panjang.
3. **Seleksi parser** memakai `media_type_matches` DRF: parameter dicek hanya di sisi kiri (`application/json` cocok dengan `application/json; charset=utf-8`, bukan sebaliknya). Tidak cocok → 415 `{"detail":"Unsupported media type \"<header mentah>\" in request."}`.
4. **Charset**: nama yang tidak dikenal `codecs.lookup` jatuh ke UTF-8 (bukan error). Codec bytes-ke-bytes (`bz2`) → 400 `{"detail":"Unsupported charset \"bz2\" in request Content-Type header."}`. Codec teks yang tidak didukung port ini → **500 HTML** (`UnsupportedCodec`), jangan menebak tabel.
5. **Batas body**: Django membaca `request.body` lebih dulu untuk JSON dan form, jadi body melebihi `DATA_UPLOAD_MAX_MEMORY_SIZE` (Plane: 5 MiB, dari `FILE_SIZE_LIMIT`) → **400 HTML** sebelum charset diproses. Multipart dikecualikan: anggarannya per-field saat parsing (`len(name) + 2 + len(raw)`), file tidak dihitung.
6. **`Content-Length` negatif**: JSON → body kosong → 400 `JSON parse error - Expecting value: line 1 column 1 (char 0)`; form → 200 form kosong; multipart → 400 `Multipart form parse error - Invalid content length: -1`.

**Multipart (Django `multipartparser.py`)**

- Boundary harus fullmatch `[ -~]{0,200}[!-~]`; kosong/hilang → `Invalid boundary in multipart: None` (atau `\"\"` untuk `boundary=`). Boundary berkutip di-unquote oleh `parse_header_parameters`.
- Blok header harus berakhir (`\r\n\r\n`) dalam 1024 byte; chunk 1024 → 2048 lalu berhenti. Lewat itu → 400 `Request max total header size exceeded.` Tanpa header, part menjadi `RAW` dan tidak menghasilkan field/file.
- Nama header dibandingkan **verbatim setelah lowercase**, tanpa trim: `Content-Disposition : ...` bukan header. Baris header non-UTF-8 di-skip (karena `UnicodeDecodeError ⊂ ValueError`), bukan 500.
- Isi `name` di-`strip()` sebagai **bytes** (ASCII saja): tab/spasi hilang, `U+00A0` dipertahankan.
- `DATA_UPLOAD_MAX_NUMBER_FIELDS` menghitung `RAW` dan `FIELD` saja (`FIELD_TYPES = {RAW, FIELD}`; `FILE` tidak), dengan toleransi `+2` untuk dua part mentah; `DATA_UPLOAD_MAX_NUMBER_FILES` menghitung file dengan `filename` non-kosong. `filename=""` tetap `FIELD`; `filename=" "` adalah file bernama spasi.
- Filename: HTML-unescape (`html.unescape`), buang direktori (`/` dan `\`), saring per `str.isprintable()`, lalu tolak `""`, `"."`, `".."`. Tabel `PRINTABLE_RANGES` (711 rentang, Unicode 15) digenerate dari CPython 3.12; regenerate dengan:

  ```python
  ranges, start, prev = [], None, None
  for cp in range(0x110000):
      if chr(cp).isprintable() and cp != 0x20:
          if start is None:
              start = prev = cp
          elif cp == prev + 1:
              prev = cp
          else:
              ranges.append((start, prev)); start = prev = cp
  if start is not None:
      ranges.append((start, prev))
  print(len(ranges))
  for a, b in ranges:
      print(f"    (0x{a:x}, 0x{b:x}),")
  ```

- `base64` (case-insensitive setelah header di-lowercase) memakai `base64.b64decode(..., validate=False)`/`binascii.a2b_base64(strict_mode=0)`: karakter di luar alfabet dilewati, `=` hanya menutup pada posisi 2/3 dalam kuad, sisa kuad → error. Field yang gagal decode memakai byte mentah; file yang gagal → 400 `Could not decode base64 data.`
- Body tanpa boundary penutup: field terakhir menyertakan `\r\n` di nilainya (`"x\r\n"`); file yang belum selesai dibuang. Preamble diabaikan; delimiter LF-only tidak dikenali (form kosong).
- Error Django (`RequestDataTooBig`, `TooManyFieldsSent`, `TooManyFilesSent`, `SuspiciousMultipartForm`) dan `RecursionError`/`UnsupportedCodec` dijawab **HTML 400/500** tanpa `Allow`; error `MultiPartParserError`/`ParseError` DRF dijawab JSON dan tetap membawa `Allow`.

**Golden parser**: `contracts/tools/capture_parser.py` merekam 48 direktori `contracts/golden/parser-*` (request + response + `body.raw`) dan `contracts/golden/parser-json-errors.json` (55 input). Rekaman bersifat idempoten; jalankan dua kali dan bandingkan hash. Verifikasi Rust: `cargo test -p plane-pkg --test parser_golden` (replay 48 kasus, tabel error CPython, edge `Content-Length`/multipart). Setiap perubahan parser wajib memperbarui golden **dan** test, bukan sebaliknya.

**Response yang wajib sama**

- Status code dan header: `Content-Type` (`application/json`, tanpa charset), `Set-Cookie` (nama, atribut, urutan), CORS, `X-RateLimit-*`, serta security header bawaan Django (`X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`, `Cross-Origin-Opener-Policy: same-origin`), dan `Vary`.
- **Body DRF**: compact (`separators=(",", ":")`), UTF-8 mentah (bukan `\uXXXX`), urutan field = urutan serializer. Aktifkan fitur `serde_json` `preserve_order`; jangan memakai `HashMap` untuk map yang tampil di respons.
- **Body non-DRF** memakai `JsonResponse`, yang default-nya **ber-spasi**: `GET /` → `{"status": "OK"}`; 404 resolver → `{"error": "Page not found."}`; 413 → `{"error": "REQUEST_BODY_TOO_LARGE", "detail": "..."}`. Jangan "rapikan" menjadi compact.
- 404 dibedakan: tidak ada route cocok → `handler404` (`{"error": "Page not found."}`); `Http404`/`get_object_or_404` di dalam view → `{"detail": "Not found."}`.
- Angka: `FloatField` mengikuti `repr` Python (`300.0`, eksponen `1e+16`, `1e-07`), bukan format `ryu`/`serde_json` default; `NaN`/`Infinity` menjadi 500, bukan `null`. Tidak ada `DecimalField` di permukaan saat ini (lihat 7.4).
- `null` vs field hilang vs `""` tidak boleh berubah; UUID, waktu, dan enum mengikuti 7.4.
- Pesan validasi/error disalin dari DRF (mis. `"This field is required."`, `"Must be a valid UUID."`), bukan ditulis ulang.

**Satu-satunya nilai yang boleh berbeda** (karena memang berbeda tiap run, bukan kontrak): UUID/ID baru, timestamp "now", token/session/key, signature presigned S3, angka acak, header `Date`, `Expires`/`Max-Age` cookie, `X-Request-Id`/trace, boundary multipart, serta byte hasil kompresi gzip (kontrak = payload setelah dekompresi + `Content-Encoding`). Semua nilai deterministik harus **byte-identik**.

**Harness `wire-parity`**

- Rekam dari Django: request mentah (method, path, query, header, body) + response mentah (status, header, body sebelum kompresi) ke `contracts/golden/<route>/<case>.http`.
- Diff body **byte-per-byte** setelah normalisasi nilai volatil berbasis path (bukan regex global yang bisa menyembunyikan bug).
- Header dibandingkan sebagai set (nama case-insensitive) dengan nilai persis; `Set-Cookie` dibandingkan per atribut.
- Matriks minimal per route: anon/member/admin/API key × happy/400/401/403/404/405/413/415/429.
- Rekaman error path tanpa Docker: `py contracts/tools/capture_errors.py` menulis `contracts/golden/error-*` dari Django/DRF asli (bukan mock), lalu `cargo test --test error_golden` membandingkan output axum byte-per-byte. Dataset ini mencakup 400 (tiga bentuk), 401, 403, 404 (Django + DRF), 405, 413, 415, dan 429.
- Gate `wire-parity` memblokir PR bila satu kasus berubah tanpa pembaruan golden yang disetujui.

---

### 8.8 Bukti 100%: satu matriks

Empat dimensi harus hijau bersamaan. Jika satu merah, klaim "identik" batal.

| Dimensi      | Sumber kebenaran                               | Artefak                                         | Gate                                                                                                                        | Gagal bila                                               |
| ------------ | ---------------------------------------------- | ----------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------- |
| URL & method | `get_resolver()` Django                        | `contracts/routes.json`                         | `route-parity`                                                                                                              | ada route hilang/ekstra, urutan beda, converter beda     |
| Parameter    | OpenAPI + filterset/serializer + request nyata | `contracts/params.json`                         | `param-parity`                                                                                                              | nama, tipe, default, atau penanganan nilai invalid beda  |
| Response     | request nyata ke Django                        | `contracts/golden/**`                           | `wire-parity`                                                                                                               | status, header, atau body beda di luar nilai volatil 8.7 |
| Migrasi      | file migrasi Django + DB hasil migrasi         | `manifest.json`, `pg_dump`, `django_migrations` | `migration-drift`, `schema-parity-fresh`, `schema-parity-stepwise`, `schema-parity-upgrade`, `ledger-parity`, `data-parity` | ada diff DDL, urutan, ledger, atau data `RunPython`      |

Aturan yang membuat "100%" bisa diuji:

- Semua artefak **di-generate**, tidak ditulis tangan. PR yang mengubah `contracts/**`/`migrations/**` tanpa perubahan sumber Django ditolak.
- Perbandingan **dua arah** (hilang maupun ekstra) untuk route, parameter, dan kolom.
- Tidak ada pengecualian baru; satu-satunya nilai yang boleh berbeda adalah daftar volatil di 8.7.
- Perubahan perilaku yang disengaja (bug fix) keluar dari PR migrasi: PR terpisah, disetujui eksplisit, golden diperbarui di PR itu.
- Modul tidak menerima traffic sebelum keempat gate hijau untuk modul itu.

---

## 9. Auth & Keamanan

### 9.1 Alur yang harus diport

| Alur                             | Endpoint contoh                                                   | Catatan                                           |
| -------------------------------- | ----------------------------------------------------------------- | ------------------------------------------------- |
| Email/password sign-in & sign-up | `/auth/sign-in/`, `/auth/sign-up/`                                | validasi password (zxcvbn di Django)              |
| Magic code                       | `/auth/magic-code/...`                                            | kirim kode + verifikasi, kode expire              |
| OAuth                            | `/auth/google/`, `/auth/github/`, `/auth/gitlab/`, `/auth/gitea/` | state cookie anti-CSRF                            |
| Sign-out                         | `/auth/sign-out/`                                                 | hapus session                                     |
| Password reset                   | view `password_management`                                        | email + signed token (port `django.core.signing`) |
| Workspace/project invite join    | `workspace_project_join.py`                                       | token invite, email                               |
| Admin login                      | `/auth/...` + cookie admin                                        | sesi terpisah                                     |

### 9.2 Rate limit

| Scope              | Rate                                    |
| ------------------ | --------------------------------------- |
| anon               | 30/menit                                |
| asset (`asset_id`) | 5/menit                                 |
| API key            | `API_KEY_RATE_LIMIT` (default 60/menit) |

Implementasi harus memakai Redis (multi-instance) dengan semantik sliding window seperti DRF `SimpleRateThrottle`, serta `NUM_PROXIES` untuk pengambilan IP dari `X-Forwarded-For`.

### 9.3 SSRF & konten

- Semua fetch keluar (webhook test, unfurl link) memakai padanan `plane/utils/ip_address.py` + `url_security.py`: resolve sekali, validasi semua IP, **pin ke IP** saat connect, redirect divalidasi per hop, tanpa auto-follow.
- Sanitasi HTML (`content_validator.py`, `html_processor.py`) harus menghasilkan whitelist tag/atribut yang sama; `ammonia` dikonfigurasi eksplisit.

### 9.4 Otorisasi

Hierarki:

- Workspace role: owner, admin, member, guest.
- Project role: admin, member, guest, viewer.
- Page: private/public + akses project.
- Deploy board: public anchor (workspace slug + project id) untuk space.

Padanan trait/permission object di Rust:

```rust
trait Permission { fn check(&self, actor: &Actor, ctx: &Ctx) -> Result<()>; }
```

Aturan izin harus tinggal di satu modul dan dipakai semua handler. Setiap kelas permission Django (`WorkspaceOwnerPermission`, `ProjectAdminPermission`, `ProjectPagePermission`, dst.) dipetakan 1:1 beserta urutan pengecekannya.

---

## 10. Background Job

### 10.1 Jadwal Beat (dari `plane/celery.py`)

| Jadwal (UTC)                                       | Job                                                                         |
| -------------------------------------------------- | --------------------------------------------------------------------------- |
| \*/5 menit                                         | `stack_email_notification` (kirim email notifikasi tertunda)                |
| \*/5 menit                                         | `unsnooze_expired_intake_issues`                                            |
| tiap `METRICS_PUSH_INTERVAL_MINUTES` (default 360) | `push_instance_metrics`                                                     |
| 00:00                                              | `hard_delete` (purge data soft-deleted)                                     |
| 01:00                                              | `archive_and_close_old_issues`                                              |
| 01:30                                              | `delete_old_s3_link` (exporter)                                             |
| 02:00                                              | `delete_unuploaded_file_asset`                                              |
| 02:30                                              | `delete_api_logs`                                                           |
| 02:45                                              | `delete_email_notification_logs`                                            |
| 03:00                                              | `delete_page_versions`                                                      |
| 03:15                                              | `delete_issue_description_versions`                                         |
| 03:30                                              | `delete_webhook_logs`                                                       |
| 03:45                                              | `delete_old_s3_link` (duplikat jadwal — pertahankan atau catat sebagai bug) |

Retensi dibaca dari env (`*_RETENTION_DAYS`); nilai default harus sama.

### 10.2 Task ad-hoc (31 modul)

Dikelompokkan:

- **Notifikasi & email**: `notification_task`, `email_notification_task`, `*_email_task`, `workspace_invitation_task`, `project_invitation_task`, `magic_link_code_task`, `forgot_password_task`.
- **Aktivitas & versi**: `issue_activities_task` (diff tiap field), `issue_version_sync`, `issue_description_version_*`, `page_version_task`, `page_transaction_task`.
- **Webhook**: `webhook_task` (HMAC-SHA256, header `X-Plane-Delivery`, `X-Plane-Event`, `X-Plane-Signature`, retry, `WebhookLog`).
- **Asset**: `copy_s3_object` (duplikasi deskripsi + aset), `file_asset_task`, `storage_metadata_task`, `duplicate-assets`.
- **Export/import/seed**: `export_task` (ZIP ke S3), `analytic_plot_export`, `dummy_data_task`, `workspace_seed_task`.
- **Pemeliharaan**: `cleanup_task`, `deletion_task`, `exporter_expired_task`, `recent_visited_task`, `logger_task`, `work_item_link_task` (unfurl).

### 10.3 Aturan implementasi job

1. **Idempoten**: setiap job harus aman dijalankan dua kali (retry/at-least-once).
2. **Payload sama**: key dan bentuk data job (yang dipakai untuk dedup/lock Redis) dipertahankan; lock Redis pakai pola `acquire_lock`/`release_lock` yang ada.
3. **Transaksi**: job yang menulis beberapa tabel memakai transaksi; status job dicatat agar bisa di-resume.
4. **Backpressure**: worker punya concurrency terpisah per jenis job (email vs export vs activity) untuk menghindari satu job berat memblokir notifikasi.
5. **Observability**: setiap job punya span + metrik durasi/error, dan log memuat `task`, `entity_id`, `workspace_id`.
6. **Enqueue setelah commit**: job dipublikasikan setelah transaksi Postgres `COMMIT`; job yang haram hilang punya tiket `PENDING` di tabel yang ada dan di-push ulang cron setelah 5 menit.
7. **Recovery Streams**: `XAUTOCLAIM` untuk pesan idle > 60 s, retry maksimal 3, lalu DLQ + `XACK` + status `FAILED` di DB.
8. **Batas memori**: `MAXLEN ~ 1000` pada setiap `XADD`, TTL pada semua key cache, `maxmemory-policy noeviction`, AOF `everysec` (detail di 11.6).
9. **Namespace Redis**: job Rust memakai DB index + prefix sendiri (`plane-rs:`); jangan menabrak DB 0 Django (`:1:` cache, lock `send_email_notif_*`).
10. **Sumber jadwal**: scheduler Rust membaca tabel `django_celery_beat_*` yang sudah ada (migrasi cermin tetap membuatnya); jangan menulis jadwal di kode dan jangan menjalankan Celery beat bersamaan (18.4 #4).

---

## 11. Storage, Email, Realtime, Public API

### 11.1 Asset / S3

Alur yang harus identik:

1. Klien minta presigned POST (`/api/workspaces/<slug>/file-assets/` atau `/assets/v2/...`) dengan `asset_id` + tipe/ukuran.
2. Server membuat presigned POST ke S3 dengan field & kebijakan yang sama (`Content-Type`, ukuran, key).
3. Klien upload ke S3.
4. Server memvalidasi metadata (`storage_metadata_task`, `AssetCheckEndpoint`), lalu menyimpan `FileAsset`.
5. Tampilan memakai presigned GET (`SIGNED_URL_EXPIRATION`, default 3600 detik).
6. Ada alur restore, duplicate (copy object), bulk, dan download.

Rust: `aws-sdk-s3` (`presigning`), dan pastikan **bentuk JSON** respons presigned sama (nama field, urutan tidak penting tapi key harus sama).

### 11.2 Email

- Template HTML ada di `apps/api/templates`. Pakai ulang file-nya dengan `minijinja` (variabel konteks harus sama), atau render di Rust dengan template baru yang diverifikasi secara visual.
- Pengiriman via SMTP (`EMAIL_HOST`, `EMAIL_PORT`, `EMAIL_HOST_USER`, `EMAIL_USE_TLS`, dst.).
- Notifikasi email dibatch tiap 5 menit + lock Redis, sesuai `email_notification_task` (`EmailNotificationLog` untuk dedup).

### 11.3 Realtime (`apps/live` -> `plane-live`)

Sepanjang fase 0-8 dan jendela cutover Fase 9 sebelum Node dimatikan, `apps/live` Node/Hocuspocus adalah oracle protokol/rollback dan tidak boleh diubah. Fase 9 (13.10) memindahkan server yang sama ke binary Rust `plane-live`. Kontrak yang harus dijaga:

- Path publik tetap `LIVE_BASE_PATH` + `/collaboration` (default `/live/collaboration`), beserta WebSocket upgrade, subprotocol Yjs/Hocuspocus, sync step 1/2, update, awareness/presence, stateless broadcast, dan reconnect.
- `plane-live` memanggil API Rust dengan **cookie user** yang diteruskan (`Cookie: session-id=...`) untuk `/api/users/me/` dan `/api/workspaces/<slug>/pages/<page-id>/live-access/`. Header `live-server-secret-key` hanya dibaca oleh middleware endpoint internal server live dan tidak diteruskan ke API.
- Endpoint HTTP live yang harus identik: `/health`, `/convert-document`, dan `/pdf-export`; respons error, status, header, dan body mengikuti `apps/live` saat ini.
- Persistensi dokumen: binary Yjs di `Description`/`Page` dibaca dan ditulis dengan format yang sama; HTML/JSON turunan tetap dihasilkan lewat jalur editor yang terverifikasi, bukan konversi ad hoc. Title sync dan page-access read-only wajib dipertahankan.
- Redis extension menyediakan fan-out antar-replica, lock per dokumen, dan kanal admin. Rust wajib memakai namespace/kanal yang sama atau adapter kompatibel yang diuji; Redis Pub/Sub tetap bukan sumber kebenaran dokumen.
- Aksi wajib: petakan seluruh handler, event, dan endpoint dari `apps/live/src/**` ke desain Rust. Jangan mengandalkan asumsi atau hanya mem-port jalur WebSocket.

### 11.4 Public space API

- `AllowAny`, scoping via deploy board (`ProjectDeployBoard` / `DeployBoard`), bukan slug workspace biasa.
- Form intake publik membuat issue di project tujuan; wajib anti-spam (rate limit per IP + honeypot/turnstile bila ada), dan tidak boleh membocorkan data project lain.
- Endpoint asset publik memakai presigned GET dengan validasi kepemilikan.

### 11.5 Instance admin (god mode)

- Prefix `/api/instances/`, cookie `admin-session-id`, akses hanya user instance-admin.
- Termasuk konfigurasi instance, license, health instance, dan telemetry push (`push_instance_metrics`).
- **Wajib parity ketat**: salah sedikit bisa mengunci akses admin.

---

### 11.6 Redis/Valkey: job, lock, cache, dan batasnya

Prinsip: **Postgres tetap sumber kebenaran**; Redis hanya transport, lock, dan cache yang boleh hilang. Pola di bawah diadaptasi dari panduan Redis-Rust yang sudah terbukti, disesuaikan dengan topologi Plane (Postgres 15 + Valkey 7.2.11 + `plane-live` Rust setelah cutover Fase 9 selesai).

#### Pilih fitur sesuai sifat data

| Kebutuhan Plane                                                                     | Fitur Redis                                  | Catatan                                                                                    |
| ----------------------------------------------------------------------------------- | -------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Export ZIP/CSV/XLSX, analytic plot, import/seed, `copy_s3_object`, sync versi batch | **Streams** (`XADD`/`XREADGROUP`/`XACK`)     | tahan restart worker, consumer group, ack eksplisit                                        |
| Notifikasi email batch, activity diff, webhook                                      | Streams, consumer group terpisah per kelas   | jangan Pub/Sub untuk job inti; harus bisa retry                                            |
| Rate limit, cache status/ACL, `last_used` API key                                   | String/Hash + TTL (`SETEX`, `INCR`+`EXPIRE`) | DB 0 milik Django; Rust memakai DB sendiri                                                 |
| Lock scheduler/email                                                                | `SET key value NX EX`                        | key harus sama dengan `acquire_lock`/`release_lock`                                        |
| Jawaban sinkron di tengah request                                                   | **bukan Redis**                              | panggil fungsi `domain`/`db` langsung (satu proses) atau HTTP internal bila dipisah proses |

#### Namespace dan kompatibilitas dengan Django

- Image repo: `valkey/valkey:7.2.11-alpine` (`docker-compose*.yml`). API Valkey kompatibel Redis 7, termasuk `XADD`/`XAUTOCLAIM`/`XINFO`.
- Django memakai **DB 0 secara eksplisit**: `apps/api/plane/settings/redis.py` memaksa `db=0`; cache `LOCATION=REDIS_URL` (django-redis memberi prefix `:1:`); lock email `send_email_notif_{issue_id}_{receiver_id}_{ids_str}` tanpa prefix, TTL 300 s.
- Rust wajib memakai **DB index terpisah** (`REDIS_QUEUE_DB=1`, `REDIS_CACHE_DB=2`, atau 2/3) plus prefix eksplisit `plane-rs:`. Dilarang menulis ke DB 0, memakai `FLUSHDB`/`FLUSHALL`, atau memakai key `send_email_notif_*` untuk hal lain.
- `REDIS_URL` (bisa `rediss://`) tetap env utama. Env internal Rust boleh ditambah, tetapi tidak boleh mengganti nama env yang sudah ada.
- Koneksi: pool `deadpool-redis` untuk `GET`/`SET`; **satu koneksi khusus** untuk `XREADGROUP ... BLOCK` dan Pub/Sub, karena koneksi blocking tidak boleh dipinjam dari pool.

#### Empat jebakan yang harus ditutup sejak awal

1. **Prefix/serializer**: Plane tidak menulis pickle ke Redis (Celery `json`, cache django-redis). Rust tetap mengirim **JSON murni** (`serde_json`), bukan biner. Setiap payload punya penanda versi `{"v": 1, ...}` dan fixture JSON di `contracts/` yang dipakai sebagai body request nyata di harness `wire-parity` (bukan unit test schema).
2. **Memori**: pasang `maxmemory` + `maxmemory-policy noeviction`; `allkeys-lru` bisa menghapus stream yang belum diproses. Setiap `XADD` memakai `MAXLEN ~ 1000`, setiap key cache wajib TTL.
3. **Persistence**: `appendonly yes` + `appendfsync everysec`; `vm.overcommit_memory=1` agar `BGREWRITEAOF` tidak gagal; `save ""` bila AOF aktif agar tulis SSD tidak dobel. Sisakan headroom RAM untuk lonjakan copy-on-write saat rewrite.
4. **Keamanan**: `bind` hanya localhost/jaringan privat, `requirepass`/TLS, tidak diekspos ke internet.

#### Konsistensi: enqueue hanya setelah commit

- `XADD`/`PUBLISH` dilakukan **setelah** transaksi Postgres `COMMIT`. Untuk job yang haram hilang, tulis tiket `PENDING` di tabel Postgres **yang sudah ada** (`ExporterHistory`, `EmailNotificationLog`, `WebhookLog`), lalu cron mendorong ulang tiket `PENDING` berumur > 5 menit. Ini menutup kasus "commit sukses, Redis putus".
- Jangan menambah tabel/kolom baru untuk outbox: pakai skema yang ada (aturan migrasi cermin di 7.1).
- Idempotensi memakai kunci natural (`export_id`, `issue_id+receiver_id+ids`, `webhook_id+event`), bukan timestamp acak.
- Bila Redis down: transaksi bisnis tetap sukses dengan tiket `PENDING`; kegagalan enqueue dicatat dan di-retry cron. Endpoint yang **hanya** bisa jalan lewat antrean mengembalikan 503 dengan bentuk error yang sama seperti Django saat broker down.

#### At-least-once: PEL, retry, DLQ

Streams tidak punya auto-retry/DLQ bawaan. Wajib di `worker`:

- `XGROUP CREATE <stream> <group> MKSTREAM` idempoten saat start.
- Task periodik `XAUTOCLAIM` untuk pesan idle > 60 s; baca delivery count.
- Gagal 1–2×: `XACK` lalu re-enqueue (dengan backoff); gagal 3×: pindah ke `<stream>:dlq`, `XACK`, dan tandai `FAILED` di DB agar tidak loop.
- Setiap job idempoten (aman dijalankan dua kali).
- Observability: metrik `XLEN`, `XPENDING`/`XINFO GROUPS` (lag), jumlah DLQ, durasi/error per job. Alert pada kenaikan lag/PEL/DLQ, bukan hanya saat worker mati.
- `apalis-redis` memakai key sendiri (`apalis:*`) dan melakukan polling; jangan dicampur dengan key cache. Retry + DLQ tetap harus di level aplikasi.
- Concurrency dipisah per kelas job (export vs email vs activity) agar export berat tidak menahan notifikasi.

#### Pub/Sub dan realtime

- Realtime Plane dimiliki `plane-live` setelah cutover Fase 9 selesai. Selama jendela cutover, Node tetap oracle/rollback dan tidak boleh diubah. Pub/Sub Redis dipakai untuk fan-out update Yjs, awareness, dan perintah admin antar-replica, bukan sebagai penyimpanan dokumen. Bila dipakai untuk sinyal UI, sifatnya fire-and-forget sehingga wajib ada fallback `GET` status dari Postgres.
- Pub/Sub tidak boleh dipakai untuk job inti: pesan hilang saat worker restart/deploy.

#### Saat Redis bermasalah

- Read path tetap jalan dari Postgres. Rate limit boleh fail-open (dengan log + metrik) atau fail-closed; putuskan di Fase 0 dan dokumentasikan. Auth API key tetap divalidasi ke DB, bukan hanya cache.
- Satu Valkey melayani cache + antrean = SPOF. Untuk produksi besar, pisahkan instance cache dan queue, atau siapkan replica/failover. Bila memakai Cluster, gunakan hash tag agar key satu stream/group berada di slot yang sama; operasi multi-key lintas slot tidak didukung.

---

## 12. Testing: real API, real data, tanpa mock

Sinyal kelulusan paritas adalah **request HTTP nyata ke stack nyata**, dibandingkan dengan Django pada data yang sama. Unit test internal tidak menggantikan gate HTTP, tetapi tetap wajib untuk logika domain, parser, serializer, dan perhitungan yang tidak bisa dibuktikan memadai hanya dari beberapa respons endpoint.

### 12.1 Aturan: hanya endpoint nyata

- Semua pengujian paritas dikirim lewat network ke binary `plane-api` (dan `plane-worker`) yang benar-benar berjalan. Bukan `tower::oneshot`, bukan pemanggilan handler/repository langsung.
- Satu-satunya unit analisis adalah **endpoint** (method + path) dari `contracts/routes.json`.
- Dilarang memakai mock/fake untuk Postgres, Redis/Valkey, S3, SMTP, webhook, OAuth, atau jam pada uji paritas HTTP. `httpmock`, wiremock, in-memory DB, dan repository palsu tidak dipakai sebagai pengganti stack nyata.
- Yang boleh berdiri sendiri hanya yang memang bukan HTTP: `schema-parity`, `ledger-parity`, `data-parity` (migrasi), unit test domain/parser/serializer, plus `clippy`, `rustfmt`, `cargo deny`.
- Efek samping diverifikasi lewat API nyata (GET lanjutan) atau SQL ke DB nyata; tidak lewat spy/mock.

### 12.2 Stack nyata

`docker compose` test menjalankan:

- `postgres:15.7-alpine` dengan template DB dari snapshot nyata,
- `valkey/valkey:7.2.11-alpine`,
- MinIO (S3 nyata),
- SMTP server nyata (mis. Mailpit) untuk memeriksa email,
- service HTTP terpisah sebagai receiver webhook/unfurl (bukan mock di dalam proses),
- `plane-api` + `plane-worker` Rust,
- `api` Django sebagai referensi (image yang sama dengan produksi),
- `apps/live` bila case menyentuh endpoint live.

Semua service memakai env yang sama dengan produksi (paritas config). OAuth diuji ke provider sandbox/test tenant nyata; bila provider tidak menyediakan sandbox, replay callback dengan state/token nyata dari sesi uji.

### 12.3 Data nyata

- Baseline: salinan **tersanitasi** database produksi/staging, di-restore ke Postgres. Bukan fixture beberapa baris.
- Minimum: 2 workspace (satu besar), 50+ project, 5.000+ issues, cycle/module/label/state, 20+ user dengan 4 role, page + versi, dan aset di MinIO.
- Setiap worker memakai clone DB sendiri (template + `CREATE DATABASE ... TEMPLATE`), Redis DB index sendiri, dan prefix MinIO sendiri, sehingga bisa paralel tanpa saling merusak.
- Data sintetis hanya untuk kasus tepi (unicode, body 413, UUID invalid, filter ekstrem) dan tetap dimasukkan lewat API/SQL nyata.
- Catat hash snapshot + versi seed di laporan; sanitasi PII sebelum snapshot disimpan sebagai artifact.

### 12.4 Skenario per endpoint

Matriks minimal per route: `anon/member/admin/guest/API-key` × `happy/400/401/403/404/405/413/415/429` × state data (kosong, 1 item, 1.000 item, soft-deleted, filter/pagination).

- **Read**: request identik ke Django dan Rust pada snapshot yang sama → bandingkan wire (8.7).
- **Write**: bandingkan respons, lalu GET lanjutan, lalu isi tabel nyata (pivot, audit, activity/version, soft delete).
- **Job**: picu lewat endpoint nyata, poll status lewat API, verifikasi email di inbox SMTP nyata, webhook di receiver nyata, objek di MinIO.
- **Upload**: multipart nyata ke MinIO dengan presigned POST/GET nyata (bukan URL palsu).

### 12.5 Replay traffic nyata

- Rekam request dari staging (setelah sanitasi) beserta respons Django.
- Replay ke Rust pada snapshot DB yang sama; bandingkan status/header/body byte-level.
- Untuk write, replay berurutan pada clone DB identik; bandingkan state akhir.
- Go/no-go: divergensi < 0,5% per endpoint dan nol pada 20 endpoint teratas.

### 12.6 Isolasi, normalisasi, laporan

- Normalisasi hanya nilai volatil sesuai 8.7 (ID/timestamp/token/signature/`Date`/kompresi), berbasis path.
- Header dibandingkan sebagai set; body byte-identik setelah normalisasi.
- Laporan per endpoint (bukan per fungsi): jumlah case, green/red, diff pertama, hash snapshot.
- Benchmark memakai load tester HTTP nyata (`k6`/`oha`) terhadap endpoint yang sama, bukan microbenchmark internal.

### 12.7 Gate CI

| Gate                                      | Isi                                                                                                                                                       |
| ----------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `api-e2e`                                 | boot stack nyata + jalankan seluruh case endpoint (Postgres/Valkey/MinIO/SMTP/receiver nyata)                                                             |
| `fe-unchanged`                            | `git diff --exit-code` untuk `apps/web`, `apps/admin`, `apps/space`, `apps/live`                                                                          |
| `fe-smoke`                                | build FE apa adanya + smoke browser terhadap Rust (login, issue, filter, upload, live, admin, public)                                                     |
| `route-parity` (target CI; checker lokal) | daftar route Rust = Django dua arah (method, path, converter, urutan); 404/405/trailing slash diuji                                                       |
| `param-parity`                            | spesifikasi parameter per route = Django dua arah (nama, tipe, default, invalid); kasus kosong/duplikat/array diuji lewat HTTP nyata                      |
| `route-owner` (target CI; checker lokal)  | tiap endpoint punya tepat satu owner (`rust`/`django`) dan status sah; `green` hanya untuk route yang benar-benar dilayani Rust, `waived` wajib beralasan |
| `wire-parity`                             | request→response mentah per route/kasus; header persis dan body byte-identik setelah normalisasi nilai volatil                                            |
| `state-parity`                            | efek samping write/job diverifikasi via GET lanjutan + SQL pada DB nyata                                                                                  |
| `replay`                                  | replay traffic staging tersanitasi; divergensi < 0,5% per endpoint                                                                                        |
| `rust-lint`                               | target CI: `fmt --check` + `check --all-targets` + `clippy -D warnings`, cache hangat < 3 menit                                                           |
| `migration-validate`                      | target CI: tiap migrasi di-apply ke Postgres kosong (`ON_ERROR_STOP=1`), checksum + ledger diperiksa                                                      |
| `gen-check`                               | target CI: regenerate `manifest.json`/`contracts/`; `git diff --exit-code` harus nol                                                                      |
| `clippy -D warnings` + `rustfmt --check`  | style                                                                                                                                                     |
| `cargo deny`                              | lisensi & CVE                                                                                                                                             |
| `image build`                             | multi-stage, distroless, healthcheck                                                                                                                      |
| `migration-drift` (target CI)             | target CI: `sha256` dan daftar migrasi baseline di `manifest.json` cocok dengan snapshot historis                                                         |
| `schema-parity-fresh`                     | target CI: DB kosong, Django vs Rust -> `pg_dump --schema-only` identik                                                                                   |
| `schema-parity-stepwise`                  | target CI: skema dibandingkan setelah tiap langkah migrasi, bukan hanya di akhir                                                                          |
| `schema-parity-upgrade`                   | target CI: snapshot rilis lama -> head, dua jalur identik                                                                                                 |
| `ledger-parity`                           | target CI: isi `django_migrations` sama                                                                                                                   |
| `data-parity`                             | target CI: tiap `RunPython`: snapshot tabel terdampak sama                                                                                                |
| `comment-coverage`                        | 100% file migrasi Rust berheader + komentar per statement                                                                                                 |
| `no-ad-hoc-ddl`                           | DDL hanya boleh ada di `apps/api-rs/migrations/**`                                                                                                        |
| `parity report`                           | ringkasan % endpoint green; PR diblokir bila modul yang disentuh turun                                                                                    |

Gate kontrak tidak diganti unit test: perilaku endpoint harus terlihat lewat HTTP nyata. Unit test tetap wajib untuk logika domain internal dan parser/serializer yang tidak bisa dibuktikan memadai hanya dari satu respons HTTP.

**Status implementasi gate:** tabel ini adalah target desain, bukan daftar CI yang sudah aktif. Repo saat ini belum memiliki workflow Rust. Satu-satunya workflow API yang berjalan adalah `.github/workflows/pull-request-build-lint-api.yml`, dan itu hanya menjalankan `ruff` untuk Django. `route-owner` dan `route-parity` sudah ada sebagai checker lokal (`contracts/tools/route_parity.py`) dan test kontrak (`contracts/tests/test_route_parity.py`), tetapi belum dijalankan oleh GitHub Actions dan belum menjadi required check. Hal yang sama berlaku untuk `migration-drift`, `rust-lint`, `migration-validate`, dan `gen-check`: statusnya **target sampai workflow Rust ditambahkan**. `apps/api-rs/justfile` baru menjalankan `fmt`, `check`, `clippy`, dan `test`; `migrate-validate`, `apps/api-rs/migrations/`, `tests/api_e2e.rs`, dan `scripts/verify-migrations.sh` belum ada. Gate baru boleh disebut required setelah workflow-nya dibuat dan terbukti berjalan di PR.

### 12.8 Frontend tanpa perubahan kode

Target: **nol diff** di `apps/web`, `apps/admin`, `apps/space`; `apps/live` tetap Node selama fase 0-8 dan jendela cutover Fase 9, dan juga tidak diubah.

- Gate `fe-unchanged`: `git diff --exit-code -- apps/web apps/admin apps/space apps/live` pada branch migrasi fase 0-8. Ada diff = PR gagal, kecuali perubahan itu bug fix lama yang dipisah dan disetujui eksplisit. Setelah Fase 8, perubahan backend hanya boleh ada di `apps/api-rs`; `apps/live` tetap oracle/read-only sampai cutover Fase 9 selesai, lalu menjadi arsip.
- Gate `fe-smoke`: build frontend apa adanya lalu jalankan smoke browser terhadap `plane-api` Rust: login (session/OAuth/magic code), buka workspace/project, list issue + filter + pagination, create/update issue, upload asset, load/save page lewat live, instance admin, public space.
- Bandingkan request browser (HAR/proxy) dengan versi Django: URL, method, header, body harus sama. Tidak ada env/flag baru untuk FE.
- Uji dua mode base URL: Mode 1 (same-origin, `VITE_API_BASE_URL=""`) dan Mode 2 (direct build-time ke Rust/Django); detail di 14.4.
- Jika smoke gagal, perbaiki di Rust atau routing Caddy—bukan di FE. Tooling test (mis. Playwright) ditempatkan di workspace test terpisah, bukan sebagai dependency/kode produk FE.

---

### 12.9 Fast feedback: menghindari build gagal karena syntax sepele

Prinsip: kesalahan sepele harus mati dalam hitungan detik di mesin lokal, bukan setelah 10 menit di CI. Tiga lapis: editor -> pre-commit -> CI fail-fast.

**Lapis 1 - Editor (milidetik sampai detik)**

- Pin toolchain di `rust-toolchain.toml` (channel + komponen `rustfmt`, `clippy`, `rust-src`); semua engineer dan CI memakai compiler yang sama. Versi berbeda = error yang tidak bisa direproduksi.
- rust-analyzer aktif dengan `checkOnSave` memakai `cargo check` (bukan `build`), plus clippy; error syntax/tipe muncul saat mengetik.
- `.vscode/settings.json` dibagikan: format on save, import on save, inlay hints; semua orang mendapat perilaku sama tanpa setup manual.
- `cargo check --all-targets` jauh lebih murah dari `cargo build --release`: tanpa codegen dan tanpa link. Ini perintah default saat iterasi; `build --release` hanya untuk image CI.
- `CARGO_INCREMENTAL=1` di dev, `sccache` untuk cache lintas branch/CI, dan linker cepat (`mold` di Linux, `lld` di macOS/Windows) lewat `.cargo/config.toml`.
- Pecah `apps/api-rs` menjadi crate kecil (domain, db, api, worker, migrate): `cargo check -p plane-api` hanya mengompilasi yang berubah, bukan seluruh workspace.

**Lapis 2 - Pre-commit (target < 10 detik pada cache hangat)**

- `.husky/pre-commit` saat ini hanya `pnpm lint-staged` (JS/TS). Tambahkan `scripts/rust-precommit.sh` yang hanya berjalan bila ada file `.rs`/`.sql`/`Cargo.toml` yang staged.
- Isi script berurutan, berhenti pada kegagalan pertama:
  - `cargo fmt --all -- --check` - paling murah, menangkap syntax rusak seketika.
  - `cargo check -q --all-targets` - incremental.
  - `cargo clippy -q --all-targets -- -D warnings` - boleh dipindah ke pre-push bila terasa lambat.
- `cargo run -q --bin plane-migrate -- --validate` - parse, urutan, dan checksum manifest; tidak menyentuh DB produksi. Perintah ini **target**, belum tersedia selama spike.
- Bila `cargo check` gagal, commit ditolak dengan pesan singkat `file:line:col` dan saran perbaikan. Tidak ada commit yang berisi kode yang tidak bisa dikompilasi.
- Pre-push menjalankan `cargo nextest run -p <crate>` hanya untuk crate yang berubah, bukan seluruh suite E2E.

**Lapis 3 - CI fail-fast (urutan biaya naik)**

| #   | Job                    | Biaya     | Isi                                                                                                                        |
| --- | ---------------------- | --------- | -------------------------------------------------------------------------------------------------------------------------- |
| 1   | `rust-lint`            | 2-3 menit | **Target:** `cargo fmt --check`, `cargo check --all-targets`, `clippy -D warnings`; cache `~/.cargo` + `target/` (sccache) |
| 2   | `migration-validate`   | paralel   | **Target:** apply tiap migrasi ke Postgres sementara `ON_ERROR_STOP=1`, cek checksum dan ledger                            |
| 3   | `gen-check`            | 1 menit   | **Target:** regenerate `manifest.json`/`contracts/routes.json`, lalu `git diff --exit-code` (harus nol)                    |
| 4   | `build`                | butuh 1-3 | **Target:** image multi-stage; hanya jalan bila lint dan validate hijau                                                    |
| 5   | `api-e2e` / `fe-smoke` | butuh 4   | **Target:** stack nyata + endpoint nyata                                                                                   |

- `needs:` wajib: job mahal tidak pernah mulai bila syntax, lint, atau migrasi sudah gagal.
- `cancel-in-progress: true` per PR: push terbaru membatalkan run lama, bukan menumpuk antrean.
- Cache key = hash `Cargo.lock` + versi toolchain; tanpa ini CI selalu dingin dan lambat.

**Guardrail file generated (sumber utama gagal sepele)**

- `apps/api-rs/migrations/**` dan `contracts/**` tidak boleh diedit tangan; hanya lewat generator `plane-migrate`/`xtask`. Setelah baseline dibekukan, PR yang mengubah snapshot historis tanpa audit ditolak; migrasi baru berasal dari pipeline Rust.
- Migrasi divalidasi dengan benar-benar di-apply ke database kosong (`plane-migrate --apply --database <temp>`), bukan hanya dibaca. Syntax SQL salah mati di sini, bukan di staging.
- `manifest.json` divalidasi terhadap JSON Schema sebelum dipakai; checksum `sha256` dihitung ulang dan dibandingkan.
- SQL di-lint (`sqlfluff`, atau `psql --single-transaction -v ON_ERROR_STOP=1`) dan output generator harus deterministik (urutan stabil, tanpa timestamp acak) agar `git diff` bersih.
- File Rust hasil generate memakai header `// @generated - jangan diedit`; review hanya pada template bila ada perubahan perilaku.

**Kebiasaan yang mencegah loop gagal**

- Satu PR = satu route/modul. Perubahan besar memperbesar blast radius dan memperlambat feedback.
- Setelah tiap unit logis jalankan `cargo check`, jangan menunggu 20 file selesai.
- Scaffold via `cargo xtask new-route <method> <path>` (menghasilkan handler, DTO, case kontrak, dan entri `routes.json`) supaya struktur tidak ditulis tangan.
- Tambah dependency hanya lewat `cargo add` dan `cargo deny check`, bukan edit `Cargo.toml` manual.
- Agent/AI (termasuk Codex) wajib menjalankan `just check` sebelum melaporkan tugas selesai; laporan tanpa bukti `just check` hijau dianggap belum selesai.
- `just check` saat ini menjalankan `fmt --check` -> `check --all-targets` -> `clippy -D warnings` -> `test --all-targets`. Target `migrate-validate` masih direncanakan dan belum ada di `justfile`.

**File wajib di repo**

- `rust-toolchain.toml` (pin compiler), `rustfmt.toml`, `clippy.toml` (level lint), `.cargo/config.toml` (linker + alias `cargo lint`/`cargo fast-check`), `.vscode/settings.json`, serta `justfile` atau `xtask` dengan target `check`, `fmt`, `lint`, `migrate-validate`, `e2e`.
- `scripts/rust-precommit.sh` + entri Husky; dokumentasikan di `apps/api-rs/README.md`.

**Target terukur**

- Edit -> error terlihat: < 2 detik (rust-analyzer).
- Commit -> diterima/ditolak: < 10 detik (cache hangat).
- Push -> job lint merah: < 3 menit.
- PR -> tahu bisa merge: < 15 menit (lint + validate + gen-check, tanpa menunggu E2E).

### 12.10 Dev loop: `cargo` native, Docker hanya untuk dependency

Aturan: **kode yang sedang diubah dijalankan native di host; service stateful dijalankan di container.** Jangan `cargo build` di dalam bind mount Docker saat iterasi.

| Komponen                                     | Saat dev                                    | Alasan                                                                                                         |
| -------------------------------------------- | ------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `plane-api`, `plane-worker`, `plane-migrate` | native (`cargo run`, `cargo watch`/`bacon`) | incremental compile, rust-analyzer, debugger, `target/` lokal                                                  |
| Postgres, Valkey, MinIO, Mailpit             | `docker compose`                            | versi harus sama produksi (`postgres:15.7-alpine`, `valkey/valkey:7.2.11-alpine`), tidak perlu install di host |
| Django `api` + `apps/live`                   | profil compose `parity`, hanya saat perlu   | referensi `wire-parity`, replay, cutover; bukan untuk edit harian                                              |
| FE (`apps/web` dll.)                         | native `pnpm dev`                           | HMR cepat; container tidak memberi nilai tambah                                                                |
| E2E / gate CI                                | compose penuh (12.2)                        | reproducibility, env bersih, bukti akhir                                                                       |

**Mode A - hybrid (default, paling cepat)**

```bash
# dependency saja; app tetap native
docker compose -f docker-compose-local.yml up -d postgres valkey minio mailpit
export DATABASE_URL='postgres://plane:plane@localhost:5432/plane'
export VALKEY_URL='redis://localhost:6379/0'
export S3_ENDPOINT_URL='http://localhost:9000'   # URL yang dilihat browser

cd apps/api-rs && cargo watch -x 'run --bin plane-api'
cd apps/api-rs && cargo watch -x 'run --bin plane-worker'
```

- Pakai port host berbeda dari Django (`plane-api` native di `:8001`, Django container di `:8000`) supaya keduanya bisa hidup bersamaan untuk `wire-parity`.
- `docker compose up -d` tanpa nama service hanya menyalakan dependency (beri `profiles: ["parity"]` pada Django/live agar tidak ikut start).
- `.env.local` digenerate dari `.env.example`, memakai nama env yang sama dengan produksi (18.1), dan tidak di-commit.

**Mode B - full container (bukti, bukan iterasi)**

- Dipakai untuk: `api-e2e`, `schema-parity`, replay, smoke FE, dan verifikasi akhir sebelum PR/milestone.
- Perintah: `docker compose -f docker-compose.test.yml up --build --abort-on-container-exit`.
- Build image 5-20 menit pada cache dingin; jangan dipakai setiap kali ganti satu baris.

**Kenapa full Docker buruk untuk loop edit**

- Bind mount `target/` melewati VM Docker = 3-10x lebih lambat, dan `cargo check` tetap membayar start container.
- rust-analyzer tidak bisa berbagi `target/` dengan container: kompilasi ganda, disk bisa >10 GB per developer.
- Attach debugger, breakpoint, dan crash loop jauh lebih repot; file watching `inotify` di path ter-mount sering tidak andal (terutama Docker Desktop di Windows/macOS).
- Container menyembunyikan perbedaan host (DNS, timezone, path, glibc) yang justru ingin Anda lihat cepat saat dev.

**Catatan khusus Windows (repo ini di `D:\projects\plane`)**

- Jangan taruh repo di OneDrive; exclude `D:\projects\plane\target` dan `apps/api-rs\target` dari real-time scan Defender, lalu pakai `sccache` (`RUSTC_WRAPPER=sccache`).
- Docker Desktop file sharing drive `D:` bisa lambat: cukup jalankan dependency di container, bukan source mount untuk build.
- `mold` tidak tersedia di Windows; biarkan linker MSVC default atau pakai `lld` lewat `.cargo/config.toml`. Untuk paritas Linux penuh, jalankan dev di WSL2 dengan repo di filesystem ext4 (bukan `/mnt/d`).
- Jika muncul error `openssl-sys`, pakai `vendored` atau TLS Rust (`rustls`) selama tidak mengubah wire behavior.

**Paritas config yang tetap wajib walau native**

- Versi Postgres/Valkey/MinIO sama dengan produksi; jangan "Postgres terbaru di laptop".
- Env var sama (nama dan arti), `TIME_ZONE`/locale sama, `DATABASE_URL` memakai schema yang sama.
- Presigned URL: host di signature harus bisa dijangkau browser (`localhost:9000`), bedakan dari host internal container (`minio:9000`) lewat `extra_hosts: host.docker.internal:host-gateway` bila perlu.
- Native Windows/Linux bukan bukti paritas: keputusan akhir tetap image container yang sama dengan produksi.

**Target & guardrail**

- Save -> error rust-analyzer < 2 detik; restart `plane-api` < 3 detik; `cargo check` per crate < 10 detik (cache hangat).
- `just dev` = dependency up + `plane-api` native; `just dev-down`; `just dev-reset` = hapus volume + restore snapshot; `just parity` = full compose.
- Agent/AI memakai Mode A untuk iterasi dan wajib menjalankan Mode B (`just parity`/`api-e2e`) sebelum menyatakan modul selesai.
- Bila bug hanya muncul di container, jalankan Mode B dan perbaiki penyebabnya (DNS/path/timezone), jangan mengubah perilaku produksi agar cocok dengan host.

### 12.11 Apakah hybrid sudah yang tercepat? (bottleneck & opsi)

Jawaban singkat: **hybrid native + Docker dependency adalah default tercepat untuk mayoritas developer**, tetapi bukan yang tercepat secara absolut. Yang menentukan adalah bottleneck mesin, bukan pilihan Docker-nya.

**Urutan bottleneck nyata (perbaiki dari atas)**

1. Rebuild seluruh workspace karena satu crate berubah -> pecah crate dan pakai `cargo check -p <crate>`.
2. Linker saat `cargo run` (bukan compiler) -> pakai `lld`/`mold`, `debug=1`, `split-debuginfo`.
3. Filesystem/antivirus (Windows) -> exclude `target/`, jangan taruh repo di OneDrive.
4. Docker Desktop VM (RAM/CPU) -> jalankan dependency saja, beri limit, jangan rebuild image.
5. rust-analyzer dan `cargo check` saling berebut `target/` -> satu target dir, `checkOnSave` saja; clippy pindah ke commit/CI.

**Profil Cargo paling cepat untuk dev**

```toml
[profile.dev]
opt-level = 0
debug = 1              # lebih cepat dari debug=2; cukup untuk breakpoint
incremental = true

# opsional: runtime query lebih cepat, tetapi build pertama lebih lama
# [profile.dev.package."*"]
# opt-level = 2
```

- `RUSTC_WRAPPER=sccache` untuk build dingin; `CARGO_INCREMENTAL=1` untuk build harian.
- Linker cepat: `mold` (Linux), `lld` (Windows/macOS) lewat `.cargo/config.toml`.
- Jangan `cargo clean` untuk "memperbaiki" error; itu membuang seluruh keuntungan incremental.

**Opsi yang lebih cepat dari rekomendasi saat ini**

| Situasi                                       | Pilihan tercepat                                                                       |
| --------------------------------------------- | -------------------------------------------------------------------------------------- |
| Laptop < 4 core / 8 GB, atau build > 60 detik | Remote dev box Linux 8-16 core (VS Code Remote/SSH); compile di sana, edit tetap lokal |
| Butuh versi Postgres/Valkey ketat             | Docker dependency (rekomendasi sekarang)                                               |
| Butuh latensi DB paling rendah                | Postgres/Valkey native di WSL2 ext4 (bukan `/mnt/d`)                                   |
| Ingin restart API < 1 detik                   | `bacon` + `cargo check` + `lld`, bukan `cargo watch` default                           |
| FE dan BE sering berubah bersamaan            | `pnpm dev` + `plane-api` native (bukan compose)                                        |
| Hanya mengubah satu modul                     | `cargo check -p plane-api` lalu `cargo run -p plane-api`                               |

**Yang tidak mempercepat (hindari)**

- `cargo build --release` saat iterasi.
- `docker compose up --build` setiap perubahan.
- Menjalankan clippy + check + test bersamaan pada setiap save.
- Rebuild dependency setiap ganti env atau branch.
- Dev di dalam container dengan bind mount `target/`.

**Cara mengukur, bukan menebak**

- `cargo check --timings` dan `Measure-Command { cargo check -p plane-api }` (PowerShell).
- Target: cache hangat < 10 detik per crate, restart < 3 detik. Bila > 30 detik, lihat `--timings`: biasanya proc-macro, `serde`, atau satu crate terlalu besar.
- Catat hasil di README tim; angka ini yang menentukan perlu remote builder atau tidak.

## 13. Rencana Fase (dengan Exit Criteria)

Estimasi dalam "orang-minggu" (pw) untuk engineer Rust berpengalaman; lihat §16. MVP (13.9) sengaja memotong Fase 0-3 dan tidak menunggu Fase 4-8.

### Fase 0 — Discovery & spike (2-3 minggu)

- Petakan seluruh endpoint → `contracts/endpoints.csv` + `contracts/routes.json` (method, path template, converter, urutan, matriks env). Baseline terukur: 391 route / 722 endpoint.
- Spike: (a) sesi Django-compatible, (b) password verify, (c) presigned POST S3, (d) query filter kompleks, (e) job queue semantics, (f) generator migrasi + `schema-parity` awal, (g) harness `wire-parity` (rekam request+response mentah).
- Putuskan Opsi A/B untuk sesi.
- **Exit**: 7 spike berjalan, estimasi modul final, keputusan 18.4 terbukti lewat spike, kontrak Django beku dan konsisten, `route-owner` hijau, `route-parity --allow-missing` hijau selama transisi, serta harness `wire-parity` bisa membandingkan satu endpoint penuh. Paritas route penuh menunggu fase port berikutnya.

### Fase 1 — Fondasi (3-4 minggu)

- Workspace Cargo, config loader (env parity), error mapping DRF, waktu/JSON serializer, logging JSON + request id.
- Middleware: cors, gzip, body limit, origin-check, rate limit, auth extractor.
- Parser request DRF-compatible (JSON/form/multipart, 415, unknown field diabaikan) + renderer ganda (DRF compact vs `JsonResponse` ber-spasi) + header keamanan/CORS identik.
- Repository base: pool RW tunggal, `Actor`, soft-delete helper, pagination, filter kompilator (awal).
- Migrasi: generator `manifest.json` + file `.sql` berkomentar + binary `plane-migrate` (plan/check/apply).
- Dev tooling fast feedback: `rust-toolchain.toml`, rust-analyzer, `just check` (fmt + check + clippy + test), script pre-commit Rust, cache sccache; target edit->error < 2 detik dan push->lint < 3 menit (lihat 12.9). Target `migrate-validate` masih direncanakan dan belum ada di `justfile`.
- Docker `Dockerfile.api-rs`, compose service, CI.
- **Exit**: `/` health, `/robots.txt`, dan satu endpoint read (`GET /api/workspaces/`) 100% `wire-parity` terhadap Django pada snapshot data nyata; `schema-parity-fresh` hijau.

### Fase 2 — Read parity modul inti (6-8 minggu)

- Workspace/member/project/issue/cycle/module/state/label/estimate/view/intake/page (GET).
- Filter + grouping + cursor + search.
- Shadow traffic di staging.
- **Exit**: seluruh GET modul inti `wire-parity` hijau pada snapshot data nyata; p95 (k6/oha ke API nyata) tidak lebih buruk 20%.

### Fase 3 — Write parity modul inti (6-8 minggu)

- Create/update/delete + soft delete + audit + pivot.
- Activity & versioning (issue activity, description version, page version) — hati-hati payload.
- Asset upload/duplikasi/restore.
- **Exit**: seluruh case write `wire-parity` + `state-parity` hijau di staging pada data nyata; uji beban tulis; rollback drill.

### Fase 4 — Auth, API v1, live integration (4-5 minggu)

- Session login, OAuth (4 provider), magic code, password reset, signout, admin session.
- API key + rate limit v1.
- Endpoint yang dipakai live server.
- **Exit**: login end-to-end dari web + live berjalan; sesi lama (Opsi A) atau migrasi logout (Opsi B) teruji.

### Fase 5 — Background job & bukan-HTTP (6-8 minggu)

- 13 jadwal beat + 31 modul task, idempotensi, retry, dead-letter.
- Email, webhook, notification, unfurl, export/import/seed.
- **Exit**: dua worker berjalan paralel (Django + Rust) tanpa duplikasi terkirim; metrik job setara.

### Fase 6 — Public space & instance admin (3-4 minggu)

- 25 endpoint public + instance admin + telemetry.
- **Exit**: intake publik & god mode `wire-parity` hijau; uji abuse/rate limit lewat HTTP nyata.

### Fase 7 — Cutover (2-3 minggu)

- Canary per workspace → per deployment.
- Matikan Celery beat Django, arahkan scheduler ke Rust.
- Sizing 2 vCPU/2 GB hanya boleh divalidasi setelah Django, Celery, dan RabbitMQ mati. Image dibangun di CI, aset memakai S3 eksternal, lalu soak 7 hari. Fase hybrid Django + Rust membutuhkan minimal 4 GB; stack produksi dan sandbox tidak dijalankan bersamaan di 2 GB (lihat 21).
- **Exit**: 7 hari tanpa rollback; error rate & latensi stabil.

### Fase 8 — Dekomisioning runtime Django (source tetap arsip; 1-2 minggu)

- Hentikan dan hapus service/container/process Django, Celery worker/beat, dan RabbitMQ, keluarkan dependency Python dari image produksi, serta hentikan job CI Python permanen. **Jangan hapus source `apps/api/**`\*\*; seluruh source dan test Python tetap disimpan sebagai arsip read-only yang tidak dibangun atau dijalankan.
- Source `apps/api/**` tidak menjadi fallback, route resolver, generator permanen, atau tempat menambah migrasi baru. Jangan ubah source atau test Django untuk menyesuaikan hasil Rust.
- Jangan hapus atau ubah file migrasi historis tanpa audit. Setelah baseline dibekukan, migrasi baru dimiliki Rust dan `plane-migrate` menjadi satu-satunya migration runtime produksi.
- Pensiunkan toolchain Python dan generator Django dari image produksi serta job CI permanen. Tool audit offline, bila dipertahankan, wajib punya exit plan; source arsip tetap ada.
- **Exit**: tidak ada service Django/Celery/RabbitMQ; `plane-api` + `plane-worker` + `plane-migrate` berjalan di produksi; source `apps/api/**` utuh sebagai arsip read-only; `schema-parity`/`ledger-parity`/`data-parity` hijau; dokumentasi (`AGENTS.md`) diperbarui.

---

### 13.9 Target MVP (milestone, bukan full parity)

MVP adalah potongan vertikal yang bisa dipakai **pilot nyata** tanpa mengubah FE, sementara Django tetap melayani sisanya. Ada dua gate: read-only shadow, lalu pilot write.

#### MVP-A — Read-only shadow (2-3 minggu)

Tujuan: membuktikan auth/session, koneksi DB, filter DSL, pagination, format respons, dan operasi produksi **tanpa risiko tulis**.

Endpoint allowlist:

- `GET /` dan `/robots.txt`
- `GET /api/users/me/`
- `GET /api/workspaces/`, detail, dan members
- `GET .../projects/` dan detail
- `GET .../states/`, `.../labels/`, `.../members/`
- `GET .../issues/` + detail (filter, grouping, cursor, search)
- `GET .../cycles/`, `.../modules/` (list/detail)
- `GET .../issue-comments/` (list)

Tidak termasuk: semua tulis, upload, export/import, page/live, public API, instance admin.

Mekanisme: Caddy mengirim persentase traffic GET allowlist ke Rust (shadow → canary), sisanya Django; DB shared.

Exit:

- `wire-parity` 100% untuk allowlist (0 diff selain nilai volatil).
- 72 jam produksi: 0 respons 5xx, error < 0,1%, p95 ≤ Django + 20%.
- Rollback < 1 menit; `fe-smoke` membaca data lewat Rust hijau.

#### MVP-B — Pilot write: core issue loop (5-6 minggu)

Scope:

- Semua read MVP-A.
- Tulis: issue create/update/delete (assignees/labels lewat pivot) dan komentar issue CRUD.
- Activity/version: tulis baris activity + description version identik, sinkron dalam transaksi.
- Email: tulis `EmailNotificationLog` identik; **Django Celery tetap mengirim** (`stack_email_notification`).
- Izin 4 role + non-member, rate limit, body limit, dan `Origin` check.

Non-goals MVP: webhook (tetap dilayani Django untuk workspace pilot), attachment/upload, export/import, page/live write, public space, instance admin, dan OAuth baru.

Mekanisme transisi:

- `contracts/routes.json` menandai owner per route (`rust`/`django`); Caddy hanya mengikuti peta itu.
- Tidak ada endpoint yang dilayani dua backend dan tidak ada dua scheduler untuk job yang sama.
- DB shared; worker Django tetap jalan untuk email dan job lain. Rust belum menjadi runner migrasi produksi di MVP (Django yang apply), tetapi file migrasi cermin tetap wajib ada.
- Sesi **Opsi A wajib** agar login Django tetap valid untuk Rust.

Exit:

- `wire-parity` + `state-parity` 100% untuk endpoint MVP (4 role × kasus error).
- Pilot 1 workspace, 10-20 user, 2 minggu: 0 data corrupt/duplikat; webhook tetap ditangani Django tanpa perubahan perilaku.
- p95 ≤ Django + 20%, error < 0,5%, 0 email hilang (bandingkan `EmailNotificationLog` dengan yang terkirim).
- Rollback drill: flip Caddy ke Django, data tetap terbaca, tidak ada job ganda.
- `fe-unchanged` + `fe-smoke` (baca + create/update issue) hijau.

#### Yang belum dibuktikan MVP

Full worker/scheduler Rust, webhook, export/import, attachment, page/live write, API v1 penuh, public space, instance admin, OAuth lengkap, dan `plane-migrate` sebagai runner produksi. Semua itu tetap di Fase 4-8.

#### Timeline MVP

- MVP-A: 2-3 minggu setelah Fase 1.
- MVP-B: 5-6 minggu setelah MVP-A.
- Total 8-9 minggu kalender (2 engineer Rust + 1 QA/contract + 0,5 DevOps), buffer 20%.

#### Gate tambahan

`route-owner`: setiap endpoint punya tepat satu owner selama transisi; sumbernya `contracts/route-owners.json`, statusnya `contracts/endpoint-status.json`. Route tanpa owner, owner ganda, route `rust` yang belum dilayani, `green` palsu, atau waiver tanpa alasan harus membuat PR gagal. **Status saat ini:** checker masih berupa tool kontrak lokal (`contracts/tools/route_parity.py`), belum menjadi required check di workflow Rust karena workflow tersebut belum ada. Keputusan ini menjadi target Fase 1, bukan klaim bahwa CI sudah aktif.

### 13.10 Fase 9 - Server live Node ke Rust `plane-live` (setelah cutover penuh, 4-8 minggu)

Dimulai setelah Fase 8 selesai, dengan runtime Django sudah tidak dipakai dan source `apps/api/**` tetap arsip read-only. `apps/live` Node/Hocuspocus **belum** menjadi arsip pada awal Fase 9: selama seluruh jendela cutover, Node tetap oracle protokol/rollback dan source-nya tidak boleh diubah. Fase 9 baru menjadikan **`plane-live` Rust satu-satunya server realtime produksi** setelah canary, soak, pengamatan, dan cutover dinyatakan selesai. Setelah exit, tidak ada runtime atau container Node untuk live, dan `apps/live/**` menjadi arsip read-only.

**Desain target**

- Crate baru `apps/api-rs/crates/live` menghasilkan binary `plane-live`, memakai kembali `pkg`, `db`, dan `domain`. Server HTTP/WebSocket memakai `axum` + `tokio`; CRDT memakai crate `yrs`, dengan codec pesan yang sudah diuji terhadap klien Yjs/Hocuspocus yang ada.
- Path, subprotocol, handshake/auth (`cookie` user untuk API; header `live-server-secret-key` hanya untuk endpoint internal live dan tidak diteruskan ke API), sync step 1/2, update, awareness/presence, stateless broadcast, dan reconnect wajib identik. Health, convert-document, dan PDF export dipertahankan dengan kontrak yang sama.
- Persistensi tetap ke tabel/kolom yang sama (`Description`/`Page`): binary Yjs, HTML, dan JSON harus dapat dibaca bolak-balik oleh server Node dan Rust. Title sync, page access/read-only, limit ukuran dokumen, fan-out Redis, lock per dokumen, dan drain saat shutdown wajib dipertahankan.
- Satu writer per dokumen selama cutover. Node dan Rust boleh hidup bersamaan sebagai replica saat uji, tetapi hanya satu yang menerima penulisan untuk dokumen tertentu. Redis channel/namespace diuji agar replica campuran tidak menggandakan update atau membuat lock yang tidak saling terlihat.

**Gate `live-parity`**

- Buka dokumen yang sama dari klien nyata ke Node dan Rust; bandingkan state akhir Yjs byte-identik, HTML/JSON turunan, title, presence, dan event stateless.
- Uji dua editor bersamaan, editor join/leave, reconnect, putus-sambung jaringan, reload dokumen, dokumen kosong, HTML lama, dokumen besar, dan percobaan tulis read-only.
- Uji auth: cookie valid/expired, user mismatch, page tanpa akses, `documentType` salah, dan secret internal salah. Status/close code/pesan error tidak boleh berubah.
- Uji dua replica Rust dan replica campuran Node+Rust: fan-out, lock, admin force-close, dan drain tanpa update hilang atau dokumen korup.
- Soak minimal 24 jam dengan beban representatif; ukur reconnect latency, error rate, RSS per koneksi, jumlah dokumen terbuka, dan pertumbuhan memori.

**Cutover dan rollback**

- Node tetap oracle sampai seluruh gate di atas hijau. Deploy `plane-live` sebagai canary per deployment/room dengan satu writer per dokumen; pantau divergensi dokumen, error simpan, dan reconnect.
- Rollback sebelum Node dihentikan: alihkan room/canary kembali ke Node, lalu hapus replica Rust. Setelah Node dihentikan, rollback hanya ke image `plane-live` Rust sebelumnya; jangan menghidupkan Node lagi.
- Setelah 7 hari produksi tanpa divergensi dokumen, reconnect < 2 s, dan memori per koneksi dalam anggaran 21.2, hentikan service/container Node, hapus dari compose, dan jadikan `apps/live/**` arsip read-only. Exit baru boleh diklaim setelah tidak ada proses Node live yang berjalan.

**Batas klaim:** Fase 9 tidak mengubah frontend dan tidak menghapus runtime SSR `apps/space`. "Tanpa Node" berarti tidak ada server live Node; frontend tetap memakai stack yang ada sampai ada proyek terpisah.

---

## 14. Cutover & Rollback

### 14.1 Strategi rute

```
Caddy: /api/*  ->  primary (Django)
                  fallback/shadow -> Rust
```

Caddy hanya memilih backend: **path tidak di-rewrite** (tanpa strip/add prefix), query dan header diteruskan apa adanya.

Bertahap:

1. Shadow (Rust menerima copy, response dibuang).
2. Canary by workspace (header/cookie) 1% → 10% → 50%.
3. Per-surface: `/api/public` → `/api/v1` → `/api` GET → `/api` write → `/auth` → `/api/instances`.
4. Full cutover per deployment.

### 14.2 Rollback

- **Selama transisi (sebelum Fase 8):** Caddy boleh dikembalikan ke Django dalam hitungan detik tanpa deploy. Simpan dua image Django terakhir sampai Fase 8. Syaratnya Rust tidak menulis data yang tidak bisa dibaca Django; skema tidak diubah dan format nilai mengikuti Django.
- **Setelah Fase 8:** Django, Celery, dan RabbitMQ tidak boleh dihidupkan lagi. Rollback hanya ke image Rust sebelumnya lewat redeploy tag, dengan migrasi yang sudah kompatibel dan `plane-migrate` sebagai runner tunggal.
- Job: pastikan tidak ada dua scheduler aktif bersamaan. Saat transisi, idempotensi + lock Redis mengurangi risiko tetapi salah satu scheduler tetap harus dimatikan. Setelah Fase 8, hanya scheduler Rust yang ada.
- Rollback tidak boleh menurunkan skema secara ad-hoc. Bila migrasi Rust sudah maju, image lama harus kompatibel dengan skema maju atau rollback memakai prosedur forward-fix yang diuji.

### 14.3 Decommission runtime Django: source tetap arsip, Rust memiliki runtime dan migrasi

Seluruh source `apps/api/**` dipertahankan sebagai arsip read-only dan tidak dihapus. Setelah baseline parity terbukti, source itu tidak dijalankan di production; Rust memiliki runtime serta migrasi baru.

- **`plane-migrate` menjadi satu-satunya runner produksi.** Menjalankan `apps/api-rs/migrations/**` sesuai `manifest.json` dan menulis ke `django_migrations`.
- **File Django hanya referensi/arsip** (`plane/db/migrations/**`, `plane/license/migrations/**`) untuk audit baseline dan checksum. File ini tidak dipakai sebagai input migrasi baru.
- **Migrasi baru dimiliki Rust.** PR migrasi menambahkan file Rust + manifest baru; tidak ada alur "Django menambah migrasi lalu CI menyalinnya".
- **Tidak ada dependency Python di image produksi.** Generator Django tidak boleh menjadi dependency build/runtime, tetapi source dan test Python tetap disimpan sebagai arsip read-only.
- **`migration-check.yml` lama dipensiunkan atau dipersempit** menjadi audit snapshot historis. Gate baru (`migration-validate`, parity, dan manifest check) **ditargetkan** dijalankan oleh workflow Rust; sampai workflow itu ada, statusnya belum required check.
- **Generator Django hanya boleh bertahan sebagai tool audit offline sementara**, dengan exit plan yang jelas; bukan job CI permanen.

Yang tidak boleh:

- Mengedit file migrasi Django historis, termasuk menambah komentar di sana.
- Menggabung baseline historis tanpa persetujuan eksplisit dan audit parity.
- Memakai ledger selain `django_migrations`.
- Memakai `sqlx migrate`/`refinery` dengan format migrasi sendiri.

---

### 14.4 Switch base URL FE (tanpa ubah kode)

FE sudah menyediakan titik ganti: `VITE_API_BASE_URL` di `apps/web|admin|space/.env.example`, dibaca `packages/constants/src/endpoints.ts`, dan diterima sebagai `ARG VITE_API_BASE_URL=""` di ketiga Dockerfile. `APIService` memakai `withCredentials: true`. Jadi berpindah Django ↔ Rust tidak memerlukan perubahan kode FE.

| Mode                                   | Cara set                                                                         | Rebuild FE?       | Kapan dipakai                                        |
| -------------------------------------- | -------------------------------------------------------------------------------- | ----------------- | ---------------------------------------------------- |
| **1. Same-origin via Caddy (default)** | `VITE_API_BASE_URL=""` + peta owner di `apps/proxy/Caddyfile.ce`                 | Tidak             | produksi, canary per route, rollback detik           |
| **2. Direct build-time**               | `--build-arg VITE_API_BASE_URL=https://api-django...` atau `https://api-rust...` | Ya                | uji isolasi backend, cutover/rollback per deployment |
| **3. Local dev**                       | ubah `.env`: `http://localhost:8000` (Django) / `http://localhost:8001` (Rust)   | Restart Vite saja | pengembangan                                         |

Mode 1 (disarankan). FE tetap relatif, Caddy yang memilih backend per route:

```
# apps/proxy/Caddyfile.ce — matcher spesifik HARUS sebelum /api/*
handle /api/workspaces/* { reverse_proxy {$RUST_API_UPSTREAM:api-rs:8000} }
handle /api/*            { reverse_proxy {$API_UPSTREAM:api:8000} }
handle /auth/*           { reverse_proxy {$API_UPSTREAM:api:8000} }
```

- Potongan di atas ilustrasi; allowlist sebenarnya per endpoint diambil dari `contracts/route-owners.json` (gate `route-owner`).
- Ganti `API_UPSTREAM` ke `api-rs:8000` = full cutover; kembalikan ke `api:8000` = rollback.
- `caddy reload` berlaku dalam hitungan detik, tanpa rebuild image FE.
- Path tidak di-rewrite; kontrak URL tetap sama (8.0).

**Guardrails**

- Produksi default `VITE_API_BASE_URL=""` agar cookie `session-id`/`admin-session-id` tetap same-origin.
- Mode 2 hanya aman bila: host cookie sama atau `COOKIE_DOMAIN` mencakup keduanya, `CORS_ALLOWED_ORIGINS` memuat origin FE, dan HTTPS aktif. Untuk cross-site perlu `SameSite=None; Secure`.
- `VITE_API_BASE_URL` harus sama untuk web, admin, dan space dalam satu deployment; beda nilai = sesi terpecah.
- `API_BASE_URL` di `deployments/**` adalah env backend/internal, bukan pengganti `VITE_API_BASE_URL`.
- Jangan menambah runtime `config.js` sekarang: itu perubahan kode FE. Bila nanti dibutuhkan, buat PR terpisah dan keluar dari aturan nol diff.

**Verifikasi**

- Build dua kali (kosong dan URL eksplisit), lalu pastikan bundle memuat nilai yang diharapkan.
- `fe-smoke` dijalankan pada Mode 1 dan Mode 2 minimal sekali per rilis.
- Uji preflight CORS dari origin web/admin/space dan round-trip sesi: login di Django → baca/tulis lewat Rust dengan cookie yang sama.

**Rollback**

- Mode 1: ubah `API_UPSTREAM` + `caddy reload` (< 5 detik).
- Mode 2: redeploy image FE sebelumnya (menit); kedua host API tetap hidup selama transisi.

---

## 15. Risiko & Mitigasi

| #   | Risiko                                                      | Dampak                               | Mitigasi                                                                                                                                                                                                    |
| --- | ----------------------------------------------------------- | ------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Sesi Django tidak kompatibel                                | Semua user logout / live putus       | Spike fase 0; Opsi A atau umumkan logout terkontrol                                                                                                                                                         |
| 2   | Hash password beda                                          | Login gagal                          | Port `pbkdf2_sha256`, baca iterasi dari hash, uji dengan hash produksi tersanitasi                                                                                                                          |
| 3   | Filter DSL salah                                            | Saved view rusak, data tampil salah  | Replay request nyata per operator pada data nyata; jangan refactor DSL                                                                                                                                      |
| 4   | Payload activity/versioning beda                            | Feed aktivitas rusak, riwayat hilang | Bandingkan JSON activity per jenis perubahan field (diff test)                                                                                                                                              |
| 5   | Soft delete tidak cascade                                   | Data "hantu" muncul / hilang         | Uji dengan snapshot; port daftar relasi eksplisit                                                                                                                                                           |
| 6   | Dua scheduler aktif                                         | Email/webhook ganda                  | Lock Redis + rollout scheduler terkoordinasi                                                                                                                                                                |
| 7   | N+1 di list issue                                           | Latensi naik, DB terbebani           | Batch query, benchmark vs Django dengan data nyata                                                                                                                                                          |
| 8   | Format waktu/`null` berbeda                                 | UI crash halus (salah parse)         | `wire-parity` per field pada API nyata, serializer kustom                                                                                                                                                   |
| 9   | SSRF/HTML sanitize lebih longgar                            | Kerentanan keamanan                  | Port aturan keamanan 1:1, security review, uji negatif                                                                                                                                                      |
| 10  | Realtime (live) butuh endpoint yang terlewat                | Halaman kolaboratif gagal simpan     | Peta endpoint live di fase 0                                                                                                                                                                                |
| 11  | Tim Rust belum paham domain Plane                           | Salah tafsir aturan bisnis           | Pairing dengan maintainer; domain crate + dokumentasi perilaku                                                                                                                                              |
| 12  | AGPL & header hak cipta                                     | Masalah legal/CI                     | Pertahankan lisensi & header; tambahkan header ke file baru (perluas CI bila perlu)                                                                                                                         |
| 13  | `allkeys-lru`/OOM menghapus stream                          | Job hilang tanpa error               | `noeviction`, `MAXLEN ~ 1000`, TTL, alert memori, tiket `PENDING` di Postgres                                                                                                                               |
| 14  | Enqueue sebelum commit / Redis down                         | Job hantu atau job hilang            | Enqueue setelah commit + cron re-push tiket `PENDING`; 503 terdefinisi                                                                                                                                      |
| 15  | FE diam-diam bergantung perilaku tak terdokumentasi         | Fitur rusak saat cutover             | `wire-parity` + `fe-smoke`; perbaiki di Rust, diff FE harus nol                                                                                                                                             |
| 16  | VPS 2 vCPU/2 GB OOM saat list issue besar atau worker burst | API/worker restart, job gagal        | 2C2G hanya untuk Rust penuh setelah runtime Django/Celery/RabbitMQ dihentikan; fase hybrid wajib 4 GB. Batasi pool/concurrency, pasang swap + earlyoom, pakai S3 eksternal, soak 7 hari sebelum klaim lulus |

---

## 16. Estimasi Effort & Tim

Asumsi: 1 engineer Rust senior ≈ 300-500 baris kode produksi/hari untuk paritas (lebih lambat dari greenfield karena harus mencocokkan perilaku), plus review/test.

| Area                                | Perkiraan                     |
| ----------------------------------- | ----------------------------- |
| Fondasi & infra                     | 4-6 pw                        |
| Migrasi cermin + generator/verifier | 5-7 pw                        |
| Read parity modul inti              | 10-14 pw                      |
| Write parity + activity/versioning  | 12-16 pw                      |
| Auth + API v1 + live                | 8-10 pw                       |
| Job + email + webhook + export      | 12-16 pw                      |
| Public + admin                      | 5-7 pw                        |
| Cutover + dekomisi                  | 5-7 pw                        |
| Test harness & kontrak              | 8-10 pw (paralel, tapi wajib) |
| **Total**                           | **±69-93 pw**                 |

Dengan 4 engineer + 1 QA/contract engineer: **6-8 bulan kalender** (dengan buffer 20%).

**MVP (13.9) bukan tambahan di atas total**: ia memakai pekerjaan Fase 0-3. Target kalender: MVP-A 2-3 minggu setelah Fase 1, MVP-B 5-6 minggu setelahnya. Full parity tetap 6-8 bulan. Fase 9 (live ke Rust, 13.10) berada **di luar total di atas**: perkiraan 4-8 pw dan baru mulai setelah Fase 8.

Tim minimum yang realistis:

- 2 engineer Rust (core domain + query),
- 1 engineer Rust (auth/infra/job),
- 1 engineer full-stack untuk harness & integrasi frontend/live (**tanpa mengubah kode FE**),
- 1 reviewer domain (maintainer Plane) part-time,
- 1 DevOps part-time untuk cutover/observability.

---

## 17. Definition of Done

Sebuah modul dianggap selesai bila **semua** terpenuhi:

- [ ] Semua endpoint modul terdaftar di `contracts/endpoints.csv` dan berstatus `green` atau `waived` dengan alasan tertulis.
- [ ] Case endpoint (read + write) lulus `wire-parity` pada stack nyata + snapshot data nyata.
- [ ] `git diff --exit-code -- apps/web apps/admin apps/space apps/live` hijau (nol perubahan FE/live).
- [ ] Smoke browser frontend apa adanya terhadap Rust hijau (login, issue, filter, upload, live, admin, public).
- [ ] Efek samping diverifikasi lewat GET API lanjutan + SQL pada DB nyata (pivot, soft delete, audit, activity/version, job terkirim).
- [ ] Izin diuji untuk 4 peran (owner/admin/member/guest) + non-member.
- [ ] Rate limit & body limit berlaku.
- [ ] Log & metrik muncul dengan field yang sama.
- [ ] Benchmark p95 (k6/oha ke API nyata) ≤ Django + 20%.
- [ ] Dokumentasi modul (aturan bisnis non-obvious) ditulis di `docs/` atau doc-comment crate.
- [ ] Tidak ada query tanpa filter workspace/project (uji isolasi workspace).

Proyek dianggap selesai bila:

- [ ] 100% endpoint green atau `waived` dengan persetujuan tertulis.
- [ ] 7 hari produksi tanpa rollback.
- [ ] Tidak ada service/runtime Django, Celery worker/beat, atau RabbitMQ. Seluruh source `apps/api/**` tetap utuh sebagai arsip read-only, termasuk test Python dan file migrasi historis.
- [ ] Image produksi tidak memiliki dependency Python; `plane-migrate` adalah satu-satunya runner migrasi production.
- [ ] `schema-parity`, `ledger-parity`, dan `data-parity` hijau terhadap baseline historis yang dibekukan.
- [ ] `AGENTS.md`, `CONTRIBUTING.md`, `docker-compose*.yml`, dan dokumen migrasi diperbarui.
- [ ] Runbook operasional (job, queue, rollback) tersedia.
- [ ] Untuk proyek penuh termasuk realtime: Fase 9 (live Node ke `plane-live` Rust, 13.10) selesai, tidak ada service/container Node live, dan gate `live-parity` beserta soak-nya hijau. Untuk DoD backend API setelah Fase 8, Fase 9 tetap proyek lanjutan dengan gate sendiri.

---

## 18. Lampiran

### 18.1 Env var yang harus dipertahankan

Database: `DATABASE_URL` / `POSTGRES_*`, `ENABLE_READ_REPLICA`, `DATABASE_READ_REPLICA_URL`. Read replica tidak dipakai: `ENABLE_READ_REPLICA` harus `0`/unset dan Rust menolak start bila `1` (7.5).
Redis: `REDIS_URL`, `REDIS_HOST`, `REDIS_PORT`, `REDIS_SSL*`.
Redis internal Rust (baru): `REDIS_QUEUE_DB` (default 1), `REDIS_CACHE_DB` (default 2), `REDIS_PREFIX` (default `plane-rs:`). Selama transisi, DB 0 memuat key legacy Django (cache `:1:` + lock `send_email_notif_*`) dan tidak dibaca oleh Rust setelah Fase 8. Setelah dekomisi, tidak ada proses Django yang memiliki key tersebut; pembersihan key legacy dilakukan sebagai langkah migrasi tersendiri.
Queue: selama Django/Celery aktif, broker tetap `RABBITMQ_*` / `AMQP_URL` (atau `CELERY_BROKER_URL` pada konfigurasi lama). Worker Rust target membaca `REDIS_URL` + `REDIS_QUEUE_DB` (default 1) dan `REDIS_PREFIX` (default `plane-rs:`). Jangan mengganti broker Django sebelum worker dan beat ikut pindah. Setelah Fase 8, hanya Redis Streams yang dipakai.
Storage: `AWS_*`, `USE_MINIO`, `MINIO_ENDPOINT_URL`, `MINIO_ENDPOINT_SSL`, `SIGNED_URL_EXPIRATION`, `FILE_SIZE_LIMIT`.
Auth/Web: `SECRET_KEY`, `SESSION_COOKIE_*`, `COOKIE_DOMAIN`, `ADMIN_SESSION_COOKIE_NAME`, `CORS_ALLOWED_ORIGINS`, `WEB_URL`, `APP_BASE_URL`, `ADMIN_BASE_URL`, `SPACE_BASE_URL`, `LIVE_BASE_URL`, `NUM_PROXIES`, `API_KEY_RATE_LIMIT`.
Frontend (build-time, kode tidak diubah): `VITE_API_BASE_URL` (default `""` = same-origin), `VITE_WEB_BASE_URL`, `VITE_ADMIN_BASE_URL`, `VITE_SPACE_BASE_URL`, `VITE_LIVE_BASE_URL` + base path masing-masing.
Email: `EMAIL_HOST`, `EMAIL_PORT`, `EMAIL_HOST_USER`, `EMAIL_HOST_PASSWORD`, `EMAIL_USE_TLS`, `EMAIL_FROM`.
OAuth: kredensial Google/GitHub/GitLab/Gitea + redirect URL.
Lain: `ENABLE_DRF_SPECTACULAR`, `METRICS_PUSH_INTERVAL_MINUTES`, `*_RETENTION_DAYS`, `LOG_LEVEL`.

**Aturan**: file `.env` dan `setup.sh` harus tetap kompatibel; jangan mengganti nama env var.

**Fail-fast Rust (implementasi `crates/pkg/src/config.rs`)**: `SECRET_KEY` wajib diisi; Django akan membuat key acak saat kosong, yang diam-diam membatalkan session, token reset password, dan invite link setiap restart, jadi Rust menolak start dengan `ConfigError::Missing`. `ENABLE_READ_REPLICA=1` juga ditolak (`ConfigError::Unsupported`) karena Rust hanya punya satu pool read-write (7.5); nama `POSTGRES_READ_REPLICA_*` tetap ada di tabel env supaya `.env` utuh terpetakan, tetapi nilainya tidak dibaca. Nilai lain mengikuti parsing Django apa adanya: boolean dibandingkan `== "1"` (kecuali `USE_MINIO` dan `DEBUG` yang lewat `int()`, jadi `DEBUG=release` gagal seperti di Django), `ALLOWED_HOSTS` dipecah pada koma tanpa trim, dan `TIME_ZONE` adalah konstanta `UTC`. Secret (`SECRET_KEY`, `*_PASSWORD`, `*_SECRET`, `AWS_*KEY*`) diredaksi oleh `Secret`; satu-satunya snapshot env yang boleh di-log adalah `Config::redacted(&ProcessEnv)`.

**Bukti parity helper (tanpa Docker/Django)**: `contracts/tools/capture_config.py` mengeksekusi fungsi asli `_cors_settings`, `_cookies_secure`, `_redis_ssl_kwargs`, `_retention_days`, dan `is_valid_url` yang diambil dari `apps/api/plane/settings/common.py` + `apps/api/plane/utils/url.py` memakai `ast` (tanpa import Django), lalu menulis `contracts/golden/config-helpers.json`. Test `config::tests::every_recorded_helper_case_matches` memutar ulang seluruh baris golden itu, sehingga port tidak bisa melenceng diam-diam. Tiga koreksi yang lahir dari pembacaan statis ini: `CORS_ALLOWED_ORIGINS` tidak di-dedupe Django dan `_cookies_secure` membaca daftar _terkonfigurasi_ (bukan hasil fallback base URL), `ssl_cert_reqs` di query `REDIS_URL` menang atas `REDIS_SSL_CERT_REQS` dan ikut mematikan `ssl_check_hostname` saat `none`, serta `is_valid_url` hanya menuntut skema + netloc non-kosong (jadi `https://:8080` valid, `mailto:x@y.test` tidak).

### 18.2 Daftar endpoint (baseline)

Angka di bawah adalah **route**, bukan endpoint. Sumber: `contracts/routes.json` (dibekukan, `dump_routes.py`).

`/api` (session): 234 route / 442 endpoint - workspace, issue, project, asset, user, cycle, analytic, module, page, intake, notification, views, estimate, state, webhook, external, search, timezone, exporter. Rincian modul dicatat di `endpoints.csv` agar angka total tidak dihitung ulang secara manual.

`/api/v1` (API key): 79 route / 178 endpoint - work_item, cycle, module, member, asset, project, estimate, schema, intake, label, state, invite, sticky, user.

`/api/public`: 25 route / 42 endpoint. `/api/instances`: 14 route / 21 endpoint. `/auth`: 37 route / 37 endpoint. `web`: 2 route / 2 endpoint (`/`, `/robots.txt`). Total **391 route / 722 endpoint**.

Rincian per method dan path template ada di `contracts/endpoints.csv`; jangan menghitung ulang dengan `grep` karena prefix seperti `api/assets/v2/` adalah literal, bukan versi.

### 18.2.1 Environment capture (Docker lokal)

Baseline `contracts/` dihasilkan dari stack `deployments/cli/community` di WSL. Tiga hal yang perlu diketahui agar capture bisa diulang:

- `docker cp contracts/tools/dump_routes.py community-api-1:/tmp/` lalu `python manage.py shell -c "exec(open('/tmp/dump_routes.py').read())"`. Container hanya me-mount `apps/api`, bukan root repo, jadi file harus disalin.
- `contracts/tools/capture-internal.sh` memanggil `127.0.0.1:8000` **di dalam** container, sehingga tidak bergantung pada Caddy, container `web`, atau cookie jar yang sudah ada. Login dilakukan lewat `/auth/sign-in/` dan redirect 302 direkam, bukan diikuti (tujuan redirect `APP_DOMAIN` tidak bisa diakses dari dalam container).
- Jika `docker ps` menunjukkan container baru saja restart, tunggu sampai `GET /api/users/me/` mengembalikan 401. Database butuh ~40-70 detik dan container `api` ikut restart ketika `plane-db` belum siap; 502 saat warm-up bukan kegagalan capture.

Build dan test Rust **tidak** membutuhkan Docker: `cargo build`, `cargo test`, dan `wire_golden` memakai golden yang sudah dibekukan.

### 18.3 Perintah kerja yang disarankan

```bash
# daftar endpoint dari Django (sekali jalan, simpan ke contracts/)
cd apps/api && uv run python manage.py spectacular --file ../../contracts/openapi-django.json

# rekam golden wire dari API Django yang benar-benar berjalan (bukan pytest/unit).
# Perencanaan: binary wire-record belum ada; sementara rekaman memakai
# contracts/tools/capture-*.py yang sudah menghasilkan contracts/golden/.
cd apps/api-rs && cargo run --bin wire-record -- --base-url http://localhost:8000 --cases ../../contracts/cases

# migrasi: generate lalu verifikasi
cd apps/api-rs && cargo run --bin plane-migrate -- --plan
cd apps/api-rs && cargo run --bin plane-migrate -- --check
# Direncanakan, belum ada: ./scripts/verify-migrations.sh (schema/ledger/data parity)

# dev loop cepat: dependency di Docker, app native (lihat 12.10)
docker compose -f docker-compose-local.yml up -d postgres valkey minio mailpit

# Rust
cd apps/api-rs && just check            # fmt + check + clippy + test (lihat 12.9)
# Direncanakan, belum ada: just migrate-validate
# Direncanakan, belum ada: cargo nextest run --test api_e2e
cd apps/api-rs && cargo run --bin plane-api
cd apps/api-rs && cargo run --bin plane-worker
```

### 18.4 Keputusan yang sudah dikunci

Keputusan berikut adalah baseline. Mengubahnya butuh PR terpisah yang memperbarui dokumen ini beserta artefak terkait (`contracts/`, gate, compose, runbook).

| #   | Keputusan                                                                                                                                     | Konsekuensi                                                                                                                                                                                                                                                                                                                                                                              |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Sesi: **Opsi A** (kompatibel penuh dengan Django)                                                                                             | user tidak logout saat cutover; Django dan Rust membaca/menulis cookie yang sama selama transisi                                                                                                                                                                                                                                                                                         |
| 2   | Queue: **Redis Streams** (+ `XAUTOCLAIM`, retry, DLQ) sebagai target worker Rust                                                              | RabbitMQ + Celery tetap hidup selama Django melayani job. Redis Streams baru aktif bersama worker Rust; service Celery worker, Celery beat, dan RabbitMQ dihentikan bersamaan setelah tidak ada task Django yang membutuhkan broker. Source Django tetap arsip read-only. Satu consumer per job, tidak ada job dobel                                                                     |
| 3   | `apps/live`: **Node/Hocuspocus selama fase 0-8 dan jendela cutover Fase 9**, lalu diganti binary Rust `plane-live` (13.10)                    | selama fase tersebut Node adalah oracle/rollback protokol dan Rust menyediakan endpoint yang dipanggilnya; setelah cutover Fase 9 selesai tidak ada runtime/container Node live, source `apps/live/**` menjadi arsip read-only, dan frontend SSR `apps/space` tidak termasuk perubahan ini                                                                                               |
| 4   | Scheduler: tabel `django_celery_beat_*` tetap sumber kebenaran, dibaca **scheduler Rust**                                                     | jadwal tetap bisa diubah di DB seperti hari ini; tidak ada cron yang menduplikasi config dan tidak ada Celery beat kedua                                                                                                                                                                                                                                                                 |
| 5   | `/api/schema/*`: **mati by default** (`ENABLE_DRF_SPECTACULAR=0`), boleh diaktifkan **per pelanggan** (`=1`); saat aktif isinya harus identik | mengikuti default Django; `route-parity` dan OpenAPI diff diuji untuk kedua nilai env                                                                                                                                                                                                                                                                                                    |
| 6   | Read replica: **tidak dipakai**                                                                                                               | Rust satu pool RW; startup gagal bila `ENABLE_READ_REPLICA=1` (7.5)                                                                                                                                                                                                                                                                                                                      |
| 7   | Tenant: **tidak ada** (satu deployment, satu schema)                                                                                          | lihat 7.7                                                                                                                                                                                                                                                                                                                                                                                |
| 8   | Deployment: satu instalasi self-host per deployment; sandbox = deployment terpisah, host sendiri, tag sama                                    | lihat 21.5                                                                                                                                                                                                                                                                                                                                                                               |
| 9   | Git: `preview` + tag rilis, tanpa branch `prod`/`dev`                                                                                         | lihat 23                                                                                                                                                                                                                                                                                                                                                                                 |
| 10  | Backend production: **Rust only** setelah Fase 8, termasuk live setelah cutover Fase 9 selesai                                                | `plane-api`, `plane-worker`, dan `plane-migrate` menjadi satu-satunya runtime backend setelah Fase 8; `plane-live` menjadi satu-satunya server realtime setelah cutover Fase 9 selesai. Source `apps/api/**` dan `apps/live/**` tetap arsip read-only, tanpa fallback permanen, route `owner=django`, service Django, Celery, RabbitMQ, atau server live Node setelah runtime dihentikan |

**Masih terbuka (tidak memblokir Fase 1):**

1. Siapa reviewer domain dan workspace mana yang dipakai untuk pilot MVP-B?

---

## 19. Checklist Kritis: Hal yang Paling Sering Terlewat

Gunakan ini sebagai daftar review setiap PR migrasi.

**Migrasi**

- [ ] Jumlah & urutan file migrasi Rust = `showmigrations --plan` Django (termasuk auth, contenttypes, sessions, celery beat).
- [ ] `pg_dump --schema-only` Django vs Rust: diff nol (fresh DB).
- [ ] Upgrade dari snapshot rilis lama: diff nol.
- [ ] `django_migrations` terisi sama (app, name, applied).
- [ ] Semua 60 `RunPython` punya padanan; data terdampak identik.
- [ ] Setiap file Rust punya header + komentar per statement (tidak ada yang kosong).
- [ ] `sha256` di `manifest.json` cocok; gate `migration-drift` (target, menunggu workflow Rust) hijau.
- [ ] Tidak ada DDL di luar `apps/api-rs/migrations/**`.

**Data & query**

- [ ] `deleted_at IS NULL` di semua query (termasuk join dan subquery).
- [ ] Cascade soft delete sesuai daftar relasi, bukan menebak.
- [ ] `created_at`/`updated_at` diisi aplikasi bila DB tidak punya default.
- [ ] Tabel pivot ditulis lengkap dengan kolom audit.
- [ ] Ordering memakai allowlist; grouping mengikuti perilaku Django.
- [ ] Isolasi workspace: setiap query membatasi `workspace_id`/`project_id`.
- [ ] `distinct` pada join M2M (Django `distinct()` sering dipakai di search/filter).
- [ ] Semua query memakai pool RW (read replica tidak dipakai); startup menolak `ENABLE_READ_REPLICA=1`.

**Kontrak HTTP**

- [ ] Status code persis (201 vs 200, 204 vs 200, 403 vs 404).
- [ ] Bentuk error per kasus (field error, detail, envelope auth).
- [ ] Nama field JSON sama, termasuk yang `null` vs `""`.
- [ ] Waktu 6 digit mikrodetik + `Z`.
- [ ] Cursor pagination dan envelope grouping.
- [ ] `contracts/routes.json` sama persis: method, path, converter, dan urutan; tidak ada route hilang/ekstra.
- [ ] `contracts/params.json` sama dua arah: nama, tipe, default, dan perilaku nilai invalid/kosong/duplikat identik.
- [ ] Converter `<str>`/`<uuid>`: tipe salah → 404 (bukan 400); urutan route bertumpuk mengikuti Django.
- [ ] Trailing slash & redirect 301/404 identik, termasuk perilaku `POST`/`PUT`/`PATCH`.
- [ ] 404/405 body + header `Allow` sama; `OPTIONS`/`HEAD` sama.
- [ ] Nama query/header dan `Location` redirect (OAuth/magic link) identik.
- [ ] Body DRF compact + UTF-8 mentah; body `JsonResponse` (health/404 resolver/413) ber-spasi dan sama persis.
- [ ] 404 route vs 404 objek dibedakan (`{"error": ...}` vs `{"detail": "Not found."}`).
- [ ] Urutan field JSON sama (`preserve_order`), termasuk map hasil grouping.
- [ ] Float/Decimal/unicode/escaping identik dengan `json.dumps`; field tak dikenal diabaikan; pesan validasi DRF disalin.
- [ ] `Content-Type`, `Set-Cookie`, CORS, rate limit, `Vary`, dan security header Django ada dengan nilai sama.
- [ ] Semua case dikirim lewat HTTP nyata ke stack nyata; tidak ada test per service/method dan tidak ada mock DB/Redis/S3/SMTP/webhook.
- [ ] Header CORS, rate limit, content-type, gzip.
- [ ] 413 saat body melebihi `FILE_SIZE_LIMIT`.

**Auth & keamanan**

- [ ] Hash password Django (iterasi dibaca dari hash) + rehash policy.
- [ ] Sesi kompatibel (encoding + signing) atau migrasi logout terencana.
- [ ] Cookie: nama, `HttpOnly`, `Secure`, `SameSite`, domain, path.
- [ ] Write tanpa `Origin` (non-browser, mis. live server) tetap diterima sesuai aturan.
- [ ] API key: quota, `last_used`, token nonaktif/expired.
- [ ] OAuth state anti-CSRF dan validasi redirect URI.
- [ ] SSRF pinning + validasi redirect per hop pada semua fetch keluar.
- [ ] Sanitasi HTML identik (whitelist tag/atribut/style).

**Job & efek samping**

- [ ] Idempoten (retry aman), tidak mengirim email/webhook ganda.
- [ ] Lock Redis dengan key yang sama (menghindari race antar instance).
- [ ] Enqueue hanya setelah `COMMIT`; job yang haram hilang punya tiket `PENDING` di tabel Postgres yang sudah ada.
- [ ] `XAUTOCLAIM` (idle > 60 s) + retry maksimal 3 + DLQ + `XACK`; status `FAILED` tercatat di DB.
- [ ] `MAXLEN ~ 1000` pada `XADD`, TTL pada semua key, `maxmemory-policy noeviction`, AOF `everysec`.
- [ ] Key Rust tidak menabrak DB 0 Django (`:1:` cache, `send_email_notif_*`); payload JSON `{"v":1}` < 1 KB dipakai sebagai case `wire-parity`.
- [ ] Job berat tidak memblokir notifikasi (concurrency terpisah).
- [ ] Email/webhook/S3 diverifikasi di service nyata (SMTP/MinIO/receiver), bukan mock.
- [ ] Dead-letter & alert bila job gagal berulang.
- [ ] Retention cleanup memakai env yang sama.
- [ ] Activity/version payload sama (diff field per field).

**Operasional**

- [ ] Graceful shutdown (SIGTERM) menunggu request/job selesai.
- [ ] Kesiapan DB/Redis dicek internal (log/metrik/healthcheck) tanpa route readiness baru; health check memakai `GET /` yang sudah ada.
- [ ] Connection pool: ukuran, timeout, idle recycle; jumlah worker × pool ≤ `max_connections` Postgres.
- [ ] Timeout request, timeout upstream (OAuth, webhook, S3).
- [ ] Log tidak memuat token/cookie/password.
- [ ] Metrik: latency per route, error rate, queue depth, job duration.
- [ ] Sizing 2 vCPU/2 GB diuji **setelah runtime Django/Celery/RabbitMQ dihentikan** (source Django tetap arsip read-only), aset di S3 eksternal, satu lingkungan berat per host; RSS < 1,5 GB dan tanpa OOM setelah soak. Hybrid Django + Rust diuji terpisah di 4 GB.

**Build hygiene**

- [ ] `rust-toolchain.toml` di-pin; compiler lokal = CI.
- [ ] `just check` < 60 detik (cache hangat) dan dijalankan sebelum commit/PR.
- [ ] Pre-commit menolak file Rust yang gagal `fmt`/`check`; tidak ada commit red.
- [ ] Target CI: `rust-lint` dan `migration-validate` berjalan sebelum `build`/`api-e2e` (`needs:`) setelah workflow Rust tersedia dan terbukti di PR.
- [ ] Migrasi di-apply nyata ke Postgres kosong, bukan hanya dibaca.
- [ ] File generated (`migrations/**`, `contracts/**`) hanya lewat generator; target `gen-check` hijau setelah workflow Rust tersedia.
- [ ] Satu PR = satu route/modul agar feedback tetap cepat.
- [ ] Dev loop: app Rust native, dependency di Docker; full build container hanya untuk parity/CI.

**MVP (pilot)**

- [ ] Allowlist MVP punya owner tunggal di `contracts/routes.json`; tidak ada route dobel.
- [ ] `wire-parity` 100% endpoint MVP dan `state-parity` untuk write.
- [ ] Sesi Opsi A diverifikasi dua arah (Django ↔ Rust).
- [ ] Email terkirim lewat worker Django; `EmailNotificationLog` tidak dobel.
- [ ] Webhook tetap dilayani Django untuk workspace pilot; tidak ada perubahan perilaku bagi user.
- [ ] Rollback Caddy < 1 menit; data tetap Django-readable.
- [ ] Switch base URL diuji di Mode 1 (same-origin) dan Mode 2 (direct build-time) tanpa diff kode FE.
- [ ] Pilot 10-20 user selama 2 minggu tanpa data corrupt/duplikat.
- [ ] Rollback drill diuji, bukan diasumsikan.

**Proses**

- [ ] Feature freeze backend selama fase write parity & cutover.
- [ ] Satu daftar endpoint (single source of truth) yang diperbarui tiap PR.
- [ ] Perubahan perilaku (bug fix) dipisah dan disetujui eksplisit, tidak "diam-diam".
- [ ] Lisensi AGPL & header hak cipta dipertahankan; CI diperluas ke file Rust.

---

## 20. Langkah 2 Minggu Pertama (konkret)

1. **Hari 1-2**: buat `contracts/endpoints.csv` + `contracts/routes.json` dari `get_resolver()` Django, salin `openapi-django.json` dari `manage.py spectacular`, dan rekam wire dari API Django yang benar-benar berjalan (status+header+body mentah) untuk 5-10 endpoint. Tulis skrip pembanding route Rust vs manifest sejak hari pertama.
2. **Hari 2-4**: spike sesi — ambil satu cookie `session-id` nyata dari environment lokal, tulis parser/penulis di Rust, uji round-trip dua arah (Django buat → Rust baca, Rust buat → Django baca).
3. **Hari 3-4**: spike password — verifikasi hash `pbkdf2_sha256` produksi (data tersanitasi) di Rust.
4. **Hari 4-6**: spike DB — scaffold `apps/api-rs` + `sqlx` pool, lalu bandingkan `GET /api/workspaces/` lewat HTTP nyata terhadap Django pada snapshot data nyata.
5. **Hari 6-8**: spike filter — port satu kombinasi filter issue yang dipakai saved view, lalu bandingkan respons API nyata dengan Django pada snapshot yang sama.
6. **Hari 8-9**: spike S3 — presigned POST + presigned GET, bandingkan JSON dengan Django.
7. **Hari 8-10**: bangun generator migrasi (manifest + SQL berkomentar) dan bandingkan `pg_dump --schema-only` Django vs Rust pada DB kosong; target diff nol.
8. **Hari 9-10**: buktikan keputusan 18.4 lewat spike (sesi Opsi A, Redis Streams, Node live sebagai oracle/rollback sampai cutover Fase 9 selesai lalu `plane-live` Rust, scheduler membaca `django_celery_beat_*`) + perbarui dokumen ini dengan hasil nyata.
9. **Hari 10**: build FE tanpa perubahan lalu jalankan smoke browser terhadap Rust (login → list issue → filter → create/update → upload). Jika gagal, catat sebagai bug paritas; jangan patch FE.

---

## 21. Sizing Deployment: RAM, VPS 2 vCPU / 2 GB, dan Simulasi

Angka 2C2G adalah **target soak setelah cutover Fase 9 selesai** (setelah backend dan live memakai Rust), bukan kondisi running saat ini. Saat ini backend masih Django + Celery + RabbitMQ, ditambah `apps/space` (Node SSR) dan `apps/live` (Node/Hocuspocus) sebagai proses terpisah. Fase hybrid membutuhkan host lebih besar; lihat 21.8.

8 GB yang terlihat di Docker Desktop bukan RSS satu proses API. Angka itu adalah gabungan memori VM WSL2, container yang sedang hidup, page cache, image/build cache, dan proses host. Diagnosis yang benar memisahkan **working set** dari **cache yang bisa direclaim**, lalu melihat tren, bukan satu angka puncak. Lihat 21.8 sebelum menyimpulkan kebocoran atau membeli RAM.

### 21.1 Aturan dasar

- **Build di CI, bukan di VPS.** `cargo build` dengan codegen butuh > 2 GB dan akan OOM di 2 GB. VPS hanya `pull` image/artifact.
- **Django, Celery, dan RabbitMQ tidak boleh ikut** di target 2 GB. Runtime Python API + worker + beat, broker, dan overhead prosesnya membuat batas ini tidak realistis. Eksekusi migrasi baru pindah ke `plane-migrate` setelah Fase 8 (14.3).
- **Selama cutover/hybrid** Django + Rust + Celery + RabbitMQ hidup bersamaan: gunakan minimal **4 GB** sementara, lalu turun ke 2 GB hanya setelah dekomisi. Jangan paksakan hybrid di 2 GB.
- Satu VPS menjalankan **satu stack berat** pada satu waktu. Bila prod dan sandbox harus hidup bersamaan, lihat 21.6.
- Swap 2 GB + `earlyoom`/`systemd-oomd` wajib; target RSS total <= 1,5 GB agar page cache punya ruang.
- Docker boleh, tetapi overhead ~100-150 MB; native systemd lebih hemat. Bila memakai compose, set `mem_limit`/`cpus` per service dan rotasi log.

### 21.2 Anggaran memori (Rust penuh setelah dekomisi)

| Komponen                                              | RSS target | Catatan                                                              |
| ----------------------------------------------------- | ---------- | -------------------------------------------------------------------- |
| OS, systemd, sshd, journald                           | 150-250 MB | `SystemMaxUse=200M`                                                  |
| Caddy                                                 | 20-35 MB   | serve static `web`/`admin`, reverse proxy, TLS, gzip                 |
| `apps/space` (Node SSR)                               | 90-150 MB  | `react-router-serve`; bukan static Caddy, satu proses Node           |
| `plane-api` (tokio 2 thread)                          | 50-80 MB   | pool DB 10, body limit 10 MB                                         |
| `plane-worker`                                        | 40-70 MB   | concurrency 2                                                        |
| scheduler (in-worker/terpisah)                        | 10-20 MB   | cron di-stagger, lock Redis                                          |
| `plane-live` (Rust)                                   | 35-60 MB   | satu proses, `tokio` 2 thread; target setelah cutover Fase 9 selesai |
| `apps/live` (Node, fase 0-8 + jendela cutover Fase 9) | 80-120 MB  | oracle/rollback; `--max-old-space-size=96`, satu instance            |
| RabbitMQ (hanya target transisi)                      | 150-250 MB | tidak ada di target Redis Streams                                    |
| Postgres 15                                           | 200-300 MB | `shared_buffers=192MB`                                               |
| Valkey                                                | 25-50 MB   | `maxmemory=96-128MB`                                                 |
| MinIO                                                 | 120-200 MB | **disarankan S3 eksternal**                                          |
| Cadangan spike                                        | 200-300 MB | page cache reclaimable                                               |

Perkiraan kasar dari komponen di atas:

Catatan: baris `plane-live` Rust dan `apps/live` Node **tidak berjalan bersama sebagai target akhir**. `apps/live` hanya oracle/rollback selama fase 0-8 dan jendela cutover Fase 9; setelah cutover selesai, hanya `plane-live` yang masuk anggaran 2C2G. `apps/space` tetap SSR Node dan tidak termasuk klaim penghapusan Node live.

- **Stack Django sekarang**: sekitar **1,56-2,5 GB** untuk API, Celery worker/beat, Postgres, Redis, MinIO, Caddy, serta `space` dan `live`; belum termasuk page cache, build, dan overhead Docker/WSL di mesin developer.
- **Rust + RabbitMQ (transisi)**: sekitar **1,03-1,7 GB** sebelum `space`/`live` dan page cache; ini belum layak disebut sizing produksi 2 GB yang teruji.
- **Rust + Redis Streams (target)**: sekitar **0,91-1,54 GB** tanpa MinIO. Dengan S3 eksternal dan satu lingkungan berat per host, 2 GB baru masuk akal sebagai target soak, bukan jaminan.
- **Hybrid Django + Rust**: minimal **4 GB**; menjalankan produksi dan sandbox bersamaan di 2 GB tidak realistis.

Angka ini adalah anggaran perencanaan dari komponen yang terlihat di kode/compose, bukan hasil pengukuran RSS di VPS. Klaim lulus hanya boleh setelah soak.

### 21.3 Tuning wajib

Postgres (`postgresql.conf`):

```
shared_buffers = 192MB
effective_cache_size = 768MB
work_mem = 4MB
maintenance_work_mem = 64MB
max_connections = 30
jit = off
wal_compression = on
checkpoint_timeout = 15min
max_wal_size = 1GB
```

- Total koneksi pool (api + worker + maintenance) <= 24, sisakan 6 untuk admin.
- Index untuk list issue/filter wajib ada sebelum cutover; verifikasi `EXPLAIN (ANALYZE, BUFFERS)` pada data nyata.

Valkey:

- `maxmemory 96mb`, `maxmemory-policy noeviction` (LRU bisa menghapus stream job), `appendonly yes`, `appendfsync everysec`.
- Semua key cache ber-TTL; `XADD MAXLEN ~ 1000`; alert bila `used_memory` > 80%.

Rust:

- `TOKIO_WORKER_THREADS=2`, pool 8-10, request timeout 30 s, concurrency limit di layer `tower`, gzip aktif.
- Worker concurrency 2, batch 50-100, jeda antar batch agar CPU tidak starvation.
- `plane-live`: satu proses Rust, tanpa cluster, target RSS 35-60 MB. `apps/live` Node hanya oracle sampai cutover Fase 9 selesai dan tidak ikut target 2C2G setelah dekomisi.

Sistem:

- Swap 2 GB (`vm.swappiness=10`), `earlyoom`/`systemd-oomd`, limit file 65535.
- Firewall hanya 22/80/443; Postgres/Valkey bind `127.0.0.1` atau network internal.
- Docker log `max-size=10m`, `max-file=3`; journald dibatasi.
- Backup `pg_dump` harian ke S3 eksternal + uji restore bulanan; aset di S3 eksternal.

### 21.4 Target kapasitas (diukur, bukan diasumsikan)

- 10-20 user aktif bersamaan, 5.000-50.000 issue per workspace.
- p95 list issue (filter umum) < 500 ms; create/update < 800 ms pada data nyata.
- k6: 10 VU read + 2 VU write selama 30 menit tanpa OOM; soak 7 hari di kelas VPS yang sama.
- Bila target tidak tercapai: naik ke 4 GB atau pindahkan Postgres/MinIO keluar. Jangan menurunkan paritas demi muat di 2 GB.

### 21.5 Dua lingkungan: produksi dan sandbox

Sandbox adalah **lingkungan deployment**, bukan fitur produk. Django tidak punya konsep mode aplikasi, jadi Rust juga tidak boleh punya.

| Aspek    | Produksi            | Sandbox                                                                                                              |
| -------- | ------------------- | -------------------------------------------------------------------------------------------------------------------- |
| Database | `plane` (data asli) | database terpisah (seed yang sudah ada di Django: `workspace_seed_task`/`dummy_data_task`, atau salinan tersanitasi) |
| Valkey   | instance produksi   | instance/DB index terpisah                                                                                           |
| S3       | bucket produksi     | bucket/prefix terpisah                                                                                               |
| Email    | SMTP produksi       | SMTP uji (Mailpit) lewat env `EMAIL_*` yang sama                                                                     |
| Host     | domain produksi     | host/port terpisah (`sandbox.<domain>`)                                                                              |
| Env      | `.env` produksi     | `.env` sandbox dengan **nama env var yang sama** (18.1)                                                              |
| Backup   | wajib               | opsional                                                                                                             |

- Kode sama, image sama, migrasi sama, **perilaku API sama**. Perbedaan hanya nilai config dan isi database.
- Tidak ada env var baru untuk membedakan mode, tidak ada banner "Sandbox", tidak ada mode read-only, kredensial khusus, rate limit berbeda, scheduler berbeda, atau endpoint reset. Semua itu tidak ada di Django dan tidak boleh ditambahkan.
- Jangan menyalin data produksi mentah ke sandbox; pakai seed yang sudah ada atau sanitasi ulang.
- Sandbox memakai host terpisah dengan cookie host-only; jangan pakai `COOKIE_DOMAIN` yang sama agar sesi tidak tertukar.

### 21.6 Bila produksi dan sandbox harus hidup bersamaan

- Opsi A (paling aman): VPS terpisah untuk sandbox.
- Opsi B: VPS yang sama dengan **minimal 4 GB**, container terpisah, DB/Redis/S3 sendiri, dan limit per service. Dua stack berat di 2 GB tidak didukung.
- Opsi C: jalankan satu lingkungan pada satu waktu (stop prod saat sandbox dipakai). Ini satu-satunya cara mempertahankan host 2 GB, dan tetap butuh smoke/restore sebelum dipakai.
- Semua opsi wajib memakai DB/Redis/S3 sendiri untuk sandbox. Jangan jadikan sandbox sebagai workspace di instance prod: user/data bercampur dan itu bukan model Django (7.7). Jangan gabung DB antar lingkungan.

### 21.7 Gate kelulusan (Fase 7)

- [ ] Image/artifact dibangun di CI; VPS tidak menjalankan `cargo build`.
- [ ] Tidak ada Django, Celery worker/beat, atau RabbitMQ di VPS; aset memakai S3 eksternal.
- [ ] Maksimal satu lingkungan berat berjalan per host 2 GB; prod dan sandbox bersamaan memakai host terpisah atau minimal 4 GB.
- [ ] RSS total < 1,5 GB setelah soak 7 hari; tidak ada OOM/restart.
- [ ] Swap in/out ~0 saat idle; alert bila sustained > 5%.
- [ ] k6 30 menit + soak 7 hari lulus target 21.4 pada kelas VPS yang sama.
- [ ] Rollback < 1 menit memakai binary/image sebelumnya.
- [ ] Smoke stack produksi dan stack sandbox hijau di CI dan di VPS.
- [ ] Backup + restore drill sukses; Postgres/Valkey tidak terbuka ke publik.

### 21.8 Diagnosis RAM 8 GB dan simulasi 2C2G (code-based)

**Fakta dari konfigurasi repo, bukan tebakan:**

- `deployments/cli/community/docker-compose.yml` menjalankan satu VM/stack dengan 11 service: `web`, `space`, `admin`, `live`, `api`, `worker`, `beat-worker`, `migrator`, `plane-db`, `plane-redis`, dan `plane-mq`, plus `plane-minio` dan `proxy` (13 container). API dan worker memakai image backend yang sama tetapi proses berbeda; `space` dan `live` adalah dua proses Node terpisah.
- `apps/api/bin/docker-entrypoint-api.sh` memakai Gunicorn + Uvicorn worker, dan `GUNICORN_WORKERS` default di compose adalah **1**. Worker Celery berjalan sebagai proses penuh dengan prefork pool; beat adalah proses penuh lain. Image Python 3.12 + Gunicorn + Celery + Django + DRF memuat banyak halaman memori yang dibagi lewat page cache, tetapi RSS container tetap menjumlahkan working set per proses.
- `apps/api/plane/settings/common.py` mengaktifkan Redis cache dan RabbitMQ broker. Django tidak menyimpan job queue di Postgres, jadi laporan RAM harus mencakup `plane-mq` dan `plane-redis`, bukan hanya `api`.
- `plane-db` dijalankan dengan `max_connections=1000`. Postgres menyiapkan shared memory untuk `shared_buffers`, wal, dan koneksi; nilai default image bukan angka kecil yang bisa diabaikan. `max_connections` yang besar juga menaikkan overhead per koneksi dan dapat membuat `work_mem` efektif membengkak saat query paralel.
- Build image tidak boleh dihitung sebagai RAM runtime: buildx/cargo/pnpm menggunakan builder container dan cache disk, dan puncaknya bisa jauh di atas RAM API. `docker system df` memisahkan image/cache dari working set.

**Kenapa Docker Desktop bisa menunjukkan ~8 GB:**

1. **VM WSL2 adalah batas atas, bukan pemakaian API.** Docker Desktop menjalankan daemon di VM; memori yang dilaporkan mencakup kernel VM, page cache file image/container, dan semua container. Linux memakai RAM kosong untuk cache; cache itu reclaimable dan tidak otomatis berarti leak.
2. **Page cache ikut terhitung.** Membaca image, `node_modules`, dan file Python menghasilkan page cache. Angka `docker stats` adalah working set container (termasuk cache yang belum direclaim), bukan jumlah heap aplikasi. `free -h` di WSL memisahkan `used`, `buff/cache`, dan `available`.
3. **Proses per service banyak.** API, worker, beat, migrator, space, live, web, admin, MinIO, RabbitMQ, Postgres, Redis, dan proxy semuanya punya baseline RSS. Menjumlahkan baseline proses yang memang berjalan bisa mendekati beberapa GB tanpa ada kebocoran.
4. **Build dan runtime bercampur.** `docker compose up --build`, `pnpm install`, `cargo build`, dan image layer besar dapat menaikkan pemakaian sementara. Setelah build selesai, cache bisa tetap terlihat sampai direclaim.
5. **Jangan menyimpulkan leak dari satu grafik.** Leak ditunjukkan oleh **pertumbuhan working set yang sustained setelah beban berhenti**, bukan oleh satu puncak. Bandingkan baseline idle, setelah k6, dan 30-60 menit setelah beban berhenti; jika RSS turun atau datar, itu cache/allocator, bukan leak.

**Cara mengukur yang benar (urutan):**

```bash
# 1. Per container, bukan hanya total VM
docker stats --no-stream

# 2. Di dalam VM WSL2: pisahkan cache dari working set
free -h
cat /proc/meminfo | egrep 'MemTotal|MemFree|MemAvailable|Buffers|Cached|SReclaimable|Swap'

# 3. Disk/image/cache, bukan RAM
docker system df -v

# 4. Process-level RSS di dalam container backend
docker exec <api-container> ps -eo pid,rss,comm,args --sort=-rss | head -20
docker exec <worker-container> ps -eo pid,rss,comm,args --sort=-rss | head -20

# 5. Setelah beban: bandingkan, jangan menebak
docker stats --no-stream
```

**Simulasi 2C2G (perencanaan, bukan hasil uji):**

| Skenario                                                                            | Working set kasar                                        | Kesimpulan                                                                                              |
| ----------------------------------------------------------------------------------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| Django sekarang + Docker + build/cache di mesin developer                           | 2-4 GB+ untuk stack, 8 GB terlihat karena VM/cache/build | 8 GB bukan bukti API butuh 8 GB; ukur per container dulu                                                |
| Hybrid Django + Rust + Celery + RabbitMQ                                            | minimal 4 GB                                             | tidak layak dipaksa ke 2 GB; pertahankan host lebih besar selama cutover                                |
| Rust-only + Redis Streams + Postgres + Caddy + `space` + `plane-live`, S3 eksternal | sekitar 0,9-1,5 GB                                       | target setelah cutover Fase 9 selesai; 2 GB hanya target soak, butuh swap, limit, dan pengukuran 7 hari |
| Rust-only + produksi dan sandbox bersamaan di satu host 2 GB                        | tidak didukung                                           | pakai host terpisah, host >= 4 GB, atau satu lingkungan aktif                                           |

**RabbitMQ vs Redis Streams:** mengganti broker saja tidak menghentikan Celery worker/beat atau proses Django, jadi penghematannya kecil dibanding total stack. RabbitMQ memang lebih berat (Erlang VM + management plugin) daripada Redis Streams, tetapi penghematan yang signifikan baru muncul setelah service Django, Celery worker/beat, dan RabbitMQ dihentikan bersamaan. Source `apps/api/**` tetap arsip read-only dan tidak ikut dijalankan. Keputusan repo tetap: **Redis Streams untuk target Rust**, RabbitMQ hanya selama transisi.

**Aturan klaim:** angka 8 GB tidak boleh dipakai untuk menyimpulkan ?API leak? atau ?butuh VPS 8 GB?. Klaim lulus 2C2G hanya boleh setelah `docker stats`, `free -h`, dan soak 7 hari pada host yang sama menunjukkan working set < 1,5 GB tanpa OOM/restart. Samakan kelas CPU/RAM, ukuran data, dan beban k6 sebelum membandingkan.

---

## 22. Breakdown Kartu Kerja (Trello)

Papan: **Plane Rust Migration**. Sumber kebenaran: `docs/trello/plane-rust-cards.json` (77 kartu, 793 item checklist, 37 label). CSV siap-import: `docs/trello/plane-rust-cards.csv`.

### 22.1 Prinsip

- 1 kartu = 1 grup endpoint/modul atau 1 pekerjaan non-HTTP dengan acceptance profile eksplisit; ukuran 0,5-2 hari, pecah bila > 3 hari.
- Dua level: epic per modul + kartu anak hanya untuk write/upload/auth/bulk/job/webhook yang berisiko.
- Endpoint read sederhana menjadi item checklist di epic modul, bukan kartu terpisah.
- Tidak ada kartu untuk layer (repository/DTO) atau migrasi per file; itu checklist di dalam kartu.
- Kartu HTTP memakai bukti boundary HTTP: `route-owner`, `wire-parity`, `state-parity`, dan case `api-e2e`. Kartu non-HTTP memakai bukti profilnya sendiri dan tidak boleh mewarisi gate HTTP generik.

### 22.2 List papan

| List kategori asal     | Isi                                                     | Total kartu kategori |
| ---------------------- | ------------------------------------------------------- | -------------------- |
| Spike (Fase 0)         | Spike dan keputusan arsitektur sebelum coding           | 10                   |
| Fondasi                | Kerangka HTTP, config, migrasi, CI, dev tooling         | 17                   |
| Read parity            | Seluruh GET modul inti                                  | 11                   |
| Write parity           | Create/update/delete, activity, upload, permission      | 10                   |
| Auth & Live            | Sesi, password, OAuth, magic code, API key, live, admin | 7                    |
| Jobs/Non-HTTP          | Queue, scheduler, email, webhook, notifikasi            | 7                    |
| MVP                    | Milestone pilot dan gate parity                         | 8                    |
| Cutover & Decommission | Canary, sizing 2C2G, hentikan runtime Django, operasi   | 7                    |

Tabel di atas menghitung kartu berdasarkan **kategori asal**, bukan list tempat kartu berada saat ini. Snapshot 2026-11-26: 71 kartu berada di 8 list kategori dan 6 kartu berada di list alur kerja (`progress` 1, `review` 5, `todo`/`done`/`Blocked` 0); totalnya 77. Jadi angka 10/16/11/... adalah total kategori yang sudah mencakup kartu yang pindah ke `progress` atau `review`, dan angka itu tidak boleh dijumlahkan lagi dengan 5 kartu workflow.

List alur kerja (kartu berpindah ke sini saat dikerjakan; prefiks judul tetap menandai jenis kartu):

| List       | Isi                                   | Aturan                                                       |
| ---------- | ------------------------------------- | ------------------------------------------------------------ |
| `todo`     | Kartu yang siap dikerjakan berikutnya | pilih dari sini saat mulai                                   |
| `progress` | Kartu yang sedang dikerjakan          | **maksimal 1**; wajib dipindah ke sini sebelum mengubah file |
| `review`   | PR terbuka, menunggu CI/review        | keluar hanya setelah merge atau kembali ke `progress`        |
| `done`     | Selesai                               | DoD lengkap dan bukti parity tertaut                         |
| `Blocked`  | Terhalang                             | tulis blocker di kartu                                       |

Kelima list itu ada di `docs/trello/plane-rust-cards.json`; `import-cards.ps1` membuatnya bila belum ada dan memindahkan kartu yang sudah ada ke list sesuai JSON, sehingga status `progress` bertahan saat impor ulang.

Label: `jenis:*` (read/write/auth/job/infra/migrasi/spike/cutover/mvp/ops/size/decom/decision), `modul:*` (workspace, project, issue, cycle, module, state, view, intake, page, auth, live, admin, job, migrasi, deploy, ops, security), `risiko:tinggi|sedang|rendah`.

### 22.3 Template kartu dan acceptance profile

Setiap kartu di `plane-rust-cards.json` wajib punya field `profile`. Profil menentukan apa yang membuktikan kartu selesai; ia bukan label kosmetik dan tidak boleh dipakai untuk mengklaim gate yang belum dijalankan.

| Profile      | Kartu | Acceptance utama                                                                                                                      |
| ------------ | ----: | ------------------------------------------------------------------------------------------------------------------------------------- |
| `http`       |    25 | Kontrak endpoint, `route-owner`, `wire-parity`, `api-e2e`, plus bukti read/auth/admin/core                                            |
| `http-write` |    10 | Sama seperti `http`, ditambah `state-parity` dan pemeriksaan efek samping via GET + SQL                                               |
| `http-live`  |     1 | Endpoint HTTP yang dipanggil `apps/live`; WebSocket/Yjs masuk Fase 9 `live-parity`                                                    |
| non-HTTP     |    40 | Bukti sesuai profil: `spike`, `decision`, `infra`, `data`, `migration`, `dev`, `job`, `mvp`, `cutover`, `sizing`, `decom`, atau `ops` |

**Kartu HTTP: satu-satunya pemilik gate HTTP**

- Method, path, query, header, request body, status, response header, dan response body harus sama dengan Django pada data nyata.
- Gate `route-owner`, `wire-parity`, `api-e2e`, dan `param-parity` hanya bermakna untuk kartu HTTP. `state-parity` wajib untuk `http-write`.
- Perbandingan dilakukan di HTTP API boundary, bukan dengan memanggil handler Rust secara langsung. Unit test domain/parser/serializer tetap wajib untuk logika internal.
- Gate CI yang belum ada tetap ditulis sebagai **target**; jangan mengganti status kartu menjadi hijau hanya karena checker lokal lolos.

**Kartu non-HTTP: tidak memiliki gate HTTP generik**

- 41 kartu non-HTTP tidak memakai gate HTTP generik: `route-owner`, `wire-parity`, `api-e2e`, dan klaim "request/response identik" bukan DoD kartu tersebut. Bukti mereka adalah artefak profil masing-masing: catatan spike, keputusan, build/rollback, query plan, schema/ledger parity, hasil queue, laporan pilot, soak, decommission, atau backup/restore. Kartu `spike`/`mvp` boleh merujuk `wire-parity` sebagai harness atau laporan agregat yang mereka bangun/koordinasikan; item DoD kartu tetap mengikuti profilnya.
- Setiap kartu non-HTTP memuat empat DoD profil pada posisinya masing-masing. Item khusus kartu dipertahankan, termasuk urutan khusus pada `[DECOM] Pensiunkan generator migrasi Django`; item `PR kecil (< 400 baris diff produk)...` tetap ada.
- Menambah kartu baru wajib memilih `profile` dan menulis acceptance yang bisa dibuktikan. Jangan menyalin gate HTTP ke kartu infra, job, sizing, ops, atau migrasi.

**LIVE dan Fase 9**

- Kartu `[LIVE] Live endpoints (apps/live)` hanya menguji endpoint HTTP Django yang dipanggil server live: current user, live-access, page GET/PATCH, page description GET/PATCH, mentions, dan redirect asset gambar untuk PDF export.
- Endpoint `/health`, `/convert-document`, dan `/pdf-export` adalah endpoint server live sendiri, bukan API Django. Porting ketiganya masuk Fase 9 `live-parity` (13.10).
- Handshake WebSocket, Yjs, reconnect, presence, dan konflik versi adalah scope Fase 9 `live-parity` (13.10), bukan DoD kartu ini.
- Server Node tetap oracle/rollback selama fase 0-8 dan jendela cutover; source `apps/live/**` tidak dihapus. Setelah cutover Fase 9 selesai, produksi hanya menjalankan `plane-live` Rust dan source lama menjadi arsip read-only.

### 22.4 Breakdown per fase

Angka di setiap heading adalah total kartu kategori asal, termasuk kartu yang saat ini berada di `progress` atau `review`. Jumlah fisik kartu di 8 list kategori adalah 71; 6 kartu sedang berada di list workflow (lihat 22.2).

#### Spike (Fase 0) (10 kartu)

| Kartu                                         | Fokus                                                                                                                                       | Checklist |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- | --------- |
| [SPIKE] Peta kontrak endpoint & route surface | Hasilkan contracts/endpoints.csv dan contracts/routes.json dari Django: method, path template, converter, u...                              | 10 item   |
| [SPIKE] Sesi Django-compatible (Opsi A/B)     | Buktikan cookie session Django bisa dibaca dan ditulis dua arah, lalu putuskan Opsi A atau B.                                               | 10 item   |
| [SPIKE] Password pbkdf2_sha256                | Verifikasi hash password produksi (data tersanitasi) di Rust tanpa mengubah format.                                                         | 10 item   |
| [SPIKE] Presigned S3 POST/GET                 | Bandingkan JSON presigned upload/download Django vs Rust, termasuk policy dan expiry.                                                       | 10 item   |
| [SPIKE] Filter & query DSL issue              | Port 10 kombinasi filter dari saved view nyata dan bandingkan hasil serta total count.                                                      | 10 item   |
| [SPIKE] Job queue semantics (Redis Streams)   | Buktikan XADD/XAUTOCLAIM, lock, retry, dan DLQ memenuhi kebutuhan job Plane.                                                                | 10 item   |
| [SPIKE] Generator migrasi & schema parity     | Hasilkan manifest + SQL berkomentar dari 125 migrasi Django dan buktikan pg_dump identik.                                                   | 10 item   |
| [SPIKE] Harness wire-parity                   | Rekam status/header/body mentah dari Django dan bandingkan otomatis dengan Rust.                                                            | 10 item   |
| [SPIKE] Baseline performa Django              | Rekam p95 dan query plan endpoint inti sebagai pembanding performa Rust.                                                                    | 10 item   |
| [DECISION] Keputusan arsitektur Fase 0        | Buktikan keputusan 18.4 (sesi Opsi A, Redis Streams, target live Node lalu Rust `plane-live`, scheduler DB) dan kunci batas MVP + estimasi. | 9 item    |

#### Fondasi (17 kartu)

| Kartu                                                                | Fokus                                                                                                   | Checklist |
| -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- | --------- |
| [INFRA] Workspace Cargo & struktur crate                             | Scaffold apps/api-rs dengan crate domain/repo/http/worker/migrate dan fitur minimal.                    | 10 item   |
| [INFRA] Config loader & env parity                                   | Loader env mengikuti 18.1, fail-fast, dan meredaksi secret di log.                                      | 10 item   |
| [CORE] Error mapping DRF                                             | Port bentuk error 400/401/403/404/405/409/413/415/429 dan handler404 persis Django.                     | 10 item   |
| [CORE] Serializer waktu/JSON & null rules                            | Samakan ISO8601, unicode, decimal, UUID lowercase, null vs omit, dan compact vs spaced.                 | 10 item   |
| [CORE] Middleware CORS/gzip/body limit/origin/rate limit             | Middleware setara Django: preflight, gzip, body limit 10 MB/413, origin check, rate limit Redis.        | 10 item   |
| [CORE] Auth extractor & Actor                                        | Ekstrak session/API key/OAuth menjadi Actor dan bedakan 401 vs 403.                                     | 10 item   |
| [CORE] Parser request DRF-compatible                                 | Parser JSON/form/multipart dengan unknown field diabaikan dan 415 sesuai Django.                        | 10 item   |
| [CORE] Renderer ganda (compact vs JsonResponse)                      | Dua jalur render: DRF compact dan JsonResponse ber-spasi untuk health/404/413.                          | 10 item   |
| [DATA] Pool Postgres (RW) & repository base                          | Pool RW tunggal, timeout, soft-delete helper, pagination, dan transaction helper.                       | 10 item   |
| [CORE] Logging & request id                                          | Log JSON dengan request id, level config, dan redaksi data sensitif.                                    | 10 item   |
| [CORE] Health/readiness/robots                                       | Implementasi / dan /robots.txt identik; kesiapan DB/Redis dicek internal tanpa route baru.              | 10 item   |
| [MIGRASI] Generator manifest + SQL berkomentar                       | Generator menghasilkan manifest.json + file .sql berkomentar dari migrasi Django tanpa mengedit sumber. | 10 item   |
| [MIGRASI] Binary plane-migrate                                       | Binary plan/check/apply/validate dengan ledger django_migrations dan advisory lock.                     | 10 item   |
| [INFRA] Dockerfile.api-rs + compose service                          | Image multi-stage, distroless, non-root, healthcheck, dan compose service.                              | 10 item   |
| [INFRA] CI fail-fast (lint/validate/gen-check/build)                 | Urutan job rust-lint -> migration-validate -> gen-check -> build -> e2e dengan cache dan needs.         | 10 item   |
| [DEV] Dev tooling fast feedback                                      | rust-toolchain, rust-analyzer, just check, pre-commit Rust, sccache, linker cepat.                      | 10 item   |
| [DEV] Wajibkan skill rust-anything di AGENTS.md dan docs/rust-api.md | Aturan wajib skill rust-anything, pemasangan lokal, dan batas keras migrasi yang mengikatnya.           | 6 item    |

#### Read parity (11 kartu)

| Kartu                                          | Fokus                                                                                  | Checklist |
| ---------------------------------------------- | -------------------------------------------------------------------------------------- | --------- |
| [READ] Workspace & member                      | GET list/detail workspace dan member dengan role payload identik.                      | 11 item   |
| [READ] Project & project member                | GET project list/detail, member, dan count terkait.                                    | 11 item   |
| [READ] Issue list (filter/group/cursor/search) | GET list issue dengan 20 kombinasi filter nyata, grouping, cursor, dan search.         | 11 item   |
| [READ] Issue detail + activity/version         | GET detail issue beserta activity dan description version.                             | 11 item   |
| [READ] Cycle                                   | GET list/detail cycle, progress, dan issue count.                                      | 11 item   |
| [READ] Module                                  | GET list/detail module, relasi issue, dan progress.                                    | 11 item   |
| [READ] State/label/estimate                    | GET state, label, dan estimate point dengan grouping/ordering sama.                    | 11 item   |
| [READ] View/intake                             | GET saved view dan intake issue dengan filter tersimpan.                               | 11 item   |
| [READ] Page & version                          | GET page list/detail, version history, dan flag public.                                | 11 item   |
| [READ] Dashboard/analytics ringan              | GET payload dashboard/analytics yang dipakai FE, termasuk cache TTL.                   | 11 item   |
| [READ] Pagination & search lintas modul        | Uji batas pagination, cursor invalid, page size, dan hasil kosong di semua modul read. | 11 item   |

#### Write parity (10 kartu)

| Kartu                                        | Fokus                                                                         | Checklist |
| -------------------------------------------- | ----------------------------------------------------------------------------- | --------- |
| [WRITE] Issue create                         | POST create issue: validasi, default, activity, dan notifikasi.               | 11 item   |
| [WRITE] Issue update (partial/version/audit) | PATCH issue: partial update, null handling, versioning, audit.                | 11 item   |
| [WRITE] Issue delete/soft-delete/restore     | DELETE/soft delete, trash list, dan restore issue beserta relasi.             | 11 item   |
| [WRITE] Activity & description versioning    | Tulis activity dan description version dengan payload identik Django.         | 11 item   |
| [WRITE] Asset upload/duplicate/restore       | Upload asset (presigned), duplicate, restore, dan orphan handling.            | 11 item   |
| [WRITE] Bulk operations                      | Bulk update/delete/move issue dengan atomicity dan partial failure yang sama. | 11 item   |
| [WRITE] Project write                        | Create/update/archive project dan sinkronisasi member.                        | 11 item   |
| [WRITE] Cycle/module write                   | Create/update/delete cycle dan module, termasuk move issue.                   | 11 item   |
| [WRITE] State/label/view/intake write        | Write state, label, saved view, dan intake dengan constraint sama.            | 11 item   |
| [WRITE] Permission matrix per role           | Matriks 4 role x aksi utama; verifikasi 403 vs 404 identik Django.            | 11 item   |

#### Auth & Live (7 kartu)

| Kartu                             | Fokus                                                                                                                                                                                                                                                            | Checklist |
| --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------- |
| [AUTH] Session login/logout       | Login/logout session dengan cookie flags dan rotasi identik.                                                                                                                                                                                                     | 10 item   |
| [AUTH] Password policy & rehash   | Kebijakan password, rehash, dan pesan error sama.                                                                                                                                                                                                                | 10 item   |
| [AUTH] OAuth                      | OAuth callback/state/redirect URI dan account linking.                                                                                                                                                                                                           | 10 item   |
| [AUTH] Magic code                 | Magic code: kirim, verifikasi, single-use, expiry.                                                                                                                                                                                                               | 10 item   |
| [AUTH] API key                    | API key: quota, last_used, revoke, dan scope.                                                                                                                                                                                                                    | 10 item   |
| [LIVE] Live endpoints (apps/live) | Port endpoint HTTP Django yang dipanggil apps/live: current user, live-access, page GET/PATCH, description, mentions, dan asset redirect. `/health`, `/convert-document`, `/pdf-export`, WebSocket/Yjs, reconnect, dan konflik versi masuk Fase 9 `live-parity`. | 10 item   |
| [ADMIN] Instance admin (god mode) | Port instance admin: user list, instance settings, dan audit.                                                                                                                                                                                                    | 10 item   |

#### Jobs/Non-HTTP (7 kartu)

| Kartu                                | Fokus                                                                    | Checklist |
| ------------------------------------ | ------------------------------------------------------------------------ | --------- |
| [JOB] Queue & worker (Redis Streams) | Worker Redis Streams dengan XAUTOCLAIM, retry 3, DLQ, dan tiket PENDING. | 11 item   |
| [JOB] Scheduler/beat parity          | Port 13 jadwal beat Django dengan timezone dan lock yang sama.           | 11 item   |
| [JOB] Email notification             | Email notification: template, dedupe key, log, unsubscribe.              | 11 item   |
| [JOB] Webhook                        | Webhook workspace/instance: HMAC, retry/backoff, timeout, SSRF guard.    | 11 item   |
| [JOB] Notification fan-out           | Fan-out in-app, mention, dan email tanpa duplikasi.                      | 11 item   |
| [JOB] Asset cleanup/orphan           | Job pembersihan asset yatim dengan grace period.                         | 11 item   |
| [JOB] License/instance jobs          | Job license/instance dengan isolasi error dan opt-out telemetry.         | 11 item   |

#### MVP (8 kartu)

| Kartu                                  | Fokus                                                                           | Checklist |
| -------------------------------------- | ------------------------------------------------------------------------------- | --------- |
| [MVP] Allowlist endpoint & route owner | Kunci daftar endpoint MVP di contracts/routes.json dengan satu owner per route. | 10 item   |
| [MVP] Wire-parity 100% endpoint MVP    | Semua endpoint MVP hijau wire-parity pada data nyata.                           | 10 item   |
| [MVP] State-parity write MVP           | Write MVP diverifikasi efek sampingnya via GET + SQL nyata.                     | 10 item   |
| [MVP] Session Opsi A dua arah          | Sesi Django <-> Rust terbukti dua arah pada staging.                            | 10 item   |
| [MVP] Email via Django worker          | Email MVP dikirim worker Django; log tidak dobel.                               | 10 item   |
| [MVP] FE base URL Mode 1 & 2           | Smoke FE tanpa diff kode pada Mode 1 (same-origin) dan Mode 2 (direct).         | 10 item   |
| [MVP] Pilot 10-20 user (2 minggu)      | Jalankan pilot terbatas dengan monitoring dan feedback harian.                  | 10 item   |
| [MVP] Rollback drill MVP               | Latih rollback Caddy + data tetap Django-readable.                              | 10 item   |

#### Cutover & Decommission (7 kartu)

| Kartu                                       | Fokus                                                                                                                                                                                                                                                      | Checklist |
| ------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------- |
| [CUTOVER] Canary per workspace              | Aktifkan Rust bertahap: 1 workspace -> 10% -> semua, dengan monitoring.                                                                                                                                                                                    | 10 item   |
| [CUTOVER] Scheduler switch                  | Matikan Celery beat Django dan aktifkan scheduler Rust tanpa job dobel.                                                                                                                                                                                    | 10 item   |
| [CUTOVER] Rollback drill pra-cutover        | Latih rollback penuh sebelum cutover produksi.                                                                                                                                                                                                             | 10 item   |
| [SIZING] VPS 2 vCPU/2 GB (prod & sandbox)   | Validasi sizing minimum di kelas VPS 2C2G untuk lingkungan produksi dan sandbox.                                                                                                                                                                           | 10 item   |
| [DECOM] Hapus runtime Django                | Hentikan service/API/worker/beat Django, Celery, dan RabbitMQ; source `apps/api/**` tetap arsip read-only dan tidak dipakai runtime. Production menjalankan `plane-api`/`plane-worker`/`plane-migrate`, lalu menambah `plane-live` setelah cutover Fase 9. | 10 item   |
| [DECOM] Pensiunkan generator migrasi Django | Bekukan baseline historis, keluarkan generator dari CI permanen, dan pindahkan migrasi baru ke Rust; tool audit offline wajib punya exit plan.                                                                                                             | 10 item   |
| [OPS] Backup/restore & monitoring           | Backup harian, uji restore, dan alert memori/swap/DB/queue.                                                                                                                                                                                                | 10 item   |

### 22.5 Aturan ukuran dan anti-pattern

- Pecah kartu bila > 3 hari, > 8 endpoint, atau menyentuh > 2 modul.
- Jangan membuat kartu "migrasi per file" atau "repository layer".
- Jangan menutup kartu tanpa link bukti acceptance profil dan PR.
- Jangan memindahkan kartu ke Done sebelum `state-parity` untuk write.
- Satu PR = satu perubahan yang bisa direview dan di-rollback sendiri; jangan campur kartu yang tidak terkait. Satu kartu boleh jadi beberapa stacked PR; perbaikan sepele (typo, dependency bump, docs) boleh satu PR.

### 22.6 Import ke Trello

**Opsi A - CSV (manual, tanpa script)**

1. Trello > buat board baru > menu board > Import/Export > Import CSV.
2. Pilih `docs/trello/plane-rust-cards.csv`.
3. Map: `Name` -> Card name, `Description` -> Description, `List` -> List, `Labels` -> Labels, `Due Date` -> Due date.
4. CSV hanya membawa checklist sebagai teks Markdown. Untuk checklist Trello asli, pakai Opsi B.

**Opsi B - script (checklist asli, idempoten)**

```powershell
$env:TRELLO_KEY = '<key>'
$env:TRELLO_TOKEN = '<token>'
.\docs\trello\import-cards.ps1 -BoardId 2sYIabqF -DryRun -RemoveStaleDoD   # lihat rencana
.\docs\trello\import-cards.ps1 -BoardId 2sYIabqF -RemoveStaleDoD            # eksekusi
```

- Script membuat list dan label yang belum ada, lalu membuat kartu beserta checklist `Definition of Done`.
- Kartu dicocokkan berdasarkan nama. Bila nama berubah, entri `aliases` dipakai untuk rename di tempat agar tidak membuat duplikat.
- Kartu yang sudah ada tidak dilewati begitu saja: `desc` diperbarui dari JSON, kartu dipindahkan ke list yang ditentukan, dan label yang belum ada ditambahkan.
- Checklist disinkronkan per item. String biasa hanya memastikan item ada; objek `{"name","state"}` membuat state Trello mengikuti JSON. Item dengan nama sama tidak diduplikasi.
- `-DryRun` hanya membaca dan melaporkan rencana: `created`, rename, perpindahan, perubahan desc/label, `newCheckItems`, `checkItemStates`, `staleDoDRemoved`, `extraCheckItems`, dan `extraCards`. Dry-run tidak membuat atau mengubah apa pun.
- `-RemoveStaleDoD` adalah pembersihan bertarget: ia hanya menghapus empat gate HTTP lama dari kartu non-HTTP dan lima item WebSocket/Yjs lama dari kartu `http-live`. Item manual lain tidak disentuh.
- `-Prune` menghapus **semua checklist item** yang tidak ada di JSON; kartu ekstra hanya dilaporkan, tidak dihapus. Jangan memakai `-Prune` tanpa persetujuan eksplisit karena item manual di board bisa hilang. Untuk migrasi acceptance profile ini, gunakan `-RemoveStaleDoD`, bukan `-Prune`.
- Token hanya dari env atau `.mcp.json` yang di-ignore, tidak ditulis ke file. Jangan commit token.

**Status sinkronisasi 2026-11-26:** JSON adalah sumber kebenaran untuk 77 kartu, 793 checklist item, dan 37 label. Acceptance profile sudah ditambahkan ke setiap kartu: 36 kartu HTTP (`http` 25, `http-write` 10, `http-live` 1) dan 41 kartu non-HTTP. Import memakai `-RemoveStaleDoD`, bukan `-Prune`, sehingga hanya gate lama yang salah tempat yang dibersihkan. Sembilan kartu ekstra `Fase 0`-`Fase 8` di board dibiarkan sebagai kartu terpisah. Angka hasil import aktual dan dry-run akhir dicatat di laporan pekerjaan; dry-run harus berakhir dengan `descUpdated=0 newCheckItems=0 checkItemStates=0 staleDoDRemoved=0` agar sinkronisasi dianggap idempoten.

### 22.7 Perawatan

- JSON adalah sumber kebenaran. Setelah mengubah JSON, jalankan `docs/trello/export-csv.ps1` lalu commit JSON + CSV.
- Rename kartu live wajib dicatat di JSON: tambahkan nama lama ke `aliases` pada kartu yang sama, lalu jalankan importer agar rename dilakukan di tempat. Perubahan nama yang tidak tercatat di JSON akan terlihat sebagai kartu ekstra dan berpotensi membuat duplikat saat import berikutnya.
- Tambah kartu baru dengan `profile` yang sesuai. Kartu HTTP memakai gate HTTP di atas; pekerjaan non-HTTP tetap boleh menjadi kartu bila punya acceptance profil yang dapat dibuktikan, bukan menyalin gate HTTP.
- Perbarui tabel 22.4 dengan cara regenerate dari JSON, bukan edit manual.
- Kartu Fase 9 (live ke Rust, 13.10) belum ada di papan; buat setelah Fase 8 mendekati selesai dengan template 22.3, bukan dicampur ke kartu migrasi API.

---

## 23. Alur Kerja Git: branch, tag, dan rilis

Prinsip: **trunk-based** dengan satu branch integrasi (`preview`) dan rilis immutable berbentuk **tag**. Tidak ada branch permanen `prod` atau `dev`.

```text
preview ---o---o---o---o--->   (integrasi, selalu hijau)
            \   \   \
             PR  PR  PR       (squash merge; satu PR = satu perubahan)
                     \
                      tag v0.9.0 -> deploy -> rollback = tag sebelumnya
```

### 23.1 Branch

| Branch                      | Sifat     | Tujuan                              | Aturan                                                                  |
| --------------------------- | --------- | ----------------------------------- | ----------------------------------------------------------------------- |
| `preview`                   | permanen  | integrasi, satu-satunya tujuan PR   | dilindungi: no direct push, CI wajib hijau, 1 approval, linear history  |
| `feat/<kartu>-<slug>`       | sementara | kartu baru / write parity           | dari `preview` terbaru, umur <= 3 hari                                  |
| `fix/<kartu>-<slug>`        | sementara | bug paritas atau bug produk         | sama seperti `feat`, rebase harian                                      |
| `spike/<slug>`              | sementara | spike Fase 0 (timebox 2-3 hari)     | boleh tidak merge; kesimpulan ditulis di kartu                          |
| `chore/mirror-<app>-<name>` | sementara | migrasi cermin hasil generator      | hanya berisi file generated + manifest                                  |
| `release/<major.minor>`     | sementara | patch untuk rilis yang sudah keluar | dibuat dari tag, bukan dari `preview`; dihapus setelah rilis berikutnya |

- Semua branch kerja dibuat dari `preview` terbaru dan **rebase** ke `preview` setiap hari. Jangan `git merge preview` ke branch kerja: commit merge merusak diff review.
- Umur branch maksimal 3 hari. Bila lebih, pecah kartunya (lihat 22.5).
- Satu kartu WIP = satu branch aktif. Kartu berikutnya menunggu sampai branch sebelumnya merge atau dikembalikan ke `todo`.
- Tidak ada branch yang mewakili environment. Produksi dan sandbox adalah **deployment** dari tag yang sama (21.5), bukan branch berbeda.

### 23.2 Commit

- Format: `[Tipe] deskripsi singkat`. Tipe: `Feat`, `Fix`, `Chore`, `Docs`, `Refactor`, `Test`, `Perf`, `Build`, `CI`.
- Body menyebut ID kartu Trello dan alasan perubahan bila tidak obvious.
- Setiap commit harus bisa dikompilasi. Pre-commit menolak `fmt`/`cargo check` yang gagal (12.9).
- File generated (`apps/api-rs/migrations/**`, `contracts/**`) di-commit terpisah dari kode tangan, dan hanya berasal dari generator.
- Sebelum review: `git commit --fixup` + `git rebase -i --autosquash` boleh. Setelah review dimulai, jangan force-push tanpa meminta review ulang.

### 23.3 Pull request

- Target selalu `preview`. Judul PR = judul kartu. Buka sebagai draft di awal agar review berjalan paralel.
- Body wajib: link kartu, scope/out-of-scope, artefak bukti (`route-parity`, `param-parity`, `wire-parity`, `state-parity`, laporan migrasi), rencana rollback, dan risiko.
- Ukuran: <= 400 baris diff produk. Bila lebih, pecah menjadi **stacked PR**: PR-2 di-base ke branch PR-1, dan chain di-rebase saat PR bawahnya merge.
- Satu PR = satu perubahan yang bisa di-review dan di-rollback sendiri. Satu kartu boleh beberapa PR; satu PR tidak boleh berisi beberapa kartu.

### 23.4 Gate dan merge

- **Target** required checks (urutan job di 12.7): `rust-lint`, `migration-validate`, `gen-check`, `route-owner`, `route-parity`, `param-parity`, `wire-parity`, `fe-unchanged`, plus `state-parity` untuk write. Bila PR menyentuh migrasi, tambah `migration-drift`, `schema-parity-fresh`, `schema-parity-stepwise`, `schema-parity-upgrade`, `ledger-parity`, `data-parity`. Daftar ini belum aktif sebagai branch protection karena workflow Rust belum ada; status "required" hanya boleh dipakai setelah job benar-benar berjalan dan terbukti di PR.
- Merge hanya dengan **squash merge** ke `preview`, lalu branch dihapus. Linear history membuat `git bisect` dan rollback per perubahan tetap mungkin.
- Jangan merge saat merah, dan jangan "merge dulu, perbaiki di PR berikutnya".
- Bila repo menyediakan merge queue, aktifkan agar `preview` tidak pernah merah karena dua PR saling menimpa.

### 23.5 Tag dan rilis

- Format tag: `v<major>.<minor>.<patch>` dengan sufiks opsional untuk milestone migrasi, mis. `v0.9.0-rust-mvp`, `v1.0.0-cutover`.
- Deploy hanya dari tag. Image/artifact diberi tag yang sama dan bersifat **immutable**; jangan rebuild dari branch saat deploy.
- Setiap tag mencatat: versi image, head migrasi (pasangan `app.name` terakhir), mode backend selama transisi (`django`, `rust`, atau `hybrid`), status runtime live (`node` atau `rust`), dan peta owner route di `contracts/routes.json`. Setelah Fase 8, hanya mode backend `rust` yang valid; setelah cutover Fase 9 selesai, hanya runtime live `plane-live` Rust yang valid. Tag dengan backend `django`/`hybrid` atau live Node tidak boleh dideploy sebagai target akhir.
- Sandbox di-deploy lebih dulu dengan tag yang sama, lalu produksi. Tidak ada tag kedua khusus sandbox.
- Rollback = redeploy tag sebelumnya (14.2). Tidak ada `git checkout <branch>` di server.
- Release candidate memakai tag `-rc.N`, bukan branch.

### 23.6 Hotfix rilis lama

- Buat `release/<major.minor>` dari **tag** yang dipakai user, isi fix minimal, tag patch (`v0.9.1`), lalu cherry-pick ke `preview` lewat PR.
- Jangan cherry-pick langsung ke `preview` tanpa PR, dan jangan menambah fitur di branch rilis.
- Hapus `release/<major.minor>` setelah tidak ada user di versi itu.

### 23.7 Larangan

- Membuat branch permanen `prod`, `staging`, atau `dev`.
- Force-push ke `preview`, atau rewrite tag yang sudah di-deploy.
- Mengedit file di `migrations/**`/`contracts/**` dengan tangan.
- Tag tanpa catatan rilis (versi, head migrasi, status cutover).

### 23.8 Sinkronisasi kartu

- Sebelum mengubah file: kartu pindah ke `progress`, dan itu satu-satunya kartu di sana.
- Branch dibuat -> kartu sudah berada di `progress` (bukan `Ready`).
- PR dibuka -> kartu pindah ke `review`.
- PR merged dan bukti parity terlampir -> kartu pindah ke `done`.
- Berhenti karena blocker -> kartu pindah ke `Blocked`, alasannya ditulis di kartu.

---

## 24. Required skill: rust-anything

Every task in this repo must use the [`rust-anything`](https://github.com/erkamyaman/rust-anything) skill alongside `rust-skills` and REA, which `AGENTS.md` already requires. The skill is used to choose port units and to keep the evidence honest: it plans the port and records behavior as the oracle, and it does **not** replace the repo gates. While the no-Docker/no-Django rule is active the oracle is static: the source is read at the deciding line and frozen by a `contracts/tools/capture_*.py` script, never by running Django.

**Install and verify**

- CLI: `cargo install rust-anything`, then `rust-anything setup --agent codex` (writes the MCP server to `~/.codex/config.toml` and the skill to `~/.codex/skills/rust-anything/`). Verified locally: `rust-anything 4.1.0` (2026-11-26); `rust-anything doctor` reports the MCP server answering with 22 tools. Run `--version`/`doctor` only when a step fails or the MCP looks stale.
- Pin the skill to the installed CLI. GitHub `main` is v5.0.0 and documents `rules`, `app drift`, and `diff --redact`, which v4.1.0 does not have. `rust-anything setup` writes the matching copy, so re-run it after every CLI upgrade instead of copying `SKILL.md` from `main`.
- Local skill copy: `C:\Users\nhaid\.agents\skills\rust-anything\SKILL.md` + `whole-app.md`. Read the skill before writing code, not after a review fails.
- In this repo, run `analyze`/`profile` against the Django source in `apps/api` as a reference; the output is a unit plan, not permission to change that source.

**Commands required when relevant**

| Stage          | Command                                                                          | Output recorded on the card/PR                                     |
| -------------- | -------------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| Choose a unit  | `rust-anything analyze apps/api`, `rust-anything profile apps/api -- <workload>` | `start_here` units, `keep`/`blocked` units and reasons, wave order |
| Static oracle  | `contracts/tools/capture_*.py` (source parsed with `ast`, no Django import)      | new files under `contracts/golden/` + the row count                |
| Prove behavior | `cargo test --all-targets`, `route-manifest`, `python contracts/tests/*.py`      | test counts, manifest size, parity run result                      |

`bridge`, `record`, `build`, `diff`, `bench`, `fuzz`, and `gate` need a bridge crate, so they only apply once a unit has been scaffolded; with the no-Django rule active (see `AGENTS.md`), a unit that `analyze` marks `blocked` or export-less is documented on the card instead of forced through the skill.

**Hard boundaries that bind the skill**

The `rust-anything` skill must not be used to break §0. Specifically:

1. **Do not run `enable`/`disable`.** Those commands edit source files with marker blocks; `apps/api/**`, `apps/live/**`, and the frontend must not change.
2. **Do not edit the Django/live/frontend source.** `record`, `--from-tests`, and `app record` are read-only. If a command needs a temporary patch, run it on a disposable copy outside the repo, never in `apps/api`.
3. **No bridges in the production path.** `bridge` output (PyO3/napi/cgo/JNI) is for proof only; production stays `plane-api`/`plane-worker`/`plane-migrate`, pure Rust. A bridge that would have to run in production is rejected.
4. **The workspace stays in the repo.** Crates go under `apps/api-rs/crates/*` following §4.2, not a new `rust/` tree at the repo root. Crate names, edition, license, and `rust-toolchain.toml` follow the existing workspace.
5. **Repo oracles win.** `contracts/tools/capture_*.py`, `contracts/golden/`, `wire_golden`, `route-parity`/`wire-parity`, and `just check` remain the official evidence. `rust-anything` cases are additional evidence; they must not be used to keep `wire-parity` green or to replace the Django golden files.
6. **No Docker, no running Django** (see the active rule in `AGENTS.md`): do not start the stack, `manage.py`, `runserver`, or the `apps/api` pytest suite. `record`, `diff`, and `gate` need a bridge crate, so they are not used while that rule is active; the oracle is a static `contracts/tools/capture_*.py` capture, and a step that needs a run is written on the card as `unknown` instead.
7. **`gate` does not replace `just check`.** Both must be green before a PR: `gate` checks the port crate, `just check` checks the whole workspace.

**Reporting**

The final report states: the `rust-anything` commands used, the golden rows or test counts that back each behavior claim, the units deliberately kept `keep`/`blocked`, and the reason when the skill is not relevant (e.g. a docs-only task). When `bridge`/`record`/`gate` were not applicable because the unit is export-less, say that instead of leaving it out.
