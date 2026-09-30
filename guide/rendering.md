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

Initial Windows measurements showed a large renderer-dependent difference: a five-second counter sample using the GPU path had 608,256,000 private bytes (580.08 MiB), while an early CPU-path build had 9,256,960 private bytes (8.83 MiB) and 31,227,904 working-set bytes (29.78 MiB). These are short, single-machine process samples, not peak/steady-state benchmarks. The difference points to renderer/driver overhead but does not identify individual allocations. GPU memory, first-interactive timing, frame-time percentiles and long-running detached-DOM memory remain unmeasured.

The current CPU-default Windows release, including PNG export and native reload, is 29,792,256 bytes (28.41 MiB); its five-second counter sample used 10,297,344 private bytes (9.82 MiB) and 32,112,640 working-set bytes (30.63 MiB). Linux offscreen painting passed after supplying Noto CJK fonts to Fontconfig; the initial font-minimal WSL image showed missing glyphs. Pixel and font verification is separate from physical input verification.

A separate short Windows release workload on an earlier build replaced 5,000 listener-bearing subtrees per pass, three passes in one window (about 75,000 created nodes). Private memory was 8,794,112 bytes before the workload and 12,406,784 / 14,094,336 / 12,181,504 bytes three seconds after each pass (11.83 / 13.44 / 11.62 MiB). Threads remained at 13; post-pass handles remained at 257; diagnostics were empty. The following 10-second idle interval had no measurable process CPU-time increment at the OS counter's resolution. These samples exclude in-pass peaks and are not long-run leak proof, frame timings or an idle-CPU guarantee. The non-monotonic post-pass readings complement the native-node collection regression tests. The current counter sample has 21 threads and 258 handles, including eight fixed TCP connection workers; the HTTP reactor starts only when used.
