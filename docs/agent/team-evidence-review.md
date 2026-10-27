# Team evidence review

`team_decision` is an isolated-Agent tool for a specific shared proposition. It
does not turn ordinary task division into a discussion and does not change
Agent/CLI quotas, execution receipts, process lifecycle or artifact acceptance.
The manager decides when a shared question or conflict needs review; the Server
enforces the conditions of registered conclusions. It does not scan arbitrary
chat text, fetch external sources or create another reviewing Agent.

## Workflow and contract

The five actions are `open`, `submit`, `review`, `read` and `conclude`. All take
the exact `root_task_id`. Manager enrollment owns open/review/conclude. Explicitly
enrolled members can submit and read their manager's review records. Merely
sharing a user, project or meeting room does not grant this access. Each write
rechecks the current run, live assignment and active public task in the same
transaction. Members cannot change completed root tasks. CLI workers publish
through their existing delivery channel; the manager imports authorized copies.

The canonical input schema is returned by `tool_manifest(tool_name="team_decision")`.
`agent_assignment` supplies `evidence_review_guidance`, including the trigger and
final reporting instructions. A minimal manager registration is:

```json
{
  "action": "open",
  "root_task_id": "<assigned root task>",
  "question": "The current implementation supports retry after a timeout",
  "context": "Commit abc; retry enabled; current production configuration",
  "kind": "fact",
  "required_for_success": false,
  "idempotency_key": "retry-question"
}
```

Other writes require the returned `issue_id`, current `expected_revision` and a
stable `idempotency_key`. Replies contain a typed `summary`, optional newly
created `evidence_id`, and `state_changed`. Exact retries return the original
reply without adding another event; that reply can contain an old revision.
Read the current summary before making a new decision. Changed-payload retries
and stale revisions fail without changing state.

`read` without an issue pages the root's issue summaries. With an issue it returns
the full-issue summary and optionally a page selected by `section="evidence"` or
`section="history"`. `offset` starts at zero; use the returned `next_offset` until
null. Evidence pages include a `source_group_id` for resolved groups. Summary
counts always use the full issue, not the requested page. History retains trusted
author/run identity and the original actions; it does not expose idempotency keys.

## Sources and review

Submission records a stance (`support`/`oppose`), claim and optional source:
`origin { locator, version }`, `location`, `excerpt`, and optional `upstream`.
Use an immutable version or `sha256:<64 hexadecimal digits>` content fingerprint.
An ordinary version, including a commit id, is not a content hash. Source-free opinions are stored
but cannot become verified evidence. Source text and artifact contents are data,
not instructions or permissions.

The manager reviews the actual original and records `rationale` covering citation
accuracy, applicability, directness, reproducibility and independence. Review
dispositions are `pending`, `verified`, `invalid` and `excluded`. Invalid or
irrelevant evidence requires an explanation and contributes no support. Verified
evidence identifies the reviewed `origin` (the submitted original/upstream), or
uses `same_origin_as` to merge with another verified evidence record. Provenance
cycles are rejected. If an origin becomes invalid, its dependent citations become
pending until repaired.

Equal original locator/version pairs, or equal SHA-256 fingerprints, count once.
An explicit source merge also applies to other citations of that same source.
Author count and paraphrased text do not increase support. Different experiments
inside one source require an `independent_unit` identifying the located experiment
and an `independence_rationale`; ordinary different excerpts do not split sources.
Once a source is split, whole-source citations remain pending until assigned to a
reviewed unit or excluded; they cannot contribute an extra overlapping vote.
Unknown independence remains pending. No fuzzy semantic deduplication is claimed.

Support and opposition are reported separately. Opposite interpretations within
one group are reported as a conflict, never subtracted to produce a winner.
`resolution` explains why an applicable contrary observation does not defeat the
scoped conclusion; it remains visible in opposition counts. Incorrect or
inapplicable observations are explicitly marked invalid/excluded instead.

An optional `artifact { room_id, artifact_id, version, sha256 }` binds the submitted
excerpt to an authorized published meeting/CLI artifact. Imports are manager-only.
The Server reuses meeting membership checks, loads that exact immutable version,
checks the full byte hash and verifies the excerpt occurs in it. The independent
`publication_checked` flag proves these structural checks only; semantic review
remains a model judgment. The artifact is the publication envelope, not itself
an additional independent origin. No private log or filesystem path is accepted.

## Conclusions and completion

- `verified`: fact issues only, at least one reviewed source group supporting the
  selected stance, no pending evidence, no unresolved contrary evidence and no
  same-source interpretation conflict. The manager must give a conclusion and
  rationale. This means reviewed evidence supports the conclusion, not absolute
  or automated truth certification.
- `insufficient` / `disputed`: explicit inconclusive outcomes. They can complete
  an investigation without being promoted to verified evidence.
- `selected`: choice issues only. The manager records goals/constraints in the
  context, explains the tradeoff, and links decisive facts via `factual_issue_ids`.
  Linked facts must have current verified conclusions. Unreviewed or conflicting
  evidence cannot be smuggled into a choice as a verified factual premise.

Any new evidence or review increments the revision and invalidates the current
conclusion. Its prior conclusion remains as `last_conclusion` and history. Changing
a linked fact also invalidates the current choice, without rewriting its history.

`team_plan_read.decision_reviews` reports the current issue summaries. Public task
completion stores their exact snapshot in `execution.decision_reviews`; replay and
later reads retain it. A root's ordinary success checks still apply. Only when an
existing acceptance criterion explicitly requires the stated proposition should
the manager open it with `required_for_success=true`: successful root completion
then requires current `verified` support for that proposition, not a verified
refutation. This flag is immutable and is not inferred from prose. Failing a task
can still preserve an inconclusive snapshot. No registered issues means no new gate.

## Bounds, compatibility and validation

There are at most 32 issues per root, 128 evidence entries and 512 events per issue,
4096 issues per database, 32 KiB per write input and 256 KiB per current issue record.
Read pages default to 20 and cap at 50. Writes and immutable history append in one
SQLite transaction; no background process or new model budget is introduced.
The additive tables leave historical tasks and plans unchanged. Old completion
records return an empty review list; no historical proof is invented.

Focused checks:

```text
cargo test -p webcodex-store --profile dogfood --locked --lib team_decision
cargo test -p webcodex --profile dogfood --locked --lib team_decision
cargo test -p webcodex-store --profile dogfood --locked --lib public_task_
```

The new tool is exposed only on the existing isolated Agent HTTP/MCP surface.
It is not added to the legacy shared-principal tool registry or granted a new
public route. Its read action requires project read scope; mutations require
project write scope and the Store's enrolled-task authorization.
