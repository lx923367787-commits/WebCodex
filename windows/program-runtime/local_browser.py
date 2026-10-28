"""Fixed browser actions for a Runner-attested local program service.

The Runner supplies the browser binary, origin and private artifact directory.
Model data supplies bounded actions only, never Python, browser arguments,
filesystem paths, JavaScript or an arbitrary origin. No user profile is reused.
This helper is not an execution-authority boundary: its caller must validate the
live owning Job and cross a persistent one-shot dispatch fence before invoking it.
"""
from __future__ import annotations

import json
import time
from pathlib import Path
from urllib.parse import urlsplit

MAX_STEPS = 40
MAX_TEXT = 16384


def local_origin(value):
    parsed = urlsplit(value)
    if (parsed.scheme != "http" or parsed.hostname != "127.0.0.1"
            or parsed.username is not None or parsed.password is not None
            or parsed.port is None or not 1024 <= parsed.port <= 65535
            or value != f"http://127.0.0.1:{parsed.port}"):
        raise ValueError("browser_exact_local_origin_required")
    return value


def allowed_url(url, origin):
    try:
        parsed = urlsplit(url)
        return (parsed.scheme == "http" and parsed.hostname == "127.0.0.1"
                and parsed.port == urlsplit(origin).port
                and parsed.username is None and parsed.password is None)
    except ValueError:
        return False


def validate_steps(steps):
    if not isinstance(steps, list) or not 1 <= len(steps) <= MAX_STEPS:
        raise ValueError("browser_steps_invalid")
    fields = {"navigate": {"path"}, "click": {"selector"},
              "fill": {"selector", "text"}, "select": {"selector", "value"},
              "text": {"selector"}, "expect_text": {"selector", "text"}, "screenshot": set()}
    for step in steps:
        if not isinstance(step, dict) or step.get("action") not in fields:
            raise ValueError("browser_action_invalid")
        required = fields[step["action"]] | {"action"}
        allowed = required | ({"accept_dialog"} if step["action"] == "click" else set())
        if not required <= set(step) or set(step) - allowed:
            raise ValueError("browser_action_fields_invalid")
        for key, value in step.items():
            if key == "accept_dialog":
                if type(value) is not bool:
                    raise ValueError("browser_dialog_policy_invalid")
                continue
            if not isinstance(value, str) or len(value) > MAX_TEXT or "\0" in value:
                raise ValueError("browser_action_value_invalid")
        if step["action"] == "expect_text" and not step["text"].strip():
            raise ValueError("browser_expected_text_required")
        if "path" in step:
            path = step["path"]
            if (not path.startswith("/") or path.startswith("//") or "\\" in path
                    or any(ord(c) < 32 for c in path)):
                raise ValueError("browser_relative_path_required")
        if "selector" in step and (not step["selector"].strip() or len(step["selector"]) > 1000 or ">>" in step["selector"]):
            raise ValueError("browser_selector_invalid")


def run(*, origin, steps, browser_executable, artifact_directory, timeout=30):
    local_origin(origin)
    validate_steps(steps)
    if not 1 <= timeout <= 60:
        raise ValueError("browser_timeout_invalid")
    # These paths are trusted Runner inputs, never part of the action schema.
    binary, artifacts = Path(browser_executable), Path(artifact_directory)
    if not binary.is_absolute() or not binary.is_file() or not artifacts.is_absolute():
        raise ValueError("browser_runtime_unavailable")
    artifacts.mkdir(exist_ok=False, parents=True)
    from playwright.sync_api import sync_playwright, expect
    deadline = time.monotonic() + timeout
    results, violations = [], []
    current_step = None
    dialogs, accept_dialog = [], False

    def remaining():
        left = int((deadline - time.monotonic()) * 1000)
        if left <= 0:
            raise TimeoutError("browser_deadline_exceeded")
        return left

    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(
            executable_path=str(binary), headless=True, chromium_sandbox=True,
            timeout=remaining(), args=["--disable-background-networking", "--no-proxy-server"])
        try:
            (artifacts / "engine.json").write_text(json.dumps({
                "phase": "launched", "engine_version": browser.version,
                "business_verified": False}), encoding="utf-8")
            context = browser.new_context(service_workers="block", accept_downloads=False,
                                          permissions=[], viewport={"width": 1280, "height": 800})

            def intercept(route):
                if allowed_url(route.request.url, origin):
                    route.continue_()
                else:
                    violations.append("request_outside_local_origin")
                    route.abort("blockedbyclient")

            context.route("**/*", intercept)
            # No WebSocket, popup, worker, download or extension channel is part
            # of this fixed form-interaction adapter.
            context.route_web_socket("**/*", lambda socket: socket.close())
            page = context.new_page()
            context.on("page", lambda popup: popup.close())
            page.on("download", lambda download: download.cancel())
            def on_dialog(dialog):
                permitted = accept_dialog and time.monotonic() < deadline and len(dialogs) < 8
                if len(dialogs) < 8:
                    message = dialog.message.encode("utf-8")
                    dialogs.append({"step": current_step, "type": dialog.type,
                                    "message": message[:MAX_TEXT].decode("utf-8", errors="ignore"),
                                    "accepted": permitted, "truncated": len(message) > MAX_TEXT})
                else:
                    violations.append("dialog_limit")
                dialog.accept() if permitted else dialog.dismiss()
            page.on("dialog", on_dialog)
            page.on("worker", lambda worker: violations.append("worker_not_supported"))
            for index, step in enumerate(steps):
                current_step = index
                action = step["action"]
                accept_dialog = action == "click" and step.get("accept_dialog", False)
                page.set_default_timeout(remaining())
                if action == "navigate":
                    page.goto(origin + step["path"], wait_until="domcontentloaded", timeout=remaining())
                else:
                    if not allowed_url(page.url, origin):
                        raise ValueError("browser_page_left_owned_service")
                    # Force CSS rather than accepting Playwright's alternative
                    # selector-engine language. No page.evaluate is exposed.
                    locator = page.locator("css=" + step["selector"]) if "selector" in step else None
                    if action == "click":
                        locator.click(timeout=remaining())
                    elif action == "fill":
                        locator.fill(step["text"], timeout=remaining())
                    elif action == "select":
                        locator.select_option(step["value"], timeout=remaining())
                    elif action in {"text", "expect_text"}:
                        if action == "expect_text":
                            expect(locator).to_contain_text(step["text"], timeout=remaining())
                        value = locator.inner_text(timeout=remaining())
                        if violations or not allowed_url(page.url, origin):
                            raise ValueError("browser_page_scope_violation")
                        encoded = value.encode("utf-8")
                        results.append({"index": index, "action": action,
                                        "text": encoded[:MAX_TEXT].decode("utf-8", errors="ignore"),
                                        "truncated": len(encoded) > MAX_TEXT,
                                        "matched": True if action == "expect_text" else None})
                    elif action == "screenshot":
                        name = f"step-{index:02d}.png"
                        page.screenshot(path=str(artifacts / name), timeout=remaining())
                        results.append({"index": index, "action": action, "artifact": name})
                if violations or not allowed_url(page.url, origin):
                    raise ValueError("browser_page_scope_violation")
                if action not in {"text", "expect_text", "screenshot"}:
                    results.append({"index": index, "action": action, "completed": True})
            result = {"engine_version": browser.version, "url": page.url, "steps": results, "dialogs": dialogs,
                      "business_verified": False}
            (artifacts / "result.json").write_text(json.dumps(result, ensure_ascii=False), encoding="utf-8")
            return result
        except Exception as error:
            # Keep partial execution and the failure distinct from success.
            # Raw browser exceptions may contain a redirect URL or document text.
            failure = {"completed": False, "error": type(error).__name__,
                       "failed_step": current_step, "dialogs": dialogs,
                       "steps": results, "scope_violations": violations,
                       "business_verified": False}
            (artifacts / "failure.json").write_text(json.dumps(failure, ensure_ascii=False), encoding="utf-8")
            raise
        finally:
            browser.close()


def main():
    """Bounded stdin/stdout carrier, invoked only by trusted Runner code.

    The public Agent request must never be forwarded verbatim here: the Runner
    supplies all four runtime/ownership fields after native service inspection.
    It also owns the outer process-tree deadline and the durable send fence.
    """
    import sys
    raw = sys.stdin.buffer.readline(65537)
    if len(raw) > 65536 or not raw.endswith(b"\n"):
        raise ValueError("browser_carrier_frame_invalid")
    request = json.loads(raw)
    if not isinstance(request, dict) or set(request) != {
            "origin", "steps", "browser_executable", "artifact_directory", "timeout"}:
        raise ValueError("browser_carrier_fields_invalid")
    success = True
    try:
        result = run(**request)
        payload = {"success": True, "result": result}
    except Exception as error:
        success = False
        code = str(error) if type(error) is ValueError and str(error).startswith("browser_") else "browser_action_failed"
        payload = {"success": False, "error": code}
        failure = Path(request["artifact_directory"]) / "failure.json"
        if failure.is_file() and failure.stat().st_size <= 1024 * 1024:
            payload["failure"] = json.loads(failure.read_text(encoding="utf-8"))
    encoded = json.dumps(payload, ensure_ascii=False)
    if len(encoded.encode("utf-8")) > 1024 * 1024:
        raise ValueError("browser_result_too_large")
    # -I intentionally ignores PYTHON* environment settings. Use bytes here so
    # Chinese page text cannot be emitted in the host's legacy console codec.
    sys.stdout.buffer.write(encoded.encode("utf-8") + b"\n")
    sys.stdout.buffer.flush()
    return success


if __name__ == "__main__":
    try:
        import sys
        sys.exit(0 if main() else 1)
    except Exception as error:
        # Only fixed local ValueError codes can cross the carrier. Library
        # exception strings may include page text, URLs or runtime paths.
        import sys
        code = str(error) if type(error) is ValueError and str(error).startswith("browser_") else "browser_action_failed"
        print(json.dumps({"success": False, "error": code}), flush=True)
        sys.exit(1)
