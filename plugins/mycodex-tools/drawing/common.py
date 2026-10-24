"""Small shared contracts for the standalone drawing scripts."""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import stat

from mycodex_tools.common import Context, ToolError


class DrawingError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


def safe_path(root: Path, relative: str, *, suffixes=None, must_exist=False) -> Path:
    if not isinstance(relative, str):
        raise DrawingError("invalid_path", "Use a project-relative path.")
    try:
        result = Context(root).resolve_path(relative, Path(relative).suffix)
    except (ToolError, OSError, ValueError) as error:
        raise DrawingError("invalid_path", "Path must remain inside the project and cannot contain protected components or links.") from error
    if suffixes is not None and result.suffix.lower() not in suffixes:
        raise DrawingError("unsupported_format", "The input file format is not supported for this drawing.")
    if must_exist:
        try:
            info = result.lstat()
        except FileNotFoundError:
            raise DrawingError("file_not_found", "Input file does not exist in this project.") from None
        if not stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
            raise DrawingError("invalid_path", "Input must be a regular file without links or reparse points.")
    return result


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def default_font(bundle: Path) -> Path:
    # Prefer a clear installed office font; keep the redistributable bundled
    # Chinese font as an explicit fallback, without modifying system fonts.
    if os.name == "nt":
        windows = Path(os.environ.get("WINDIR", "C:/Windows"))
        for name in ("msyh.ttc", "simhei.ttf", "simsun.ttc"):
            candidate = windows / "Fonts" / name
            if candidate.is_file():
                return candidate
    candidate = bundle / "fonts" / "ZCOOLXiaoWei-Regular.ttf"
    if candidate.is_file():
        return candidate
    raise DrawingError("missing_font", "Configure an installed Chinese font or provide the bundled Chinese font.")
