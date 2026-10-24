# Windows snapshot validation — 2026-10-24

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
