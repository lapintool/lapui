//! Rendering-opportunity callbacks around Blitz's existing window application.
use crate::reload::ReloadDocument;
use crate::runtime::LapuiDocument;
use crate::window_state::WindowStateStore;
use anyrender::WindowRenderer;
use blitz::dom::Document;
use blitz::shell::{BlitzApplication, BlitzShellEvent, BlitzShellProxy, View, WindowConfig};
use serde_json::json;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::cursor::CursorIcon;
use winit::event::{
    ButtonSource, DeviceEvent, DeviceId, MouseButton, PointerSource, StartCause, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::PhysicalKey;
use winit::window::{ResizeDirection, WindowId};

const WINDOW_STATE_DEBOUNCE: Duration = Duration::from_millis(350);

#[derive(Clone, Debug)]
pub(crate) struct WindowCommand {
    pub document_id: usize,
    pub action: String,
}

pub(crate) const WINDOW_CHROME_SCRIPT: &str = r#"(() => {
  if (!document.body || document.getElementById('lapui-window-chrome')) return;
  const style = document.createElement('style');
  style.textContent = `
    #lapui-window-chrome { position: fixed !important; z-index: 2147483647 !important; inset: 0 0 auto 0 !important; height: 34px !important; display: flex !important; align-items: center !important; box-sizing: border-box !important; padding-left: 12px !important; background: #202124 !important; color: #e8eaed !important; border-bottom: 1px solid #303134 !important; font: 12px/1.2 "Segoe UI", sans-serif !important; user-select: none !important; }
    #lapui-window-chrome-title { flex: 1 !important; overflow: hidden !important; white-space: nowrap !important; text-overflow: ellipsis !important; opacity: .9 !important; }
    .lapui-window-button { width: 46px !important; height: 33px !important; display: inline-flex !important; align-items: center !important; justify-content: center !important; border: 0 !important; border-radius: 0 !important; padding: 0 !important; margin: 0 !important; background: transparent !important; color: inherit !important; font: 16px/1 "Segoe UI Symbol", sans-serif !important; cursor: default !important; }
    .lapui-window-button:hover { background: #3a3b3d !important; }
    .lapui-window-button[data-action="close"]:hover { background: #c42b1c !important; color: white !important; }
  `;
  document.body.appendChild(style);
  const bar = document.createElement('div');
  bar.id = 'lapui-window-chrome';
  bar.setAttribute('role', 'toolbar');
  bar.setAttribute('aria-label', 'Lapui window controls');
  const title = document.createElement('span');
  title.id = 'lapui-window-chrome-title';
  title.textContent = document.title || 'Lapui';
  bar.appendChild(title);
  const controls = [
    ['minimize', '−', 'Minimize'],
    ['toggle-maximize', '□', 'Maximize or restore'],
    ['close', '×', 'Close'],
  ];
  for (const [action, glyph, label] of controls) {
    const button = document.createElement('button');
    button.className = 'lapui-window-button';
    button.type = 'button';
    button.setAttribute('data-action', action);
    button.setAttribute('aria-label', label);
    button.setAttribute('title', label);
    button.textContent = glyph;
    button.addEventListener('click', event => {
      event.stopPropagation();
      window.__lapui_window_command(action);
    });
    bar.appendChild(button);
  }
  bar.addEventListener('pointerdown', event => {
    if (event.button !== 0 || event.target.getAttribute('data-action')) return;
    window.__lapui_window_command('drag');
  });
  bar.addEventListener('dblclick', event => {
    if (!event.target.getAttribute('data-action')) window.__lapui_window_command('toggle-maximize');
  });
  document.body.insertBefore(bar, document.body.firstChild);
  document.body.style.setProperty('box-sizing', 'border-box', 'important');
  document.body.style.setProperty('padding-top', '34px', 'important');
})();"#;

/// Keeps Blitz's layout, painting and OS handling, adding a JS frame checkpoint
/// immediately before visible, active-window redraws. Embedders should use this
/// application instead of bare BlitzApplication to drive animation callbacks.
pub struct LapuiApplication<R: WindowRenderer> {
    pub inner: BlitzApplication<R>,
    proxy: BlitzShellProxy,
    frames: HashMap<WindowId, Instant>,
    refresh_intervals: HashMap<WindowId, Duration>,
    native_resize_releases: HashSet<WindowId>,
    restored_zoom: HashSet<WindowId>,
    window_state: WindowStateStore,
    save_deadline: Option<Instant>,
    wait: AnimationWait,
}

fn frame_document(document: &mut dyn Document) -> Option<&mut LapuiDocument> {
    let any = document as &mut dyn Any;
    if any.is::<ReloadDocument>() {
        return any
            .downcast_mut::<ReloadDocument>()
            .map(ReloadDocument::document_for_frame);
    }
    any.downcast_mut::<LapuiDocument>()
}

impl<R: WindowRenderer> LapuiApplication<R> {
    pub fn new(proxy: BlitzShellProxy, events: Receiver<BlitzShellEvent>) -> Self {
        Self::with_window_state(proxy, events, WindowStateStore::load_default())
    }

    pub fn with_window_state(
        proxy: BlitzShellProxy,
        events: Receiver<BlitzShellEvent>,
        window_state: WindowStateStore,
    ) -> Self {
        Self {
            proxy: proxy.clone(),
            inner: BlitzApplication::new(proxy, events),
            frames: HashMap::new(),
            refresh_intervals: HashMap::new(),
            native_resize_releases: HashSet::new(),
            restored_zoom: HashSet::new(),
            window_state,
            save_deadline: None,
            wait: AnimationWait::default(),
        }
    }
    pub fn add_window(&mut self, config: WindowConfig<R>) {
        self.inner.add_window(config);
    }
}

impl<R: WindowRenderer> ApplicationHandler for LapuiApplication<R> {
    fn new_events(&mut self, event_loop: &dyn ActiveEventLoop, cause: StartCause) {
        self.inner.new_events(event_loop, cause);
    }
    fn device_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        id: Option<DeviceId>,
        event: DeviceEvent,
    ) {
        self.inner.device_event(event_loop, id, event);
    }
    fn memory_warning(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.memory_warning(event_loop);
    }
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.can_create_surfaces(event_loop);
        for (id, view) in &mut self.inner.windows {
            if self.restored_zoom.insert(*id) {
                view.doc
                    .inner_mut()
                    .viewport_mut()
                    .set_zoom(self.window_state.zoom());
                view.request_redraw();
            }
        }
    }
    fn destroy_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.frames.clear();
        self.refresh_intervals.clear();
        self.native_resize_releases.clear();
        self.inner.destroy_surfaces(event_loop);
    }
    fn resumed(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.resumed(event_loop);
    }
    fn suspended(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.suspended(event_loop);
    }
    fn proxy_wake_up(&mut self, event_loop: &dyn ActiveEventLoop) {
        while let Ok(event) = self.inner.event_queue.try_recv() {
            match event {
                BlitzShellEvent::Embedder(payload) => {
                    if let Some(command) = payload.downcast_ref::<WindowCommand>().cloned() {
                        self.handle_window_command(event_loop, command);
                    } else {
                        self.inner.handle_blitz_shell_event(
                            event_loop,
                            BlitzShellEvent::Embedder(payload),
                        );
                    }
                }
                event => self.inner.handle_blitz_shell_event(event_loop, event),
            }
        }
    }
    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.about_to_wait(event_loop);
        self.frames
            .retain(|id, _| self.inner.windows.contains_key(id));
        self.refresh_intervals
            .retain(|id, _| self.inner.windows.contains_key(id));
        self.native_resize_releases
            .retain(|id| self.inner.windows.contains_key(id));
        self.restored_zoom
            .retain(|id| self.inner.windows.contains_key(id));
        let now = Instant::now();
        if self.save_deadline.is_some_and(|deadline| deadline <= now) {
            self.save_window_state();
        }
        let mut earliest = self.save_deadline;
        for (id, view) in &mut self.inner.windows {
            if !view.is_visible
                || !view.renderer.is_active()
                || !frame_document(view.doc.as_mut()).is_some_and(|document| {
                    document.has_animation_callbacks() || document.has_pending_rendering_update()
                })
            {
                continue;
            }
            let interval = *self
                .refresh_intervals
                .entry(*id)
                .or_insert_with(|| frame_interval(view));
            let due = self.frames.get(id).map_or(now, |last| *last + interval);
            let wake = if due <= now {
                view.request_redraw();
                // A slow/inactive platform must not turn an overdue frame into
                // a WaitUntil deadline in the past and spin the event loop.
                now + interval
            } else {
                due
            };
            earliest = Some(earliest.map_or(wake, |old: Instant| old.min(wake)));
        }
        event_loop.set_control_flow(self.wait.update(event_loop.control_flow(), earliest));
    }

    fn window_event(&mut self, event_loop: &dyn ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // The native resize loop may consume the release or deliver it later.
        // Never forward an unmatched release to Blitz (it toggles button bits).
        if let WindowEvent::PointerButton {
            button: ButtonSource::Mouse(MouseButton::Left),
            state,
            ..
        } = &event
        {
            if self.native_resize_releases.remove(&id) && !state.is_pressed() {
                return;
            }
        }
        if matches!(event, WindowEvent::RedrawRequested) {
            let now = Instant::now();
            if let Some(view) = self.inner.windows.get_mut(&id) {
                if view.is_visible
                    && view.renderer.is_active()
                    && frame_document(view.doc.as_mut()).is_some()
                {
                    let interval = *self
                        .refresh_intervals
                        .entry(id)
                        .or_insert_with(|| frame_interval(view));
                    if !frame_is_due(self.frames.get(&id).copied(), now, interval) {
                        // Coalesce redraws as well as callbacks. Repainting here
                        // can otherwise spin native CSS animations faster than
                        // the display refresh rate and delay queued input. about_to_wait schedules
                        // this deferred opportunity at the next frame deadline.
                        frame_document(view.doc.as_mut())
                            .unwrap()
                            .defer_rendering_update();
                        return;
                    }
                    self.frames.insert(id, now);
                    redraw_document(view);
                    self.proxy
                        .send_event(BlitzShellEvent::Poll { window_id: id });
                    return;
                }
            }
        }
        if matches!(event, WindowEvent::CloseRequested) {
            self.save_window_state_for(id);
            self.frames.remove(&id);
            self.refresh_intervals.remove(&id);
            self.native_resize_releases.remove(&id);
            self.restored_zoom.remove(&id);
            self.inner.window_event(event_loop, id, event);
            return;
        }
        if let Some(zoom) = zoom_shortcut(&self.inner, id, &event) {
            if let Some(view) = self.inner.windows.get_mut(&id) {
                view.doc.inner_mut().viewport_mut().set_zoom(zoom);
                view.request_redraw();
            }
            self.schedule_state_save();
            return;
        }
        // Native surface hit testing takes precedence over all DOM stacking and
        // application handlers. OS logical pixels keep this independent of zoom.
        if let Some(view) = self.inner.windows.get_mut(&id) {
            let window = &view.window;
            let enabled = window.is_resizable()
                && !window.is_decorated()
                && !window.is_maximized()
                && window.fullscreen().is_none();
            let size = window.surface_size();
            let direction_at = |position: winit::dpi::PhysicalPosition<f64>| {
                enabled
                    .then(|| {
                        resize_direction(
                            position.x,
                            position.y,
                            size.width as f64,
                            size.height as f64,
                            window.scale_factor(),
                        )
                    })
                    .flatten()
            };
            match &event {
                WindowEvent::PointerMoved {
                    position,
                    source: PointerSource::Mouse,
                    ..
                } if view.buttons == blitz::traits::events::MouseEventButtons::None => {
                    if let Some(direction) = direction_at(*position) {
                        view.pointer_pos = *position;
                        window.set_cursor(resize_cursor(direction).into());
                        return;
                    }
                }
                WindowEvent::PointerButton {
                    position,
                    button: ButtonSource::Mouse(MouseButton::Left),
                    state,
                    ..
                } if state.is_pressed()
                    && view.buttons == blitz::traits::events::MouseEventButtons::None =>
                {
                    if let Some(direction) = direction_at(*position) {
                        if window.drag_resize_window(direction).is_ok() {
                            self.native_resize_releases.insert(id);
                            return;
                        }
                    }
                }
                _ => {}
            }
        }
        let restore_cursor = matches!(
            event,
            WindowEvent::PointerMoved {
                source: PointerSource::Mouse,
                ..
            }
        );
        let state_changed = matches!(
            event,
            WindowEvent::Moved(_)
                | WindowEvent::SurfaceResized(_)
                | WindowEvent::ScaleFactorChanged { .. }
        );
        self.inner.window_event(event_loop, id, event);
        if restore_cursor {
            if let Some(view) = self.inner.windows.get(&id) {
                view.window
                    .set_cursor(view.doc.inner().get_cursor().unwrap_or_default().into());
            }
        }
        if state_changed {
            self.refresh_intervals.remove(&id);
            self.schedule_state_save();
        }
    }

    #[cfg(target_os = "macos")]
    fn macos_handler(
        &mut self,
    ) -> Option<&mut dyn winit::platform::macos::ApplicationHandlerExtMacOS> {
        self.inner.macos_handler()
    }
}

impl<R: WindowRenderer> LapuiApplication<R> {
    fn schedule_state_save(&mut self) {
        self.save_deadline = Some(Instant::now() + WINDOW_STATE_DEBOUNCE);
    }

    fn save_window_state(&mut self) {
        self.save_deadline = None;
        if let Some(view) = self.inner.windows.values().next() {
            let zoom = view.doc.inner().viewport().zoom();
            self.window_state.capture(&*view.window, zoom);
            self.window_state.save();
        }
    }

    fn save_window_state_for(&mut self, id: WindowId) {
        if let Some(view) = self.inner.windows.get(&id) {
            let zoom = view.doc.inner().viewport().zoom();
            self.window_state.capture(&*view.window, zoom);
            self.window_state.save();
            self.save_deadline = None;
        }
    }

    fn handle_window_command(&mut self, event_loop: &dyn ActiveEventLoop, command: WindowCommand) {
        let Some(window_id) = self
            .inner
            .windows
            .iter()
            .find_map(|(id, view)| (view.doc.id() == command.document_id).then_some(*id))
        else {
            return;
        };
        match command.action.as_str() {
            "minimize" => {
                if let Some(view) = self.inner.windows.get(&window_id) {
                    view.window.set_minimized(true);
                }
            }
            "toggle-maximize" => {
                if let Some(view) = self.inner.windows.get(&window_id) {
                    view.window.set_maximized(!view.window.is_maximized());
                }
                self.schedule_state_save();
            }
            "close" => {
                self.save_window_state_for(window_id);
                self.inner.handle_blitz_shell_event(
                    event_loop,
                    BlitzShellEvent::CloseWindow { window_id },
                );
            }
            "drag" => {
                if let Some(view) = self.inner.windows.get(&window_id) {
                    let _ = view.window.drag_window();
                }
            }
            action => {
                let direction = match action {
                    "resize-n" => Some(ResizeDirection::North),
                    "resize-ne" => Some(ResizeDirection::NorthEast),
                    "resize-e" => Some(ResizeDirection::East),
                    "resize-se" => Some(ResizeDirection::SouthEast),
                    "resize-s" => Some(ResizeDirection::South),
                    "resize-sw" => Some(ResizeDirection::SouthWest),
                    "resize-w" => Some(ResizeDirection::West),
                    "resize-nw" => Some(ResizeDirection::NorthWest),
                    _ => None,
                };
                if let (Some(direction), Some(view)) =
                    (direction, self.inner.windows.get(&window_id))
                {
                    let _ = view.window.drag_resize_window(direction);
                }
            }
        }
    }
}

fn resize_direction(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
) -> Option<ResizeDirection> {
    if !scale.is_finite()
        || scale <= 0.0
        || !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x >= width
        || y >= height
    {
        return None;
    }
    let edge = 5.0 * scale;
    let corner = 10.0 * scale;
    let left = x < edge;
    let right = x >= width - edge;
    let top = y < edge;
    let bottom = y >= height - edge;
    if (left && y < corner) || (top && x < corner) {
        Some(ResizeDirection::NorthWest)
    } else if (right && y < corner) || (top && x >= width - corner) {
        Some(ResizeDirection::NorthEast)
    } else if (left && y >= height - corner) || (bottom && x < corner) {
        Some(ResizeDirection::SouthWest)
    } else if (right && y >= height - corner) || (bottom && x >= width - corner) {
        Some(ResizeDirection::SouthEast)
    } else if left {
        Some(ResizeDirection::West)
    } else if right {
        Some(ResizeDirection::East)
    } else if top {
        Some(ResizeDirection::North)
    } else if bottom {
        Some(ResizeDirection::South)
    } else {
        None
    }
}

fn resize_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
    }
}

fn zoom_shortcut<R: WindowRenderer>(
    application: &BlitzApplication<R>,
    id: WindowId,
    event: &WindowEvent,
) -> Option<f32> {
    let WindowEvent::KeyboardInput { event, .. } = event else {
        return None;
    };
    if !event.state.is_pressed() {
        return None;
    }
    let view = application.windows.get(&id)?;
    let modifiers = view.keyboard_modifiers.state();
    if !modifiers.control_key() && !modifiers.meta_key() {
        return None;
    }
    let PhysicalKey::Code(code) = event.physical_key else {
        return None;
    };
    zoom_from_key(code, view.doc.inner().viewport().zoom())
}

fn zoom_from_key(code: winit::keyboard::KeyCode, current: f32) -> Option<f32> {
    match code {
        winit::keyboard::KeyCode::Equal | winit::keyboard::KeyCode::NumpadAdd => {
            Some((current + 0.1).min(5.0))
        }
        winit::keyboard::KeyCode::Minus | winit::keyboard::KeyCode::NumpadSubtract => {
            Some((current - 0.1).max(0.25))
        }
        winit::keyboard::KeyCode::Digit0 | winit::keyboard::KeyCode::Numpad0 => Some(1.0),
        _ => None,
    }
}

#[cfg(test)]
mod window_chrome_tests {
    use super::*;
    use winit::keyboard::KeyCode;

    #[test]
    fn native_resize_targets_cover_all_edges_corners_and_fractional_dpi() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for (x, y, direction) in [
                (1.0, 1.0, ResizeDirection::NorthWest),
                (400.0, 1.0, ResizeDirection::North),
                (799.0, 1.0, ResizeDirection::NorthEast),
                (799.0, 300.0, ResizeDirection::East),
                (799.0, 599.0, ResizeDirection::SouthEast),
                (400.0, 599.0, ResizeDirection::South),
                (1.0, 599.0, ResizeDirection::SouthWest),
                (1.0, 300.0, ResizeDirection::West),
            ] {
                assert_eq!(
                    resize_direction(x * scale, y * scale, 800.0 * scale, 600.0 * scale, scale),
                    Some(direction)
                );
            }
            assert_eq!(
                resize_direction(
                    400.0 * scale,
                    300.0 * scale,
                    800.0 * scale,
                    600.0 * scale,
                    scale
                ),
                None
            );
            assert_eq!(resize_direction(-1.0, 10.0, 800.0, 600.0, scale), None);
        }
    }

    #[test]
    fn redraw_pacing_accepts_vsync_jitter_without_skipping_a_refresh() {
        let now = Instant::now();
        for interval in [FRAME_INTERVAL, Duration::from_nanos(6_944_444)] {
            assert!(frame_is_due(
                Some(now),
                now + interval - Duration::from_micros(400),
                interval
            ));
            assert!(!frame_is_due(Some(now), now + interval / 2, interval));
        }
    }

    #[test]
    fn browser_zoom_keys_cover_main_and_keypad_keys_with_bounds_and_reset() {
        assert!((zoom_from_key(KeyCode::Equal, 1.0).unwrap() - 1.1).abs() < 0.0001);
        assert!((zoom_from_key(KeyCode::NumpadAdd, 1.0).unwrap() - 1.1).abs() < 0.0001);
        assert!((zoom_from_key(KeyCode::Minus, 1.0).unwrap() - 0.9).abs() < 0.0001);
        assert!((zoom_from_key(KeyCode::NumpadSubtract, 1.0).unwrap() - 0.9).abs() < 0.0001);
        assert_eq!(zoom_from_key(KeyCode::NumpadSubtract, 0.25), Some(0.25));
        assert_eq!(zoom_from_key(KeyCode::Equal, 5.0), Some(5.0));
        assert_eq!(zoom_from_key(KeyCode::Digit0, 1.7), Some(1.0));
        assert_eq!(zoom_from_key(KeyCode::Numpad0, 1.7), Some(1.0));
        assert_eq!(zoom_from_key(KeyCode::KeyA, 1.0), None);
    }
}

// Native engine resolve/paint through public APIs, retaining Blitz's resource
// and visibility gates. Renderer return is not a display-server presentation ack.
fn redraw_document<R: WindowRenderer>(view: &mut View<R>) {
    let layout_time = view.current_animation_time();
    let document = frame_document(view.doc.as_mut()).unwrap();
    let frame = document.frame_trace("window");
    let measured = frame.sequence().is_some();
    let callbacks_start = measured.then(Instant::now);
    document.animation_frame_at(layout_time);
    document.rendering_update_at(layout_time);
    let callbacks_millis = callbacks_start.map(|start| start.elapsed().as_secs_f64() * 1000.0);
    let layout = document.layout_trace();
    let mut dom = view.doc.inner_mut();
    dom.resolve(layout_time);
    layout.finish(json!({"outcome":"resolved"}), false);
    let (width, height) = dom.viewport().window_size;
    let scale = dom.viewport().scale_f64();
    let blocked = dom.has_pending_critical_resources();
    let animating = dom.is_animating();
    let insets = view.safe_area_insets;
    let mut scene_millis = None;
    let mut scene_built = false;
    let renderer_start = measured.then(Instant::now);
    if !blocked {
        view.renderer.render(|scene| {
            let start = measured.then(Instant::now);
            crate::paint::paint_scene(
                scene,
                &mut dom,
                scale,
                width,
                height,
                insets.left,
                insets.top,
            );
            scene_millis = start.map(|start| start.elapsed().as_secs_f64() * 1000.0);
            scene_built = true;
        });
    }
    let renderer_millis = renderer_start.map(|start| start.elapsed().as_secs_f64() * 1000.0);
    drop(dom);
    frame_document(view.doc.as_mut())
        .unwrap()
        .complete_render_revision(!blocked && scene_built);
    frame.finish(json!({"outcome":if blocked{"blocked_resources"}else if scene_built{"renderer_returned"}else{"renderer_no_scene"},
        "callbackPhaseMillis":callbacks_millis,"callbacksDue":true,"sceneBuildMillis":scene_millis,
        "rendererCallMillis":if blocked{None}else{renderer_millis},"width":width,"height":height,
        "physicalPresentation":"unknown"}),false);
    if !blocked && animating {
        view.request_redraw();
    }
}

// Pace against the current display. A small scheduling tolerance prevents a
// VSync event arriving microseconds early from being deferred a whole refresh.
// This remains a soft opportunity limit, not a presentation acknowledgement.
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

fn frame_interval<R: WindowRenderer>(view: &View<R>) -> Duration {
    let rate = view
        .window
        .current_monitor()
        .and_then(|monitor| monitor.current_video_mode())
        .and_then(|mode| mode.refresh_rate_millihertz())
        .map(|rate| rate.get());
    rate.filter(|rate| (30_000..=240_000).contains(rate))
        .map_or(FRAME_INTERVAL, |rate| {
            Duration::from_secs_f64(1000.0 / rate as f64)
        })
}

fn frame_is_due(last: Option<Instant>, now: Instant, interval: Duration) -> bool {
    let tolerance = Duration::from_millis(1).min(interval / 8);
    last.is_none_or(|last| now.saturating_duration_since(last) + tolerance >= interval)
}

#[derive(Default)]
struct AnimationWait {
    installed: Option<(ControlFlow, ControlFlow)>,
}

impl AnimationWait {
    fn update(&mut self, current: ControlFlow, deadline: Option<Instant>) -> ControlFlow {
        // Restore only our own deadline. Preserve a control-flow change made by
        // an embedder, including Poll or an earlier independent wake deadline.
        let base = self
            .installed
            .take()
            .filter(|(_, installed)| *installed == current)
            .map_or(current, |(base, _)| base);
        let next = match (base, deadline) {
            (ControlFlow::Wait, Some(deadline)) => ControlFlow::WaitUntil(deadline),
            (ControlFlow::WaitUntil(old), Some(deadline)) => {
                ControlFlow::WaitUntil(old.min(deadline))
            }
            _ => base,
        };
        if next != base {
            self.installed = Some((base, next));
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_deadlines_restore_idle_and_preserve_embedding_policy() {
        let now = Instant::now();
        let mut wait = AnimationWait::default();
        let first = wait.update(ControlFlow::Wait, Some(now + FRAME_INTERVAL));
        assert_eq!(first, ControlFlow::WaitUntil(now + FRAME_INTERVAL));
        let second = wait.update(first, Some(now + FRAME_INTERVAL * 2));
        assert_eq!(second, ControlFlow::WaitUntil(now + FRAME_INTERVAL * 2));
        assert_eq!(wait.update(second, None), ControlFlow::Wait);
        let early = ControlFlow::WaitUntil(now);
        assert_eq!(wait.update(early, Some(now + FRAME_INTERVAL)), early);
        let late = ControlFlow::WaitUntil(now + FRAME_INTERVAL * 3);
        let animation = wait.update(late, Some(now + FRAME_INTERVAL));
        assert_eq!(wait.update(animation, None), late);
        wait.update(ControlFlow::Wait, Some(now + FRAME_INTERVAL));
        assert_eq!(
            wait.update(ControlFlow::Poll, Some(now + FRAME_INTERVAL)),
            ControlFlow::Poll
        );
        assert_eq!(wait.update(ControlFlow::Poll, None), ControlFlow::Poll);
    }
}
