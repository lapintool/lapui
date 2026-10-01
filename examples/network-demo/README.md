# Local network demo

This example combines the runtime's local HTTP `fetch`, EventSource, and WebSocket APIs with a searchable list, input events, repeated node insertion/removal, a popover, a small asynchronous task, and an optional script error.

It uses a dependency-free Python fixture server on loopback. In one terminal, start the server:

```powershell
py -3 examples/network-demo/server.py
```

In another terminal, build and run Lapui:

```powershell
cargo run --release --locked -- --html examples/network-demo/index.html --mcp-bridge-id network-demo
```

The server must be running before the example starts. It binds only to `127.0.0.1:8765`; the browser-like APIs are intended for this trusted local UI. The `Inject asynchronous error` button intentionally reports a JavaScript diagnostic and is useful when checking reload recovery. The fixture implements only the local endpoints needed by this example and is not a general HTTP or WebSocket server.

To connect an MCP client, run `target/release/lapui.exe mcp-stdio network-demo` in a separate terminal. Repeated fetch, SSE connect/close, WebSocket echo, list churn, popover toggle, and task operations exercise the same DOM that the user sees.
