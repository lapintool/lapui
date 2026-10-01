# Lapui

[简体中文](README.zh-CN.md) · [Documentation](guide/README.md)

Lapui is an experimental desktop UI runtime written in Rust. It combines Blitz for HTML/CSS rendering with QuickJS-ng for JavaScript, aiming to support local interfaces without bundling Chromium or requiring a system WebView.
**Status: early prototype; P1–P3 acceptance remains in progress.** The current build demonstrates a single window, a limited dynamic DOM and form subset, capture/bubble listeners, local modules and timers, semantic controls and diagnostics, asynchronous Rust actions, HTTP/WebSocket/SSE, and runnable Vue 3 and React DOM examples. It is not a general-purpose browser or production-ready framework runtime.

## Try the prototype

On Windows with a Rust MSVC toolchain:

```powershell
cargo run --release --locked
```

CPU drawing is the default; `--renderer gpu` selects the original compute-GPU path. `--snapshot target/demo.png` paints the current document without a window. See [rendering and images](guide/rendering.md) for behavior and measurements.

The app prints `LAPUI_CONTROL=127.0.0.1:<port>` and opens a counter window. In another terminal, use the printed address:

```powershell
.\target\release\lapui.exe client 127.0.0.1:<port> describe
.\target\release\lapui.exe client 127.0.0.1:<port> observe
.\target\release\lapui.exe client 127.0.0.1:<port> increment
.\target\release\lapui.exe client 127.0.0.1:<port> controls
.\target\release\lapui.exe client 127.0.0.1:<port> diagnostics
```

The button and the local client use the same Rust action and validation. `describe` reports protocol methods and implemented capability flags; `observe` returns versioned state and an action schema. The control endpoint is bound to loopback for development; it has no authentication and must not be exposed as a production API.

Linux compilation and tests have been verified on Ubuntu 24.04 in WSL with Rust 1.91 and `libfontconfig1-dev`. The React window and TCP activation path also passed under WSLg using X11. Wayland startup failed in that environment; physical input and other Linux desktops remain unverified.

To try a local page, use `cargo run -- --html .\app\index.html --js .\app\dist\app.js`. The runtime loads relative CSS/image/font resources and scripts within the HTML application's directory. Local ES modules support static/dynamic imports, re-exports, and top-level await; bare package imports still require a bundler. See the [module guide](guide/modules.md).

The [module example](examples/modules-demo/README.md) runs without Node or a build step: `cargo run --release --locked -- --html examples/modules-demo/index.html`.

The [form example](guide/forms.md) covers shared click defaults, labels, independent radio groups, disabled fieldsets, read-only input and keyboard activation: `cargo run --release --locked -- --html examples/forms-demo/index.html --watch`.

The [local submission example](examples/form-submit-demo/README.md) shares submit/reset, string FormData and selected validation with AI controls: `cargo run --release --locked -- --html examples/form-submit-demo/index.html --watch`. See [form behavior and limits](guide/forms.md).

The [change-feed example](examples/changes-demo/README.md) shares Rust state with a resumable UI and local clients: `cargo run --release --locked -- --html examples/changes-demo/index.html`. Pause/resume, consistent baselines, bounded history and disconnect recovery are described in [change subscriptions](guide/changes.md).

The [animation example](guide/getting-started.md#run-the-animation-and-measurement-example) uses requestAnimationFrame, cancellation and CSS-pixel measurements without a build step: `cargo run --release --locked -- --html examples/animation-demo/index.html`. See [scheduling](guide/scheduling.md) and [geometry](guide/geometry.md) for the supported subset. The scrollable-list example runs with `cargo run --release --locked -- --html examples/scroll-demo/index.html`.

The [Floating UI example](examples/floating-demo/README.md) runs the actual DOM positioning library with native computed styles and geometry: `cargo run --release --locked -- --html examples/floating-demo/index.html`. Selected offset, flip and shift cases pass; the library’s autoUpdate follows viewport and anchor-size changes while open. See [computed styles](guide/computed-styles.md) for the supported subset.

The [Vue 3 example](examples/vue-demo/README.md) and [React DOM example](examples/react-demo/README.md) exercise the live DOM bridge, reactive lists, and conditional content. React also tests controlled input, delegated capture/bubble events, effects and the asynchronous Rust action.

Use `--watch` with a local page to reload edits and rebuilt bundles. `lapui client <address> reload` performs a manual replacement; `reload-status` reports the latest attempt. Application state survives while frontend drafts and document references reset. See [development](guide/development.md).

For a richer in-memory tool with Chinese search, file details, shared rename actions, entity-version conflicts, scanning progress and cancellation, run:

```powershell
cargo run --release --locked -- --demo files
```

The demo does not modify disk files. See [Rust host actions](guide/host-actions.md) to register your own backend and use operation IDs and finite tracing.

AI clients can discover bounded action summaries with `actions.list`, retrieve a selected schema with `actions.describe`, and query business blockers with `actions.check`. Rust scopes retire temporary actions when their owner/document closes. See [action discovery and lifetime](guide/action-discovery.md).

## Build the documentation

The public guide is an [mdBook](https://rust-lang.github.io/mdBook/). After installing the `mdbook` CLI, run `mdbook build` from the repository root; `mdbook serve` starts a local preview. The generated `book/` directory is ignored by Git.

See the [getting started guide](guide/getting-started.md), [architecture](guide/architecture.md), [structured interaction](guide/interaction.md), and [current limits](guide/limitations.md).

## License

Lapui's original code and documentation are available under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. Dependencies retain their own licenses; see [third-party notes](THIRD_PARTY.md).
