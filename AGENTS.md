# Agent Development Guide

## Commands

- `pnpm dev` - Start all dev servers (web:3000, admin:3001, space:3002, live:3100); the API runs in Docker via `docker-compose-local.yml` on :8000
- `pnpm build` - Build all packages and apps
- `pnpm check` - Run all checks (format, lint, types)
- `pnpm check:lint` - OxLint across all packages
- `pnpm check:types` - TypeScript type checking
- `pnpm fix` - Auto-fix format and lint issues
- `pnpm turbo run <command> --filter=<package>` - Target specific package/app

## Code Style

- **Imports**: Use `workspace:*` for internal packages, `catalog:` for external deps
- **TypeScript**: Strict mode enabled, all files must be typed
- **Formatting**: oxfmt, run `pnpm fix:format`
- **Linting**: OxLint with shared `.oxlintrc.json` config
- **Naming**: camelCase for variables/functions, PascalCase for components/types
- **Error Handling**: Use try-catch with proper error types, log errors appropriately
- **State Management**: MobX stores live in each app's `store/` folder (`apps/web/store`, `apps/space/store`, `apps/admin/store`); stores shared across apps go in `packages/shared-state`
- **Testing**: All features require unit tests, use existing test framework per package
- **Components**: Primitives come from the published `@makeplane/propel` npm package (`@makeplane/propel/components/*`, `elements/*`, `icons`); composite/Plane-specific components live in `@plane/blocks` (`packages/blocks`, subpath imports only, e.g. `@plane/blocks/toast`)

## Backend dependencies (uv)

`apps/api` dependencies are managed with [uv](https://docs.astral.sh/uv/): declared in `apps/api/pyproject.toml`, pinned in `apps/api/uv.lock` (commit both). Groups: `dev` and `test` (installed by default; they use `psycopg-binary`), `production` (gunicorn + the source-built `psycopg-c`; production image and migration-check only). Lint config lives in `apps/api/ruff.toml`.

- uv must be exactly the `required-version` in `apps/api/pyproject.toml` (`uv self update <version>`); bump it together with the uv image in both Dockerfiles
- Set up a local env: `cd apps/api && uv sync`
- Inside the dev/test containers, always pass `--inexact` (`uv sync --inexact`): uv targets the image's system Python there, and an exact sync would uninstall pip
- Add/upgrade a dependency: `uv add <pkg>==<version>` (or `--group test`/`--group dev`), or edit `pyproject.toml` and run `uv lock`
- Run tools: `uv run python manage.py ...`, `uv run ruff check .`

## Migrations

- Generate migrations with `uv run python manage.py makemigrations <app> -n <name>`. The command also rewrites `apps/api/migrations_lockfile.txt`; commit it with the migration and never edit it by hand
- A merge conflict in the lockfile means another PR added a migration to the same app first: merge `preview`, delete your migration, and run `makemigrations` again
- `migration-check.yml` fails a PR that touches migrations when the lockfile does not match the migration graph

## Backend tests (Docker)

The Django/pytest suite for `apps/api` runs in an isolated stack defined by `docker-compose-test.yml` at the repo root.

Prereq (once): `./setup.sh` — generates `apps/api/.env` from `.env.example`.

- Full suite: `docker compose -f docker-compose-test.yml up --build --abort-on-container-exit --exit-code-from api-tests`
- Subset: `docker compose -f docker-compose-test.yml run --rm --build api-tests pytest -m unit`
- Teardown: `docker compose -f docker-compose-test.yml down -v`

See `apps/api/tests/RUNNING_TESTS.md` for the full walkthrough and troubleshooting; see `apps/api/plane/tests/TESTING_GUIDE.md` for test conventions and fixtures.

## Translations

Strings live in `packages/i18n/src/locales/<lang>/<namespace>.json`; `en` is the source of truth and every locale must have the same keys. Use the `translate` skill when changing locale files, and run `pnpm --filter=@plane/i18n sync:check` (CI runs it with `--ci`).

## Work tracking (Trello)

- **Every card you work on must be in the `progress` list before you change any file.** A branch, a commit, or an uncommitted edit without the card in `progress` is out of process.
- **WIP limit: exactly one card in `progress` per person or agent.** Do not start a second card until the current one is done or explicitly moved back out of `progress`.
- Board `Plane Rust Migration`; the source of truth for lists and cards is `docs/trello/plane-rust-cards.json`. The workflow lists `todo`, `progress`, `review`, `done`, and `Blocked` are created by `docs/trello/import-cards.ps1`; do not rename them.
- Card lifecycle: `todo` -> `progress` (work starts) -> `review` (PR opened) -> `done` (merged + evidence). Use `Blocked` when stopped, and record the blocker on the card.
- One PR per independently reviewable change. Never mix unrelated cards in one PR. A card may span multiple stacked PRs; trivial fixes (typos, dependency bumps, docs) may share a PR.
- A card may leave `progress` only when its Definition of Done checklist is complete and the parity/verification evidence is linked in the card or PR.
- Waiting on CI, review, or another person does not free the WIP slot. Exceptions require explicit approval.

## Required: three skills on every task

Every task in this repo must use all three skills below without being asked again:

1. **`rust-skills`** - <https://github.com/leonardomso/rust-skills>
   - Local copy: `C:\Users\nhaid\.agents\skills\rust-skills\SKILL.md` (265 rules under `rules/`, one folder per category).
   - Required when writing, changing, reviewing, or debugging Rust code in `apps/api-rs`: ownership/borrowing, error handling, async/Tokio, API design, serde, testing, and performance tuning.
   - Pick the rules relevant to the diff instead of reading all 265 files. Name the rules used in the final report when a design decision depends on them.
   - `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all -- --check` remain the final gates; this skill does not replace them.

2. **`reverse-engineer-anything` (REA)** - <https://github.com/morluto/rea>
   - Local copy: `C:\Users\nhaid\.agents\skills\reverse-engineer-anything\SKILL.md` (plus the `references/` folder).
   - Required when a claim depends on shipped artifacts, decompiled output, or runtime evidence: real Django/DRF behavior, response bytes, headers, status, and version comparisons.
   - For full source-repository analysis, keep using the normal repo tools; REA is used for its evidence discipline, not as a replacement for reading the source.
   - Every finding must be labeled as observation, inference, or unknown, and backed by evidence (command + result, or an Evidence ID when the REA MCP is active).
   - Do not run routine `rea setup`/`rea doctor` when the MCP tools are registered and not stale.

3. **`rust-anything`** - <https://github.com/erkamyaman/rust-anything>
   - Local copy: `C:\Users\nhaid\.agents\skills\rust-anything\SKILL.md` (plus `whole-app.md`). CLI: `cargo install rust-anything`, then `rust-anything setup`; verify with `rust-anything --version` and run `rust-anything doctor` only when a step fails.
   - Required on every task for planning and evidence: `analyze` (unit plan, `start_here`/`blocked`/`keep`), `profile` when speed is the goal, and `record`/`diff`/`bench`/`gate` only when a bridge crate exists for the unit. The installed CLI is v4.1.0 (crates.io stable); commands added in `main` (v5), such as `rules` and `app drift`, are not available yet, so do not plan around them.
   - Planning-and-evidence only: this repo does not execute Django or DRF (see the rule below), so the oracle is static. Record behavior by extracting the real helper/route code from `apps/api` into `contracts/golden/` with a `contracts/tools/capture_*.py` script that parses the source (`ast`) instead of importing Django; `contracts/tools/capture_config.py` is the reference for this pattern. `bridge`, `record`, and `gate` need a bridge crate, so when `analyze` marks the unit `blocked` or export-less (e.g. `plane/settings/`), say so on the card and verify with the repo gates instead of forcing a port through the skill.
   - The hard boundaries in `docs/rust-api.md` §0 override the skill's instructions: do not use `enable`/`disable` (the skill edits source files), do not edit `apps/api/**`, `apps/live/**`, or the frontend, and do not put PyO3/napi/cgo bridges in the production path. The workspace stays in `apps/api-rs/crates/*`, not a new `rust/` tree.
   - The repo's oracle and gates remain authoritative: `contracts/tools/capture_*.py`, `contracts/golden/`, `wire_golden`, `route-parity`, and `just check`. This skill adds evidence discipline; it does not replace those gates.
   - Mention the skill commands used (or why they were not relevant) in the final report when a port decision depends on them.

How to use them:

- Use all three together: `rust-anything` for the port plan and evidence, REA to record the oracle and contract boundaries, `rust-skills` for the implementation shape.
- State plainly in the report which step was observation, which was inference, and which stayed unknown, when a claim rests on source that was only read statically.
- Read the relevant skill before writing code, not after a review fails.
- If a skill is deliberately skipped because the task is outside its scope (e.g. a docs-only change), state the reason in the final report.

## No Docker, no running Django (in effect until revoked)

Neither Docker nor a running Django/DRF process is used for the current work. Do not start, restart, or wait for the `community`/`docker-compose-test.yml` stack, and do not run `manage.py`, `runserver`, `pytest` for `apps/api`, or any script that boots Django settings, before coding. Reading `apps/api` source is always allowed; executing it is not.

- Rust verification is local only: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, `cargo run -p plane-api --bin route-manifest`.
- Behavior claims come from static evidence: read the Django/DRF source at the line that decides the output, then freeze the decision in `contracts/golden/` with a `contracts/tools/capture_*.py` script. A capture script may parse source (`ast`) and call the extracted pure helpers, but it must not import Django or DRF, hit a database, or start a server. `contracts/tools/capture_config.py` is the reference: it executes `_cors_settings`, `_cookies_secure`, `_redis_ssl_kwargs`, `_retention_days`, and `is_valid_url` straight out of `plane/settings/common.py` and `plane/utils/url.py`.
- Existing goldens recorded earlier from a real venv keep their provenance: Django 5.2.17, DRF 3.18.3. `contracts/tools/capture_errors.py` stays the named path for error goldens, but do not run it while this rule is active; if a case is missing, mark it `unknown` on the card instead of inventing bytes. `contracts/tools/capture-internal.sh` is a cutover tool and is not a prerequisite.
- Run a capture with the interpreter the API targets: `apps/api/.venv/Scripts/python.exe` (3.12, per `apps/api/pyproject.toml` `>=3.12,<3.13`), never the system Python. `urlparse` changed between 3.10 and 3.12, so a golden recorded with the wrong interpreter is invalid evidence; captures stamp their version in the output and the Rust golden test rejects a non-3.12 recording.
- Label every finding as observation (line read + quoted decision), inference (Rust port of that decision), or unknown (needs a run that this rule forbids be run).
- The Django/pytest suite in `apps/api` and any `pnpm dev` that needs Docker are deferred; do not turn them into blockers for Rust tasks.
- If a task can only be verified by running Docker or Django, write the reason on the Trello card, move it to `Blocked`, and take another task.

## Task completion: Trello update, then git push

Every finished task must go through these two steps, in order, without waiting to be asked twice:

1. **Update Trello** - tick the Definition of Done items actually completed on the card, write the evidence (commands run + results), then move the card to the right list (`progress` -> `review` when the PR is not open/merged yet, `done` when the DoD is complete and evidence is linked, `Blocked` when stopped). Edit `docs/trello/plane-rust-cards.json` (source of truth), run `docs/trello/import-cards.ps1`, and commit the JSON + CSV together with the code.
2. **`$gitpush`** - skill: `C:\Users\nhaid\.agents\skills\gitpush\SKILL.md`. Verify first (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, or `pnpm check` depending on the changed packages), commit per topic in `[Type] description` format, then push to the active branch.

Additional rules:

- The final report to the user states: commit hash, branch, Trello card(s) updated, and verification results.
- If a step is deliberately skipped (e.g. tests are irrelevant, the card cannot be `done` because the PR is not merged), state the reason in the report and on the card.
- Do not `--force`, do not amend commits that were already pushed, and do not push to another branch without an explicit request.

## Pull requests

Open PRs against `preview`. New `.py`, `.ts` and `.tsx` files need the `COPYRIGHT.txt` header (checked in CI, see `COPYRIGHT_CHECK.md`).
