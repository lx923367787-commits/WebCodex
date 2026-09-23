"""Bounded JSON flow/architecture diagrams rendered by an operator-owned dot.

Graphviz CLI/escaping contracts: https://graphviz.org/doc/info/command.html
and https://graphviz.org/docs/attr-types/escString/ . No user DOT or HTML labels.
The caller owns final bundle publication; this module writes a fresh staging dir.
"""
from __future__ import annotations

from copy import deepcopy
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import unicodedata
import xml.etree.ElementTree as ET

from PIL import Image
from pypdf import PdfReader
from pypdf.errors import PyPdfError

from .common import DrawingError, safe_path

MAX_NODES, MAX_EDGES, MAX_GROUPS = 80, 160, 16
MAX_ARTIFACT_BYTES = 16 * 1024 * 1024
RENDER_TIMEOUT = 60
_ID = re.compile(r"[A-Za-z0-9_-]{1,64}\Z")
_HTML = re.compile(r"<(?:/?[A-Za-z][^>]*|[!?][^>]*)>")
_SHAPES = {"process": "box", "decision": "diamond", "start": "oval",
           "end": "oval", "database": "cylinder"}
_SVG_NS = "http://www.w3.org/2000/svg"


def _fail(code, message):
    raise DrawingError(code, message)


def _object(value, fields, where):
    if not isinstance(value, dict) or value.keys() - fields:
        _fail("invalid_spec", f"{where} must contain only documented fields.")


def _identifier(value, where):
    if not isinstance(value, str) or not _ID.fullmatch(value):
        _fail("invalid_id", f"{where} must use 1..64 ASCII letters, digits, underscores or hyphens.")
    return value


def _text(value, maximum, where, required=False):
    if not isinstance(value, str) or len(value) > maximum or (required and not value.strip()):
        _fail("invalid_text", f"{where} must be plain text of at most {maximum} characters.")
    value = value.replace("\r\n", "\n").replace("\r", "\n")
    if any(unicodedata.category(c) in ("Cc", "Cs") and c not in "\n\t" for c in value) or _HTML.search(value):
        _fail("invalid_text", f"{where} cannot contain HTML/XML tags or control characters.")
    return value


def normalize_spec(spec: dict) -> dict:
    _object(spec, {"kind", "title", "direction", "nodes", "edges", "groups"}, "Diagram")
    if spec.get("kind") not in ("flowchart", "architecture"):
        _fail("invalid_spec", "kind must be flowchart or architecture.")
    direction = spec.get("direction", "TB")
    if direction not in ("TB", "LR"):
        _fail("invalid_spec", "direction must be TB or LR.")
    nodes, edges, groups = spec.get("nodes"), spec.get("edges", []), spec.get("groups", [])
    for values, lower, upper, name in ((nodes, 1, MAX_NODES, "nodes"),
                                      (edges, 0, MAX_EDGES, "edges"),
                                      (groups, 0, MAX_GROUPS, "groups")):
        if not isinstance(values, list) or not lower <= len(values) <= upper:
            _fail("diagram_too_large", f"{name} must contain {lower}..{upper} items.")
    result = {"kind": spec["kind"], "title": _text(spec.get("title", ""), 160, "title"),
              "direction": direction, "groups": [], "nodes": [], "edges": []}
    group_ids, node_ids, edge_ids = set(), set(), set()
    for group in groups:
        _object(group, {"id", "label"}, "Group")
        identity = _identifier(group.get("id"), "Group id")
        if identity in group_ids:
            _fail("duplicate_id", "Group IDs must be unique.")
        group_ids.add(identity)
        result["groups"].append({"id": identity, "label": _text(group.get("label"), 120, "Group label", True)})
    for node in nodes:
        _object(node, {"id", "label", "shape", "group", "description"}, "Node")
        identity = _identifier(node.get("id"), "Node id")
        if identity in node_ids:
            _fail("duplicate_id", "Node IDs must be unique.")
        node_ids.add(identity)
        shape = node.get("shape", "process")
        if not isinstance(shape, str) or shape not in _SHAPES:
            _fail("invalid_shape", "shape must be process, decision, start, end or database.")
        item = {"id": identity, "label": _text(node.get("label"), 240, "Node label", True), "shape": shape}
        if "group" in node:
            group = _identifier(node["group"], "Node group")
            if group not in group_ids:
                _fail("unknown_reference", "A node references an unknown group.")
            item["group"] = group
        if "description" in node:
            item["description"] = _text(node["description"], 400, "Node description")
        result["nodes"].append(item)
    if group_ids - {node["group"] for node in result["nodes"] if "group" in node}:
        _fail("empty_group", "Each architecture group must contain a node; empty layers would not be rendered.")
    # Reserve supplied IDs first so generated IDs cannot collide with a later edge.
    for edge in edges:
        _object(edge, {"id", "from", "to", "label"}, "Edge")
        if "id" in edge:
            identity = _identifier(edge["id"], "Edge id")
            if identity in edge_ids:
                _fail("duplicate_id", "Edge IDs must be unique.")
            edge_ids.add(identity)
    for index, edge in enumerate(edges, 1):
        tail, head = _identifier(edge.get("from"), "Edge from"), _identifier(edge.get("to"), "Edge to")
        if tail not in node_ids or head not in node_ids:
            _fail("unknown_reference", "Every edge endpoint must reference an existing node.")
        identity = edge.get("id")
        if identity is None:
            candidate = index
            while f"edge_{candidate:03d}" in edge_ids:
                candidate += 1
            identity = f"edge_{candidate:03d}"
            edge_ids.add(identity)
        result["edges"].append({"id": identity, "from": tail, "to": head,
                                "label": _text(edge.get("label", ""), 120, "Edge label")})
    return result


def _wrap(text: str, width=26) -> str:
    """Wrap CJK by display width without truncation or changing the JSON source."""
    lines = []
    for paragraph in text.expandtabs(4).split("\n"):
        current, used = "", 0
        for char in paragraph:
            size = 0 if unicodedata.combining(char) else 2 if unicodedata.east_asian_width(char) in "WF" else 1
            if current and used + size > width:
                lines.append(current)
                current, used = "", 0
            current += char
            used += size
        lines.append(current)
    return "\n".join(lines)


def _quote(text):
    # Double every user backslash BEFORE adding our own newline escapes: \N,
    # \E and friends must remain literal user text, never Graphviz substitutions.
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n") + '"'


def build_dot(spec: dict, font_name: str) -> str:
    data = normalize_spec(spec)
    font = _quote(_text(font_name, 120, "Configured font name", True))
    lines = ["digraph diagram {",
             f'  graph [id="diagram_root", charset="UTF-8", rankdir={data["direction"]}, fontname={font}, fontsize=22, fontcolor="#173B63", bgcolor="white", label={_quote(_wrap(data["title"], 64))}, labelloc=t, labeljust=l, pad="0.3", nodesep="0.38", ranksep="0.6", size="20,20", dpi=144, splines=spline, outputorder=edgesfirst, newrank=true];',
             f'  node [fontname={font}, fontsize=14, fontcolor="#18354F", color="#709BC4", fillcolor="#EFF5FC", penwidth=1.3, style="rounded,filled", margin="0.20,0.12", width=2.0, height=0.58];',
             f'  edge [fontname={font}, fontsize=11, fontcolor="#375B7D", color="#6D8FAF", penwidth=1.2, arrowsize=0.72];']

    def node_line(node):
        shape = node["shape"]
        attrs = [f'id="node_{node["id"]}"', f'label={_quote(_wrap(node["label"]))}', f'shape={_SHAPES[shape]}']
        if shape in ("start", "end"):
            attrs.extend(['fillcolor="#255D95"', 'color="#255D95"', 'fontcolor="white"', 'style="filled"'])
        if shape == "end":
            attrs.append("peripheries=2")
        if shape == "decision":
            attrs.extend(['fillcolor="#E1ECF9"', 'style="filled"', 'margin="0.16,0.08"'])
        if shape == "database":
            attrs.append('style="filled"')
        if node.get("description"):
            attrs.append(f'tooltip={_quote(node["description"])}')
        return f'    "n_{node["id"]}" [{", ".join(attrs)}];'

    for group in data["groups"]:
        lines += [f'  subgraph "cluster_{group["id"]}" {{',
                  f'    graph [id="group_{group["id"]}", label={_quote(_wrap(group["label"], 40))}, fontsize=16, fontname={font}, fontcolor="#345D84", style="rounded,filled", fillcolor="#F7FAFE", color="#CFDEEE", margin=20];']
        lines.extend(node_line(node) for node in data["nodes"] if node.get("group") == group["id"])
        lines.append("  }")
    lines.extend(node_line(node) for node in data["nodes"] if "group" not in node)
    for edge in data["edges"]:
        lines.append(f'  "n_{edge["from"]}" -> "n_{edge["to"]}" [id="edge_{edge["id"]}", label={_quote(_wrap(edge["label"], 22))}];')
    lines.append("}")
    return "\n".join(lines) + "\n"


def _verify(paths, spec):
    for path in paths.values():
        if not path.is_file() or not 0 < path.stat().st_size <= MAX_ARTIFACT_BYTES:
            _fail("invalid_artifact", "A generated diagram is missing or exceeds 16 MiB.")
    try:
        svg = ET.parse(paths["svg"]).getroot()
        if svg.tag != f"{{{_SVG_NS}}}svg":
            raise ValueError("not SVG")
        ids = {node.attrib.get("id") for node in svg.iter()}
        expected = ({f'node_{node["id"]}' for node in spec["nodes"]}
                    | {f'edge_{edge["id"]}' for edge in spec["edges"]}
                    | {f'group_{group["id"]}' for group in spec["groups"]})
        if not expected <= ids:
            raise ValueError("missing node or edge")
        for css_class, count in (("node", len(spec["nodes"])), ("edge", len(spec["edges"])),
                                 ("cluster", len(spec["groups"]))):
            if sum(node.attrib.get("class") == css_class for node in svg.iter()) != count:
                raise ValueError("unexpected graph elements")
        for node in svg.iter():
            if node.tag.rsplit("}", 1)[-1] in ("script", "foreignObject", "image") or any(key.rsplit("}", 1)[-1] in ("href", "onload", "onclick") for key in node.attrib):
                raise ValueError("unexpected active SVG content")
        with Image.open(paths["png"]) as picture:
            if picture.format != "PNG" or max(picture.size) > 5000 or picture.width * picture.height > 16_000_000:
                raise ValueError("oversized PNG")
            picture.verify()
        with paths["pdf"].open("rb") as stream:
            pdf = PdfReader(stream, strict=True)
            if pdf.is_encrypted or len(pdf.pages) != 1:
                raise ValueError("invalid PDF pages")
            box = pdf.pages[0].mediabox
            if not (0 < float(box.width) <= 2000 and 0 < float(box.height) <= 2000):
                raise ValueError("invalid PDF dimensions")
    except (OSError, ValueError, ET.ParseError, PyPdfError) as error:
        raise DrawingError("invalid_artifact", "Graphviz output could not be verified.") from error


def render_diagram(spec: dict, output_dir: Path, context: dict) -> dict:
    data = normalize_spec(spec)
    executable = Path(context.get("graphviz_dot", ""))
    if not executable.is_absolute() or not executable.is_file() or executable.name.lower() not in ("dot", "dot.exe"):
        _fail("missing_graphviz", "Configure the absolute path of the trusted Graphviz dot executable.")
    font = context.get("font_name", "Microsoft YaHei" if os.name == "nt" else "Noto Sans CJK SC")
    dot = build_dot(data, font)
    root, directory = Path(context["project_root"]).resolve(strict=True), Path(output_dir).absolute()
    try:
        relative = directory.relative_to(root).as_posix()
    except ValueError:
        _fail("invalid_path", "Diagram output must be inside the registered project.")
    names = {"svg": "diagram.svg", "png": "diagram.png", "pdf": "diagram.pdf", "source_dot": "source.dot"}
    targets = {kind: safe_path(root, f"{relative}/{name}" if relative != "." else name) for kind, name in names.items()}
    if any(path.exists() for path in targets.values()):
        _fail("target_exists", "Diagram outputs already exist; generate into a new output directory.")
    directory.mkdir(parents=True, exist_ok=True)
    warnings = []
    if len(data["nodes"]) > 24 or len(data["edges"]) > max(12, len(data["nodes"]) * 1.5):
        warnings.append({"code": "complex_layout", "message": "较复杂的图可能保留交叉连线；业务关系未增删。请用 SVG/PDF 放大检查，必要时按业务拆图。"})
    env = os.environ.copy()
    env.update(context.get("env") or {})
    if context.get("font_path"):
        font_path = Path(context["font_path"])
        if not font_path.is_absolute() or not font_path.is_file():
            _fail("missing_font", "Configured diagram font file is unavailable.")
        env["DOTFONTPATH"] = str(font_path.parent)
    with tempfile.TemporaryDirectory(prefix=".diagram-", dir=directory) as temporary:
        stage = Path(temporary)
        paths = {kind: stage / name for kind, name in names.items()}
        paths["source_dot"].write_text(dot, encoding="utf-8", newline="\n")
        # Fixed per-format DPI keeps PDF/SVG dimensions in points while PNG
        # remains readable at 2x density. The timeout never resets between calls.
        started = time.monotonic()
        renderer_warning = False
        for kind, format_name, dpi in (("svg", "svg", 72), ("png", "png:cairo", 144), ("pdf", "pdf", 72)):
            remaining = RENDER_TIMEOUT - (time.monotonic() - started)
            if remaining <= 0:
                _fail("renderer_timeout", "Graphviz exceeded the 60-second diagram rendering deadline.")
            argv = [str(executable.resolve()), "-Kdot", f"-T{format_name}", f"-Gdpi={dpi}", "-o", str(paths[kind])]
            try:
                completed = subprocess.run(argv, input=dot.encode("utf-8"), stdout=subprocess.DEVNULL,
                                           stderr=subprocess.PIPE, cwd=stage, env=env, shell=False,
                                           timeout=remaining,
                                           creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            except subprocess.TimeoutExpired:
                _fail("renderer_timeout", "Graphviz exceeded the 60-second diagram rendering deadline.")
            except OSError as error:
                raise DrawingError("renderer_failed", "The configured Graphviz executable could not start.") from error
            if completed.returncode:
                _fail("renderer_failed", "Graphviz could not render all SVG/PNG/PDF outputs; check the configured renderer and fonts.")
            renderer_warning |= bool(completed.stderr.strip())
        if renderer_warning:
            warnings.append({"code": "renderer_warning", "message": "Graphviz reported a layout or font warning; inspect the generated diagram."})
        _verify(paths, data)
        # The caller owns publication of this staging directory. Hard links only
        # claim absent output names, so a concurrent writer is never overwritten.
        for kind, path in paths.items():
            try:
                os.link(path, targets[kind])
            except FileExistsError:
                _fail("target_exists", "A diagram output appeared concurrently; no existing file was overwritten.")
            except OSError as error:
                raise DrawingError("publish_failed", "Filesystem could not publish verified diagram outputs without overwriting.") from error
    return {"outputs": {kind: str(path) for kind, path in targets.items()},
            "normalized_spec": deepcopy(data), "warnings": warnings, "engine": "graphviz-dot",
            "node_count": len(data["nodes"]), "edge_count": len(data["edges"]),
            "group_count": len(data["groups"]), "duration_ms": round((time.monotonic() - started) * 1000)}
