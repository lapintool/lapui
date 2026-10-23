# Representative UI fixtures

This is a small local workbench with seven fixed scenarios: form submission,
list/conditional updates, scrolling, Chinese wrapping, an anchored panel,
responsive columns and asynchronous completion. The same HTML/CSS/JavaScript
runs in Lapui and Chromium. It has no network, framework bundle, Rust business
adapter or replacement browser API. Existing Vue, React and Floating UI examples
remain separate compatibility tests.

Open `index.html` in a browser, or run:

```powershell
cargo run --locked -- --html examples/ui-fixtures/index.html
```

The buttons run a scenario and store its explicit result in the hidden
`#fixture-result` element. Automated runs use a fresh document for every case.
Reload before manually repeating a case that mutates its data.

## Compare with a browser

Build the development-only native driver, then use Python 3.11+, Node.js 22+
and an installed Chrome or Edge executable. The drivers use only standard
libraries; Node is a development-test dependency, not an application requirement.

```powershell
cargo build --locked --example fixture_runner
python tools/compare_ui_fixtures.py --browser "C:/Program Files/Google/Chrome/Application/chrome.exe"
```

The standard-library Python driver reads `manifest.json` and compares all seven
cases at 960×640 and 640×480 CSS pixels. The browser protocol sets the CSS viewport
and captures its pixels from the same document after explicit completion. Both
the measured viewport and PNG dimensions must match; a window's frame size is
not treated as its viewport. The native driver uses 1× offscreen CPU rendering.
The browser gets its own temporary profile and loads the original offline
HTML/script files.

Results, paired PNGs and executable/fixture SHA-256 metadata are written under
`target/fixture-comparison/`. Use `--out` to choose another directory. The default
native runner is `target/debug/examples/fixture_runner`; `--lapui-runner` can select
another build. `--browser-version` records a version obtained from the browser
installation and checks it against the running browser. The browser product,
engine version and capture boundary are always recorded. Generated reports
are excluded from Git.

Every case must reach its explicit ready state, pass its behavior checks and
match the browser's state. Reported rectangles have a fixed tolerance of 1 CSS
pixel; differences cause a nonzero exit and remain in `summary.json`. The images
are for visual inspection; pixel equality is **not** an acceptance check. The
browser document-load and script-completion waits are bounded, and missing
readiness is an error, not assumed success after a delay.

The stylesheet requests Arial and Microsoft YaHei, with system fallbacks.
Install the same fonts for a meaningful typography comparison. Font files are
not redistributed. The scroll fixture hides scrollbars so content geometry can
be compared independently of platform scrollbar styling.

## Actual framework examples

The separate framework suite loads the real checked-in Vue 3.5.43,
React/React DOM 19.2.0 and Floating UI DOM 1.8.0 example bundles and their original
stylesheets. It does not substitute the basic workbench or mock Rust actions:

```powershell
cargo build --locked --example fixture_runner
python tools/compare_ui_fixtures.py --suite frameworks --browser "C:/Program Files/Google/Chrome/Application/chrome.exe"
```

`--suite all` runs both groups. The default remains `basic`. Outputs go to
`target/framework-comparison/` or `target/fixture-comparison-all/`, respectively.
After minimal-feature checks, rebuild the native example with default features
before comparing: a minimal runner explicitly rejects CPU image capture.

Vue adds/removes a Chinese task, checks an item, clears the draft and opens the
dialog; the dialog must cover and center in the viewport. React exercises its
effect, controlled input/reset, list and conditional content. Four Floating UI
cases cover center placement, edge flip, left shift and an anchor resize observed
by the actual library's autoUpdate. Each case runs at both viewport sizes.

Native input uses the public structured `fill`; the browser uses its real input
prototype setter followed by native input/change events, bypassing framework
value tracking just as an edit does. These are synthetic controls, not physical
keyboard/IME evidence. The React Rust-counter action remains untouched; its
business path has separate runtime/MCP tests. No fake host backend is installed.

The framework reference comparison currently **fails** its strict geometry gate.
On the recorded Windows/Chrome setup all 12 per-engine behavior checks passed,
but only 10 full states matched and none of the 12 complete rectangle sets met
the 1 CSS pixel threshold. Default font line metrics/control sizes and classic
browser scrollbar space differ; narrow Floating UI coordinate readouts also
differ. These failures remain in the reports and produce exit code 1. The
summary records behavior, state and geometry separately without changing the
overall pass rule. Library bundle, page, CSS and lockfile hashes are retained.

## Evidence boundaries

The basic fixtures do not establish arbitrary website compatibility,
framework compatibility, native select widgets, physical keyboard/pointer/IME
behavior, monitor presentation, or complete CSS transform/clip semantics.
The anchored panel uses ordinary absolute positioning; it does not replace the
actual Floating UI suite. Framework results cover only the named steps and
versions. A fresh-document width comparison is distinct from
live OS resize/DPI acceptance. The async case waits on a declared application
completion, not on all work becoming idle. Reload/error recovery, native input,
the Rust file-tool demo and wider component coverage remain separate gates.

The current pinned Blitz version subtracts a scroll container's own offset
from its native client rectangle. Lapui's geometry adapter compensates for that
offset while retaining ancestor and viewport scrolling. The scroll scenario
and runtime regression exercise this behavior for both script and AI geometry;
dependency upgrades must recheck this compensation.
