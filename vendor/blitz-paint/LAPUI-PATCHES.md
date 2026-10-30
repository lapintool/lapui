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
Fractional-border compensation stays in Lapui `src/paint.rs`, and now also
visits generated before/after boxes. Remove this directory and its Cargo source
override when a compatible upstream group passes those retained tests.
