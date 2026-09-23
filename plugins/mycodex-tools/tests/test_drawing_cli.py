from __future__ import annotations

import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import draw
from drawing.common import DrawingError
from PIL import Image


class DrawingCliTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()

    def tearDown(self):
        self.temporary.cleanup()

    def spec(self, text):
        (self.root / "input.json").write_text(text, encoding="utf-8")
        return draw.read_spec(self.root, "input.json")

    def test_invalid_json_and_duplicate_keys(self):
        for text in ('{"kind":"chart","kind":"flowchart"}', '{"kind":"chart","x":NaN}',
                     '[1,2]', '{"kind":"unknown"}', '{"kind":[]}', '[' * 2000):
            with self.subTest(text=text[:40]), self.assertRaises(DrawingError):
                self.spec(text)

    def test_size_and_root_boundary(self):
        with self.assertRaises(DrawingError) as caught:
            self.spec(' ' * (draw.MAX_SPEC_BYTES + 1))
        self.assertEqual(caught.exception.code, "spec_too_large")
        for name in ("../input.json", ".local/input.json", "C:/Windows/input.json", "sub/CON.json"):
            with self.subTest(name=name), self.assertRaises(DrawingError):
                draw.read_spec(self.root, name)

    def test_output_existing_is_unchanged(self):
        folder = self.root / "existing"
        folder.mkdir()
        marker = folder / "keep.txt"
        marker.write_text("keep", encoding="utf-8")
        with self.assertRaises(DrawingError) as caught:
            draw.render(self.root, {"kind": "chart"}, "existing")
        self.assertEqual(caught.exception.code, "target_exists")
        self.assertEqual(marker.read_text(), "keep")

    def test_preview_is_real_bounded_image(self):
        source = self.root / "large.png"
        Image.effect_noise((3000, 2500), 70).convert("RGB").save(source)
        path, metadata = draw.preview_image(source, self.root)
        self.assertLessEqual(path.stat().st_size, draw.MAX_PREVIEW_BYTES)
        self.assertTrue(metadata["scaled"])
        with Image.open(path) as image:
            image.verify()

    def test_success_packages_only_outputs(self):
        secret = self.root / "unrelated.xlsx"
        secret.write_bytes(b"never include this unrelated workbook")
        spec = {"kind": "flowchart", "nodes": [{"id": "a", "label": "测试"}], "edges": []}

        def fake_renderer(spec, output, context):
            png = output / "diagram.png"
            Image.new("RGB", (300, 150), "white").save(png)
            return {"outputs": {"png": png}, "normalized_spec": spec, "warnings": []}

        with patch("drawing.diagrams.render_diagram", side_effect=fake_renderer):
            result = draw.render(self.root, spec, "drawings/v1")
        self.assertTrue(result["success"])
        directory = self.root / result["output_dir"]
        with zipfile.ZipFile(directory / "bundle.zip") as archive:
            names = archive.namelist()
            self.assertIn("spec.json", names)
            self.assertIn("reproduce.py", names)
            self.assertNotIn("unrelated.xlsx", names)
            self.assertFalse(any("_cache" in name for name in names))
        manifest = json.loads((directory / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["preview"]["sha256"], draw.sha256(self.root / result["preview"]["path"]))

    def test_failed_attempt_is_not_success(self):
        with patch("drawing.diagrams.render_diagram", side_effect=DrawingError("renderer_timeout", "timeout")):
            with self.assertRaises(DrawingError):
                draw.render(self.root, {"kind": "flowchart"}, "drawings/fail")
        folder = self.root / "drawings/fail"
        self.assertTrue((folder / "FAILED.json").is_file())
        self.assertFalse((folder / "manifest.json").exists())
        self.assertFalse((folder / "bundle.zip").exists())

    def test_archive_failure_never_publishes_success_manifest(self):
        def fake_renderer(spec, output, context):
            png = output / "diagram.png"
            Image.new("RGB", (300, 150), "white").save(png)
            return {"outputs": {"png": png}}

        with patch("drawing.diagrams.render_diagram", side_effect=fake_renderer), \
                patch("draw.zipfile.ZipFile", side_effect=OSError("Disk full")):
            with self.assertRaises(OSError):
                draw.render(self.root, {"kind": "flowchart"}, "drawings/archive-fail")
        folder = self.root / "drawings/archive-fail"
        self.assertTrue((folder / "FAILED.json").exists())
        self.assertFalse((folder / "manifest.json").exists())


if __name__ == "__main__":
    unittest.main()
