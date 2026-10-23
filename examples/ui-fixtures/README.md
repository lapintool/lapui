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

## Evidence boundaries

These selected fixtures do not establish arbitrary website compatibility,
framework compatibility, native select widgets, physical keyboard/pointer/IME
behavior, monitor presentation, or complete CSS transform/clip semantics.
The anchored panel uses ordinary absolute positioning; it does not replace the
actual Floating UI tests. A fresh-document width comparison is distinct from
live OS resize/DPI acceptance. The async case waits on a declared application
completion, not on all work becoming idle. Reload/error recovery, native input,
the Rust file-tool demo and wider component coverage remain separate gates.

The current pinned Blitz version subtracts a scroll container's own offset
from its native client rectangle. Lapui's geometry adapter compensates for that
offset while retaining ancestor and viewport scrolling. The scroll scenario
and runtime regression exercise this behavior for both script and AI geometry;
dependency upgrades must recheck this compensation.
