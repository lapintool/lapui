# Taffy source baseline and bounded backports

This directory contains the runtime sources of Taffy 0.14.0 at
`ceb7443d386fc5cc67a1f72b51f7bf505439dff8` from
https://github.com/DioxusLabs/taffy. The original MIT license is in `LICENSE`.
`Cargo.toml` retains the original package, dependencies and features; upstream
development dependencies, benchmark/example targets and workspace members are
omitted because those trees are not distributed here. There are no changes to
the dependency API or enabled feature defaults.

## P-GRID-CALC-001

- Location: `src/style/grid.rs`, `TrackSizingFunction::has_fixed_component`.
- Backport of upstream commit
  [`63779e56f482140d794b980734cbd2db45857f7a`](https://github.com/DioxusLabs/taffy/commit/63779e56f482140d794b980734cbd2db45857f7a)
  (PR #1320), adapted to this baseline's CompactLength representation.
- Accept calc-backed length/percentage tracks, including `min()`, `max()` and
  `clamp()`, as fixed components in auto-repeat Grid templates. Previously
  `repeat(auto-fit,minmax(min(280px,100%),1fr))` was rejected and collapsed to
  implicit placement instead of producing responsive columns.
- Regression: Lapui's
  `auto_fit_grid_accepts_css_math_tracks_and_reflows_on_resize` checks two
  columns at 1100 px and one column at 240 px. The real Lapstyle UI integration
  test checks table examples in addition to mounting all component pages.
- Removal: upgrade the Blitz/Taffy compatibility group to a pinned version
  containing #1320, remove the root Cargo patch and this directory, then run
  the same regression, full application check, and CPU/GPU visual acceptance.

## P-GRID-BLOCK-001

- Location: `src/compute/grid/track_sizing.rs`, expansion-space classification.
- Backport of upstream commit
  [`e2fbe56495ea4c033329f2ed2e61c4cf0dee8154`](https://github.com/DioxusLabs/taffy/commit/e2fbe56495ea4c033329f2ed2e61c4cf0dee8154)
  (PR #1284).
- Local extension to the same rule: map a block-axis min-content constraint
  to max-content for the preceding finite-growth-limit maximization step as
  well. Upstream #1284 alone fixes fractional rows, but the auto-row regression
  still collapsed from 400 px to 200 px. The extension covers the auto rows
  used by the real API table grid. It is not part of the upstream commit.
- A grid container's intrinsic min-content block size uses its max-content
  block size. Preserve a min-content constraint only in the inline axis.
  Without this rule an auto-height grid inside a constrained column flexbox
  can collapse below its overflowing table/flex children and overlap following
  content. Both auto and fractional rows are affected.
- Regressions: Lapui's
  `grid_flex_items_preserve_intrinsic_block_height_unless_min_height_is_zero`
  checks auto and fractional rows, and explicitly allowing shrink with
  `min-height:0`. The full Lapstyle UI test checks both API table columns fit
  their grid and the following text starts below it.
- Removal: adopt a compatible upstream dependency group containing #1284,
  remove this backport, and retain these tests plus real-app screenshots.

Do not modify the Cargo cache or update these sources in place without updating
this record. Additional patches require a separate ID, failing reproduction,
upstream reference where available, and removal condition.

## P-OVERFLOW-ROUND-001

- Location: `src/compute/mod.rs`, `round_layout` overflow-rectangle origin.
- Overflow extents are padding-box-relative; rounding them against the border
  box origin disagreed with independently rounded borders. A fitting box could
  gain a 1 CSS px scroll range depending on its fractional position and size.
- Round overflow coordinates from the unrounded padding-box start instead.
  This retains real overflow; it does not impose a blanket small-range cutoff.
- Regressions: fractional bordered fitting/overflowing boxes, and the actual
  Lapstyle Button page at 30 viewport/DPI combinations. Verify both metrics
  and painting, and retain actual short/long scroll-container coverage.
- Removal: remove after a compatible upstream rounding implementation passes
  the same cases without this change. No upstream fix is claimed here.
