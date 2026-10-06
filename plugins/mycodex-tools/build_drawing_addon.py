"""Build a new, additive drawing payload; never modify an existing installation.

Use the development Python that has pip. The destination is a NEW directory.
Pass an extracted, SHA-256-verified official Graphviz 16.1.0 Windows ZIP root.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys

from build_local_bundle import copy_file, copy_tree, safe_path, sha256

PACKAGE = Path(__file__).resolve().parent
GRAPHVIZ_SHA256 = "733e49626c492242eb8dca30ea627b6ead20710e207998c7933b4909d92d6abc"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", required=True)
    parser.add_argument("--graphviz-root", required=True)
    parser.add_argument("--graphviz-archive", required=True)
    args = parser.parse_args()
    target = safe_path(Path(args.destination))
    graphviz = safe_path(Path(args.graphviz_root))
    archive = safe_path(Path(args.graphviz_archive))
    if target.exists():
        parser.error("Destination must not exist.")
    if sha256(archive) != GRAPHVIZ_SHA256:
        parser.error("Graphviz archive hash does not match the official 16.1.0 Windows ZIP.")
    if not (graphviz / "bin" / "dot.exe").is_file():
        parser.error("The prepared Graphviz root must contain bin/dot.exe.")
    # Verify every extracted file against the authenticated archive as well as
    # the ZIP hash, so a stale or modified extraction cannot be packaged.
    import hashlib
    import zipfile
    with zipfile.ZipFile(archive) as packed:
        expected = {}
        for entry in packed.infolist():
            if entry.is_dir():
                continue
            relative = Path(*Path(entry.filename).parts[1:])
            if not relative.parts or relative.is_absolute() or ".." in relative.parts:
                parser.error("Unexpected Graphviz archive path.")
            expected[relative.as_posix()] = hashlib.sha256(packed.read(entry)).hexdigest()
        actual = {item.relative_to(graphviz).as_posix(): sha256(item) for item in graphviz.rglob("*") if item.is_file()}
        if actual != expected:
            parser.error("Prepared Graphviz files differ from the verified archive.")
    target.mkdir(parents=True)
    copy_file(PACKAGE / "draw.py", target / "draw.py")
    copy_tree(PACKAGE / "drawing", target / "drawing", skip=("vendor", "graphviz"))
    copy_tree(graphviz, target / "drawing" / "graphviz")
    copy_file(PACKAGE.parents[1] / "docs" / "DRAWING_WORKFLOW.zh-CN.md", target / "DRAWING_GUIDE.md")
    subprocess.run([sys.executable, "-m", "pip", "install", "--disable-pip-version-check", "--only-binary=:all:",
                    "--no-deps", "--target", str(target / "drawing" / "vendor"),
                    "-r", str(PACKAGE / "drawing" / "requirements.lock")], check=True, timeout=300)
    files = {item.relative_to(target).as_posix(): sha256(item)
             for item in sorted(target.rglob("*")) if item.is_file() and "__pycache__" not in item.parts}
    manifest = {"schema_version": 1, "addon": "office-drawings", "graphviz_version": "16.1.0",
                "graphviz_archive_sha256": GRAPHVIZ_SHA256, "files": files,
                "requires": "Existing mycodex-tools Office payload (openpyxl, Pillow, pypdf)",
                "install": "Copy these additive files only when their target names do not exist; do not replace the Office MANIFEST.json or python packages."}
    (target / "DRAWING_MANIFEST.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({"success": True, "destination": str(target), "files": len(files)}))


if __name__ == "__main__":
    main()
