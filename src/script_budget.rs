//! Cooperative QuickJS limits. An interrupt invalidates bridge execution state:
//! QuickJS skips JS catch/finally handlers when interrupted, so only reload may resume it.
use rquickjs::Runtime;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub(crate) const HEAP_LIMIT: usize = 128 * 1024 * 1024;
pub(crate) const CALLBACK_LIMIT: Duration = Duration::from_millis(250);
pub(crate) const STARTUP_LIMIT: Duration = Duration::from_secs(2);
pub(crate) const CHECKPOINT_SLICE: Duration = Duration::from_millis(20);
pub(crate) const INTERRUPTED_MESSAGE: &str =
    "JavaScript execution limit exceeded; scripting is suspended until document reload";

#[derive(Clone)]
pub(crate) struct ScriptBudget {
    deadline: Rc<Cell<Option<Instant>>>,
    interrupted: Rc<Cell<bool>>,
}

impl ScriptBudget {
    pub fn install(runtime: &Runtime) -> Self {
        let budget = Self {
            deadline: Rc::new(Cell::new(None)),
            interrupted: Rc::new(Cell::new(false)),
        };
        let handler = budget.clone();
        runtime.set_memory_limit(HEAP_LIMIT);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            if handler.interrupted.get()
                || handler
                    .deadline
                    .get()
                    .is_some_and(|deadline| Instant::now() >= deadline)
            {
                handler.interrupted.set(true);
                return true;
            }
            false
        })));
        budget
    }

    pub fn interrupted(&self) -> bool {
        self.interrupted.get()
    }

    pub fn run<T>(&self, limit: Duration, callback: impl FnOnce() -> T) -> T {
        // Nested Rust-to-JS entries cannot extend the outer call's deadline.
        let previous = self.deadline.get();
        let deadline = Instant::now() + limit;
        self.deadline
            .set(Some(previous.map_or(deadline, |old| old.min(deadline))));
        struct Restore<'a>(&'a Cell<Option<Instant>>, Option<Instant>);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.0.set(self.1);
            }
        }
        let _restore = Restore(&self.deadline, previous);
        callback()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rquickjs::Context;

    #[test]
    fn oversized_array_buffer_is_rejected_without_reserving_the_entire_heap_limit() {
        let runtime = Runtime::new().unwrap();
        let budget = ScriptBudget::install(&runtime);
        let context = Context::full(&runtime).unwrap();
        context
            .with(|ctx| ctx.globals().set("limit", HEAP_LIMIT + 1))
            .unwrap();
        let rejected = budget
            .run(CALLBACK_LIMIT, || {
                context.with(|ctx| {
                    ctx.eval::<bool, _>(
            "(() => { try { new ArrayBuffer(limit); return false; } catch { return true; } })()"
        )
                })
            })
            .unwrap();
        assert!(rejected);
        assert!(!budget.interrupted());
        assert_eq!(context.with(|ctx| ctx.eval::<i32, _>("2+3")).unwrap(), 5);
    }
}
