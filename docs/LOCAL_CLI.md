# Local installed CLI tool

`local_cli` is a task-owned interface to programs installed on the selected
Runner computer. It uses ordinary native process jobs and the Runner user's
existing environment and login. A fixed model-provider enum, a specific CLI
version, an executable SHA allowlist and an isolated file-worker approval are
not prerequisites for this tool.

This is trusted local-user execution. The selected project determines the
starting directory; it is not an OS filesystem sandbox. The CLI has the local
user's operating-system permissions. Existing explicitly isolated shell
environments and local Runner execution policy remain effective. This tool
does not silently change those settings or copy another user's login.

## Workflow

1. Read `agent_assignment.project_access` and use its exact project identifier.
2. Call `local_cli` with `action: "discover"`, that `project`, and a `commands`
   array of command names or absolute executable paths. Discovery does not start
   a program or verify the account. Its resolved executable path is diagnostic;
   prefer the installer's stable command/launcher when starting later jobs.
3. Inspect the CLI's actual version, help, model and reasoning options. Use its
   native argument syntax; different CLIs do not share a universal model flag.
4. Use `action: "start"` with `project`, `executable`, literal `args`, optional
   `stdin`, an explicit timeout and a stable `idempotency_key`. `model` and
   `effort` record the caller's choice; they do not generate command flags.
5. Keep the returned `call_id`. Use `observe` for output and status, or `stop`
   to request cancellation. A successful stop request is not process-exit proof.

No frozen team plan is required for an ordinary local CLI call. The current
Agent's existing CLI authority and call budget still apply. The server binds
receipts to its authenticated Agent and task, and records request fingerprints,
the selected Runner/project and the original durable Job. Another Agent cannot
read or replace those receipts. Reusing a key with different input is rejected;
completed calls are not restarted. Unknown execution keeps completion blocked
until authoritative Job evidence is obtained.

## Updates and failure reporting

Program discovery runs against the current installation. Windows discovery
includes the user's current PATH registrations and standard per-user executable
directories; installer launcher symlinks resolve to the current version. An
installer changing its executable bytes or version does not require recompiling
WebCodex or manufacturing a new isolation approval. Explicit PATH/profile
configuration takes precedence during execution. Credential-free system proxy
settings are inherited per process only when no explicit proxy override exists.

Missing programs are reported as missing; an older Runner reports that an update
is required. Discovery is not proof of a valid login or model entitlement.
`process_success` refers only to the process result. Provider error payloads,
the requested files and the task's acceptance conditions must also be checked;
exit code zero alone is not business-task success.

The existing `team_cli_*` isolated file-worker protocol remains available for
work that explicitly needs that execution mode. Its fixed adapter constraints
do not govern generic `local_cli` calls.
