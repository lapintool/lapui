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
