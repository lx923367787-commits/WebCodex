"""One-shot trusted worker launched by Runner Jobs, never by model-supplied argv."""
from __future__ import annotations

import argparse
import contextlib
import json
from pathlib import Path
import sys

from mycodex_tools.common import Context, ToolError, bounded_json, encode
from mycodex_tools.document_store import DocumentStore, atomic_json, identifier
from mycodex_tools.documents import dispatch


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--request", required=True)
    parser.add_argument("--result", required=True)
    parser.add_argument("--font-path")
    parser.add_argument("--tesseract-path")
    parser.add_argument("--tessdata-path")
    args = parser.parse_args()
    request_id = None
    result_path = None
    try:
        source = sys.stdin.buffer if args.request == "-" else Path(args.request).open("rb")
        try:
            raw = source.read(128 * 1024 + 1)
        finally:
            if source is not sys.stdin.buffer:
                source.close()
        if len(raw) > 128 * 1024:
            raise ToolError("request_too_large", "Worker request exceeds its input budget.")
        request = json.loads(raw)
        request_id = identifier(request.get("request_id"))
        expected_result = f".webcodex-office/cache/{request_id}/result.json"
        if args.result.replace("\\", "/") != expected_result:
            raise ToolError("invalid_result_path", "Worker result path must belong to this request id.")
        ctx = Context(Path.cwd(), args.font_path)
        store = DocumentStore(ctx, request_id)
        result_path = store.work / "result.json"
        with contextlib.redirect_stdout(sys.stderr):
            output = dispatch(ctx, request["operation"], request["arguments"],
                              {"font_path": args.font_path, "tesseract_path": args.tesseract_path,
                               "tessdata_path": args.tessdata_path}, request_id)
        result = {"request_id": request_id, "success": True, "output": output, "error": None}
        bounded_json(result, 256 * 1024)
    except ToolError as error:
        if error.code in ("file_busy", "request_conflict", "outcome_unknown"):
            # Another attempt owns this identity, or its effects are not yet
            # reconciled. Never replace its original evidence with our error.
            result_path = None
        result = {"request_id": request_id, "success": False, "output": None,
                  "error": {"code": error.code, "message": error.message}}
    except Exception as error:
        print(f"Document worker: {type(error).__name__}", file=sys.stderr)
        result = {"request_id": request_id, "success": False, "output": None,
                  "error": {"code": "operation_failed", "message": "Document operation failed; inspect format, dependencies and local file permissions."}}
    if result_path is not None:
        if not result_path.exists():
            atomic_json(result_path, result)
        # Repeated Job reads consume the original archive. They cannot edit
        # its image provenance or turn a previous result into a new judgment.
    # A small terminal result, not an unbounded copy of document contents.
    print(encode({"request_id": request_id, "success": result["success"],
                  "result_path": args.result if result_path else None,
                  "error": result["error"]}).decode("utf-8"))
    return 0 if result["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
