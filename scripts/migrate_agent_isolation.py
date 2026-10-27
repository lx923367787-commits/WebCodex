"""Prepare legacy data quarantine without assigning, moving or publishing it.

Default: strictly read-only metadata plan. Apply requires a stopped-service
acknowledgement and makes a protected SQLite backup before adding inventory.
Quarantine is a management inventory, not an access-control replacement: deploy
the credential-bound broker before exposing any model tools. No restore action
is implemented; verify_backup only validates a protected recovery artifact.
"""
from __future__ import annotations

import argparse
import contextlib
import ctypes
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import sys
import time


MIGRATION = "agent-private-quarantine-v1"
INVENTORY = "wc_agent_legacy_quarantine"
MIGRATIONS = "wc_agent_isolation_migrations"
TABLES = (
    "wc_goals", "wc_host_goals", "wc_host_goal_plans", "wc_host_goal_attempts",
    "wc_host_goal_reviews", "wc_host_goal_repairs", "wc_host_goal_checkpoints",
    "wc_host_goal_operations", "wc_host_goal_replays", "wc_host_goal_check_requests",
    "wc_host_goal_checkpoint_requests", "wc_host_goal_current", "wc_host_goal_handoffs",
    "wc_host_goal_windows", "wc_host_context_meters", "wc_host_context_samples",
    "wc_host_goal_context_refs", "wc_host_goal_context_reads", "wc_host_context_states",
    "project_memories", "project_memory_scopes",
)
OWNERSHIP_COLUMNS = frozenset({"owner_key", "owner_user", "project", "project_runtime_id",
                               "memory_scope_id", "agent_id", "goal_id"})
REASON = "legacy_owner_is_not_verified_agent_identity"
BACKUP_NAME = "database.sqlite3"
MANIFEST_NAME = "manifest.json"


class MigrationError(Exception):
    """Only fixed, non-content error codes may be printed by the CLI."""


def _fail(code):
    raise MigrationError(code)


def _safe_path(path, *, file=False, fresh=False):
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts:
        _fail("absolute_nontraversing_path_required")
    if os.name == "nt" and (str(path).startswith("\\\\") or ":" in str(path)[2:]):
        _fail("device_unc_or_stream_path_rejected")
    parts = list(reversed(path.parents)) + [path]
    for index, part in enumerate(parts):
        try:
            metadata = part.lstat()
        except FileNotFoundError:
            if fresh and index == len(parts) - 1:
                return path
            _fail("path_missing")
        if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400:
            _fail("linked_path_rejected")
        if index != len(parts) - 1 and not stat.S_ISDIR(metadata.st_mode):
            _fail("parent_not_directory")
        if index == len(parts) - 1:
            if fresh:
                _fail("backup_directory_already_exists")
            if file and (not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1):
                _fail("database_or_backup_not_plain_file")
    return path


def _database(path):
    path = _safe_path(path, file=True)
    for suffix in ("-wal", "-shm", "-journal"):
        sidecar = Path(str(path) + suffix)
        if os.path.lexists(sidecar):
            _safe_path(sidecar, file=True)
    return path


def _connect(path, mode="ro", immutable=False):
    uri = path.as_uri() + "?mode=" + mode + ("&immutable=1" if immutable else "")
    connection = sqlite3.connect(uri, uri=True, timeout=0, isolation_level=None)
    connection.execute("PRAGMA trusted_schema=OFF")
    if mode == "ro":
        connection.execute("PRAGMA query_only=ON")
    return connection


def _table_exists(connection, name):
    row = connection.execute("SELECT type FROM sqlite_master WHERE name=?", (name,)).fetchone()
    if row and row[0] != "table":
        _fail("unexpected_schema_object")
    return row is not None


def _inventory(connection):
    result = []
    for name in TABLES:
        if _table_exists(connection, name):
            # Identifiers are constants. Never read owner values or payloads.
            count = connection.execute('SELECT COUNT(*) FROM "' + name + '"').fetchone()[0]
            columns = sorted(row[0] for row in connection.execute(
                "SELECT name FROM pragma_table_info(?)", (name,)) if row[0] in OWNERSHIP_COLUMNS)
            result.append({"table": name, "row_count": count, "ownership_columns": columns,
                           "assignment": "unassigned", "reason": REASON})
    return result


def _existing(connection):
    inventory_exists = _table_exists(connection, INVENTORY)
    migrations_exists = _table_exists(connection, MIGRATIONS)
    if inventory_exists != migrations_exists:
        _fail("incomplete_quarantine_schema")
    if not migrations_exists:
        return None
    row = connection.execute(
        "SELECT backup_directory,manifest_sha256 FROM " + MIGRATIONS + " WHERE migration_id=?",
        (MIGRATION,)).fetchone()
    if row is None:
        _fail("quarantine_migration_marker_missing")
    return {"backup_directory": row[0], "manifest_sha256": row[1]}


def plan(database):
    database = _database(database)
    # immutable avoids even SQLite shared-memory side effects. WAL contents may
    # not be ignored, so a read-only plan requires checkpointed main-db state.
    for suffix in ("-wal", "-journal"):
        sidecar = Path(str(database) + suffix)
        if sidecar.exists() and sidecar.stat().st_size:
            _fail("checkpoint_required_for_readonly_plan")
    with contextlib.closing(_connect(database, immutable=True)) as connection:
        return {"mode": "plan", "migration": MIGRATION, "database": str(database),
                "tables": _inventory(connection), "already_applied": _existing(connection) is not None,
                "payloads_changed": False, "agent_assignments": 0, "meeting_publications": 0}


def _private_directory(path):
    os.mkdir(path, 0o700)
    if os.name != "nt":
        os.chmod(path, 0o700)
        return
    # Restrict only the newly-created backup directory. Its children inherit
    # current-user and SYSTEM access; existing paths/host accounts are untouched.
    from ctypes import wintypes
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    advapi = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    kernel.LocalFree.argtypes = [wintypes.HLOCAL]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    advapi.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    advapi.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p,
                                          wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    advapi.ConvertSidToStringSidW.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.LPWSTR)]
    advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW.argtypes = [wintypes.LPCWSTR,
        wintypes.DWORD, ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p]
    advapi.SetFileSecurityW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, ctypes.c_void_p]
    token = wintypes.HANDLE()
    sid_text = wintypes.LPWSTR()
    descriptor = ctypes.c_void_p()
    try:
        if not advapi.OpenProcessToken(kernel.GetCurrentProcess(), 0x8, ctypes.byref(token)):
            _fail("backup_permissions_unavailable")
        size = wintypes.DWORD()
        advapi.GetTokenInformation(token, 1, None, 0, ctypes.byref(size))
        if not size.value or size.value > 65536:
            _fail("backup_permissions_unavailable")
        buffer = ctypes.create_string_buffer(size.value)
        if not advapi.GetTokenInformation(token, 1, buffer, size, ctypes.byref(size)):
            _fail("backup_permissions_unavailable")
        sid = ctypes.cast(buffer, ctypes.POINTER(ctypes.c_void_p))[0]
        if not advapi.ConvertSidToStringSidW(sid, ctypes.byref(sid_text)):
            _fail("backup_permissions_unavailable")
        sddl = "D:P(A;OICI;FA;;;" + sid_text.value + ")(A;OICI;FA;;;SY)"
        if not advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, 1, ctypes.byref(descriptor), None):
            _fail("backup_permissions_unavailable")
        if not advapi.SetFileSecurityW(str(path), 0x80000004, descriptor):
            _fail("backup_permissions_unavailable")
    finally:
        if descriptor.value:
            kernel.LocalFree(descriptor)
        if sid_text:
            kernel.LocalFree(sid_text)
        if token:
            kernel.CloseHandle(token)


def _sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _write_new(path, data):
    with path.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def verify_backup(backup_directory, expected_manifest_sha256=None):
    directory = _safe_path(backup_directory)
    manifest_path = _safe_path(directory / MANIFEST_NAME, file=True)
    if manifest_path.stat().st_size > 65536:
        _fail("backup_manifest_invalid")
    manifest_sha256 = _sha256(manifest_path)
    if expected_manifest_sha256 is not None and manifest_sha256 != expected_manifest_sha256:
        _fail("backup_manifest_hash_mismatch")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict) or manifest.get("migration") != MIGRATION or manifest.get("backup_file") != BACKUP_NAME:
        _fail("backup_manifest_invalid")
    backup = _safe_path(directory / BACKUP_NAME, file=True)
    if backup.stat().st_size != manifest.get("backup_bytes") or _sha256(backup) != manifest.get("backup_sha256"):
        _fail("backup_hash_mismatch")
    with contextlib.closing(_connect(backup, immutable=True)) as connection:
        if connection.execute("PRAGMA quick_check").fetchall() != [("ok",)]:
            _fail("backup_integrity_failed")
    return {"verified": True, "backup_directory": str(directory), "manifest_sha256": manifest_sha256,
            "backup_sha256": manifest["backup_sha256"], "backup_bytes": manifest["backup_bytes"],
            "automatic_restore": False}


def apply(database, backup_directory, *, service_stopped=False):
    if not service_stopped:
        _fail("stopped_service_acknowledgement_required")
    database = _database(database)
    with contextlib.closing(_connect(database, "rw")) as connection:
        # A RESERVED/IMMEDIATE transaction excludes every other SQLite writer,
        # while allowing the separate backup reader in rollback and WAL modes.
        connection.execute("BEGIN IMMEDIATE")
        try:
            existing = _existing(connection)
            if existing is not None:
                verified = verify_backup(existing["backup_directory"], existing["manifest_sha256"])
                connection.rollback()
                return dict(verified, mode="already_applied", payloads_changed=False,
                            agent_assignments=0, meeting_publications=0)
            rows = _inventory(connection)
            directory = _safe_path(backup_directory, fresh=True)
            _private_directory(directory)
            _safe_path(directory)
            backup = directory / BACKUP_NAME
            # Reserve the filename without overwrite before SQLite opens it.
            _write_new(backup, b"")
            with contextlib.closing(_connect(database)) as source, contextlib.closing(sqlite3.connect(backup)) as destination:
                deadline = time.monotonic() + 60
                def progress(_status, _remaining, _total):
                    if time.monotonic() > deadline:
                        _fail("backup_timeout")
                source.backup(destination, pages=256, progress=progress, sleep=0.05)
            with backup.open("r+b") as stream:
                os.fsync(stream.fileno())
            manifest = {"migration": MIGRATION, "backup_file": BACKUP_NAME,
                        "source_database": str(database), "backup_bytes": backup.stat().st_size,
                        "backup_sha256": _sha256(backup), "tables": rows,
                        "agent_assignments": 0, "meeting_publications": 0,
                        "recovery_requires_stopped_service_and_exclusive_access": True}
            encoded = (json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n").encode()
            _write_new(directory / MANIFEST_NAME, encoded)
            verified = verify_backup(directory)
            connection.execute("CREATE TABLE " + INVENTORY + " (table_name TEXT PRIMARY KEY,row_count INTEGER NOT NULL,ownership_columns_json TEXT NOT NULL,reason TEXT NOT NULL,assignment TEXT NOT NULL CHECK(assignment='unassigned'))")
            connection.execute("CREATE TABLE " + MIGRATIONS + " (migration_id TEXT PRIMARY KEY,backup_directory TEXT NOT NULL,manifest_sha256 TEXT NOT NULL)")
            connection.executemany("INSERT INTO " + INVENTORY + " VALUES(?,?,?,?,?)", [
                (row["table"], row["row_count"], json.dumps(row["ownership_columns"]), REASON, "unassigned")
                for row in rows])
            connection.execute("INSERT INTO " + MIGRATIONS + " VALUES(?,?,?)",
                               (MIGRATION, str(directory), verified["manifest_sha256"]))
            connection.commit()
            return dict(verified, mode="applied", tables=rows, payloads_changed=False,
                        agent_assignments=0, meeting_publications=0)
        except BaseException:
            connection.rollback()
            # Preserve completed or incomplete recovery artifacts; never erase
            # them or reuse their directory automatically after an unknown error.
            raise


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", type=Path)
    parser.add_argument("--backup-dir", type=Path)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--service-stopped", action="store_true")
    parser.add_argument("--verify-backup", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.verify_backup is not None:
            if args.apply or args.database is not None or args.backup_dir is not None:
                _fail("ambiguous_operation")
            result = verify_backup(args.verify_backup)
        else:
            if args.database is None:
                _fail("database_required")
            if args.apply:
                if args.backup_dir is None:
                    _fail("backup_directory_required")
                result = apply(args.database, args.backup_dir, service_stopped=args.service_stopped)
            else:
                result = plan(args.database)
        print(json.dumps(result, sort_keys=True))
        return 0
    except MigrationError as error:
        print(json.dumps({"error": str(error)}))
    except Exception:
        print(json.dumps({"error": "quarantine_operation_failed"}))
    return 1


if __name__ == "__main__":
    sys.exit(main())
