//! A document-owned pool for blocking host calls and operation revision waits.

use crate::action::ActionError;
use crate::lifecycle::Cancellation;
use std::sync::{mpsc, Arc, Mutex};

type Work = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone)]
pub(crate) struct HostWork {
    sender: mpsc::SyncSender<Work>,
    lifetime: Cancellation,
}

impl HostWork {
    pub fn new(lifetime: Cancellation) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Work>(64);
        let receiver = Arc::new(Mutex::new(receiver));
        for _ in 0..2 {
            let receiver = receiver.clone();
            let lifetime = lifetime.clone();
            std::thread::Builder::new()
                .name("lapui-host".into())
                .spawn(move || loop {
                    let work = receiver.lock().unwrap().recv();
                    match work {
                        Ok(work) => {
                            if !lifetime.is_cancelled() {
                                work();
                            }
                        }
                        Err(_) => break,
                    }
                })?;
        }
        Ok(Self { sender, lifetime })
    }

    pub fn submit(&self, work: impl FnOnce() + Send + 'static) -> Result<(), ActionError> {
        if self.lifetime.is_cancelled() {
            return Err(ActionError::new("document_closed", "document is closed"));
        }
        self.sender
            .try_send(Box::new(work))
            .map_err(|failure| match failure {
                mpsc::TrySendError::Full(_) => ActionError::new(
                    "host_busy",
                    "host work queue is full; at most 64 calls may wait",
                ),
                mpsc::TrySendError::Disconnected(_) => {
                    ActionError::new("document_closed", "host workers have stopped")
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_jobs_have_a_bounded_queue_and_do_not_spawn_unlimited_workers() {
        let lifetime = Cancellation::default();
        let pool = HostWork::new(lifetime.clone()).unwrap();
        let (started, entered) = mpsc::channel();
        let releases: Vec<_> = (0..2)
            .map(|_| {
                let (release, blocking) = mpsc::channel::<()>();
                let started = started.clone();
                pool.submit(move || {
                    started.send(()).unwrap();
                    let _ = blocking.recv();
                })
                .unwrap();
                release
            })
            .collect();
        for _ in 0..2 {
            entered
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
        }
        for _ in 0..64 {
            pool.submit(|| {}).unwrap();
        }
        assert_eq!(pool.submit(|| {}).unwrap_err().code, "host_busy");
        lifetime.cancel();
        assert_eq!(pool.submit(|| {}).unwrap_err().code, "document_closed");
        drop(pool);
        drop(releases);
    }
}
