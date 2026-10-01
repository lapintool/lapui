//! Opt-in document tracing. Callers pass metadata, never input or result bodies.
use crate::action::ActionError;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Instant;

const RECORDS: usize = 512;
const BYTES: usize = 256 * 1024;
const PAGE: usize = 128;
const CAUSES: usize = 64;

#[derive(Clone)]
pub(crate) struct DebugTrace(Rc<RefCell<State>>);
struct State {
    epoch: Instant,
    enabled: bool,
    generation: u64,
    sequence: u64,
    dropped_through: u64,
    active: Option<u64>,
    records: VecDeque<(Value, usize)>,
    bytes: usize,
    causes: Vec<u64>,
    causes_lost: bool,
}

pub(crate) struct Span {
    trace: DebugTrace,
    generation: u64,
    sequence: Option<u64>,
    previous: Option<u64>,
    kind: &'static str,
    started: Option<Instant>,
    finished: bool,
}

impl DebugTrace {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(State {
            epoch: Instant::now(),
            enabled: false,
            generation: 0,
            sequence: 0,
            dropped_through: 0,
            active: None,
            records: VecDeque::new(),
            bytes: 0,
            causes: Vec::new(),
            causes_lost: false,
        })))
    }
    pub fn limits() -> Value {
        json!({"records":RECORDS,"serializedBytes":BYTES,"pageRecords":PAGE,"frameCauses":CAUSES,"defaultEnabled":false})
    }
    pub fn enabled(&self) -> bool {
        self.0.borrow().enabled
    }
    pub fn configure(&self, enabled: bool, clear: bool) {
        let mut state = self.0.borrow_mut();
        if enabled != state.enabled || clear {
            state.generation += 1;
            state.active = None;
            state.causes.clear();
            state.causes_lost = false;
        }
        state.enabled = enabled;
        if clear {
            state.records.clear();
            state.bytes = 0;
            state.dropped_through = state.sequence;
        }
    }
    // Origin generations prevent a delayed completion linking to a later session.
    pub fn origin(&self, sequence: Option<u64>) -> Option<(u64, u64)> {
        sequence.map(|sequence| (self.0.borrow().generation, sequence))
    }
    pub fn live_origin(&self, origin: Option<(u64, u64)>) -> Option<u64> {
        origin
            .filter(|(generation, _)| *generation == self.0.borrow().generation)
            .map(|(_, sequence)| sequence)
    }
    pub fn instant(&self, kind: &'static str, data: Value, cause: bool) -> Option<u64> {
        let mut state = self.0.borrow_mut();
        let parent = state.active;
        state.record(kind, "instant", parent, data, cause)
    }
    pub fn span(&self, kind: &'static str, data: Value, parent: Option<u64>) -> Span {
        let mut state = self.0.borrow_mut();
        let previous = state.active;
        let sequence = state.record(kind, "start", parent.or(previous), data, false);
        if sequence.is_some() {
            state.active = sequence;
        }
        Span {
            trace: self.clone(),
            generation: state.generation,
            sequence,
            previous,
            kind,
            started: sequence.map(|_| Instant::now()),
            finished: false,
        }
    }
    pub fn take_causes(&self) -> (Vec<u64>, bool) {
        let mut state = self.0.borrow_mut();
        (
            std::mem::take(&mut state.causes),
            std::mem::take(&mut state.causes_lost),
        )
    }
    pub fn read(&self, after: u64, limit: usize) -> Result<Value, ActionError> {
        if limit == 0 || limit > PAGE {
            return Err(ActionError::new(
                "invalid_request",
                "debug trace limit must be 1..128",
            ));
        }
        let state = self.0.borrow();
        if after > state.sequence {
            return Err(ActionError::new(
                "invalid_request",
                "debug trace cursor is ahead of this document",
            ));
        }

        let records: Vec<_> = state
            .records
            .iter()
            .filter(|(value, _)| value["sequence"].as_u64().unwrap() > after)
            .take(limit)
            .map(|(value, _)| value.clone())
            .collect();
        let next = records
            .last()
            .map_or(state.sequence, |value| value["sequence"].as_u64().unwrap());
        Ok(
            json!({"enabled":state.enabled,"session":state.generation,"afterSequence":after,"nextSequence":next,
            "latestSequence":state.sequence,"resyncRequired":after<state.dropped_through,"records":records,
            "retainedRecords":state.records.len(),"serializedBytes":state.bytes,"limits":Self::limits(),"physicalPresentation":"unknown"}),
        )
    }
}

impl State {
    fn record(
        &mut self,
        kind: &'static str,
        phase: &'static str,
        parent: Option<u64>,
        data: Value,
        cause: bool,
    ) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        self.sequence += 1;
        let value = json!({"sequence":self.sequence,"session":self.generation,"timestampMillis":self.epoch.elapsed().as_secs_f64()*1000.0,
            "kind":kind,"phase":phase,"parentSequence":parent,"data":data});
        let bytes = value.to_string().len();
        // Internal callers use bounded metadata. Keep the log bounded even if a
        // future internal caller accidentally passes an oversized record.
        if bytes <= BYTES {
            while self.records.len() >= RECORDS || self.bytes + bytes > BYTES {
                let (dropped, bytes) = self.records.pop_front().unwrap();
                self.bytes -= bytes;
                self.dropped_through = self
                    .dropped_through
                    .max(dropped["sequence"].as_u64().unwrap());
            }
            self.bytes += bytes;
            self.records.push_back((value, bytes));
        } else {
            self.dropped_through = self.sequence;
        }
        if cause {
            if self.causes.len() == CAUSES {
                self.causes.remove(0);
                self.causes_lost = true;
            }
            self.causes.push(self.sequence);
        }
        Some(self.sequence)
    }
}

impl Span {
    pub fn sequence(&self) -> Option<u64> {
        self.sequence
    }
    pub fn elapsed_millis(&self) -> Option<f64> {
        self.started
            .map(|start| start.elapsed().as_secs_f64() * 1000.0)
    }
    pub fn finish(mut self, mut data: Value, cause: bool) {
        if let Some(object) = data.as_object_mut() {
            object.insert("durationMillis".into(), json!(self.elapsed_millis()));
        }
        let mut state = self.trace.0.borrow_mut();
        if self.sequence.is_some() && self.generation == state.generation {
            state.record(self.kind, "end", self.sequence, data, cause);
            if state.active == self.sequence {
                state.active = self.previous;
            }
        }
        self.finished = true;
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if self.finished || self.sequence.is_none() {
            return;
        }
        let mut state = self.trace.0.borrow_mut();
        if self.generation == state.generation {
            state.record(
                self.kind,
                "end",
                self.sequence,
                json!({"outcome":"unfinished"}),
                false,
            );
            if state.active == self.sequence {
                state.active = self.previous;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_scopes_nested_causality_pages_eviction_and_sessions_are_bounded() {
        let trace = DebugTrace::new();
        trace.span("input", json!({}), None).finish(json!({}), true);
        assert_eq!(trace.read(0, 128).unwrap()["latestSequence"], 0);
        trace.configure(true, false);
        let input = trace.span("input", json!({"kind":"keyDown"}), None);
        let request = trace
            .instant("host_request", json!({"action":"test"}), false)
            .unwrap();
        let origin = trace.origin(Some(request));
        input.finish(json!({"outcome":"handled"}), true);
        let page = trace.read(0, 2).unwrap();
        assert_eq!(page["records"][1]["parentSequence"], 1);
        assert_eq!(page["nextSequence"], 2);
        assert_eq!(trace.take_causes().0, vec![3]);
        for _ in 0..600 {
            trace.instant("input", json!({"metadata":"x".repeat(1024)}), true);
        }
        let page = trace.read(0, 128).unwrap();
        assert_eq!(page["resyncRequired"], true);
        assert!(page["serializedBytes"].as_u64().unwrap() <= BYTES as u64);
        assert!(page["retainedRecords"].as_u64().unwrap() <= RECORDS as u64);
        let (causes, lost) = trace.take_causes();
        assert_eq!(causes.len(), 64);
        assert!(lost);
        trace.configure(false, false);
        trace.configure(true, true);
        assert!(trace.live_origin(origin).is_none());
        assert_eq!(trace.read(0, 128).unwrap()["resyncRequired"], true);
        assert!(trace.read(u64::MAX, 1).is_err());
        assert!(trace.read(0, 129).is_err());
        let before = trace.read(0, 1).unwrap()["latestSequence"]
            .as_u64()
            .unwrap();
        let abandoned = trace.span("input", json!({}), None);
        drop(abandoned);
        assert_eq!(
            trace.read(before, 128).unwrap()["records"][1]["data"]["outcome"],
            "unfinished"
        );
    }
}
