# State Ownership Contract

This contract describes the current implementation and constrains refactors.
[中文](STATE_OWNERSHIP.zh-CN.md). Related contracts:
[Sessions](agent/session-model.md), [permissions](agent/permission-model.md),
[durable Agents](architecture/durable-agent-conversation.md).

## Rules and vocabulary

**Durable (D)** means successfully persisted to the configured store, not that
every in-memory update survives a crash. **Ephemeral (E)** means process-local.
**Reconstructable (R)** means a projection can be rebuilt from its named source,
never that execution can be replayed. **Observation-only (O)** means no execution
authority. **Authority-bearing (A)** is always limited to the stated fact.
These labels are orthogonal: a durable audit is still observation-only.

Only the named writer may change a fact, through its existing validated API.
All other components are observers. Shared SQLite handles do not merge domains.
The Kernel owns admission (auth/scope, Session, Goal, permission, Project) and
hands typed calls to business handlers. Services do not introduce another gate.
The Runner still enforces its local policy, paths and process ownership.

In the tables, S/R/H mean Server restart / Runner restart / Chat handoff.
“Yes” assumes the same storage and identity scope and successful persistence;
retention and explicit deletion still apply. “Reconcile” never means retry.

## Execution and control authority

| Fact; class | Sole authority and writer | S / R / H | Admission, derivation and conflict rule |
| --- | --- | --- | --- |
| Host Goal phase; D,A | Host Goal SQLite store; validated Host Goal transitions | Yes / Yes / Yes | Gates allowed work. Never derive completion from chat, Session or provider end-turn; committed Goal row wins. |
| Current plan version and approval; D,A | Host Goal store; plan save/confirm transitions | Yes / Yes / Yes | Gates implementation. Files/Markdown are materializations, not approval; store version wins. |
| Current node; D,A | Host Goal store; node claim/check/checkpoint transitions | Yes / Yes / Yes | Gates node effects; a Session task title cannot select a node. |
| Current attempt; D,A | Host Goal store; claim/check and attempt transitions; bootstrap only observes it | Yes / Yes / History preserved; continuing business execution requires a fresh attempt after retirement | Exact tuple required; old attempt evidence never reactivates execution. |
| Execution fence; D,A | Host Goal store; atomic attempt allocation | Yes / Yes / Old fence retained as history; new claim allocates new fence | Stale fence loses even if Runner/Session appears current. |
| Context ACTIVE/DRAINING/RETIRED and identity source; D,A | Host context SQLite state; trusted identity binding and retirement transitions | Yes / Yes / Retirement persists | DRAINING only admits existing allowed completion/reconciliation; RETIRED cannot regain business execution. Caller metadata cannot replace trusted host identity. |
| Operation intent, dispatch state, receipt and completion; D,A | Host Goal Operation store; admission plus authoritative reconciliation | Yes / Yes / Yes | Receipt bound to exact operation/attempt/fence. A recorded invocation or timeout is not success. Unknown remains unknown until proven by receipt/Job/Run or explicit supported resolution. |
| Actual ordinary Job execution; E + D/R registry projection, A | Runner JobManager for process state; RunnerRegistry validates instance, connection, sequence and lifecycle updates | Reconcile same live Runner / ordinary process ownership lost; fence/recover/lost rules apply / Yes, without granting old Chat authority | Persisted server Job records are recovery state, not proof of current liveness. Matching current-instance inventory wins; stale instances/updates cannot revive terminal state. |
| Detached Job; D + live supervisor, A | Detached supervisor and durable Runner detached store; JobManager reconciles | Reconcile / durable supervisor receipt reconciliation / Yes | Unlike ordinary jobs, may survive Runner exit. Never restart from a Session event or absence of a response. |
| Coding Agent Run; D + E live provider, A | Runner coding-agent manager/provider Run state and persisted run records; Server validates bound inventory/receipts | Reconcile / recover durable terminal or interrupted/unknown state, never assume live / Yes | Provider instance and run identity matter. Server run binding/cache is not process authority. Durable observation MAC supports lookup; token is not permission. |
| Runner instance/connection lease and capabilities; E,R,A for routing | RunnerRegistry accepts authenticated registration and owns lease replacement | Re-register / new instance / unaffected | Accepted protocol/capabilities govern routing. Old connection teardown/results lose to current lease; health UI cannot grant capability. |
| Persistent shell; E,A | Runner/local persistent-shell manager; Server SessionShellRegistry owns routing binding | Server binding lost / shell ownership lost / existing shell state does not grant new context rights | CWD/resource binding frozen at open. Session context cannot retarget an existing shell. |
| Goal loop worker; E | GoalController owns live workers, Goal SQLite store owns durable loop state | Worker lost; recover from store / reconcile Run / unaffected | A worker map entry is local liveness only; never overrides durable Goal completion. |

Implementation anchors: `webcodex-store/src/host_goal*.rs`, `goal.rs`;
`tool_runtime/host_goal*.rs`, `goal.rs`, `coding_agent.rs`;
`webcodex-runner-registry/src/{reconciliation,job_updates,jobs}.rs`;
Runner JobManager, `webcodex_runner/{detached_job,coding_agent,persistent_shell}`.

## Continuity, evidence and domain records

| Fact; class | Sole authority and writer | S / R / H | Admission, derivation and conflict rule |
| --- | --- | --- | --- |
| Workflow Session history; D when ledger configured, otherwise E; A for history | Workflow Session store and version-2 JSON ledger; canonical recorders and Session APIs | Persisted bounded history / Yes / Yes with explicit authorization | Ledger records what was observed, not actual process success. Recording Session and business Session authorize independently; no inference from transport/window. |
| Session guards, project and lifecycle; D/E,A | Workflow Session store; Session create/update/close APIs | As ledger / Yes / Yes | Kernel checks exact target, scope, active lifecycle and guards; current store wins over caller context. |
| Session messages, assignment fence, attention/resolution; D/E,A for message facts | Workflow Session message APIs and atomic assignment transitions | As ledger / Yes / Yes | Assignment fence is separate from Goal fence. ACK is request-scoped remembered-context evidence; never durable resolution or execution permission. |
| Session context revision; D/E,A for continuity watermark | Session recorder allocates checkpoint revisions | As ledger / Yes / Yes | ACK does not prove delivery or grant execution. Missing/retention gaps use bounded recovery, not invented history. |
| Host Goal handoff checkpoint; D,A | Host Goal checkpoint transaction plus context retirement | Yes / Yes / Yes | Canonical checkpoint wins over handoff files/summary. Retirement and claim rules remain authoritative even if files are absent. |
| Workspace checkpoint; D | Runner project snapshot and checkpoint files; workspace checkpoint APIs; Server metadata store for local path | Yes / Yes / Yes | Separate filesystem restore artifact, not Host Goal phase/fence/checkpoint. Exact snapshot/path checks govern restore. |
| Tool execution evidence; D/E,O | Canonical Session recorder records bounded/redacted start/result pairs | As ledger / Yes / Yes | “run_process called” does not imply success. Authoritative operation receipt or Job/Run result decides execution. |
| Validation evidence; D/E,O, input to acceptance | Validation domain interprets structured command/Job results; Session stores evidence and reconciliation markers | As ledger / reconcile Job / Yes | Validation owns assertion meaning/freshness; Job owns actual execution. Terminal snapshot lock preserves marker order. Successful check does not itself advance Goal without validated transition. |
| Project filesystem and project registry; D,A for content/path | Runner-owned project and filesystem tools, including user/external edits | Yes / Yes / Yes | Canonical root/path policy and current filesystem win. Server inventories and cached hashes require freshness checks; no inferred write authority. |
| Memory; D,A for memory records | Server SQLite Memory domain; scoped Memory APIs | Yes / Yes / Yes | Memory is reference context, never execution permission or Goal completion. Revision checks win over stale model copies. |
| Agent identity, Conversation, message, Wake, Agent Task/Attempt; D,A in their domain | Server SQLite Communication/Agent Task APIs | Yes / Yes / Yes | Independent of Workflow Session and Host Goal. Endpoint generation, task fence and idempotency remain domain-specific. |
| Host continuation endpoint/controller; E,A for current delivery binding | AgentContinuationController created with Communication DB | Lost / unaffected / requires current binding | Durable Agent identity survives; a process-local endpoint does not. No inference of endpoint liveness from durable messages. |

Session durability includes asynchronous ledger writes. Ordinary queued updates
can be lost before flush; APIs requiring durable acknowledgement use their
existing durable-write path. Retention/cold restore obey the current v2 shape.
This refactor does not strengthen persistence promises or migrate old formats.

## Gateways, configuration and observations

| Fact; class | Sole authority and writer | S / R / H | Admission, derivation and conflict rule |
| --- | --- | --- | --- |
| Plugin schema; E,R,O | Current provider describe observation, validated by Plugin Gateway | Re-describe / invalidate provider instance / only matching binding usable | Revision mismatch fails closed. Schema describes allowed call shape/effect; does not prove execution. |
| Plugin binding; E, process-local, A for call binding only | Plugin Gateway binding store after validated describe | Lost / old provider binding invalid / scoped binding rules remain | Not an execution receipt. Re-describe after schema/instance change; never derive from Audit. |
| MCP schema observation; E,R,O | MCP Gateway observation keyed to current provider/caller | Lost / re-observe provider / scoped rules remain | Describe → validated call; changed schema fails closed until re-describe. |
| SSH resource binding; E,A for exact resource selection | SSH Gateway validated describe/binding | Lost / revalidate Runner identity/config / scoped rules remain | No credentials copied into Session. Routing binding is not completed-operation evidence. |
| Runtime config, limits, exposure; E,R | Composition startup/builders and validated Runner config reload | Reload source / reload source / unchanged | Kernel uses current exposure/policy; reported capability/config summaries are observations, not override inputs. |
| Permission decision; E,A for this invocation; recorded copy O | Single canonical PermissionEvaluator plus hard security gates | Re-evaluate / re-evaluate / re-evaluate | Historical auto-approval cannot authorize a later call. Deny/hard boundary wins over soft decision. |
| Git continuation MAC; E,A for cursor integrity | Runtime-created key and Git cursor validator | Invalidated / unchanged / unchanged scope checks | Cursor integrity is not project permission or snapshot freshness. |
| Window activity and loop timing; E plus durable audit projection,R,O | Runtime WindowActivityRegistry and ActionAudit projection | Live view reset; retained audit survives / unaffected / historical correlation retained | Never infers Session, context authority, model turn or execution lease. |
| Runtime connection/activity observations; E,R,O | RuntimeObservations and injected activity recorder | Memory reset; durable sink history retained / new observations / history only | Last successful call/connection is not current execution authority. |
| Metrics; O | Metrics sink consumes facts, fail-open | Depends on sink / unaffected / unaffected | No authority, retry or persistence decisions. |
| Audit / Action Audit Session; D,O | Authenticated request/audit recorder in Server SQLite | Yes / Yes / Yes | Historical evidence only. Audit Session is not Workflow Session and cannot authorize or reconcile an effect by itself. |

## Conflict and recovery procedure

1. Identify the exact fact and its authority above; do not vote between caches.
2. Validate current identity, project, context, attempt/fence, Runner/provider
   instance, schema revision and sequence wherever the existing domain requires.
3. Read the authoritative receipt/state. Preserve separate `NotStarted`,
   `OutcomeUnknown`, `Running`, `Succeeded`, `Failed`, `Cancelled` meanings.
   Wire-specific lifecycle spellings remain unchanged.
4. After dispatch with a lost response, reconcile the original operation/Job/Run.
   Never automatically replay a potentially consequential call. Absence from a
   retained cache is not proof of NotStarted.
5. Handoff retires the old context durably. A new context bootstraps the original
   Goal and continues from its actual phase. If business execution is needed,
   read node requirements and reconcile original operations before claiming a
   new attempt/fence under the existing contract. An unchanged confirmed plan
   needs no repeat confirmation; completed Goals do not need another claim.
   Old receipts remain historical evidence.
   Reconnect, Session resume, provider re-describe and a copied epoch cannot undo
   retirement.

Unknown or conflicting evidence stays unknown/fails closed according to the
existing domain policy. Observation projections may be rebuilt or dropped; none
may fabricate execution facts to make recovery look complete.
