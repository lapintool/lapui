//! Application-owned, bounded change history with resumable scoped long polls.
//! Cursors are observations, not permissions or idempotency keys.
use crate::action::{ActionError, ActionTrace};
use crate::operation::OperationSummary;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_RECORDS: usize = 512;
const MAX_BYTES: usize = 256 * 1024;
const MAX_EVENT_BYTES: usize = 8 * 1024;
const MAX_DELTA_BYTES: usize = 4 * 1024;
const MAX_PAGE: usize = 128;
const MAX_WAITERS: usize = 4;
const MAX_WAIT_MS: u64 = 1000;
static IDENTITIES: AtomicU64 = AtomicU64::new(1);

fn encoded_size(value: &impl Serialize, limit: usize) -> Option<usize> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(io::Error::other("JSON byte limit exceeded"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.bytes)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeScope {
    #[default]
    Application,
    State,
    Actions,
    Operations,
    Host,
}

impl ChangeScope {
    fn name(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::State => "state",
            Self::Actions => "actions",
            Self::Operations => "operations",
            Self::Host => "host",
        }
    }
    fn includes(self, event: &ChangeEvent) -> bool {
        self == Self::Application
            || self
                == match event {
                    ChangeEvent::StateChanged { .. } => Self::State,
                    ChangeEvent::ActionRegistered { .. }
                    | ChangeEvent::ActionUnregistered { .. }
                    | ChangeEvent::ActionFinished(_) => Self::Actions,
                    ChangeEvent::OperationChanged(_) | ChangeEvent::OperationRemoved { .. } => {
                        Self::Operations
                    }
                    ChangeEvent::HostEvent { .. } => Self::Host,
                }
    }
}

fn default_limit() -> usize {
    64
}
fn default_wait() -> u64 {
    MAX_WAIT_MS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangesRequest {
    #[serde(default)]
    pub scope: ChangeScope,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_wait")]
    pub wait_ms: u64,
}

impl Default for ChangesRequest {
    fn default() -> Self {
        Self {
            scope: ChangeScope::Application,
            cursor: None,
            limit: default_limit(),
            wait_ms: default_wait(),
        }
    }
}

impl ChangesRequest {
    pub(crate) fn validate(&self) -> Result<(), ActionError> {
        if !(1..=MAX_PAGE).contains(&self.limit) || self.wait_ms > MAX_WAIT_MS {
            return Err(ActionError::new(
                "invalid_request",
                "changes limit must be 1..128 and waitMs 0..1000",
            ));
        }
        if let Some(cursor) = &self.cursor {
            if cursor.len() > 256
                || cursor.rsplit_once(':').is_none_or(|(identity, sequence)| {
                    identity.is_empty() || sequence.parse::<u64>().is_err()
                })
            {
                return Err(ActionError::new(
                    "invalid_cursor",
                    "cursor must be an opaque changes cursor returned by the runtime",
                ));
            }
        }
        Ok(())
    }
}

/// Top-level object edits, or a complete replacement for non-object state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StateDelta {
    Fields {
        set: serde_json::Map<String, Value>,
        remove: Vec<String>,
    },
    Replace {
        value: Value,
    },
}

impl StateDelta {
    pub(crate) fn between(before: &Value, after: &Value) -> Option<Self> {
        let delta = match (before.as_object(), after.as_object()) {
            (Some(before), Some(after)) => Self::Fields {
                set: after
                    .iter()
                    .filter(|(key, value)| before.get(*key) != Some(*value))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                remove: before
                    .keys()
                    .filter(|key| !after.contains_key(*key))
                    .cloned()
                    .collect(),
            },
            _ => Self::Replace {
                value: after.clone(),
            },
        };
        encoded_size(&delta, MAX_DELTA_BYTES).map(|_| delta)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ChangeEvent {
    ActionRegistered {
        action_id: String,
    },
    ActionUnregistered {
        action_id: String,
    },
    ActionFinished(ActionTrace),
    StateChanged {
        action_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        base_version: u64,
        version: u64,
        delta: Option<StateDelta>,
        requires_snapshot: bool,
    },
    OperationChanged(OperationSummary),
    OperationRemoved {
        operation_id: String,
    },
    HostEvent {
        name: String,
        payload: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRecord {
    pub sequence: u64,
    #[serde(flatten)]
    pub event: ChangeEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePage {
    pub scope: ChangeScope,
    pub cursor: String,
    pub resync_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resync_reason: Option<ResyncReason>,
    pub has_more: bool,
    pub records: Vec<ChangeRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResyncReason {
    CursorContextChanged,
    CursorAhead,
    HistoryEvicted,
    SequenceExhausted,
}

#[derive(Clone)]
pub(crate) struct ChangeLog(Arc<Shared>);
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}
struct State {
    identity: String,
    sequence: u64,
    exhausted: bool,
    bytes: usize,
    records: VecDeque<(ChangeRecord, usize)>,
    waiters: usize,
}

impl Default for ChangeLog {
    fn default() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self(Arc::new(Shared {
            state: Mutex::new(State {
                identity: format!(
                    "changes-v1-{}-{timestamp:x}-{}",
                    std::process::id(),
                    IDENTITIES.fetch_add(1, Ordering::Relaxed)
                ),
                sequence: 0,
                exhausted: false,
                bytes: 0,
                records: VecDeque::new(),
                waiters: 0,
            }),
            changed: Condvar::new(),
        }))
    }
}

impl State {
    fn cursor(&self, scope: ChangeScope, sequence: u64) -> String {
        format!("{}:{}:{sequence}", self.identity, scope.name())
    }
    fn page(&self, request: &ChangesRequest, sequence: u64) -> ChangePage {
        let valid = request
            .cursor
            .as_ref()
            .is_some_and(|cursor| *cursor == self.cursor(request.scope, sequence));
        let resync_reason = if !valid {
            Some(ResyncReason::CursorContextChanged)
        } else if self.exhausted {
            Some(ResyncReason::SequenceExhausted)
        } else if sequence > self.sequence {
            Some(ResyncReason::CursorAhead)
        } else if self
            .records
            .front()
            .is_some_and(|(record, _)| sequence < record.sequence.saturating_sub(1))
        {
            Some(ResyncReason::HistoryEvicted)
        } else {
            None
        };
        let resync_required = resync_reason.is_some();
        let mut records = Vec::new();
        let mut scanned = sequence;
        let mut has_more = false;
        if !resync_required {
            for (record, _) in self
                .records
                .iter()
                .filter(|(record, _)| record.sequence > sequence)
            {
                if request.scope.includes(&record.event) {
                    if records.len() == request.limit {
                        has_more = true;
                        break;
                    }
                    records.push(record.clone());
                }
                scanned = record.sequence;
            }
        }
        if !has_more {
            scanned = self.sequence;
        }
        ChangePage {
            scope: request.scope,
            cursor: self.cursor(request.scope, scanned),
            resync_required,
            resync_reason,
            has_more,
            records,
            baseline: None,
        }
    }
}

impl ChangeLog {
    pub(crate) fn limits() -> Value {
        json!({"historyRecords":MAX_RECORDS,"historyBytes":MAX_BYTES,"eventBytes":MAX_EVENT_BYTES,
            "deltaBytes":MAX_DELTA_BYTES,"pageRecords":MAX_PAGE,"waiters":MAX_WAITERS,"waitMillis":MAX_WAIT_MS,
            "stateDelta":"top-level-fields-or-root-replacement",
            "transport":"bounded-long-poll","durable":false,"scopes":["application","state","actions","operations","host"]})
    }
    pub(crate) fn publish(&self, event: ChangeEvent) -> Result<(), ActionError> {
        let mut state = self.0.state.lock().unwrap();
        let Some(sequence) = state.sequence.checked_add(1) else {
            state.exhausted = true;
            self.0.changed.notify_all();
            return Err(ActionError::new(
                "changes_exhausted",
                "change history sequence exhausted; a new registry is required",
            ));
        };
        let record = ChangeRecord { sequence, event };
        let bytes = encoded_size(&record, MAX_EVENT_BYTES)
            .ok_or_else(|| ActionError::new("event_too_large", "change event exceeds 8 KiB"))?;
        state.sequence = sequence;
        state.bytes += bytes;
        state.records.push_back((record, bytes));
        while state.records.len() > MAX_RECORDS || state.bytes > MAX_BYTES {
            let (_, bytes) = state.records.pop_front().unwrap();
            state.bytes -= bytes;
        }
        self.0.changed.notify_all();
        Ok(())
    }
    pub(crate) fn checkpoint(&self, scope: ChangeScope) -> String {
        let state = self.0.state.lock().unwrap();
        state.cursor(scope, state.sequence)
    }
    pub(crate) fn page(&self, request: &ChangesRequest) -> Result<ChangePage, ActionError> {
        request.validate()?;
        let sequence = request
            .cursor
            .as_ref()
            .and_then(|cursor| cursor.rsplit_once(':'))
            .and_then(|(_, number)| number.parse::<u64>().ok())
            .unwrap_or(0);
        let deadline = Instant::now() + Duration::from_millis(request.wait_ms);
        let mut state = self.0.state.lock().unwrap();
        let initial = state.page(request, sequence);
        if initial.resync_required || !initial.records.is_empty() || request.wait_ms == 0 {
            return Ok(initial);
        }
        if state.waiters == MAX_WAITERS {
            return Err(ActionError::new(
                "changes_busy",
                "at most four change requests may wait",
            ));
        }
        state.waiters += 1;
        let page = loop {
            let sequence_now = state.sequence;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break state.page(request, sequence);
            }
            state = self
                .0
                .changed
                .wait_timeout_while(state, remaining, |state| {
                    state.sequence == sequence_now && !state.exhausted
                })
                .unwrap()
                .0;
            let page = state.page(request, sequence);
            if page.resync_required || !page.records.is_empty() {
                break page;
            }
        };
        state.waiters -= 1;
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn resumed(log: &ChangeLog, scope: ChangeScope) -> ChangesRequest {
        ChangesRequest {
            scope,
            cursor: Some(log.checkpoint(scope)),
            ..Default::default()
        }
    }
    fn await_waiters(log: &ChangeLog, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while log.0.state.lock().unwrap().waiters != count {
            assert!(
                Instant::now() < deadline,
                "waiter count did not reach {count}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn long_polls_wake_on_matching_changes_bound_waiters_and_advance_past_unmatched_records() {
        let log = ChangeLog::default();
        let request = resumed(&log, ChangeScope::Host);
        let waiting: Vec<_> = (0..MAX_WAITERS)
            .map(|_| {
                let log = log.clone();
                let request = request.clone();
                thread::spawn(move || log.page(&request).unwrap())
            })
            .collect();
        await_waiters(&log, MAX_WAITERS);
        assert_eq!(log.page(&request).unwrap_err().code, "changes_busy");
        log.publish(ChangeEvent::ActionRegistered {
            action_id: "irrelevant".into(),
        })
        .unwrap();
        await_waiters(&log, MAX_WAITERS);
        assert!(waiting.iter().all(|waiter| !waiter.is_finished()));
        log.publish(ChangeEvent::HostEvent {
            name: "ready".into(),
            payload: json!({"value":1}),
        })
        .unwrap();
        for waiter in waiting {
            let page = waiter.join().unwrap();
            assert_eq!(page.records.len(), 1);
            assert!(
                matches!(&page.records[0].event, ChangeEvent::HostEvent { name, .. } if name == "ready")
            );
        }
        await_waiters(&log, 0);
        let mut request = resumed(&log, ChangeScope::State);
        request.wait_ms = 10;
        log.publish(ChangeEvent::HostEvent {
            name: "other".into(),
            payload: Value::Null,
        })
        .unwrap();
        let empty = log.page(&request).unwrap();
        assert!(empty.records.is_empty());
        assert!(!empty.resync_required);
        assert_ne!(empty.cursor, request.cursor.unwrap());
        assert_eq!(empty.cursor, log.checkpoint(ChangeScope::State));
    }

    #[test]
    fn pages_preserve_filtered_record_order_and_report_count_byte_and_identity_gaps() {
        let log = ChangeLog::default();
        let mut request = resumed(&log, ChangeScope::Host);
        request.limit = 1;
        request.wait_ms = 0;
        for value in 0..3 {
            log.publish(ChangeEvent::HostEvent {
                name: "fixture".into(),
                payload: json!(value),
            })
            .unwrap();
            log.publish(ChangeEvent::ActionRegistered {
                action_id: "noise".into(),
            })
            .unwrap();
        }
        let mut sequences = Vec::new();
        loop {
            let page = log.page(&request).unwrap();
            assert!(!page.resync_required);
            sequences.extend(page.records.iter().map(|record| record.sequence));
            request.cursor = Some(page.cursor);
            if !page.has_more {
                break;
            }
        }
        assert_eq!(sequences, vec![1, 3, 5]);
        let old = resumed(&log, ChangeScope::Host);
        for _ in 0..MAX_RECORDS + 1 {
            log.publish(ChangeEvent::HostEvent {
                name: "tiny".into(),
                payload: Value::Null,
            })
            .unwrap();
        }
        assert_eq!(log.0.state.lock().unwrap().records.len(), MAX_RECORDS);
        assert!(log.page(&old).unwrap().resync_required);
        let byte_cursor = resumed(&log, ChangeScope::Host);
        for _ in 0..100 {
            log.publish(ChangeEvent::HostEvent {
                name: "large".into(),
                payload: json!("x".repeat(7000)),
            })
            .unwrap();
        }
        let state = log.0.state.lock().unwrap();
        assert!(state.bytes <= MAX_BYTES);
        assert!(state.records.len() < MAX_RECORDS);
        drop(state);
        assert!(log.page(&byte_cursor).unwrap().resync_required);
        let other = ChangeLog::default();
        assert!(
            other
                .page(&resumed(&log, ChangeScope::Host))
                .unwrap()
                .resync_required
        );
        let mut scope_change = resumed(&log, ChangeScope::Host);
        scope_change.scope = ChangeScope::State;
        assert!(log.page(&scope_change).unwrap().resync_required);
        let before = log.checkpoint(ChangeScope::Host);
        assert_eq!(
            log.publish(ChangeEvent::HostEvent {
                name: "oversized".into(),
                payload: json!("界".repeat(5000))
            })
            .unwrap_err()
            .code,
            "event_too_large"
        );
        assert_eq!(before, log.checkpoint(ChangeScope::Host));
    }
}
