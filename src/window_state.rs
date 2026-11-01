use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::window::{Window, WindowAttributes};

const DEFAULT_WIDTH: f64 = 1_200.0;
const DEFAULT_HEIGHT: f64 = 800.0;
const MIN_WIDTH: f64 = 320.0;
const MIN_HEIGHT: f64 = 240.0;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 5.0;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct SavedWindowState {
    x: Option<i32>,
    y: Option<i32>,
    width: f64,
    height: f64,
    zoom: f32,
    maximized: bool,
}

impl Default for SavedWindowState {
    fn default() -> Self {
        Self {
            x: None,
            y: None,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            zoom: 1.0,
            maximized: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct WindowStateStore {
    path: PathBuf,
    saved: SavedWindowState,
}

impl WindowStateStore {
    pub fn load_default() -> Self {
        Self::load_from(state_path())
    }

    fn load_from(path: PathBuf) -> Self {
        let saved = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { path, saved }
    }

    pub fn zoom(&self) -> f32 {
        self.saved.zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    }

    pub fn window_attributes(&self) -> WindowAttributes {
        let mut attributes = WindowAttributes::default()
            .with_title("Lapui")
            .with_decorations(false)
            .with_resizable(true)
            .with_min_surface_size(LogicalSize::new(MIN_WIDTH, MIN_HEIGHT))
            .with_surface_size(LogicalSize::new(
                sane_dimension(self.saved.width, DEFAULT_WIDTH, MIN_WIDTH),
                sane_dimension(self.saved.height, DEFAULT_HEIGHT, MIN_HEIGHT),
            ))
            .with_maximized(self.saved.maximized);
        // Keep custom title-bar contents, but let DWM own the native outline,
        // shadow and corner clipping (including square snapped/maximized edges).
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::{Color, CornerPreference, WindowAttributesWindows};
            attributes = attributes.with_platform_attributes(Box::new(
                WindowAttributesWindows::default()
                    .with_undecorated_shadow(true)
                    .with_border_color(Some(Color::SYSTEM_DEFAULT))
                    .with_corner_preference(CornerPreference::Round),
            ));
        }
        if let (Some(x), Some(y)) = (self.saved.x, self.saved.y) {
            attributes = attributes.with_position(PhysicalPosition::new(x, y));
        }
        attributes
    }

    pub fn capture(&mut self, window: &dyn Window, zoom: f32) {
        let maximized = window.is_maximized();
        self.saved.maximized = maximized;
        if window.is_minimized() == Some(true) {
            return;
        }
        if !maximized {
            if let Ok(position) = window.outer_position() {
                self.saved.x = Some(position.x);
                self.saved.y = Some(position.y);
            }
            let size = window
                .surface_size()
                .to_logical::<f64>(window.scale_factor());
            if size.width.is_finite() && size.width >= MIN_WIDTH {
                self.saved.width = size.width;
            }
            if size.height.is_finite() && size.height >= MIN_HEIGHT {
                self.saved.height = size.height;
            }
        }
        if zoom.is_finite() {
            self.saved.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        }
    }

    pub fn save(&self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!("Lapui could not create window settings directory: {error}");
            return;
        }
        match serde_json::to_vec_pretty(&self.saved) {
            Ok(bytes) => {
                if let Err(error) = std::fs::write(&self.path, bytes) {
                    eprintln!("Lapui could not save window settings: {error}");
                }
            }
            Err(error) => eprintln!("Lapui could not encode window settings: {error}"),
        }
    }
}

fn sane_dimension(value: f64, fallback: f64, minimum: f64) -> f64 {
    if value.is_finite() && value >= minimum && value <= 16_384.0 {
        value
    } else {
        fallback
    }
}

fn state_path() -> PathBuf {
    if let Some(path) = std::env::var_os("LAPUI_WINDOW_STATE_FILE") {
        return path.into();
    }
    #[cfg(target_os = "windows")]
    if let Some(root) = std::env::var_os("APPDATA") {
        return PathBuf::from(root).join("lapui").join("window-state.json");
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("lapui")
            .join("window-state.json");
    }
    #[cfg(not(target_os = "windows"))]
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(config)
            .join("lapui")
            .join("window-state.json");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".config")
        .join("lapui")
        .join("window-state.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_is_sanitized_and_window_dimensions_have_safe_defaults() {
        let path =
            std::env::temp_dir().join(format!("lapui-window-state-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"x":-120,"y":40,"width":1,"height":900,"zoom":99,"maximized":true}"#,
        )
        .unwrap();
        let state = WindowStateStore::load_from(path.clone());
        assert_eq!(state.zoom(), MAX_ZOOM);
        let attributes = state.window_attributes();
        assert_eq!(
            attributes.position,
            Some(PhysicalPosition::new(-120, 40).into())
        );
        assert_eq!(
            attributes.surface_size,
            Some(LogicalSize::new(DEFAULT_WIDTH, 900.0).into())
        );
        assert!(attributes.maximized);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_saved_state_falls_back_to_default() {
        let path = std::env::temp_dir().join(format!(
            "lapui-window-state-invalid-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, b"not json").unwrap();
        let state = WindowStateStore::load_from(path.clone());
        assert_eq!(state.zoom(), 1.0);
        let attributes = state.window_attributes();
        assert!(attributes.position.is_none());
        assert_eq!(
            attributes.surface_size,
            Some(LogicalSize::new(DEFAULT_WIDTH, DEFAULT_HEIGHT).into())
        );
        std::fs::remove_file(path).unwrap();
    }
}
