"""Headless drawing entry point, used by the existing run_process tool."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import importlib.metadata
import hashlib
import io
import json
import os
from pathlib import Path
import sys
import zipfile

BUNDLE = Path(__file__).resolve().parent
# The embedded Python uses an isolated ._pth. Keep drawing dependencies private
# so installing this add-on cannot replace Office's existing Python packages.
sys.path.insert(0, str(BUNDLE))
sys.path.insert(0, str(BUNDLE / "drawing" / "vendor"))

from drawing.common import DrawingError, default_font, safe_path, sha256
from mycodex_tools.common import ToolError
from mycodex_tools.document_review import requirement

MAX_SPEC_BYTES = 256 * 1024
MAX_PREVIEW_BYTES = 1024 * 1024
TEMPLATES = ("flowchart", "architecture", "bar", "line", "pie", "scatter")


def _unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise DrawingError("invalid_spec", f"Duplicate JSON key: {key}")
        result[key] = value
    return result


def read_spec(root: Path, name: str):
    if name == "-":
        content = sys.stdin.buffer.read(MAX_SPEC_BYTES + 1)
    else:
        source = safe_path(root, name, suffixes={".json"}, must_exist=True)
        with source.open("rb") as stream:
            content = stream.read(MAX_SPEC_BYTES + 1)
    if len(content) > MAX_SPEC_BYTES:
        raise DrawingError("spec_too_large", "Drawing JSON exceeds 256 KiB.")
    try:
        data = json.loads(content.decode("utf-8-sig"), object_pairs_hook=_unique_pairs,
                          parse_constant=lambda value: (_ for _ in ()).throw(ValueError(value)))
    except (ValueError, UnicodeError, RecursionError) as error:
        raise DrawingError("invalid_spec", "Provide valid UTF-8 JSON with finite numbers.") from error
    if not isinstance(data, dict) or data.get("kind") not in ("chart", "flowchart", "architecture"):
        raise DrawingError("invalid_spec", "kind must be chart, flowchart, or architecture.")
    return data


def write_json(path, value):
    with Path(path).open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, ensure_ascii=False, allow_nan=False, indent=2)
        stream.write("\n")


def preview_image(png: Path, output: Path):
    from PIL import Image
    with Image.open(png) as image:
        if image.width * image.height > 16_000_000:
            raise DrawingError("invalid_artifact", "Preview image exceeds the pixel limit.")
        image.load()
        picture = image.convert("RGB")
    original = picture.size
    picture.thumbnail((2400, 2400), Image.Resampling.LANCZOS)
    stream = io.BytesIO()
    picture.save(stream, format="PNG", optimize=True)
    extension, mime = "png", "image/png"
    if stream.tell() > MAX_PREVIEW_BYTES:
        extension, mime = "jpg", "image/jpeg"
        for quality in (92, 85, 75, 65):
            stream = io.BytesIO()
            picture.save(stream, format="JPEG", quality=quality, optimize=True)
            if stream.tell() <= MAX_PREVIEW_BYTES:
                break
        if stream.tell() > MAX_PREVIEW_BYTES:
            for edge in (2000, 1600, 1200):
                picture.thumbnail((edge, edge), Image.Resampling.LANCZOS)
                stream = io.BytesIO()
                picture.save(stream, format="JPEG", quality=85, optimize=True)
                if stream.tell() <= MAX_PREVIEW_BYTES:
                    break
        if stream.tell() > MAX_PREVIEW_BYTES:
            raise DrawingError("preview_too_large", "Split this diagram into smaller diagrams for a readable preview.")
    path = output / f"preview.{extension}"
    with path.open("xb") as target:
        target.write(stream.getvalue())
    return path, {"mime_type": mime, "bytes": path.stat().st_size,
                  "width": picture.width, "height": picture.height,
                  "scaled": picture.size != original}


def _reproducer(renderer: Path):
    # User labels and paths are data in JSON; they never become executable code.
    return '''"""Re-render this saved definition/data from a project working directory."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

parser = argparse.ArgumentParser()
parser.add_argument("--output", required=True, help="New project-relative output folder")
parser.add_argument("--renderer", default=RENDERER_PATH)
parser.add_argument("--python", default=PYTHON_PATH)
args = parser.parse_args()
folder = Path(__file__).resolve().parent
root = Path.cwd().resolve()
try:
    relative = folder.relative_to(root).as_posix()
except ValueError:
    parser.error("Run from the project folder that contains this saved package.")
snapshot = folder / "snapshot.spec.json"
spec = json.loads((snapshot if snapshot.is_file() else folder / "normalized_spec.json").read_text(encoding="utf-8"))
if snapshot.is_file():
    spec["source"]["path"] = (relative + "/" if relative != "." else "") + "data.csv"
result = subprocess.run([args.python, args.renderer, "render", "--spec", "-", "--output", args.output],
                        input=json.dumps(spec, ensure_ascii=False).encode("utf-8"), cwd=root)
sys.exit(result.returncode)
'''.replace("RENDERER_PATH", repr(str(renderer))).replace("PYTHON_PATH", repr(sys.executable))


def render(root: Path, spec: dict, relative_output: str | None):
    try:
        policy = requirement(spec, True)
    except ToolError as error:
        raise DrawingError(error.code, error.message) from error
    render_spec = {key: value for key, value in spec.items() if key != "review"}
    name = relative_output or f'drawings/{spec["kind"]}-{datetime.now().strftime("%Y%m%d-%H%M%S-%f")}'
    output = safe_path(root, name)
    if output.exists():
        raise DrawingError("target_exists", "Output already exists; choose a new output folder such as drawings/v2.")
    font = default_font(BUNDLE)
    output.parent.mkdir(parents=True, exist_ok=True)
    safe_path(root, name)  # Recheck parents after creating them.
    output.mkdir()
    relative = output.relative_to(root).as_posix()
    try:
        cache = output / "_cache"
        cache.mkdir()
        os.environ["MPLCONFIGDIR"] = str(cache)
        context = {"project_root": root, "font_path": font,
                   "font_name": {"msyh.ttc": "Microsoft YaHei", "simhei.ttf": "SimHei",
                                 "simsun.ttc": "SimSun"}.get(font.name, "ZCOOL XiaoWei"),
                   "graphviz_dot": BUNDLE / "drawing" / "graphviz" / "bin" / ("dot.exe" if os.name == "nt" else "dot")}
        if spec["kind"] == "chart":
            from drawing.charts import render_chart
            result = render_chart(render_spec, output, context)
        else:
            from drawing.diagrams import render_diagram
            result = render_diagram(render_spec, output, context)
        artifacts = {}
        for role, path in result["outputs"].items():
            path = Path(path)
            if path.parent != output:
                raise DrawingError("invalid_artifact", "Renderer returned a file outside the output folder.")
            checked = safe_path(root, f"{relative}/{path.name}", must_exist=True)
            if not 0 < checked.stat().st_size <= 16 * 1024 * 1024:
                raise DrawingError("invalid_artifact", "Drawing artifact is empty or exceeds 16 MiB.")
            artifacts[role] = {"path": f"{relative}/{path.name}", "bytes": path.stat().st_size, "sha256": sha256(path)}
        write_json(output / "spec.json", spec)
        write_json(output / "normalized_spec.json", result.get("normalized_spec", spec))
        if "snapshot_spec" in result:
            snapshot = result["snapshot_spec"]
            snapshot["source"]["path"] = f"{relative}/data.csv"
            write_json(output / "snapshot.spec.json", snapshot)
        preview, preview_info = preview_image(Path(result["outputs"]["png"]), output)
        preview_info.update(path=f"{relative}/{preview.name}", sha256=sha256(preview))
        # This preview proves the PNG only. Independently rendered PDF/SVG are
        # separate delivery artifacts, never implicitly approved by this image.
        png = artifacts["png"]
        dependencies = {"font_sha256": sha256(font), "pipeline_sha256": sha256(Path(__file__)),
                        "renderer_sha256": sha256(BUNDLE / "drawing" / ("charts.py" if spec["kind"] == "chart" else "diagrams.py")),
                        "spec_sha256": sha256(output / "normalized_spec.json")}
        dependency_sha = hashlib.sha256(json.dumps(dependencies, sort_keys=True).encode()).hexdigest()
        preview_info.update(source_path=png["path"], source_sha256=png["sha256"],
                            render_source_path=png["path"], render_source_sha256=png["sha256"],
                            render_dependencies_sha256=dependency_sha, coverage_id="page:1", page=1)
        review_material = {"source_path": png["path"], "source_sha256": png["sha256"],
                           "render_source_path": png["path"], "render_source_sha256": png["sha256"],
                           "render_dependencies_sha256": dependency_sha,
                           "coverage": {"required": ["page:1"], "prepared": ["page:1"], "missing": [], "complete": True},
                           "delivery_status": "awaiting_native_image_delivery",
                           "managed_artifacts": [{"role": role, **artifact} for role, artifact in artifacts.items()],
                           "continuation": {"tool": "present_document", "arguments": {"path": png["path"], "view": "pages", "review": 1}},
                           "next_action": "Present the actual managed PNG source through present_document(path=source_path,view='pages',review=1), then submit a new version-bound model_self_check judgment after its native images arrive. Reading the separate scaled preview is only auxiliary viewing. Review any PDF/SVG separately if delivering it; this PNG cannot prove their layout."}
        if preview_info["scaled"]:
            result.setdefault("warnings", []).append({"code": "preview_scaled", "message": "预览已缩小；完整细节请查阅原始 PNG、SVG 或 PDF。复杂图请拆成多张检查。"})
        with (output / "reproduce.py").open("x", encoding="utf-8", newline="\n") as stream:
            stream.write(_reproducer(BUNDLE / "draw.py"))
        metadata = {key: value for key, value in result.items() if key not in ("outputs", "normalized_spec", "snapshot_spec")}
        manifest = {"schema_version": 1, "success": True, "kind": spec["kind"], **policy,
                    "review_material": review_material,
                    "created_at": datetime.now(timezone.utc).isoformat(), "font": font.name,
                    "output_dir": relative, "artifacts": artifacts, "preview": preview_info,
                    "result": metadata, "archive": f"{relative}/bundle.zip",
                    "edit": "Modify normalized_spec.json and render to a new folder. For charts, snapshot.spec.json uses the saved data.csv."}
        with zipfile.ZipFile(output / "bundle.zip", "x", compression=zipfile.ZIP_DEFLATED) as archive:
            for path in sorted(output.iterdir()):
                if path.is_file() and path.name != "bundle.zip":
                    archive.write(path, path.name)
            archive.writestr("manifest.json", json.dumps(manifest, ensure_ascii=False, allow_nan=False, indent=2) + "\n")
        # This external manifest is the final completion marker. A failed or
        # interrupted archive must never leave a successful manifest behind.
        write_json(output / "manifest.json", manifest)
        return {"success": True, "kind": spec["kind"], "output_dir": relative, **policy,
                "review_material": review_material,
                "manifest": f"{relative}/manifest.json", "preview": preview_info,
                "archive": f"{relative}/bundle.zip", "warnings": result.get("warnings", []),
                "next": "Follow review_material.continuation to present the actual managed PNG source and inspect its native images before delivery. preview.path is auxiliary viewing only. Read manifest for provenance and separately review delivered PDF/SVG outputs."}
    except Exception as error:
        # A partial directory is never reported as a successful drawing, and
        # preserved artifacts make a failed attempt inspectable without retrying
        # into an existing destination.
        try:
            write_json(output / "FAILED.json", {"success": False, "code": getattr(error, "code", "drawing_failed")})
        except OSError:
            pass
        raise


def doctor():
    versions = {}
    for module in ("matplotlib", "numpy", "openpyxl", "Pillow", "pypdf"):
        try:
            versions[module] = importlib.metadata.version(module)
        except importlib.metadata.PackageNotFoundError:
            versions[module] = None
    font = default_font(BUNDLE)
    dot = BUNDLE / "drawing" / "graphviz" / "bin" / ("dot.exe" if os.name == "nt" else "dot")
    return {"success": all(versions.values()) and dot.is_file(), "python": sys.executable,
            "versions": versions, "font": str(font), "graphviz": str(dot),
            "graphviz_available": dot.is_file(), "templates": list(TEMPLATES)}


def main(argv=None):
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("doctor")
    commands.add_parser("templates")
    init = commands.add_parser("init")
    init.add_argument("template", choices=TEMPLATES)
    init.add_argument("--spec", required=True, help="New project-relative .json file")
    drawing = commands.add_parser("render")
    drawing.add_argument("--spec", required=True, help="Project-relative JSON, or - for stdin")
    drawing.add_argument("--output", help="New project-relative output folder")
    args = parser.parse_args(argv)
    try:
        root = Path.cwd().resolve(strict=True)
        if args.command == "doctor":
            result = doctor()
        elif args.command == "templates":
            result = {"success": True, "templates": list(TEMPLATES), "usage": "init <template> --spec drawings/task.json"}
        elif args.command == "init":
            destination = safe_path(root, args.spec, suffixes={".json"})
            if destination.exists():
                raise DrawingError("target_exists", "Spec already exists; choose a new file.")
            value = json.loads((BUNDLE / "drawing" / "templates" / f"{args.template}.json").read_text(encoding="utf-8"))
            destination.parent.mkdir(parents=True, exist_ok=True)
            safe_path(root, args.spec)
            write_json(destination, value)
            result = {"success": True, "spec": destination.relative_to(root).as_posix(), "template": args.template}
        else:
            result = render(root, read_spec(root, args.spec), args.output)
        print(json.dumps(result, ensure_ascii=False, allow_nan=False))
        return 0 if result["success"] else 1
    except DrawingError as error:
        result = {"success": False, "code": error.code, "message": error.message}
    except (OSError, ImportError, ValueError, zipfile.BadZipFile) as error:
        result = {"success": False, "code": "drawing_failed", "message": f"{type(error).__name__}: {str(error)[:300]}"}
    print(json.dumps(result, ensure_ascii=False))
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
