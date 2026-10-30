# Blitz DOM compatibility patches

Baseline: DioxusLabs/blitz
`8b54d6d2930f654d75446e423b42033d323205f0`, package `blitz-dom`.
The source tree and assets were copied unchanged before applying the bounded
patches below. Original MIT and Apache-2.0 licenses are retained. The original
workspace manifest is `Cargo.toml.upstream`; the standalone manifest expands
workspace package/dependency/feature inheritance, pins in-workspace packages
back to the same Git revision, and retains runtime dependencies and features.
Both crates.io and the Blitz Git source are patched to this one package.

- **P-HAS-001:** enable Stylo's existing `layout.css.has-selector.enabled` in
  BaseDocument initialization and invoke its relative-selector invalidator for
  snapshot state changes in `stylo.rs`. Enabling parsing alone leaves parent
  styles stale after changing the selected radio. This uses Stylo's dependency
  maps, without forcing a full-document restyle. Regressions exercise radio
  group clicks, parent restyling, and hover. Remove when upstream provides both
  parsing and invalidation and the retained state-change tests pass.
- **P-SVG-PAINT-001:** `node/serialize.rs` carries the resolved SVG paint
  cascade into the isolated SVG snapshot passed to usvg. Host stylesheets
  otherwise disappear at that boundary, turning CSS-painted slider/progress
  thumbs black. Ordinary HTML serialization stays unchanged. Regressions
  check CSS/inherited currentColor and rebuilding after a color change.
  `stylo.rs` includes SVG paint presentation attributes in the low-priority
  presentation-hint cascade, preserving fill=none, stroke attributes, author
  CSS overrides and local gradient references. The bounded property list is
  color, fill, stroke, stroke-width, fill/stroke-opacity, fill-rule and
  stroke-linecap/linejoin. This does not implement every SVG CSS property or
  external paint-server URL resolution.
  Remove when upstream preserves the cascade and these pixels pass unchanged.
- **P-INPUT-LINE-001:** `layout/mod.rs` measures text inputs as one-line
  replaced controls even when empty and without a definite min-height. It
  supplies an intrinsic line height, respects explicit CSS dimensions, and
  refreshes the editor width during real layout. This fixes collapsed dense
  inputs. Retain empty/nonempty and form/editor regressions on removal.
- **P-SCROLLBAR-ALPHA-001:** explicitly authored scrollbar colors use their
  computed alpha for visibility rather than being multiplied by a native
  scroll-activity fade timer. This allows CSS hover/fade to work before any
  scrolling. Unstyled overlay scrollbars retain upstream activity fading.
  The matching paint/interaction feature is explicitly enabled by Lapui.
  Regressions check never-scrolled hover visibility and scrollbar-color
  interpolation on enter and leave, matching Lapstyle's dual transition.

Update each entry with any further behavior change. Do not modify Cargo cache
sources. Remove these copies only after a compatible upstream group passes the
same focused regressions and real application checks without the patches.

Follow-up fixes:

- P-OVERFLOW-001: table wrapper overflow caps subtract both borders so a
  non-overflowing bordered table does not acquire a 2px scroll range. After
  layout, clamp retained element/viewport offsets to their new extents. Retain
  table-no-overflow and long-to-short content regressions on removal.
- P-SVG-PAINT-001 extension: in layout/damage.rs mark SVG reconstruction before
  propagating damage and invalidating layout caches. Percentage-sized inline
  icons must survive every frame of an inherited color hover/leave transition.
- P-TYPED-TRANSITION-001 integration: enable the existing @property parser;
  the narrowly scoped Stylo transition gate correction is tracked separately
  in vendor/stylo/LAPUI-PATCHES.md.
- P-WHEEL-001: discrete line-wheel input uses the existing smooth-scroll
  animation with a 160ms ease-out, accumulating consecutive ticks at the same
  scroll target. Precision pixel input, explicit instant scroll and scrollbar
  dragging stay direct. Preserve nested chaining and accumulation regressions.
