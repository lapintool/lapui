//! Cooperative QuickJS limits. An interrupt invalidates bridge execution state:
//! QuickJS skips JS catch/finally handlers when interrupted, so only reload may resume it.
use rquickjs::Runtime;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub(crate) const HEAP_LIMIT: usize = 128 * 1024 * 1024;
pub(crate) const STACK_LIMIT: usize = 4 * 1024 * 1024;
#[cfg(target_os = "windows")]
const NATIVE_STACK_HEADROOM: usize = 512 * 1024;

fn stack_limit() -> usize {
    #[cfg(target_os = "windows")]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentThreadStackLimits(low: *mut usize, high: *mut usize);
        }
        let (mut low, mut high) = (0, 0);
        let marker = 0_u8;
        // SAFETY: the OS writes two pointer-sized bounds for this thread.
        // Comparing an address to them does not access stack memory.
        unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) };
        let current = std::ptr::addr_of!(marker) as usize;
        if current > low && current < high {
            return (current - low)
                .saturating_sub(NATIVE_STACK_HEADROOM)
                .clamp(64 * 1024, STACK_LIMIT);
        }
    }
    // Keep the original QuickJS allowance on platforms where the host's
    // native stack reservation is not queried.
    1024 * 1024
}
pub(crate) const CALLBACK_LIMIT: Duration = Duration::from_millis(250);
pub(crate) const LARGE_CALLBACK_LIMIT: Duration = Duration::from_secs(5);
pub(crate) const STARTUP_LIMIT: Duration = Duration::from_secs(2);
pub(crate) const LARGE_STARTUP_LIMIT: Duration = Duration::from_secs(10);
pub(crate) const LARGE_STARTUP_SOURCE_BYTES: usize = 256 * 1024;
pub(crate) const CHECKPOINT_SLICE: Duration = Duration::from_millis(20);
pub(crate) const INTERRUPTED_MESSAGE: &str =
    "JavaScript execution limit exceeded; scripting is suspended until document reload";

#[derive(Clone)]
pub(crate) struct ScriptBudget {
    deadline: Rc<Cell<Option<Instant>>>,
    interrupted: Rc<Cell<bool>>,
    large_application: bool,
}

impl ScriptBudget {
    pub fn install(runtime: &Runtime) -> Self {
        let budget = Self {
            deadline: Rc::new(Cell::new(None)),
            interrupted: Rc::new(Cell::new(false)),
            large_application: false,
        };
        let handler = budget.clone();
        runtime.set_memory_limit(HEAP_LIMIT);
        runtime.set_max_stack_size(stack_limit());
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

    pub fn with_large_application(mut self, large: bool) -> Self {
        self.large_application = large;
        self
    }

    pub fn run_callback<T>(&self, callback: impl FnOnce() -> T) -> T {
        self.run(
            if self.large_application {
                LARGE_CALLBACK_LIMIT
            } else {
                CALLBACK_LIMIT
            },
            callback,
        )
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
    fn recursion_is_caught_on_small_and_large_host_thread_stacks() {
        for size in [2 * 1024 * 1024, 8 * 1024 * 1024] {
            std::thread::Builder::new().stack_size(size).spawn(|| {
                let runtime = Runtime::new().unwrap();
                let budget = ScriptBudget::install(&runtime);
                let context = Context::full(&runtime).unwrap();
                let caught = budget.run(CALLBACK_LIMIT, || context.with(|ctx| ctx.eval::<bool, _>(
                    "(() => { function recurse(n) { return recurse(n + 1) + n; } try { recurse(0); } catch (error) { return /stack/i.test(error.message); } return false; })()"
                ))).unwrap();
                assert!(caught);
                assert!(!budget.interrupted());
                assert_eq!(context.with(|ctx| ctx.eval::<i32, _>("2+3")).unwrap(), 5);
            }).unwrap().join().unwrap();
        }
    }

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
