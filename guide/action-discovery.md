# Action discovery, availability and lifetime

The action catalog gives AI clients small metadata pages, full schemas on demand, and parameterized business checks. JavaScript, Rust embeddings and the loopback TCP adapter use the same registry. JavaScript registration remains unsupported; the APIs below discover Rust handlers. These are application capabilities, separate from DOM control references.

| TCP method | JavaScript Promise | Rust method |
| --- | --- | --- |
| `actions.list` | `lapui.actions.list(options)` | `list_actions(ActionsRequest)` |
| `actions.describe` | `lapui.actions.describe(actionId)` | `describe_action(actionId)` |
| `actions.check` | `lapui.actions.check(actionId, args)` | `check_action(actionId, &args)` |

Start with a summary page, retrieve the needed schema, supply valid arguments, check business conditions, then invoke the shared handler:

```js
const page = await lapui.actions.list({prefix:'files.', limit:16});
const action = await lapui.actions.describe('files.rename');
// action.input_schema and output_schema retain the existing ActionInfo names.
const args = {fileId:'file-1', name:'Report.md', expectedFileVersion:1};
const check = await lapui.actions.check(action.id, args);
if (check.available) {
  await lapui.invoke(action.id, args, {requestId:'rename-42'});
} else {
  console.log(check.reason.code, check.reason.message);
}
```

`list` accepts `prefix` (default empty), optional `scope`, optional `cursor`, and `limit` (1–64, default 32). Strings are limited to 128 UTF-8 bytes, except the opaque cursor's 1,024-byte cap. Unknown fields are rejected. Results are `{revision, items, hasMore, nextCursor?}`; summaries contain `id`, `description`, `kind`, `hasAvailabilityCheck`, and optional `scope`/`scopeName`. They omit input/output schemas and do not execute predicates across the catalog. A missing predicate does not skip argument validation or the handler's own business rules.

Results are sorted by action ID. Resume with `nextCursor` and the same filters; the page size may change. Cursors bind the registry, filters and action-catalog revision. Registering or retiring an action invalidates an outstanding page cursor with `stale_action_cursor`; restart without a cursor. State writes and job progress do not invalidate catalog pages. `revision` is the catalog revision, distinct from the application's state version. Scope IDs are process-local selectors, not access-control tokens or durable entity IDs.

`describe` returns full `ActionInfo` and scope/check metadata. `check` validates the action's input schema and returns `{actionId, version, available, reason?}`. `version` is the state snapshot used for that check. Invalid arguments and unknown actions return errors; a valid request blocked by an application predicate returns `available:false` with an `ActionError` reason. For the file fixture, reasons include `target_missing`, `invalid_name`, `name_conflict` and `stale_entity`. The same validator runs when the rename handler executes, preserving the human draft-conflict behavior.

Add a predicate through `ActionOptions::default().with_availability(check)`. Use `register_with_options`, `register_query_with_options` or `register_operation_with_options` on the registry; scoped registration methods take options directly. The callback receives immutable state and validated arguments:

```rust
use lapui::action::ActionError;
use lapui::action_catalog::ActionOptions;

let options = ActionOptions::default().with_availability(|state, _args| {
    if state["locked"] == true {
        Err(ActionError::new("locked", "Unlock the workspace before editing"))
    } else {
        Ok(())
    }
});
// actions.register_with_options(info, options, existing_handler)?;
```

Checks must be fast, read-only and use the supplied snapshot/arguments. They run outside the state mutex under the same execution ordering as transactions, so they may observe the registry but cannot recursively check/invoke its actions (`reentrant_action`). Fresh invocations validate arguments/version and rerun the predicate immediately before their handler. A previous `available:true` is advisory; state can change before invocation. External authorization and conditions that live outside the registry still belong in the handler under their own state owner's synchronization. Predicate panics are caught as `availability_panicked`; error codes/messages are bounded to 128 bytes/16 KiB.

Successful idempotent retries return the previously recorded outcome without rerunning a predicate or handler, even if the current state is blocked or the old registration has retired. They confirm a past request. Request-cache eviction/restart loses this protection; never interpret an unknown retired action as permission to repeat external effects. Job retries retain their original operation ID and kind even if a later action reuses the name.

`ActionRegistry::create_scope(name)` returns an owning, non-cloneable `ActionScope`. Keep it alive while its actions are needed. Its `register`, `register_query` and `register_operation` methods use the same schema/availability/handler rules. `scope.id()` selects its catalog entries; `scope.close()` or dropping the owner removes all its registrations, releases their metadata quota and publishes `action_unregistered` events. Close is idempotent, scope identities are never reused within a registry, and a closed owner cannot register again. The scope holds a weak registry reference and does not keep a dropped application alive. Retired handler captures are dropped outside the registry's state lock.

```rust
let editor = actions.create_scope("editor")?;
editor.register(info, options, existing_handler)?;
// Register before loading scripts if those scripts need the action at startup.
let (mut document, notify) = lapui::runtime::LapuiDocument::new_with_source(
    actions.clone(), None, html, script,
)?;
document.attach_action_scope(editor)?;
```

`attach_action_scope` transfers a pre-registered scope into a document; it accepts only live scopes from that document's registry. It consumes the supplied owner, including on error. `document.create_action_scope(name)` instead creates and retains one after construction. Dropping or successfully replacing that document retires these scoped actions; application registrations made directly on the registry survive. A failed source read retains the old document and its scopes. A scope can also be owned by a Rust dialog/tool component independently of a document.

Retirement prevents future admission. Calls still queued behind another transaction subsequently reject an unknown action. A call that already selected its handler may complete and commit; accepted background jobs keep their application lifetime and explicit cancellation API. Retirement does not undo effects or force native code to stop. Re-register a same-name action only after its previous registration has retired; callers should use new request IDs for new work.

The registry permits 256 actions, 256 live scopes and 1 MiB of serialized action metadata; scope names are at most 128 bytes. JavaScript catalog calls share the two document host workers and 64 queue slots, avoiding predicate execution on the UI thread. A running native predicate cannot be preempted and can delay other host work. Discovery is not an authorization boundary. Domain entities/projections, authenticated caller identities and JavaScript handler registration are separate unfinished capabilities.

Raw requests for the embedded file tool:

```json
{"method":"actions.list","prefix":"files.","limit":16}
{"method":"actions.describe","action":"files.rename"}
{"method":"actions.check","action":"files.rename","args":{"fileId":"file-1","name":"Report.md","expectedFileVersion":1}}
```

The CLI supports `lapui client <address> actions`, `describe-action <id>`, and `request-file` for filters/check arguments. `describe` publishes `actionCatalogSchemas`, `actionCatalogLimits` and the implemented discovery/availability/scoped-action capability flags. [Change subscriptions](changes.md) report registration retirement, actions and application state separately from renderer frames.
