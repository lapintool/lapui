# Architecture

The runtime connects five parts.

The default drawing path is Vello CPU/Softbuffer; the original GPU path is selectable with `--renderer gpu`. Offscreen PNG rendering is available for the current document state. See [Rendering and images](rendering.md).

1. Blitz parses `ui/index.html` and owns the document used by the renderer.
2. QuickJS-ng runs a small JavaScript bridge. Supported element operations call Rust methods that mutate the Blitz document.
3. `ActionRegistry` owns explicitly exposed JSON state and registered read/write/job handlers. Input/output schemas and business preconditions apply to both the window and structured client. Background jobs have separate bounded operation records.
4. Background workers send owned completion data back to the UI thread. The UI thread resolves the JavaScript Promise, processes microtasks, and redraws the document.

Application state, actions and jobs share a bounded change journal independent of document lifetime. Consistent baselines and scoped opaque cursors allow clients to resume after disconnect or a frontend reload; eviction produces an explicit resync instead of retaining unlimited events. Change waits run on existing host workers or the bounded TCP connection pool and wake through condition variables. They report backend changes, not frame presentation; see [change subscriptions](changes.md).
5. A bounded document-controller channel delivers semantic queries and control operations to that same UI thread. Module entries use an app-local loader and resume asynchronous top-level await through the completion path.

There is one window, one QuickJS runtime/context, and one Blitz document. The JavaScript bridge is intentionally narrow: it is an integration test of the execution model, not a browser DOM implementation. Blitz and QuickJS-ng versions are pinned through `Cargo.toml` and `Cargo.lock`.

Each document has a unique process-local ID. Canonical node references combine it with Blitz's versioned node ID, preventing references from another document from resolving accidentally. Closing a document cancels pending HTTP work, closes SSE streams and WebSockets (including a pending handshake), and stops its external notification worker. A replacement document has separate JS/module state and completion queues. The CLI supports manual replacement and native `--watch` notifications, with shared application state and a refreshed control endpoint; see [development](development.md).

The canonical-wrapper cache uses JavaScript `WeakRef`; listeners belong to the wrapper rather than a global strong map. Ownership links retain connected nodes and preserve a detached subtree while any of its nodes or style/class-list objects are held. These links are not a second authority for DOM reads or layout. QuickJS collects unreachable cycles, and finalization jobs free detached native trees through Blitz's mutator. Collection runs after a document turn that created wrappers or detached children, outside native DOM borrows and active event dispatch. A string node reference alone does not retain a node.

Fetch uses one lazily started document-owned reactor and reusable client, with eight active requests and 16 outstanding slots. Cancelled queued requests complete without waiting for an active slot. Completions hold their slot until UI delivery, bounding the number of buffered native fetch results. Closing or replacing a document interrupts the reactor's active tasks and discards queued work. See [Network requests](network.md) for body/input limits and cancellation semantics.

QuickJS uses a cooperative execution deadline and a 128 MiB managed-heap cap. An interrupted document suspends scripts until reload, retaining diagnostic/control reads while cancelling document-owned work. See [task scheduling](scheduling.md) for limits and native-call exceptions.

The local control listener is a development-only interface. It listens on `127.0.0.1` and accepts one JSON request per connection. Future work must add an explicit permission model before offering a production AI or automation endpoint.

WebSocket and SSE use a second lazy document-owned reactor with a shared eight-stream limit, bounded incoming delivery, and bounded WebSocket sends. UI dispatch converts owned native fields and binary buffers directly into JS values. Stream callbacks run without holding the queue borrow, allowing close/send/replacement construction. See [Network requests](network.md) for the exact transport budgets, backpressure, shutdown, and native allocation limits.

The CLI uses LapuiApplication around Blitz for actual-redraw animation callbacks and sleeping event-loop deadlines. CSS-pixel measurements and immediate JS scroll commands read/modify the same native layout and offsets used by the renderer. They commit pending batch writes when queried; callbacks and returned measurements do not acknowledge screen presentation. See [scheduling](scheduling.md) and [geometry](geometry.md).

At the same paced rendering opportunity, native box observations and window/scroll notifications run after animation callbacks and before painting. Size entries sample Blitz layout; active observations own their targets, while native scroll sampling uses weak target references. Close/reload retires the document context. See [size observations and window events](observers.md) for budgets and lifecycle rules.
