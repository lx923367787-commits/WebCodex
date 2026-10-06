# WebCodex

**English** | [简体中文](README.zh-CN.md)

**Connect ChatGPT, Claude, and other AI agents to code, developer tools, and office documents on your own machines.**

WebCodex provides project-scoped file access, editing, Git, commands, tests, observable jobs, and code navigation. This customized source tree also includes persistent goal plans and acceptance checks, conversation handoff, review feedback, and Office / PDF / OCR and drawing workflows.

Based on [yyjeqhc/webcodex](https://github.com/yyjeqhc/webcodex). Upstream material remains under [Apache-2.0](LICENSE); original contributions owned by the customization contributors use [MIT](LICENSE-MIT). See [License scope](LICENSING.md). The source declares version `0.4.1`; available features depend on the actual commit, build identity, and configuration. Upstream packages and this customized source are separate distribution paths.

## Features

| Capability | What it provides | Requirements |
| --- | --- | --- |
| Projects and code | Read, search, guarded edits, Git inspection, code navigation | Registered projects and appropriate permissions |
| Execution and jobs | Real compilers, tests and scripts; observe and cancel long-running work | An available Runner and installed toolchain |
| Desktop | Manage local Server / Runner, projects, connections and activity | A Desktop build for the target platform |
| Host Goal mode | Discuss requirements, save a plan, confirm its version, execute and verify nodes | Server / Runner with Host Goal support |
| Background ACP goals | Execute, check and repair within round, time and no-progress budgets | A separately configured ACP provider and model authentication |
| Handoff and review | Persist checkpoints, resume in another conversation, track review evidence | Persistent goal records and current tool contracts |
| Office / PDF / OCR | Read, edit, convert, restore backups and inspect rendered pages | Document worker, renderers and format-specific dependencies |
| Drawing | Flowcharts, architecture diagrams, data charts and editable JSON specifications | Drawing Python environment and required renderers |

Features reuse existing project permissions and execution channels. Installing a dependency or enabling a connection does not grant additional authority.

## Architecture

```text
ChatGPT / Claude / other MCP clients
                 | MCP / HTTPS
                 v
               Server
      APIs, permissions, plans, durable state
                 |
                 v
               Runner
       Files, Git, commands, jobs, plugins
                 |
                 v
       Authorized projects and local tools

Desktop manages the local runtime, projects and connection settings.
```

Repository files remain on their owning machine. Content read by the agent and tool outputs may still be transmitted to the connected AI service. See [Architecture](docs/ARCHITECTURE.md), [State ownership](docs/STATE_OWNERSHIP.md), and [Authentication](docs/AUTH_MODEL.md).

## Getting started

### Daily use: Desktop or a regular Server + Runner

For Windows / macOS, follow [Desktop installation](docs/desktop-install.md), then [Using Desktop](docs/desktop-guide.md) to register a working directory, configure the connection, and check Runner availability. For CLI, Linux, existing Servers, or advanced setup, see [Full Setup](docs/PERSONAL_SETUP.md).

Use a build containing this repository's changes when you need its customized features. Upstream download links and npm commands in inherited documentation refer to upstream distributions. This README does not assume a public download location for this customized repository.

Public HTTPS, Cloudflare Tunnel, and OpenAI Secure MCP Tunnel provide different ways to reach the Server. Configure API keys, tunnel IDs, and other credentials in the actual runtime environment; do not distribute them with source code.

### Quick trial using this source build

After building the runtime below, run the resulting CLI inside the project you want to share:

```powershell
# Windows: replace both paths with your actual directories
Set-Location 'C:\work\my-project'
& 'C:\work\webcodex\target\dogfood\webcodex.exe' share
```

```bash
# Linux / macOS: replace both paths with your actual directories
cd /path/to/my-project
/path/to/webcodex/target/dogfood/webcodex share
```

`share` creates a temporary, single-project, restricted environment. Its temporary connection expires when the command exits. Use a regular Server + Runner for a durable setup. See [Quick Trial](docs/QUICK_START.md) and [CLI](docs/CLI.md) for options and boundaries.

### Goal workflows

Host Goal mode is advanced by the model in the current conversation and does not require a separate background ACP provider. Ask the assistant to read the project, discuss the requirements, and save a local plan. Review and confirm that specific plan version before execution. The assistant then executes and verifies individual nodes and can persist a handoff checkpoint for another conversation. Saving a plan alone does not authorize execution.

The detailed [Host Goal guide](docs/HOST_GOAL_MODE.zh-CN.md) is currently in Chinese. For a server-driven execution, verification, and feedback loop, separately configure an ACP provider and follow [Background Goal mode](docs/GOAL_MODE.zh-CN.md). A finished model response or an existing output file does not by itself satisfy goal acceptance.

### Documents and drawing

With the document worker in `plugins/mycodex-tools` configured, use `present_document`, `document_edit`, `document_convert`, and `document_restore`. Edits bind to the current source SHA; in-place edits create backups, and conversions preserve their inputs. Observe the same Job when an operation continues in the background. See [Plugin documentation](plugins/mycodex-tools/README.md) and the Chinese [Office workflow](docs/OFFICE_WORKFLOW.zh-CN.md).

Native Office operations and previews depend on locally installed Office, which is not bundled. Some operations use OOXML tooling. PDF / OCR features use dependencies such as PDFium and Tesseract; complex PDF-to-DOCX conversion does not guarantee layout preservation. Successful generation and completed visual review are separate results: inspect actual pages and review status.

Drawing uses JSON specifications, Graphviz and Matplotlib for diagrams and charts. Specifications, previews and manifests support subsequent edits. See the Chinese [Drawing workflow](docs/DRAWING_WORKFLOW.zh-CN.md). Replace historical machine paths and project IDs in examples with your actual configuration.

## Build from source

### Prerequisites

- Rust stable / Cargo and the target platform's native build tools.
- Node.js / npm for the web and Desktop frontends; their lockfiles define dependencies.
- On Windows, MSVC Build Tools and Windows SDK for native builds, and WebView2 to run Desktop. Prepare the appropriate native development dependencies on macOS / Linux.
- Office / OCR / drawing are optional extensions. Prepare their specified Python environment, fonts, renderers and external applications when enabling them.

Run the following commands at this repository's root. In the local archival layout, that directory is `source`, rather than its parent.

### Server / Runner / CLI

Build the embedded web assets before the runtime:

```bash
npm ci --prefix frontend
npm run build --prefix frontend
cargo build --locked --profile dogfood --workspace --bins
```

Outputs are in `target/dogfood/`, including `webcodex`, `webcodex-server`, and `webcodex-runner` (`.exe` on Windows). Use `dogfood` for iterative development builds. Formal releases use `release` and follow [Release process](docs/agent/release-process.md) and [Release checklist](docs/RELEASE_CHECKLIST.md).

### Desktop

Desktop is a separate Cargo workspace. The runtime build above does not automatically build it:

```bash
npm ci --prefix apps/desktop
npm run build --prefix apps/desktop
cargo build --locked --profile dogfood --features tauri/custom-protocol --manifest-path apps/desktop/src-tauri/Cargo.toml --bin webcodex-desktop
```

The Desktop output is under `apps/desktop/src-tauri/target/dogfood/`, unless `CARGO_TARGET_DIR` overrides it. A distributable installer also requires the matching runtime, tool resources, and bundle configuration. Building the Desktop executable alone does not produce a complete installer. See [Release process](docs/agent/release-process.md) and `scripts/prepare_desktop_bundle.ps1` / `scripts/prepare_desktop_bundle_macos.py`.

## Development and validation

Select checks appropriate to the change, for example:

```bash
python scripts/check_markdown_links.py --root .
npm run typecheck --prefix frontend
npm test --prefix frontend
npm run typecheck --prefix apps/desktop
npm test --prefix apps/desktop
cargo test --locked -p webcodex --lib metadata
```

Documentation-only changes do not require a Rust build. Real-process, end-to-end, and release checks have additional prerequisites; see [Testing](docs/TESTING.md). Historical acceptance reports apply only to their recorded source, build and environment, and do not replace validation of the current checkout.

## Repository layout

| Path | Purpose |
| --- | --- |
| `src/` | Server, HTTP / MCP surfaces, tool dispatch and goal workflows |
| `crates/` | Runner, CLI, core protocols, storage and Rust contracts |
| `frontend/` | Administration and runtime web UIs, goal panel, build and tests |
| `apps/desktop/` | Tauri / React Desktop with a separate Rust workspace |
| `plugins/mycodex-tools/` | Office / PDF / OCR, drawing and document worker |
| `plugins/safe-delete/` | Deletion plugin requiring explicit configuration and authorization |
| `scripts/` | Build, verification, deployment and release tooling |
| `docs/` | User, architecture, feature and operations guides |
| `.github/workflows/` | CI and release workflows; review destinations and secrets for your repository |
| `npm/webcodex/` | npm wrapper and publishing tools; the existing package name belongs to upstream |

## Platforms

| Platform | Core runtime | Desktop |
| --- | --- | --- |
| Windows x64 | CLI, Runner, local foreground Server, temporary share | Supported; primary platform for native Office customization |
| Windows arm64 | CLI, Runner, foreground Server, share; Cloudflare requires a trusted external binary | Current installer targets x64 |
| macOS x64 / arm64 | CLI, Server, Runner, share | Supported; verify extension dependencies separately |
| Linux x64 / arm64 | CLI, Server, Runner, share | CLI / self-hosted workflows |

Core platform support does not establish acceptance of every customized feature or native Office operation on that platform. Standalone Windows Server service hosting has different boundaries from Desktop's owned foreground runtime. See [Deployment](docs/DEPLOYMENT.md).

## Documentation

- [Desktop installation](docs/desktop-install.md), [Full Setup](docs/PERSONAL_SETUP.md), [Troubleshooting](docs/TROUBLESHOOTING.md)
- [Host Goal](docs/HOST_GOAL_MODE.zh-CN.md), [ACP Goal](docs/GOAL_MODE.zh-CN.md), [Review feedback](docs/REVIEW_FEEDBACK_LOOP.zh-CN.md) — Chinese feature guides
- [Office](docs/OFFICE_WORKFLOW.zh-CN.md), [Drawing](docs/DRAWING_WORKFLOW.zh-CN.md) — Chinese workflow guides
- [Architecture](docs/ARCHITECTURE.md), [State ownership](docs/STATE_OWNERSHIP.md), [MCP](docs/MCP.md), [CLI](docs/CLI.md)
- [Security](SECURITY.md), [Contributing](CONTRIBUTING.md), [Documentation index](docs/INDEX.md)

## Maintaining your own GitHub repository

The source repository builds independently of the parent archive's installers, offline toolchains and migration material. Preserve and review actual modifications and new files before pushing; an older HEAD may not include all customized features. Inspect proposed commits and existing history for credentials and machine-private state.

Keep upstream licensing and copyright notices. Before publishing your own npm package, container image or installer, review package names, repository metadata, CI publication destinations, secrets and download locations. Inherited upstream destinations must be adapted before publishing from a personal repository. Use your actual repository URL and release locations.

## Contributing and license

For issues, include the version / commit, OS, client, reproduction steps, and redacted logs. Keep contributions focused and provide relevant validation; see [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).

Based on [yyjeqhc/webcodex](https://github.com/yyjeqhc/webcodex). Thanks to upstream contributors and the [LINUX DO](https://linux.do/) community. Retain [Apache License 2.0](LICENSE) for upstream material, [MIT](LICENSE-MIT) for applicable original customizations, and the [attribution notice](NOTICE). [LICENSING.md](LICENSING.md) defines the scope; third-party dependencies retain their own licenses.
