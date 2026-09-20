"""PDF behavior tests; uses the production publish/read boundary."""

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from PIL import Image, ImageDraw
from pypdf import PdfWriter
from reportlab.lib.utils import ImageReader
from reportlab.pdfgen import canvas

from mycodex_tools.common import Context, ToolError
from mycodex_tools.pdf import pdf_create, pdf_read, pdf_search


FONT = Path("C:/Windows/Fonts/simhei.ttf")


class PdfTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.ctx = Context(self.root, font_path=FONT if FONT.is_file() else None)

    def create(self, name="sample.pdf", text="中文报告 Hello world"):
        if not FONT.is_file():
            self.skipTest("Chinese TTF fixture is not available on this host")
        return pdf_create(self.ctx, {"output_path": name, "title": "中文验收", "pages": [{"blocks": [{"type": "heading", "text": "第一页"}, {"type": "paragraph", "text": text}, {"type": "table", "rows": [["项目", "状态"], ["中文", "通过"]]}]}, {"blocks": [{"type": "heading", "text": "第二页"}, {"type": "paragraph", "text": "第二页内容 page two"}]}]})

    def assert_code(self, code, function, *args):
        with self.assertRaises(ToolError) as caught:
            function(*args)
        self.assertEqual(caught.exception.code, code)

    def test_chinese_two_page_roundtrip_and_literal_search(self):
        created = self.create()
        self.assertTrue(created["verified"])
        self.assertEqual(created["page_count"], 2)
        result = pdf_read(self.ctx, {"path": "sample.pdf"})
        self.assertEqual(result["page_count_total"], 2)
        self.assertIn("中文报告", result["pages"][0]["text"])
        self.assertIn("第二页内容", result["pages"][1]["text"])
        self.assertEqual(result["metadata"]["/Title"], "中文验收")
        self.assertEqual(len(result["sha256"]), 64)
        searched = pdf_search(self.ctx, {"path": "sample.pdf", "query": "中文报告"})
        self.assertEqual(searched["match_count"], 1)
        self.assertEqual(searched["matches"][0]["page"], 1)
        self.assertEqual(searched["matches"][0]["text"], "中文报告")
        self.assertEqual(pdf_search(self.ctx, {"path": "sample.pdf", "query": "HELLO", "case_sensitive": False})["match_count"], 1)

    def test_casefold_search_preserves_original_unicode_offsets(self):
        document = canvas.Canvas(str(self.root / "casefold.pdf"))
        document.drawString(20, 20, "Die Straße")
        document.showPage()
        document.save()
        folded = pdf_search(self.ctx, {"path": "casefold.pdf", "query": "STRASSE", "case_sensitive": False})
        self.assertEqual(folded["matches"][0]["text"], "Straße")
        self.assertEqual(folded["matches"][0]["text_offset"], 4)

    def test_text_window_reconstructs_full_page(self):
        self.create(text="abcdef" * 100)
        full = pdf_read(self.ctx, {"path": "sample.pdf", "page_count": 1})["pages"][0]["text"]
        offset, pieces = 0, []
        while offset is not None:
            page = pdf_read(self.ctx, {"path": "sample.pdf", "page_count": 1, "text_offset": offset, "max_chars": 73})["pages"][0]
            self.assertEqual(page["chars_total"], len(full))
            pieces.append(page["text"])
            offset = page["next_offset"]
        self.assertEqual("".join(pieces), full)

    def test_textless_page_is_explicit(self):
        writer = PdfWriter()
        writer.add_blank_page(width=200, height=200)
        writer.write(self.root / "blank.pdf")
        result = pdf_read(self.ctx, {"path": "blank.pdf"})
        self.assertEqual(result["pages"][0]["text_status"], "no_extractable_text")
        self.assertEqual(pdf_search(self.ctx, {"path": "blank.pdf", "query": "hello"})["textless_pages"], [1])

    def test_image_only_scan_has_no_extractable_text(self):
        bitmap = Image.new("RGB", (200, 100), "white")
        ImageDraw.Draw(bitmap).text((10, 10), "Scanned page", fill="black")
        document = canvas.Canvas(str(self.root / "scan.pdf"))
        document.drawImage(ImageReader(bitmap), 20, 20)
        document.showPage()
        document.save()
        page = pdf_read(self.ctx, {"path": "scan.pdf"})["pages"][0]
        self.assertEqual(page["text_status"], "no_extractable_text")
        self.assertEqual(page["text"], "")

    def test_missing_font_and_invalid_pdf(self):
        ctx = Context(self.root, font_path=self.root / "missing.ttf")
        self.assert_code("missing_font", pdf_create, ctx, {"output_path": "a.pdf", "pages": [{"blocks": []}]})
        (self.root / "bad.pdf").write_bytes(b"not a PDF")
        self.assert_code("invalid_pdf", pdf_read, self.ctx, {"path": "bad.pdf"})

    def test_invalid_ranges_and_match_overflow(self):
        self.create(text="same same same")
        for args in ({"page_start": 0}, {"page_start": 3}, {"max_chars": 0}, {"page_count": 21}):
            self.assert_code("invalid_argument", pdf_read, self.ctx, {"path": "sample.pdf", **args})
        self.assert_code("invalid_range", pdf_read, self.ctx, {"path": "sample.pdf", "page_start": 2, "page_count": 2})
        self.assert_code("invalid_range", pdf_read, self.ctx, {"path": "sample.pdf", "page_count": 1, "text_offset": 100000})
        self.assert_code("result_too_large", pdf_search, self.ctx, {"path": "sample.pdf", "query": "same", "max_matches": 2})
        self.assert_code("invalid_argument", pdf_search, self.ctx, {"path": "sample.pdf", "query": ""})

    def test_bookmark_pagination(self):
        writer = PdfWriter()
        writer.add_blank_page(width=200, height=200)
        for i in range(3):
            writer.add_outline_item(f"Bookmark {i}", 0)
        writer.write(self.root / "bookmarks.pdf")
        result = pdf_read(self.ctx, {"path": "bookmarks.pdf", "bookmark_count": 2})
        self.assertEqual(result["bookmark_count_total"], 3)
        self.assertEqual(result["next_bookmark_start"], 2)
        self.assertEqual(result["bookmarks"][0]["page"], 1)
        final = pdf_read(self.ctx, {"path": "bookmarks.pdf", "bookmark_start": 2})
        self.assertEqual(final["bookmarks"][0]["title"], "Bookmark 2")
        self.assertIsNone(final["next_bookmark_start"])

    def test_encrypted_pdf_is_rejected(self):
        writer = PdfWriter()
        writer.add_blank_page(width=200, height=200)
        writer.encrypt("password")
        writer.write(self.root / "encrypted.pdf")
        self.assert_code("unsupported_pdf", pdf_read, self.ctx, {"path": "encrypted.pdf"})

    def test_layout_overflow_does_not_publish(self):
        if not FONT.is_file():
            self.skipTest("Chinese TTF fixture unavailable")
        self.assert_code("layout_overflow", pdf_create, self.ctx, {"output_path": "overflow.pdf", "pages": [{"blocks": [{"type": "paragraph", "text": "Long content. " * 5000}]}]})
        self.assertFalse((self.root / "overflow.pdf").exists())
        self.assertEqual(list(self.root.iterdir()), [])

    def test_invalid_table_and_existing_target(self):
        self.create()
        original = (self.root / "sample.pdf").read_bytes()
        self.assert_code("target_exists", pdf_create, self.ctx, {"output_path": "sample.pdf", "pages": [{"blocks": []}]})
        self.assertEqual((self.root / "sample.pdf").read_bytes(), original)
        self.assert_code("invalid_argument", pdf_create, self.ctx, {"output_path": "uneven.pdf", "pages": [{"blocks": [{"type": "table", "rows": [["a", "b"], ["c"]]}]}]})
        self.assertFalse((self.root / "uneven.pdf").exists())

    def test_missing_glyph_is_rejected_instead_of_silent_corruption(self):
        if not FONT.is_file():
            self.skipTest("Chinese TTF fixture unavailable")
        self.assert_code("unsupported_font_glyph", pdf_create, self.ctx, {"output_path": "glyph.pdf", "pages": [{"blocks": [{"type": "paragraph", "text": "\U0001f600"}]}]})
        self.assertFalse((self.root / "glyph.pdf").exists())

    def test_verification_mismatch_does_not_publish(self):
        if not FONT.is_file():
            self.skipTest("Chinese TTF fixture unavailable")
        with patch("mycodex_tools.pdf._page_text", return_value="incorrect"):
            self.assert_code("verification_failed", self.create)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_level_only_applies_to_headings(self):
        if not FONT.is_file():
            self.skipTest("Chinese TTF fixture unavailable")
        for block in ({"type": "paragraph", "text": "text", "level": 1}, {"type": "table", "rows": [["text"]], "level": 1}):
            self.assert_code("invalid_argument", pdf_create, self.ctx, {"output_path": "bad-level.pdf", "pages": [{"blocks": [block]}]})
        self.assertEqual(list(self.root.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
