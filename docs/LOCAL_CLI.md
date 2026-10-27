# Unified local CLI tool

`cli` discovers and invokes programs installed on the selected Runner computer.
Its default `execution_mode: "local"` uses native process jobs and the Runner
user's existing environment and login. It does not require a fixed model-provider
list, a specific CLI version/hash approval, or a frozen team plan.

Local mode runs with the operating-system permissions of the Runner user. The
project selects the starting directory; it is not an OS filesystem sandbox.
Existing explicit isolated shell profiles and Runner execution policy remain
effective. No login is copied and no global environment setting is modified.

## Discover, select, invoke

1. Read the exact authorized project identifier in `agent_assignment.project_access`.
2. `cli({action:"discover", request:{task_id}})` lists installed adapters and their
   stable launcher commands. `models` with `{task_id,cli_id}` reads actual native
   model/reasoning options. Installation and help do not prove account entitlement.
3. Start a selected adapter using `action:"start"` and
   `request:{project,cli_id,model,effort?,instruction,idempotency_key,timeout_secs?}`.
   The adapter on that exact Runner supplies native flags and input framing.
4. For a CLI with a custom interface, discover an explicit command using
   `request:{project,commands:[name_or_absolute_path]}`. Inspect its own help, then
   start with `request:{project,executable,args,stdin?,model?,effort?,idempotency_key}`.
   Arguments remain literal native argv. In this raw form, model/effort only record
   the selection; they do not generate flags. No compiled adapter is required.
5. Keep the returned `call_id`; `status` observes its original Job and `cancel`
   requests its closure. `status` with an empty request lists the current Agent task receipts when a response was lost. A cancellation acknowledgment is not process-exit proof.

The current Agent's existing CLI authority and call budget still apply. Ordinary native calls and the original isolated team budget share a 32-call task-family ceiling; explicit additional isolated rounds retain their existing grant contract. Receipts
bind the authenticated Agent, task, request fingerprint and original Runner/Job.
Another Agent cannot read or replace them. A replay with different input is
rejected. Completed calls are not restarted, and unknown execution cannot prove
completion. The model-facing interface is one `cli` tool; `local_cli` is an
internal implementation module.

## Persistent machine configuration and updates

Built-in adapters describe protocols, not bundled model installations or accounts.
Optional machine-specific adapter definitions live in
`%LOCALAPPDATA%/webcodex/cli-adapters.json` on Windows, or the configured
`WEBCODEX_CLI_ADAPTERS_FILE`. Keep these definitions outside the application
installation and source checkout; they must not contain credentials. They extend
or replace protocol recipes without rebuilding WebCodex.

Discovery uses current installations and stable launcher paths. Native command
lookup includes Windows user PATH registrations and standard per-user binary
directories; installer symlinks can point to a new version without a new resource
approval. Explicit PATH/profile settings take precedence. A credential-free
system proxy is used per process when no explicit proxy override exists.

Codex and Claude adapters read their installed structured model catalogs. When
the catalog supplies reasoning choices per model, both selection and execution
validate that exact model's choices. Codex completion is its terminal protocol
event; an earlier error event cannot become success after a later completion.
For Claude recipes that disable user customizations, the Runner projects only
the installed CLI's account/gateway/model settings into that child process.
Explicit environment values win. These private values never enter discovery
responses or saved call receipts, and unrelated executables receive no mapping.

Missing installations, unsupported automatic metadata and expired accounts are
different states. For an adapter without automatic invocation metadata, use the
raw native-argument form after inspecting actual CLI help. Never invent options.

`process_success` is only the native process result. For adapters declaring a
result contract, `cli_success` additionally checks the structured terminal event;
provider errors, ambiguous/missing results and unavailable output do not become
success merely because exit code is zero. CodeBuddy JSON event arrays and single
terminal objects are both supported. Task acceptance still requires validating
its actual deliverables.

## Explicit isolated execution

Use `execution_mode:"isolated"` only for the existing isolated team workflow.
That mode retains frozen plan slots, reviewed resources, source-attempt receipts
and published artifact review. The `team_cli_*` compatibility tools retain the
same semantics. Their isolation approvals do not gate ordinary local calls.
