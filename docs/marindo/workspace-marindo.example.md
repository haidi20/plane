# Workspace Marindo (Plane) - redacted example

Shareable version of `workspace-marindo.md`. The real file holds employee names,
emails, NIPs, and internal links from the restored production dump, so it is
gitignored. This file keeps every card, description, and comment but replaces
personal data with placeholders.

## Redaction legend

| Placeholder                        | Replaces                                       |
| ---------------------------------- | ---------------------------------------------- |
| `<user-a>`                         | workspace admin, card author on timesheet work |
| `<user-b>`, `<user-c>`, `<user-d>` | other workspace admins, no card activity       |
| `<user-f>`                         | developer on timesheet and web category cards  |
| `<user-e>`                         | product owner, opens the payroll and SOP cards |
| `<user-g>`                         | developer on timesheet, towing, and SOP cards  |
| `<hrd-contact>`                    | HRD staff quoted in a comment                  |
| `<employee-name>`                  | crew member named in a comment                 |
| `<nip-1>`, `<nip-2>`               | employee ID numbers quoted in a comment        |
| `<internal-app-host>`              | internal staging host for sounding/fuel pages  |
| `<drive-link>`                     | Google Drive diagram link                      |

| Item         | Value                                                                 |
| ------------ | --------------------------------------------------------------------- |
| Server       | `<ssh-host>`, container `plane-app-plane-db-1`                        |
| Dump         | `<path-to>.sql` (sha256 `<hash>`)                                     |
| Local DB     | WSL Ubuntu, PostgreSQL 16, `127.0.0.1:5432`                           |
| Workspace id | `5329d1e5-6fb8-4991-8d4f-994286e74c87`                                |
| Slug         | `marindo`                                                             |
| Captured     | 2026-11-27                                                            |
| Source       | `pg_dump` from the container, restored with `psql`, read-only queries |

## Summary

| Metric                | Count                                                  |
| --------------------- | ------------------------------------------------------ |
| Active projects       | 2 (`ALLTASK`, `SOP`)                                   |
| Soft-deleted projects | 1 (`TES`)                                              |
| Active cards          | 9 (8 in `ALLTASK`, 1 in `SOP`)                         |
| Soft-deleted cards    | 1 (`ALLTASK-4`)                                        |
| Comments              | 13 (`ALLTASK-2` = 4, `ALLTASK-3` = 5, `ALLTASK-5` = 4) |
| Labels                | 0                                                      |
| Cycles                | 0                                                      |
| Modules               | 0                                                      |
| Pages / documents     | 0                                                      |
| Workspace members     | 7 (all role `20` = Admin)                              |

## Projects

| Identifier | Name     | Status                  | Cards | Notes                       |
| ---------- | -------- | ----------------------- | ----- | --------------------------- |
| `ALLTASK`  | All Task | active                  | 8     | Main operational backlog    |
| `SOP`      | sop      | active                  | 1     | Placeholder                 |
| `TES`      | Tes      | soft-deleted 2026-11-23 | 0     | Test project, no cards left |

State workflow for both active projects: Backlog, Todo, In Progress, Done,
Cancelled, Triage.

## Members

| Name       | Email                  | Role       |
| ---------- | ---------------------- | ---------- |
| `<user-a>` | `<user-a>@example.com` | 20 (Admin) |
| `<user-b>` | `<user-b>@example.com` | 20 (Admin) |
| `<user-c>` | `<user-c>@example.com` | 20 (Admin) |
| `<user-d>` | `<user-d>@example.com` | 20 (Admin) |
| `<user-e>` | `<user-e>@example.com` | 20 (Admin) |
| `<user-f>` | `<user-f>@example.com` | 20 (Admin) |
| `<user-g>` | `<user-g>@example.com` | 20 (Admin) |

Role mapping, observed in `apps/api/plane/db/models/workspace.py:19`:
`ROLE_CHOICES = ((20, "Admin"), (15, "Member"), (5, "Guest"))`.

## Cards

### `ALLTASK-1` - penawaran aplikasi SOP

| Field               | Value                        |
| ------------------- | ---------------------------- |
| State               | Backlog                      |
| Created by          | `<user-e>`, 2026-11-23 13:48 |
| Last updated        | 2026-11-23 13:48             |
| Completed           | -                            |
| Start / target date | -                            |
| Priority            | none                         |
| Assignees           | -                            |
| Comments            | 0                            |

Description: _(empty)_

Notes: administrative card for quoting an SOP application. No technical detail
yet.

### `ALLTASK-2` - perhitungan gaji pegawai mangkir

| Field               | Value                        |
| ------------------- | ---------------------------- |
| State               | Done                         |
| Created by          | `<user-e>`, 2026-11-23 13:48 |
| Last updated        | 2026-11-26 16:45             |
| Completed           | 2026-11-26 16:45             |
| Start / target date | -                            |
| Priority            | none                         |
| Assignees           | -                            |
| Comments            | 4                            |

Description: _(empty; all context lives in the comments)_

Comments:

1. **`<user-g>`, 2026-11-25 09:14** - report from HRD (`<hrd-contact>`) on
   11 Nov 2026: a crew member was not on board and had filed no leave request,
   so salary would stop from 10 November until that person returns to the ship.
   While attendance still reads `izin pay off` / `izin pay on` / `hadir` /
   `tidak hadir`, salary keeps running. HRD worried that resigning the person
   inside the app would break the finance team. Two employee IDs are named:
   `<nip-1>` (resign because this month's salary is held) and `<nip-2>` (active
   for towing money) for `<employee-name>`.
2. **`<user-g>`, 2026-11-26 11:14** - decision: keep the old NIP, zero the
   salary, then record a note to deduct the debt that was not cut this month.
3. **`<user-g>`, 2026-11-26 11:23** - add a ship field to the attendance report.
4. **`<user-e>`, 2026-11-26 16:45** - meeting notes moved into this card as a
   marker that no new feature came out of the case; card closed.

### `ALLTASK-3` - timesheet harus selesai baru bisa bayaran towing

| Field               | Value                        |
| ------------------- | ---------------------------- |
| State               | In Progress                  |
| Created by          | `<user-g>`, 2026-11-24 09:35 |
| Last updated        | 2026-11-25 07:20             |
| Completed           | -                            |
| Start / target date | 2026-11-18 / -               |
| Priority            | low                          |
| Assignees           | `<user-f>`                   |
| Comments            | 5                            |

Description: _(empty; all context lives in the comments)_

Comments:

1. **`<user-g>`, 2026-11-24 10:38** - Mahkota 5 data and a sounding timesheet
   were created
   (`<internal-app-host>/operasional/sounding/2117`) plus a fuel calculation
   (`<internal-app-host>/operasional/fuel/2111`). Analysis: unit `2162` shows in
   the list even though it is unfinished, so an admin can remind the captain to
   complete the timesheet. When creating towing, the buyback relation does not
   appear in the list.
2. **`<user-f>`, 2026-11-24 10:57** - "Gk ada transaksi buyback".
3. **`<user-g>`, 2026-11-24 11:06** - fuel calculation status is valid and now
   shows in the towing calculation, but one timesheet is still processing and
   must be `complete` before towing is paid. After checking, the row did not
   appear because the barge differs.
4. **`<user-g>`, 2026-11-24 11:11** - "ini data time sheet" (no text attachment).
5. **`<user-g>`, 2026-11-24 11:26** - `<drive-link>` with the analysis diagram.

Notes: attachments are not stored in the DB (0 rows in `issue_attachments`), so
the Drive file survives only as a URL.

### `ALLTASK-4` - test (deleted)

| Field      | Value                          |
| ---------- | ------------------------------ |
| State      | - (card deleted)               |
| Created by | -                              |
| Deleted    | 2026-11-24 10:30 (soft delete) |
| Comments   | 0                              |

Description: _(empty)_

Notes: throwaway card titled "test". Not counted as active work, but the row is
still present with `deleted_at` set.

### `ALLTASK-5` - Set Pilihan ketegori Kegiatan Time Sheet di mobile Jadi Static

| Field               | Value                        |
| ------------------- | ---------------------------- |
| State               | Backlog                      |
| Created by          | `<user-f>`, 2026-11-26 12:26 |
| Last updated        | 2026-11-27 07:29             |
| Completed           | -                            |
| Start / target date | -                            |
| Priority            | none                         |
| Assignees           | `<user-a>`, `<user-f>`       |
| Comments            | 4                            |

Description (verbatim, one item per line):

> Tambat
> Berlayar
> Berlabuh
> Muat
> Bongkar
> Sandar
>
> Set pilihan kategori kegiatan timesheet jadi list diatas

Comments:

1. **`<user-e>`, 2026-11-26 14:37** - let the admin manage the content; give it
   an active checkbox.
2. **`<user-e>`, 2026-11-26 15:31** - make it static on web too.
3. **`<user-a>`, 2026-11-27 05:18** - the APK still needs an update because it
   saves data to the local DB; timesheet uses a _first offline_ concept, so any
   API logic change must be mirrored in the APK logic.
4. **`<user-e>`, 2026-11-27 07:29** - also plan the update steps so nothing
   breaks during rollout while the implementation is not 100% done.

### `ALLTASK-6` - laporan absen untuk abk/crew kapal

| Field               | Value                        |
| ------------------- | ---------------------------- |
| State               | Todo                         |
| Created by          | `<user-g>`, 2026-11-27 08:37 |
| Last updated        | 2026-11-27 09:38             |
| Completed           | -                            |
| Start / target date | -                            |
| Priority            | none                         |
| Assignees           | -                            |
| Comments            | 0                            |

Description (verbatim):

> hasil rapat kemarin kamis 26 nop 2026
> dari admin HRD minta dibuatkan laporan untuk absen abk
>
> kebutuhan datanya /tabelnya
> no
> NIP
> nama crew
> jabatan
> nama kapal
> Tanggal
> type (hadir, izin pay off, izin pay on, tidak hadir) sesuai absensi yg sudah ada

Notes: crew attendance report with the columns `no`, `NIP`, `nama crew`,
`jabatan`, `nama kapal`, `tanggal`, and `type` (hadir / izin pay off / izin pay
on / tidak hadir).

### `ALLTASK-7` - [WEB] Kategori Kegiatan Timesheet Tambah is_active

| Field               | Value                        |
| ------------------- | ---------------------------- |
| State               | In Progress                  |
| Created by          | `<user-f>`, 2026-11-27 09:18 |
| Last updated        | 2026-11-27 09:43             |
| Completed           | -                            |
| Start / target date | -                            |
| Priority            | none                         |
| Assignees           | `<user-f>`                   |
| Comments            | 0                            |

Description (verbatim):

> Tambah field is_active. Lalu buat fungsi untuk ubah status. Di index tambahkan
> filter by status.

Notes: directly tied to `ALLTASK-5`; the activity category now needs to be
activatable, not just a static list.

### `ALLTASK-8` - [WEB] Timesheet Filter Get Kategori Kegiatan Jadi Yang Aktif Aja

| Field               | Value                        |
| ------------------- | ---------------------------- |
| State               | Todo                         |
| Created by          | `<user-f>`, 2026-11-27 09:19 |
| Last updated        | 2026-11-27 09:23             |
| Completed           | -                            |
| Start / target date | -                            |
| Priority            | none                         |
| Assignees           | `<user-f>`                   |
| Comments            | 0                            |

Description (verbatim):

> Saat edit list option kategori kegiatan harus menambahkan option lama misalkan
> option lama sudah disabled.

Notes: when editing, old categories that were disabled must still be listed so
existing records keep their label.

### `ALLTASK-9` - [WEB] Timesheet Kategori Kegiatan Disable Create New From Select Input

| Field               | Value                        |
| ------------------- | ---------------------------- |
| State               | Todo                         |
| Created by          | `<user-f>`, 2026-11-27 09:21 |
| Last updated        | 2026-11-27 09:21             |
| Completed           | -                            |
| Start / target date | -                            |
| Priority            | none                         |
| Assignees           | `<user-f>`                   |
| Comments            | 0                            |

Description: _(empty)_

Notes: the title states the intent, remove the ability to create a category
straight from the select input so categories are only managed on the master
page.

### `SOP-1` - sop

| Field               | Value                        |
| ------------------- | ---------------------------- |
| State               | Backlog                      |
| Created by          | `<user-g>`, 2026-11-25 13:40 |
| Last updated        | 2026-11-25 13:40             |
| Completed           | -                            |
| Start / target date | -                            |
| Priority            | none                         |
| Assignees           | -                            |
| Comments            | 0                            |

Description: _(empty)_

Notes: placeholder, no content yet.

## Patterns (inference, not observation)

- Active work clusters around **timesheet -> payroll -> towing**: `ALLTASK-3`
  and `ALLTASK-2` both stop at the rule for when salary or payment may proceed.
- `ALLTASK-5`, `ALLTASK-7`, `ALLTASK-8`, and `ALLTASK-9` are one change split
  by layer (mobile, web, filter) around static activity categories plus
  `is_active`.
- `SOP` and `ALLTASK-1` are still quotes or placeholders with no technical work.
- `ALLTASK-4` and project `TES` are leftover experiments, both soft-deleted.
- No labels, cycles, modules, or pages, so the board is a flat task list rather
  than a structured project.

## Reproduce

```sql
-- cards
SELECT p.identifier, s.name AS state, i.sequence_id, i.name
FROM issues i
JOIN projects p ON p.id = i.project_id
JOIN states  s ON s.id = i.state_id
WHERE i.workspace_id = '5329d1e5-6fb8-4991-8d4f-994286e74c87'
  AND i.deleted_at IS NULL
ORDER BY p.identifier, i.sequence_id;

-- descriptions
SELECT p.identifier, i.sequence_id, i.description_stripped
FROM issues i
JOIN projects p ON p.id = i.project_id
WHERE i.workspace_id = '5329d1e5-6fb8-4991-8d4f-994286e74c87'
  AND i.deleted_at IS NULL
ORDER BY p.identifier, i.sequence_id;

-- comments
SELECT p.identifier, i.sequence_id, u.display_name, c.comment_stripped
FROM issue_comments c
JOIN issues   i ON i.id = c.issue_id
JOIN projects p ON p.id = i.project_id
LEFT JOIN users u ON u.id = c.created_by_id
WHERE c.workspace_id = '5329d1e5-6fb8-4991-8d4f-994286e74c87'
ORDER BY p.identifier, i.sequence_id, c.created_at;
```
