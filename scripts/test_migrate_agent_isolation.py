import contextlib
import io
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import migrate_agent_isolation as migration


SENTINEL = "SYNTHETIC-PRIVATE-PAYLOAD-NEVER-PRINT"
OWNER = "SYNTHETIC-OWNER-NEVER-PRINT"


class MigrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.database = self.root / "state.sqlite3"
        self.backup = self.root / "recovery"
        with contextlib.closing(sqlite3.connect(self.database)) as connection:
            connection.execute("CREATE TABLE wc_goals(owner_key TEXT,project TEXT,snapshot_json TEXT)")
            connection.execute("INSERT INTO wc_goals VALUES(?,?,?)", (OWNER, "project-one", SENTINEL))
            connection.execute("CREATE TABLE project_memories(memory_scope_id TEXT,content TEXT)")
            connection.execute("INSERT INTO project_memories VALUES(?,?)", ("scope-one", SENTINEL))
            connection.execute("CREATE TABLE unrelated_secret(content TEXT)")
            connection.execute("INSERT INTO unrelated_secret VALUES(?)", (SENTINEL,))
            connection.commit()

    def run_cli(self, arguments):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            status = migration.main(arguments)
        text = output.getvalue()
        self.assertNotIn(SENTINEL, text)
        self.assertNotIn(OWNER, text)
        return status, json.loads(text)

    def apply(self):
        return migration.apply(self.database, self.backup, service_stopped=True)

    def test_default_plan_reads_only_whitelisted_metadata_and_writes_nothing(self):
        before = migration._sha256(self.database)
        files = sorted(path.name for path in self.root.iterdir())
        status, result = self.run_cli(["--database", str(self.database), "--backup-dir", str(self.backup)])
        self.assertEqual(status, 0)
        self.assertEqual(result["mode"], "plan")
        self.assertEqual({row["table"] for row in result["tables"]}, {"wc_goals", "project_memories"})
        self.assertTrue(all(row["assignment"] == "unassigned" for row in result["tables"]))
        self.assertEqual(result["agent_assignments"], 0)
        self.assertEqual(result["meeting_publications"], 0)
        self.assertEqual(migration._sha256(self.database), before)
        self.assertEqual(sorted(path.name for path in self.root.iterdir()), files)

    def test_apply_preserves_private_rows_and_does_not_publish_or_assign(self):
        status, result = self.run_cli(["--apply", "--service-stopped", "--database", str(self.database), "--backup-dir", str(self.backup)])
        self.assertEqual(status, 0)
        self.assertEqual(result["mode"], "applied")
        with contextlib.closing(sqlite3.connect(self.database)) as connection:
            self.assertEqual(connection.execute("SELECT owner_key,snapshot_json FROM wc_goals").fetchone(), (OWNER, SENTINEL))
            self.assertEqual(connection.execute("SELECT content FROM project_memories").fetchone()[0], SENTINEL)
            self.assertEqual(connection.execute("SELECT content FROM unrelated_secret").fetchone()[0], SENTINEL)
            rows = connection.execute("SELECT table_name,row_count,reason,assignment FROM " + migration.INVENTORY).fetchall()
            self.assertEqual(len(rows), 2)
            self.assertTrue(all(row[1:] == (1, migration.REASON, "unassigned") for row in rows))
            self.assertEqual(connection.execute("SELECT name FROM sqlite_master WHERE name LIKE '%meeting%'").fetchall(), [])
            self.assertEqual(connection.execute("SELECT name FROM sqlite_master WHERE name='wc_isolated_agents'").fetchall(), [])

    def test_backup_recovers_original_sentinel_without_script_output(self):
        result = self.apply()
        status, verified = self.run_cli(["--verify-backup", str(self.backup)])
        self.assertEqual(status, 0)
        self.assertTrue(verified["verified"])
        self.assertFalse(verified["automatic_restore"])
        self.assertEqual(result["manifest_sha256"], verified["manifest_sha256"])
        manifest = json.loads((self.backup / migration.MANIFEST_NAME).read_text())
        self.assertNotIn(SENTINEL, json.dumps(manifest))
        self.assertNotIn(OWNER, json.dumps(manifest))
        with contextlib.closing(sqlite3.connect(self.backup / migration.BACKUP_NAME)) as connection:
            self.assertEqual(connection.execute("SELECT snapshot_json FROM wc_goals").fetchone()[0], SENTINEL)
            self.assertIsNone(connection.execute("SELECT name FROM sqlite_master WHERE name=?", (migration.INVENTORY,)).fetchone())

    def test_repeat_apply_is_idempotent_and_never_overwrites_backup(self):
        first = self.apply()
        backup_hash = migration._sha256(self.backup / migration.BACKUP_NAME)
        manifest_hash = migration._sha256(self.backup / migration.MANIFEST_NAME)
        second = self.apply()
        self.assertEqual(second["mode"], "already_applied")
        self.assertEqual(second["manifest_sha256"], first["manifest_sha256"])
        self.assertEqual(backup_hash, migration._sha256(self.backup / migration.BACKUP_NAME))
        self.assertEqual(manifest_hash, migration._sha256(self.backup / migration.MANIFEST_NAME))
        with contextlib.closing(sqlite3.connect(self.database)) as connection:
            self.assertEqual(connection.execute("SELECT COUNT(*) FROM " + migration.INVENTORY).fetchone()[0], 2)

    def test_apply_requires_stopped_ack_and_fresh_backup_directory(self):
        before = migration._sha256(self.database)
        with self.assertRaisesRegex(migration.MigrationError, "stopped_service"):
            migration.apply(self.database, self.backup)
        self.backup.mkdir()
        sentinel = self.backup / "keep.txt"
        sentinel.write_text(SENTINEL)
        with self.assertRaisesRegex(migration.MigrationError, "already_exists"):
            self.apply()
        self.assertEqual(sentinel.read_text(), SENTINEL)
        self.assertEqual(migration._sha256(self.database), before)

    def test_backup_or_manifest_tamper_is_rejected_on_verify_and_repeat(self):
        self.apply()
        with (self.backup / migration.BACKUP_NAME).open("ab") as stream:
            stream.write(b"tamper")
        with self.assertRaisesRegex(migration.MigrationError, "backup_hash_mismatch"):
            migration.verify_backup(self.backup)
        with self.assertRaisesRegex(migration.MigrationError, "backup_hash_mismatch"):
            self.apply()

    def test_manifest_hash_is_pinned_in_database(self):
        self.apply()
        path = self.backup / migration.MANIFEST_NAME
        path.write_text(path.read_text() + " ")
        with self.assertRaisesRegex(migration.MigrationError, "manifest_hash_mismatch"):
            self.apply()

    def test_wal_plan_fails_closed_and_apply_backs_up_committed_wal(self):
        connection = sqlite3.connect(self.database)
        self.addCleanup(connection.close)
        connection.execute("PRAGMA journal_mode=WAL")
        connection.execute("UPDATE wc_goals SET project='wal-project'")
        connection.commit()
        with self.assertRaisesRegex(migration.MigrationError, "checkpoint_required"):
            migration.plan(self.database)
        self.apply()
        with contextlib.closing(sqlite3.connect(self.backup / migration.BACKUP_NAME)) as backup:
            self.assertEqual(backup.execute("SELECT project,snapshot_json FROM wc_goals").fetchone(), ("wal-project", SENTINEL))

    def test_concurrent_writer_prevents_migration_before_backup(self):
        with contextlib.closing(sqlite3.connect(self.database)) as writer:
            writer.execute("BEGIN IMMEDIATE")
            with self.assertRaises(sqlite3.OperationalError):
                self.apply()
            self.assertFalse(self.backup.exists())

    def test_backup_failure_leaves_old_rows_and_preserves_recovery_directory(self):
        with patch.object(migration, "verify_backup", side_effect=migration.MigrationError("injected_verification_failure")):
            with self.assertRaisesRegex(migration.MigrationError, "injected"):
                self.apply()
        self.assertTrue(self.backup.is_dir())
        with contextlib.closing(sqlite3.connect(self.database)) as connection:
            self.assertEqual(connection.execute("SELECT snapshot_json FROM wc_goals").fetchone()[0], SENTINEL)
            self.assertIsNone(connection.execute("SELECT name FROM sqlite_master WHERE name=?", (migration.INVENTORY,)).fetchone())

    def test_hardlinked_database_and_backup_are_rejected(self):
        link = self.root / "database-alias"
        os.link(self.database, link)
        with self.assertRaisesRegex(migration.MigrationError, "plain_file"):
            migration.plan(self.database)
        link.unlink()
        self.apply()
        os.link(self.backup / migration.BACKUP_NAME, self.root / "backup-alias")
        with self.assertRaisesRegex(migration.MigrationError, "plain_file"):
            migration.verify_backup(self.backup)

    def directory_link(self, link, target):
        if os.name == "nt":
            result = subprocess.run(["cmd.exe", "/d", "/c", "mklink", "/J", str(link), str(target)],
                                    capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, "synthetic junction creation failed")
            self.addCleanup(lambda: link.rmdir() if os.path.lexists(link) else None)
        else:
            link.symlink_to(target, target_is_directory=True)

    def test_linked_database_parent_and_backup_parent_are_rejected(self):
        link = self.root / "alias"
        self.directory_link(link, self.root)
        with self.assertRaisesRegex(migration.MigrationError, "linked_path"):
            migration.plan(link / self.database.name)
        with self.assertRaisesRegex(migration.MigrationError, "linked_path"):
            migration.apply(self.database, link / "backup", service_stopped=True)
        self.assertFalse((self.root / "backup").exists())

    def test_errors_never_print_sqlite_private_diagnostics(self):
        with patch.object(migration, "plan", side_effect=sqlite3.DatabaseError(SENTINEL)):
            status, output = self.run_cli(["--database", str(self.database)])
        self.assertEqual(status, 1)
        self.assertEqual(output, {"error": "quarantine_operation_failed"})


if __name__ == "__main__":
    unittest.main()
