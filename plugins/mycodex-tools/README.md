# MyCodex Office and PDF native plugin

[简体中文](README.zh-CN.md)

Eleven headless Office/PDF tools for the existing WebCodex Runner. This optional
plugin uses the [native plugin gateway](../../docs/PLUGINS.md); it does not add an
Agent framework, another MCP server, or tools to outer MCP `tools/list`.

## Architecture and existing capabilities

The model calls WebCodex over MCP, then `plugin_tool` routes to a Python process
on the selected Runner. Native file/process/Job/artifact tools retain their own
contracts. The plugin implements only missing document content operations.

| Capability | Owner |
| --- | --- |
| UTF-8 text reads, creation, guarded edits, search | Existing WebCodex file tools |
| Directory management, copy/move, generic file metadata | Existing PowerShell/Python execution |
| PowerShell | `run_script(language="powershell")` or `run_shell` |
| Python | `run_process` with the venv Python executable and literal argv |
| DOCX/XLSX/PPTX/PDF content | This plugin, through `plugin_tool` |
| PNG/JPEG generation | Pillow scripts executed through `run_process` |
| Binary metadata, download, native image content | Existing artifact tools |

For legacy text encodings, explicitly choose the Python codec. There is no
automatic encoding guess or implicit rewrite. Long-running Python work follows
the existing Job handle; polling a Job must not rerun its command.

## Install without changing a running Runner

Requires Python 3.11. From this directory:

```powershell
.\install.ps1
```

The default isolated environment is `E:\wbecodex\.local\venvs\mycodex-tools`.
Use `-Python` and `-VenvPath` to select another Python 3.11 and dedicated environment.
The installer uses `requirements.lock`, checks dependency consistency, and does
not modify Runner configuration, credentials, services, or the global Python.
`requirements.in` records direct requirements; the lock also pins transitive
dependencies, resolved on Windows x64/Python 3.11.

Libraries: python-docx (MIT), openpyxl (MIT), python-pptx (MIT), pypdf (BSD-3),
ReportLab (BSD), Pillow (MIT-CMU), defusedxml (PSF). No Office installation,
COM, LibreOffice, OCR, OpenCV, paid API, or MCP SDK is required. System fonts
are not distributed. PDF creation requires a local Chinese-capable TTF font;
the supplied configuration points to Windows `simhei.ttf`.

## Explicit activation on a selected Runner

1. Review `runner.example.toml`, then merge its provider into the selected writable
   Runner's existing `[plugins]` section. Do not duplicate that TOML table.
2. Set absolute Python/script/font paths and set provider `cwd` to the intended
   registered project root. The sample uses `E:\wbecodex\sandbox`.
3. Ensure the client has `plugin:inspect` and `plugin:invoke`; an operator performing
   check/reload also needs `plugin:manage`. Inspect/invoke do not grant management.
   Do not silently widen existing client credentials or a read-only Runner.
   The shared-key OAuth bridge opts into inspect/invoke with
   `webcodex connect ... --auth oauth --oauth-local-plugins`; that flag does not
   grant management. Existing clients need reauthorization for changed scopes.
4. Use the normal gateway sequence:

```text
plugin_tool(action="check", runner="webcodex-local-sandbox", plugin="mycodex-tools")
plugin_tool(action="reload", runner="webcodex-local-sandbox")
plugin_tool(action="list", runner="webcodex-local-sandbox", plugin="mycodex-tools")
plugin_tool(action="describe", runner="webcodex-local-sandbox", plugin="mycodex-tools", tool="docx_create")
plugin_tool(action="call", binding="<binding from describe>", arguments={...})
```

Check starts a disposable plugin process but does not write documents or commit
the provider. Reload activates all configured providers atomically; it does not
restart the Runner. Describe again after reload: previous bindings are stale.
For rollback, remove this provider from the configuration and reload after checking
the remaining configuration; created documents are not deleted.

The plugin `cwd` is **not** inferred from a model's active project. Calls to native
file/process/artifact tools must explicitly select the same registered root.
Import/copy inputs into that project first using the authorized existing tools.
For another project, configure a separately named provider with that exact root;
there is no caller-supplied root override.

## Tool API

`describe` is the authoritative input schema. All paths are root-relative. Parents
must already exist. Every create/update requires a new `output_path`; updates also
require `path` and the `expected_sha256` returned by a previous read.

| Tool | Content / selection |
| --- | --- |
| `docx_read` | Body paragraphs and heading styles; top-level tables. Zero-based paragraph/table starts and counts; row/column ranges for tables. |
| `docx_create` | Ordered `blocks`: paragraph, heading (`level` 0–9), rectangular table (`rows`). |
| `docx_update` | `paragraph_updates`, `cell_updates`, and/or `append_blocks`; indexes are zero-based. |
| `xlsx_read` | Paginated sheet list (`sheet_start`/`sheet_count`); optional `sheet` and A1 `range` (default A1:J20). Formulas are expressions, not calculated values. |
| `xlsx_create` | `sheets` with `name` and typed `cells`. |
| `xlsx_update` | `updates` with `sheet` and typed `cells`. |
| `pptx_read` | Zero-based slide and flattened shape starts/counts, titles, text and tables, including groups. Use `shape_start`/`shape_count` within a dense slide. |
| `pptx_create` | `slides` with title, optional body and bullets; editable 16:9 slides, 32 pt headings and 24 pt body. |
| `pdf_read` | One-based page starts/counts, text, metadata, flat bookmarks; no OCR. |
| `pdf_search` | Literal `query` in a page range; case-sensitive by default. |
| `pdf_create` | Explicit A4 `pages`, each containing heading, paragraph and table blocks. Overflow is rejected. |

Excel cell example: `{"coordinate":"A1","type":"text","text":"=literal text"}`.
Types are `text`, `number`, `boolean`, `formula`, `empty`; provide only the matching
value field, and none for `empty`. An explicit formula uses `"formula":"=SUM(B1:B2)"`.

Create a Word document (arguments to the described `docx_create` binding):

```json
{"output_path":"test.docx","blocks":[{"type":"heading","text":"工作汇报","level":1},{"type":"paragraph","text":"WebCodex document test"},{"type":"table","rows":[["阶段","状态"],["第一阶段","完成"]]}]}
```

Read it with `docx_read {"path":"test.docx"}`, then update a new copy:

```json
{"path":"test.docx","expected_sha256":"<sha256 from read>","output_path":"test-updated.docx","paragraph_updates":[{"index":1,"text":"Verified update"}]}
```

Use `read_project_artifact_metadata` on the matching project to verify actual
bytes/MIME/hash; use the existing export path to retrieve the file. For generated
images use `read_project_artifact(as_image=true)`; the native decoded image ceiling
is 1 MiB. Merely returning a path is not proof that the image reached the model.

## Bounds, failures and fidelity

- Input and generated files: 32 MiB maximum; Office ZIP declared expansion:
  128 MiB and 10,000 entries maximum. Provider timeout: 60 seconds.
- Arguments: 64 KiB; structured result: 128 KiB; JSON depth: 16; nodes: 4,096;
  individual strings: 64 KiB UTF-8. Oversized output is an explicit error, never
  an undocumented truncation. Select fewer cells/pages/paragraphs and use
  `text_offset`/`max_chars` for long text. Counts and continuation information
  distinguish selected content from full-document totals.
- Sources are read into a bounded snapshot. Stale update hashes fail before
  publication. Excel copy updates also verify unedited stored basic cell values
  and types; an unrepresentable round trip fails without publishing. Writers
  verify a sibling temporary file, then claim an absent
  destination atomically using a same-filesystem hard link. Unsupported filesystems
  fail rather than falling back to an overwriting write.
- Absolute paths, traversal, device/alternate-stream names, reparse points,
  symlinks and protected credential/config/Git paths are rejected. This is an
  ordinary mistake-prevention policy, **not an OS sandbox** or protection against
  hostile local filesystem races. Native plugins are trusted local executables.
- Tool errors use `isError=true` and `structuredContent.error.code/message`.
  Common codes include `target_exists`, `stale_source`, `path_rejected`,
  `file_too_large`, `result_too_large`, `missing_font`, `layout_overflow`.
  Library exceptions are not copied verbatim into model output.
- Gateway `busy` means no queued duplicate. `OutcomeUnknown` is not failure proof:
  inspect the intended destination and verify expected content first. Do not
  replay automatically or treat an unrelated existing file as success. Writes
  intentionally do not advertise idempotency.
- Word text replacement replaces runs/inline formatting at the edited location.
  Headers, footers, text boxes, revisions and complex layout are outside the read
  contract. Excel complex objects and external links may not survive a save;
  formulas are not recalculated. Original Word/Excel inputs are retained.
- PPTX generation is basic editable content, not animation/SmartArt/master-layout
  reproduction. PDF structure means pages, text, metadata and existing bookmarks,
  not recovered semantic headings or table cells. Scans report
  `no_extractable_text`; extraction/search of externally produced PDFs depends on
  their text mappings. Font coverage and viewer rendering remain relevant.

## Validation and further work

```powershell
$py = 'E:\wbecodex\.local\venvs\mycodex-tools\Scripts\python.exe'
& $py -m unittest discover -s tests -p 'test_*.py' -v
& $py tests/e2e_mcp.py --help
```

The E2E harness starts only its own loopback Server/Runner with synthetic test
credentials and a disposable project. It exercises MCP, the native gateway,
document round trips and built-in execution/artifact paths. See `TEST_REPORT.md`
for the actual results, commands, version identity and limitations.

Future OCR, richer templates, rendering or additional project providers should
be separate explicit extensions. No new generic execution interface is needed.
