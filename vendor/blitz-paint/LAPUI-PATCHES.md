# Blitz paint baseline

This is the unchanged `blitz-paint` source at DioxusLabs/blitz commit
`8b54d6d2930f654d75446e423b42033d323205f0`, copied beside the bounded DOM
patches. Its standalone Cargo manifest expands workspace inheritance and
references the exact same pinned dependency group; `Cargo.toml.upstream`
retains the original manifest. Both upstream licenses are included.

Lapui enables the existing `scrollbars` feature. The drawing source is retained
unchanged to keep the local DOM and paint package sources explicit and unified.
Fractional-border compensation stays in Lapui `src/paint.rs`, and now also
visits generated before/after boxes. Remove this directory and its Cargo source
override when removing the associated local DOM group.
