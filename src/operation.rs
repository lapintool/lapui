//! Bounded, application-scoped background jobs with cooperative cancellation.

use crate::action::ActionError;
use crate::changes::{ChangeEvent, ChangeLog};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const MAX_RECORDS: usize = 128;
const MAX_RUNNING: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Execution {
    Accepted,
    Running,
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
}

impl Execution {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationSnapshot {
    pub operation_id: String,
    pub action: String,
    pub execution: Execution,
    pub revision: u64,
    pub progress: f64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ActionError>,
}

/// Public change-feed projection; outputs, error messages and progress text are
/// retrieved explicitly with `get` instead of copied into every event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationSummary {
    pub operation_id: String,
    pub action: String,
    pub execution: Execution,
    pub revision: u64,
    pub progress: f64,
}

impl OperationSnapshot {
    fn summary(&self) -> OperationSummary {
        OperationSummary {
            operation_id: self.operation_id.clone(),
            action: self.action.clone(),
            execution: self.execution,
            revision: self.revision,
            progress: self.progress,
        }
    }
}

struct Shared {
    snapshot: Mutex<OperationSnapshot>,
    changed: Condvar,
    changes: ChangeLog,
}

impl Shared {
    fn publish(&self, snapshot: &OperationSnapshot) {
        let _ = self
            .changes
            .publish(ChangeEvent::OperationChanged(snapshot.summary()));
        self.changed.notify_all();
    }
}

/// Passed to a Rust job. Jobs must check cancellation between units of work.
/// This context does not provide DOM access or transactional application writes.
#[derive(Clone)]
pub struct OperationContext {
    shared: Arc<Shared>,
}

impl OperationContext {
    pub fn cancelled(&self) -> bool {
        matches!(
            self.shared.snapshot.lock().unwrap().execution,
            Execution::CancelRequested | Execution::Cancelled
        )
    }

    pub fn checkpoint(&self) -> Result<(), ActionError> {
        if self.cancelled() {
            Err(ActionError::new(
                "operation_cancelled",
                "operation cancellation requested",
            ))
        } else {
            Ok(())
        }
    }

    pub fn report(&self, progress: f64, message: &str) -> Result<(), ActionError> {
        if !progress.is_finite() || !(0.0..=1.0).contains(&progress) || message.len() > 1024 {
            return Err(ActionError::new(
                "invalid_progress",
                "progress must be in 0..1 and message at most 1 KiB",
            ));
        }
        let mut state = self.shared.snapshot.lock().unwrap();
        if state.execution == Execution::CancelRequested {
            return Err(ActionError::new(
                "operation_cancelled",
                "operation cancellation requested",
            ));
        }
        if state.execution != Execution::Running {
            return Err(ActionError::new(
                "operation_finished",
                "operation is not running",
            ));
        }
        state.progress = progress;
        state.message = message.into();
        state.revision += 1;
        self.shared.publish(&state);
        Ok(())
    }

    /// An interruptible delay useful for retry/backoff or simulated workloads.
    pub fn delay(&self, duration: Duration) -> Result<(), ActionError> {
        let state = self.shared.snapshot.lock().unwrap();
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, duration, |state| {
                state.execution == Execution::Running
            })
            .unwrap();
        if state.execution == Execution::CancelRequested {
            Err(ActionError::new(
                "operation_cancelled",
                "operation cancellation requested",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Default)]
pub struct Operations {
    inner: Arc<Mutex<State>>,
    changes: ChangeLog,
}

#[derive(Default)]
struct State {
    sequence: u64,
    records: BTreeMap<String, Arc<Shared>>,
    order: VecDeque<String>,
}

impl Operations {
    pub(crate) fn with_changes(changes: ChangeLog) -> Self {
        Self {
            inner: Default::default(),
            changes,
        }
    }

    /// Hold the registry and all snapshot locks until the feed checkpoint is
    /// captured. Lock order is application -> operation registry -> snapshots
    /// -> change journal, never the reverse.
    pub(crate) fn with_summaries<T>(&self, callback: impl FnOnce(Vec<OperationSummary>) -> T) -> T {
        let state = self.inner.lock().unwrap();
        let snapshots: Vec<_> = state
            .records
            .values()
            .map(|shared| shared.snapshot.lock().unwrap())
            .collect();
        callback(
            snapshots
                .iter()
                .map(|snapshot| snapshot.summary())
                .collect(),
        )
    }
    pub(crate) fn start(
        &self,
        action: &str,
        job: impl FnOnce(OperationContext) -> Result<Value, ActionError> + Send + 'static,
    ) -> Result<OperationSnapshot, ActionError> {
        let mut state = self.inner.lock().unwrap();
        let running = state
            .records
            .values()
            .filter(|record| !record.snapshot.lock().unwrap().execution.terminal())
            .count();
        if running >= MAX_RUNNING {
            return Err(ActionError::new(
                "operation_busy",
                "at most 8 operations may run concurrently",
            ));
        }
        while state.records.len() >= MAX_RECORDS {
            let index = state
                .order
                .iter()
                .position(|id| {
                    state.records[id]
                        .snapshot
                        .lock()
                        .unwrap()
                        .execution
                        .terminal()
                })
                .ok_or_else(|| ActionError::new("operation_busy", "operation buffer is full"))?;
            let id = state.order.remove(index).unwrap();
            state.records.remove(&id);
            let _ = self
                .changes
                .publish(ChangeEvent::OperationRemoved { operation_id: id });
        }
        state.sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| ActionError::new("operation_limit", "operation identifier overflow"))?;
        let id = format!("operation:{}", state.sequence);
        let snapshot = OperationSnapshot {
            operation_id: id.clone(),
            action: action.into(),
            execution: Execution::Accepted,
            revision: 0,
            progress: 0.0,
            message: String::new(),
            output: None,
            error: None,
        };
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(snapshot.clone()),
            changed: Condvar::new(),
            changes: self.changes.clone(),
        });
        state.records.insert(id.clone(), shared.clone());
        state.order.push_back(id.clone());
        shared.publish(&snapshot);
        let worker = std::thread::Builder::new()
            .name("lapui-operation".into())
            .spawn(move || {
                {
                    let mut state = shared.snapshot.lock().unwrap();
                    if state.execution != Execution::CancelRequested {
                        state.execution = Execution::Running;
                    }
                    state.revision += 1;
                    shared.publish(&state);
                }
                let context = OperationContext {
                    shared: shared.clone(),
                };
                let result = if context.cancelled() {
                    Err(ActionError::new(
                        "operation_cancelled",
                        "operation cancelled before execution",
                    ))
                } else {
                    catch_unwind(AssertUnwindSafe(|| job(context))).unwrap_or_else(|_| {
                        Err(ActionError::new(
                            "operation_panicked",
                            "Rust operation panicked",
                        ))
                    })
                };
                let mut state = shared.snapshot.lock().unwrap();
                if state.execution == Execution::CancelRequested {
                    state.execution = Execution::Cancelled;
                    state.output = None;
                    state.error = None;
                } else {
                    match result {
                        Ok(output) => {
                            state.execution = Execution::Completed;
                            state.progress = 1.0;
                            state.output = Some(output);
                        }
                        Err(mut error) => {
                            bound(&mut error.code, 128);
                            bound(&mut error.message, 16 * 1024);
                            state.execution = Execution::Failed;
                            state.error = Some(error);
                        }
                    }
                }
                state.revision += 1;
                shared.publish(&state);
            });
        if let Err(error) = worker {
            state.records.remove(&id);
            state.order.retain(|entry| entry != &id);
            let _ = self
                .changes
                .publish(ChangeEvent::OperationRemoved { operation_id: id });
            return Err(ActionError::new(
                "operation_start_failed",
                error.to_string(),
            ));
        }
        Ok(snapshot)
    }

    fn shared(&self, id: &str) -> Result<Arc<Shared>, ActionError> {
        self.inner
            .lock()
            .unwrap()
            .records
            .get(id)
            .cloned()
            .ok_or_else(|| {
                ActionError::new(
                    "unknown_operation",
                    "operation is unknown or was evicted; do not automatically repeat its effects",
                )
            })
    }

    pub fn get(&self, id: &str) -> Result<OperationSnapshot, ActionError> {
        Ok(self.shared(id)?.snapshot.lock().unwrap().clone())
    }

    /// Returns cancel_requested until the worker actually exits. Terminal jobs
    /// retain their original outcome when cancellation arrives too late.
    pub fn cancel(&self, id: &str) -> Result<OperationSnapshot, ActionError> {
        let shared = self.shared(id)?;
        let mut state = shared.snapshot.lock().unwrap();
        if !state.execution.terminal() && state.execution != Execution::CancelRequested {
            state.execution = Execution::CancelRequested;
            state.revision += 1;
            shared.publish(&state);
        }
        Ok(state.clone())
    }

    /// Wait off the UI thread for a revision change or timeout. No polling.
    pub fn wait(
        &self,
        id: &str,
        after_revision: u64,
        timeout: Duration,
    ) -> Result<OperationSnapshot, ActionError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| ActionError::new("invalid_request", "invalid operation wait timeout"))?;
        let shared = self.shared(id)?;
        let state = shared.snapshot.lock().unwrap();
        if after_revision > state.revision {
            return Err(ActionError::new(
                "invalid_request",
                "afterRevision is ahead of the operation",
            ));
        }
        let (state, _) = shared
            .changed
            .wait_timeout_while(
                state,
                deadline.saturating_duration_since(Instant::now()),
                |state| state.revision == after_revision && !state.execution.terminal(),
            )
            .unwrap();
        Ok(state.clone())
    }
}

fn bound(text: &mut String, limit: usize) {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn terminal(operations: &Operations, mut job: OperationSnapshot) -> OperationSnapshot {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !job.execution.terminal() {
            assert!(
                Instant::now() < deadline,
                "operation did not reach a terminal outcome: {job:?}"
            );
            job = operations
                .wait(&job.operation_id, job.revision, Duration::from_millis(100))
                .unwrap();
        }
        job
    }
    #[test]
    fn progress_wait_cancel_and_late_cancel_preserve_outcomes() {
        let operations = Operations::default();
        let (started, entered) = std::sync::mpsc::channel();
        let job = operations
            .start("fixture", move |context| {
                context.report(0.25, "中文进度")?;
                started.send(()).unwrap();
                context.delay(Duration::from_secs(30))?;
                Ok(json!({}))
            })
            .unwrap();
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let running = operations.get(&job.operation_id).unwrap();
        assert_eq!(running.progress, 0.25);
        let cancelled = operations.cancel(&job.operation_id).unwrap();
        assert_eq!(cancelled.execution, Execution::CancelRequested);
        let done = terminal(&operations, cancelled);
        assert_eq!(done.execution, Execution::Cancelled);
        assert!(done.output.is_none());
        let job = operations.start("done", |_| Ok(json!("done"))).unwrap();
        let state = terminal(&operations, job);
        assert_eq!(state.execution, Execution::Completed);
        assert_eq!(
            operations.cancel(&state.operation_id).unwrap().execution,
            Execution::Completed
        );
    }
    #[test]
    fn running_limit_panics_and_finished_record_eviction_are_bounded() {
        let operations = Operations::default();
        let jobs: Vec<_> = (0..MAX_RUNNING)
            .map(|_| {
                operations
                    .start("slow", |context| {
                        context.delay(Duration::from_secs(30))?;
                        Ok(json!({}))
                    })
                    .unwrap()
            })
            .collect();
        assert_eq!(
            operations
                .start("overflow", |_| Ok(json!({})))
                .unwrap_err()
                .code,
            "operation_busy"
        );
        for job in jobs {
            let job = operations.cancel(&job.operation_id).unwrap();
            let done = terminal(&operations, job);
            assert_eq!(done.execution, Execution::Cancelled);
        }
        let job = operations.start("panic", |_| panic!("fixture")).unwrap();
        let first = job.operation_id.clone();
        let state = terminal(&operations, job);
        assert_eq!(state.error.unwrap().code, "operation_panicked");
        for _ in 0..MAX_RECORDS {
            let job = operations.start("done", |_| Ok(json!({}))).unwrap();
            terminal(&operations, job);
        }
        assert_eq!(
            operations.get(&first).unwrap_err().code,
            "unknown_operation"
        );
        assert_eq!(operations.inner.lock().unwrap().records.len(), MAX_RECORDS);
    }
}
