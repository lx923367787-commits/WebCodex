"""Real drawing CLI acceptance in an explicitly supplied, retained test project.

Usage: python e2e_drawing.py --python <runtime-python> --renderer <draw.py>
                            --project <authorized-test-directory>
No services, installed files, existing drawings or source workbooks are changed.
"""
from __future__ import annotations

import argparse
import csv
from datetime import datetime, timezone
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
import zipfile

from openpyxl import Workbook
from PIL import Image
from pypdf import PdfReader

TEMPLATES = ("flowchart", "architecture", "bar", "line", "pie", "scatter")


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require(condition, message):
    if not condition:
        raise AssertionError(message)


class Acceptance:
    def __init__(self, python, renderer, project):
        self.python, self.renderer, self.project = python, renderer, project
        self.receipt = {"success": False, "created_at": datetime.now(timezone.utc).isoformat(),
                        "project": str(project), "python": str(python), "renderer": str(renderer),
                        "checks": [], "outputs": {}}

    def call(self, *args, script=None, expected_error=None):
        command = [str(self.python), "-B", str(script or self.renderer), *map(str, args)]
        completed = subprocess.run(command, cwd=self.project, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=150,
                                   shell=False, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        try:
            value = json.loads(completed.stdout.decode("utf-8-sig"))
        except (UnicodeError, ValueError) as error:
            raise AssertionError(f"CLI did not return one JSON result: {completed.stderr.decode('utf-8', 'replace')[-500:]}") from error
        if expected_error:
            require(completed.returncode != 0 and value.get("success") is False,
                    f"Expected CLI rejection for {expected_error}: {value}")
            require(value.get("code") == expected_error, f"Wrong rejection: {value}")
        else:
            require(completed.returncode == 0 and value.get("success") is True, f"CLI failed: {value}")
        return value

    def project_file(self, relative):
        path = (self.project / relative).resolve(strict=True)
        require(path.is_relative_to(self.project), "Artifact escaped the test project")
        require(path.is_file(), "Artifact is not a regular file")
        return path

    def verify(self, result):
        manifest = json.loads(self.project_file(result["manifest"]).read_text(encoding="utf-8"))
        require(manifest["success"] is True, "Missing success manifest")
        paths = {}
        for role, record in manifest["artifacts"].items():
            path = self.project_file(record["path"])
            require(path.stat().st_size == record["bytes"] and digest(path) == record["sha256"],
                    f"Artifact receipt differs: {role}")
            paths[role] = path
        require({"png", "svg", "pdf"} <= paths.keys(), "Three drawing formats were not produced")
        with Image.open(paths["png"]) as image:
            require(image.format == "PNG" and min(image.size) > 100, "Invalid full PNG")
            image.verify()
        svg = ET.parse(paths["svg"]).getroot()
        require(svg.tag == "{http://www.w3.org/2000/svg}svg", "Invalid SVG document")
        with paths["pdf"].open("rb") as stream:
            pdf = PdfReader(stream, strict=True)
            require(not pdf.is_encrypted and len(pdf.pages) == 1, "Invalid single-page PDF")
        preview = self.project_file(result["preview"]["path"])
        require(0 < preview.stat().st_size <= 1024 * 1024, "Preview exceeds native MCP image bound")
        require(digest(preview) == manifest["preview"]["sha256"], "Preview SHA differs from receipt")
        with Image.open(preview) as image:
            image.verify()
        folder = self.project / result["output_dir"]
        for name in ("spec.json", "normalized_spec.json", "manifest.json"):
            require(isinstance(json.loads((folder / name).read_text(encoding="utf-8")), dict), "Invalid saved JSON")
        with zipfile.ZipFile(self.project_file(result["archive"])) as archive:
            require(archive.testzip() is None, "Archive CRC validation failed")
            require({"spec.json", "normalized_spec.json", "manifest.json", "reproduce.py"} <= set(archive.namelist()),
                    "Archive omitted editable source or reproduction script")
            require(not any("_cache" in name or ".." in Path(name).parts for name in archive.namelist()),
                    "Archive included cache or unsafe paths")
        return manifest, paths, svg

    def run(self):
        drawings = self.project / "drawings"
        drawings.mkdir()
        workbook = Workbook()
        sheet = workbook.active
        sheet.title = "Sheet1"
        sheet.append(["月份", "收入", "成本"])
        rows = [(f"{month}月", 12000 + month * 1000, 7000 + month * 400) for month in range(1, 7)]
        for row in rows:
            sheet.append(row)
        source = drawings / "data.xlsx"
        workbook.save(source)
        workbook.close()
        source_hash = digest(source)
        self.receipt["source_sha256"] = source_hash
        baseline = {}
        for template in TEMPLATES:
            spec_name, output = f"drawings/{template}-spec.json", f"drawings/{template}-v1"
            self.call("init", template, "--spec", spec_name)
            original_spec_hash = digest(self.project / spec_name)
            result = self.call("render", "--spec", spec_name, "--output", output)
            manifest, paths, svg = self.verify(result)
            require(digest(self.project / spec_name) == original_spec_hash, "Renderer modified the input JSON")
            require(digest(source) == source_hash, "Renderer modified the source workbook")
            if template in ("flowchart", "architecture"):
                normalized = json.loads((self.project / output / "normalized_spec.json").read_text(encoding="utf-8"))
                require(sum(node.attrib.get("class") == "node" for node in svg.iter()) == len(normalized["nodes"]),
                        "Diagram node count differs from JSON")
                require(sum(node.attrib.get("class") == "edge" for node in svg.iter()) == len(normalized["edges"]),
                        "Diagram edge count differs from JSON")
            else:
                provenance = manifest["result"]["metadata"]
                require(provenance["source_sha256"] == source_hash and provenance["data_rows"] == 6,
                        "Chart did not use the exact workbook and six selected data rows")
                snapshot = list(csv.reader(io.StringIO(paths["data_csv"].read_text(encoding="utf-8-sig"))))
                expected = [[str(index + 2), *map(str, row if template in ("bar", "line") else
                                                    (row[0], row[1]) if template == "pie" else (row[1], row[2]))]
                            for index, row in enumerate(rows)]
                require(snapshot[1:] == expected, f"Chart snapshot data mismatch for {template}")
            baseline[template] = (result, manifest, paths)
            self.receipt["outputs"][template] = {"manifest": result["manifest"], "preview": result["preview"]["path"]}
            self.receipt["checks"].append(f"{template}: init/render/JSON/SVG/PNG/PDF/ZIP/preview/source_hash")

        # Editing a saved JSON generates v2 without changing the v1 package.
        flow, _, paths = baseline["flowchart"]
        old_dot_hash = digest(paths["source_dot"])
        changed = json.loads((self.project / flow["output_dir"] / "normalized_spec.json").read_text(encoding="utf-8"))
        changed["nodes"][0]["label"] = "开始（修订版）"
        changed["edges"].append({"id": "review_v2", "from": changed["nodes"][0]["id"],
                                 "to": changed["nodes"][-1]["id"], "label": "测试快捷通道"})
        changed_path = drawings / "flowchart-v2-spec.json"
        changed_path.write_text(json.dumps(changed, ensure_ascii=False), encoding="utf-8")
        edited = self.call("render", "--spec", "drawings/flowchart-v2-spec.json", "--output", "drawings/flowchart-v2")
        _, _, svg = self.verify(edited)
        require(any(node.attrib.get("id") == "edge_review_v2" for node in svg.iter()), "Revised edge was not rendered")
        require("开始（修订版）" in "".join(svg.itertext()), "Revised label was not rendered")
        require(digest(paths["source_dot"]) == old_dot_hash, "Editing v2 changed v1")
        self.receipt["checks"].append("flowchart: edit label+edge to v2 preserves v1")

        for template in ("bar", "line", "pie", "scatter"):
            original, _, paths = baseline[template]
            result = self.call("--output", f"drawings/{template}-v2",
                               script=self.project / original["output_dir"] / "reproduce.py")
            manifest, reproduced, _ = self.verify(result)
            require(paths["data_csv"].read_bytes() == reproduced["data_csv"].read_bytes(), "Reproduction changed the data snapshot")
            require(manifest["result"]["metadata"]["source_sha256"] == digest(paths["data_csv"]),
                    "Reproducer did not bind the saved CSV bytes")
            self.receipt["checks"].append(f"{template}: reproduce.py v2 retains exact data.csv")

        previous = digest(baseline["bar"][2]["png"])
        self.call("render", "--spec", "drawings/bar-spec.json", "--output", "drawings/bar-v1", expected_error="target_exists")
        require(digest(baseline["bar"][2]["png"]) == previous, "Existing-output rejection changed the output")
        outside = self.project.parent / (self.project.name + "-drawing-escape")
        require(not outside.exists(), "Choose a test project without an existing escape sentinel")
        self.call("render", "--spec", "drawings/bar-spec.json", "--output", "../" + outside.name, expected_error="invalid_path")
        require(not outside.exists(), "Path traversal created an outside directory")

        formula_book = Workbook()
        formula_book.active.append(["月份", "收入", "成本"])
        formula_book.active.append(["合计", "=SUM(1,2)", 1])
        formula_book.active.title = "Sheet1"
        formula_path = drawings / "formula.xlsx"
        formula_book.save(formula_path)
        formula_book.close()
        formula_hash = digest(formula_path)
        bad = json.loads((drawings / "bar-spec.json").read_text(encoding="utf-8"))
        bad["source"].update(path="drawings/formula.xlsx", range="A1:C2")
        bad_path = drawings / "formula-spec.json"
        bad_path.write_text(json.dumps(bad, ensure_ascii=False), encoding="utf-8")
        self.call("render", "--spec", "drawings/formula-spec.json", "--output", "drawings/formula-rejected", expected_error="formula_cache_missing")
        require(digest(formula_path) == formula_hash, "Rejected formula source was modified")
        require(not (drawings / "formula-rejected" / "manifest.json").exists(), "Failed formula render published success")
        bad["source"]["path"] = "../outside.xlsx"
        (drawings / "source-escape.json").write_text(json.dumps(bad, ensure_ascii=False), encoding="utf-8")
        self.call("render", "--spec", "drawings/source-escape.json", "--output", "drawings/source-rejected", expected_error="invalid_path")
        require(digest(source) == source_hash, "Acceptance modified the source workbook")
        self.receipt["checks"].append("reject existing output, output/source traversal, uncached formulas without modifying sources")
        self.receipt["success"] = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", required=True, type=Path)
    parser.add_argument("--renderer", required=True, type=Path)
    parser.add_argument("--project", required=True, type=Path)
    args = parser.parse_args()
    python, renderer = args.python.resolve(strict=True), args.renderer.resolve(strict=True)
    project = args.project.resolve()
    project.mkdir(parents=True, exist_ok=True)
    receipt_path = project / "drawing-e2e-receipt.json"
    if (project / "drawings").exists() or receipt_path.exists():
        parser.error("Use a project with no drawings/ or previous receipt; existing test data is never deleted or overwritten.")
    acceptance = Acceptance(python, renderer, project)
    started = time.monotonic()
    try:
        acceptance.run()
    except Exception as error:
        acceptance.receipt["error"] = {"type": type(error).__name__, "message": str(error)[:1200]}
    acceptance.receipt["duration_seconds"] = round(time.monotonic() - started, 2)
    with receipt_path.open("x", encoding="utf-8") as stream:
        json.dump(acceptance.receipt, stream, ensure_ascii=False, indent=2)
    print(json.dumps({"success": acceptance.receipt["success"], "receipt": str(receipt_path),
                      "checks": len(acceptance.receipt["checks"]), "error": acceptance.receipt.get("error")}, ensure_ascii=False))
    return 0 if acceptance.receipt["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
