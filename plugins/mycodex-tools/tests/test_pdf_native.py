"""Readback/rendering regressions for the local PDF/OCR backend."""
import hashlib
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from PIL import Image, ImageChops, ImageDraw, ImageFont
from pypdf import PdfReader, PdfWriter
from reportlab.lib.utils import ImageReader
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas

from mycodex_tools.common import ToolError
from mycodex_tools import pdf_native


FONT = Path(os.environ.get("MYCODEX_TEST_FONT", "C:/Windows/Fonts/simhei.ttf"))
TESSERACT = os.environ.get("MYCODEX_TEST_TESSERACT")
TESSDATA = os.environ.get("MYCODEX_TEST_TESSDATA")


class PdfNativeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def make_pdf(self, name="source.pdf", texts=("Hello world", "Second page"), chinese=False):
        source = self.root / name
        document = canvas.Canvas(str(source), pagesize=(400, 300))
        if chinese:
            if not FONT.is_file():
                self.skipTest("Chinese TrueType fixture unavailable")
            pdfmetrics.registerFont(TTFont("NativeTestChinese", str(FONT)))
        for text in texts:
            document.setFont("NativeTestChinese" if chinese else "Helvetica", 18)
            document.drawString(30, 230, text)
            document.drawString(30, 140, "其他内容不变" if chinese else "Unchanged content")
            document.showPage()
        document.save()
        return source

    def assert_code(self, code, function, *args):
        with self.assertRaises(ToolError) as caught:
            function(*args)
        self.assertEqual(caught.exception.code, code, str(caught.exception))

    def options(self):
        return {"font_path": str(FONT), "tesseract_path": TESSERACT, "tessdata_path": TESSDATA}

    def test_native_read_has_coordinates_hash_and_continuation(self):
        source = self.make_pdf()
        result = pdf_native.inspect(source, {"page_count": 1, "max_chars": 5, "ocr": "never"})
        self.assertEqual(result["sha256"], hashlib.sha256(source.read_bytes()).hexdigest())
        self.assertEqual(result["pages"][0]["text"], "Hello")
        self.assertEqual(result["pages"][0]["blocks"][0]["source"], "native")
        self.assertEqual(result["pages"][0]["blocks"][0]["object_index"], 0)
        following = pdf_native.inspect(source, result["next_options"])
        self.assertEqual(following["pages"][0]["text_offset"], 5)
        self.assertEqual(result["pages"][0]["coordinate_system"], "pdf_points_bottom_left")

    def test_image_input_formats_and_low_confidence(self):
        sample = {"text": "文字", "blocks": [{"text": "文字", "rect": [10, 10, 30, 20], "confidence": 40,
                  "low_confidence": True, "source": "ocr"}], "status": "ocr_text", "confidence_mean": 40, "low_confidence_count": 1}
        for extension in ("png", "jpg", "webp"):
            source = self.root / f"image.{extension}"
            Image.new("RGB", (100, 50), "white").save(source)
            with patch.object(pdf_native, "_ocr", return_value=sample.copy()):
                result = pdf_native.inspect(source, {})
            self.assertEqual(result["pages"][0]["low_confidence_count"], 1)
            self.assertEqual(result["pages"][0]["coordinate_system"], "pixels_top_left")

    def test_blank_pdf_distinguishes_no_native_text_and_no_ocr_detection(self):
        source = self.root / "blank.pdf"
        writer = PdfWriter()
        writer.add_blank_page(400, 300)
        writer.write(source)
        self.assertEqual(pdf_native.inspect(source, {"ocr": "never"})["pages"][0]["status"], "no_extractable_text")
        with patch.object(pdf_native, "_ocr", return_value={"text": "", "blocks": [], "status": "no_text_detected"}):
            self.assertEqual(pdf_native.inspect(source, {})["pages"][0]["status"], "no_text_detected")

    def test_missing_ocr_engine_is_explicit(self):
        source = self.root / "scan.png"
        Image.new("RGB", (100, 50), "white").save(source)
        self.assert_code("ocr_unavailable", pdf_native.inspect, source, {})

    def test_render_is_bounded_real_image_and_does_not_overwrite(self):
        source = self.make_pdf()
        first = pdf_native.render(source, self.root, {"page_count": 1})[0]
        second = pdf_native.render(source, self.root, {"page_count": 1, "crop": [0, 0, 100, 100]})[0]
        self.assertNotEqual(first["path"], second["path"])
        self.assertTrue(Path(first["path"]).is_absolute())
        self.assertLessEqual(first["bytes"], 1024 * 1024)
        with Image.open(first["path"]) as image:
            self.assertIsNotNone(ImageChops.difference(image.convert("RGB"), Image.new("RGB", image.size, "white")).getbbox())
        with Image.open(second["path"]) as image:
            self.assertEqual(image.size, (100, 100))

    def test_local_text_replacement_removes_original_and_preserves_other_pixels(self):
        source = self.make_pdf(texts=("Hello world",))
        original_hash = hashlib.sha256(source.read_bytes()).hexdigest()
        destination = self.root / "edited.pdf"
        result = pdf_native.edit(source, destination,
                                 [{"op": "replace_text", "page": 1, "old_text": "Hello world", "new_text": "Hello earth"}], {})
        self.assertTrue(result["verified"])
        text = PdfReader(destination).pages[0].extract_text()
        self.assertNotIn("Hello world", text)
        self.assertIn("Hello earth", text)
        self.assertIn("Unchanged content", text)
        self.assertEqual(original_hash, hashlib.sha256(source.read_bytes()).hexdigest())
        before = pdf_native.render(source, self.root, {"dpi": 72})[0]
        after = pdf_native.render(destination, self.root, {"dpi": 72})[0]
        with Image.open(before["path"]) as a, Image.open(after["path"]) as b:
            changed = ImageChops.difference(a.convert("RGB"), b.convert("RGB")).getbbox()
            self.assertIsNotNone(changed)
            self.assertLess(changed[3], 100)
            self.assertIsNone(ImageChops.difference(a.crop((0, 120, 400, 300)), b.crop((0, 120, 400, 300))).getbbox())

    def test_chinese_subset_font_replacement_embeds_missing_glyphs(self):
        source = self.make_pdf(texts=("原文测试",), chinese=True)
        destination = self.root / "chinese-edited.pdf"
        result = pdf_native.edit(source, destination,
                                 [{"op": "replace_text", "page": 1, "old_text": "原文测试", "new_text": "修改结果"}], self.options())
        self.assertTrue(result["operations"][0]["font_substituted"])
        page = pdf_native.inspect(destination, {"ocr": "never"})["pages"][0]
        self.assertNotIn("原文测试", page["text"])
        self.assertIn("修改结果", page["text"])
        self.assertEqual({block["text"] for block in page["blocks"]}, {"修改结果", "其他内容不变"})
        self.assertTrue(pdf_native.render(destination, self.root, {"dpi": 150}))

    def test_replace_rejects_overflow_partial_match_and_missing_font(self):
        source = self.make_pdf(texts=("Hello world",))
        target = self.root / "rejected.pdf"
        self.assert_code("unsafe_pdf_replacement", pdf_native.edit, source, target,
                         [{"op": "replace_text", "old_text": "Hello", "new_text": "Hi"}], {})
        self.assert_code("pdf_text_overflow", pdf_native.edit, source, target,
                         [{"op": "replace_text", "old_text": "Hello world", "new_text": "This text is much much too long"}], {})
        self.assert_code("missing_font", pdf_native.edit, source, target,
                         [{"op": "replace_text", "old_text": "Hello world", "new_text": "中文"}], {})
        self.assertFalse(target.exists())

    def test_ambiguous_objects_require_exact_index(self):
        source = self.root / "duplicate.pdf"
        document = canvas.Canvas(str(source))
        document.drawString(20, 200, "Same")
        document.drawString(20, 100, "Same")
        document.save()
        target = self.root / "one.pdf"
        operation = {"op": "replace_text", "old_text": "Same", "new_text": "New"}
        self.assert_code("unsafe_pdf_replacement", pdf_native.edit, source, target, [operation], {})
        pdf_native.edit(source, target, [{**operation, "object_index": 0}], {})
        self.assertEqual(PdfReader(target).pages[0].extract_text().count("Same"), 1)

    def test_merge_select_reorder_delete_rotate(self):
        source = self.make_pdf()
        extra = self.make_pdf("extra.pdf", texts=("Third page",))
        target = self.root / "pages.pdf"
        pdf_native.edit(source, target, [
            {"op": "merge", "paths": [str(extra)]},
            {"op": "reorder_pages", "pages": [3, 1, 2]},
            {"op": "delete_pages", "pages": [2]},
            {"op": "rotate_pages", "pages": [1], "angle": 90},
            {"op": "select_pages", "pages": [1]}], {})
        reader = PdfReader(target)
        self.assertEqual(len(reader.pages), 1)
        self.assertIn("Third page", reader.pages[0].extract_text())
        self.assertEqual(reader.pages[0].rotation, 90)
        self.assertTrue(pdf_native.render(target, self.root, {}))

    def test_form_fill_and_annotations_survive_save(self):
        source = self.root / "form.pdf"
        document = canvas.Canvas(str(source))
        document.acroForm.textfield(name="name", x=30, y=700, width=180, height=24)
        document.acroForm.checkbox(name="approved", x=30, y=650)
        document.showPage()
        document.save()
        target = self.root / "filled.pdf"
        pdf_native.edit(source, target, [{"op": "fill_form", "fields": {"name": "Alice", "approved": True}},
                         {"op": "annotate", "page": 1, "text": "核对完成", "rect": [200, 650, 220, 670]},
                         {"op": "annotate", "kind": "highlight", "page": 1, "rect": [30, 700, 150, 724]}], {})
        fields = PdfReader(target).get_fields()
        self.assertEqual(fields["name"]["/V"], "Alice")
        self.assertNotEqual(fields["approved"]["/V"], "/Off")
        self.assertEqual(len(PdfReader(target).pages[0]["/Annots"]), 4)
        self.assertTrue(pdf_native.render(target, self.root, {"page_count": 1}))
        if FONT.is_file():
            chinese = self.root / "filled-chinese.pdf"
            pdf_native.edit(source, chinese, [{"op": "fill_form", "fields": {"name": "中文办公"}}], self.options())
            fields = pdf_native.inspect(chinese, {"ocr": "never"})["fields"]
            self.assertIn({"name": "name", "type": "/Tx", "value": "中文办公", "states": []}, fields)
            appearance = PdfReader(chinese).pages[0]["/Annots"][0].get_object()["/AP"]["/N"]
            self.assertTrue(any(font.get_object().get("/ToUnicode") for font in appearance["/Resources"]["/Font"].values()))
            before = pdf_native.render(source, self.root, {"page_count": 1, "dpi": 150})[0]
            after = pdf_native.render(chinese, self.root, {"page_count": 1, "dpi": 150})[0]
            with Image.open(before["path"]) as a, Image.open(after["path"]) as b:
                self.assertIsNotNone(ImageChops.difference(a.convert("RGB"), b.convert("RGB")).getbbox())

    def test_page_selection_removes_deleted_page_form_values(self):
        source = self.root / "two-forms.pdf"
        drawing = canvas.Canvas(str(source))
        for name in ("removed", "retained"):
            drawing.acroForm.textfield(name=name, value=name, x=30, y=700, width=180, height=24)
            drawing.showPage()
        drawing.save()
        target = self.root / "selected.pdf"
        pdf_native.edit(source, target, [{"op": "select_pages", "pages": [2]}], {})
        fields = PdfReader(target).get_fields()
        self.assertEqual(set(fields), {"retained"})
        self.assertEqual(fields["retained"]["/V"], "retained")

    def test_search_layer_maps_rotated_and_cropped_pdf_coordinates(self):
        if not FONT.is_file():
            self.skipTest("Chinese TrueType fixture unavailable")
        source = self.root / "rotated-cropped.pdf"
        writer = PdfWriter()
        page = writer.add_blank_page(400, 300)
        page.cropbox.lower_left = (10, 20)
        page.cropbox.upper_right = (300, 220)
        page.rotate(90)
        writer.write(source)
        sample = {"text": "Office", "blocks": [{"text": "Office", "rect": [10, 10, 100, 30],
                  "confidence": 90, "low_confidence": False, "source": "ocr"}], "status": "ocr_text"}
        with patch.object(pdf_native, "_ocr", return_value=sample):
            pdf_native.convert(source, self.root / "search.pdf", self.options())
        result = pdf_native.inspect(self.root / "search.pdf", {"ocr": "never"})
        self.assertIn("Office", result["pages"][0]["text"])
        rect = result["pages"][0]["blocks"][0]["rect"]
        self.assertGreater(rect[3] - rect[1], rect[2] - rect[0])
        self.assertEqual(PdfReader(self.root / "search.pdf").pages[0].rotation, 90)

    def test_in_place_destination_and_invalid_pages_rejected_without_writes(self):
        source = self.make_pdf()
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        self.assert_code("unsafe_destination", pdf_native.edit, source, source, [{"op": "rotate_pages", "pages": [1], "angle": 90}], {})
        self.assert_code("invalid_argument", pdf_native.edit, source, self.root / "bad.pdf", [{"op": "delete_pages", "pages": [1, 2]}], {})
        self.assertEqual(digest, hashlib.sha256(source.read_bytes()).hexdigest())

    def test_native_pdf_conversion_routes_to_office_unless_reflow_requested(self):
        source = self.make_pdf()
        self.assert_code("office_conversion_required", pdf_native.convert, source, self.root / "out.docx", {})
        result = pdf_native.convert(source, self.root / "plain.docx", {"allow_text_reflow": True})
        self.assertTrue(result["verified"])

    @unittest.skipUnless(TESSERACT and FONT.is_file(), "Set MYCODEX_TEST_TESSERACT and MYCODEX_TEST_FONT for real OCR acceptance")
    def test_real_chinese_ocr_searchable_pdf_and_docx_roundtrip(self):
        source = self.root / "scan.png"
        image = Image.new("RGB", (1400, 500), "white")
        draw = ImageDraw.Draw(image)
        draw.text((60, 70), "日常办公 文件修改", font=ImageFont.truetype(str(FONT), 68), fill="black")
        draw.text((60, 200), "Office report 2026", font=ImageFont.truetype(str(FONT), 64), fill="black")
        image.save(source)
        options = {**self.options(), "ocr_psm": 6}
        inspected = pdf_native.inspect(source, options)
        text = inspected["pages"][0]["text"].replace(" ", "")
        self.assertIn("日常办公", text)
        self.assertIn("2026", text)
        pdf = self.root / "searchable.pdf"
        docx = self.root / "editable.docx"
        self.assertTrue(pdf_native.convert(source, pdf, options)["verified"])
        self.assertTrue(pdf_native.convert(source, docx, options)["verified"])
        self.assertIn("日常办公", PdfReader(pdf).pages[0].extract_text().replace(" ", "").replace("\n", ""))
        # Compare against the same image embedded without an OCR layer. PDFium
        # rounds physical page sizes, so a raw-image dimension equality is not a
        # useful assertion (a 240 pt page may round to 501 pixels at 150 dpi).
        scan_pdf = self.root / "image-only.pdf"
        document = canvas.Canvas(str(scan_pdf), pagesize=(672, 240))
        document.drawImage(ImageReader(image), 0, 0, width=672, height=240)
        document.save()
        reference = pdf_native.render(scan_pdf, self.root, {"dpi": 150})[0]
        rendered = pdf_native.render(pdf, self.root, {"dpi": 150})[0]
        with Image.open(rendered["path"]) as page, Image.open(reference["path"]) as original:
            self.assertEqual(page.size, original.size)
            self.assertIsNone(ImageChops.difference(page.convert("RGB"), original.convert("RGB")).getbbox())
        # Repeat from an actual image-only PDF, not only an image file.
        searched = self.root / "searchable-scan.pdf"
        self.assertTrue(pdf_native.convert(scan_pdf, searched, options)["verified"])
        self.assertIn("2026", PdfReader(searched).pages[0].extract_text())


if __name__ == "__main__":
    unittest.main()
