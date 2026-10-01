# Floating UI DOM example

This bundle uses **@floating-ui/dom 1.8.0** with its actual DOM platform adapter and offset/flip/shift middleware. Lapui supplies native computed styles, bounding/client rectangles, offset parents and CSS-pixel dimensions. No replacement platform or stub position calculator is used.

From the repository root:

```powershell
cargo run --release --locked -- --html examples/floating-demo/index.html
```

Click Open. The center anchor gives bottom-start positioning; Bottom-right edge flips both side and alignment; Left edge shifts the popover into its clipping container. Accept updates application UI and hides the popover. Resize the window or click Resize anchor to see the library’s default autoUpdate follow size changes. Reposition also remains available. The readouts expose placement, coordinates, containment and a live computed-width query to structured clients too.

The included `dist/app.js` needs no Node.js at runtime. Rebuild in this directory:

```powershell
npm ci
npm run build
```

The lockfile pins all packages, and upstream license files are retained alongside this example. The focused test uses the included unmodified library bundle through the shared control channel:

```powershell
cargo test --locked floating_ui_dom_bundle_positions_flips_shifts_and_accepts_through_shared_controls
```

This proves the selected local popover cases. The actual library’s default autoUpdate is active while open and is cleaned up on close. Selected window-resize and element-resize updates pass through Lapui’s window listener and ResizeObserver paths. IntersectionObserver/layout-shift detection, transformed/fixed/RTL/iframe/top-layer placement, portals and ecosystem-wide component compatibility remain separate work. The library detects the absent IntersectionObserver and disables that optional path; this example does not enable its opt-in animation-frame mode.

See [computed styles](../../guide/computed-styles.md), [geometry](../../guide/geometry.md) and [observers/window events](../../guide/observers.md) for API limits. Original demo code is MIT OR Apache-2.0; the bundled library and esbuild portions retain their respective MIT notices.
