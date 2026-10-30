# Stylo typed custom-property transition patch

Baseline: crates.io stylo 0.22.0, upstream source commit
34d25f741765a0bdb894f8271852e633299352d8, repository path `style`.
The normalized registry manifest and original Cargo.toml.orig are preserved.
Runtime source is unchanged except the condition below in servo/animation.rs.
Original source headers retain MPL-2.0; LICENSE-MPL-2.0 is the complete license
text from the repository's checksum-pinned SPDX license bundle.

P-TYPED-TRANSITION-001: PropertyDeclarationId currently classifies every custom
property as discrete (upstream TODO references bug 1885995). Servo consequently
rejects even typed/interpolable @property transitions unless transition-behavior
is allow-discrete. Let custom properties whose actual AnimationValue pair is
interpolable pass this gate. Unregistered/untyped values still fail the normal
interpolation check; non-custom discrete properties retain their behavior.

Retain the registered-length enter/leave interpolation regression and the real
radio hover check. Remove this override when a compatible upstream release
passes them without it. Lapui enables the upstream properties-and-values parser
preference in BaseDocument. It does not replace Stylo's animation engine.

Portable packaging includes these sources under sources/stylo and the full
license under licenses/stylo-MPL-2.0.txt. The registry-only source collector does
not represent this modified path dependency; preserve this local source copy
when preparing any other binary distribution.
