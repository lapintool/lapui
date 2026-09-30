use std::collections::{BTreeSet, HashMap};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const MAX_TIMERS: usize = 1024;

#[derive(Default)]
struct State {
    deadlines: BTreeSet<(Instant, i32)>,
    by_id: HashMap<i32, Instant>,
    stopped: bool,
}

#[derive(Clone)]
pub(crate) struct TimerHandle(Arc<(Mutex<State>, Condvar)>);

impl TimerHandle {
    pub fn arm(&self, id: i32, delay: i32) -> bool {
        if id <= 0 || delay < 0 {
            return false;
        }
        let (lock, wake) = &*self.0;
        let mut state = lock.lock().unwrap();
        if state.stopped || (state.by_id.len() >= MAX_TIMERS && !state.by_id.contains_key(&id)) {
            return false;
        }
        if let Some(old) = state.by_id.remove(&id) {
            state.deadlines.remove(&(old, id));
        }
        let deadline = Instant::now() + Duration::from_millis(delay.max(1) as u64);
        state.by_id.insert(id, deadline);
        state.deadlines.insert((deadline, id));
        wake.notify_one();
        true
    }

    pub fn clear(&self, id: i32) {
        let (lock, wake) = &*self.0;
        let mut state = lock.lock().unwrap();
        if let Some(old) = state.by_id.remove(&id) {
            state.deadlines.remove(&(old, id));
        }
        wake.notify_one();
    }

    pub fn stop(&self) {
        let (lock, wake) = &*self.0;
        let mut state = lock.lock().unwrap();
        state.stopped = true;
        state.by_id.clear();
        state.deadlines.clear();
        wake.notify_one();
    }
}

pub(crate) struct Timers {
    pub handle: TimerHandle,
    pub expired: mpsc::Receiver<i32>,
}

impl Timers {
    pub fn new(wake_document: impl Fn() + Send + 'static) -> Self {
        let handle = TimerHandle(Arc::new((Mutex::new(State::default()), Condvar::new())));
        let worker = handle.clone();
        let (expired, receiver) = mpsc::sync_channel(MAX_TIMERS);
        std::thread::spawn(move || {
            let (lock, wake) = &*worker.0;
            loop {
                let id = {
                    let mut state = lock.lock().unwrap();
                    loop {
                        if state.stopped {
                            return;
                        }
                        let Some(&(deadline, id)) = state.deadlines.first() else {
                            state = wake.wait(state).unwrap();
                            continue;
                        };
                        let now = Instant::now();
                        if deadline <= now {
                            state.deadlines.remove(&(deadline, id));
                            state.by_id.remove(&id);
                            break id;
                        }
                        state = wake.wait_timeout(state, deadline - now).unwrap().0;
                    }
                };
                if expired.send(id).is_err() {
                    return;
                }
                wake_document();
            }
        });
        Self {
            handle,
            expired: receiver,
        }
    }
}

impl Drop for Timers {
    fn drop(&mut self) {
        self.handle.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_rescheduling_cancellation_and_document_shutdown_release_deadlines() {
        let timers = Timers::new(|| {});
        assert!(timers.handle.arm(1, 30_000));
        assert!(timers.handle.arm(2, 30_000));
        timers.handle.clear(2);
        assert!(timers.handle.arm(1, 1));
        assert_eq!(
            timers.expired.recv_timeout(Duration::from_secs(1)).unwrap(),
            1
        );
        assert!(timers.expired.try_recv().is_err());
        let handle = timers.handle.clone();
        drop(timers);
        assert!(!handle.arm(3, 1));
        assert!(handle.0 .0.lock().unwrap().deadlines.is_empty());
    }
}
