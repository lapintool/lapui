# Structured interaction

The development client exposes protocol discovery, application state, and action invocation without a screenshot or DOM scraping. Send a JSON line over the printed loopback address. Start with:

```json
{"method":"describe"}
```

`describe` reports protocol version and capability flags; unsupported operations are explicitly false. The TCP adapter exposes semantic controls, control mutations and script diagnostics through the document's UI thread. Registered Rust actions and bounded asynchronous operations share the application registry; see [Rust host actions](host-actions.md). [Change subscriptions](changes.md) provide bounded application-state, action, operation and explicit host events with scoped cursor recovery. DOM/control change streams and frame events remain unsupported.

Use `actions.list` for bounded metadata summaries, `actions.describe` for a selected schema and `actions.check` for a business availability check with actual arguments. JavaScript provides the corresponding Promise APIs under `lapui.actions`; see [action discovery](action-discovery.md). An earlier available result does not bypass invocation-time checks.

To inspect current state:

```json
{"method":"observe"}
```

The response contains a versioned observation and an action list. To execute the same action as the window button:

```json
{"method":"invoke","action":"counter.increment","args":{}}
```

Both paths invoke the same registered Rust handler. Its argument check and state transition are shared. The observation version increases after a successful write action; queries and accepted background jobs preserve it; an invalid action or unexpected argument returns a structured error.

The embedded JavaScript bridge also provides a read-only semantic control snapshot:

```js
lapui.controls()
// { documentEpoch, controls: [{ ref, id, tag, role, name, focused, enabled, type, value? }] }
```

It reports button, text input, textarea, select, link, checkbox, radio, slider, spinbutton, and explicit-ARIA roles. Names prefer basic `aria-labelledby` references, `aria-label`, associated explicit/implicit labels, control text/button value, then title; this is not the full accessible-name algorithm. Snapshots include focused, enabled, required, read-only, placeholder, and current checkbox/radio checked state. Text input values reflect Blitz's current editor value; password values are excluded. Each `ref` is a canonical node handle, so repeated queries for a live node return the same JavaScript wrapper even after the node is detached and reinserted. References contain the document ID and Blitz's versioned node ID. Each new document receives a different process-local `documentEpoch`; old references cannot alias a new document's nodes. References are not durable across process restarts or reloads.

Query controls over TCP with `{"method":"controls"}`. To mutate a control, use the exact `documentEpoch` and canonical `ref` from that response:

```json
{"method":"fill","documentEpoch":7,"ref":"node:7:4294967312","value":"Search text"}
```

The other mutation methods are `activate`, `focus`, and `check` (with a boolean `checked`). References in this example are illustrative; obtain actual values from `controls`. `describe` includes control method schemas, the required document precondition, and which methods support idempotency/version checks. `requestId` and `expectedVersion` apply to `invoke` only; control mutations reject these fields instead of silently ignoring them. Mutations reject a different document, detached/missing controls, disabled controls, and read-only fill targets. They use the same JavaScript event path as in-application `lapui` calls. Successful mutations return `status: "dispatched"` and a fresh control snapshot after synchronous handlers and a budgeted microtask checkpoint. This acknowledges event dispatch, not completion of asynchronous application work or a rendered frame; query application state separately.

To wait for a semantic control condition instead of guessing with a fixed delay, send `waitForControl`:

```json
{"method":"waitForControl","documentEpoch":7,"id":"scan-status","field":"name","contains":"扫描完成","timeoutMs":3000,"waitId":"scan-status-01"}
```

Use exactly one selector (`id` or a current `ref`) and one condition (`equals` or string-only `contains`). Supported fields are `value`, `checked`, `focused`, `enabled`, `name`, and `role`; `equals` must match the selected field's string or boolean type. Every wait requires a caller-chosen unique `waitId`, and at most four waits may be active per process. Cancel by ID on another connection:

```json
{"method":"cancelWait","waitId":"scan-status-01"}
```

The wait is bounded to four seconds and returns `status: "matched"` or `"timed_out"` with the last semantic control snapshot; cancellation returns `wait_cancelled`. Reload during the wait returns `stale_document`; a control that disappears returns `stale_reference`. `describe` publishes the schemas, `controlWaitMaximumMs`, and `activeWaitLimit`. This waits for the control snapshot only: it does not guarantee that style/layout ran, a frame was painted, or the operating system presented pixels. Use application operation waits for Rust jobs and the debug trace to inspect the separate render path.

When debug tracing is enabled, a control mutation response includes `debugTraceSequence`. Pass that sequence to `waitForRender` to await a frame whose recorded causes descend from that control request:

```json
{"method":"waitForRender","documentEpoch":7,"afterSequence":42,"timeoutMs":2000,"waitId":"render-01"}
```

The trace must remain enabled and retain the requested sequence. Use `cancelWait` with the same `waitId` to stop it. The result distinguishes `rendered`, `render_unavailable`, and `timed_out`. `rendered` means a causally linked frame's layout resolved and renderer call returned; `physicalPresentation` remains `unknown` because the current backends provide no OS presentation acknowledgement. This is a trace boundary, not proof that an arbitrary later async UI state has painted; first wait for the relevant semantic condition, then wait for a causally linked render if the caller has its trace root.

The CLI supports `client <address> controls`, `client <address> diagnostics`, and `client <address> request-file <request.json>` for arbitrary supported requests. Responses retain the existing `{ ok, observation }` or `{ ok: false, error }` envelope. Successful control dispatches include `dispatchErrors`, containing bounded diagnostics reported during that dispatch/checkpoint. Listener exceptions are reported without preventing later listeners or undoing already-applied effects, so a dispatched operation can carry errors. Requests are limited to 64 KiB and the document queue to 64 entries. A queued timeout cancels the command before dispatch; a timeout after dispatch reports `outcome_unknown`, requiring observation before retry. A controller for a closed document reports `document_closed`.

`lapui.fill(ref, value)` sets supported text-like input values or a single-select value and dispatches `input` and `change`; `lapui.check(ref, checked)` updates a checkbox or radio's current state, applies same-name/tree/form-owner radio exclusivity, and dispatches the same events. Both reject disabled controls including inherited disabled fieldsets; fill also rejects read-only targets; `check` also requires a boolean. `lapui.focus(ref)` focuses enabled interactive controls and rejects nodes hidden by `hidden`, `aria-hidden`, or computed CSS visibility/display state, while `element.focus()` and `element.blur()` dispatch focus/blur and bubbling focusin/focusout events through Blitz's focus state. Native `keydown`/`keyup` listeners receive `key`, `code`, location, modifier booleans, repeat/composition state, and text. For cancelable keys, `event.preventDefault()` reaches Blitz and suppresses its default editing/navigation behavior. Tab and Shift+Tab traverse focusable controls and produce the corresponding focus events. `lapui.activate(ref)` shares checkbox/radio click defaults, cancellation rollback, option selection, and label activation with JavaScript and native events. Space/Enter activate controls; radio arrows move within the form-scoped group; select arrows/Home/End update a single selection. These are prototype controls without full HTML constraints or platform IME behavior; see [forms](forms.md).

The bridge batches DOM text/style mutations with `lapui.batch(callback)`, so related presentation changes produce one Blitz mutation transaction. A small DOM subset supports `createElement`, text/comment nodes, `createDocumentFragment`, fragment insertion that moves its children into the target, shallow/deep `cloneNode`, child node collections, `append`/`prepend`/`replaceChildren`/`contains`, `innerHTML` fragment parsing/serialization, `appendChild`, `insertBefore`, `removeChild`, `remove`, node type/parent/sibling reads, attribute reads/writes/removal, `className`/`classList`, `textContent`/`nodeValue`, inline CSS property set/read/remove, and Blitz-backed `querySelector`/`querySelectorAll`. The parser and serializer reuse Blitz HTML support; serialization is intentionally small, and `innerHTML` insertion does not execute script elements. Inline CSS reads serialize Blitz's parsed declaration block; `setProperty` accepts the `important` priority. DOM listeners support capture, target and bubble phases through the document; capture-aware deduplication/removal, once, passive, handleEvent objects, propagation stops and JavaScript on-event properties are tested. Listener exceptions are logged and later listeners continue. `getPropertyPriority`, AbortSignal listener removal, full default actions, live collection identity, and the full browser event model remain unsupported. Only trusted application markup should be assigned to `innerHTML`.

The loopback JSON control endpoint accepts an optional idempotency key and a global version precondition:

```json
{"method":"invoke","action":"counter.increment","args":{},"requestId":"agent-turn-42","expectedVersion":3}
```

Repeating the same successful request within the in-memory cache (at most 1,024 entries and 4 MiB of serialized data) returns the original observation without applying the action twice. Reusing the key with a different action, arguments, or version precondition returns `request_id_conflict`. A precondition that does not match the current application revision returns `stale_state`. The cache is per process and is lost on restart; current revisions are global to the prototype state rather than scoped to individual entities.

For developer diagnostics, `lapui.diagnostics()` returns the last 128 reported errors as `{ sequence, source, phase, message }`. Startup load/evaluation failures, asynchronous module entry rejections, timer callback exceptions, native DOM event errors, external control handler errors and host-completion dispatch failures are included; stacks are retained when available. Sources and messages are bounded to 4 KiB and 16 KiB respectively. The TCP `diagnostics` method returns `{ documentEpoch, scriptStatus, errors }` from the same buffer. This is not a global unhandled-rejection monitor or debugger; see [JavaScript modules](modules.md) and [Task scheduling](scheduling.md).

See [Form interaction](forms.md) for shared click defaults, cancellation/rollback, labels, keyboard activation and the remaining form limitations.
