"""Review requirements and real-render provenance, never model approval.

Host Goal owns review rounds/verdicts. These bounded material descriptors let
that owner bind native image delivery to the exact saved document version.
"""
from __future__ import annotations

import hashlib
import importlib.metadata
import importlib.util
import os
from pathlib import Path
import re
import time

from .common import ToolError, encode
from .document_store import digest_file


def requirement(arguments, mutation):
    requested = arguments.get("review")
    if "review" in arguments and (type(requested) is not int or requested not in (0, 1)):
        raise ToolError("invalid_review", "review must be the integer 0 or 1.")
    effective = 1 if mutation else requested or 0
    reason = ("Layout delivery requires actual image review; requested review=0 was corrected to 1."
              if mutation and requested == 0 else
              "Creating or modifying a layout delivery requires actual image review."
              if mutation else "Explicit review=1 requires a new model judgment."
              if effective else "Read-only inspection does not start a delivery review.")
    return {"review": effective, "review_reason": reason,
            "review_status": "pending" if effective else "not_required"}


def render_options(source, options):
    office = source.suffix.lower() in (".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx")
    return {"dpi": options.get("dpi", 120 if office else 150),
            "revision_view": options.get("revision_view", "final")}


def _environment_fingerprints(source):
    """Bound the renderer/font inventory; no stale mtime-only hash cache.

    Office/PDFium may substitute fonts not named by the source, so those
    formats include the two standard Windows font directories. Bitmap reads
    do not depend on fonts. Unsupported platform font resolution is explicit.
    """
    paths = set()
    for module in ("PIL._imaging", "pypdfium2_raw"):
        spec = importlib.util.find_spec(module)
        if spec and spec.origin:
            binary = Path(spec.origin)
            paths.add(binary)
            if module == "pypdfium2_raw":
                paths.update(binary.parent.glob("*pdfium*.dll"))
                paths.update(binary.parent.glob("*pdfium*.so*"))
                paths.update(binary.parent.glob("*pdfium*.dylib"))
    needs_fonts = source.suffix.lower() not in (".png", ".jpg", ".jpeg", ".webp")
    configuration = {}
    if needs_fonts:
        if os.name != "nt":
            raise ToolError("dependency_probe_unavailable", "System font dependencies cannot yet be verified on this platform.")
        import winreg
        for hive_name, hive in (("machine", winreg.HKEY_LOCAL_MACHINE), ("user", winreg.HKEY_CURRENT_USER)):
            for branch in ("Fonts", "FontSubstitutes", r"FontLink\SystemLink"):
                identity = hive_name + ":" + branch
                try:
                    with winreg.OpenKey(hive, "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\" + branch) as key:
                        values = []
                        for index in range(winreg.QueryInfoKey(key)[1]):
                            name, value, _ = winreg.EnumValue(key, index)
                            values.append((name, value))
                            if branch == "Fonts" and isinstance(value, str) and Path(value).is_absolute():
                                paths.add(Path(value))
                        configuration[identity] = sorted(values)
                except FileNotFoundError:
                    configuration[identity] = []
        directories = [Path(os.environ.get("WINDIR", "C:/Windows")) / "Fonts"]
        if os.environ.get("LOCALAPPDATA"):
            directories.append(Path(os.environ["LOCALAPPDATA"]) / "Microsoft/Windows/Fonts")
        for directory in directories:
            if directory.is_dir():
                paths.update(path for path in directory.iterdir()
                             if path.suffix.lower() in (".ttf", ".ttc", ".otf", ".fon") and path.is_file())
        if source.suffix.lower() != ".pdf":
            from .office_native import FORMATS, PROGIDS
            try:
                progid = PROGIDS[FORMATS[source.suffix.lower()]]
                clsid = winreg.QueryValue(winreg.HKEY_CLASSES_ROOT, progid + r"\CLSID")
                command = winreg.QueryValue(winreg.HKEY_CLASSES_ROOT, "CLSID\\" + clsid + r"\LocalServer32")
                match = re.match(r'^\s*"([^"]+)"|^\s*(.*?\.exe)(?:\s|$)', command, re.I)
                if not match:
                    raise ValueError()
                paths.add(Path(match.group(1) or match.group(2)))
                configuration["office_registration"] = command
            except (OSError, KeyError, ValueError):
                raise ToolError("dependency_probe_unavailable", "Registered Office renderer binary cannot be fingerprinted.") from None
    if len(paths) > 1024:
        raise ToolError("dependency_probe_unavailable", "Renderer/font inventory exceeds 1024 files.")
    deadline, total, result = time.monotonic() + 30, 0, []
    try:
        for path in sorted(paths):
            digest = hashlib.sha256()
            with path.open("rb") as stream:
                while chunk := stream.read(1024 * 1024):
                    total += len(chunk)
                    if total > 1024 * 1024 * 1024 or time.monotonic() > deadline:
                        raise ToolError("dependency_probe_unavailable", "Renderer/font fingerprints exceed the 1 GiB or 30 second budget.")
                    digest.update(chunk)
            result.append((str(path), digest.hexdigest()))
    except OSError:
        raise ToolError("dependency_probe_unavailable", "A renderer or font dependency is unavailable.") from None
    return {"files": result, "font_configuration": configuration}


def render_dependencies(engine, source, options):
    # Pagination is not a render dependency. A crop is a separate coverage unit.
    versions = {}
    for name in ("Pillow", "pypdfium2", "pypdf"):
        try:
            versions[name] = importlib.metadata.version(name)
        except importlib.metadata.PackageNotFoundError:
            versions[name] = "unavailable"
    dependencies = {"backend_sha256": digest_file(engine.__file__),
                    "preview_pipeline_sha256": digest_file(Path(__file__).with_name("documents.py")),
                    "dependency_probe_sha256": digest_file(__file__),
                    "versions": versions,
                    "options": render_options(source, options),
                    "environment": _environment_fingerprints(source)}
    if options.get("font_path"):
        dependencies["font_sha256"] = digest_file(options["font_path"])
    return hashlib.sha256(encode(dependencies)).hexdigest()


def material(path, sha, content, previews, options, dependencies):
    total = content.get("page_count_total", content.get("slide_count"))
    if previews:
        rendered_totals = {item.get("page_count_total") for item in previews}
        if len(rendered_totals) == 1 and None not in rendered_totals:
            rendered_total = rendered_totals.pop()
            if total is not None and total != rendered_total:
                raise ToolError("render_source_mismatch", "Rendered page count differs from the inspected source.")
            total = rendered_total
    if total is not None and (type(total) is not int or not 1 <= total <= 1000):
        raise ToolError("page_limit", "Review coverage supports 1..1000 actual pages.")
    required = [f"page:{page}" for page in range(1, total + 1)] if total else ["all_pages:unresolved"]
    # Native Excel print export alone cannot prove all used cells/chart regions
    # are included in print areas. Keep those scopes explicitly outstanding.
    if Path(path).suffix.lower() in (".xls", ".xlsx", ".xlsm"):
        required.extend(content.get("review_sheet_scopes", ["all_sheet_regions:unresolved"]))
    prepared = []
    for preview in previews:
        if preview.get("render_source_sha256") != sha:
            raise ToolError("render_source_mismatch", "Preview was not rendered from this exact source snapshot.")
        page = preview.get("page", preview.get("slide", -1) + 1)
        if type(page) is not int or page < 1 or (total and page > total):
            raise ToolError("invalid_preview", "Preview has an invalid source page number.")
        scope = f"page:{page}"
        if preview.get("crop"):
            scope += ":crop:" + ",".join(str(value) for value in preview["crop"])
        preview.update(page=page, coverage_id=scope, render_source_path=path,
                       render_dependencies_sha256=dependencies, render_options=render_options(Path(path), options))
        prepared.append(scope)
    return {"source_path": path, "source_sha256": sha, "render_source_path": path,
            "render_source_sha256": sha, "render_dependencies_sha256": dependencies,
            "render_options": render_options(Path(path), options),
            "total_pages": total, "coverage": {"required": required, "prepared": prepared,
                "missing": [scope for scope in required if scope not in prepared],
                "complete": all(scope in prepared for scope in required)},
            "delivery_status": "awaiting_native_image_delivery",
            "evidence_reuse": "No cross-version reuse; rerender the complete delivery after any source or dependency change.",
            "next_action": "Return all required images through the native image channel, then submit a new model_self_check judgment with page-specific findings and this version's evidence. Prepared paths or renderer success are not approval."}


def pending_delivery(result, arguments):
    policy = requirement(arguments, True)
    path, sha = result["path"], result["after_sha256"]
    return {**result, **policy, "review_material": {
        "source_path": path, "source_sha256": sha,
        "coverage": {"required": ["all_pages:unresolved"], "prepared": [],
                     "missing": ["all_pages:unresolved"], "complete": False},
        "delivery_status": "awaiting_render",
        "continuation": {"tool": "present_document", "arguments": {
            "path": path, "expected_sha256": sha, "view": "pages", "review": 1}},
        "next_action": "Machine validation succeeded. Render the actual saved output, return every required image to the host LLM, and submit its version-bound review before completing the managed stage."}}
