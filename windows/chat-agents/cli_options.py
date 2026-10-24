"""Bounded installed-CLI help discovery; never run a model task or read settings."""
from __future__ import annotations

import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import threading


CLI_NAMES = frozenset({"codebuddy", "gemini", "codex"})
ROOT = Path(__file__).resolve().parent
FIXED_EXECUTABLES = {
    "codebuddy": Path(os.environ.get("WEBCODEX_CODEBUDDY", "") or str(
        Path(os.environ.get("LOCALAPPDATA", str(Path.home() / "AppData" / "Local"))) /
        "codebuddy" / "bin" / "codebuddy.exe")),
    "codex": Path(os.environ.get("WEBCODEX_CODEX", "") or str(
        Path(os.environ.get("APPDATA", str(Path.home() / "AppData" / "Roaming"))) /
        "npm" / "node_modules" / "@openai" / "codex" / "node_modules" /
        "@openai" / "codex-win32-x64" / "vendor" / "x86_64-pc-windows-msvc" / "bin" / "codex.exe")),
}
MAX_OUTPUT_BYTES = 65536
COMMAND_TIMEOUT_SECONDS = 10
SAFE_ENVIRONMENT = frozenset({
    "SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "TEMP", "TMP",
    "USERPROFILE", "LOCALAPPDATA", "APPDATA", "PROGRAMDATA", "PROGRAMFILES",
    "PROGRAMFILES(X86)", "PROGRAMW6432", "SYSTEMDRIVE", "HOME", "HOMEDRIVE",
    "HOMEPATH", "LANG", "LC_ALL", "LC_CTYPE",
})


class DiscoveryError(Exception):
    """Only fixed error codes leave the subprocess boundary."""


def _resolve(cli_name):
    fixed = FIXED_EXECUTABLES.get(cli_name)
    if fixed is not None and fixed.is_file():
        return fixed
    located = shutil.which(cli_name)
    return Path(located).resolve() if located else None


def _stop(process):
    try:
        if os.name == "nt":
            process.kill()
        else:
            os.killpg(process.pid, signal.SIGKILL)
    except (OSError, ProcessLookupError):
        pass
    try:
        process.wait(timeout=2)
    except (OSError, subprocess.TimeoutExpired):
        pass


def _capture(executable, flag):
    if flag not in {"--version", "--help"}:
        raise DiscoveryError("unsupported_probe")
    env = {key: value for key, value in os.environ.items() if key.upper() in SAFE_ENVIRONMENT}
    env.update(NO_COLOR="1", CI="1")
    options = {"stdin": subprocess.DEVNULL, "stdout": subprocess.PIPE, "stderr": subprocess.STDOUT,
               "shell": False, "env": env, "cwd": str(ROOT), "bufsize": 0}
    if os.name == "nt":
        options["creationflags"] = subprocess.CREATE_NO_WINDOW
    else:
        options["start_new_session"] = True
    try:
        process = subprocess.Popen([str(executable), flag], **options)
    except OSError:
        raise DiscoveryError("process_start_failed") from None
    output = bytearray()
    read_error = []

    def read_output():
        try:
            while True:
                chunk = process.stdout.read(4096)
                if not chunk:
                    return
                if len(output) + len(chunk) > MAX_OUTPUT_BYTES:
                    read_error.append("output_limit")
                    _stop(process)
                    return
                output.extend(chunk)
        except Exception:
            read_error.append("output_read_failed")

    reader = threading.Thread(target=read_output, name="cli-help-reader", daemon=True)
    reader.start()
    try:
        try:
            exit_code = process.wait(timeout=COMMAND_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            _stop(process)
            raise DiscoveryError("process_timeout") from None
        reader.join(timeout=2)
        if reader.is_alive():
            _stop(process)
            raise DiscoveryError("output_read_timeout")
        if read_error:
            raise DiscoveryError(read_error[0])
        if exit_code != 0:
            raise DiscoveryError("process_failed")
        try:
            return output.decode("utf-8-sig")
        except UnicodeError:
            raise DiscoveryError("output_encoding_unknown") from None
    finally:
        if process.poll() is None:
            _stop(process)
        if not reader.is_alive():
            process.stdout.close()


def _option(help_text, name):
    """Read one declared long option and its continuation lines, never neighboring options."""
    options = list(re.finditer(r"(?m)^\s*(?:-[A-Za-z],\s*)?(--[A-Za-z][A-Za-z0-9-]*)\s+", help_text))
    matching = [(index, match) for index, match in enumerate(options) if match.group(1) == name]
    if len(matching) != 1:
        return None
    index, match = matching[0]
    end = options[index + 1].start() if index + 1 < len(options) else len(help_text)
    section = help_text[match.end():end]
    return section if re.match(r"<[^>\r\n]+>", section) else None


def _choices(text, pattern):
    match = re.search(pattern, text or "", re.IGNORECASE | re.DOTALL)
    if match is None:
        return None
    values = [item.strip() for item in match.group(1).split(",")]
    if not 1 <= len(values) <= 128 or any(not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}", item) for item in values):
        return None
    if len(values) != len(set(values)):
        return None
    return values


def _version(text):
    match = re.fullmatch(r"\s*(?:(?:codebuddy(?: code)?|codex(?:-cli)?|gemini(?: cli)?)\s+)?v?(\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?)\s*", text, re.IGNORECASE)
    return match.group(1) if match else None


def discover_cli_options(cli_name):
    """Return local declarations, not account entitlement or an execution decision."""
    if not isinstance(cli_name, str) or cli_name not in CLI_NAMES:
        raise ValueError("unsupported_cli")
    result = {"cli_name": cli_name, "installed": False, "source": "installed_cli_help",
              "account_availability_verified": False, "selection_required": True,
              "discovery_incomplete": True, "status": "unavailable", "version": None,
              "models": [], "reasoning_efforts": [], "model_argument": None,
              "reasoning_argument": None}
    executable = _resolve(cli_name)
    if executable is None:
        return dict(result, reason="cli_not_installed")
    result["installed"] = True
    # Batch/PowerShell launchers are not passed to a shell. A verified native
    # Codex executable is preferred above; unknown wrappers stay undispatched.
    if os.name == "nt" and executable.suffix.lower() != ".exe":
        return dict(result, status="unknown", reason="unsupported_cli_launcher")
    try:
        version_text = _capture(executable, "--version")
        help_text = _capture(executable, "--help")
    except DiscoveryError as error:
        return dict(result, status="unknown", reason=str(error))
    result["version"] = _version(version_text)
    model_section = _option(help_text, "--model")
    if model_section is not None:
        result["model_argument"] = "--model"
    if cli_name != "codebuddy":
        reason = "official_model_catalog_required" if cli_name == "codex" else "cli_capability_catalog_required"
        return dict(result, status="incomplete", reason=reason)
    effort_section = _option(help_text, "--effort")
    if effort_section is not None:
        result["reasoning_argument"] = "--effort"
    models = _choices(model_section, r"Currently supported:\s*\(([^)]+)\)")
    efforts = _choices(effort_section, r"Reasoning effort level\s*\(([^)]+)\)")
    if models is not None:
        result["models"] = models
    if efforts is not None:
        result["reasoning_efforts"] = efforts
    if models is None or efforts is None or result["version"] is None:
        return dict(result, status="unknown", reason="help_format_unknown")
    return dict(result, status="complete", discovery_incomplete=False)
