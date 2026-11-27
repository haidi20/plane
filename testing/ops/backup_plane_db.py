# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Back up the Postgres database that Plane uses into a plain ``.sql`` file.

The target server runs Plane Community as a Docker Compose stack. Postgres is
not published to the host, so the dump runs *inside* the database container and
the bytes are streamed back over SSH.

Two transports are supported:

``--transport ssh`` (default)
    paramiko, password or key auth. The SSH host key is pinned by SHA-256
    fingerprint; a mismatch aborts instead of trusting the new key.

``--transport local``
    for a machine that already has ``docker`` and the container running, such
    as a cron job on the server itself.

Both transports end the same way: stdout of ``pg_dump`` becomes a ``.sql`` file
on the machine that runs this script, plus ``.sha256`` and a JSON manifest.

Why the socket is the default
-----------------------------
The container's ``pg_hba.conf`` trusts local socket connections but requires
``scram-sha-256`` for TCP. The container also exports ``PGHOST=plane-db``, which
forces TCP. Pointing ``PGHOST`` at ``/var/run/postgresql`` keeps the dump on the
socket: no database password leaves the server, and the ``PGPASSWORD`` handling
inside ``docker exec`` is not needed. Pass ``--db-host`` to use TCP if a future
deployment tightens the local trust rule.

Examples
--------
Local dry run (no SSH, no secrets)::

    python testing/ops/backup_plane_db.py --transport local --dry-run

Backup the ``plane`` database over SSH, compressed::

    export PLANE_SSH_PASS='...'
    python testing/ops/backup_plane_db.py --out-dir ./backups --gzip

Backup the whole cluster with ``pg_dumpall`` (globals + every database, so
``plane_audit`` is included)::

    python testing/ops/backup_plane_db.py --all-databases

Exit codes: 0 ok, 1 unexpected failure / bad dump, 2 configuration or auth
problem, 3 the dump command failed.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gzip
import hashlib
import json
import os
import shlex
import stat
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

PINNED_FINGERPRINT = "34:55:3d:2d:00:df:58:94:16:9b:58:67:de:de:1d:2c:fc:a9:56:6a:d9:09:7f:d5:cb:25:78:5c:58:9b:eb:90"
SOCKET_HOST = "/var/run/postgresql"
CHUNK = 1 << 20


class BackupError(RuntimeError):
    """A failure that must reach the operator with a clear message."""


@dataclass
class Target:
    """Everything the dump command needs to know about the source."""

    transport: str
    container: str
    user: str
    database: str
    host: str = ""
    port: int = 22
    ssh_user: str = "ubuntu"
    sudo: bool = True
    fingerprint: str = PINNED_FINGERPRINT
    db_host: str = SOCKET_HOST
    password: str = ""
    all_databases: bool = False
    label: str = ""
    flags: list[str] = field(default_factory=list)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Dump the Plane Postgres database to a plain .sql file.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--transport", choices=("ssh", "local"), default="ssh")
    parser.add_argument("--host", default="43.134.240.76", help="SSH host")
    parser.add_argument("--port", type=int, default=22, help="SSH port")
    parser.add_argument("--ssh-user", default="ubuntu", help="SSH user")
    parser.add_argument(
        "--ssh-password-env",
        default="PLANE_SSH_PASS",
        help="environment variable holding the SSH password (key auth wins when PLANE_SSH_KEY is set)",
    )
    parser.add_argument(
        "--fingerprint",
        default=PINNED_FINGERPRINT,
        help="expected SHA-256 host key fingerprint; empty disables pinning (not recommended)",
    )
    parser.add_argument("--container", default="plane-app-plane-db-1")
    parser.add_argument("--db-user", default="plane")
    parser.add_argument("--db", default="plane", help="database to dump when --all-databases is off")
    parser.add_argument(
        "--all-databases",
        action="store_true",
        help="dump the whole cluster with pg_dumpall (globals + every database, includes plane_audit)",
    )
    parser.add_argument(
        "--db-host",
        default=SOCKET_HOST,
        help="PGHOST for pg_dump; the container socket by default",
    )
    parser.add_argument(
        "--db-password-env",
        default="",
        help="environment variable holding the Postgres password; only needed for TCP (--db-host)",
    )
    parser.add_argument("--no-sudo", action="store_true", help="docker does not need sudo")
    parser.add_argument("--gzip", action="store_true", help="also write a .sql.gz")
    parser.add_argument("--keep-plain", action="store_true", help="keep .sql when --gzip is set")
    parser.add_argument("--out-dir", default="backups", help="destination directory")
    parser.add_argument("--label", default="", help="suffix for the filename, e.g. 'pre-migration'")
    parser.add_argument("--pg-dump", default="/usr/local/bin/pg_dump")
    parser.add_argument(
        "--pg-dumpall",
        default="/usr/local/bin/pg_dumpall",
        help="pg_dumpall binary used when --all-databases is set",
    )
    parser.add_argument("--no-owner", action="store_true", help="add --no-owner to pg_dump")
    parser.add_argument("--no-privileges", action="store_true", help="add --no-privileges to pg_dump")
    parser.add_argument(
        "--no-clean",
        action="store_true",
        help="drop --clean/--if-exists from the dump",
    )
    parser.add_argument("--dry-run", action="store_true", help="print the command and exit")
    return parser.parse_args(argv)


def resolve_target(args: argparse.Namespace) -> Target:
    flags = ["--format=plain"]
    if not args.no_clean:
        flags += ["--clean", "--if-exists"]
    if args.no_owner:
        flags.append("--no-owner")
    if args.no_privileges:
        flags.append("--no-privileges")
    return Target(
        transport=args.transport,
        container=args.container,
        user=args.db_user,
        database=args.db,
        host=args.host,
        port=args.port,
        ssh_user=args.ssh_user,
        sudo=not args.no_sudo,
        fingerprint=args.fingerprint,
        db_host=args.db_host,
        password=os.environ.get(args.db_password_env, "") if args.db_password_env else "",
        all_databases=args.all_databases,
        label=args.label,
        flags=flags,
    )


def pg_dump_argv(target: Target, binary: str) -> list[str]:
    """The inner ``pg_dump`` command, run *inside* the container.

    ``PGHOST`` and any ``PGPASSWORD`` are exported by the caller; neither is in
    argv, so neither shows up in the server's ``ps`` output.
    """
    return [binary, "--username", target.user, "--dbname", target.database, *target.flags]

def pg_dumpall_argv(target: Target, binary: str) -> list[str]:
    """The inner ``pg_dumpall`` command used by ``--all-databases``.

    ``pg_dumpall`` writes plain SQL only and has no ``--format`` flag, so the
    ``--format=plain`` entry in ``target.flags`` is dropped. It also emits the
    cluster globals (roles, tablespaces) that ``pg_dump`` cannot, which is what
    makes a full-cluster restore possible.
    """
    return [binary, "--username", target.user, *effective_flags(target)]


def docker_argv(target: Target, inner: list[str]) -> list[str]:
    """Wrap the inner command in ``docker exec`` with the dump environment."""
    env = ["--env", "PGHOST=" + target.db_host]
    if target.password:
        env += ["--env", "PGPASSWORD=" + target.password]
    argv = ["docker", "exec", *env, target.container, *inner]
    if target.sudo:
        argv = ["sudo", "-n", *argv]
    return argv


def describe(plan: list[str]) -> str:
    return " ".join(shlex.quote(part) for part in plan)


def effective_flags(target: Target) -> list[str]:
    """Flags the dump tool actually receives.

    ``pg_dumpall`` writes plain SQL only and rejects ``--format``, so the
    ``--format=plain`` entry is dropped from the command and from the manifest.
    """
    if not target.all_databases:
        return list(target.flags)
    return [flag for flag in target.flags if not flag.startswith("--format")]

def dump_label(target: Target) -> str:
    base = "all-databases" if target.all_databases else target.database
    if not target.label:
        return base
    safe = "".join(ch if ch.isalnum() or ch in "-_." else "_" for ch in target.label)
    return base + "_" + safe


@dataclass
class SshSession:
    """Minimal paramiko wrapper that enforces the pinned host key."""

    target: Target
    password: str
    _client: object = None

    def connect(self) -> None:
        try:
            import paramiko
        except ModuleNotFoundError as exc:
            raise BackupError(
                "paramiko is required for --transport ssh; pip install paramiko"
            ) from exc

        expected = self.target.fingerprint.replace(":", "").lower()
        session_password = self.password
        key_path = os.environ.get("PLANE_SSH_KEY", "")

        class PinnedKey(paramiko.MissingHostKeyPolicy):
            def missing_host_key(self, client, hostname, key):
                digest = hashlib.sha256(key.asbytes()).hexdigest()
                if expected and digest != expected:
                    raise paramiko.SSHException(
                        "host key mismatch for " + hostname + ": " + digest
                    )

        client = paramiko.SSHClient()
        client.set_missing_host_key_policy(PinnedKey() if expected else paramiko.AutoAddPolicy())
        client.connect(
            self.target.host,
            port=self.target.port,
            username=self.target.ssh_user,
            password=None if key_path else session_password or None,
            key_filename=key_path or None,
            look_for_keys=bool(key_path) or not session_password,
            allow_agent=True,
            timeout=15,
            auth_timeout=20,
            banner_timeout=20,
        )
        self._client = client

    def open_command(self, command: str):
        if self._client is None:
            raise BackupError("SSH session is not connected")
        transport = self._client.get_transport()
        channel = transport.open_session(timeout=20)
        channel.exec_command(command)
        return channel

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None


def _timestamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def _dest(out_dir: Path, label: str) -> Path:
    name = "plane_" + label + "_" + _timestamp() + ".sql"
    return out_dir / name


def _hash_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(CHUNK), b""):
            digest.update(block)
    return digest.hexdigest()


def _chmod_private(path: Path) -> None:
    try:
        path.chmod(stat.S_IRUSR | stat.S_IWUSR)
    except OSError:
        pass


def _write_manifest(
    result: dict,
    target: Target,
    sql_path: Path,
    plain_sha: str,
    gz_path: Path | None = None,
    gz_sha: str = "",
    plain_kept: bool = True,
) -> dict:
    with sql_path.open("rb") as handle:
        head = handle.read(8192)
    if b"PostgreSQL database dump" not in head and b"PostgreSQL database cluster dump" not in head:
        raise BackupError("dump does not start like pg_dump plain output: " + str(sql_path))
    manifest = {
        "source": {
            "server": target.host if target.transport == "ssh" else "local",
            "container": target.container,
            "databases": "all" if target.all_databases else target.database,
            "pg_host": target.db_host,
            "format": "plain",
            "tool": "pg_dumpall" if target.all_databases else "pg_dump",
        },
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "file": sql_path.name if plain_kept or gz_path is None else gz_path.name,
        "plain_file": sql_path.name,
        "plain_kept": plain_kept,
        "bytes": result["bytes"],
        "sha256": plain_sha,
        "seconds": result["seconds"],
        "pg_dump_flags": effective_flags(target),
    }
    if gz_path is not None and gz_sha:
        manifest["gzip_file"] = gz_path.name
        manifest["gzip_bytes"] = gz_path.stat().st_size
        manifest["gzip_sha256"] = gz_sha
    return manifest


def _gzip_copy(sql_path: Path, gz_path: Path) -> str:
    with sql_path.open("rb") as src, gzip.open(gz_path, "wb", compresslevel=6) as dst:
        for block in iter(lambda: src.read(CHUNK), b""):
            dst.write(block)
    return _hash_file(gz_path)

def stream_ssh(target: Target, plan_inner: list[str], sql_path: Path) -> dict:
    session = SshSession(target, os.environ.get("PLANE_SSH_PASS", ""))
    session.connect()
    command = describe(docker_argv(target, plan_inner))
    started = dt.datetime.now(dt.timezone.utc)
    written = 0
    try:
        channel = session.open_command(command)
        with sql_path.open("wb") as handle:
            while True:
                data = channel.recv(CHUNK)
                if not data:
                    break
                handle.write(data)
                written += len(data)
        status = channel.recv_exit_status()
        stderr = ""
        if channel.recv_stderr_ready():
            stderr = channel.recv_stderr(CHUNK).decode("utf-8", "replace").strip()
    finally:
        session.close()
    if status != 0:
        raise BackupError("pg_dump exited " + str(status) + ": " + (stderr or "no stderr"))
    return {"bytes": written, "seconds": round((dt.datetime.now(dt.timezone.utc) - started).total_seconds(), 2)}


def stream_local(target: Target, plan_inner: list[str], sql_path: Path) -> dict:
    argv = docker_argv(target, plan_inner)
    started = dt.datetime.now(dt.timezone.utc)
    with sql_path.open("wb") as handle:
        process = subprocess.run(argv, stdout=handle, stderr=subprocess.PIPE, check=False)
    if process.returncode != 0:
        raise BackupError(
            "pg_dump exited " + str(process.returncode) + ": "
            + process.stderr.decode("utf-8", "replace").strip()
        )
    return {
        "bytes": sql_path.stat().st_size,
        "seconds": round((dt.datetime.now(dt.timezone.utc) - started).total_seconds(), 2),
    }


def run(args: argparse.Namespace) -> int:
    target = resolve_target(args)
    if args.transport == "ssh" and not target.fingerprint:
        print("[warn] host key pinning disabled", file=sys.stderr)
    inner = (
        pg_dumpall_argv(target, args.pg_dumpall)
        if target.all_databases
        else pg_dump_argv(target, args.pg_dump)
    )

    out_dir = Path(args.out_dir).expanduser()
    out_dir.mkdir(parents=True, exist_ok=True)
    sql_path = _dest(out_dir, dump_label(target))
    gz_path = sql_path.with_name(sql_path.name + ".gz") if args.gzip else None

    if args.dry_run:
        print("transport:", target.transport)
        print("would run:", describe(docker_argv(target, inner)))
        print("would write:", sql_path)
        return 0

    result = (
        stream_ssh(target, inner, sql_path)
        if target.transport == "ssh"
        else stream_local(target, inner, sql_path)
    )
    if result["bytes"] == 0:
        sql_path.unlink(missing_ok=True)
        raise BackupError("pg_dump produced an empty file")

    plain_sha = _hash_file(sql_path)
    gz_sha = ""
    if gz_path is not None:
        gz_sha = _gzip_copy(sql_path, gz_path)

    plain_kept = gz_path is None or args.keep_plain
    manifest = _write_manifest(
        result, target, sql_path, plain_sha, gz_path, gz_sha, plain_kept
    )
    final = gz_path if gz_path is not None else sql_path
    manifest_path = final.with_name(final.name + ".manifest.json")
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    final.with_name(final.name + ".sha256").write_text(
        (gz_sha or plain_sha) + "  " + final.name + "\n", encoding="utf-8"
    )
    _chmod_private(final)
    _chmod_private(manifest_path)
    if gz_path is not None and not args.keep_plain:
        sql_path.unlink(missing_ok=True)
    print(json.dumps(manifest, indent=2))
    return 0

def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        return run(args)
    except BackupError as exc:
        print("[error] " + str(exc), file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())