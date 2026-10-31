use blitz::dom::BaseDocument;

/// Build a scene without changing authoritative layout or scroll geometry.
/// Fractional border widths are handled locally by the painter's CSS box.
pub(crate) fn paint_scene(
    scene: &mut impl anyrender::PaintScene,
    document: &mut BaseDocument,
    scale: f64,
    width: u32,
    height: u32,
    x_offset: u32,
    y_offset: u32,
) {
    blitz_paint::paint_scene(scene, document, scale, width, height, x_offset, y_offset);
}
