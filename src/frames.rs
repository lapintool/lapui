//! Document-scoped clock and callback identities; no frame timer or worker.
use std::collections::BTreeSet;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) const MAX_CALLBACKS: usize = 1024;

pub(crate) struct Frames {
    pub debug_trace: crate::debug_trace::DebugTrace,
    epoch: Instant,
    pub time_origin: f64,
    pub layout_time: f64,
    pub in_frame: bool,
    pub rendering_pending: bool,
    render_revision: u64,
    completed_render_revision: u64,
    render_outcome: &'static str,
    pending: BTreeSet<i32>,
    stopped: bool,
}

impl Frames {
    pub fn new(debug_trace: crate::debug_trace::DebugTrace) -> Self {
        Self {
            debug_trace,
            epoch: Instant::now(),
            time_origin: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                * 1000.0,
            pending: BTreeSet::new(),
            layout_time: 0.0,
            in_frame: false,
            rendering_pending: false,
            render_revision: 0,
            completed_render_revision: 0,
            render_outcome: "pending",
            stopped: false,
        }
    }
    pub fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64() * 1000.0
    }
    pub fn request(&mut self, id: i32) -> bool {
        if self.stopped
            || id <= 0
            || (self.pending.len() >= MAX_CALLBACKS && !self.pending.contains(&id))
        {
            return false;
        }
        self.pending.insert(id);
        true
    }
    pub fn cancel(&mut self, id: i32) {
        self.pending.remove(&id);
    }
    pub fn snapshot(&self) -> (f64, Vec<i32>) {
        (self.now(), self.pending.iter().copied().collect())
    }
    pub fn take(&mut self, id: i32) -> bool {
        self.pending.remove(&id)
    }
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn stop(&mut self) {
        self.stopped = true;
        self.pending.clear();
        self.rendering_pending = false;
    }

    /// A bounded, trace-independent marker for callers that need to wait for
    /// the next native renderer opportunity after a semantic control action.
    pub fn request_render_revision(&mut self) -> u64 {
        self.render_revision = self.render_revision.saturating_add(1);
        self.render_revision
    }

    pub fn complete_render_revision(&mut self, outcome: &'static str) {
        if self.completed_render_revision < self.render_revision {
            self.completed_render_revision = self.render_revision;
            self.render_outcome = outcome;
        }
    }

    pub fn render_status(&self) -> (u64, u64, &'static str) {
        (
            self.render_revision,
            self.completed_render_revision,
            self.render_outcome,
        )
    }
}
