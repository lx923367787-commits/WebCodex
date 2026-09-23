import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from mycodex_tools.common import Context, ToolError, bounded_json, obj, string, validate_args


class FilePolicyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.ctx = Context(self.root)

    def test_paths_reject_escape_devices_and_canonical_secrets(self):
        for path in ("../x.pdf", "a/../x.pdf", "C:/x.pdf", "C:x.pdf", "//server/share/x.pdf",
                     "x.pdf:stream", "NUL.pdf", "AUX/x.pdf", ".git/x.pdf", "secrets/x.pdf",
                     "Tokens/x.pdf", "project-registry/x.pdf", "Runner.TOML.bak/x.pdf",
                     ".ENV/x.pdf", "credential.key/x.pdf", "folder./x.pdf"):
            with self.subTest(path=path), self.assertRaises(ToolError):
                self.ctx.resolve_path(path, ".pdf")
        self.assertEqual(self.ctx.resolve_path("中文.pdf", ".pdf"), self.root / "中文.pdf")

    def test_reparse_or_symlink_directory_rejected(self):
        outside = self.root / "real"
        outside.mkdir()
        link = self.root / "link"
        try:
            link.symlink_to(outside, target_is_directory=True)
        except OSError:
            self.skipTest("Host does not allow creating symlink fixtures")
        self.addCleanup(lambda: link.unlink(missing_ok=True))
        with self.assertRaises(ToolError):
            self.ctx.resolve_path("link/x.pdf", ".pdf")

    def test_input_snapshot_digest_and_size(self):
        (self.root / "x.pdf").write_bytes(b"%PDF-test")
        data, digest = self.ctx.read_bytes("x.pdf", ".pdf")
        self.assertEqual(data, b"%PDF-test")
        self.assertEqual(len(digest), 64)
        with self.assertRaisesRegex(ToolError, "Source changed"):
            self.ctx.read_bytes("x.pdf", ".pdf", "0" * 64)
        with mock.patch("mycodex_tools.common.MAX_FILE_BYTES", 3):
            with self.assertRaisesRegex(ToolError, "64 MiB"):
                self.ctx.read_bytes("x.pdf", ".pdf")

    def test_invalid_and_expanding_office_archives_rejected(self):
        (self.root / "broken.docx").write_bytes(b"not zip")
        with self.assertRaises(ToolError):
            self.ctx.read_bytes("broken.docx", ".docx")
        with zipfile.ZipFile(self.root / "large.docx", "w", zipfile.ZIP_DEFLATED) as archive:
            archive.writestr("[Content_Types].xml", "x" * 100)
        with mock.patch("mycodex_tools.common.MAX_ZIP_BYTES", 99):
            with self.assertRaisesRegex(ToolError, "expansion"):
                self.ctx.read_bytes("large.docx", ".docx")

    def test_publish_verifies_before_claiming_target(self):
        def verify(path):
            self.assertFalse((self.root / "out.pdf").exists())
            self.assertEqual(path.read_bytes(), b"verified")
        result = self.ctx.publish("out.pdf", ".pdf", lambda p: p.write_bytes(b"verified"), verify)
        self.assertTrue(result["verified"])
        self.assertEqual((self.root / "out.pdf").read_bytes(), b"verified")
        self.assertEqual(list(self.root.glob(".mycodex-*")), [])
        with self.assertRaisesRegex(ToolError, "already exists"):
            self.ctx.publish("out.pdf", ".pdf", lambda p: p.write_bytes(b"bad"), verify)

    def test_failed_verification_and_competing_writer_do_not_overwrite(self):
        def fail(_):
            raise ToolError("verification_failed", "verification failed")
        with self.assertRaises(ToolError):
            self.ctx.publish("out.pdf", ".pdf", lambda p: p.write_bytes(b"bad"), fail)
        self.assertFalse((self.root / "out.pdf").exists())
        def competitor(_):
            (self.root / "out.pdf").write_bytes(b"other writer")
        with self.assertRaisesRegex(ToolError, "concurrently"):
            self.ctx.publish("out.pdf", ".pdf", lambda p: p.write_bytes(b"ours"), competitor)
        self.assertEqual((self.root / "out.pdf").read_bytes(), b"other writer")
        self.assertEqual(list(self.root.glob(".mycodex-*")), [])

    def test_json_bounds_and_unknown_arguments(self):
        with self.assertRaises(ToolError):
            bounded_json({"text": "中" * 30000}, 128 * 1024)
        with self.assertRaises(ToolError):
            bounded_json({"items": [0] * 4096}, 128 * 1024)
        with self.assertRaises(ToolError):
            validate_args({"path": "x", "command": "unwanted"}, obj({"path": string()}, ["path"]))


if __name__ == "__main__":
    unittest.main()
