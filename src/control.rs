//! Bounded requests from background clients to the document's owning UI thread.

use crate::action::ActionError;
use serde_json::Value;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const QUEUED: u8 = 0;
const STARTED: u8 = 1;
const CANCELLED: u8 = 2;
const QUEUE_CAPACITY: usize = 64;
const MAX_REQUEST_BYTES: usize = 64 * 1024;

fn error(code: &str, message: &str) -> ActionError {
    ActionError {
        code: code.into(),
        message: message.into(),
    }
}

pub(crate) struct DocumentRequest {
    pub command: Value,
    reply: mpsc::Sender<Result<Value, ActionError>>,
    state: Arc<AtomicU8>,
    deadline: Instant,
}

impl DocumentRequest {
    pub fn start(&self) -> bool {
        if Instant::now() >= self.deadline {
            let _ =
                self.state
                    .compare_exchange(QUEUED, CANCELLED, Ordering::AcqRel, Ordering::Acquire);
            let _ = self.reply.send(Err(error(
                "control_timeout",
                "request expired before document dispatch",
            )));
            return false;
        }
        self.state
            .compare_exchange(QUEUED, STARTED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn finish(self, result: Result<Value, ActionError>) {
        let _ = self.reply.send(result);
    }
}

/// A cloneable handle for clients running outside the UI thread.
///
/// `request` blocks its caller; never call it on the owning document's UI thread.
/// Commands execute only when that document is polled. A timeout cancels queued
/// commands; an already dispatched command has an explicitly unknown outcome.
#[derive(Clone)]
pub struct DocumentController {
    sender: mpsc::SyncSender<DocumentRequest>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

pub(crate) fn channel(
    wake: impl Fn() + Send + Sync + 'static,
) -> (DocumentController, mpsc::Receiver<DocumentRequest>) {
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
    (
        DocumentController {
            sender,
            wake: Arc::new(wake),
        },
        receiver,
    )
}

impl DocumentController {
    pub fn request(&self, command: Value, timeout: Duration) -> Result<Value, ActionError> {
        if command.to_string().len() > MAX_REQUEST_BYTES {
            return Err(error("invalid_request", "document request exceeds 64 KiB"));
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| error("invalid_request", "invalid request timeout"))?;
        let (reply, response) = mpsc::channel();
        let state = Arc::new(AtomicU8::new(QUEUED));
        let request = DocumentRequest {
            command,
            reply,
            state: state.clone(),
            deadline,
        };
        self.sender
            .try_send(request)
            .map_err(|failure| match failure {
                mpsc::TrySendError::Full(_) => {
                    error("control_busy", "document request queue is full")
                }
                mpsc::TrySendError::Disconnected(_) => {
                    error("document_closed", "document is closed")
                }
            })?;
        (self.wake)();
        match response.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(error("document_closed", "document closed before replying"))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if state
                    .compare_exchange(QUEUED, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    Err(error(
                        "control_timeout",
                        "request cancelled before document dispatch",
                    ))
                } else {
                    Err(error(
                        "outcome_unknown",
                        "request may have executed; observe state before retrying",
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn queued_timeout_cancels_but_dispatched_timeout_has_unknown_outcome() {
        let (controller, requests) = channel(|| {});
        let error = controller
            .request(json!({"method":"activate"}), Duration::ZERO)
            .unwrap_err();
        assert_eq!(error.code, "control_timeout");
        assert!(!requests.recv().unwrap().start());

        let caller = std::thread::spawn(move || {
            controller.request(json!({"method":"activate"}), Duration::from_millis(100))
        });
        let request = requests.recv().unwrap();
        assert!(request.start());
        assert_eq!(caller.join().unwrap().unwrap_err().code, "outcome_unknown");
    }

    #[test]
    fn closed_documents_and_oversized_requests_fail_before_dispatch() {
        let (controller, requests) = channel(|| {});
        assert_eq!(
            controller
                .request(
                    json!({"value":"x".repeat(MAX_REQUEST_BYTES)}),
                    Duration::from_secs(1)
                )
                .unwrap_err()
                .code,
            "invalid_request"
        );
        assert!(requests.try_recv().is_err());
        drop(requests);
        assert_eq!(
            controller
                .request(json!({"method":"controls"}), Duration::from_secs(1))
                .unwrap_err()
                .code,
            "document_closed"
        );
    }

    #[test]
    fn pending_request_queue_is_bounded() {
        let (controller, _requests) = channel(|| {});
        for _ in 0..QUEUE_CAPACITY {
            let (reply, _response) = mpsc::channel();
            controller
                .sender
                .try_send(DocumentRequest {
                    command: json!({"method":"controls"}),
                    reply,
                    state: Arc::new(AtomicU8::new(QUEUED)),
                    deadline: Instant::now() + Duration::from_secs(10),
                })
                .unwrap();
        }
        assert_eq!(
            controller
                .request(json!({"method":"controls"}), Duration::from_secs(1))
                .unwrap_err()
                .code,
            "control_busy"
        );
    }
}
