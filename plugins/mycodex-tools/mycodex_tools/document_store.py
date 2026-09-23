"""Project-bound snapshots, recoverable commits and durable operation receipts.

The private directory is not a general model-visible filesystem API. Every path
in it is constructed here from a validated request id or backup id.
"""
from __future__ import annotations

from contextlib import contextmanager
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import time
import uuid

from .common import Context, MAX_FILE_BYTES, ToolError, encode

OFFICE_EXTENSIONS = {".doc", ".docx", ".docm", ".xls", ".xlsx", ".xlsm", ".ppt", ".pptx", ".pptm"}
DOCUMENT_EXTENSIONS = OFFICE_EXTENSIONS | {".pdf", ".png", ".jpg", ".jpeg", ".webp"}
_IDENTIFIER = re.compile(r"[a-f0-9]{32}\Z")


def identifier(value):
    if not isinstance(value, str) or not _IDENTIFIER.fullmatch(value):
        raise ToolError("invalid_identifier", "Expected a 32-character lowercase hexadecimal operation identifier.")
    return value


def digest_file(path):
    digest = hashlib.sha256()
    total = 0
    with Path(path).open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            total += len(chunk)
            if total > MAX_FILE_BYTES:
                raise ToolError("file_too_large", "Document exceeds 64 MiB.")
            digest.update(chunk)
    return digest.hexdigest()


def _regular(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
        raise ToolError("path_rejected", "Expected an ordinary file without reparse points.")


def _check_office_owner_file(path):
    if path.suffix.lower() not in OFFICE_EXTENSIONS:
        return
    # Office may release the original file handle while retaining an unsaved
    # in-memory document. Its owner file still marks that document as in use.
    candidates = {path.with_name("~$" + path.name), path.with_name("~$" + path.name[2:])}
    if any(candidate.exists() for candidate in candidates):
        raise ToolError("file_busy", "Office still has this document open; save and close it before replacing the original.")


def atomic_json(path, value):
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex + ".tmp")
    try:
        with temporary.open("xb") as stream:
            stream.write(encode(value))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def _atomic_replace(source, destination):
    if os.name != "nt":
        os.replace(source, destination)
        return
    # Modern Windows POSIX rename semantics permit replacing the destination
    # while our read/deny-write guard remains open on its old file identity.
    # MoveFileEx (os.replace) instead fails with ERROR_ACCESS_DENIED here.
    from ctypes import wintypes
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                  ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    kernel.CreateFileW.restype = wintypes.HANDLE
    kernel.SetFileInformationByHandle.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    name = str(destination.resolve()).encode("utf-16-le")
    class RenameInfo(ctypes.Structure):
        _fields_ = [("Flags", wintypes.DWORD), ("RootDirectory", wintypes.HANDLE),
                    ("FileNameLength", wintypes.DWORD), ("FileName", ctypes.c_ubyte * (len(name) + 2))]
    info = RenameInfo()
    info.Flags = 1 | 2  # REPLACE_IF_EXISTS | POSIX_SEMANTICS
    info.FileNameLength = len(name)
    ctypes.memmove(ctypes.addressof(info) + RenameInfo.FileName.offset, name, len(name))
    handle = kernel.CreateFileW(str(source), 0x00010000, 1 | 2 | 4, None, 3, 0x80, None)
    if handle == wintypes.HANDLE(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        if not kernel.SetFileInformationByHandle(handle, 22, ctypes.byref(info), ctypes.sizeof(info)):
            raise ctypes.WinError(ctypes.get_last_error())
    finally:
        kernel.CloseHandle(handle)


@contextmanager
def _source_commit_guard(path):
    """Deny concurrent Windows writers while hashing and publishing the source.

    Delete sharing permits the atomic replacement of the locked old file. This
    is an ordinary desktop concurrency guard, not a hostile-process sandbox.
    """
    if os.name == "nt":
        from ctypes import wintypes
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                      ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
        kernel.CreateFileW.restype = wintypes.HANDLE
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel.CreateFileW(str(path), 0x80000000, 1 | 4, None, 3, 0x80, None)
        if handle == wintypes.HANDLE(-1).value:
            raise ToolError("file_busy", "The document is open for writing or cannot be locked; close it and retry.")
        try:
            yield
        finally:
            kernel.CloseHandle(handle)
    else:
        import fcntl
        with path.open("rb") as stream:
            try:
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise ToolError("file_busy", "Another document operation owns this file.") from None
            yield


class DocumentStore:
    def __init__(self, ctx: Context, request_id=None):
        self.ctx = ctx
        self.request_id = identifier(request_id or uuid.uuid4().hex)
        self.private = self._directory(ctx.root / ".webcodex-office")
        self.cache = self._directory(self.private / "cache")
        self.backups = self._directory(self.private / "backups")
        self.receipts = self._directory(self.private / "receipts")
        self.work = self._directory(self.cache / self.request_id)
        self._snapshot_bytes = 0

    def _directory(self, path):
        # Walk existing ancestors before creating anything; junctions are not
        # allowed even when their apparent path lies inside the project.
        relative = path.relative_to(self.ctx.root)
        current = self.ctx.root
        for part in relative.parts:
            current = current / part
            if not current.exists():
                current.mkdir()
            info = current.lstat()
            if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
                raise ToolError("path_rejected", "Document state directories cannot be links or reparse points.")
        return path

    def source_path(self, relative):
        if not isinstance(relative, str):
            raise ToolError("invalid_path", "Use a project-relative document path.")
        extension = Path(relative).suffix.lower()
        if extension not in DOCUMENT_EXTENSIONS:
            raise ToolError("unsupported_format", "Unsupported document format.")
        return self.ctx.resolve_path(relative, extension)

    def snapshot(self, relative, expected_sha256=None, name="source"):
        source = self.source_path(relative)
        _regular(source)
        content, digest = self.ctx.read_bytes(relative, source.suffix.lower(), expected_sha256)
        self._snapshot_bytes += len(content)
        if self._snapshot_bytes > 256 * 1024 * 1024:
            raise ToolError("request_too_large", "Combined source documents exceed the 256 MiB request budget.")
        snapshot = self.work / (name + source.suffix.lower())
        # A repeated operation id must never replace its original snapshot.
        if snapshot.exists():
            _regular(snapshot)
            if digest_file(snapshot) != digest:
                raise ToolError("request_conflict", "Operation id already belongs to a different document version.")
        else:
            with snapshot.open("xb") as stream:
                stream.write(content)
                stream.flush()
                os.fsync(stream.fileno())
        return source, snapshot, digest

    def check_source(self, relative, expected):
        source = self.source_path(relative)
        _regular(source)
        if digest_file(source) != expected:
            raise ToolError("stale_source", "Source changed; read the current document before editing.")
        return source

    def receipt(self, fingerprint):
        path = self.receipts / (self.request_id + ".json")
        if not path.exists():
            return None
        _regular(path)
        record = json.loads(path.read_text(encoding="utf-8"))
        if record.get("fingerprint") != fingerprint:
            raise ToolError("request_conflict", "Operation id is already bound to another request.")
        if record.get("state") == "completed":
            return {**record["result"], "replayed_receipt": True}
        # A crash after atomic replace but before completing the receipt can be
        # resolved without executing the edit a second time.
        target = self.source_path(record["target"])
        if record.get("state") == "prepared" and target.exists() and digest_file(target) == record.get("after_sha256"):
            result = record["result"]
            atomic_json(path, {**record, "state": "completed"})
            return {**result, "reconciled_receipt": True}
        raise ToolError("outcome_unknown", "An earlier attempt exists. Inspect its target and backup before starting another edit.")

    @contextmanager
    def operation_lock(self, source):
        # msvcrt locks are released by process death; stale lock files are safe.
        lock_id = hashlib.sha256(str(source).casefold().encode("utf-8")).hexdigest()
        lock_dir = self._directory(self.private / "locks")
        path = lock_dir / (lock_id + ".lock")
        if path.exists():
            _regular(path)
        stream = path.open("a+b")
        try:
            if stream.tell() == 0:
                stream.write(b"\0")
                stream.flush()
            stream.seek(0)
            if os.name == "nt":
                import msvcrt
                try:
                    msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
                except OSError:
                    raise ToolError("file_busy", "Another document operation is already in progress.") from None
            else:
                import fcntl
                try:
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    raise ToolError("file_busy", "Another document operation is already in progress.") from None
            yield
        finally:
            stream.close()

    def publish(self, relative, before_sha256, staged, fingerprint, changes, save_mode="in_place", output_path=None):
        source = self.check_source(relative, before_sha256)
        staged = Path(staged)
        _regular(staged)
        after_sha256 = digest_file(staged)
        self.ctx.validate_file_bytes(staged.read_bytes(), staged.suffix.lower())
        if save_mode not in ("in_place", "copy"):
            raise ToolError("invalid_argument", "save_mode must be in_place or copy.")
        if save_mode == "copy" and not output_path:
            raise ToolError("invalid_argument", "Copy mode requires a new output_path.")
        target = self.source_path(output_path) if save_mode == "copy" else source
        if not target.parent.is_dir():
            raise ToolError("parent_missing", "The output directory must already exist.")
        if target.suffix.lower() != staged.suffix.lower():
            raise ToolError("format_mismatch", "The output extension must match the saved file format.")
        if save_mode == "copy" and target.exists():
            raise ToolError("target_exists", "The output already exists; choose a new name.")
        # Place final staged bytes on the target filesystem before commit.
        publishing = target.with_name(".mycodex-" + self.request_id + target.suffix)
        if publishing.exists():
            raise ToolError("outcome_unknown", "A prior publication file exists; inspect the operation receipt.")
        committed = False
        try:
            with staged.open("rb") as src, publishing.open("xb") as dst:
                shutil.copyfileobj(src, dst, 1024 * 1024)
                dst.flush()
                os.fsync(dst.fileno())
            with self.operation_lock(source), _source_commit_guard(source):
                self.check_source(relative, before_sha256)
                if save_mode == "in_place":
                    _check_office_owner_file(source)
                backup_id = None
                backup_path = None
                if save_mode == "in_place":
                    backup_id = self.request_id
                    backup_path = self.backups / (backup_id + source.suffix.lower())
                    with source.open("rb") as src, backup_path.open("xb") as dst:
                        shutil.copyfileobj(src, dst, 1024 * 1024)
                        dst.flush()
                        os.fsync(dst.fileno())
                    if digest_file(backup_path) != before_sha256:
                        raise ToolError("backup_failed", "Backup verification failed; the original was not changed.")
                    atomic_json(self.backups / (backup_id + ".json"), {
                        "backup_id": backup_id, "path": relative, "sha256": before_sha256,
                        "extension": source.suffix.lower(), "created_at": int(time.time()),
                    })
                result = {
                    "path": target.relative_to(self.ctx.root).as_posix(), "source_path": relative,
                    "before_sha256": before_sha256, "after_sha256": after_sha256,
                    "source_sha256": before_sha256, "sha256": after_sha256,
                    "backup_id": backup_id, "backup_path": backup_path.relative_to(self.ctx.root).as_posix() if backup_path else None,
                    "save_mode": save_mode, "changes": changes, "verified": True,
                    "request_id": self.request_id,
                }
                record = {"fingerprint": fingerprint, "state": "prepared", "target": result["path"],
                          "after_sha256": after_sha256, "result": result}
                receipt_path = self.receipts / (self.request_id + ".json")
                atomic_json(receipt_path, record)
                # Recheck immediately before publication, while denying writers.
                self.check_source(relative, before_sha256)
                if save_mode == "in_place":
                    _atomic_replace(publishing, target)
                else:
                    os.link(publishing, target)  # Atomic create-if-absent; never overwrite.
                committed = True
                if digest_file(target) != after_sha256:
                    raise ToolError("outcome_unknown", "Published file changed before final verification; inspect the receipt.")
                atomic_json(receipt_path, {**record, "state": "completed"})
                return result
        except Exception as error:
            if committed:
                raise ToolError("outcome_unknown", "Publication completed but final verification or receipt failed; reconcile the saved operation before retrying.") from None
            if isinstance(error, PermissionError):
                raise ToolError("file_busy", "The document is locked or not writable; the original and backup remain available.") from None
            if isinstance(error, FileExistsError):
                raise ToolError("target_exists", "A target or backup already exists; inspect the prior operation before retrying.") from None
            raise
        finally:
            try:
                publishing.unlink(missing_ok=True)
            except OSError:
                # The committed receipt owns the outcome. Cleanup failure must
                # not turn a successful edit into an apparent failed mutation.
                pass

    def list_backups(self, relative):
        self.source_path(relative)
        records = []
        for path in self.backups.glob("*.json"):
            _regular(path)
            record = json.loads(path.read_text(encoding="utf-8"))
            if record.get("path") == relative:
                records.append(record)
        records.sort(key=lambda item: (item["created_at"], item["backup_id"]), reverse=True)
        return records

    def backup_snapshot(self, relative, backup_id):
        backup_id = identifier(backup_id)
        metadata = self.backups / (backup_id + ".json")
        if not metadata.exists():
            raise ToolError("backup_not_found", "Backup does not exist.")
        _regular(metadata)
        record = json.loads(metadata.read_text(encoding="utf-8"))
        if record.get("path") != relative or record.get("extension") != Path(relative).suffix.lower():
            raise ToolError("backup_mismatch", "Backup belongs to another document.")
        source = self.backups / (backup_id + record["extension"])
        _regular(source)
        if digest_file(source) != record["sha256"]:
            raise ToolError("backup_corrupt", "Backup checksum does not match its record.")
        return source
