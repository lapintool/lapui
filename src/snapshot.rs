//! Offscreen CPU painting of the document's current state; no window or GPU.

use crate::runtime::LapuiDocument;
use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz::dom::Document;
use blitz::traits::shell::{ColorScheme, Viewport};
use image::ImageEncoder;
use serde_json::json;
use std::path::Path;
use std::time::Instant;

/// Owned CPU capture of one rendering opportunity at the current viewport.
///
/// Capture is independent of MCP and native desktop capture permissions. The
/// pixels include the document's content, but not OS chrome, the system cursor
/// or IME candidate windows. The epoch identifies the document instance, not a
/// DOM revision or an acknowledgement of physical screen presentation.
pub struct Screenshot {
    document_epoch: usize,
    width: u32,
    height: u32,
    scale_factor: f64,
    rgba: Vec<u8>,
}

impl Screenshot {
    pub fn document_epoch(&self) -> usize {
        self.document_epoch
    }

    /// Physical pixel width.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Physical pixel height.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Physical pixels per CSS pixel.
    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    /// Row-major, straight-alpha RGBA8 pixels.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Encode this capture without advancing the document again.
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                &self.rgba,
                self.width,
                self.height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|error| error.to_string())?;
        Ok(png)
    }

    /// Save this capture as PNG. Parent directories must already exist.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<(), String> {
        std::fs::write(path, self.to_png()?).map_err(|error| error.to_string())
    }
}

/// Capture on the owning document thread without changing viewport size, DPI,
/// or color scheme. Polls pending work and paints one animation opportunity;
/// callers supply their own application-ready condition. Requires the default
/// `software-renderer` feature. Captures are limited to 8192 per dimension and
/// 4 megapixels, independently of the MCP transport's PNG byte budget.
pub fn capture(document: &mut LapuiDocument) -> Result<Screenshot, String> {
    document.poll(None);
    capture_without_poll(document)
}

pub(crate) fn capture_without_poll(document: &mut LapuiDocument) -> Result<Screenshot, String> {
    let viewport = document.inner().viewport().clone();
    let (width, height) = viewport.window_size;
    validate_dimensions(width, height, 4 * 1024 * 1024)?;
    let scale_factor = viewport.scale_f64();
    let rgba = render_current_layout(document, width, height, scale_factor)?;
    Ok(Screenshot {
        document_epoch: document.inner().id(),
        width,
        height,
        scale_factor,
        rgba,
    })
}

/// Render current state at 1x scale. This resolves layout but does not wait for
/// all application/network work, and changes the document's viewport.
pub fn render_rgba(
    document: &mut LapuiDocument,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    validate_dimensions(width, height, 16 * 1024 * 1024)?;
    document.poll(None);
    document
        .inner_mut()
        .set_viewport(Viewport::new(width, height, 1.0, ColorScheme::Light));
    render_current_layout(document, width, height, 1.0)
}

/// Paint the current physical viewport without changing its size or scale.
/// This does not confirm that the native window presented the returned pixels.
pub fn render_current_rgba(document: &mut LapuiDocument) -> Result<(u32, u32, Vec<u8>), String> {
    let screenshot = capture(document)?;
    Ok((screenshot.width, screenshot.height, screenshot.rgba))
}

fn validate_dimensions(width: u32, height: u32, max_pixels: u64) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > max_pixels
    {
        return Err(format!(
            "snapshot dimensions must be 1..8192 and at most {} megapixels",
            max_pixels / (1024 * 1024)
        ));
    }
    Ok(())
}

fn render_current_layout(
    document: &mut LapuiDocument,
    width: u32,
    height: u32,
    scale: f64,
) -> Result<Vec<u8>, String> {
    let frame = document.frame_trace("image");
    let measured = frame.sequence().is_some();
    document.animation_frame();
    document.rendering_update();
    let layout_time = document.layout_animation_time();
    let layout = document.layout_trace();
    let mut dom = document.inner_mut();
    dom.resolve(layout_time);
    layout.finish(json!({"outcome":"resolved"}), false);
    let mut renderer = VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    let renderer_start = measured.then(Instant::now);
    let mut scene_millis = None;
    renderer.render_to_vec(
        |painter| {
            let start = measured.then(Instant::now);
            crate::paint::paint_scene(painter, &mut dom, scale, width, height, 0, 0);
            scene_millis = start.map(|start| start.elapsed().as_secs_f64() * 1000.0);
        },
        &mut pixels,
    );
    // Vello returns premultiplied RGBA; image encoders expect straight alpha.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 {
            pixel[..3].fill(0);
        } else if alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    frame.finish(json!({"outcome":"image_ready","width":width,"height":height,
        "sceneBuildMillis":scene_millis,"rendererCallMillis":renderer_start.map(|start|start.elapsed().as_secs_f64()*1000.0),
        "physicalPresentation":"not_applicable"}),false);
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
    fn capture_preserves_viewport_and_encodes_the_owned_frame_after_mutation() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(), None,
            "<html><head><style>html,body{margin:0;background:transparent}#box{width:40px;height:40px;background:rgba(255,0,0,0.5)}</style></head><body><div id='box'></div></body></html>", "",
        ).unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(150, 120, 1.5, ColorScheme::Dark));
        let epoch = doc.inner().id();
        let shot = capture(&mut doc).unwrap();
        assert_eq!(
            (shot.width(), shot.height(), shot.scale_factor()),
            (150, 120, 1.5)
        );
        assert_eq!(shot.document_epoch(), epoch);
        assert_eq!(doc.inner().viewport().window_size, (150, 120));
        assert_eq!(doc.inner().viewport().scale_f64(), 1.5);
        assert_eq!(doc.inner().viewport().color_scheme, ColorScheme::Dark);
        assert_eq!(shot.rgba().len(), 150 * 120 * 4);
        let pixel = (15 * 150 + 15) * 4;
        assert_eq!(shot.rgba()[pixel], 255); // Straight alpha, not premultiplied.
        assert!((127..=128).contains(&shot.rgba()[pixel + 3]));

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
        let next = capture(&mut doc).unwrap();
        assert_eq!(&next.rgba()[pixel..pixel + 4], &[0, 0, 255, 255]);
        let decoded = image::load_from_memory(&shot.to_png().unwrap())
            .unwrap()
            .into_rgba8();
        assert_eq!(decoded.dimensions(), (150, 120));
        assert_eq!(decoded.as_raw(), shot.rgba()); // Encoding keeps the captured frame.
        let path =
            std::env::temp_dir().join(format!("lapui-capture-{epoch}-{}.png", std::process::id()));
        shot.save_png(&path).unwrap();
        assert_eq!(image::open(&path).unwrap().into_rgba8(), decoded);
        std::fs::remove_file(path).unwrap();

        doc.inner_mut()
            .set_viewport(Viewport::new(8192, 8192, 1.0, ColorScheme::Light));
        assert!(capture(&mut doc).is_err());
    }
    #[test]
    fn cpu_snapshot_delivers_native_resize_before_painting_observer_changes() {
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None,
            "<html><head><style>html,body{margin:0;background:#fff}#box{width:50%;height:40px;background:#f00}</style></head><body><div id='box'></div></body></html>",
            "new ResizeObserver(entries => { document.getElementById('box').style.background = entries[0].contentRect.width >= 60 ? '#00f' : '#0f0'; }).observe(document.getElementById('box'));").unwrap();
        let pixels = render_rgba(&mut doc, 100, 80).unwrap();
        let pixel = (10 * 100 + 10) * 4;
        assert_eq!(&pixels[pixel..pixel + 4], &[0, 255, 0, 255]);
        let pixels = render_rgba(&mut doc, 140, 80).unwrap();
        let pixel = (10 * 140 + 10) * 4;
        assert_eq!(&pixels[pixel..pixel + 4], &[0, 0, 255, 255]);
        assert!(!doc.has_pending_rendering_update());
    }

    #[test]
    fn cpu_snapshot_runs_one_animation_opportunity_before_resolving_and_painting() {
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None,
            "<html><head><style>html,body{margin:0;background:#fff}#box{width:40px;height:40px;background:#f00}</style></head><body><div id='box'></div></body></html>",
            "requestAnimationFrame(() => { document.getElementById('box').style.background = '#00f'; requestAnimationFrame(() => document.getElementById('box').style.background = '#0f0'); });").unwrap();
        let pixels = render_rgba(&mut doc, 100, 80).unwrap();
        let pixel = (10 * 100 + 10) * 4;
        assert_eq!(&pixels[pixel..pixel + 4], &[0, 0, 255, 255]);
        assert!(doc.has_animation_callbacks());
        let pixels = render_rgba(&mut doc, 100, 80).unwrap();
        assert_eq!(&pixels[pixel..pixel + 4], &[0, 255, 0, 255]);
        assert!(!doc.has_animation_callbacks());
    }

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

    #[test]
    fn rounded_input_keeps_both_vertical_borders_at_fractional_dpi_scales() {
        for scale in [1.0_f32, 1.25, 1.5, 1.75] {
            let (mut doc, _) = LapuiDocument::new_with_source(
                ActionRegistry::default(),
                None,
                "<html><head><style>html,body{margin:0;background:#fff}input{position:absolute;left:20px;top:20px;width:240px;height:36px;padding:8px;border:1px solid #a6b4c6;border-radius:4px;background:#fff}</style></head><body><input id='field'></body></html>",
                "",
            )
            .unwrap();
            let width = (400.0 * scale) as u32;
            let height = (150.0 * scale) as u32;
            doc.inner_mut()
                .set_viewport(Viewport::new(width, height, scale, ColorScheme::Light));
            let pixels = capture_without_poll(&mut doc).unwrap().rgba;
            let stride = width as usize * 4;
            let has_vertical_border_near = |edge_css_x: f32| {
                let edge_px = (edge_css_x * scale).round() as i32;
                // Sample well inside the straight section, away from rounded
                // corners. A pixel just outside a rounded edge can still be
                // faintly tinted by antialiasing, so require a visible stroke
                // on several rows instead of accepting any non-white pixel.
                [8.0_f32, 16.0, 27.0, 38.0, 46.0].into_iter().all(|offset| {
                    let y = ((20.0 + offset) * scale).round() as usize;
                    (edge_px - 3..=edge_px + 3).any(|x| {
                        x >= 0
                            && (x as u32) < width
                            && pixels[y * stride + x as usize * 4..y * stride + x as usize * 4 + 3]
                                .iter()
                                .all(|channel| *channel < 240)
                    })
                })
            };

            assert!(
                has_vertical_border_near(20.0),
                "left vertical border is clipped at {scale}x"
            );
            assert!(
                has_vertical_border_near(278.0),
                "right vertical border is clipped at {scale}x"
            );

            // The correction is paint-only; hit testing and CSS geometry keep
            // the final Taffy layout exactly as resolved.
            if scale == 1.5 {
                let id = doc.inner().get_element_by_id("field").unwrap();
                assert_eq!(
                    doc.inner()
                        .get_node(id)
                        .unwrap()
                        .final_layout()
                        .border
                        .right,
                    0.0
                );
            }
        }
    }
}
