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
