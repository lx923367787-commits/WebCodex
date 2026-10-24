"""Assemble a fresh Windows portable package without starting the desktop or services."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import struct
import subprocess
import sys
import zipfile


REPO = Path(__file__).resolve().parents[1]
PROVIDERS = ("chat-handoff", "chat-agents", "subagent-pilot", "goal-provider")
CLIENT_FILES = ("WebCodex.exe", "webcodex-runtime/webcodex.exe",
                "webcodex-runtime/webcodex-server.exe", "webcodex-runtime/webcodex-runner.exe")
NPM_TREES = ("chat-handoff/extension-runtime/node_modules", "goal-provider/node_modules")
GUIDES = ("configure-providers.ps1", "setup-providers.ps1", "launch-portable.cmd",
          "launch-portable.ps1", "PORTABLE.zh-CN.md", "DEPENDENCIES.zh-CN.md", "dependencies.json",
          "HOME-DEVELOPMENT.zh-CN.md", "VALIDATION.md", "configure-native-host.py", "NATIVE-HOST-SETUP.zh-CN.md")


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def check_path(path: Path) -> None:
    """Reject symlinks/junctions, including ancestors of explicitly selected inputs."""
    for entry in (path, *path.parents):
        if not entry.exists() and not entry.is_symlink():
            continue
        attrs = entry.lstat()
        if stat.S_ISLNK(attrs.st_mode) or getattr(attrs, "st_file_attributes", 0) & 0x400:
            raise ValueError(f"Reparse point is not allowed: {entry}")


def relative_path(value: str) -> Path:
    relative = PurePosixPath(value.replace("\\", "/"))
    if not value or relative.is_absolute() or ".." in relative.parts or ":" in value:
        raise ValueError(f"Unsafe relative path: {value!r}")
    if relative == PurePosixPath("."):
        raise ValueError("Empty relative file path")
    return Path(*relative.parts)


def is_cache(path: Path) -> bool:
    return "__pycache__" in path.parts or path.suffix.lower() in (".pyc", ".pyo")


def copy_file(source: Path, destination: Path, expected: str | None = None,
              expected_size: int | None = None) -> None:
    check_path(source)
    if not source.is_file():
        raise ValueError(f"Missing regular input file: {source}")
    if destination.exists():
        raise ValueError(f"Payload collision: {destination}")
    expected = expected or digest(source)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    if digest(destination) != expected or (
            expected_size is not None and destination.stat().st_size != expected_size):
        raise ValueError(f"Input or copy does not match recorded bytes: {source}")


def tree_files(root: Path):
    check_path(root)
    if not root.is_dir():
        raise ValueError(f"Missing input directory: {root}")
    for current, dirs, files in os.walk(root, followlinks=False):
        for name in dirs + files:
            entry = Path(current) / name
            attrs = entry.lstat()
            if stat.S_ISLNK(attrs.st_mode) or getattr(attrs, "st_file_attributes", 0) & 0x400:
                raise ValueError(f"Reparse point is not allowed: {entry}")
        dirs[:] = sorted(name for name in dirs if name != "__pycache__")
        for name in sorted(files):
            entry = Path(current) / name
            if not is_cache(entry.relative_to(root)):
                yield entry


def copy_tree(source: Path, destination: Path) -> None:
    for path in tree_files(source):
        copy_file(path, destination / path.relative_to(source))


def copy_manifest_payload(source: Path, name: str, destination: Path) -> None:
    manifest_path = source / name
    check_path(manifest_path)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8-sig"))
    files = manifest["files"]
    if not isinstance(files, dict) or not files:
        raise ValueError(f"Manifest has no file map: {manifest_path}")
    seen: set[str] = set()
    included = {}
    omitted = []
    for relative, recorded in files.items():
        path = relative_path(relative)
        folded = path.as_posix().casefold()
        if folded in seen:
            raise ValueError(f"Case-insensitive manifest collision: {relative}")
        seen.add(folded)
        expected = recorded if isinstance(recorded, str) else recorded["sha256"]
        size = None if isinstance(recorded, str) else recorded.get("bytes", recorded.get("size"))
        original = source / path
        check_path(original)
        if not original.is_file() or digest(original) != expected or (
                size is not None and original.stat().st_size != size):
            raise ValueError(f"Source does not match its manifest: {original}")
        if is_cache(path):
            omitted.append(relative)
            continue
        copy_file(original, destination / path, expected, size)
        included[relative] = recorded
    manifest["files"] = included
    manifest["omitted_reproducible_caches"] = omitted
    manifest["input_manifest_sha256"] = digest(manifest_path)
    output_manifest = destination / name
    if output_manifest.exists():
        raise ValueError(f"Manifest destination collision: {output_manifest}")
    output_manifest.parent.mkdir(parents=True, exist_ok=True)
    output_manifest.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def run(args: list[str], cwd: Path | None = None, timeout: int = 60) -> str:
    result = subprocess.run(args, cwd=cwd, check=True, capture_output=True,
                            text=True, encoding="utf-8", errors="replace", timeout=timeout,
                            creationflags=subprocess.CREATE_NO_WINDOW)
    return result.stdout.strip()


def git(*args: str) -> str:
    return run(["git", "-c", "core.quotepath=false", "-C", str(REPO), *args])


def copy_tracked(prefix: str, destination: Path) -> None:
    paths = git("ls-files", "-z", "--", prefix).split("\0")
    paths = [path for path in paths if path]
    if not paths:
        raise ValueError(f"No tracked source files: {prefix}")
    for value in paths:
        relative = relative_path(value)
        child = relative.relative_to(Path(prefix))
        if not is_cache(child):
            copy_file(REPO / relative, destination / child)


def prepare_python(archive: Path, output: Path, wheels: Path, lock: Path) -> None:
    output.mkdir()
    with zipfile.ZipFile(archive) as bundle:
        seen: set[str] = set()
        for info in bundle.infolist():
            relative = relative_path(info.filename)
            folded = relative.as_posix().casefold()
            if folded in seen or stat.S_ISLNK(info.external_attr >> 16):
                raise ValueError(f"Unsupported Python archive entry: {info.filename}")
            seen.add(folded)
            destination = output / relative
            if info.is_dir():
                destination.mkdir(parents=True, exist_ok=True)
            else:
                destination.parent.mkdir(parents=True, exist_ok=True)
                with bundle.open(info) as source, destination.open("xb") as target:
                    shutil.copyfileobj(source, target)
    site_packages = output / "Lib" / "site-packages"
    run([sys.executable, "-m", "pip", "install", "--disable-pip-version-check",
         "--no-index", "--find-links", str(wheels), "--no-deps", "--no-compile",
         "--only-binary=:all:", "--target", str(site_packages), "-r", str(lock)], timeout=300)
    (output / "python311._pth").write_text(
        "python311.zip\n.\nLib/site-packages\n../chat-handoff\n../chat-agents\n../subagent-pilot\nimport site\n",
        encoding="utf-8")
    (output / "sitecustomize.py").write_text(
        '"""Activate installed wheel .pth files without any build-machine path."""\n'
        "from pathlib import Path\nimport site\n"
        'site.addsitedir(str(Path(__file__).resolve().parent / "Lib" / "site-packages"))\n',
        encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("output", "client-root", "office-payload", "drawing-addon", "provider-npm-root",
                 "node-root", "python-archive", "provider-wheels", "codebuddy-root",
                 "codex-package", "agy-bin", "tunnel-bin"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt" or sys.version_info[:2] != (3, 11) or struct.calcsize("P") != 8:
        raise SystemExit("Build with Windows x64 Python 3.11 and pip")
    for name, value in vars(args).items():
        value = value.absolute()
        check_path(value)
        setattr(args, name, value)
    if args.output.exists():
        raise SystemExit("Choose a new output directory; existing output is never overwritten")
    for name in ("client_root", "office_payload", "drawing_addon", "provider_npm_root", "node_root",
                 "provider_wheels", "codebuddy_root", "codex_package"):
        source = getattr(args, name)
        if not source.is_dir() or args.output == source or source in args.output.parents:
            raise SystemExit(f"Expected an input directory outside the output ancestry: {name}")
    dependency_file = REPO / "windows" / "dependencies.json"
    python_pin = next(item for item in json.loads(dependency_file.read_text(encoding="utf-8"))["artifacts"]
                      if item["id"] == "python-embedded")
    if digest(args.python_archive) != python_pin["sha256"]:
        raise SystemExit("Python embeddable archive SHA-256 does not match dependencies.json")
    tunnel_source = (REPO / "src" / "project_entry_openai_tunnel.rs").read_text(encoding="utf-8")
    tunnel_pin = re.search(r'\("windows",\s*"x86_64"\)\s*=>\s*TunnelClientAsset\s*\{.*?'
                           r'binary_sha256:\s*"([a-f0-9]{64})"', tunnel_source, re.DOTALL)
    if tunnel_pin is None or digest(args.tunnel_bin) != tunnel_pin.group(1):
        raise SystemExit("Tunnel client SHA-256 does not match the Windows x64 source pin")
    wheel_files = list(tree_files(args.provider_wheels))
    if not wheel_files or any(path.suffix.lower() != ".whl" for path in wheel_files):
        raise SystemExit("Provider wheelhouse must contain only wheel files")
    for required in (args.node_root / "node.exe", args.node_root / "LICENSE"):
        check_path(required)
        if not required.is_file():
            raise SystemExit(f"Incomplete official Node archive directory: {required.name}")
    for required in (args.codebuddy_root / "codebuddy.exe", args.codex_package / "bin" / "codex.js", args.agy_bin):
        check_path(required)
        if not required.is_file():
            raise SystemExit(f"Missing external CLI input: {required}")
    for prefix in (*("windows/" + name for name in PROVIDERS), "plugins/safe-delete"):
        untracked = git("ls-files", "--others", "--exclude-standard", "--", prefix)
        if untracked:
            raise SystemExit(f"Review and stage new provider source before packaging ({prefix}):\n{untracked}")
    head = git("rev-parse", "HEAD")
    dirty = bool(git("status", "--porcelain", "--untracked-files=normal"))
    args.output.mkdir(parents=True)
    for relative in CLIENT_FILES:
        copy_file(args.client_root / relative, args.output / relative)
    office = args.output / "mycodex-tools"
    copy_manifest_payload(args.office_payload, "MANIFEST.json", office)
    copy_manifest_payload(args.drawing_addon, "DRAWING_MANIFEST.json", office)
    for name in PROVIDERS:
        copy_tracked("windows/" + name, args.output / name)
    copy_tracked("plugins/safe-delete", args.output / "safe-delete")
    for relative in NPM_TREES:
        copy_tree(args.provider_npm_root / relative, args.output / relative)
    copy_tree(args.node_root, args.output / "node")
    copy_file(args.codebuddy_root / "codebuddy.exe", args.output / "cli" / "codebuddy" / "codebuddy.exe")
    copy_tree(args.codebuddy_root / "vendor", args.output / "cli" / "codebuddy" / "vendor")
    copy_tree(args.codex_package, args.output / "cli" / "codex")
    copy_file(args.agy_bin, args.output / "cli" / "agy" / "agy.exe")
    copy_file(args.tunnel_bin, args.output / "tunnel" / "tunnel-client.exe", tunnel_pin.group(1))
    for name in GUIDES:
        copy_file(REPO / "windows" / name, args.output / name)
    copy_file(REPO / "LICENSE", args.output / "LICENSE")
    offline = args.output / "offline"
    copied_archive = offline / python_pin["filename"]
    copy_file(args.python_archive, copied_archive, python_pin["sha256"], python_pin["bytes"])
    copied_wheels = offline / "provider-wheels"
    copy_tree(args.provider_wheels, copied_wheels)
    prepare_python(copied_archive, args.output / "provider-python", copied_wheels,
                   args.output / "chat-handoff" / "requirements.lock")
    executables = {}
    for relative in CLIENT_FILES:
        executable = args.output / relative
        record = {"sha256": digest(executable), "bytes": executable.stat().st_size}
        if relative != "WebCodex.exe":
            record["version"] = run([str(executable), "--version"], timeout=30)
        else:
            record["inspection"] = "hashed only; desktop was not started"
        executables[relative] = record
    cli_commands = {
        "cli/codebuddy/codebuddy.exe": [str(args.output / "cli/codebuddy/codebuddy.exe")],
        "cli/codex/bin/codex.js": [str(args.output / "node/node.exe"), str(args.output / "cli/codex/bin/codex.js")],
        "cli/agy/agy.exe": [str(args.output / "cli/agy/agy.exe")],
        "tunnel/tunnel-client.exe": [str(args.output / "tunnel/tunnel-client.exe")],
    }
    for relative, command in cli_commands.items():
        executable = args.output / relative
        executables[relative] = {"sha256": digest(executable), "bytes": executable.stat().st_size,
                                 "version": run([*command, "--version"], timeout=30)}
    if git("rev-parse", "HEAD") != head:
        raise RuntimeError("Repository HEAD changed during packaging; rebuild from a stable checkout")
    dirty = dirty or bool(git("status", "--porcelain", "--untracked-files=normal"))
    files = {}
    for path in tree_files(args.output):
        files[path.relative_to(args.output).as_posix()] = {"sha256": digest(path), "bytes": path.stat().st_size}
    manifest = {"schema_version": 1, "platform": "windows-x86_64",
                "created_at": datetime.now(timezone.utc).isoformat(),
                "source": {"head": head, "dirty": dirty}, "executables": executables,
                "python_archive_sha256": python_pin["sha256"],
                "tunnel_binary_sha256": tunnel_pin.group(1),
                "file_count": len(files), "files": files}
    (args.output / "MANIFEST.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(args.output), "files": len(files), "source": manifest["source"]}, ensure_ascii=False))


if __name__ == "__main__":
    main()
