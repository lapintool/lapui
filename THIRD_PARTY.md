# Third-party licensing

Lapui's `MIT OR Apache-2.0` choice applies to Lapui's original files. It does not replace the licenses of dependencies.

The frame-aware shell uses the already shared AnyRender 0.13.0 (`MIT OR Apache-2.0`) and winit 0.31.0-beta.2 (`Apache-2.0`) APIs directly. It wraps Blitz window handling without vendoring or modifying its source.

Native development file notifications use notify 8.2.0 (`CC0-1.0`); its platform adapters and other dependencies retain their own upstream terms.

The default software path uses AnyRender Vello CPU 0.17.0 and Softbuffer Window Renderer 0.8.0 (`MIT OR Apache-2.0`), alongside their upstream drawing/window dependencies. Offscreen PNG output uses the already shared image 0.25.10 package (`MIT OR Apache-2.0`). GPU drawing remains available through AnyRender Vello; this does not make the renderer implementations Lapui-authored code.

The runtime depends directly on Blitz (`MIT OR Apache-2.0`), AnyRender Vello (`MIT OR Apache-2.0`), rquickjs (`MIT`), and keyboard-types (`MIT OR Apache-2.0`). Its transitive rendering stack includes Stylo (`MPL-2.0`). The exact package versions are recorded in `Cargo.lock`; the authoritative license terms are those supplied by each upstream package.

The [local preview packager](guide/packaging.md) collects the selected Windows normal/build dependency graph, supplied notice files and matching original Cargo archives, with source and file hashes. This conservatively includes build dependencies and does not prove every package contributed linked code. Before a public binary release, finish the dependency/embedded-data notice review and source-availability arrangements for MPL-covered components. This source repository does not yet publish binary releases.

The checked-in `examples/vue-demo/dist/app.js` is a reproducible bundle of Vue 3 (`MIT`) and esbuild output (`MIT`), built from the exact versions in `examples/vue-demo/package-lock.json`. The Vue family shares the retained `LICENSE.vue` text; `LICENSE.esbuild` and the bundle's upstream headers are also preserved. Its original packages are available from npm; the surrounding demo source is Lapui-authored.

The checked-in `examples/react-demo/dist/app.js` contains React, React DOM and Scheduler (`MIT`) and esbuild-generated output, built from `examples/react-demo/package-lock.json`. React family and esbuild license files are retained in that example directory, along with the bundle's upstream license headers. The surrounding example is Lapui-authored.

The checked-in `examples/floating-demo/dist/app.js` contains Floating UI DOM/core/utils and esbuild output (MIT). Exact versions are locked in that example; all three Floating UI license files and esbuild notice are retained alongside it. Lapui calls public Stylo computed-value/declaration serializers and Blitz geometry APIs through dependencies; it does not copy or modify their source files.
