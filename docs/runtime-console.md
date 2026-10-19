# Runtime Console navigation

Open `/runtime` and connect with an existing runtime credential.

Runtime Overview projects the existing `runtime_status.connection_layers`, Job
reconciliation counters, and build compatibility into separate Server, Tunnel,
Runner, transport, Project, Job, and build diagnostics. States distinguish online,
offline, degraded, unknown, and unconfigured. The Server has no authoritative
external Tunnel health feed, so Tunnel remains **unknown**, even when the local
Server responds. A configured URL and a past successful tool call are not health
proof. Layer TTLs are enforced and the Console expires its snapshot after at most
65 seconds (earlier when a layer expires), counting from request start to include
handling and transit delay without comparing clocks across hosts. Failed refreshes
immediately mark retained observations as unknown. Summaries, Project cards,
Runner cards and recent Sessions share the freshness gate and label retained
values as historical until a verified refresh; language changes cannot revive them.
Project connectivity uses the newest matching Project observation from Overview
or `/projects`, each with its own request-start age and failure state. An unrelated
Overview refresh cannot renew an old Project, and project-scoped credentials can
refresh their connection facts without `runtime:read`. Session aggregates retain
their separate Overview freshness.
Console requests have a 35-second deadline and never automatically retry mutations.

The regular OpenAI Tunnel owner may reconnect only its own confirmed-exited
transport child, at most three times over that owner's lifetime. Each recovery
uses one 60-second deadline and re-runs the doctor, control-plane, and new-child
readiness checks. Its existing machine output channel invalidates cached readiness
while reconnecting; Desktop observes that same channel until verified readiness
returns or failure/shutdown is reported. Quick Share retains its existing
exit-on-tunnel-failure behavior. Shutdown cancels recovery. Recovery does not restart the Server
or Runner, replay a tool request, or resolve unknown Operation/Job outcomes;
those still require canonical reconciliation. A live tunnel daemon retains its
own network reconnect behavior; the owner does not kill it for a business failure.

The workspace start page offers **Find a project**, **Runtime overview**, and
**Durable Agents**. Press **Command+K** on macOS or **Ctrl+K** on Windows/Linux
while connected to open Projects & Sessions and focus project search. On narrow
screens this also opens the navigation drawer. These shortcuts navigate only;
they do not create Sessions or send messages.

- **Projects & Sessions** is the collaboration workspace. Select a Project and
  Workflow Session in the sidebar. Recent Sessions starts expanded and can be
  collapsed when more room is needed.
- **Context** opens the selected Session's context. **Overview** shows work,
  attention, validation, and model-reported progress directly. **Activity** shows
  retained events and the existing follow-latest control. **Details** shows
  identity, lifecycle, mode, timestamps, and workspace information.
- **Runtime & Agents** provides four separate destinations: **Overview**,
  **Runner fleet**, **Windows**, and **Durable Agents**. Selecting a destination
  shows its full content and updates the navigation highlight. Switching
  destinations keeps existing forms mounted so unsent input is retained.
- **Windows** is an observability view for ChatGPT/WebCodex call correlation. It
  lists hashed `ClientWindow` identities, current in-flight WebCodex requests,
  bounded durable call history, linked Workflow Sessions, and explicit recorder
  continuity gaps. It never shows the raw host window value, tool arguments or
  outputs, and it cannot observe model reasoning or determine whether a host UI
  is frozen. Window/Session links are many-to-many evidence only: they do not
  select a Workflow Session, grant Project authority, or make a Window an
  execution/continuity owner.

For ordinary non-streaming `tools/call`, Window activity can project three timing
facts from canonical adapter timestamps: `service_ms` is request-observed to
response-handoff time, `next_call_gap_ms` is response handoff to the next
meaningful same-Window/same-principal request, and `cycle_ms` is request start to
the next meaningful request start. The gap is explicitly outside-WebCodex time;
it may include network, host scheduling, model inference, user interaction, or
other unobservable work and is never presented as model think/reasoning time.
Status/discovery calls do not break the meaningful sequence. Overlapping calls
are classified as overlap rather than producing a negative serial gap. Streaming
handoff is not stream completion, and a Server restart loses process-local prior
completion state, so both cases leave ordinary next-call timing unavailable.
A meaningful request consumes its predecessor at arrival; cancellation and hard
timeout also break continuity until a later eligible response establishes a new
anchor.
Legacy ActionAudit `window_ended_at_ms` remains the audit-record boundary and is
not used as the HTTP response-handoff performance timestamp.

The context panel still adapts between a docked rail, popover, and mobile sheet.
Closing it leaves a labeled Context entry in the header. Context navigation uses
ordinary keyboard-focusable buttons, with the current choice announced as pressed.
Mobile operation navigation closes after selection and focuses the destination.

These are presentation changes. Workflow Session and durable Agent identities,
credential scopes, refresh behavior, and mutation handling keep their existing
contracts. Model-reported progress remains informational.

The Windows list and detail routes require `runtime:read`. Non-admin callers are
first principal-filtered and then re-projected through current canonical Project
authority, so revoked Project access cannot leave a Window timestamp, Session
count, gap count, timing projection, or direct-key existence oracle. Session detail itself keeps its
existing Project-read contract; without `runtime:read` it reports Window activity
as unavailable instead of elevating the whole Session read to a runtime-wide
permission requirement. Durable terminal history is backed by ActionAudit, while
currently-running requests are process-local and intentionally disappear on
Server restart. The dedicated Windows refresh is three seconds only while that
view is selected and the page is foregrounded; the normal Runtime Console refresh
cadence is unchanged.

Each selected Project/workspace has its own keyboard-accessible disclosure below
its Runner. Closing it hides that workspace's Sessions without clearing the
selected Session or composer; its preference survives refresh. Selecting another
workspace opens its Session list.

At widths of 1280px and above, Context docks beside a narrower conversation, with
more room for readable status and progress. Its Overview starts with the latest
retained Agent-authored message. Resolution text is also visible directly below
the original message, rather than only in a tooltip.

Project search continues to query authorized Projects by name, id, Runner, or
workspace path. Separate searches filter the loaded Sessions by title/id/lifecycle
and retained messages by body, resolution, id, or author Session. Match counts
refer only to loaded results; these controls do not search unretained history or
host transcripts and do not change the selected Session. Expand **Search retained
messages** above the board to filter messages. Its match count stays visible when
collapsed; closing the search keeps the current filter.

The Session board is not a mirrored host chat transcript. An observed ACK records
an explicit model-context acknowledgement, not a reply, read receipt, or completed
work. Model replies appear only when explicitly posted to the Session; reported
progress is separately labeled as informational. An empty latest-message card
means no Agent message exists in the currently retained window.
