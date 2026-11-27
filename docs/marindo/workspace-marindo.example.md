# Workspace Marindo (Plane) - redacted example

This is the shareable shape of `workspace-marindo.md`. The real file holds the
actual employee names, emails, and meeting notes from the restored production
dump, so it is gitignored. Copy this file, fill it from the local database, and
keep the filled copy out of git.

| Item         | Value                                          |
| ------------ | ---------------------------------------------- |
| Server       | `<ssh-host>`, container `plane-app-plane-db-1` |
| Dump         | `<path-to>.sql` (sha256 `<hash>`)              |
| Local DB     | WSL Ubuntu, PostgreSQL 16, `127.0.0.1:5432`    |
| Workspace id | `<workspace-uuid>`                             |
| Slug         | `marindo`                                      |
| Captured     | `<date>`                                       |

## Summary

| Metric                            | Count         |
| --------------------------------- | ------------- |
| Active projects                   | 2             |
| Soft-deleted projects             | 1             |
| Active cards                      | 9             |
| Comments                          | 13            |
| Labels / cycles / modules / pages | 0 / 0 / 0 / 0 |
| Workspace members                 | 7             |

## Projects

| Identifier | Name     | Status       | Cards | Notes                    |
| ---------- | -------- | ------------ | ----- | ------------------------ |
| `ALLTASK`  | All Task | active       | 8     | Main operational backlog |
| `SOP`      | sop      | active       | 1     | Placeholder              |
| `TES`      | Tes      | soft-deleted | 0     | Test project             |

State workflow for both active projects: Backlog, Todo, In Progress, Done,
Cancelled, Triage.

## Members

| Name       | Email                  | Role       |
| ---------- | ---------------------- | ---------- |
| `<user-a>` | `<user-a>@example.com` | 20 (Admin) |
| ...        | ...                    | ...        |

## Cards

Every card is recorded with the same fields. Use this block per card:

### `<IDENTIFIER>-<seq>` - `<title>`

| Field               | Value                                              |
| ------------------- | -------------------------------------------------- |
| State               | `<backlog/todo/in progress/done/cancelled/triage>` |
| Created by          | `<user>`, `<yyyy-mm-dd hh:mm>`                     |
| Last updated        | `<yyyy-mm-dd hh:mm>`                               |
| Completed           | `<yyyy-mm-dd hh:mm>` or `-`                        |
| Start / target date | `<date>` or `-`                                    |
| Priority            | `<none/low/medium/high/urgent>`                    |
| Assignees           | `<user>`, `<user>` or `-`                          |
| Comments            | `<count>`                                          |

Description:

> `<verbatim description, or "(empty)" when the original is blank>`

Comments:

1. **`<user>`, `<yyyy-mm-dd hh:mm>`** - `<verbatim comment>`
2. ...

Actual card titles observed in the dump (no PII):

| Card        | State       | Comments | Title                                                                  |
| ----------- | ----------- | -------- | ---------------------------------------------------------------------- |
| `ALLTASK-1` | Backlog     | 0        | penawaran aplikasi SOP                                                 |
| `ALLTASK-2` | Done        | 4        | perhitungan gaji pegawai mangkir                                       |
| `ALLTASK-3` | In Progress | 7        | timesheet harus selesai baru bisa bayaran towing                       |
| `ALLTASK-5` | Backlog     | 4        | Set Pilihan ketegori Kegiatan Time Sheet di mobile Jadi Static         |
| `ALLTASK-6` | Todo        | 0        | laporan absen untuk abk/crew kapal                                     |
| `ALLTASK-7` | In Progress | 0        | [WEB] Kategori Kegiatan Timesheet Tambah is_active                     |
| `ALLTASK-8` | Todo        | 0        | [WEB] Timesheet Filter Get Kategori Kegiatan Jadi Yang Aktif Aja       |
| `ALLTASK-9` | Todo        | 0        | [WEB] Timesheet Kategori Kegiatan Disable Create New From Select Input |
| `SOP-1`     | Backlog     | 0        | sop                                                                    |

## Patterns (inference, not observation)

- The active work clusters around timesheet -> payroll -> towing payment gating.
- `ALLTASK-5`, `ALLTASK-7`, `ALLTASK-8`, and `ALLTASK-9` are one change split by
  layer (mobile, web, filter).
- `SOP` and `ALLTASK-1` are still placeholders.
- No labels, cycles, modules, or pages, so the board is a flat task list.

## Reproduce

```sql
SELECT p.identifier, s.name AS state, i.sequence_id, i.name
FROM issues i
JOIN projects p ON p.id = i.project_id
JOIN states  s ON s.id = i.state_id
WHERE i.workspace_id = '<workspace-uuid>'
  AND i.deleted_at IS NULL
ORDER BY p.identifier, i.sequence_id;
```
