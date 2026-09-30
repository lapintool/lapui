# JavaScript modules

Use ordinary local module entries in your application HTML:

```html
<script type="module" src="./main.mjs"></script>
```

```js
import { formatCount } from './lib/format.mjs';
const greeting = await (await fetch('./greeting.json')).json();
document.getElementById('status').textContent = greeting.message;
const actions = await import('./actions.mjs');
```

Static and dynamic imports, re-exports, module scope, live bindings, cycles supported by QuickJS, `import.meta.url`, and top-level await use QuickJS-ng's module implementation. A module is cached by its canonical file URL. Dot-path and filesystem aliases resolve to one module; distinct query strings or fragments retain distinct URL identities. External module entries use that same cache. Inline entries have synthetic `#inline-script-N` URLs under the application base directory. Imported `.js` and `.mjs` files are evaluated as modules without extension-based package lookup.

Relative specifiers resolve from the importing module's URL. Absolute `file:` URLs must remain inside the application directory. The loader canonicalizes paths, rejects directory escapes, invalid UTF-8 and individual files above 32 MiB. Bare package names, remote HTTP(S) imports, import maps, import attributes and Node.js package resolution are unsupported. Build tools can still bundle package imports for the classic-script entry path.

All HTML is parsed before scripts run. Classic scripts execute in document order, then module entries are initiated in document order, then the optional CLI bundle executes. Top-level await may remain pending while the UI runs and resumes after host/network completions. Modules are not synchronously waited on during startup. Browser-complete async/defer scheduling and DOMContentLoaded semantics are not implemented. Classic `nomodule` fallbacks are skipped.

Startup load/evaluation failures do not stop later entries. `lapui.diagnostics()` returns entries with a per-document `sequence`, `source`, `phase` and `message` (including a stack when available). Module entry rejections after top-level await are also recorded. The buffer retains at most 128 entries; sources are capped at 4 KiB, phases at 64 bytes and messages at 16 KiB, truncating on UTF-8 boundaries. It does not collect all application-caught errors or every unhandled Promise rejection.

The [module example](getting-started.md#run-the-es-module-example) needs no frontend build step. Its Rust integration test loads the same files, waits for local fetch initialization, activates the button and verifies the asynchronous Rust action:

```powershell
cargo test --locked es_module_example_resumes_top_level_await_and_lazy_rust_action
```
