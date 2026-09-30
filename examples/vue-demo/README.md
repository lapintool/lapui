# Vue 3 demo

This demo uses Vue 3's runtime DOM renderer to mount a small Chinese task list into Lapui's Blitz-backed document. It covers input events, reactive list diffing, AI-driven checkbox state reflected back into Vue, boolean `disabled` reflection, and conditional dialog insertion/removal.

The built bundle is checked in so Rust users can run it without installing Node. From the repository root:

```powershell
cargo run --release --locked -- --html examples/vue-demo/index.html
```

To rebuild `dist/app.js`, use Node.js/npm:

```powershell
cd examples/vue-demo
npm ci
npm run build
```

The integration test executes this same bundle against the native Blitz document:

```powershell
cargo test --locked vue3_runtime_dom_bundle_renders_and_patches_the_blitz_tree
```

The demo shows a focused compatibility sample, not general Vue or browser conformance. See the public [compatibility matrix](../../guide/compatibility.md) for verified behavior and current gaps.
