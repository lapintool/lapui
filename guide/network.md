# Network requests

The runtime currently provides an initial asynchronous `fetch` implementation for absolute `http://` and `https://` URLs:

```js
const response = await fetch('http://127.0.0.1:8080/api/items', {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ query: 'lapui' })
});
const items = await response.json();
```

The first fetch starts one document-owned Tokio reactor thread and a reusable HTTP client. It runs at most eight requests at once, with at most 16 outstanding requests including queued work and results awaiting UI delivery. Excess requests reject with `code: "network_busy"`; encoded URL/method/headers/body input over 64 KiB rejects with `code: "invalid_arguments"`. Completed native responses retain their request slot until the UI consumes the completion. No thread is started per request, and waiting for network I/O does not block the UI event loop. App-local file reads are bounded synchronous reads on that reactor; slow storage can delay other network work.

Responses expose `status`, `ok`, `url`, iterable `headers`, `bodyUsed`, `text()`, `json()`, and `clone()`. Reading consumes a body once, including a failed JSON parse. Clone before consumption to read an independent copy. Non-2xx HTTP statuses resolve to a response; transport failures reject with `code: "network_error"`. Request headers accept a dictionary, `[name, value]` pairs, or a `Headers` instance. The basic `Headers` collection supports case-insensitive get/has/set/append/delete, iteration, and forEach; browser header guards and special Set-Cookie behavior are not implemented.

When the app starts from `--html`, relative `fetch()` URLs resolve under the local application directory. Local-file fetches are GET-only, restricted to canonical paths under that directory, and limited to 8 MiB. Basic content types are inferred from the file extension. This path supports local JSON/text resources; it is not a general-purpose file API.

Rust embedders can use `LapuiDocument::new_with_http_base(actions, proxy, html, script, "http://127.0.0.1:8080/ui/index.html")` to resolve relative fetch URLs against an explicit HTTP(S) document base. This constructor runs the supplied markup and script; it does not fetch the page or enable remote JS modules. The embedded CLI counter has no HTTP base, and `--html` continues to use app-local file resolution. Changing a `<base>` element after construction does not update fetch resolution.

The current implementation has a 20-second request timeout, follows at most five redirects, and limits response bodies to 8 MiB, checking both declared lengths and received chunks. It buffers the full response before resolving fetch. Streaming bodies, binary responses/requests, credential/cookie policy, and browser cache semantics are not implemented yet.

Use `AbortController` to cancel a queued or active fetch, including a stalled response body:

```js
const controller = new AbortController();
const loading = fetch('http://127.0.0.1:8080/api/items', {
  signal: controller.signal
});
controller.abort(); // loading rejects with an AbortError DOMException
```

Signals expose `aborted`, `reason`, `throwIfAborted()`, abort listeners, and `onabort`; `AbortSignal.abort(reason)` and `AbortSignal.timeout(milliseconds)` are available. A supplied reason is preserved as the rejection value. Listener exceptions are diagnosed and other listeners continue. Queued cancellation completes even while all eight network slots are occupied. Active cancellation drops the transport future; completed or cancelled native work releases its outstanding slot when the UI receives the completion. The server might already have applied a side effect: abort does not roll it back. Cancelling a buffered local-file read cannot interrupt an OS read already in progress. `AbortSignal.any()`, full EventTarget behavior, DOM listener `{signal}` options, and browser-complete timeout scheduling are not implemented.

Text and binary WebSocket frames are supported over `ws://` and `wss://`:

```js
const socket = new WebSocket('ws://127.0.0.1:9000/events');
socket.addEventListener('open', () => socket.send('ready'));
socket.addEventListener('message', event => console.log(event.data));
```

The basic API includes open/message/error/close callbacks, event listeners, `bufferedAmount`, and `close()`. Send strings, ArrayBuffers, typed arrays, or DataViews; a view sends only its byte range. Binary input/output uses buffer copies rather than JSON number arrays; incoming buffers are charged to QuickJS's managed heap. Binary messages are exposed as `Uint8Array`. Both individual frames and assembled messages are limited to 8 MiB. A declared oversized frame is rejected before waiting for its payload, with `error.code: "message_too_large"` and a local close event code of 1009. This local code does not promise that a close handshake reached the peer. Connections time out after 20 seconds. Protocol extensions, compression, subprotocol negotiation, and browser-compatible `Blob` behavior are not implemented.

`send()` throws with `code: "network_busy"` when its native queue or document byte budget is full, or `code: "message_too_large"` for an oversized message. Rejected sends do not close an otherwise open connection; callers decide how to wait, retry, or report pressure. Each socket has 16 queued sends, plus at most one currently flushing. Pending and flushing sends across the document share a 16 MiB byte budget (empty messages reserve one byte). `bufferedAmount` counts pending/in-flight payload bytes, excluding frame overhead and OS buffers; it reaches zero when the native send future finishes, not when the peer processes the message.

Closing during a handshake interrupts the connection attempt. Closing an open socket rejects new sends, then gives previously accepted sends and the close frame a shared one-second flush deadline. This preserves send-then-close ordering when flushing succeeds. Failure/expiry reports a local close code of 1006; unsent data is discarded. The runtime does not wait for peer acknowledgement of the close frame. Document shutdown or script suspension cancels this grace period and drops the transport immediately; it cannot retract bytes already accepted by the operating system.

Server-Sent Events support `EventSource`, named events, multi-line `data`, IDs, retry hints, reconnection with `Last-Event-ID`, and `close()`:

```js
const source = new EventSource('http://127.0.0.1:9000/progress');
source.addEventListener('progress', event => render(JSON.parse(event.data)));
```

SSE parsing is incremental, with a 64 KiB line limit, a 128 KiB assembled event-data limit, and 1 KiB limits on event names and IDs. This also bounds unterminated lines/events. Oversize input produces an error with `code: "message_too_large"`, `fatal: true`, and a CLOSED readyState. HTTP 204 and a successful response with an invalid content type also close permanently; connection failures otherwise reconnect. Retry hints are clamped to 250–30,000 ms. An empty `id` resets Last-Event-ID; named `open`/`error` messages do not change transport readyState.

WebSocket and SSE share a separate lazy `lapui-streams` reactor and reusable SSE HTTP client, rather than one OS thread per connection. At most eight streams may be outstanding, including streams whose queued terminal events have not yet been consumed by the UI. Opening more throws `code: "network_busy"`. URLs must be absolute, use the appropriate protocol, and fit within 16 KiB. Constructors reject unsupported URLs with a structured error.

Native stream-to-UI delivery shares a 64-record queue and a 16 MiB logical byte budget. Each record accounts for 512 bytes plus string/binary payload lengths. The byte budget includes acquired permits and partial reservations for waiting deliveries, plus the currently dispatched record. It is not a measurement or cap of total allocation: parsing/transport buffers, up to one waiting incoming message per active stream, TLS/DNS/OS storage, JS buffers, and allocator overhead are additional. Producers await capacity, preserving message order rather than discarding messages. Native stream slots are retained until their transport and queued UI events are released. The UI delivers at most 16 records per poll, checking a 20 ms slice between deliveries; an individual callback can take its separate script deadline. This is not a hard frame-time guarantee.

Inspect these stream counters with `lapui.networkStatus()`; the counters do not include fetch. The development TCP `networkStatus` method returns documentEpoch, stream counters, and limits, and `describe` includes `networkStreamLimits`. CLI: `lapui client <address> network-status`. The reactor starts on demand and does not poll while idle. Each reactor permits at most two Tokio blocking workers, but native DNS resolution already executing cannot be preempted. Shutdown does not wait for those native calls, and this is not a global thread-count limit.

SSE close and document shutdown use cancellation notifications that interrupt connection establishment, idle response reads, and reconnect delays without periodic polling. Closing a document also aborts active/queued HTTP work and closes its WebSockets. A stalled-server test verifies that all three connection types release their sockets after document drop. A bounded duplex-transport test separately verifies cancellation of a proven blocked WebSocket write, including its close grace period. Queue tests and a paused-UI SSE test verify byte/count backpressure, cancellation, slot reclamation, and ordered delivery after resuming. HTTP cancellation does not apply to Rust application actions or roll back a server operation.

All network APIs are intended for trusted local UI code. They do not sandbox arbitrary remote pages.
