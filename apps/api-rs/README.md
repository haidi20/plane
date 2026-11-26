# plane-api-rs

Rust backend for Plane. The Django app in `apps/api` stays the source of truth
for schema and HTTP contracts until cutover; this workspace only reproduces
them.

## Layout

| Crate           | Binary                        | Responsibility                                    |
| --------------- | ----------------------------- | ------------------------------------------------- |
| `plane-domain`  | -                             | business rules, route manifest (no axum, no sqlx) |
| `plane-db`      | -                             | repositories, pagination, soft delete             |
| `plane-pkg`     | -                             | config, JSON renderings, logging                  |
| `plane-api`     | `plane-api`, `route-manifest` | axum router, handlers, middleware                 |
| `plane-worker`  | `plane-worker`                | background jobs                                   |
| `plane-migrate` | `plane-migrate`               | mirrored SQL runner                               |

## Commands

```bash
cd apps/api-rs
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

`just` wraps the same sequence:

```bash
just check     # fmt + check + clippy + test
just parity    # regenerate manifest and compare with contracts/routes.json
just e2e       # wire_golden against the frozen Django cases
```

`justfile` uses `sh -cu` rather than `bash -cu`. On Windows the `bash` on
`PATH` is usually WSL's, which has no `cargo`, and every recipe then exits 127.
Git Bash's `sh` runs the same recipes and works on Linux and macOS unchanged.

## Continuous integration

`.github/workflows/pull-request-build-lint-rust.yml` runs three jobs on every PR
that touches `apps/api-rs/**` or `contracts/**`:

| Job              | Steps                                                                                                                   |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `rust-lint`      | `cargo fmt --check`, `cargo check --all-targets`, `cargo clippy -- -D warnings` (all `--locked`)                        |
| `rust-test`      | `cargo test --all-targets` — includes `wire_golden`, which spawns the binary and replays `contracts/golden/**` over TCP |
| `route-contract` | emit the manifest twice and `cmp` them, `route_parity.py --allow-missing`, `contracts/tests`                            |

No job needs Docker, Django, or a database: the Django side is the frozen JSON
and golden files already in the repository. `~/.cargo` and `target/` are cached
per job with `Swatinem/rust-cache`, and the workflow pins 1.90.0 - the same
version as `rust-toolchain.toml` - so CI compiles with the toolchain the hook
uses.

`apps/api-rs/contracts/` is gitignored, so a fresh checkout has no such
directory. `route-manifest --out` creates the parent directory itself; without
that, the `route-contract` job would stop with `NotFound` before comparing
anything. `tests/route_manifest.rs` keeps that behavior pinned.

`--allow-missing` is deliberate while 718 of Django's 722 endpoints are still
missing from Rust. It still fails on an extra route, a converter change, a
duplicate, or a route that claims `owner=rust`/`status=green` without being
served.

Two runs on PR #1, both green:

| Run                                                                             | `rust-lint` | `rust-test` | `route-contract` |
| ------------------------------------------------------------------------------- | ----------- | ----------- | ---------------- |
| [45731269163](https://github.com/haidi20/plane/actions/runs/45731269163) (cold) | 41 s        | 37 s        | 41 s             |
| [45731687455](https://github.com/haidi20/plane/actions/runs/45731687455) (warm) | 18 s        | 29 s        | 18 s             |

The second run restores the `v0-rust-*` caches written by the first, so the
warm numbers are the real cost of a re-push. Both runs finish well inside the
3-minute budget the card sets for a red-on-lint signal.

## Fast feedback

`rust-toolchain.toml` pins 1.90.0 with `rustfmt`, `clippy`, and `rust-src`, so
rust-analyzer, the hook, and CI compile with the same toolchain.
`plane.code-workspace` sets `rust-analyzer.check.command = clippy` with
`--all-targets`, format on save, and the rust-analyzer formatter.

```bash
sh scripts/rust-precommit.sh --all   # fmt -> check -> clippy, timed per step
```

`.husky/pre-commit` runs `pnpm lint-staged` and then this script. It exits
immediately unless the commit stages a `.rs`/`.sql` file or a Cargo/toolchain
config under `apps/api-rs`; a broken file is rejected at `cargo fmt --check`,
before `check` or `clippy` run. `RUST_PRECOMMIT_FAST=1` skips clippy in the hook
(the `just check` gate still runs it).

`.cargo/config.toml` only sets `incremental = true` and three aliases
(`cargo check-all`, `cargo lint`, `cargo fmt-check`). `sccache` and `lld` stay
environment choices (`RUSTC_WRAPPER=sccache`, `RUSTFLAGS=-C link-arg=-fuse-ld=lld`)
because a missing wrapper in the file would break every build.

Measured on this machine (warm cache, `apps/api-rs`): `cargo fmt --check` 0.8 s,
`cargo check --all-targets` 5.6 s, `cargo check -p plane-api --all-targets`
0.4 s, `cargo clippy --all-targets` 1.9 s, `cargo test --all-targets` 3.0 s.

## Workspace profile and dependencies

`Cargo.toml` pins the dev profile so the warm-check budget does not depend on
Cargo defaults:

```toml
[profile.dev]
opt-level = 0
debug = 1        # line tables are enough for a breakpoint; faster than debug = 2
incremental = true
```

`crates/api/tests/workspace_profile.rs` reads the workspace manifest and fails
if any of the three values changes.

Measured after that change, `cargo check -p <crate> --all-targets --locked` with
a warm cache:

| Crate           | Time   |
| --------------- | ------ |
| `plane-domain`  | 0.15 s |
| `plane-db`      | 0.16 s |
| `plane-pkg`     | 0.20 s |
| `plane-api`     | 0.25 s |
| `plane-worker`  | 0.20 s |
| `plane-migrate` | 0.13 s |

All six crates are far below the 10-second budget in `docs/rust-api.md` 12.9.

Dependency edges are audited, not guessed: `cargo machete .` reports no unused
dependency. Twelve edges that no crate referenced were removed
(`plane-api` -> `plane-db`, `serde`, `tower`, `tower-http`; `plane-db` ->
`plane-domain`, `serde_json`; `plane-migrate` -> `plane-pkg`, `serde`,
`serde_json`; `plane-worker` -> `plane-db`, `plane-domain`, `serde_json`).
The lockfile lost seven transitive packages (`tower-http`, `async-compression`,
`compression-codecs`, `compression-core`, `tokio-util`, `futures-sink`,
`bitflags`) and still verifies with `--locked`.

## Reproducible release builds

`cargo build --release` on Windows/MSVC was not byte-identical between two
runs: link.exe stamps a timestamp in the COFF header (offset 248) and in the
debug directory. `.cargo/config.toml` now sets `-C link-arg=/Brepro` for the
`x86_64-pc-windows-msvc` target only, so gnu/Linux builds never receive a flag
their linker rejects. Evidence: two consecutive release builds of `plane-api`
after touching a source file both hash to
`857FF65B1BDBA45819E88507B21BAD322D59F90B65937671547BAB8F69465D75`
(2,507,776 bytes).

The dev profile is not reproducible by design (incremental + line tables); use
`--release` when comparing artifacts.

## Route contract

`contracts/routes.json` is generated from Django's resolver and frozen in git.
Every route this workspace serves is declared in `plane-domain/src/route.rs`
first; `route-manifest` emits the same shape so the two can be diffed:

```bash
cargo run -q -p plane-api --bin route-manifest -- --out contracts/routes.rust.json
cd ../.. && python contracts/tools/route_parity.py --rust apps/api-rs/contracts/routes.rust.json
```

Use `--allow-missing` while porting: undeclared routes are expected, everything
else (converters, methods, trailing slash, duplicates) must already match.

## Golden wire cases

`contracts/golden/<case>/` holds real Django request/response pairs. The
`wire_golden` test builds the binary, starts it on an ephemeral port, and
compares responses over TCP. JSON bodies compare as values; raw bodies compare
byte-for-byte.

## Middleware wire

`crates/api/src/wire.rs` installs the Django middleware chain as axum layers, in
Django's order: CORS outermost, then security headers, gzip, and the body limit,
with the request logger inside all of them.

| Layer                                  | Port                  | Notes                                                                                |
| -------------------------------------- | --------------------- | ------------------------------------------------------------------------------------ |
| `CorsMiddleware`                       | `plane_pkg::cors`     | answers a preflight itself, so `OPTIONS` carries no `X-Frame-Options`                |
| `SecurityMiddleware` + `XFrameOptions` | `plane_pkg::security` | four `setdefault` headers: `DENY`, `nosniff`, `same-origin`, `same-origin`           |
| `GZipMiddleware`                       | `plane_pkg::gzip`     | bodies under 200 bytes skip before `Vary` is touched; `gzip;q=0` still compresses    |
| `RequestBodySizeLimitMiddleware`       | `wire::body_limit`    | `DATA_UPLOAD_MAX_MEMORY_SIZE` (`FILE_SIZE_LIMIT`, 5 MiB) -> 413                      |
| rate limit                             | `plane_pkg::throttle` | DRF `SimpleRateThrottle` keys and TTL; `MemoryStore` until the Redis pool card lands |
| origin check                           | `plane_pkg::origin`   | `BaseSessionAuthentication.enforce_csrf`, host-only comparison                       |
| auth extractor                         | `plane_pkg::api_key`  | `X-Api-Key` verbatim, 403 (not 401) for a bad token, `last_used` side effect         |
| session door                           | `plane_pkg::session`  | cookie chosen by substring `instances`, safe methods, 403 on a bad Origin            |
| cookie bytes                           | `plane_pkg::cookie`   | `parse_cookie`, `Morsel.OutputString`, `http_date`, `delete_cookie`                  |
| actor                                  | `plane_pkg::actor`    | `user_id`, token id, source, workspace slug and Django role integer                  |

`contracts/golden/middleware-cases.json` records 28 cases from Django 5.2.17,
django-cors-headers 4.9.0, and DRF 3.18.3 through
`contracts/tools/capture_middleware.py` (no Docker, no database).
`cargo test --test middleware_golden` replays both halves: the pure decisions in
`plane-pkg` and the responses of a spawned `plane-api`.

## Auth

`contracts/golden/auth-cases.json` is recorded by
`contracts/tools/capture_auth.py` with Django 5.2.17 + DRF 3.18.3 and no
database; `APIToken` is stubbed so the capture records the **query** Django
builds (connector, lookups, kwargs) rather than a mocked answer.
`cargo test --test auth_golden` replays it. Four behaviours are easy to get
wrong and are pinned there:

- `X-Api-Key` is read verbatim: no trimming. `None` and `""` are absent (401),
  `"   "` is truthy, looked up, and rejected.
- An invalid, expired or inactive token answers **403**
  `{"detail":"Given API token is not valid"}`, not 401: `APIKeyAuthentication`
  has no `authenticate_header`, so DRF coerces `AuthenticationFailed` to 403.
  The body is DRF's `detail`, not Plane's `error` envelope.
- `Origin` is only checked for unsafe, cookie-authenticated requests, and the
  safe-method comparison is case-sensitive (`"get"` is not safe).
- `SessionMiddleware` picks the cookie by substring: `"instances" in path`, so
  `/api/v1/my-instances/x/` reads `admin-session-id`.

`ApiKeyRateThrottle` does not use DRF's `cache_format`: its key is
`api_key:<raw header>` (measured `:1:api_key:key-abc` in the cache), while
`AuthenticationThrottle` keeps `throttle_authentication_<ip>`. The middleware
golden previously recorded the wrong key and has been corrected.

## Logging

`crates/pkg/src/logging.rs` is the port of `plane/middleware/logger.py` plus the
`LOGGING` blocks in `plane/settings/{local,production}.py`. It writes the same
flat JSON object Django does — `levelname`, `asctime`, `module`, `name`,
`message`, then the seven `extra` fields in insertion order — to **stderr**,
with `ensure_ascii` escaping and no ANSI codes so an aggregator can parse it.

`apps/api-rs` reuses Django's logger names as `tracing` targets, so
`RUST_LOG=plane.api.request=debug` narrows one logger exactly like
`LOGGING["loggers"]` does. Without `RUST_LOG`, `logging::default_filter(debug)`
applies the seven levels from `production.py`.

`contracts/golden/logging-cases.json` is recorded by
`contracts/tools/capture_logging.py` from Plane's source and from the real
`python-json-logger==4.0.0`; `cargo test --test logging_golden` replays it and
`cargo test --test access_log` checks the line a running server writes.

Docker is not required to build or test this workspace, and neither is Django:
only `capture-internal.sh` (the cutover tool) needs a live Django.
