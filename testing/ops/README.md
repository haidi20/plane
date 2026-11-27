# Ops scripts

Python utilities that operate on the deployed Plane stack. They are not part of
the API runtime and are never imported by `apps/api` or `apps/api-rs`.

## `backup_plane_db.py`

Dumps the Postgres database that Plane uses into a **plain `.sql`** file, then
writes `.sha256` and a JSON manifest next to it.

Target verified on 2026-11-27:

| Item        | Value                                                    |
| ----------- | -------------------------------------------------------- |
| Server      | `ubuntu@43.134.240.76` (internal `10.3.1.54`)            |
| Stack       | Plane Community v1.5.0, Docker Compose (`plane-app-*`)   |
| Container   | `plane-app-plane-db-1` (`postgres:15.7-alpine`)          |
| Volume      | `plane-app_pgdata` -> `/var/lib/postgresql/data`         |
| Role / DB   | `plane` / `plane` (superuser + createdb)                 |
| Other DBs   | `plane_audit` (~23 MB), `postgres`, templates            |
| Size (dump) | `plane`: ~2.7 MB plain, ~375 KB gzip, 110 tables         |
| Ports       | none published; Postgres only on the internal Docker net |

### Usage

```bash
# dependency (only needed for --transport ssh)
pip install paramiko

export PLANE_SSH_PASS='...'          # or PLANE_SSH_KEY=/path/to/id_ed25519

python testing/ops/backup_plane_db.py --out-dir ./backups --gzip
```

Output:

```
backups/plane_plane_<UTC>.sql.gz
backups/plane_plane_<UTC>.sql.gz.sha256
backups/plane_plane_<UTC>.sql.gz.manifest.json
```

Add `--keep-plain` to keep the uncompressed `.sql` too. Add `--label <tag>` to
suffix the filename (for example `--label pre-migration`). The script refuses to
finish if the dump is empty or does not start like plain `pg_dump`/`pg_dumpall`
output.

### Whole cluster

`--all-databases` switches the inner tool from `pg_dump` to `pg_dumpall`, which
writes plain SQL only and also emits the cluster globals (roles, tablespaces).
The verified sizes on the target are 5,116,563 B plain / 718,463 B gzip,
covering `plane` plus `plane_audit` (220 tables) — use it before a broad change,
and keep the single-DB dump for routine backups.

```bash
python testing/ops/backup_plane_db.py --all-databases --gzip --label pre-upgrade
```

Verified 2026-11-27: `pg_dump` and `pg_dumpall` are both 15.7 at
`/usr/local/bin/`. `pg_dump` has no `--all-databases` flag, so the two modes are
separate tools rather than one flag passed to the same binary.

### Why the socket

`pg_hba.conf` inside the container trusts `local` connections but requires
`scram-sha-256` over TCP, and the container exports `PGHOST=plane-db`, which
forces TCP. The script overrides `PGHOST` to `/var/run/postgresql`, so no
database password is needed and none is passed on the command line. Use
`--db-host` plus `--db-password-env` only if that local trust rule changes.

### Restore

```bash
gunzip -c plane_plane_<UTC>.sql.gz > /tmp/restore.sql
sudo docker exec -i plane-app-plane-db-1 \
  psql --host /var/run/postgresql --username plane --dbname plane < /tmp/restore.sql
```

The dump uses `--clean --if-exists`, so it drops and recreates the objects it
contains. Restore into a scratch database first (`plane_restore`) and compare
row counts before touching `plane`.

For a cluster dump (`--all-databases`), restore with `psql -d postgres` against
the socket instead: `pg_dumpall` output contains `CREATE ROLE` and `\connect`
statements, so it recreates the databases itself.

```bash
sudo docker exec -i plane-app-plane-db-1 \
  psql --host /var/run/postgresql --username plane --dbname postgres < /tmp/restore.sql
```

Restore into a fresh Postgres instance (or one with the roles dropped) if you
want to prove the globals are complete; restoring over the live cluster
re-applies roles that already exist.

### Verification evidence (2026-11-27)

| Run                                    | Result                                                  |
| -------------------------------------- | ------------------------------------------------------- |
| `pg_dump`, `--gzip`                    | 375,465 B gzip / 2,681,745 B plain, 110 tables          |
| `pg_dumpall`, `--all-databases --gzip` | 718,463 B gzip / 5,116,563 B plain, 220 tables          |
| `--transport local --dry-run` (both)   | correct `docker exec` command, no SSH needed            |
| Port audit (`docker ps`, `ss -ltnp`)   | only `8080`/`4430` public; `5432`/`6379`/`9000` private |

### Working from this repo

`apps/api/.venv` does **not** ship `paramiko`, so run the script with a
throwaway venv (`uv venv ... && uv pip install paramiko`) or the system
interpreter that has it. `--transport local` needs no paramiko.

The SSH password is read from `PLANE_SSH_PASS` (or a key from `PLANE_SSH_KEY`),
never from argv, so it does not appear in process listings. The host key is
pinned by SHA-256 fingerprint; a mismatch aborts the run.

Rotate both secrets after a screen-share or chat: the Compose default database
password and the SSH password. Prefer key-only auth for `ubuntu`.
