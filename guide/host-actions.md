# Rust host actions and background operations

Create an `ActionRegistry` from the JSON state you intend to expose, register handlers, then pass a clone to `LapuiDocument`. JavaScript and the development TCP adapter invoke the same registry. State is application scoped, separate from transient document node references. Registration is a Rust embedding API; JavaScript action registration is not implemented.

[Action discovery and lifetime](action-discovery.md) adds bounded metadata pages, full schemas on demand, parameterized availability predicates and Rust-owned scopes that retire their actions on close or document replacement. These use the same handlers and invocation checks as the APIs below.

```rust
use lapui::action::{ActionInfo, ActionKind, ActionRegistry};
use serde_json::json;

let actions = ActionRegistry::new(json!({"name":"Lapui"}))?;
actions.register(ActionInfo {
    id: "profile.rename".into(),
    description: "Change the display name".into(),
    input_schema: json!({
        "type":"object", "required":["name"], "additionalProperties":false,
        "properties":{"name":{"type":"string", "minLength":1, "maxLength":80}}
    }),
    output_schema: json!({"type":"string"}),
    kind: ActionKind::Write,
}, |state, args| {
    state["name"] = args["name"].clone();
    Ok(args["name"].clone())
})?;
```

`register` installs a write transaction. It validates arguments and optional global `expectedVersion`, runs the handler against a private state copy, validates output, then commits and increments the revision. Handler errors, panics, invalid output and oversized state leave the previous state intact. Invocation returns `{ version, state, actions, result, count }`; `count` is a legacy counter projection. `observe()` omits `result`. `register_query` receives immutable state and returns output without increasing its revision. Each registration method sets the discovered action `kind` to `write`, `read`, or `operation`.

Handlers serialize through one execution lock. They may observe the registry; recursive invocation returns `reentrant_action`. Do not wait for another thread to invoke this registry or perform long blocking work in a transaction. Rollback applies only to the JSON state copy and cannot reverse filesystem/network effects. Avoid strong registry captures in registered closures, which create reference cycles. Authorization and entity preconditions belong in handlers and must be checked at invocation time.

The schema subset supports string `type` (`object`, `array`, `string`, `integer`, `number`, `boolean`, `null`), `title`, `description`, `const`, `enum`, `properties`, `required`, boolean `additionalProperties`, `items`, `minItems`/`maxItems`, `minLength`/`maxLength`, and `minimum`/`maximum`. Type-specific keywords require that declared type. Schemas are objects, at most 64 KiB and 16 nesting levels. Unsupported keywords—including `$ref`, composition, formats and patterns—are rejected at registration. String lengths count Unicode scalar values. Integer bounds retain 64-bit precision; JavaScript versions must be safe integers. This is an explicit subset, not general JSON Schema support.

Limits are 256 actions, 1 MiB of serialized action metadata, 1 MiB of state, and 64 KiB of arguments/output. Successful request deduplication retains at most 1,024 entries and 4 MiB of serialized cache data, evicting oldest entries. Repeated keys return the original result even after the current revision advances; changed action/arguments/version conditions return `request_id_conflict`. Eviction and restart lose this protection; there is no durable exactly-once guarantee. JavaScript invocation options are limited to 1 KiB. Blocking work uses two document-owned workers and 64 queue slots; overload returns `host_busy`. Closing a document skips work that has not started. Running transactions can still finish.

```js
const before = lapui.observe();
const changed = await lapui.invoke('profile.rename', {name:'新名称'}, {
  requestId:'rename-42', expectedVersion:before.version
});
console.log(changed.result, changed.state);
```

Use `register_operation` for longer jobs. Its handler receives a state snapshot, arguments and an `OperationContext`. `report(progress, message)` publishes progress; `checkpoint()` and `delay(duration)` support cooperative cancellation. Returned output is validated against the output schema. Jobs do not implicitly commit state. Apply results through a separate version-checked write action when needed.

Invocation returns `result: { operationId, execution: "accepted" }`. Acceptance does not guarantee completion or frame presentation:

```js
const accepted = await lapui.invoke('files.scan', {}, {requestId:'scan-42'});
let job = lapui.operation(accepted.result.operationId);
while (!['completed', 'failed', 'cancelled'].includes(job.execution)) {
  job = await lapui.waitOperation(job.operationId, job.revision);
}
// job.output is present on completion; job.error on failure.
```

`waitOperation` waits off the UI thread until the revision changes, the job finishes, or one second expires. Timeout returns an unchanged snapshot; there is no fixed runtime polling loop. `cancelOperation(id)` reports `cancel_requested` until the worker exits. Cancellation after a terminal result preserves that result. Jobs must cooperate; cancellation cannot reverse external effects. Jobs are application scoped and can outlive a document. Concurrency is capped at eight and retained records at 128; the oldest terminal record is evicted when needed. Progress messages are capped at 1 KiB, output at 64 KiB. IDs/results are process local; `unknown_operation` must not trigger automatic repetition of effects.

TCP uses the same IDs:

```json
{"method":"operation","operationId":"operation:1","afterRevision":2}
{"method":"cancelOperation","operationId":"operation:1"}
{"method":"trace","afterSequence":0}
```

`operation` waits up to one second only with `afterRevision`. The TCP adapter has eight fixed connection workers and 64 queue slots, allowing waits and other clients to run concurrently within those limits. `trace` and `lapui.trace()` return at most 256 records with sequence, action, optional request/operation IDs, outcome, application revision and execution duration. Arguments, output and state are omitted. Retries are marked `replayed`; operation invocation traces record acceptance. Query the operation for its final outcome. Stale/future trace cursors set `resyncRequired`; observe state and continue from `nextSequence`. [Change subscriptions](changes.md) add resumable application deltas and operation summaries. Renderer frame tracking and durable recovery remain unsupported.

Try `cargo run --release --locked -- --demo files`. The in-memory file tool demonstrates pagination, entity versions, progress/cancellation and preserved human drafts when an AI write conflicts. It does not read or rename disk files.

For a real local directory, run `cargo run --release --locked -- --demo local-files --directory <path>`. This mode indexes only top-level regular file names and sizes and never reads file contents. The UI and MCP action catalog share `local_files.query`, `local_files.refresh`, and `local_files.metadata.update`; the last action changes only app-owned process-local notes with an entity-version check. It cannot rename or delete disk files. Notes reset when the process exits.
