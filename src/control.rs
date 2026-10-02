//! Bounded requests from background clients to the document's owning UI thread.

use crate::action::ActionError;
use serde_json::Value;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
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
    page_changes: PageChangeNotifier,
}

#[derive(Clone, Default)]
pub(crate) struct PageChangeNotifier {
    inner: Arc<(Mutex<PageChangeState>, Condvar)>,
}

#[derive(Default)]
struct PageChangeState {
    generation: u64,
    closed: bool,
}

impl PageChangeNotifier {
    pub(crate) fn generation(&self) -> u64 {
        self.inner.0.lock().unwrap().generation
    }

    pub(crate) fn notify(&self) {
        let (state, condition) = &*self.inner;
        let mut state = state.lock().unwrap();
        state.generation = state.generation.saturating_add(1);
        condition.notify_all();
    }

    pub(crate) fn close(&self) {
        let (state, condition) = &*self.inner;
        let mut state = state.lock().unwrap();
        state.closed = true;
        condition.notify_all();
    }

    pub(crate) fn wait_after(&self, generation: u64, timeout: Duration) -> bool {
        let (state, condition) = &*self.inner;
        let state = state.lock().unwrap();
        let (state, _) = condition
            .wait_timeout_while(state, timeout, |state| {
                !state.closed && state.generation == generation
            })
            .unwrap();
        state.closed || state.generation != generation
    }
}

pub(crate) fn channel(
    wake: impl Fn() + Send + Sync + 'static,
) -> (DocumentController, mpsc::Receiver<DocumentRequest>) {
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
    let page_changes = PageChangeNotifier::default();
    (
        DocumentController {
            sender,
            wake: Arc::new(wake),
            page_changes,
        },
        receiver,
    )
}

impl DocumentController {
    pub(crate) fn page_change_notifier(&self) -> PageChangeNotifier {
        self.page_changes.clone()
    }

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
    fn unknown_dispatched_action_can_be_observed_and_retried_without_duplicate_write() {
        use crate::action::{ActionInfo, ActionKind, ActionRegistry};

        let registry = ActionRegistry::new(json!({"count":0})).unwrap();
        let (handler_entered, entered_rx) = mpsc::channel::<Instant>();
        let (release_handler, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        registry
            .register(
                ActionInfo {
                    id: "counter.increment".into(),
                    description: "Increase the counter by one".into(),
                    input_schema: json!({"type":"object","additionalProperties":false}),
                    output_schema: json!({"type":"integer","minimum":0}),
                    kind: ActionKind::Write,
                },
                move |state, _| {
                    handler_entered.send(Instant::now()).unwrap();
                    if release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .is_err()
                    {
                        return Err(crate::action::ActionError::new(
                            "test_release_timeout",
                            "test did not release the dispatched action handler",
                        ));
                    }
                    let count = state["count"].as_u64().unwrap() + 1;
                    state["count"] = json!(count);
                    Ok(json!(count))
                },
            )
            .unwrap();

        let (controller, requests) = channel(|| {});
        let command = json!({
            "action":"counter.increment",
            "arguments":{},
            "requestId":"unknown-retry-1",
            "expectedVersion":0
        });
        let (caller_returned, caller_result) = mpsc::channel();
        let caller = std::thread::spawn(move || {
            let result = controller.request(command, Duration::from_secs(1));
            let _ = caller_returned.send((Instant::now(), result));
        });
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.start());

        let dispatch_registry = registry.clone();
        let dispatcher = std::thread::spawn(move || {
            let result = dispatch_registry
                .invoke_checked(
                    request.command["action"].as_str().unwrap(),
                    &request.command["arguments"],
                    request.command["requestId"].as_str(),
                    request.command["expectedVersion"].as_u64(),
                )
                .map(
                    |observation| json!({"version":observation.version,"state":observation.state}),
                );
            request.finish(result);
        });

        let handler_entered_at = entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (caller_returned_at, result) =
            caller_result.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            handler_entered_at < caller_returned_at,
            "the action handler must enter before the client times out"
        );
        let unknown = result.unwrap_err();
        assert_eq!(unknown.code, "outcome_unknown");
        caller.join().unwrap();
        assert_eq!(registry.observe().state["count"], 0);
        assert_eq!(registry.observe().version, 0);

        release_handler.send(()).unwrap();
        dispatcher.join().unwrap();

        let final_state = registry.observe();
        assert_eq!(final_state.state["count"], 1);
        assert_eq!(final_state.version, 1);
        let retry = registry
            .invoke_checked(
                "counter.increment",
                &json!({}),
                Some("unknown-retry-1"),
                Some(0),
            )
            .unwrap();
        assert_eq!(retry.state["count"], 1);
        assert_eq!(retry.version, 1);
        assert_eq!(registry.observe().state["count"], 1);

        let trace = registry.trace(0);
        assert_eq!(
            trace
                .records
                .iter()
                .filter(|record| {
                    record.request_id.as_deref() == Some("unknown-retry-1")
                        && record.outcome == "completed"
                })
                .count(),
            1
        );
        assert_eq!(
            trace
                .records
                .iter()
                .filter(|record| {
                    record.request_id.as_deref() == Some("unknown-retry-1")
                        && record.outcome == "replayed"
                })
                .count(),
            1
        );
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
