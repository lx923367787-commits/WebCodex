# Windows snapshot validation — updated 2026-10-25

## Native fresh-chat connection repair — 2026-10-25

The deployed repair was synchronized as 12 exact SHA-256-matched files: 10 Rust source/test files (including two new files) and two troubleshooting documents. It connects first-use native chats to independent Agent enrollment while preserving existing isolation, revocation and retired-window boundaries.

Existing verification was reused only after matching every repaired source file to its recorded after-SHA: **4 Store + 15 enrollment HTTP + 5 contract tests passed (24 distinct tests)**, and the Server dogfood build succeeded. A separate three-test run was a subset of the 15 and is not counted again. The installed and packaged Server hash is `dc6cc07bf7a443be6032d323a8dc682a271f8074ace8c6fb31af62cf5d24f5e8`; its original build identity remains commit `15e4287cda7f5e673dc449c34ed2838f9bcad4ca`, dirty=true, built_at=1792858791. Desktop, CLI, Runner and third-party runtime bytes remain unchanged.

The repair's live evidence reports successful connection, stable independent identity, readable assignment and 34 tools; existing runs were preserved. Some repeated connector calls returned older failures without reaching the new Server. The exact upstream replay/cache layer is not established, so this transfer does not claim to eliminate all stale connector responses.

New machines must configure their own enabled native Host, owner and project through the local Bootstrap control API. `configure-native-host.py` and [NATIVE-HOST-SETUP.zh-CN.md](NATIVE-HOST-SETUP.zh-CN.md) provide that step without copying old identifiers or credentials. The deployed policy keeps `max_subagents=0` and `max_cli_calls=0`; this repair does not remove those existing execution limits.

The portable setup helper passed eight offline tests using synthetic local state/SQLite and mocked API responses, including default read-only behavior, disabled-host refusal, exact-target checks, one-time apply/readback and no retry after an uncertain control response. It uses Python standard-library modules and no existing account or production configuration was changed during these tests.

Changed-line whitespace validation passed. The repaired `agent_isolation.rs` import formatting does not pass the standalone rustfmt check; it is retained byte-for-byte with the tested deployment source. The other nine repaired Rust files pass that formatting check. No production service was restarted by this upload task.

Updated source/client ZIPs are dated 20261025. The 20261024 draft remains available as a previous snapshot; use the newer draft for deployment.

## Complete home-computer transfer follow-up

The follow-up explicitly adds the current Windows client binaries and runtime dependencies, not just build recipes. The canonical production source was rehashed and had not changed since the first snapshot. Five previously omitted historical development helpers and the three parameterized installer build scripts are now included. The old installed uninstaller is intentionally excluded because it belongs to the original installation identity; its Tauri/NSIS source is retained.

The portable package contains 9,868 recorded files before its root manifest, about 2.06 GB unpacked. Its Desktop, Server, Runner and CLI hashes match the current installed binaries. CodeBuddy 2.162.0, standalone Codex 0.159.2, ACP's separate Codex 0.153.4, Antigravity 1.4.5, Node 24.15.0 and Tunnel client 0.0.12 are present with their runtime resources. Only fixed `--version` commands were used; no model task, GUI, service restart or login operation was performed.

Fresh portable Python 3.11.9 matched all 34 provider dependency versions and imported the native pywin32 DLLs successfully. Portable-provider tests ran 632 cases: **603 passed and 29 opt-in cases skipped**. A separate entirely offline setup rebuilt a new venv from the bundled wheelhouse and passed `pip check`. Windows PowerShell 5.1 generated four TOML templates with Chinese/spaced paths, and launcher environment handling was checked using headless fixtures. The packaged drawing `doctor` passed for Graphviz and all chart/document dependencies. The complete package file set and every recorded file hash were checked after these tests; no extra runtime/state files appeared.

The installer helper Python scripts passed syntax, controlled payload/hash/escaping tests and refusal cases. NSIS was not installed on the validation machine, so the newly parameterized historical NSIS installer was not compiled or executed. The supplied running client binaries came from the existing installation; no claim is made that this installer produced them.

The Windows transfer is stored as authenticated GitHub **draft release assets**, with a source ZIP, client ZIP, per-file manifest and SHA256SUMS. Keeping it draft prevents the repository's existing `release: published` workflow from publishing a Linux image. Source development remains on `windows/current-20261024`; `main` and other platforms are unchanged. See [HOME-DEVELOPMENT.zh-CN.md](HOME-DEVELOPMENT.zh-CN.md) for download and setup.

## Original source snapshot checks

Validation ran on Windows x64 against an isolated copy of the source identified by the current deployment records. The original development checkout and installed services were not changed. The earlier development checkout was found to omit newer Host enrollment, team execution and Desktop changes, so it was replaced by the canonical deployment source before the checks below.

## Completed checks

| Check | Result |
| --- | --- |
| Rust `cargo check --locked --offline --workspace` | Passed for all 17 root workspace members |
| Native Host contract / Runner Agent execution / proxy boundaries | 5 + 24 + 2 tests passed |
| Runtime frontend type checking, committed dist consistency, tests | Passed; 119 tests |
| Desktop frontend type checking, CSS contracts, production build, tests | Passed; 137 tests |
| Desktop native Windows build with `tauri/custom-protocol` | Passed; separate Desktop Cargo workspace |
| Built Desktop `--check-embedded-assets` | Exit 0; no GUI or runtime services started |
| Fresh Office/PDF/OCR payload build | Passed; 1,957 files, exact pinned packages, pywin32/PDFium and both OCR languages checked |
| Fresh drawing add-on build | Passed; 2,493 files, official Graphviz ZIP and extracted files verified, pinned Python packages installed |
| Document store/workflow tests using the fresh embedded Python | 34 tests passed |
| Handoff, chat agents and pilot offline tests | 587 passed; 29 browser opt-in tests skipped; 616 total |
| Fresh provider installation | 34 pinned Python packages matched, `pip check` passed, both `npm ci` completed; three generated TOML files parsed and their executable/script paths existed |
| Dependency inventory | Download URLs, archive/font/OCR hashes and all dependency lockfiles recorded |
| Publication content review | No runtime credentials, databases, browser profiles, personal documents or logs selected |

The Desktop validation used the existing `dogfood` profile with package opt-level 0 and incremental compilation disabled to reduce verification time; the published build helper uses the normal `dogfood` profile. The toolchain was Rust 1.98.1 MSVC, Visual Studio Build Tools 2022/MSVC 14.44.35207, Windows SDK 10.0.26100.0, Node 24.15.0/npm 11.12.1, Python 3.11 and PowerShell 7.6.5. The embedded document runtime is separately pinned to Python 3.11.9.

## Scope and remaining checks

`cargo fmt --all --check` reports existing formatting differences in 12 source files. These were retained to preserve the deployed product source; successful compilation is not a claim that this formatting check passed. Build warnings also remain. No Linux, iOS or macOS build, release or deployment was performed.

The current Goal provider lock retains `zod@4.6.5`. Its fresh `npm ci` audit reports one high-severity advisory, [GHSA-mrhx-c7xc-m3w5](https://github.com/advisories/GHSA-mrhx-c7xc-m3w5), for memory exhaustion with oversized constrained collections; the reported fixed version is 4.6.6. This snapshot preserves the deployed dependency version rather than silently changing behavior. The extension-runtime dependency audit reported no vulnerabilities. Dependency installation success is not a clean security-audit claim.

The browser-dependent opt-in tests are excluded from this headless validation. Live ChatGPT login, extension approval, external agent CLI authentication, tunnel setup and licensed Office on a new machine must be completed by that machine's operator. The source and pinned dependency recipes reproduce the implementation; this is not evidence of a complete fresh-machine, authenticated end-to-end acceptance run or byte-identical EXE reproduction.

The portable build helper is syntax checked, and its frontend/native and Office/drawing build commands were exercised individually. The complete helper has not been run from beginning to end as a single clean-clone packaging invocation. Installation and configuration steps are documented in [README.md](README.md) and [chat-agents/INSTALL.zh-CN.md](chat-agents/INSTALL.zh-CN.md).
