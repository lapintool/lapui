# Upstream compatibility patches

This ledger records where Lapui follows a pinned upstream implementation and where a small compatibility layer remains necessary. Review every entry when upgrading Blitz, Taffy, Stylo, or AnyRender. Keep the regression cases when a local patch is removed.

## Active upstream baseline

| Component | Baseline | Reason |
| --- | --- | --- |
| Blitz family | `DioxusLabs/blitz` commit `8b54d6d2930f654d75446e423b42033d323205f0`, pinned through `[patch.crates-io]` | Adds fixed table sizing and `<col>/<colgroup>` column widths absent from the published `0.3.0-beta.2` source used before this batch. This is a pinned upstream snapshot, not a released stable version. |
| Stylo | `0.22.0` | Required by the selected Blitz snapshot. |
| AnyRender | `0.14.0`, Vello `0.15.0`, CPU Vello `0.18.0` | Keeps the renderer trait graph aligned with the selected upstream snapshot. |
| Winit | `0.31.0-beta.3` | Required by the selected Blitz shell snapshot. |
| Taffy | `0.14.0` at `ceb7443d386fc5cc67a1f72b51f7bf505439dff8`, runtime sources in `vendor/taffy` | Keeps the existing Blitz API baseline and backports upstream Grid fixes #1320 and #1284. See P-GRID-CALC-001 and P-GRID-BLOCK-001. |

The dependency graph is locked in `Cargo.lock`. The source upgrade passed the all-target Rust suite, the full `lapstyle-ui` mount/navigation check, and a GPU-only `cargo check --no-default-features --all-targets`. Those checks validate build and selected semantics; visual table and GPU presentation still require manual acceptance.

## P-BORDER-001 — preserve fractional CSS border widths during paint

| Field | Record |
| --- | --- |
| Symptom | At fractional DPI, integer rounding in layout can round a non-zero border width to zero on a far edge; the input's vertical border can appear missing or narrowed. |
| Reproduction | `snapshot::tests::rounded_input_keeps_both_vertical_borders_at_fractional_dpi_scales`; manual case UI-03 at multiple system DPI and page zoom levels. |
| Baseline and location | Blitz `8b54d6d2930f654d75446e423b42033d323205f0`; `src/paint.rs` temporarily supplies unrounded border widths to paint and restores authoritative rounded layout afterward. |
| Scope | Paint scene construction, including subdocuments and generated `::before`/`::after` boxes used by outline buttons, radios and checkboxes. Shared by CPU and GPU scene generation. |
| Evidence | Input and generated-box pixel tests retain all four borders at 1x, 1.25x, 1.5x and 1.75x. Human UI-03 remains pending. |
| Exit condition | Remove the compensation only when the upstream renderer preserves these fractional edges and the pixel test plus UI-03 pass without it. |
| Status | Active; not visually accepted by the user on this candidate. |

## P-OFFSETPARENT-001 — find offsetParent through DOM ancestors

| Field | Record |
| --- | --- |
| Symptom | An absolutely positioned element whose containing block is the initial containing block returned `null` for an ancestor's `offsetParent`, even though CSSOM View selects the static `body`. |
| Reproduction | `runtime::tests::offset_metrics_use_native_parent_padding_edges_ignore_scroll_and_hide_boxless_nodes`, including the `parent.offsetParent` assertion. |
| Baseline and location | Blitz `8b54d6d2930f654d75446e423b42033d323205f0`; `src/geometry.rs` searches DOM ancestors, selecting positioned elements and the always-eligible `body`, `td`, and `th` elements. The upstream `Node::offset_parent()` follows the containing-block chain, which can skip static body. |
| Scope | CSSOM geometry and JavaScript-visible `offsetParent`; does not change layout or paint. |
| Evidence | The regression test and full Rust suite pass. |
| Exit condition | Drop the local search when the upstream API follows CSSOM View's DOM-ancestor rule and the same regression passes against that implementation. |
| Status | Active compatibility shim. |

## P-GRID-CALC-001 — accept CSS math in auto-repeat Grid tracks

| Field | Record |
| --- | --- |
| Symptom | Lapstyle UI's API tables use `repeat(auto-fit,minmax(min(280px,100%),1fr))`. Taffy rejected calc-backed track lengths as non-fixed, so wide windows produced implicit rows instead of responsive columns. |
| Reproduction | `runtime::tests::auto_fit_grid_accepts_css_math_tracks_and_reflows_on_resize`, plus the Table page in the full Lapstyle UI integration test. |
| Baseline and location | Taffy `ceb7443d386fc5cc67a1f72b51f7bf505439dff8`; `vendor/taffy/src/style/grid.rs`, `TrackSizingFunction::has_fixed_component`. Root Cargo patches only Taffy's Git source. |
| Scope | Track validation for auto-fit/auto-fill Grid templates. No Lapstyle-specific selectors or CSS substitutions. |
| Upstream | Backport of [Taffy #1320, commit 63779e5](https://github.com/DioxusLabs/taffy/commit/63779e56f482140d794b980734cbd2db45857f7a), adapted to the older CompactLength API. |
| Exit condition | Adopt a compatible Blitz/Taffy group containing #1320; remove the Cargo source override and vendor directory after the same resize/geometry and real-app CPU/GPU checks pass. |
| Status | Active bounded backport. Provenance, manifest-only packaging adjustments and original license are in `vendor/taffy/LAPUI-PATCHES.md`. |

## Lapui DOM compatibility regression: reused template content

Vue reuses a template element for static HTML insertion. Lapui formerly moved its children into `template.content` only once. Subsequent assignments left that fragment empty, silently omitting seven of Lapstyle's eight table examples. `ui/bridge.js` now replaces the stable content fragment on every innerHTML assignment, serializes that fragment, and preserves it when cloning a template. This is a Lapui DOM bridge fix, separate from upstream layout patches. The repeated-template regression and the full application's eight-table assertion must remain when changing the DOM implementation.

## P-GRID-BLOCK-001 — preserve a Grid flex item's intrinsic block height

| Field | Record |
| --- | --- |
| Symptom | API tables extend below a grid whose height collapses under column-flex pressure, overlapping the following text and examples. The reduced fixture also differs from headless Chrome's measured layout. |
| Reproduction | `grid_flex_items_preserve_intrinsic_block_height_unless_min_height_is_zero` checks auto and fractional rows plus explicit shrinking. The full application check asserts API table columns fit inside their grid and do not overlap following text. |
| Baseline and location | Taffy `ceb7443d386fc5cc67a1f72b51f7bf505439dff8`; `vendor/taffy/src/compute/grid/track_sizing.rs`, finite-growth-limit maximization and final expansion-space mapping. |
| Scope | Intrinsic block-axis Grid sizing; inline-axis min-content constraints remain unchanged. |
| Upstream | Backport of [Taffy #1284, commit e2fbe56](https://github.com/DioxusLabs/taffy/commit/e2fbe56495ea4c033329f2ed2e61c4cf0dee8154), plus a local extension of the same intrinsic block-size rule to finite-growth-limit auto-row maximization. The auto-row case still failed with the upstream change alone. |
| Exit condition | Adopt a compatible dependency group containing #1284 and pass the same regressions and real-app CPU/GPU checks without the local override. |
| Status | Active bounded backport; original sources and license are retained with P-GRID-CALC-001. |

## Blitz component compatibility group

`vendor/blitz-dom` and `vendor/blitz-paint` retain the exact Blitz baseline above, original manifests and licenses. Both registry and Blitz Git source overrides resolve to these same local packages. The paint source is unchanged; its standalone manifest keeps its DOM dependency on the patched package. No Cargo cache sources are edited. Their `LAPUI-PATCHES.md` files record the precise scope and removal conditions.

| Patch | Cause and bounded fix | Regression / removal gate |
| --- | --- | --- |
| P-HAS-001 | Stylo's `:has()` parsing preference was disabled, and state changes did not run its relative-selector invalidator. Enable the preference and use Stylo's state dependency maps to invalidate related ancestors/siblings. | Radio group clicks must move the selected style between labels and preserve hover styling. Remove only when both parsing and state invalidation work upstream. Attribute/structural mutation conformance is not established by this test. |
| P-SVG-PAINT-001 | Isolated inline SVG rendering discarded host CSS fill/stroke styles. Carry resolved paint values into the SVG snapshot and include the supported SVG presentation attributes in Stylo's lower-priority cascade. | CSS currentColor, dynamic color changes, hollow stroke icons, CSS overriding attributes and local gradient references. The bounded property list is recorded in the vendor ledger; external paint-server URLs and full SVG CSS are not covered. |
| P-INPUT-LINE-001 | Auto-height text inputs were measured as empty blocks. Supply a one-line intrinsic height and size-based intrinsic width, respecting definite CSS dimensions and refreshing the editor width. | Empty/nonempty inputs retain a font-sized line box; preserve editor/form regressions and the real application's dense-input height check. |
| P-SCROLLBAR-ALPHA-001 | The scrollbar paint feature was disabled. Native activity fading also hid CSS-authored hover scrollbars before any scrolling. Enable the existing feature and let explicit color alpha control visibility. | Pixel checks for never-scrolled hover visibility and `scrollbar-color` transition interpolation on enter/leave. Unstyled overlay scrollbars retain upstream activity fading. Registered custom-property interpolation by itself is not established. |

Lapui's DOM bridge also implements element-only sibling/child traversal and reflects the boolean `hidden` property to its native attribute. Component code uses these interfaces to locate submenus and show/hide dialogs. Retain the traversal regression and full-application menu expand/collapse and dialog open/close assertions. These are DOM compatibility fixes, not component-specific CSS substitutions.

## Upgrade review rules

1. Evaluate a new upstream candidate with local behavior patches disabled where practical; verify fixes with the retained regression cases.
2. Remove a patch when upstream passes the same regression and relevant manual case. Narrow it when only part of the behavior remains unsupported.
3. If a patch still applies but upstream semantics changed, re-check the user-visible behavior and the `offset*`, hit-testing, scroll, CPU paint, and GPU scene paths it can affect.
4. Keep upstream-derived table sizing separate from Lapui paint or DOM compatibility logic. Do not add a second table-layout algorithm without a failing geometry reproduction and a bounded ownership plan.

## Current table coverage boundary

The selected upstream snapshot passes fixed-layout `<colgroup>` geometry tests with long cell content, aligned header/body tracks, and a case combining `colspan` with `rowspan`. Lapui has not yet established broad automatic-layout, span-edge-case, or collapsed-border conflict conformance. The full application integration test checks all eight Lapstyle table examples actually exist and have positive dimensions, in addition to checking all sections mount without script diagnostics. Built-in screenshots and geometry diagnostics are available by setting `LAPUI_DIAGNOSTICS_DIR` for that test; this does not replace physical visual UI-01 inspection.
