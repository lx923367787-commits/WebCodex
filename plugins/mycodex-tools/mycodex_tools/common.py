"""Native-plugin bounds and local file policy; this is not an OS sandbox."""
from __future__ import annotations

import hashlib
import io
import json
import math
import os
from pathlib import Path, PureWindowsPath
import stat
import tempfile
import zipfile

MAX_FILE_BYTES = 64 * 1024 * 1024
MAX_ZIP_BYTES = 256 * 1024 * 1024
MAX_FRAME_BYTES = 1024 * 1024
MAX_ARGUMENT_BYTES = 64 * 1024
MAX_RESULT_BYTES = 128 * 1024


class ToolError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


def obj(properties, required=()):
    return {"type": "object", "properties": properties, "required": list(required),
            "additionalProperties": False}


def arr(items, max_items=100):
    return {"type": "array", "items": items, "maxItems": max_items}


def string(description="", max_length=32768):
    return {"type": "string", "description": description, "maxLength": max_length}


def integer(description=""):
    return {"type": "integer", "description": description}


def number(description=""):
    return {"type": "number", "description": description}


def boolean(description=""):
    return {"type": "boolean", "description": description}


def encode(value):
    return json.dumps(value, ensure_ascii=False, allow_nan=False,
                      separators=(",", ":")).encode("utf-8")


def bounded_json(value, max_bytes, code="result_too_large"):
    """Match the existing native gateway's depth/node/string ceilings."""
    nodes = 0

    def walk(item, depth):
        nonlocal nodes
        nodes += 1
        if nodes > 4096 or depth > 16:
            raise ToolError(code, "JSON exceeds gateway bounds; select a smaller range.")
        if isinstance(item, str) and len(item.encode("utf-8")) > 65536:
            raise ToolError(code, "Text exceeds gateway bounds; use a smaller character range.")
        if isinstance(item, dict):
            for key, child in item.items():
                if not isinstance(key, str):
                    raise ToolError("invalid_result", "JSON object keys must be strings.")
                walk(key, depth + 1)
                walk(child, depth + 1)
        elif isinstance(item, list):
            for child in item:
                walk(child, depth + 1)

    walk(value, 0)
    try:
        encoded = encode(value)
    except (TypeError, ValueError):
        raise ToolError("invalid_result", "Result contains an unsupported JSON value.") from None
    if len(encoded) > max_bytes:
        raise ToolError(code, "Result exceeds gateway bounds; select a smaller range.")
    return encoded


def validate_args(value, schema, location="arguments"):
    kind = schema["type"]
    valid = {
        "object": isinstance(value, dict), "array": isinstance(value, list),
        "string": isinstance(value, str), "integer": type(value) is int,
        "number": type(value) in (int, float) and math.isfinite(value),
        "boolean": type(value) is bool, "null": value is None,
    }[kind]
    if not valid:
        raise ToolError("invalid_arguments", f"{location} must have type {kind}.")
    if "enum" in schema and value not in schema["enum"]:
        raise ToolError("invalid_arguments", f"{location} has an unsupported value.")
    if "const" in schema and value != schema["const"]:
        raise ToolError("invalid_arguments", f"{location} has an unsupported value.")
    if kind == "object":
        props = schema.get("properties", {})
        if any(key not in value for key in schema.get("required", [])):
            raise ToolError("invalid_arguments", f"{location} is missing required fields.")
        if schema.get("additionalProperties") is False and value.keys() - props.keys():
            raise ToolError("invalid_arguments", f"{location} has unknown fields.")
        for key, child in value.items():
            if key in props:
                validate_args(child, props[key], f"{location}.{key}")
    elif kind == "array":
        if not schema.get("minItems", 0) <= len(value) <= schema.get("maxItems", 4096):
            raise ToolError("invalid_arguments", f"{location} has too many or too few items.")
        for child in value:
            validate_args(child, schema["items"], f"{location}[]")
    elif kind == "string":
        if not schema.get("minLength", 0) <= len(value) <= schema.get("maxLength", 65536):
            raise ToolError("invalid_arguments", f"{location} has an invalid length.")


class Context:
    def __init__(self, root: Path, font_path=None):
        self.root = Path(root).resolve(strict=True)
        if not self.root.is_dir():
            raise ValueError("Plugin cwd must be an existing directory")
        self.font_path = Path(font_path) if font_path else None

    def resolve_path(self, value: str, extension: str):
        if not isinstance(value, str) or not value or len(value) > 4096:
            raise ToolError("invalid_path", "Use a nonempty project-relative file path.")
        win = PureWindowsPath(value)
        if win.drive or win.root or value.startswith("/"):
            raise ToolError("invalid_path", "Absolute paths are not allowed.")
        parts = value.replace("\\", "/").split("/")
        reserved = {"con", "prn", "aux", "nul", "conin$", "conout$"}
        reserved |= {f"{prefix}{i}" for prefix in ("com", "lpt") for i in range(1, 10)}
        protected = {".git", ".ssh", ".aws", ".azure", ".gnupg", ".local",
                     "node_modules", "target", "credentials", "secrets", "tokens",
                     "project-registry", "projects.d", ".webcodex-office"}
        for part in parts:
            lower = part.casefold()
            if (part in ("", ".", "..") or part.endswith((" ", "."))
                    or any(ord(c) < 32 or c in ':<>"|?*' for c in part)
                    or lower in protected
                    or lower.startswith((".env", "runner.toml", "agent.toml", "webcodex.env"))
                    or lower.endswith((".pem", ".key", ".env", ".toml.bak"))
                    or lower.split(".")[0] in reserved):
                raise ToolError("path_rejected", "Path contains a forbidden or protected component.")
        candidate = self.root.joinpath(*parts)
        if candidate.suffix.casefold() != extension.casefold():
            raise ToolError("unsupported_format", f"This tool requires {extension} files.")
        current = self.root
        for part in parts:
            current = current / part
            try:
                info = current.lstat()
            except FileNotFoundError:
                continue
            if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
                raise ToolError("path_rejected", "Symlinks and reparse points are not allowed.")
        resolved = candidate.resolve(strict=False)
        try:
            resolved.relative_to(self.root)
        except ValueError:
            raise ToolError("path_rejected", "Path leaves the configured plugin root.") from None
        return candidate

    def read_bytes(self, path: str, extension: str, expected_sha256=None):
        source = self.resolve_path(path, extension)
        try:
            if not stat.S_ISREG(source.stat().st_mode):
                raise ToolError("invalid_file", "Input must be an ordinary file.")
            with source.open("rb") as stream:
                content = stream.read(MAX_FILE_BYTES + 1)
        except FileNotFoundError:
            raise ToolError("file_not_found", "Input file does not exist.") from None
        if len(content) > MAX_FILE_BYTES:
            raise ToolError("file_too_large", "Input exceeds 64 MiB.")
        digest = hashlib.sha256(content).hexdigest()
        if expected_sha256 is not None and expected_sha256 != digest:
            raise ToolError("stale_source", "Source changed; read it again before updating.")
        self.validate_file_bytes(content, extension)
        return content, digest

    @staticmethod
    def validate_file_bytes(content, extension):
        if extension in (".docx", ".xlsx", ".pptx", ".docm", ".xlsm", ".pptm"):
            try:
                with zipfile.ZipFile(io.BytesIO(content)) as archive:
                    entries = archive.infolist()
                    if len(entries) > 10000 or sum(item.file_size for item in entries) > MAX_ZIP_BYTES:
                        raise ToolError("archive_too_large", "Office archive exceeds expansion bounds.")
                    if any(item.flag_bits & 1 for item in entries):
                        raise ToolError("unsupported_format", "Encrypted Office archives are unsupported.")
                    if "[Content_Types].xml" not in archive.namelist():
                        raise ToolError("invalid_file", "Input is not an Office Open XML document.")
                    # Do not trust only ZIP directory sizes. Bound bytes actually
                    # decompressed, and validate CRCs before any Office process opens it.
                    expanded = 0
                    for item in entries:
                        with archive.open(item) as stream:
                            while chunk := stream.read(1024 * 1024):
                                expanded += len(chunk)
                                if expanded > MAX_ZIP_BYTES:
                                    raise ToolError("archive_too_large", "Office archive exceeds expansion bounds.")
            except zipfile.BadZipFile:
                raise ToolError("invalid_file", "Office archive is damaged or invalid.") from None

    def publish(self, output_path, extension, writer, verifier):
        target = self.resolve_path(output_path, extension)
        if target.exists():
            raise ToolError("target_exists", "Output already exists; choose a new output_path.")
        if not target.parent.is_dir():
            raise ToolError("parent_missing", "Create the output directory with existing file tools first.")
        descriptor, name = tempfile.mkstemp(prefix=".mycodex-", suffix=extension, dir=target.parent)
        os.close(descriptor)
        temporary = Path(name)
        try:
            writer(temporary)
            if temporary.stat().st_size > MAX_FILE_BYTES:
                raise ToolError("file_too_large", "Generated artifact exceeds 64 MiB.")
            self.validate_file_bytes(temporary.read_bytes(), extension)
            verifier(temporary)
            with temporary.open("r+b") as stream:
                os.fsync(stream.fileno())
            self.resolve_path(output_path, extension)
            try:
                # A same-directory hard link atomically claims an absent name on NTFS/POSIX.
                # Never fall back to replace(), which could overwrite a concurrent writer.
                os.link(temporary, target)
            except FileExistsError:
                raise ToolError("target_exists", "Output was created concurrently; choose another path.") from None
            except OSError:
                raise ToolError("publish_failed", "Filesystem cannot publish atomically without overwriting.") from None
            return {"path": target.relative_to(self.root).as_posix(), "verified": True}
        finally:
            try:
                temporary.unlink(missing_ok=True)
            except OSError:
                # Publication may already have succeeded. Do not misreport it as a failed write.
                pass
