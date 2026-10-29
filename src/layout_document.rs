//! Conservative invalidation for synchronous CSSOM reads.
//!
//! A mutable document borrow invalidates the last resolve. All DOM, native
//! input, viewport and host mutations therefore invalidate through one gate.
//! A poll checkpoint also invalidates to ingest newly arrived resource messages.
use blitz::dom::BaseDocument;
use std::cell::{Cell, Ref, RefCell, RefMut};

pub(crate) struct LayoutDocument {
    document: RefCell<BaseDocument>,
    resolved_time: Cell<Option<f64>>,
    generation: Cell<u64>,
}

impl LayoutDocument {
    pub fn new(document: BaseDocument) -> Self {
        Self {
            document: RefCell::new(document),
            resolved_time: Cell::new(None),
            generation: Cell::new(0),
        }
    }

    pub fn borrow(&self) -> Ref<'_, BaseDocument> {
        self.document.borrow()
    }

    pub fn borrow_mut(&self) -> RefMut<'_, BaseDocument> {
        self.invalidate();
        self.document.borrow_mut()
    }

    pub fn invalidate(&self) {
        self.resolved_time.set(None);
    }

    pub fn is_resolved(&self, time: f64) -> bool {
        self.resolved_time.get() == Some(time)
    }

    pub fn resolve(&self, time: f64) {
        let mut document = self.document.borrow_mut();
        document.resolve(time);
        self.generation.set(self.generation.get().wrapping_add(1));
        // Keep retrying while render-blocking resources are still arriving.
        if !document.has_pending_critical_resources()
            && !document.root_node().has_dirty_descendants()
        {
            self.resolved_time.set(Some(time));
        } else {
            self.resolved_time.set(None);
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.get()
    }
}
