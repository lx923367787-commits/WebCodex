"""CLI-only install/uninstall smoke using the installer's dedicated HKCU objects."""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import subprocess
import time
import uuid
import winreg
from pathlib import Path

BUILD = Path(__file__).resolve().parent
KEY = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\MyCodexDesktopTools"
BRAND = "WebCodex Desktop（工具增强版）"


def known_folder(csidl: int) -> Path:
    buffer = ctypes.create_unicode_buffer(32768)
    if ctypes.windll.shell32.SHGetFolderPathW(None, csidl, None, 0, buffer):
        raise RuntimeError("Could not resolve Windows known folder")
    return Path(buffer.value)


def uninstall_record() -> dict[str, str] | None:
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, KEY, 0, winreg.KEY_READ | winreg.KEY_WOW64_64KEY) as key:
            return {name: winreg.QueryValueEx(key, name)[0] for name in ("InstallLocation", "DisplayName", "UninstallString")}
    except FileNotFoundError:
        return None


def run(executable: Path, *args: str) -> int:
    # NSIS requires /D= and _?= to be the last argument, unquoted even when the
    # directory contains spaces. Pass one Windows command line without a shell.
    if args and args[-1].startswith(("/D=", "_?=")):
        command = subprocess.list2cmdline([str(executable), *args[:-1]]) + " " + args[-1]
    else:
        command = subprocess.list2cmdline([str(executable), *args])
    return subprocess.run(command, creationflags=subprocess.CREATE_NO_WINDOW, timeout=180).returncode


def wait_absent(path: Path) -> None:
    deadline = time.monotonic() + 30
    while path.exists() and time.monotonic() < deadline:
        time.sleep(0.1)
    if path.exists():
        raise AssertionError(f"Owned installed file still exists: {path}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("installer", type=Path, help="Installer to execute (changes HKCU and shortcuts)")
    parser.add_argument("--manifest", type=Path, default=BUILD / "payload-manifest.json")
    parser.add_argument("--work-dir", type=Path, default=BUILD,
                        help="Existing parent directory for the temporary installation")
    parser.add_argument("--report", type=Path, default=BUILD / "installer-smoke.json")
    args = parser.parse_args()
    INSTALLER = args.installer.resolve(strict=True)
    work_dir = args.work_dir.resolve(strict=True)
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))["files"]
    desktop_link = known_folder(0x10) / (BRAND + ".lnk")
    menu = known_folder(0x2) / BRAND
    if uninstall_record() is not None or desktop_link.exists() or menu.exists():
        raise SystemExit("Refusing smoke: dedicated registration/shortcut already exists")
    install_dir = work_dir / ("smoke-install-" + uuid.uuid4().hex + " 中文路径")
    if install_dir.exists():
        raise SystemExit("Refusing smoke: target already exists")
    results: dict[str, object] = {"install_dir": str(install_dir), "checks": []}
    install_exit = run(INSTALLER, "/S", "/D=" + str(install_dir))
    assert install_exit == 0, (install_exit, str(install_dir))
    record = uninstall_record()
    assert record is not None and Path(record["InstallLocation"]) == install_dir
    assert record["DisplayName"] == BRAND
    assert desktop_link.is_file() and (menu / "WebCodex.lnk").is_file() and (menu / "卸载.lnk").is_file()
    for item in manifest:
        data = (install_dir / item["path"]).read_bytes()
        assert len(data) == item["size"] and hashlib.sha256(data).hexdigest() == item["sha256"], item["path"]
    results["checks"].append(f"silent install, registration, shortcuts, {len(manifest)} exact file hashes")

    # Re-running setup cannot overwrite an installed or unrelated directory.
    assert run(INSTALLER, "/S", "/D=" + str(install_dir)) != 0
    results["checks"].append("existing installation rejected")

    # An exclusive handle simulates a program still using an installed file.
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateFileW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32,
                                   ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p]
    kernel32.CreateFileW.restype = ctypes.c_void_p
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    handle = kernel32.CreateFileW(str(install_dir / "LICENSE"), 0x80000000, 0, None, 3, 0, None)
    assert handle not in (None, ctypes.c_void_p(-1).value)
    try:
        # _?= runs the uninstaller in place, exposing its own failure exit code.
        assert run(install_dir / "Uninstall.exe", "/S", "_?=" + str(install_dir)) != 0
        assert uninstall_record() is not None
        assert all((install_dir / item["path"]).is_file() for item in manifest)
    finally:
        kernel32.CloseHandle(handle)
    results["checks"].append("occupied file rejected before any payload deletion")

    keep_root = install_dir / "smoke-user-document.txt"
    keep_nested = install_dir / "mycodex-tools" / "smoke-user-document.txt"
    keep_root.write_text("keep root document", encoding="utf-8")
    keep_nested.write_text("keep nested document", encoding="utf-8")
    assert run(install_dir / "Uninstall.exe", "/S") == 0
    wait_absent(install_dir / "Uninstall.exe")
    assert uninstall_record() is None and not desktop_link.exists() and not menu.exists()
    assert keep_root.read_text(encoding="utf-8") == "keep root document"
    assert keep_nested.read_text(encoding="utf-8") == "keep nested document"
    for item in manifest:
        assert not (install_dir / item["path"]).exists(), item["path"]
    results["checks"].append("silent uninstall removed owned files/registration/shortcuts and preserved new documents")

    # With registration gone, an unrelated existing directory must also be rejected.
    assert run(INSTALLER, "/S", "/D=" + str(install_dir)) != 0
    assert keep_root.read_text(encoding="utf-8") == "keep root document"
    assert uninstall_record() is None
    results["checks"].append("unrelated existing directory rejected without modification")
    # Delete only exact files created by this smoke; remove directories only if empty.
    keep_nested.unlink()
    keep_root.unlink()
    (install_dir / "mycodex-tools").rmdir()
    install_dir.rmdir()
    results["installer_sha256"] = hashlib.sha256(INSTALLER.read_bytes()).hexdigest()
    results["ok"] = True
    args.report.write_text(json.dumps(results, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(results, ensure_ascii=False))


if __name__ == "__main__":
    main()
