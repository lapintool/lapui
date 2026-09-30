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

The basic API includes open/message/error/close callbacks, event listeners, and `close()`. Binary data is exposed as `Uint8Array` where available. Frames are limited to 8 MiB. Connections time out after 20 seconds, and closing during a pending handshake interrupts the connection attempt. Protocol extensions, compression, subprotocol negotiation, and browser-compatible `Blob` behavior are not implemented.

Server-Sent Events support `EventSource`, named events, multi-line `data`, IDs, retry hints, reconnection with `Last-Event-ID`, and `close()`:

```js
const source = new EventSource('http://127.0.0.1:9000/progress');
source.addEventListener('progress', event => render(JSON.parse(event.data)));
```

SSE close and document shutdown use cancellation notifications that interrupt connection establishment, idle response reads, and reconnect delays without periodic polling. Closing a document also aborts active/queued HTTP work and closes its WebSockets. A stalled-server test verifies that all three connection types release their sockets after document drop. HTTP cancellation does not apply to Rust application actions or roll back a server operation.

All network APIs are intended for trusted local UI code. They do not sandbox arbitrary remote pages.
