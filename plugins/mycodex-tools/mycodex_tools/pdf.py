"""Bounded PDF text access and simple, explicitly paginated PDF creation."""

from __future__ import annotations

import hashlib
import io
from pathlib import Path
from xml.sax.saxutils import escape

from pypdf import PdfReader
from reportlab.lib import colors
from reportlab.lib.enums import TA_LEFT
from reportlab.lib.pagesizes import A4
from reportlab.lib.styles import ParagraphStyle
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas
from reportlab.platypus import Paragraph, Table, TableStyle

from .common import Context, ToolError, arr, boolean, integer, obj, string


def _bounded_int(args, key, default, minimum, maximum):
    value = args.get(key, default)
    if type(value) is not int or not minimum <= value <= maximum:
        raise ToolError("invalid_argument", f"{key} must be between {minimum} and {maximum}.")
    return value


def _reader(data: bytes):
    try:
        reader = PdfReader(io.BytesIO(data), strict=False)
        if reader.is_encrypted:
            raise ToolError("unsupported_pdf", "Encrypted PDFs are not supported.")
        if not reader.pages:
            raise ToolError("invalid_pdf", "PDF contains no pages.")
        return reader
    except ToolError:
        raise
    except Exception as exc:
        raise ToolError("invalid_pdf", "Cannot read PDF structure.") from exc


def _page_range(reader, args, default_count, maximum):
    total = len(reader.pages)
    start = _bounded_int(args, "page_start", 1, 1, total)
    count = _bounded_int(args, "page_count", min(default_count, total - start + 1), 1, maximum)
    if start + count - 1 > total:
        raise ToolError("invalid_range", "Requested page range exceeds page_count_total.")
    return start, count, total


def _page_text(page):
    try:
        return page.extract_text() or ""
    except Exception as exc:
        raise ToolError("invalid_pdf", "Cannot extract text from a selected page.") from exc


def _bookmarks(reader):
    """Flatten with an explicit budget; never silently omit a bookmark."""
    result = []
    try:
        pending = [(iter(reader.outline), 0)]
        while pending:
            iterator, depth = pending[-1]
            item = next(iterator, None)
            if item is None:
                pending.pop()
                continue
            if isinstance(item, list):
                if depth >= 32:
                    raise ToolError("result_too_large", "Bookmark nesting exceeds 32; set include_bookmarks=false.")
                pending.append((iter(item), depth + 1))
                continue
            if len(result) >= 10000:
                raise ToolError("result_too_large", "More than 10000 bookmarks; set include_bookmarks=false.")
            page_number = reader.get_destination_page_number(item)
            result.append({"title": str(item.title), "level": depth, "page": page_number + 1 if page_number is not None and page_number >= 0 else None})
        return result
    except ToolError:
        raise
    except Exception as exc:
        raise ToolError("invalid_pdf", "Cannot read PDF bookmarks; retry with include_bookmarks=false.") from exc


def pdf_read(ctx: Context, args: dict) -> dict:
    data, sha256 = ctx.read_bytes(args["path"], ".pdf")
    reader = _reader(data)
    start, count, total = _page_range(reader, args, 5, 20)
    offset = _bounded_int(args, "text_offset", 0, 0, 2**31 - 1)
    max_chars = _bounded_int(args, "max_chars", 16000, 1, 32768)
    pages = []
    for index in range(start - 1, start + count - 1):
        text = _page_text(reader.pages[index])
        if offset > len(text):
            raise ToolError("invalid_range", f"text_offset exceeds text length on page {index + 1}; select a single page.")
        end = min(offset + max_chars, len(text))
        pages.append({"page": index + 1, "text": text[offset:end], "text_status": "extracted" if text.strip() else "no_extractable_text", "text_offset": offset, "chars_returned": end - offset, "chars_total": len(text), "next_offset": end if end < len(text) else None})
    result = {"path": args["path"], "sha256": sha256, "page_count_total": total, "page_start": start, "page_count": count, "pages": pages, "limitations": ["No OCR; textless or scanned pages report no_extractable_text.", "Text extraction does not reconstruct document layout."]}
    if args.get("include_metadata", True):
        result["metadata"] = {str(key): str(value) for key, value in (reader.metadata or {}).items()}
    if args.get("include_bookmarks", True):
        bookmarks = _bookmarks(reader)
        bookmark_start = _bounded_int(args, "bookmark_start", 0, 0, len(bookmarks))
        bookmark_count = _bounded_int(args, "bookmark_count", 100, 1, 200)
        result["bookmarks"] = bookmarks[bookmark_start:bookmark_start + bookmark_count]
        result["bookmark_start"] = bookmark_start
        result["bookmark_count_total"] = len(bookmarks)
        result["next_bookmark_start"] = bookmark_start + len(result["bookmarks"]) if bookmark_start + len(result["bookmarks"]) < len(bookmarks) else None
    return result


def pdf_search(ctx: Context, args: dict) -> dict:
    data, sha256 = ctx.read_bytes(args["path"], ".pdf")
    reader = _reader(data)
    start, count, total = _page_range(reader, args, 20, 100)
    query = args["query"]
    if not query:
        raise ToolError("invalid_argument", "query must be a nonempty literal substring.")
    max_matches = _bounded_int(args, "max_matches", 100, 1, 500)
    case_sensitive = args.get("case_sensitive", True)
    matches, textless_pages = [], []
    for index in range(start - 1, start + count - 1):
        text = _page_text(reader.pages[index])
        if not text.strip():
            textless_pages.append(index + 1)
        # Per-character folding preserves a mapping to original Unicode offsets,
        # including expansions such as German sharp-s -> ss.
        if case_sensitive:
            haystack, needle, mapping = text, query, None
        else:
            folded = [char.casefold() for char in text]
            haystack, needle = "".join(folded), query.casefold()
            mapping = [i for i, char in enumerate(folded) for _ in char]
        position = 0
        while True:
            found = haystack.find(needle, position)
            if found < 0:
                break
            if len(matches) >= max_matches:
                raise ToolError("result_too_large", "Match limit exceeded; narrow page range or use a more specific query.")
            first = found if mapping is None else mapping[found]
            last = found + len(needle) if mapping is None else mapping[found + len(needle) - 1] + 1
            matches.append({"page": index + 1, "text_offset": first, "text_end": last, "text": text[first:last]})
            position = found + len(needle)
    return {"path": args["path"], "sha256": sha256, "page_count_total": total, "page_start": start, "page_count": count, "query": query, "case_sensitive": case_sensitive, "matches": matches, "match_count": len(matches), "textless_pages": textless_pages}


def _font(ctx):
    if not ctx.font_path or not Path(ctx.font_path).is_file():
        raise ToolError("missing_font", "Configure --font-path with an existing Chinese-capable TTF font before creating PDFs.")
    font_path = Path(ctx.font_path)
    name = "MyCodex-" + hashlib.sha256(str(font_path.resolve()).encode()).hexdigest()[:16]
    try:
        if name not in pdfmetrics.getRegisteredFontNames():
            pdfmetrics.registerFont(TTFont(name, str(font_path)))
        return name
    except Exception as exc:
        raise ToolError("invalid_font", "Configured font cannot be registered as a TrueType font.") from exc


def _paragraph(text, font_name, heading=False, level=1):
    glyphs = pdfmetrics.getFont(font_name).face.charToGlyph
    for char in text:
        if char not in "\r\n\t" and not glyphs.get(ord(char)):
            raise ToolError("unsupported_font_glyph", f"Configured font does not cover U+{ord(char):04X}; select a font covering the requested text.")
    size = (20 if level == 1 else 15) if heading else 11
    style = ParagraphStyle("MyCodex", fontName=font_name, fontSize=size, leading=size * 1.5, wordWrap="CJK", alignment=TA_LEFT)
    return Paragraph(escape(text).replace("\n", "<br/>"), style)


def pdf_create(ctx: Context, args: dict) -> dict:
    font_name = _font(ctx)
    pages = args["pages"]
    if not 1 <= len(pages) <= 50:
        raise ToolError("invalid_argument", "pages must contain 1 to 50 explicit pages.")
    page_width, page_height = A4
    margin = 48
    width = page_width - 2 * margin

    def writer(path):
        document = canvas.Canvas(str(path), pagesize=A4, pageCompression=1)
        document.setTitle(args.get("title", ""))
        document.setAuthor(args.get("author", ""))
        for page_number, page in enumerate(pages, 1):
            top = page_height - margin
            for block in page["blocks"]:
                kind = block["type"]
                if kind != "heading" and "level" in block:
                    raise ToolError("invalid_argument", "level is supported only on heading blocks.")
                if kind in ("heading", "paragraph"):
                    if "text" not in block or "rows" in block:
                        raise ToolError("invalid_argument", f"{kind} requires text and cannot contain rows.")
                    level = _bounded_int(block, "level", 1, 1, 2)
                    flowable = _paragraph(block["text"], font_name, kind == "heading", level)
                    gap = 12 if kind == "heading" else 8
                elif kind == "table":
                    rows = block.get("rows", [])
                    if not rows or not rows[0] or "text" in block:
                        raise ToolError("invalid_argument", "table requires nonempty rows and cannot contain text.")
                    columns = len(rows[0])
                    if columns > 12 or len(rows) > 100 or any(len(row) != columns for row in rows):
                        raise ToolError("invalid_argument", "Tables require rectangular rows, at most 12 columns and 100 rows.")
                    table_data = [[_paragraph(cell, font_name) for cell in row] for row in rows]
                    flowable = Table(table_data, colWidths=[width / columns] * columns)
                    flowable.setStyle(TableStyle([("GRID", (0, 0), (-1, -1), 0.5, colors.HexColor("#CBD5E1")), ("BACKGROUND", (0, 0), (-1, 0), colors.HexColor("#EFF6FF")), ("VALIGN", (0, 0), (-1, -1), "TOP"), ("LEFTPADDING", (0, 0), (-1, -1), 6), ("RIGHTPADDING", (0, 0), (-1, -1), 6), ("TOPPADDING", (0, 0), (-1, -1), 5), ("BOTTOMPADDING", (0, 0), (-1, -1), 5)]))
                    gap = 12
                else:
                    raise ToolError("invalid_argument", "Unsupported PDF block type.")
                rendered_width, height = flowable.wrap(width, page_height)
                if height > top - margin or rendered_width > width + 0.01:
                    raise ToolError("layout_overflow", f"Content exceeds explicit page {page_number}; move blocks to another page or shorten content.")
                flowable.drawOn(document, margin, top - height)
                top -= height + gap
            document.showPage()
        document.save()

    def verifier(path):
        reader = _reader(path.read_bytes())
        if len(reader.pages) != len(pages):
            raise ToolError("verification_failed", "Generated PDF page count does not match requested pages.")
        for index, page in enumerate(reader.pages):
            source_text = []
            for block in pages[index]["blocks"]:
                if block["type"] == "table":
                    source_text.extend(cell for row in block["rows"] for cell in row)
                else:
                    source_text.append(block["text"])
            expected = "".join("".join(source_text).split())
            actual = "".join(_page_text(page).split())
            if actual != expected:
                raise ToolError("verification_failed", f"Generated PDF text differs from requested content on page {index + 1}.")

    result = ctx.publish(args["output_path"], ".pdf", writer, verifier)
    result.update({"page_count": len(pages), "limitations": ["Basic A4 headings, paragraphs and tables; explicit page overflow is rejected."]})
    return result


_range = {"page_start": integer("1-based first page; default 1."), "page_count": integer("Number of pages; must stay within the document.")}
_block = obj({"type": {"type": "string", "enum": ["heading", "paragraph", "table"]}, "text": string("Heading or paragraph plain text."), "level": integer("Heading level 1 or 2; default 1."), "rows": arr(arr(string(), max_items=12), max_items=100)}, ["type"])

TOOL_SPECS = [
    {"name": "pdf_read", "description": "Read PDF pages, metadata and bookmarks within the configured project root. Pages are 1-based (up to 20 per call); text_offset/max_chars form an explicit character window per page with next_offset for continuation. No OCR or encrypted PDFs. Narrow ranges or disable metadata/bookmarks if the result is too large.", "inputSchema": obj({"path": string(), **_range, "text_offset": integer("0-based text offset; default 0. Use one page for continuation."), "max_chars": integer("Characters per selected page, 1..32768; default 16000."), "include_metadata": boolean("Default true."), "include_bookmarks": boolean("Default true."), "bookmark_start": integer("0-based bookmark start; default 0."), "bookmark_count": integer("Bookmarks per call, 1..200; default 100.")}, ["path"])},
    {"name": "pdf_search", "description": "Find literal substrings in selected PDF pages (up to 100 per call, default up to 20). Returns original character offsets; case_sensitive defaults true. Match overflow is an error: narrow the range or query. No OCR, regex or encrypted PDFs.", "inputSchema": obj({"path": string(), "query": string("Nonempty literal substring.", max_length=4096), **_range, "case_sensitive": boolean("Default true."), "max_matches": integer("Maximum matches, 1..500; default 100.")}, ["path", "query"])},
    {"name": "pdf_create", "description": "Create a new PDF in the configured project root using an existing configured Chinese TTF font. Requires an absent output_path and existing parent directory. Explicit A4 pages contain headings, paragraphs or rectangular tables; page overflow is rejected. Writes a temporary file, verifies it and publishes without overwriting. If outcome is unknown, inspect the target before retrying.", "inputSchema": obj({"output_path": string(), "title": string(max_length=1024), "author": string(max_length=1024), "pages": arr(obj({"blocks": arr(_block, max_items=100)}, ["blocks"]), max_items=50)}, ["output_path", "pages"])},
]

HANDLERS = {"pdf_read": pdf_read, "pdf_search": pdf_search, "pdf_create": pdf_create}
