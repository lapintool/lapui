# ES module example

Run from the repository root:

```powershell
cargo run --release --locked -- --html examples/modules-demo/index.html
```

No Node.js or bundling step is needed. The example uses a static import and re-export through a nested module, `import.meta.url`, top-level await on app-local fetch, and a dynamic import before calling the shared Rust counter action. Its interaction test runs the same files:

```powershell
cargo test --locked es_module_example_resumes_top_level_await_and_lazy_rust_action
```

Modules resolve as relative/file URLs within the HTML application's directory. Bare package names, remote imports, import maps, JSON import attributes, and browser-complete async/defer scheduling remain unsupported. Read `lapui.diagnostics()` inside the app or use the CLI `diagnostics` command to inspect the last 128 script load/evaluation errors (sources/messages capped at 4 KiB/16 KiB).
