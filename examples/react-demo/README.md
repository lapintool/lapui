# React DOM example

This focused sample uses React 19.2.0's production DOM renderer on the native Blitz document. It exercises asynchronous mount/effects, controlled text input (including Chinese text through AI fill), delegated capture/bubble listeners, state-driven list insertion/removal, conditional details and an asynchronous shared Rust action.

The reproducible bundle is included so running the example needs no Node.js:

```powershell
cargo run --release --locked -- --html examples/react-demo/index.html
```

To rebuild it:

```powershell
cd examples/react-demo
npm ci
npm run build
```

The integration test loads the same bundle and operates controls through the background client/UI-thread path:

```powershell
cargo test --locked react_dom_bundle_mounts_updates_controlled_input_lists_effects_and_rust_action
```

This sample does not establish compatibility with arbitrary React applications, component libraries, portals, SSR/hydration, media, advanced inputs or physical IME. Check the [compatibility matrix](../../guide/compatibility.md) for current scope. Upstream React/React DOM/Scheduler and esbuild licenses are retained alongside this example.
