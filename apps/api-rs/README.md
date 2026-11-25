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

## Route contract

`contracts/routes.json` is generated from Django\'s resolver and frozen in git.
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

`contracts/golden/middleware-cases.json` records 28 cases from Django 5.2.17,
django-cors-headers 4.9.0, and DRF 3.18.3 through
`contracts/tools/capture_middleware.py` (no Docker, no database).
`cargo test --test middleware_golden` replays both halves: the pure decisions in
`plane-pkg` and the responses of a spawned `plane-api`.

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
