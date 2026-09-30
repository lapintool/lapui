# Development and full reload

Run a local application with native file watching:

```powershell
cargo run --locked -- --html examples/react-demo/index.html --watch
```

Changes under the HTML application's directory trigger a complete document replacement after 150 ms without another relevant notification. Rebuilding `dist/app.js`, editing HTML/CSS/modules, or changing an asset reloads the page. An explicit `--js` outside that directory is watched by filename in its parent directory. Embedded demos support manual reload; `--watch` requires a local HTML file or script and cannot be combined with `--snapshot`.

The watcher uses the platform's native notification backend from [notify](https://docs.rs/notify/8.2.0/notify/). It does not repeatedly scan files while idle. Events for `node_modules`, `target`, `book`, `.git`, `.codex`, `.agents`, logs and common temporary files are ignored; symlinks are not followed. Recursive backend registration can still be expensive for very large application directories. Network/shared filesystems and WSL Windows-mounted paths may not reliably deliver changes: use the manual command if a change is missed. Watch the application directory rather than a whole checkout containing unrelated files.

With the printed control address, another terminal can request or inspect reload:

```powershell
.\target\debug\lapui.exe client 127.0.0.1:<port> reload
.\target\debug\lapui.exe client 127.0.0.1:<port> reload-status
```

The CLI discovers the current epoch before requesting replacement. The raw protocol request is:

```json
{"method":"reload","documentEpoch":123}
```

Use the actual epoch from `controls` or `reloadStatus`; `describe` includes the schema and active reload/watch capability flags. A successful response reports the previous and new document epochs and the shared application's state version. Repeating the same completed request with the old epoch returns `stale_document`. Reload has no request-ID deduplication. If a dispatched request times out with `outcome_unknown`, query the current epoch before retrying.

Reload replaces QuickJS, DOM, module caches, timers, document workers and connections. Old control references and controller clones close or become stale; the same TCP address routes subsequent requests to the replacement. Window size, scale and shell provider are retained. Focus, scroll position and frontend drafts reset. Registered application state, actions, request cache and application-scoped operations survive; this is not state-preserving HMR. A running application job still needs cooperative cancellation if the app wants it to stop.

A query already queued on the old document when a watched replacement happens can return `document_closed`. Re-query the new endpoint and obtain its epoch/references. Do not automatically repeat control mutations with an uncertain outcome.

Application [change-feed cursors](changes.md) also survive document replacement. An external client can resume from its last applied cursor, while the new frontend starts from a consistent baseline. Native results addressed to the old document are discarded. Resume and deduplication remain process local; restarting the executable loses them.

Rust embeddings can attach [action scopes](action-discovery.md) to a document. Successful replacement retires those scoped registrations and publishes catalog changes; direct application registrations remain. Already admitted calls and accepted jobs can finish. Register replacement actions again for the new document when using an embedding's own setup flow; scope retirement is not automatic migration of handlers.

Source read/UTF-8/setup failures retain the usable document and return `reload_failed`. HTML and an explicit bundle are each limited to 32 MiB. `reloadStatus` stores the latest attempted reload and any error, including watched failures. Script evaluation failures follow first-launch behavior: the replacement is installed and reports script diagnostics. Startup code can invoke application actions, so reload is not a rollback boundary for those effects. Rust source edits still require recompilation.

Regression coverage includes repeated document replacement, module-cache refresh, stale control channels, shared state/viewport retention, native notification coalescing, failure recovery and watcher shutdown. Physical IME/focus handover, macOS and long-run memory/pause-time measurements still require platform testing.

The repository's `Build and regression` GitHub workflow targets Windows, Ubuntu and macOS with Rust 1.91.1. It checks formatting, default/minimal-feature tests, strict Clippy, executable linking and offscreen painting; a separate job builds this guide. It runs on `master` pushes and pull requests or manual dispatch. That workflow is not evidence that an unexecuted platform passed, and its headless tests do not validate desktop input.
