# Architecture convergence plan

Status: analysis completed before implementation, 2026-09-28. Baseline: branch
`feat/mycodex-tools`, HEAD `15e4287cda7f5e673dc449c34ed2838f9bcad4ca` plus the
existing uncommitted working tree. Existing Goal, document and registry changes
are inputs to this refactor, not changes to discard. A local source snapshot and
validation logs live under ignored `target/architecture-refactor-baseline/`.

## Current dependency and call graph

HTTP (`runtime_http`) and MCP (`mcp/tools`) enter `tool_runtime/kernel.rs`.
The kernel owns concrete argument parsing, OAuth/tool admission, trusted wrapper
metadata, recorder authorization and Host Goal request context. It calls the
canonical dispatch pipeline, which resolves projects and business Sessions,
authorizes Runner capabilities, evaluates PermissionEvaluator once, admits the
Host Goal effect, dispatches the typed call, and records canonical evidence before
model-facing result projection. These are successive parts of **one Kernel**.

Plugin/SSH typed calls and native MCP gateway calls use the existing specialized
governance path (`specialized.rs`); moving static family routing must not cause
double evaluation or bypass that path. Provider-dependent schema/effect validation
stays in the gateway. Low-level filesystem/path enforcement stays in the Runner.

`ToolRuntime` currently has 28 production fields (29 with its test hook): RunnerRegistry, provider
bindings, coding-run bindings, Goal workers, Session store/shells, checkpoint
metadata, policy, configuration, telemetry and SQLite handles. Most business
methods are already in separate `impl ToolRuntime` modules, but share unrestricted
knowledge of these fields. Host Goal and Communication happen to use the same
SQLite handle; that is not shared state authority.

`dispatch.rs` has 2,567 lines: approximately 740 lines of result projections,
790 lines of canonical pre/post execution pipeline, an 850-line typed match and
projection tests. Existing `*_tools.rs` already implement many families. The
remaining inline families are Goal, Coding Agent, Skill, Agent Task,
Communication and Memory. Reuse existing handlers rather than add another layer.

Runner `main.rs` has 5,838 lines. It contains CLI, bounded HTTP decoding/error
classification, registration, polling supervision, JobManager state/queue/start,
output/update delivery, detached recovery and process lifecycle. Existing
`webcodex_runner/` already owns shell/process/script execution, Plugin/MCP/ACP
providers, project registry, WebSocket/QUIC transport and shutdown. Those modules
must remain the implementation homes; no second provider or transport framework.

## Target and migration map

| Source responsibility | Destination / ownership |
| --- | --- |
| Runner main CLI parsing | `webcodex_runner/cli.rs` |
| Runner main HTTP framing/errors | `webcodex_runner/transport/http.rs` |
| Runner main polling supervision | `webcodex_runner/transport/polling.rs` |
| Runner main registration/capabilities | `webcodex_runner/registration.rs` |
| Runner main file dispatch helpers | `webcodex_runner/files/dispatch.rs` |
| Runner main Job state, queue and lifecycle | `webcodex_runner/jobs/` |
| Runner main process/read/output helpers | Job execution and process modules |
| Existing Runner shell/providers/project/shutdown | Retained in existing modules |
| Dispatch canonical pre/post execution | `tool_runtime/dispatch/pipeline.rs` |
| Dispatch model result projection | `tool_runtime/dispatch/projection.rs` |
| Dispatch inline family logic | Family modules beneath `dispatch/` |
| Dispatch typed routing | Small exhaustive `dispatch/mod.rs` |
| Runtime execution dependencies | Execution service, including run/shell ownership |
| Runtime Session dependencies | Session service, Session ledger and validation coordination |
| Runtime Goal dependencies | Goal service, Goal database and worker ownership |
| Runtime provider bindings | Gateway service with separate provider registries |
| Runtime observation dependencies | Observation service, with no execution authority |
| Runtime limits and exposure | RuntimeConfig |
| Memory and Communication dependencies | Separate domain services sharing injected SQLite |

The service work must move behavior and dependency ownership, not merely add a
bag named Services. Keep the public `ToolRuntime` constructor/builders/dispatch
and its existing public RunnerRegistry/RuntimeInfo fields compatible. Do not
create a second independently mutable registry behind a public field; a borrowed
execution view is acceptable where needed to preserve that contract. Internal
callers should use domain operations, not reach through multiple service fields.
Small concrete Rust structs suffice; no new traits or dependencies are required.

## Authority and public/internal API

The paired [State Ownership Contract](STATE_OWNERSHIP.md) and
[Chinese version](STATE_OWNERSHIP.zh-CN.md) govern the migration. In particular:
Host Goal phase/plan/attempt/fence and retired contexts are SQLite authority;
JobManager/RunnerRegistry own process execution facts; Session history is evidence;
provider schemas are observations; telemetry cannot authorize execution.

Wire types live in `webcodex-core`, `webcodex-tool-contracts` and
`webcodex-tool-runtime-contracts`. Their names, schemas and serde shapes are out
of scope. Existing `pub(crate)` module layout may change; public facade methods,
HTTP/MCP responses, Runner wire protocol, SQLite schema, permission policy,
idempotency keys and persistence/recovery formats must not change. Any required
public incompatibility stops implementation for discussion.

## Risks and controls

| Risk | Control |
| --- | --- |
| Existing dirty work lost | Snapshot current sources; guarded extraction; compare against working baseline, not only HEAD |
| Moved Rust visibility or relative paths | Small crate checks after each phase; preserve cfg/test module visibility |
| Dropped security gate / double permission evaluation | Move pipeline intact; handlers receive typed calls after the same gates |
| Changed evidence order or sparse response | Move projection intact; preserve record-before-project ordering |
| Retry after unknown effect | Preserve lifecycle enums, receipt handling and no-replay tests |
| Retired context regains authority | Keep durable context admission/retirement and fresh-attempt/fence tests |
| Arc/lock lifetime altered by service extraction | Preserve clone sharing and lock scope; no new locks around awaits |
| Queue / shutdown / detached lifetime regression | Mechanical JobManager movement first; retain process ownership tests |
| Schema observation mistaken for binding/authority | Keep independent gateway stores and fail-closed describe/call validation |

## Phases and verification

1. Complete this analysis and baseline check before source edits.
2. Write bilingual State Ownership Contract from the actual implementation.
3. Mechanically extract Runner responsibilities; check and test Runner.
4. Separate dispatch pipeline/projection from router; extract remaining inline
   families without duplicating policy. Check the server and contract tests.
5. Converge Runtime dependencies behind services, preserving facade compatibility.
6. Review the baseline-relative diff; run `cargo fmt --check`,
   `cargo check --workspace`, and `cargo test --workspace`. Report any baseline
   failures separately. All nine requested core crates must be exercised.

Regression evidence includes `runtime_http` and `mcp` tool/kernel tests;
`host_goals`, `host_goal_context`, `host_goal_handoff`,
`host_goal_handoff_background`, `host_goal_active_operations` and
`mcp_tests/host_goal_transport`; registry
`dispatched_bridge_disconnect_is_outcome_unknown_and_not_replayed`;
Plugin/MCP describe/call/schema-change suites; permission single-evaluation,
Session recording/projection, detached Job, reconciliation and instance-lease
suites. Ordinary tests skip explicitly ignored process/timing suites; run relevant
Runner process groups separately and label that evidence accurately.

Final results and the exact migration/file list will be recorded in
`ARCHITECTURE_REFACTOR_REPORT.md`; future opportunities must be distinguished from
unfinished acceptance work. No commit, push, deployment or service restart is
part of this request.
