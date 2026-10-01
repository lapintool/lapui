# Getting started

## Requirements

- Rust 1.91 or newer.
- Windows: an MSVC Rust toolchain and its native build prerequisites.
- Linux: a working desktop toolchain; Ubuntu 24.04 needs `pkg-config`, `libssl-dev` for native TLS, and `libfontconfig1-dev` for the system-font build used here.
- Linux Chinese text: install a CJK font, for example Ubuntu's `fonts-noto-cjk`, or provide an application font. Dictionary line breaking does not supply glyphs. A minimal WSL environment with only DejaVu Sans renders unsupported Chinese characters as missing-glyph boxes.

The default Cargo features include dictionary-based complex-script line breaking for CJK text. `--no-default-features` omits that data/code path and the default CPU renderer, with character-level fallback for complex scripts and the original GPU backend. Use `--no-default-features --features software-renderer` to retain CPU drawing; see [rendering](rendering.md).

In the tested WSLg environment, the X11 window/reload path passed while Wayland connections failed. Force X11 for that environment with `env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET cargo run --locked -- --html examples/react-demo/index.html`; this is a platform workaround, not a general Wayland-support claim.

Run `cargo test --locked` to exercise the action registry, JavaScript event/Promise path, and Blitz document updates. Run `cargo run --release --locked` to launch the demo window.

The program prints a loopback control address. With the actual printed port, another terminal can run:

```text
lapui client 127.0.0.1:<port> describe
lapui client 127.0.0.1:<port> observe
lapui client 127.0.0.1:<port> increment
```

On Windows, run the equivalent command with `.\target\release\lapui.exe`. `describe` reports the protocol version and active capabilities; the action result includes `count`, `version`, and discoverable action metadata. A successful client invocation updates the same state that the window button uses.

The default demo resources in `ui/` are embedded at compile time. To load a local HTML page and its already bundled JavaScript, run:

```powershell
cargo run -- --html .\app\index.html --js .\app\dist\app.js
```

Paths are resolved from the current working directory. Relative CSS, image, font, and JavaScript subresources resolve from the HTML file's directory and are read from within that directory only; symlinks that escape the app directory, non-file URLs, non-GET requests, and individual resources larger than 32 MiB are rejected. Inline classic scripts and local `<script src>` scripts run in document order after HTML parsing. Module entries are initiated after classic scripts, then the optional `--js` bundle runs. Module top-level await can finish later through the normal UI completion queue. Browser-complete async/defer scheduling remains unsupported. An ordinary startup script error is logged and later scripts continue; an execution-limit interrupt suspends that document until reload (see [scheduling](scheduling.md)); `lapui.diagnostics()` exposes bounded load/evaluation errors. The local page must be trusted application code.

Add `--watch` to reload local files after edits or bundle rebuilds. Manual replacement uses `lapui client <address> reload`; inspect the latest attempt with `reload-status`. Rust application state survives, while frontend drafts and document references reset. See [development and full reload](development.md).

## Run the form example

```powershell
cargo run --release --locked -- --html examples/forms-demo/index.html --watch
```

No Node.js or bundle build is required. See [form interaction](forms.md) for checked-state, labels, disabled/read-only controls and keyboard behavior.

## Run the file tool

```powershell
cargo run --release --locked -- --demo files
```

This embedded fixture tool supports Chinese search, file details, rename conflicts, a settings panel, background scan progress and cancellation. It only changes an in-memory catalog. Read [Rust host actions](host-actions.md) for the shared backend and structured CLI requests. Human drafts are retained when another client edits the same entity.

## Run the ES module example

```powershell
cargo run --release --locked -- --html examples/modules-demo/index.html
```

This example needs no bundler or Node.js. It demonstrates local imports/re-exports, top-level await on local fetch, and dynamic import before a Rust action. See [JavaScript modules](modules.md) for resolution rules and current limits.

## Run the Vue 3 example

The repository includes a small Vue 3 application built with esbuild. The checked-in `dist/app.js` lets you run it without Node after cloning. To rebuild the bundle, install Node.js/npm and run:

```powershell
cd examples/vue-demo
npm ci
npm run build
cd ../..
```

Then start it from the repository root:

```powershell
cargo run --release --locked -- --html examples/vue-demo/index.html
```

The example exercises Vue's runtime DOM renderer against Lapui's Blitz-backed DOM: text input, reactive task insertion/removal, conditional dialog nodes, class updates, and button state. Its integration test runs the same checked-in bundle with `cargo test --locked vue3_runtime_dom_bundle_renders_and_patches_the_blitz_tree`.

## Run the React DOM example

```powershell
cargo run --release --locked -- --html examples/react-demo/index.html
```

The included React 19.2.0 production bundle needs no Node.js at runtime. Rebuild with `npm ci` and `npm run build` in `examples/react-demo`. This focused sample covers asynchronous mount/effects, controlled text input, delegated events, state-driven list insertion/removal, conditional content and the shared Rust counter action. Its test operates the native DOM through the background control channel:

```powershell
cargo test --locked react_dom_bundle_mounts_updates_controlled_input_lists_effects_and_rust_action
```

## Run the animation and measurement example

```powershell
cargo run --release --locked -- --html examples/animation-demo/index.html
```

Start, pause, resume and reset a frame-driven marker. The example reports callback count, progress and measured marker/track geometry. Resizing changes the track layout; the next animation/measurement callback refreshes the geometry readout. See [scheduling](scheduling.md) and [layout measurements](geometry.md). No frontend build step is needed.


## Run the scrolling example

```powershell
cargo run --release --locked -- --html examples/scroll-demo/index.html
```

Down/Right, End and Home change the native list offset; selecting a row updates the interface. Readouts show client dimensions, content extent and last-row geometry. Resize and query again to see current layout. See [scrolling and geometry](geometry.md#element-and-viewport-scrolling) for notification and smooth-scroll limits.


## Run the Floating UI example

```powershell
cargo run --release --locked -- --html examples/floating-demo/index.html
```

Open the popover, move its anchor to either edge, and accept/close it. The included Floating UI DOM 1.8.0 bundle uses its actual middleware against native CSS/geometry. Resize the window or click Resize anchor; the library’s autoUpdate follows these selected size changes through window listeners and ResizeObserver. [Example setup and focused tests](../examples/floating-demo/README.md) include reproducible bundle rebuilds and license notices.

## Run the local form submission example

```powershell
cargo run --release --locked -- --html examples/form-submit-demo/index.html --watch
```

Edit required fields, validate, submit and reset. Submission reads string FormData
and invokes the Rust counter action; AI fill/check/activate use the same form
constraints. [Form semantics and limits](forms.md) describe unsupported types and navigation.
