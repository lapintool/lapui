# Resumable application changes

`lapui.changes.subscribe(options)` returns one Promise for a bounded page or long-poll timeout. Rust embeddings use `ActionRegistry::subscribe_changes(ChangesRequest)`. The development TCP method is `changes.subscribe`; `describe` publishes its schema, limits and capability flags. All three observe the same application-owned history. This feed concerns application state and operations; it does not acknowledge a rendered frame or watch the DOM.

```js
// No cursor: obtain a consistent baseline and checkpoint without waiting.
let page = await lapui.changes.subscribe({scope: 'state'});
let state = page.baseline.state;
let version = page.baseline.version;
let cursor = page.cursor;

// Resume from the last completely applied page.
page = await lapui.changes.subscribe({scope: 'state', cursor, limit: 64, waitMs: 1000});
```

Options are `scope` (default `application`), `cursor` (string or null), `limit` (1–128, default 64), and `waitMs` (0–1,000, default 1,000). Unknown fields and invalid values are rejected. There is no persistent subscription handle: call again after applying a page, or stop calling to pause. An in-flight wait can finish after a frontend pauses; fence old callbacks with a generation number. `AbortSignal` is not accepted by this API.

| Scope | Baseline | Matching records |
| --- | --- | --- |
| `application` | `{observation, operations, traceSequence}` | All records below |
| `state` | `{version, state}` | `state_changed` |
| `actions` | `{version, actions, traceSequence}` | `action_registered`, `action_unregistered`, `action_finished` |
| `operations` | `{operations}` | `operation_changed`, `operation_removed` |
| `host` | `{snapshotAvailable:false}` | `host_event` |

These scopes select data; they are not authorization boundaries. State is the JSON explicitly placed in `ActionRegistry`. Native/private state is not exported automatically. Operation summaries contain ID, action, execution phase, revision and numeric progress; output, error details and progress messages are omitted. Action records reuse the finite action trace metadata and omit arguments/output. A host event exports exactly the name and JSON payload supplied by its Rust author:

```rust
actions.emit_event("files.selection_changed", serde_json::json!({"fileId":"file:17"}))?;
```

Custom events have no automatic state snapshot or transactional rollback. Supply an application query for rebuilding their meaning after a gap. The TCP adapter is an unauthenticated loopback development interface; do not treat a scope or cursor as permission to expose sensitive data.

Responses contain `{scope, cursor, resyncRequired, hasMore, records}` and optionally `baseline` and `resyncReason`. Each record is `{sequence, kind, data}`. Sequence numbers are ordered globally within one registry and can have gaps in a filtered scope. Cursors are opaque and tied to that registry and scope; changing scope needs a new baseline. An empty timeout page can advance past irrelevant records. Always use the returned cursor, rather than constructing one from a record sequence.

A successful write emits `state_changed` with `actionId`, optional `requestId`, `baseVersion`, `version`, `delta`, and `requiresSnapshot`. A `fields` delta replaces each complete top-level value in `set` and removes keys in `remove`; it does not recursively merge nested objects. A `replace` delta replaces the entire state. Check that `baseVersion` equals your cached version before applying. For JavaScript object state, define each own property explicitly when applying `set`, so JSON keys such as `__proto__` remain data. The `examples/changes-demo` example demonstrates this logic and pause/resume.

If `requiresSnapshot` is true or the base version does not match, request a fresh baseline by omitting the cursor. Do not acknowledge an unapplied delta. Reads, accepted jobs, failed writes and successful idempotent retries do not emit another state write. `action_finished` can still record a retry or failure. Operation events track acceptance, running/progress, cancellation and terminal revisions; `operation_removed` invalidates an evicted ID. Query `operation` for its detailed result and handle `unknown_operation` without automatically repeating external effects.

Resume requests return `resyncRequired:true`, a fresh scope baseline and a new checkpoint if the cursor is from another registry/scope, is ahead, or has fallen behind evicted history. Reasons are `cursor_context_changed`, `cursor_ahead`, `history_evicted`, or `sequence_exhausted`. Malformed cursors return `invalid_cursor`. The history is shared by scopes, so heavy traffic in another scope can also force a resync. A baseline is captured consistently with its checkpoint while state and operation snapshots are locked; later changes remain after that cursor.

```json
{"method":"changes.subscribe","scope":"state","waitMs":0}
{"method":"changes.subscribe","scope":"state","cursor":"<returned cursor>","limit":64,"waitMs":1000}
```

Use `lapui client <address> changes [cursor]` for the default application scope, or `request-file` for a JSON request with another scope. Disconnect and reconnect with the last fully applied cursor. A lost invocation response uses its original `requestId` for a bounded idempotent retry, separately from the change cursor. Full document reload preserves application history, state and running jobs; it replaces frontend callbacks and invalidates old control references. A process restart loses history and request deduplication. There is no durable event log or exactly-once side-effect recovery.

Limits are 512 retained records and 256 KiB of serialized journal JSON, 8 KiB per event, 4 KiB per state delta and 128 records per page. Larger deltas become snapshot-required markers. These bounds do not describe total heap allocation; snapshots and returned pages allocate separately. At most four requests may wait simultaneously per registry; additional waiting requests return `changes_busy`. Immediate pages do not reserve a waiter. Waits use notifications and a deadline rather than idle polling.

JavaScript waits share the two document host workers and 64-slot queue with other host calls; two outstanding waits can delay invocation until a wait ends. Prefer one combined feed. Document close skips queued work and discards late results, while a running wait ends within its one-second limit. TCP uses eight fixed workers and 64 queued connections; excess connections are closed with a best-effort `control_busy` response. Running native handlers are not preempted and can delay shutdown or other work. There are no DOM/control change streams, renderer frame events, access-control scopes or persisted subscriptions in this version.
