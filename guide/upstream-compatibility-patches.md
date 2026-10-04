# Upstream compatibility patches

This ledger records where Lapui follows a pinned upstream implementation and where a small compatibility layer remains necessary. Review every entry when upgrading Blitz, Taffy, Stylo, or AnyRender. Keep the regression cases when a local patch is removed.

## Active upstream baseline

| Component | Baseline | Reason |
| --- | --- | --- |
| Blitz family | `DioxusLabs/blitz` commit `8b54d6d2930f654d75446e423b42033d323205f0`, pinned through `[patch.crates-io]` | Adds fixed table sizing and `<col>/<colgroup>` column widths absent from the published `0.3.0-beta.2` source used before this batch. This is a pinned upstream snapshot, not a released stable version. |
| Stylo | `0.22.0` | Required by the selected Blitz snapshot. |
| AnyRender | `0.14.0`, Vello `0.15.0`, CPU Vello `0.18.0` | Keeps the renderer trait graph aligned with the selected upstream snapshot. |
| Winit | `0.31.0-beta.3` | Required by the selected Blitz shell snapshot. |

The dependency graph is locked in `Cargo.lock`. The source upgrade passed the all-target Rust suite, the full `lapstyle-ui` mount/navigation check, and a GPU-only `cargo check --no-default-features --all-targets`. Those checks validate build and selected semantics; visual table and GPU presentation still require manual acceptance.

## P-BORDER-001 — preserve fractional CSS border widths during paint

| Field | Record |
| --- | --- |
| Symptom | At fractional DPI, integer rounding in layout can round a non-zero border width to zero on a far edge; the input's vertical border can appear missing or narrowed. |
| Reproduction | `snapshot::tests::rounded_input_keeps_both_vertical_borders_at_fractional_dpi_scales`; manual case UI-03 at multiple system DPI and page zoom levels. |
| Baseline and location | Blitz `8b54d6d2930f654d75446e423b42033d323205f0`; `src/paint.rs` temporarily supplies unrounded border widths to paint and restores authoritative rounded layout afterward. |
| Scope | Paint scene construction, including subdocuments. Shared by CPU and GPU scene generation. |
| Evidence | The automated fractional-DPI pixel test passes on the selected snapshot. Human UI-03 remains pending. |
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

## Upgrade review rules

1. Evaluate a new upstream candidate with local behavior patches disabled where practical; verify fixes with the retained regression cases.
2. Remove a patch when upstream passes the same regression and relevant manual case. Narrow it when only part of the behavior remains unsupported.
3. If a patch still applies but upstream semantics changed, re-check the user-visible behavior and the `offset*`, hit-testing, scroll, CPU paint, and GPU scene paths it can affect.
4. Keep upstream-derived table sizing separate from Lapui paint or DOM compatibility logic. Do not add a second table-layout algorithm without a failing geometry reproduction and a bounded ownership plan.

## Current table coverage boundary

The selected upstream snapshot passes fixed-layout `<colgroup>` geometry tests with long cell content, aligned header/body tracks, and a case combining `colspan` with `rowspan`. Lapui has not yet established broad automatic-layout, span-edge-case, or collapsed-border conflict conformance. The full application integration test confirms all Lapstyle UI sections, including its table page, mount and navigate without script diagnostics; it does not replace visual UI-01 inspection.
