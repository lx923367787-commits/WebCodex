"""Shared implementation for the four bounded document workflow entry points."""
from __future__ import annotations

import base64
from copy import deepcopy
import hashlib
import json
from pathlib import Path
import re
import shutil
import uuid

from PIL import Image

from .common import ToolError, MAX_RESULT_BYTES, bounded_json, encode
from .document_store import DocumentStore, OFFICE_EXTENSIONS, atomic_json, digest_file, _regular
from .document_review import requirement, render_dependencies, material

# Executables, font paths and roots are operator configuration, never options.
READ_OPTIONS = {
    "page_start", "page_count", "text_offset", "max_chars", "max_items",
    "block_start", "block_count",
    "sheet", "range", "sheet_start", "sheet_count",
    "slide_start", "slide_count", "shape_start", "shape_count", "include_notes",
    "include_chart_data", "include_revisions", "revision_view",
    "ocr", "ocr_confidence", "dpi", "language",
    "field_offset", "max_fields", "block_offset", "max_blocks",
    "part", "crop",
    "review_probe",
}
EDIT_OPTIONS = {"recalculate"}
CONVERT_OPTIONS = {"ocr", "language", "dpi"}
_SHA = re.compile(r"[a-fA-F0-9]{64}\Z")


def _options(arguments, allowed, config):
    value = arguments.get("options", {})
    if not isinstance(value, dict) or value.keys() - allowed:
        raise ToolError("invalid_options", "Unknown or operator-only document option.")
    options = deepcopy(value)
    if "recalculate" in options and type(options["recalculate"]) is not bool:
        raise ToolError("invalid_options", "recalculate must be a boolean.")
    if "review_probe" in options and type(options["review_probe"]) is not bool:
        raise ToolError("invalid_options", "review_probe must be a boolean.")
    for key in ("page_count", "slide_count"):
        if key in options and (type(options[key]) is not int or not 1 <= options[key] <= 2):
            raise ToolError("invalid_range", "At most two pages or slides may be returned per call.")
    for key, maximum in (("max_chars", 32768), ("max_items", 200), ("block_count", 200),
                         ("shape_count", 100), ("sheet_count", 100)):
        if key in options and (type(options[key]) is not int or not 1 <= options[key] <= maximum):
            raise ToolError("invalid_range", f"{key} must be between 1 and {maximum}.")
    if "dpi" in options and (type(options["dpi"]) is not int or not 72 <= options["dpi"] <= 300):
        raise ToolError("invalid_range", "dpi must be between 72 and 300.")
    if "language" in options and options["language"] not in ("chi_sim", "eng", "chi_sim+eng", "eng+chi_sim"):
        raise ToolError("invalid_options", "OCR languages are chi_sim and/or eng.")
    if allowed == READ_OPTIONS:
        options.setdefault("max_chars", 12000)
        options.setdefault("max_items", 40)
    options.update({k: v for k, v in (config or {}).items()
                    if k in ("font_path", "tesseract_path", "tessdata_path") and v})
    return options


def _engine(path):
    if path.suffix.lower() in OFFICE_EXTENSIONS:
        from . import office_native
        return office_native
    from . import pdf_native
    return pdf_native


def _expected(arguments):
    value = arguments.get("expected_sha256")
    if not isinstance(value, str) or not _SHA.fullmatch(value):
        raise ToolError("expected_sha256_required", "Read the source and provide its SHA-256 before modifying it.")
    return value.lower()


def _fingerprint(operation, arguments):
    return hashlib.sha256(encode({"operation": operation, "arguments": arguments})).hexdigest()


def _cursor(value):
    try:
        if not isinstance(value, str) or len(value) > 8192:
            raise ValueError()
        result = json.loads(base64.urlsafe_b64decode(value + "=" * (-len(value) % 4)))
        if not isinstance(result, dict) or set(result) != {"path", "sha256", "view", "options"}:
            raise ValueError()
        return result
    except (ValueError, TypeError, UnicodeError):
        raise ToolError("invalid_cursor", "Invalid document cursor; read the document again.") from None


def _preview(store, item, index):
    source = Path(item["path"])
    try:
        source.resolve(strict=True).relative_to(store.work.resolve(strict=True))
    except (OSError, ValueError):
        raise ToolError("invalid_preview", "Renderer returned a path outside this operation's cache.") from None
    if source.is_symlink():
        raise ToolError("invalid_preview", "Renderer preview is not an ordinary file.")
    with Image.open(source) as opened:
        if opened.width * opened.height > 50_000_000:
            raise ToolError("preview_too_large", "Preview exceeds the pixel budget.")
        opened.load()
        picture = opened.convert("RGB")
    # Native MCP image bytes must remain <= 1 MiB. Preserve useful resolution
    # first; don't endlessly shrink unreadable pages merely to claim success.
    target = store.work / f"preview-{index}.jpg"
    picture.thumbnail((2400, 2400))
    for quality in (90, 80, 70, 60):
        picture.save(target, "JPEG", quality=quality, optimize=True)
        if target.stat().st_size <= 1024 * 1024:
            break
    else:
        raise ToolError("preview_too_large", "Select a smaller page region; the readable preview exceeds the native image limit.")
    return {
        **{k: v for k, v in item.items() if k not in ("path", "sha256", "mime_type")},
        "path": target.relative_to(store.ctx.root).as_posix(),
        "sha256": digest_file(target), "mime_type": "image/jpeg",
        "width": picture.width, "height": picture.height,
    }


def present_document(ctx, arguments, config=None, request_id=None):
    store = DocumentStore(ctx, request_id)
    args = deepcopy(arguments)
    policy = requirement(args, False)
    view = args.get("view", "auto")
    if view not in ("auto", "text", "pages"):
        raise ToolError("invalid_view", "view must be auto, text or pages.")
    if args.get("cursor"):
        previous = _cursor(args["cursor"])
        if previous["path"] != args.get("path"):
            raise ToolError("cursor_mismatch", "Cursor belongs to another document.")
        if args.get("options") or ("view" in args and view != previous["view"]):
            raise ToolError("cursor_mismatch", "Continue with the cursor without changing view or options.")
        args["options"] = previous["options"]
        args["expected_sha256"] = previous["sha256"]
        view = previous["view"]
    options = _options(args, READ_OPTIONS, config)
    source, snapshot, sha = store.snapshot(args["path"], args.get("expected_sha256"))
    if source.suffix.lower() in (".ppt", ".pptx", ".pptm"):
        options.setdefault("slide_count", 2)
    if source.suffix.lower() in (".png", ".jpg", ".jpeg", ".webp"):
        options.setdefault("ocr", "never")
    engine = _engine(source)
    content = engine.inspect(snapshot, options)
    previews, warnings = [], []
    visual_status = "not_requested"
    if view in ("auto", "pages") and not options.get("review_probe"):
        try:
            items = engine.render(snapshot, store.work, options)
            if len(items) > 2:
                raise ToolError("invalid_preview", "Renderer exceeded the two-page result budget.")
            previews = [_preview(store, item, i) for i, item in enumerate(items)]
            visual_status = "ready" if previews else "unavailable"
        except ToolError as error:
            warnings.append({"code": error.code, "message": error.message})
            visual_status = "unavailable"
    store.check_source(args["path"], sha)
    continuation = content.pop("next_options", None)
    dependencies = None
    if previews or policy["review"] or options.get("review_probe"):
        try:
            dependencies = render_dependencies(engine, snapshot, options)
        except ToolError as error:
            warnings.append({"code": error.code, "message": error.message})
            # Ordinary read-only images remain useful even when the host's
            # font inventory is unavailable. Their null dependency identity
            # cannot be admitted as managed review evidence.
            if policy["review"] or options.get("review_probe"):
                previews = []
                visual_status = "unavailable"
    store.check_source(args["path"], sha)
    review_material = material(args["path"], sha, content, previews, options, dependencies)
    review_material["dependencies_verified"] = dependencies is not None
    total = review_material["total_pages"]
    if view == "pages" and total and previews:
        last_page = max(preview["page"] for preview in previews)
        start_key = "slide_start" if source.suffix.lower() in (".ppt", ".pptx", ".pptm") else "page_start"
        count_key = "slide_count" if start_key == "slide_start" else "page_count"
        continuation = {start_key: last_page if start_key == "slide_start" else last_page + 1,
                        count_key: min(2, total - last_page)} if last_page < total else None
    next_cursor = None
    if continuation:
        merged = {k: v for k, v in options.items() if k in READ_OPTIONS}
        merged.update(continuation)
        next_cursor = base64.urlsafe_b64encode(encode({"path": args["path"], "sha256": sha,
                                                     "view": view, "options": merged})).decode("ascii").rstrip("=")
    for preview in previews:
        preview["source_sha256"] = sha
    result = {"request_id": store.request_id, "path": args["path"], "source_sha256": sha,
              "sha256": sha, "content": content, "previews": previews, "next_cursor": next_cursor,
              "visual_delivery": visual_status, "warnings": warnings, **policy,
              "total_pages": total, "review_material": review_material}
    if options.get("review_probe"):
        result["review_probe"] = {"status": "verified" if dependencies else "unavailable",
                                  "source_path": args["path"], "source_sha256": sha,
                                  "render_dependencies_sha256": dependencies}
    if next_cursor:
        review_material["continuation"] = {"tool": "present_document", "arguments": {
            "path": args["path"], "cursor": next_cursor, "review": policy["review"]}}
    if args.get("export_file"):
        result["export_file"] = {"path": args["path"], "sha256": sha}
    bounded_json(result, MAX_RESULT_BYTES)
    return result


def _operations(store, arguments):
    operations = deepcopy(arguments.get("operations"))
    if not isinstance(operations, list) or not 1 <= len(operations) <= 100:
        raise ToolError("invalid_operations", "Provide between 1 and 100 structured document operations.")
    for i, operation in enumerate(operations):
        if not isinstance(operation, dict) or not isinstance(operation.get("op"), str):
            raise ToolError("invalid_operations", "Every operation must have an op name.")
        # Resolve all external inputs here, never in a format backend. The model
        # cannot turn an image replacement/merge into arbitrary host file access.
        if operation["op"] == "replace_image":
            key = "image_path" if "image_path" in operation else "path"
            _, snapshot, _ = store.snapshot(operation.get(key), name=f"input-{i}")
            if snapshot.suffix.lower() not in (".png", ".jpg", ".jpeg", ".webp", ".tif", ".tiff"):
                raise ToolError("unsupported_format", "Image replacement needs an image file.")
            operation["image_path"] = str(snapshot)
            if key == "path":
                del operation["path"]
        if operation["op"] == "merge":
            paths = operation.get("paths")
            if not isinstance(paths, list) or not 1 <= len(paths) <= 10:
                raise ToolError("invalid_operations", "Merge accepts 1..10 project-relative PDFs.")
            snapshots = []
            for j, path in enumerate(paths):
                _, snapshot, _ = store.snapshot(path, name=f"input-{i}-{j}")
                if snapshot.suffix.lower() != ".pdf":
                    raise ToolError("unsupported_format", "PDF merge inputs must be PDFs.")
                snapshots.append(str(snapshot))
            operation["paths"] = snapshots
    return operations


def document_edit(ctx, arguments, config=None, request_id=None):
    expected = _expected(arguments)
    store = DocumentStore(ctx, request_id)
    fingerprint = _fingerprint("document_edit", arguments)
    recorded = store.receipt(fingerprint)
    if recorded:
        return recorded
    source, snapshot, sha = store.snapshot(arguments["path"], expected)
    options = _options(arguments, EDIT_OPTIONS, config)
    operations = _operations(store, arguments)
    if options.get("recalculate"):
        if source.suffix.lower() not in (".xls", ".xlsx", ".xlsm"):
            raise ToolError("invalid_options", "Recalculation applies only to spreadsheets.")
        if not any(item["op"] == "recalculate" for item in operations):
            if len(operations) == 100:
                raise ToolError("invalid_operations", "Include recalculate within the 100-operation budget.")
            operations.append({"op": "recalculate"})
    staged = store.work / ("edited" + source.suffix.lower())
    changes = _engine(source).edit(snapshot, staged, operations, options)
    if not staged.is_file():
        raise ToolError("verification_failed", "Document backend did not produce a saved file.")
    mode = arguments.get("save_mode", "copy" if arguments.get("output_path") else "in_place")
    return store.publish(arguments["path"], sha, staged, fingerprint, changes,
                         mode, arguments.get("output_path"), review=arguments.get("review"))


def document_convert(ctx, arguments, config=None, request_id=None):
    store = DocumentStore(ctx, request_id)
    fingerprint = _fingerprint("document_convert", arguments)
    recorded = store.receipt(fingerprint)
    if recorded:
        return recorded
    output_path = arguments.get("output_path")
    if not output_path:
        raise ToolError("invalid_argument", "Conversion requires a new output_path; the original is retained.")
    destination = store.source_path(output_path)
    source, snapshot, sha = store.snapshot(arguments["path"], arguments.get("expected_sha256"))
    options = _options(arguments, CONVERT_OPTIONS, config)
    staged = store.work / ("converted" + destination.suffix.lower())
    engine = _engine(source)
    if source.suffix.lower() == ".pdf" and staged.suffix.lower() == ".docx":
        # Explicit PDF-to-editable conversion is a text/OCR reflow. Native Word
        # PDF import can block on a UI-only conversion prompt; never invoke it.
        options["allow_text_reflow"] = True
    try:
        changes = engine.convert(snapshot, staged, options)
    except ToolError as error:
        if source.suffix.lower() != ".pdf" or staged.suffix.lower() != ".docx" or error.code not in (
                "unsupported_conversion", "complex_pdf", "use_office_conversion", "office_conversion_required", "unsupported_format"):
            raise
        from . import office_native
        changes = office_native.convert(snapshot, staged, options)
    return store.publish(arguments["path"], sha, staged, fingerprint, changes, "copy", output_path,
                         review=arguments.get("review"))


def document_restore(ctx, arguments, config=None, request_id=None):
    store = DocumentStore(ctx, request_id)
    if not arguments.get("backup_id"):
        records = store.list_backups(arguments["path"])
        offset = arguments.get("offset", 0)
        if type(offset) is not int or offset < 0:
            raise ToolError("invalid_range", "offset must be a nonnegative integer.")
        return {"path": arguments["path"], "backups": records[offset:offset + 50],
                "total": len(records), "next_offset": offset + 50 if offset + 50 < len(records) else None,
                **requirement(arguments, False)}
    expected = _expected(arguments)
    fingerprint = _fingerprint("document_restore", arguments)
    recorded = store.receipt(fingerprint)
    if recorded:
        return recorded
    store.check_source(arguments["path"], expected)
    backup = store.backup_snapshot(arguments["path"], arguments["backup_id"])
    staged = store.work / ("restored" + backup.suffix.lower())
    shutil.copyfile(backup, staged)
    return store.publish(arguments["path"], expected, staged, fingerprint,
                         {"restored_backup_id": arguments["backup_id"]}, review=arguments.get("review"))


HANDLERS = {name: globals()[name] for name in
            ("present_document", "document_edit", "document_convert", "document_restore")}


def dispatch(ctx, operation, arguments, config=None, request_id=None):
    if operation not in HANDLERS:
        raise ToolError("unknown_tool", "Unknown document operation.")
    if not isinstance(arguments, dict) or not isinstance(arguments.get("path"), str):
        raise ToolError("invalid_arguments", "A project-relative path is required.")
    common = {"path", "expected_sha256", "options", "review"}
    fields = {
        "present_document": common | {"view", "cursor", "export_file"},
        "document_edit": common | {"operations", "save_mode", "output_path"},
        "document_convert": common | {"output_path"},
        "document_restore": {"path", "expected_sha256", "backup_id", "offset", "review"},
    }[operation]
    if arguments.keys() - fields:
        raise ToolError("invalid_arguments", "Unknown document request fields.")
    requirement(arguments, operation in ("document_edit", "document_convert") or
                (operation == "document_restore" and bool(arguments.get("backup_id"))))
    bounded_json(arguments, 64 * 1024, "arguments_too_large")
    if operation == "document_edit" or (operation == "document_restore" and arguments.get("backup_id")):
        _expected(arguments)
    store = DocumentStore(ctx, request_id)
    fingerprint = _fingerprint(operation, arguments)
    # Serialize the complete lifecycle for an operation id, not just its final
    # source commit. Bind its inputs durably before any backend work starts.
    with store.operation_lock(store.work / "request-identity"):
        intent = store.receipts / (store.request_id + ".intent.json")
        presentation = store.receipts / (store.request_id + ".presentation.json")
        if intent.exists():
            if intent.is_symlink():
                raise ToolError("path_rejected", "Operation receipt is not an ordinary file.")
            previous = json.loads(intent.read_text(encoding="utf-8"))
            if previous.get("fingerprint") != fingerprint:
                raise ToolError("request_conflict", "Operation id is already bound to different arguments.")
            if operation == "present_document" and presentation.exists():
                _regular(presentation)
                return json.loads(presentation.read_text(encoding="utf-8"))
            if operation != "present_document" and not (operation == "document_restore" and not arguments.get("backup_id")):
                recorded = store.receipt(fingerprint)
                if recorded:
                    return recorded
                raise ToolError("outcome_unknown", "An interrupted attempt exists; inspect its receipt and original file before retrying.")
        else:
            atomic_json(intent, {"fingerprint": fingerprint, "operation": operation})
        result = HANDLERS[operation](ctx, arguments, config, store.request_id)
        if operation == "present_document":
            # The original images and material descriptor are immutable. A
            # fresh dependency probe or read needs a new request identity.
            atomic_json(presentation, result)
        return result
