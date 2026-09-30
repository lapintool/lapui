# Resumable change feed

Run without Node.js or a bundle build:

```powershell
cargo run --release --locked -- --html examples/changes-demo/index.html
```

The UI gets a consistent initial snapshot and follows state/action changes through `lapui.changes.subscribe`. It updates the counter from state deltas rather than from the invocation response or the legacy host-render callback. In another terminal, use the printed address:

```powershell
.\target\release\lapui.exe client 127.0.0.1:<port> increment
.\target\release\lapui.exe client 127.0.0.1:<port> changes
```

Pause updates, perform more increments, then resume to consume the unacknowledged history. If too much history has been evicted, the feed supplies `resyncRequired`, a reason and a fresh baseline. Document reload creates a new frontend while Rust state/history survive. The history and request deduplication are process-local; a process restart does not promise durable side-effect recovery. See [change subscriptions](../../guide/changes.md) for scopes, limits and the exact resume contract.
