"""Build a new Windows office payload without changing services or installed tools.

Run --runtime-only first when source development is ongoing. A later invocation
with --prepared-runtime copies that verified runtime into a *new* destination,
then takes a current source snapshot and writes its content manifest.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys

PACKAGE = Path(__file__).resolve().parent
REPOSITORY = PACKAGE.parents[1]


def ordinary(path: Path, directory=False):
    info = path.lstat()
    if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
        raise ValueError(f"Links and reparse points are not accepted: {path}")
    if not (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode)):
        raise ValueError(f"Unexpected filesystem object: {path}")


def safe_path(path: Path):
    path = Path(os.path.abspath(path))
    for ancestor in reversed([path, *path.parents]):
        if ancestor.exists() or ancestor.is_symlink():
            info = ancestor.lstat()
            if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
                raise ValueError(f"Path contains a link or reparse point: {ancestor}")
    return path


def copy_file(source: Path, target: Path):
    ordinary(source)
    target.parent.mkdir(parents=True, exist_ok=True)
    safe_path(target.parent)
    with source.open("rb") as incoming, target.open("xb") as outgoing:
        shutil.copyfileobj(incoming, outgoing, 1024 * 1024)


def copy_tree(source: Path, target: Path, skip=()):
    ordinary(source, directory=True)
    target.mkdir()
    for child in sorted(source.iterdir()):
        if child.name in skip or child.name == "__pycache__" or child.suffix == ".pyc":
            continue
        if child.is_dir():
            copy_tree(child, target / child.name)
        else:
            copy_file(child, target / child.name)


def run(command, *, timeout=300, cwd=None):
    environment = os.environ.copy()
    environment.pop("PYTHONHOME", None)
    environment.pop("PYTHONPATH", None)
    environment["PYTHONIOENCODING"] = "utf-8"
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    completed = subprocess.run([str(arg) for arg in command], cwd=cwd, env=environment,
                               capture_output=True, text=True, encoding="utf-8", errors="replace",
                               timeout=timeout, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    if completed.returncode:
        raise RuntimeError(f"Command failed ({completed.returncode}): {command[0]}\n{completed.stderr[-3000:]}")
    return completed.stdout


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def lock_dependencies(path):
    dependencies = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        match = re.fullmatch(r'([A-Za-z0-9_.-]+)==([^;\s]+)(?:;\s*sys_platform\s*==\s*"win32")?', line)
        if not match:
            raise ValueError(f"Only exact pinned Windows runtime dependencies are accepted: {line}")
        dependencies[re.sub(r"[-_.]+", "-", match[1]).lower()] = match[2]
    return dependencies


def prepare_runtime(args, output):
    if args.prepared_runtime:
        prepared = safe_path(args.prepared_runtime)
        ordinary(prepared / "MANIFEST.json")
        record = json.loads((prepared / "MANIFEST.json").read_text(encoding="utf-8"))
        if record.get("stage") != "runtime-only":
            raise ValueError("--prepared-runtime requires a verified runtime-only build")
        if record.get("requirements_lock_sha256") != sha256(PACKAGE / "requirements.lock"):
            raise ValueError("Runtime lock differs from current requirements.lock; prepare a new runtime")
        actual_files = {path.relative_to(prepared).as_posix() for path in prepared.rglob("*")
                        if path.is_file() and path != prepared / "MANIFEST.json"}
        if actual_files != set(record["files"]):
            raise ValueError("Prepared runtime has missing or unrecorded files; prepare a new runtime")
        for relative, expected in record["files"].items():
            source = safe_path(prepared / relative)
            source.relative_to(prepared)
            ordinary(source)
            if sha256(source) != expected["sha256"]:
                raise ValueError(f"Prepared runtime changed: {relative}")
        for name in ("python", "fonts", "tesseract"):
            copy_tree(prepared / name, output / name)
        return
    if not all((args.python_base, args.font_dir, args.tesseract_base)):
        raise ValueError("Specify --python-base, --font-dir and --tesseract-base, or --prepared-runtime")
    baseline, fonts, tesseract = (safe_path(path) for path in (args.python_base, args.font_dir, args.tesseract_base))
    # The embedded interpreter/stdlib is reused, never the old site-packages:
    # dependencies below come only from the exact runtime lock.
    copy_tree(baseline, output / "python", skip={"Lib"})
    library = output / "python" / "Lib" / "site-packages"
    library.mkdir(parents=True)
    run([sys.executable, "-m", "pip", "install", "--disable-pip-version-check", "--no-input",
         "--only-binary=:all:", "--no-deps", "--no-compile", "--target", library,
         "--requirement", PACKAGE / "requirements.lock"], timeout=600)
    # Embedded Python's explicit ._pth isolation can bypass normal site-package
    # discovery. Process the local pywin32.pth deliberately, including DLL setup.
    pth = output / "python" / "python311._pth"
    pth.write_text("python311.zip\n.\nLib/site-packages\n..\nimport site\n", encoding="utf-8")
    customize = output / "python" / "sitecustomize.py"
    with customize.open("x", encoding="utf-8") as stream:
        stream.write('"""Load only this embedded runtime\'s wheel .pth hooks."""\n'
                     'import site\nfrom pathlib import Path\n'
                     'site.addsitedir(str(Path(__file__).resolve().parent / "Lib" / "site-packages"))\n')
    copy_tree(fonts, output / "fonts")
    engine = output / "tesseract"
    engine.mkdir()
    for source in sorted(tesseract.iterdir()):
        if source.name.lower() == "tesseract.exe" or source.suffix.lower() == ".dll":
            copy_file(source, engine / source.name)
    for language in ("chi_sim", "eng"):
        copy_file(tesseract / "tessdata" / f"{language}.traineddata", engine / "tessdata" / f"{language}.traineddata")
    copy_file(tesseract / "tessdata" / "configs" / "tsv", engine / "tessdata" / "configs" / "tsv")
    copy_tree(tesseract / "doc", engine / "doc")


PROBE = r'''
import importlib.metadata as md,json,os,sys
from pathlib import Path
import pythoncom,pywintypes,win32api,pypdfium2,pypdfium2_raw
pythoncom.CoInitialize()
try: guid=str(pythoncom.CreateGuid())
finally: pythoncom.CoUninitialize()
assert win32api.GetCurrentProcessId()==os.getpid()
assert pywintypes.Time(0).year==1970
document=pypdfium2.PdfDocument.new()
page=document.new_page(100,100)
bitmap=page.render(scale=1)
assert bitmap.width==100 and bitmap.height==100
bitmap.close();page.close();document.close()
base=Path(sys.executable).resolve().parent
modules={name:str(module.__file__) for name,module in [('pythoncom',pythoncom),('pywintypes',pywintypes),('win32api',win32api),('pypdfium2',pypdfium2),('pypdfium2_raw',pypdfium2_raw)]}
for path in modules.values():Path(path).resolve().relative_to(base)
from pypdfium2.version import PDFIUM_INFO
print(json.dumps({'python':sys.version,'executable':sys.executable,'modules':modules,'pdfium':str(PDFIUM_INFO),'dependencies':{d.metadata['Name']:d.version for d in md.distributions(path=[str(base/'Lib'/'site-packages')])},'com_guid_created':bool(guid)},ensure_ascii=False))
'''


def verify_runtime(output):
    result = json.loads(run([output / "python" / "python.exe", "-B", "-c", PROBE], timeout=60))
    installed = {re.sub(r"[-_.]+", "-", name).lower(): version for name, version in result["dependencies"].items()}
    if installed != lock_dependencies(PACKAGE / "requirements.lock"):
        raise ValueError(f"Installed distributions differ from the exact runtime lock: {installed}")
    engine = output / "tesseract" / "tesseract.exe"
    result["tesseract_version"] = run([engine, "--version"], timeout=30).splitlines()[0]
    listed = run([engine, "--list-langs", "--tessdata-dir", output / "tesseract" / "tessdata"], timeout=30)
    if set(listed.splitlines()[1:]) != {"chi_sim", "eng"}:
        raise ValueError("Bundled OCR language inventory must contain exactly chi_sim and eng")
    result["ocr_languages"] = ["chi_sim", "eng"]
    return result


def source_state():
    return {
        "head": run(["git", "rev-parse", "HEAD"], cwd=REPOSITORY).strip(),
        "dirty": bool(run(["git", "status", "--porcelain", "--untracked-files=normal"], cwd=REPOSITORY).strip()),
    }


def build(args):
    if os.name != "nt" or sys.version_info[:2] != (3, 11):
        raise ValueError("Build this Windows payload with Python 3.11 on Windows")
    output = safe_path(args.output)
    if output.exists():
        raise ValueError("Destination already exists; choose a fresh output directory. Nothing was overwritten.")
    for source in (PACKAGE, args.python_base, args.font_dir, args.tesseract_base, args.prepared_runtime):
        if source:
            source = safe_path(source)
            if output == source or source in output.parents:
                raise ValueError("Output must not be inside any copied source tree")
    output.parent.mkdir(parents=True, exist_ok=True)
    safe_path(output.parent)
    output.mkdir()
    prepare_runtime(args, output)
    runtime = verify_runtime(output)
    if not args.runtime_only:
        for name in ("plugin.py", "document_worker.py", "mycodex_tools", "requirements.lock", "requirements.in",
                     "runner.example.toml", "README.md", "README.zh-CN.md", "build_local_bundle.py"):
            source = PACKAGE / name
            if source.is_dir():
                copy_tree(source, output / name)
            else:
                copy_file(source, output / name)
        copy_file(REPOSITORY / "LICENSE", output / "LICENSE")
        workflow = REPOSITORY / "docs" / "OFFICE_WORKFLOW.zh-CN.md"
        if workflow.exists():
            copy_file(workflow, output / "docs" / workflow.name)
    files = {}
    for path in sorted(output.rglob("*")):
        if path.is_dir():
            ordinary(path, directory=True)
            continue
        ordinary(path)
        files[path.relative_to(output).as_posix()] = {"sha256": sha256(path), "bytes": path.stat().st_size}
    manifest = {
        "stage": "runtime-only" if args.runtime_only else "candidate",
        "source": source_state(), "requirements_lock_sha256": sha256(PACKAGE / "requirements.lock"),
        "runtime": runtime, "files": files,
        "manifest_excludes_itself": True,
        "language_models": {name: files[f"tesseract/tessdata/{name}.traineddata"]["sha256"] for name in ("chi_sim", "eng")},
        "tesseract_sha256": files["tesseract/tesseract.exe"]["sha256"],
        "note": "Candidate only. No configuration, credential, service or installed payload was modified.",
    }
    with (output / "MANIFEST.json").open("x", encoding="utf-8") as stream:
        json.dump(manifest, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    print(json.dumps({"output": str(output), "stage": manifest["stage"], "files": len(files),
                      "bytes": sum(item["bytes"] for item in files.values()), "source": manifest["source"],
                      "dependencies": runtime["dependencies"], "tesseract_version": runtime["tesseract_version"]}, ensure_ascii=False))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--python-base", type=Path)
    parser.add_argument("--font-dir", type=Path)
    parser.add_argument("--tesseract-base", type=Path)
    parser.add_argument("--prepared-runtime", type=Path)
    parser.add_argument("--runtime-only", action="store_true")
    args = parser.parse_args()
    try:
        build(args)
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"Build failed: {error}\nPartial output, if any, was retained for inspection; never reuse it as a destination.\n")


if __name__ == "__main__":
    main()
