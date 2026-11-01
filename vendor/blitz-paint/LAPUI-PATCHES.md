# Blitz paint compatibility patches

This is the `blitz-paint` source at DioxusLabs/blitz commit
`8b54d6d2930f654d75446e423b42033d323205f0`, copied beside the bounded DOM
patches. Its standalone Cargo manifest expands workspace inheritance and
references the exact same pinned dependency group; `Cargo.toml.upstream`
retains the original manifest. Both upstream licenses are included.

Lapui enables the existing `scrollbars` feature.
P-RADIAL-001 changes only `src/gradient.rs`: resolve radial lengths/stops in
the correct CSS-to-device scale, then expand non-repeating gradient domains
to cover the painted tile. This preserves the outer color of a hard stop at
100%, which renderer ramps sampling [0,1) otherwise lose. Radio hover discs
became squares; tab outside-corner lengths also used the wrong DPI basis.
Retain the transparent-corner pixel tests at 1x, 1.65x and 2x on removal.

## P-PAINT-BORDER-001

`src/render.rs` uses unrounded border widths only in its local paint box. The
Lapui wrapper no longer temporarily mutates authoritative layout borders across
the whole tree. Such mutation also changed scroll ranges while scrollbars were
being painted. Generated boxes and subdocuments use the same painter naturally.
Retain fractional pseudo-border pixels and fitting/real-overflow regressions.

## P-TABLE-BACKGROUND-001

`src/render/background.rs` extends collapsed cell backgrounds to shared border
midpoints, including outside synthetic table gutters where the authored outside
border is zero. Shared edges use the same cumulative rounding as layout, before
scroll and CSS transform translations. This fixes header background strips
without changing column sizes or content/hit geometry. Separated border spacing
is unchanged. This is not full collapsed-border conflict resolution; background
image positioning and broader table conformance remain separate work.
Retain cyan-header pixel scans at five scales, real-page captures, and existing
fixed/auto/span geometry regressions. Remove these patches when a compatible
upstream group passes these checks without them.

## P-TABLE-BORDER-001

`src/render/border.rs` resolves shared cell perimeter segments independently
instead of using the first cell's top border for every edge. Honor zero/hidden
edges, cell colors, width/style precedence and row/column spans; draw above cell
backgrounds inside the scrolled content clip. This preserves plain/no-frame
tables and restores internal lines when the first top border is zero. The DOM
patch reserves gutters using all cells. This is still bounded cell-border
support, not full CSS table conflict resolution (row/column/group borders and
non-solid collapsed border rendering remain incomplete). Retain fractional-DPI
pixel tests with opaque backgrounds, removed outside edges and colspan.

## P-HOISTED-CLIP-001

Apply intermediate overflow ancestor clips when painting z-index-hoisted boxes;
the matching DOM hit test rejects points outside those clips. Retain scrolled
slider-thumb and pointer regressions. Axis-aligned rectangular clips are covered;
transformed/rounded intermediate clip chains are not yet fully implemented.

## P-STALE-STACKING-001

Skip removed child IDs left in an unresolved stacking tree, matching the DOM
hit-test guard. Retain the remove-before-layout hit-test and paint regression;
remove this guard when upstream invalidation makes stale IDs impossible.

## P-SHADOW-SPREAD-001

Outset box-shadow spread expands the corner radius as well as the box; apply
the CSS small-radius correction so a spread circular switch thumb keeps a
circular hover halo. This remains limited by upstream's averaged corner radii.
Retain the fractional-scale circle and square-corner pixel regression until an
upstream implementation handles spread correctly.
