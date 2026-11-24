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

## Task completion: Trello update, then git push

Setiap task selesai wajib melewati dua langkah ini, berurutan, tanpa menunggu diminta dua kali:

1. **Update Trello** - centang item Definition of Done yang benar-benar selesai di kartu, tulis bukti (perintah yang dijalankan + hasilnya), lalu pindahkan kartu ke list yang sesuai (`progress` -> `review` bila PR belum dibuka/merge, `done` bila DoD lengkap dan bukti tertaut, `Blocked` bila terhenti). Ubah `docs/trello/plane-rust-cards.json` (sumber kebenaran), jalankan `docs/trello/import-cards.ps1`, dan commit JSON + CSV bersama kode.
2. **`$gitpush`** - skill: `C:\Users\nhaid\.agents\skills\gitpush\SKILL.md`. Verifikasi dulu (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, atau `pnpm check` sesuai paket yang berubah), commit per topik dengan format `[Tipe] deskripsi`, lalu push ke branch aktif.

Aturan tambahan:

- Laporan akhir ke user menyebut: hash commit, branch, kartu Trello yang di-update, dan hasil verifikasi.
- Jika sebuah langkah sengaja dilewati (mis. test tidak relevan, kartu belum bisa `done` karena PR belum merge), sebutkan alasannya di laporan dan di kartu.
- Jangan `--force`, jangan amend commit yang sudah di-push, dan jangan push ke branch lain tanpa permintaan eksplisit.

## Pull requests

Open PRs against `preview`. New `.py`, `.ts` and `.tsx` files need the `COPYRIGHT.txt` header (checked in CI, see `COPYRIGHT_CHECK.md`).
