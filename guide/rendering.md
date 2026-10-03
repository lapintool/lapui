# Rendering and document images

Default builds use Vello CPU with Softbuffer to paint the Blitz document into an operating-system window surface. This avoids the compute-GPU initialization overhead seen in the original prototype. HTML/CSS parsing, layout, font shaping, DOM, QuickJS and application actions use the same runtime. The GPU renderer remains available:

```powershell
cargo run --release --locked -- --renderer gpu
cargo run --release --locked -- --demo files --renderer cpu
```

The CPU path uses [AnyRender Vello CPU](https://docs.rs/anyrender_vello_cpu/0.17.0/anyrender_vello_cpu/), with pinned versions in Cargo.lock. This is an upstream renderer, not a Lapui browser engine fork. Software rendering is opaque at the window surface; transparent windows and complete CSS filter/renderer equivalence are not verified. Frame performance, large complex scenes, and physical desktop input still need broader testing. System fonts affect output across platforms.

The `software-renderer` Cargo feature includes the CPU window backend and offscreen PNG support, and is enabled by default. `--no-default-features` removes both the CPU feature and CJK dictionary line-break data; that build runs the original GPU backend. Smaller executable size alone does not imply lower process memory. To retain CPU drawing while omitting that dictionary data, use `--no-default-features --features software-renderer`.

Export the current document without creating a window or GPU context:

```powershell
cargo run --release --locked -- --demo files --snapshot target/files-tool.png --width 1000 --height 700
cargo run --release --locked -- --html examples/react-demo/index.html --snapshot target/react.png
```

This resolves styles/layout at 1x scale and paints the current state using the CPU renderer. It is not a screenshot of an existing window and does not wait for every asynchronous script, image or network operation. The embedding API `lapui::snapshot::render_rgba` and `save_png` allow a host to render after its own state-ready condition. Dimensions must be 1..8192 with at most 16 megapixels. Rendering changes the document viewport. PNG export converts premultiplied pixels to straight RGBA. The pixel regression test checks known CSS colors, state mutation and viewport resize; the file-tool output has also been visually inspected with Chinese text and controls. This does not prove physical IME/cursor behavior or GPU/CPU pixel equality.

## Built-in screenshot API

To capture a running application through its printed development-client address:

```powershell
lapui client 127.0.0.1:PORT screenshot target/current-window.png
```

This saves a PNG locally and prints compact capture metadata. It uses the UI
thread's current document and the built-in renderer; no MCP SDK, native screen
capture permission or window activation is required. It also works when the
window is covered. The development listener is enabled only in the regular
development mode; MCP-only modes keep their authenticated bridge. Use
`page_screenshot` through that bridge instead. The output directory must exist.

Embedding applications can capture their live document directly on its owning
thread, using `lapui::snapshot::capture`. This API works independently of MCP
and operating-system screenshot permissions, and preserves the current physical
viewport size, scale and color scheme:

```rust,no_run
use lapui::{runtime::LapuiDocument, snapshot};

fn debug_image(document: &mut LapuiDocument) -> Result<(), String> {
    // Call after the application's own readiness condition.
    let screenshot = snapshot::capture(document)?;
    println!("epoch={}, {}x{} pixels, scale={}",
        screenshot.document_epoch(), screenshot.width(), screenshot.height(),
        screenshot.scale_factor());
    let rgba: &[u8] = screenshot.rgba(); // Straight-alpha RGBA8.
    let png: Vec<u8> = screenshot.to_png()?; // Same owned frame; no second paint.
    screenshot.save_png("debug.png")?;
    Ok(())
}
```

`Screenshot` owns its pixels, so encoding and saving remain valid after later
document mutations or teardown. Capture polls pending work and paints one
animation opportunity; it does not wait for all asynchronous work. Dimensions
are limited to 8192 per axis and 4 megapixels. The `document_epoch` identifies
the document instance, and is not a DOM revision. Capturing advances rendering
callbacks, so it is not a passive copy of the last presented window frame.

The image contains Lapui document content. Native title bars, mouse cursors and
OS IME candidate windows require desktop inspection. The CPU image can be
captured while a GPU window is in use, but does not certify GPU pixel parity or
physical screen presentation. The API requires `software-renderer`, which is
enabled by default. `page_screenshot` uses this same capture and PNG encoder,
adds `scaleFactor` metadata and a `single_ui_thread_document_snapshot`
consistency label, and applies the separate 4 MiB MCP PNG budget. The document
epoch identifies the document instance; it is not a DOM revision.

Run `py -3 tests/screenshot_smoke.py --binary target/release/lapui.exe` for a
live CLI regression without an MCP SDK. It opens the maintained forms fixture,
checks discovery, PNG dimensions, epoch/DPI metadata and viewport preservation,
then closes its test process. Images and a JSON report remain under
`target/screenshot-smoke/`. The Rust pixel tests separately check straight alpha
and the captured image's independence from later document mutations.

Fractional-DPI painting temporarily restores unrounded CSS border edges before building the paint scene, then restores the authoritative Taffy layout. The pixel regression checks both vertical borders at 100%, 125%, 150%, and 175% scales without changing final layout. A current Windows 11 `forms-demo` window also showed the input's left and right borders intact after this fix. This addresses a 1-pixel edge loss seen at fractional scales; it does not establish general pixel parity with browser engines.

The current release candidate also completed matched short network-soak screens on both renderers. The CPU screen ran 30 minutes total including 5 minutes of warm-up (about 25 minutes of steady samples), followed by 10 minutes idle; private bytes rose 0.53 MiB in steady state. The opt-in GPU screen used the same duration and workload: steady private bytes rose 0.29 MiB and working set fell 3.18 MiB, but private bytes stayed near 348 MiB. DXGI usage during steady state was about 28 MiB dedicated and 273 MiB shared, dropping to about 24 MiB dedicated during idle while process private bytes remained flat. Both screens completed 180 cycles, 15 reloads, 6,034 SSE messages, 180 WebSocket messages, and 36 planned error injections with no unexpected errors. These are short screens, not the required eight-hour mixed-load acceptance; GPU remains opt-in and CPU remains the default.

Initial Windows measurements showed a large renderer-dependent difference: a five-second counter sample using the GPU path had 608,256,000 private bytes (580.08 MiB), while an early CPU-path build had 9,256,960 private bytes (8.83 MiB) and 31,227,904 working-set bytes (29.78 MiB). These are short, single-machine process samples, not peak/steady-state benchmarks. The difference points to renderer/driver overhead but does not identify individual allocations. At the time of those initial measurements, per-process GPU memory, first-interactive timing, frame-time percentiles and long-running detached-DOM memory remained unmeasured; the short GPU screen above adds process GPU-memory counters but does not close the latter three gaps.

An early CPU-default Windows release at the S1 baseline (2026-10-01), including PNG export, native reload, animation, geometry, computed styles, size observations and local forms, measured 30,258,176 bytes (28.86 MiB); its five-second counter sample used 10,641,408 private bytes (10.15 MiB) and 33,472,512 working-set bytes (31.92 MiB). The Windows x64 release candidate built with Rust 1.99.0 and Cargo.lock on 2026-10-02 after M1 measured 33,561,600 bytes (32.01 MiB; SHA-256 `87c6cfad0b1c47df5e1b0f5f629dacc83db324ce02139cbc77c350f978c847fd`). This is above the 30 MiB aspiration and below the 50 MiB review threshold; the final package and its required resources still need measurement. Linux offscreen painting passed after supplying Noto CJK fonts to Fontconfig; the initial font-minimal WSL image showed missing glyphs. Pixel and font verification is separate from physical input verification.

A separate short Windows release workload on an earlier build replaced 5,000 listener-bearing subtrees per pass, three passes in one window (about 75,000 created nodes). Private memory was 8,794,112 bytes before the workload and 12,406,784 / 14,094,336 / 12,181,504 bytes three seconds after each pass (11.83 / 13.44 / 11.62 MiB). Threads remained at 13; post-pass handles remained at 257; diagnostics were empty. The following 10-second idle interval had no measurable process CPU-time increment at the OS counter's resolution. These samples exclude in-pass peaks and are not long-run leak proof, frame timings or an idle-CPU guarantee. The non-monotonic post-pass readings complement the native-node collection regression tests. That earlier counter sample had 21 threads and 263 handles, including eight fixed TCP connection workers; the HTTP reactor starts only when used.
