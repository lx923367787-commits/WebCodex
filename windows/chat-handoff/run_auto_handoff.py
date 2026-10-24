"""Production launcher: Desktop credentials stay in memory, never in child args."""
from __future__ import annotations

import argparse
from collections import Counter
import contextlib
import ctypes
from ctypes import wintypes
import json
import logging
from logging.handlers import RotatingFileHandler
import os
from pathlib import Path
import sqlite3
import sys
import time
import uuid

import auto_handoff as ah
import handoff_controller as hc

DEFAULT_STATE = Path(os.environ.get("WEBCODEX_DESKTOP_STATE", "") or
                     str(Path(os.environ.get("LOCALAPPDATA", str(Path.home() / "AppData" / "Local"))) /
                         "dev.webcodex.desktop" / "desktop-state.json"))


def read_json(path, limit):
    with Path(path).open("rb") as source:
        data = source.read(limit + 1)
    if len(data) > limit:
        raise ValueError("invalid_local_config")
    return json.loads(data.decode("utf-8-sig"))


def load_credentials(state_path):
    """Legacy Desktop loader for explicit migration/tests; not scheduler auth."""
    state_path = Path(state_path).resolve()
    raw = read_json(state_path, 262144)
    runtime = raw.get("runtime") if isinstance(raw, dict) else None
    if not isinstance(runtime, dict):
        raise ValueError("invalid_local_config")
    origin = hc.endpoint(runtime.get("server_url"))
    token_path = Path(runtime.get("user_token_file", ""))
    if not token_path.is_absolute():
        raise ValueError("invalid_local_config")
    token_path = token_path.resolve()
    # Desktop can write extended-length Windows paths. Normalize that prefix
    # only for path identity comparison; never interpret it as a URL.
    identity_path = str(token_path)
    if identity_path.startswith("\\\\?\\"):
        identity_path = identity_path[4:]
    identity_path = os.path.normcase(os.path.normpath(identity_path))
    root = os.path.normcase(str(state_path.parent.resolve()))
    if root.startswith("\\\\?\\"):
        root = root[4:]
    if os.path.commonpath((root, identity_path)) != root:
        raise ValueError("token_file_outside_desktop_state")
    with token_path.open("rb") as source:
        data = source.read(16385)
    if len(data) > 16384:
        raise ValueError("invalid_local_credential")
    token = data.decode("utf-8-sig").strip()
    if not token or any(char in token for char in ("\r", "\n", "\0")):
        raise ValueError("invalid_local_credential")
    return origin, token, identity_path


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex + ".tmp")
    try:
        with temporary.open("x", encoding="utf-8", newline="\n") as output:
            json.dump(value, output, ensure_ascii=False)
            output.flush()
            os.fsync(output.fileno())
        temporary.replace(path)
    finally:
        if temporary.exists():
            temporary.unlink()


def sidecar(db_path, name):
    db_path = Path(db_path).resolve()
    return db_path.with_name(db_path.stem + "." + name)


def bind_connection(db_path, origin, token_file):
    path = sidecar(db_path, "identity.json")
    identity = {"endpoint": origin, "token_file": token_file}
    if path.exists():
        if read_json(path, 16384) != identity:
            raise ValueError("desktop_connection_changed")
    else:
        write_json(path, identity)


def process_stamp(pid):
    """Process creation identity prevents a reused PID from looking alive."""
    if not isinstance(pid, int) or isinstance(pid, bool) or pid <= 0:
        return None
    if os.name != "nt":
        try:
            # Linux /proc stat has a parenthesized name that may contain spaces.
            fields = Path("/proc").joinpath(str(pid), "stat").read_text().rsplit(")", 1)[1].split()
            return fields[19]
        except (OSError, IndexError):
            return None
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.GetProcessTimes.argtypes = (wintypes.HANDLE,) + (ctypes.POINTER(wintypes.FILETIME),) * 4
    kernel.GetProcessTimes.restype = wintypes.BOOL
    kernel.CloseHandle.argtypes = (wintypes.HANDLE,)
    handle = kernel.OpenProcess(0x1000, False, pid)
    if not handle:
        return None
    try:
        created, exited, kernel_time, user_time = (wintypes.FILETIME() for _ in range(4))
        if not kernel.GetProcessTimes(handle, ctypes.byref(created), ctypes.byref(exited), ctypes.byref(kernel_time), ctypes.byref(user_time)):
            return None
        if exited.dwHighDateTime or exited.dwLowDateTime:
            return None
        return str((created.dwHighDateTime << 32) | created.dwLowDateTime)
    finally:
        kernel.CloseHandle(handle)


def summary(store):
    state = store.status()
    counts = Counter(row["status"] for row in store.entries())
    result = {"mode": state["mode"], "generation": state["generation"],
        "failures": state["failures"], "armed_at": state["armed_at"],
        "dispatch_counts": {key: counts[key] for key in sorted(counts)}}
    if counts["failed"]:
        result["handoff_failure"] = "交接失败，已停止执行，已保留可人工恢复的状态。只有用户明确要求恢复后才能重新启动。"
    return result


def status(db_path):
    result = {"mode": "not_initialized", "runner": {"active": False, "pid_alive": False}}
    if Path(db_path).is_file():
        store = ah.Store(db_path)
        try:
            result.update(summary(store))
        finally:
            store.close()
    path = sidecar(db_path, "runner.json")
    if path.is_file():
        raw = read_json(path, 16384)
        if isinstance(raw, dict):
            runner = {key: raw.get(key) for key in ("pid", "active", "started_at", "updated_at", "state")}
            stamp = process_stamp(runner["pid"])
            runner["pid_alive"] = bool(stamp and stamp == raw.get("process_stamp"))
            result["runner"] = runner
    return result


def make_logger(db_path):
    logger = logging.getLogger("auto_handoff_runner." + str(Path(db_path).resolve()))
    logger.setLevel(logging.INFO)
    logger.propagate = False
    for handler in list(logger.handlers):
        handler.close()
        logger.removeHandler(handler)
    path = sidecar(db_path, "log")
    path.parent.mkdir(parents=True, exist_ok=True)
    handler = RotatingFileHandler(path, maxBytes=131072, backupCount=2, encoding="utf-8")
    handler.setFormatter(logging.Formatter("%(asctime)s %(message)s"))
    logger.addHandler(handler)
    return logger


def log_event(logger, event, store=None):
    # Only constant event names and a bounded state summary enter the log.
    value = {"event": event}
    if store is not None:
        value.update(summary(store))
    logger.info(json.dumps(value, ensure_ascii=False, sort_keys=True))


def wait_ready(store, api, timeout=120, clock=time.monotonic, sleep=time.sleep):
    deadline = clock() + timeout
    while clock() < deadline:
        if store.status()["mode"] != "running":
            return False
        remaining = deadline - clock()
        if remaining <= 0:
            return False
        api.timeout = min(5, remaining)
        try:
            api.discover()
            return store.status()["mode"] == "running" and clock() < deadline
        except hc.ReadFailure:
            pass
        until = min(deadline, clock() + 2)
        while clock() < until:
            if store.status()["mode"] != "running":
                return False
            sleep(min(0.25, max(0, until - clock())))
    return False


@contextlib.contextmanager
def runner_record(db_path, store, wall_clock):
    value = {"pid": os.getpid(), "process_stamp": process_stamp(os.getpid()),
        "active": True, "started_at": int(wall_clock()), "updated_at": int(wall_clock()), "state": "waiting_server"}
    write_json(sidecar(db_path, "runner.json"), value)
    try:
        yield value
    finally:
        # This runs before releasing the desktop lock, so an exiting runner
        # cannot overwrite a successor process's new PID record.
        value.update(active=False, state=store.status()["mode"], updated_at=int(wall_clock()))
        write_json(sidecar(db_path, "runner.json"), value)


def run(db_path, state_path=DEFAULT_STATE, interval=5, api_factory=ah.Api,
        launcher=None, once=False, clock=time.monotonic, sleep=time.sleep, wall_clock=time.time,
        credentials_loader=None, connection_binder=bind_connection):
    if not 0.1 <= interval <= 60:
        raise ValueError("invalid_interval")
    db_path = Path(db_path).resolve()
    logger = make_logger(db_path)
    store = None
    try:
        if credentials_loader is None:
            from handoff_control_api import load_credentials as credentials_loader
        origin, token, token_file = credentials_loader()
        store = ah.Store(db_path)
        store.initialize(origin, armed_at=int(wall_clock()))
        # Explicit pause and STOP survive app and process restarts.
        if store.status()["mode"] != "running":
            log_event(logger, "persistent_stop_preserved", store)
            return 2
        lock_path = ah.ROOT / "runtime" / "desktop-dispatch"
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        with contextlib.ExitStack() as stack:
            stack.enter_context(hc.watcher_lock(lock_path))
            connection_binder(db_path, origin, token_file)
            api = api_factory(origin, token, timeout=5)
            del token
            runner = stack.enter_context(runner_record(db_path, store, wall_clock))
            log_event(logger, "waiting_server", store)
            if not wait_ready(store, api, timeout=120, clock=clock, sleep=sleep):
                log_event(logger, "server_wait_ended", store)
                return 2
            if launcher is None:
                from edge_adapter import launch
                launcher = launch
            store.recover_interrupted()
            last_log = None
            last_logged_at = float("-inf")
            while store.status()["mode"] == "running":
                runner.update(state="running", updated_at=int(wall_clock()))
                write_json(sidecar(db_path, "runner.json"), runner)
                ah.poll_once(store, api, launcher)
                current = summary(store)
                if current != last_log or clock() - last_logged_at >= 60:
                    log_event(logger, "state", store)
                    last_log, last_logged_at = current, clock()
                if once or store.status()["mode"] != "running":
                    break
                until = clock() + interval
                while clock() < until and store.status()["mode"] == "running":
                    sleep(min(0.25, max(0, until - clock())))
            return 0 if store.status()["mode"] == "running" else 2
    except KeyboardInterrupt:
        if store is not None:
            store.pause_if_running()
        log_event(logger, "interrupted", store)
        return 130
    except Exception:
        # Never include raw Desktop/API/adapter exception text or credentials.
        log_event(logger, "runner_failed", store)
        return 2
    finally:
        # The watcher owns its reusable browser connection for its lifetime.
        # Importing the façade and closing an unused pool do not connect Edge.
        from playwright_extension_browser import close_persistent_connection
        close_persistent_connection()
        if store is not None:
            store.close()
        for handler in list(logger.handlers):
            handler.close()
            logger.removeHandler(handler)


def emit(value):
    if sys.stdout is not None:
        print(json.dumps(value, ensure_ascii=False))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", nargs="?", default="run", choices=("run", "status", "stop", "pause", "resume"))
    parser.add_argument("--db", type=Path, default=ah.DEFAULT_DB)
    parser.add_argument("--desktop-state", type=Path, default=DEFAULT_STATE)
    parser.add_argument("--control-config", type=Path)
    parser.add_argument("--interval", type=float, default=5)
    parser.add_argument("--once", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.command == "run":
            if args.control_config is not None:
                from handoff_carrier import CarrierControl
                control = CarrierControl(args.control_config, args.desktop_state, args.db)
                return run(args.db, args.desktop_state, args.interval, once=args.once,
                           credentials_loader=control.load_credentials, api_factory=control.api_factory,
                           connection_binder=control.bind_connection, launcher=control.launch)
            return run(args.db, args.desktop_state, args.interval, once=args.once)
        if args.command != "status":
            if not args.db.is_file():
                raise ValueError("scheduler_not_initialized")
            store = ah.Store(args.db)
            try:
                store.set_mode({"stop": "stopped", "pause": "paused", "resume": "running"}[args.command])
            finally:
                store.close()
        emit(status(args.db))
        return 0
    except (ValueError, OSError, TypeError, sqlite3.Error):
        emit({"error": "runner_command_failed"})
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
