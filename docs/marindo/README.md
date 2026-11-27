# Marindo workspace notes

`workspace-marindo.md` documents the `Marindo` workspace in the deployed Plane
instance: its projects, members, and every card with its description and
comments. It is generated from the restored Postgres dump, read-only, with no
Django involved.

That file is **gitignored** because the production dump contains real employee
names, emails, and internal meeting notes. `workspace-marindo.example.md` is the
redacted, committable version of the same structure.

## Where the file comes from

| Item      | Value                                                           |
| --------- | --------------------------------------------------------------- |
| Source    | `testing/ops/backup_plane_db.py` dump of `plane-app-plane-db-1` |
| Local DB  | WSL Ubuntu, PostgreSQL 16, `127.0.0.1:5432`                     |
| Workspace | `Marindo`, slug `marindo`                                       |

To regenerate, see the query block at the end of the document itself.

## Rules

- Do not commit `workspace-marindo.md`; it holds PII.
- Do not paste the dump or this file into a PR, an issue, or a chat.
- Complements `docs/trello/plane-rust-cards.json`, which covers the Rust
  migration board. This folder covers the Plane product data, not the migration
  plan.
