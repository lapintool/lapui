//! Offscreen CPU painting of the document's current state; no window or GPU.

use crate::runtime::LapuiDocument;
use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz::dom::Document;
use blitz::traits::shell::{ColorScheme, Viewport};
use std::path::Path;

/// Render current state at 1x scale. This resolves layout but does not wait for
/// all application/network work, and changes the document's viewport.
pub fn render_rgba(
    document: &mut LapuiDocument,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > 16 * 1024 * 1024
    {
        return Err("snapshot dimensions must be 1..8192 and at most 16 megapixels".into());
    }
    document.poll(None);
    let mut dom = document.inner_mut();
    dom.set_viewport(Viewport::new(width, height, 1.0, ColorScheme::Light));
    dom.resolve(0.0);
    let mut renderer = VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    renderer.render_to_vec(
        |painter| blitz_paint::paint_scene(painter, &mut dom, 1.0, width, height, 0, 0),
        &mut pixels,
    );
    // Vello returns premultiplied RGBA; image encoders expect straight alpha.
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 {
            pixel[..3].fill(0);
        } else if alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Ok(pixels)
}

pub fn save_png(
    document: &mut LapuiDocument,
    path: impl AsRef<Path>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let pixels = render_rgba(document, width, height)?;
    image::save_buffer_with_format(
        path,
        &pixels,
        width,
        height,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::ActionRegistry;
    #[test]
    fn cpu_paint_outputs_css_pixels_and_changes_after_dom_mutation_and_resize() {
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None,
            "<html><head><style>html,body{margin:0;background:#ffffff;width:100%;height:100%}#box{width:40px;height:40px;background:#ff0000}</style></head><body><div id='box'></div></body></html>", "").unwrap();
        let pixels = render_rgba(&mut doc, 100, 80).unwrap();
        let at = |pixels: &[u8], width: usize, x: usize, y: usize| {
            pixels[(y * width + x) * 4..(y * width + x) * 4 + 4].to_vec()
        };
        assert_eq!(pixels.len(), 100 * 80 * 4);
        assert_eq!(at(&pixels, 100, 10, 10), [255, 0, 0, 255]);
        assert_eq!(at(&pixels, 100, 70, 60), [255, 255, 255, 255]);
        let id = doc.inner().get_element_by_id("box").unwrap();
        doc.inner_mut().mutate().set_attribute(
            id,
            blitz::dom::QualName::new(
                None,
                Default::default(),
                blitz::dom::LocalName::from("style"),
            ),
            "background:#0000ff",
        );
        let pixels = render_rgba(&mut doc, 120, 90).unwrap();
        assert_eq!(at(&pixels, 120, 10, 10), [0, 0, 255, 255]);
        assert_eq!(at(&pixels, 120, 110, 80), [255, 255, 255, 255]);
        assert!(render_rgba(&mut doc, 0, 90).is_err());
    }
}
