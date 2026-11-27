# Progres Porting Rust: Fondasi, Persentase, dan Batas yang Berlaku

> Potret per **2026-11-27**. Dokumen ini adalah pelengkap `docs/rust-api.md` (rencana, kontrak, estimasi); isi di sini hanya **angka progres terukur** dan cara memperbaruinya.
> Branch saat potret: `spike/f0-route-surface`.
> Setiap klaim diberi label: **observation** = dibaca atau dijalankan langsung di repo ini; **inference** = perkiraan turunan dari data observation; **unknown** = butuh eksekusi yang saat ini dilarang aturan "No Docker / no running Django" (`AGENTS.md`).

## 0. Ringkasan satu layar

| Pertanyaan                                  | Jawaban                             | Label                             |
| ------------------------------------------- | ----------------------------------- | --------------------------------- |
| Lib crate sebagai bahan core (`crates/pkg`) | **±65% selesai**                    | inference (dihitung per primitif) |
| Kontrak + harness paritas                   | **±70% selesai**                    | observation + inference           |
| Core (data + route + handler)               | **±2%** — 2 dari 720 endpoint hijau | observation                       |
| Total program (berbobot pw §16)             | **±13%** — ≈10,4 dari 81 pw         | inference                         |

Satu kalimat: **bahannya sudah mayoritas, core-nya belum.** Lib fondasi siap dipakai untuk handler tanpa database, tetapi belum ada satu pun blok wajib untuk endpoint baca nyata (pool + query ber-scope + serializer + pagination).

## 1. Inventaris kode Rust (observation)

Angka LOC di dokumen ini **menghitung baris kosong** (dibaca dengan `read_text().splitlines()`), jadi berbeda dari hitungan `Measure-Object -Line` yang melewati baris kosong.

| Crate     | Berkas `.rs` | LOC src    | LOC test  | Total      | Isi                                       |
| --------- | ------------ | ---------- | --------- | ---------- | ----------------------------------------- |
| `pkg`     | 31           | 11.928     | 1.806     | 13.734     | primitif paritas Django/DRF               |
| `api`     | 20           | 612        | 1.349     | 1.961      | router, middleware, wire parity, 501 stub |
| `domain`  | 2            | 150        | 0         | 150        | `RouteSpec`, `ROUTES`, manifest           |
| `db`      | 3            | 81         | 0         | 81         | `RepoError`, `Cursor`, `Page` (stub)      |
| `migrate` | 1            | 32         | 0         | 32         | stub                                      |
| `worker`  | 1            | 26         | 0         | 26         | stub                                      |
| **Total** | **58**       | **12.829** | **3.155** | **15.984** | —                                         |

Status uji (observation, `cargo test --all-targets`): **236 lulus, 1 ignored, 0 gagal**; `--list` melaporkan **237** kasus. Enam suite golden di `crates/pkg/tests` (1.806 LOC) dan enam suite di `crates/api/tests` (1.349 LOC).

Ukuran artefak (observation): `target/release/plane-api.exe` = **2,4 MB** (2.507.776 byte). Angka ini kecil karena `sqlx` belum di-wire — lihat §3 blok #1.

## 2. Lib fondasi: yang SUDAH jadi (observation)

`crates/pkg` = 11.928 LOC src, 217 item `pub`, 7 golden JSON (133.074 byte) + 89 direktori wire golden (401 berkas, 54.966 byte); total `contracts/golden/` = **408 berkas / 188.040 byte**, dan ada 11 skrip `capture_*` di `contracts/tools/`.

Dari 89 direktori golden itu, **17 adalah kasus endpoint nyata** yang sudah direkam dari Django: `issue-list`, `issue-detail`, `project-list`, `project-detail`, `state-list`, `label-list`, `member-list`, `cycle-list`, `users-me`, `users-me-workspaces`, `workspace-detail`, `workspaces-root`, `unicode-drf`, `web-root`, `web-robots`, `django-root`, `django-robots`. Sisanya kasus primitif: `parser-*`, `envelope-*`, `error-*`, `serializer-*`. Artinya oracle untuk sebagian endpoint baca **sudah ada**; yang belum adalah handler + repo di Rust untuk memakainya.

| Modul                                                                          | LOC   | Bukti pengunci                                              | Status                                  |
| ------------------------------------------------------------------------------ | ----- | ----------------------------------------------------------- | --------------------------------------- |
| `parser/*` — charset, content-type, json body, form, multipart (+2 tabel data) | 4.675 | `parser_golden.rs` (468), `parser-json-errors.json`         | selesai                                 |
| `config.rs` — settings, CORS, cookie secure, redis, retention                  | 1.595 | `config-helpers.json`, `capture_config.py`                  | selesai                                 |
| `json.rs` — encode/decode setara DRF JSONRenderer                              | 1.116 | `serializer_golden.rs` (126), `serializer-float-cases.json` | selesai                                 |
| `logging.rs` — format access log Django                                        | 925   | `logging_golden.rs` (248), `logging-cases.json`             | selesai                                 |
| `cookie.rs` — parse/serialize cookie                                           | 461   | `middleware_golden.rs` (204), `middleware-cases.json`       | selesai                                 |
| `password.rs` — `pbkdf2_sha256`                                                | 416   | `password_golden.rs` (243), `password-cases.json`           | selesai                                 |
| `throttle.rs` — rate limit DRF                                                 | 398   | `auth_golden.rs` (368), `auth-cases.json`                   | selesai                                 |
| `cors.rs`                                                                      | 327   | `middleware_golden.rs`                                      | selesai                                 |
| `actor.rs` — resolusi user dari header/cookie                                  | 300   | `auth_golden.rs`                                            | sebagian (tanpa session backend)        |
| `api_key.rs` — auth API key                                                    | 299   | `auth_golden.rs`                                            | selesai                                 |
| `error.rs` + `envelope.rs` — error DRF 404/405/415/429                         | 443   | `error_golden.rs` (368)                                     | selesai                                 |
| `python_repr.rs` — float repr, `iso8601`, offset timezone                      | 234   | `serializer-float-cases.json`                               | selesai                                 |
| `session.rs` — cookie path, `SAFE_METHODS`, origin check                       | 217   | 4 unit test hijau                                           | sebagian (**belum ada `SessionStore`**) |
| `origin.rs` — CSRF/origin                                                      | 207   | `middleware_golden.rs`                                      | selesai                                 |
| `gzip.rs`                                                                      | 169   | —                                                           | selesai                                 |
| `security.rs`                                                                  | 112   | —                                                           | selesai                                 |
| `lib.rs`                                                                       | 34    | —                                                           | —                                       |

Kesimpulan lapisan ini: semua yang bersifat **murni, deterministik, dan tidak menyentuh database** sudah punya golden. Itu sebabnya `pkg` menyerap 93% kode Rust yang ada.

## 3. Lib fondasi: yang BELUM ada (ini yang menahan core)

| #   | Blok yang hilang                                                                                | Est.   | Kenapa mahal                                                                                                                       | Bukti status                                                                                                                                               |
| --- | ----------------------------------------------------------------------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | **DB pool + repo + migration runner**                                                           | 3–4 pw | `sqlx` belum masuk dependency; `.sqlx` offline cache belum ada sehingga `sqlx::query!` tidak bisa dikompilasi tanpa `DATABASE_URL` | `crates/db` 81 LOC (`RepoError`, `Cursor`, `Page`), `crates/migrate` 32 LOC; `plane-api` deps: axum, chrono, domain, pkg, serde_json, tokio, tracing, uuid |
| 2   | **Queryset parity** — filter, ordering, prefetch, annotate, soft-delete, `select_related`       | 6–8 pw | harus meniru semantik ORM Django persis, termasuk `deleted_at IS NULL` implisit dan scope `workspace_id`                           | 0                                                                                                                                                          |
| 3   | **Serializer + validation parity** — tipe field, `required`, choices, nested, bentuk error dict | 5–7 pw | bentuk error field/non-field DRF adalah bagian kontrak HTTP                                                                        | 0 (hanya serializer _output_ untuk float/waktu)                                                                                                            |
| 4   | **Pagination DRF** — cursor + offset, `count/next/previous/results`                             | 1–2 pw | nilai `next`/`previous` dianggap bagian kontrak                                                                                    | stub `Cursor`/`Page`                                                                                                                                       |
| 5   | **Permissions** — workspace role, model permission, object permission                           | 3–4 pw | dipakai hampir semua endpoint; menentukan 403 vs 404                                                                               | 0                                                                                                                                                          |
| 6   | **`SessionStore`** — signing, expiry, rotasi, logout                                            | ±1 pw  | oracle-nya butuh run Django (lihat §6)                                                                                             | `session.rs` 217 LOC, hanya cookie path/origin/`SAFE_METHODS`                                                                                              |
| 7   | **Redis** — cache, session backend, idempotency                                                 | 2–3 pw | —                                                                                                                                  | 0                                                                                                                                                          |
| 8   | **Jobs / worker**                                                                               | 6–8 pw | padanan Celery task + `django_celery_beat`                                                                                         | `crates/worker` 26 LOC                                                                                                                                     |
| 9   | **Storage/S3 + upload + presigned URL**                                                         | 2–3 pw | —                                                                                                                                  | 0                                                                                                                                                          |
| 10  | **Email / notification**                                                                        | 2 pw   | format & template harus sama                                                                                                       | 0                                                                                                                                                          |
| 11  | **Webhook + export CSV/XLSX**                                                                   | 2–3 pw | —                                                                                                                                  | 0                                                                                                                                                          |
| 12  | **Public API v1 + token auth**                                                                  | 3–4 pw | `api_key.rs` ada, route & permission belum                                                                                         | route stub 6 LOC                                                                                                                                           |

Tambahan yang tidak masuk tabel karena di luar fokus: realtime `apps/live` (Fase 9), admin (`apps/admin` API), dan OpenAPI/schema (`drf-spectacular`).

## 4. Metode hitung persentase (supaya bisa diaudit)

Dua metode dipakai dan hasilnya berbeda tujuannya.

### 4.1 Per primitif (untuk "bahan lib")

Dari daftar kebutuhan lapisan fondasi (~30 primitif): **15 selesai, 3 sebagian, 12 belum** (§2 dan §3). Bobot tiap primitif tidak sama, jadi angka "±65%" adalah titik tengah: hitungan per item memberi ±60%, tetapi item yang belum ada (ORM, serializer, permission) jauh lebih berat per unit daripada yang sudah ada (crypto, parser, cookie).

### 4.2 Berbobot person-week dari `docs/rust-api.md` §16 (untuk "total program")

Titik tengah estimasi §16 = **81 pw** (rentang 69–93).

| Bucket §16                         | pw     | Estimasi selesai | Kontribusi pw | Dasar                                                                                   |
| ---------------------------------- | ------ | ---------------- | ------------- | --------------------------------------------------------------------------------------- |
| Fondasi & infra                    | 5      | 60%              | 3,0           | `pkg` primitif §2                                                                       |
| Test harness & kontrak             | 9      | 70%              | 6,3           | 7 golden JSON, 89 direktori wire golden, `route_parity.py`, `wire_golden`, `just check` |
| Migrasi cermin                     | 6      | 0%               | 0             | `crates/migrate` stub                                                                   |
| Read parity                        | 12     | 2%               | 0,24          | 2 dari 720 endpoint                                                                     |
| Write parity + activity/versioning | 14     | 0%               | 0             | —                                                                                       |
| Auth + API v1 + live               | 9      | 10%              | 0,9           | `api_key`, `password`, `throttle`, `actor`; `SessionStore` belum                        |
| Job + email + webhook + export     | 14     | 0%               | 0             | `worker` stub                                                                           |
| Public + admin                     | 6      | 0%               | 0             | —                                                                                       |
| Cutover + dekomisi                 | 6      | 0%               | 0             | —                                                                                       |
| **Total**                          | **81** | —                | **10,4**      | **≈13%**                                                                                |

Angka turunan yang sering ditanyakan:

- **Lib/fondasi (bahan)**: ±65% (§4.1) atau ±70% bila harness ikut dihitung sebagai bahan.
- **Core**: 2/720 endpoint = **0,28%** hijau (`contracts/endpoint-status.json`: `green: 2`, `red: 718`); 391 route family di `contracts/routes.json`; `plane_domain::ROUTES` baru **2 entri** (`robots.txt`, `""`).
- **Total**: **±13%**.

## 5. Arti praktis: bahan ini cukup untuk apa?

**Bisa ditulis hari ini tanpa lib baru (observation, sudah terbukti hijau):**

- endpoint statis/tanpa DB: `GET /`, `/robots.txt` (dua-duanya `implemented: true`);
- seluruh jalur error DRF: 404/405/413/415/429, envelope exception/validation/parse;
- access log, gzip, CORS, origin/CSRF check, security header;
- throttle per scope, auth API key, baca/tulis cookie, parse body (JSON/form/multipart), hash password, format waktu & float.

**Belum bisa ditulis (blocker nyata):**

Satu endpoint baca biasa, misalnya `GET /api/workspaces/<slug>/projects/`, memerlukan **empat** blok sekaligus: (a) pool + repo, (b) query dengan soft-delete + scope workspace + ordering, (c) serializer output + pagination, (d) permission workspace. Tidak ada satu pun yang tersedia hari ini. Golden untuk endpoint itu sendiri sudah direkam (`project-list`, `workspaces-root`), jadi pekerjaannya bukan "mencari tahu bentuk response", melainkan "menyediakan empat blok tadi lalu mencocokkan byte".

## 6. Batas atas selama aturan "No Docker / no running Django" berlaku

Aturan di `AGENTS.md` melarang menjalankan Docker, `manage.py`, `runserver`, dan `pytest` untuk `apps/api`. Konsekuensi langsung pada angka progres:

1. **`cargo sqlx prepare` tidak bisa dijalankan** (butuh database hidup) sehingga `.sqlx/` tidak bisa dibuat ⇒ `sqlx::query!` tidak bisa dikompilasi ⇒ **blok #1–#4 di §3 tidak bisa dinaikkan lewat kode query nyata**. Kartu `[READ]`, `[WRITE]`, `[DATA]`, `[MIGRASI]` tetap `Blocked`.
2. **`capture_session.py` belum selesai** dan `contracts/golden/session-cases.json` belum ada; kartu `[SPIKE] Sesi Django-compatible (Opsi A/B)` sudah dipindah ke `Blocked` dengan alasan tertulis. Tiga item DoD-nya (cookie `session-id` nyata, expiry/signing/rotasi/logout, dua arah Django↔Rust) berstatus `unknown`.
3. **`capture_errors.py` tidak boleh dijalankan** selama aturan aktif; kasus error yang belum terekam harus ditandai `unknown` di kartu, bukan direkayasa.

Progres yang **masih mungkin** tanpa mencabut aturan (inference): `pkg` dari ±65% → **±75%**, dengan pekerjaan berikut — semuanya bisa diuji tanpa DB:

- `SessionStore` murni (encode/decode `signing.dumps`, `key_salt`, 128 karakter `new_session_key`, `get_expiry_age`/`get_expiry_date`) dengan oracle dari pembacaan source;
- struct pagination (`Page`, `Cursor`, bentuk `count/next/previous/results`);
- filter DSL sebagai nilai (parse query string → predikat) yang belum mengeksekusi SQL;
- serializer output tipe dasar (string/int/bool/null/date) memakai golden `serializer-*` yang sudah ada.

Yang **tidak mungkin** naik tanpa DB: satu pun endpoint `green` baru. Selama aturan ini aktif, angka `green` tetap **2**.

## 7. Urutan langkah untuk menaikkan persentase

1. **Selesaikan `capture_session.py`** — tambahkan `hashlib`, `hmac`, `secrets`, `InvalidAlgorithm` ke namespace signing; jalankan dua kali dan pastikan output byte-identik; simpan `contracts/golden/session-cases.json`.
2. **`SessionStore` di `crates/pkg/src/session.rs`** + `crates/pkg/tests/session_golden.rs` + resep `golden-session` di `apps/api-rs/justfile`.
3. **Pagination + filter DSL murni** di `crates/pkg` (diuji tanpa DB).
4. Setelah aturan run dicabut: **`sqlx` + `.sqlx` offline cache → pool → satu repo nyata → satu endpoint baca hijau**. Satu endpoint hijau ini membuka gerbang seluruh kartu `[READ]`.
5. Target berikutnya: MVP-A (`docs/rust-api.md` §16) = ±10–12 pw untuk naik dari **13% → ±25%** (inference).

## 8. Cara memperbarui angka di dokumen ini

Semua angka di atas dapat direproduksi. Jalankan dari akar repo dengan Python yang ditargetkan API (`apps/api/.venv/Scripts/python.exe`, 3.12):

```powershell
# LOC per crate (menghitung baris kosong), terpisah src vs tests
& 'apps/api/.venv/Scripts/python.exe' -c "import pathlib; b=pathlib.Path('apps/api-rs/crates'); L=lambda p: len(p.read_text(encoding='utf-8',errors='ignore').splitlines()); print([(c.name, sum(L(p) for p in c.rglob('*.rs') if 'target' not in p.parts and '/tests/' not in p.as_posix()), sum(L(p) for p in c.rglob('*.rs') if '/tests/' in p.as_posix())) for c in sorted(b.iterdir()) if c.is_dir()])"

# status endpoint dan jumlah route family
& 'apps/api/.venv/Scripts/python.exe' -c "import json,pathlib,collections; d=json.loads(pathlib.Path('contracts/endpoint-status.json').read_text(encoding='utf-8')); print(collections.Counter(v['status'] for v in d['statuses'].values())); print('routes', len(json.loads(pathlib.Path('contracts/routes.json').read_text(encoding='utf-8'))['routes']))"

# jumlah kasus uji, hasil, biner, dan inventaris golden
cd apps/api-rs; cargo test --all-targets -- --list 2>&1 | Select-String ': test$' | Measure-Object
cargo test --all-targets 2>&1 | Select-String 'test result:'
Get-Item 'target/release/plane-api.exe' | Select-Object Length
Get-ChildItem '../../contracts/golden' -Recurse -File | Measure-Object -Property Length -Sum
```

Gate yang harus hijau bersamaan (lihat `apps/api-rs/justfile`): `just fmt`, `just lint`, `just test`, `just manifest`, `just parity`, `just e2e`.

## 9. Yang masih unknown

| Hal                                                               | Kenapa belum diketahui         | Cara menutup                                            |
| ----------------------------------------------------------------- | ------------------------------ | ------------------------------------------------------- |
| Format cookie sesi nyata (`session-id` + `admin-session-id`)      | perlu run Django untuk merekam | `capture_session.py` selesai + satu request login nyata |
| Rotasi/expiry saat logout                                         | idem                           | idem                                                    |
| Kompatibilitas dua arah Django↔Rust untuk sesi                    | butuh dua runtime hidup        | uji silang setelah aturan run dicabut                   |
| Bentuk baris tabel `sessions` (128 vs 32 karakter, `device_info`) | butuh DB untuk inspeksi        | inspeksi setelah DB tersedia                            |
| RSS nyata `plane-api` di 2 vCPU / 2 GB                            | butuh proses hidup + beban     | benchmark terkontrol (lihat Lampiran A)                 |

## Lampiran A. Bahasa: Rust vs Go untuk VPS 2c2g

Keputusan ini terpisah dari progres, tetapi muncul setiap kali angka di atas dibaca.

| Dimensi                                   | Rust (kondisi sekarang)                                      | Go (greenfield)                                                                                              |
| ----------------------------------------- | ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------ |
| File Go di repo                           | **0** (`git ls-files '*.go'`)                                | 0                                                                                                            |
| Kode yang harus ditulis ulang bila pindah | 0                                                            | ±12,8k LOC logika paritas di `pkg`/`api`                                                                     |
| Yang tetap bisa dipakai bila pindah       | —                                                            | 7 golden JSON + 89 direktori wire golden + 11 `capture_*` + `route_parity.py` (golden tidak bahasa-spesifik) |
| Waktu ke endpoint pertama byte-parity     | harness sudah ada                                            | ±1–2 minggu membangun harness setara                                                                         |
| Ukuran binary                             | 2,4 MB sekarang; realistis 8–15 MB setelah sqlx+rustls+hyper | 10–18 MB statis (`CGO_ENABLED=0`)                                                                            |
| Estimasi RSS                              | 25–60 MB                                                     | 30–90 MB (bisa dibatasi `GOMEMLIMIT`)                                                                        |
| Build di mesin 2 vCPU                     | cold release 8–20 menit, RAM 1,5–2 GB                        | 40–90 detik, RAM ±500 MB                                                                                     |
| Cross-compile                             | perlu target musl / `cross`                                  | trivial (`GOOS=linux GOARCH=amd64 CGO_ENABLED=0`)                                                            |
| Friksi harian                             | `sqlx::query!` butuh DB saat compile (`.sqlx` belum ada)     | `pgx`+`sqlc` tidak butuh DB saat build                                                                       |

Kesimpulan: **untuk backlog ini Rust lebih cepat**, karena bahan dan gate-nya sudah ada. Keunggulan Go yang nyata hanya **waktu build dan cross-compile**, dan keduanya bisa dimitigasi tanpa ganti bahasa: build di CI lalu kirim artefak, `lto = "thin"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`, serta `cargo sqlx prepare` untuk menghapus friksi `query!`.

## Lampiran B. Anggaran RAM VPS 2 vCPU / 2 GB (estimasi, belum diukur)

| Komponen                             | Stack sekarang (Django)                          | Setelah Rust            | Hipotetis Go     |
| ------------------------------------ | ------------------------------------------------ | ----------------------- | ---------------- |
| OS + systemd + proxy                 | 150–250 MB                                       | 150–250 MB              | 150–250 MB       |
| PostgreSQL (`shared_buffers` 256 MB) | 350–500 MB                                       | 350–500 MB              | 350–500 MB       |
| Redis                                | 30–80 MB                                         | 30–80 MB                | 30–80 MB         |
| API                                  | gunicorn 4–8 worker × 80–150 MB = **0,6–1,2 GB** | 25–60 MB                | 30–90 MB         |
| Worker/beat + RabbitMQ               | **±0,4–0,7 GB**                                  | `plane-worker` 20–40 MB | 25–50 MB         |
| `apps/live` (Node, sampai Fase 9)    | 150–300 MB                                       | 150–300 MB              | 150–300 MB       |
| **Total**                            | **±1,7–3,0 GB (mepet/OOM)**                      | **±0,75–1,25 GB**       | **±0,75–1,3 GB** |

Penghematan besar datang dari menghilangkan interpreter + broker (Django+gunicorn+Celery+RabbitMQ → dua proses Rust), **bukan** dari memilih Rust ketimbang Go. Selisih Rust vs Go di sini lebih kecil daripada variasi satu setelan PostgreSQL.

---

Dokumen terkait: `docs/rust-api.md` (rencana, kontrak, fase, estimasi), `apps/api-rs/README.md` (cara menjalankan), `contracts/golden/` (oracle), `contracts/tools/` (skrip capture).
