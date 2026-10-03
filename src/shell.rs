//! Rendering-opportunity callbacks around Blitz's existing window application.
use crate::reload::ReloadDocument;
use crate::runtime::LapuiDocument;
use anyrender::WindowRenderer;
use blitz::dom::Document;
use blitz::shell::{BlitzApplication, BlitzShellEvent, BlitzShellProxy, View, WindowConfig};
use serde_json::json;
use std::any::Any;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::WindowId;

/// Keeps Blitz's layout, painting and OS handling, adding a JS frame checkpoint
/// immediately before visible, active-window redraws. Embedders should use this
/// application instead of bare BlitzApplication to drive animation callbacks.
pub struct LapuiApplication<R: WindowRenderer> {
    pub inner: BlitzApplication<R>,
    proxy: BlitzShellProxy,
    frames: HashMap<WindowId, Instant>,
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
        Self {
            proxy: proxy.clone(),
            inner: BlitzApplication::new(proxy, events),
            frames: HashMap::new(),
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
    }
    fn destroy_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.frames.clear();
        self.inner.destroy_surfaces(event_loop);
    }
    fn resumed(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.resumed(event_loop);
    }
    fn suspended(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.suspended(event_loop);
    }
    fn proxy_wake_up(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.proxy_wake_up(event_loop);
    }
    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        self.inner.about_to_wait(event_loop);
        self.frames
            .retain(|id, _| self.inner.windows.contains_key(id));
        let now = Instant::now();
        let mut earliest = None;
        for (id, view) in &mut self.inner.windows {
            if !view.is_visible
                || !view.renderer.is_active()
                || !frame_document(view.doc.as_mut()).is_some_and(|document| {
                    document.has_animation_callbacks() || document.has_pending_rendering_update()
                })
            {
                continue;
            }
            let due = self
                .frames
                .get(id)
                .map_or(now, |last| *last + FRAME_INTERVAL);
            let wake = if due <= now {
                view.request_redraw();
                // A slow/inactive platform must not turn an overdue frame into
                // a WaitUntil deadline in the past and spin the event loop.
                now + FRAME_INTERVAL
            } else {
                due
            };
            earliest = Some(earliest.map_or(wake, |old: Instant| old.min(wake)));
        }
        let flow = self.wait.update(event_loop.control_flow(), earliest);
        event_loop.set_control_flow(flow);
    }

    fn window_event(&mut self, event_loop: &dyn ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::RedrawRequested) {
            let now = Instant::now();
            let due = self
                .frames
                .get(&id)
                .is_none_or(|last| now >= *last + FRAME_INTERVAL);
            if let Some(view) = self.inner.windows.get_mut(&id) {
                if view.is_visible
                    && view.renderer.is_active()
                    && frame_document(view.doc.as_mut()).is_some()
                {
                    if due {
                        self.frames.insert(id, now);
                    }
                    redraw_document(view, due);
                    self.proxy
                        .send_event(BlitzShellEvent::Poll { window_id: id });
                    return;
                }
            }
        }
        self.inner.window_event(event_loop, id, event);
    }

    #[cfg(target_os = "macos")]
    fn macos_handler(
        &mut self,
    ) -> Option<&mut dyn winit::platform::macos::ApplicationHandlerExtMacOS> {
        self.inner.macos_handler()
    }
}

// Native engine resolve/paint through public APIs, retaining Blitz's resource
// and visibility gates. Renderer return is not a display-server presentation ack.
fn redraw_document<R: WindowRenderer>(view: &mut View<R>, callbacks_due: bool) {
    let layout_time = view.current_animation_time();
    let document = frame_document(view.doc.as_mut()).unwrap();
    let frame = document.frame_trace("window");
    let measured = frame.sequence().is_some();
    let callbacks_start = measured.then(Instant::now);
    if callbacks_due {
        document.animation_frame_at(layout_time);
        document.rendering_update_at(layout_time);
    } else {
        document.defer_rendering_update();
    }
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
        "callbackPhaseMillis":callbacks_millis,"callbacksDue":callbacks_due,"sceneBuildMillis":scene_millis,
        "rendererCallMillis":if blocked{None}else{renderer_millis},"width":width,"height":height,
        "physicalPresentation":"unknown"}),false);
    if !blocked && animating {
        view.request_redraw();
    }
}

// A soft 60 Hz maximum for JS callback batches; not monitor VSync or a
// presentation guarantee. No missed-frame replay or animation worker.
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

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
