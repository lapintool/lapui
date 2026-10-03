use crate::action::ActionError;
use serde_json::{json, Value};
use std::time::Duration;

const MAX_CONTROL_WAIT: Duration = Duration::from_secs(4);
const CONTROL_WAIT_POLL: Duration = Duration::from_millis(20);

/// Wait for a renderer opportunity using the bounded frame revision counter.
/// This stays available when detailed tracing is disabled.
pub fn wait_for_render_revision_with(
    request: &Value,
    mut read_status: impl FnMut(Value, Duration) -> Result<Value, ActionError>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<Value, ActionError> {
    let invalid = |message: &str| ActionError::new("invalid_request", message);
    let fields = request
        .as_object()
        .ok_or_else(|| invalid("waitForRender requires an object"))?;
    if fields.keys().any(|key| {
        ![
            "method",
            "documentEpoch",
            "afterRevision",
            "timeoutMs",
            "waitId",
        ]
        .contains(&key.as_str())
    }) {
        return Err(invalid("unknown waitForRender field"));
    }
    let epoch = request
        .get("documentEpoch")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("documentEpoch must be a positive integer"))?;
    let revision = request
        .get("afterRevision")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("afterRevision must be a positive render revision"))?;
    let timeout = request
        .get("timeoutMs")
        .map(|value| {
            value
                .as_u64()
                .filter(|millis| *millis <= MAX_CONTROL_WAIT.as_millis() as u64)
                .map(Duration::from_millis)
                .ok_or_else(|| invalid("timeoutMs must be between 0 and 4000"))
        })
        .transpose()?
        .unwrap_or(Duration::from_secs(1));
    if request
        .get("waitId")
        .and_then(Value::as_str)
        .is_none_or(|id| id.is_empty() || id.len() > 128)
    {
        return Err(invalid("waitId must be 1..128 bytes"));
    }
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if is_cancelled() {
            return Err(ActionError::new(
                "wait_cancelled",
                "render wait was cancelled",
            ));
        }
        let status = read_status(
            json!({"method":"renderStatus","documentEpoch":epoch,"afterRevision":revision}),
            Duration::from_secs(1),
        )?;
        if status.get("documentEpoch").and_then(Value::as_u64) != Some(epoch) {
            return Err(ActionError::new(
                "stale_document",
                "document changed during render wait",
            ));
        }
        if status
            .get("issuedRevision")
            .and_then(Value::as_u64)
            .is_none_or(|issued| issued < revision)
        {
            return Err(invalid(
                "afterRevision is ahead of the current render revision",
            ));
        }
        match status.get("status").and_then(Value::as_str) {
            Some("rendered") | Some("render_unavailable") => return Ok(status),
            Some("pending") => {}
            _ => return Err(invalid("render status response is invalid")),
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(
                json!({"status":"timed_out","documentEpoch":epoch,"afterRevision":revision,
                "issuedRevision":status["issuedRevision"],"completedRevision":status["completedRevision"],
                "boundary":"renderer_returned","physicalPresentation":"unknown"}),
            );
        }
        std::thread::sleep(CONTROL_WAIT_POLL.min(deadline.saturating_duration_since(now)));
    }
}

pub fn trace_is_descendant(
    mut sequence: u64,
    ancestor: u64,
    parents: &std::collections::HashMap<u64, Option<u64>>,
) -> bool {
    for _ in 0..parents.len().saturating_add(1) {
        if sequence == ancestor {
            return true;
        }
        let Some(Some(parent)) = parents.get(&sequence) else {
            return false;
        };
        sequence = *parent;
    }
    false
}

pub fn consume_render_trace_page(
    records: &[Value],
    root_sequence: u64,
    parents: &mut std::collections::HashMap<u64, Option<u64>>,
    linked_frames: &mut std::collections::HashSet<u64>,
    layout_parents: &mut std::collections::HashMap<u64, u64>,
    resolved_layouts: &mut std::collections::HashSet<u64>,
) -> Option<Value> {
    for record in records {
        if let Some(sequence) = record.get("sequence").and_then(Value::as_u64) {
            parents.insert(
                sequence,
                record.get("parentSequence").and_then(Value::as_u64),
            );
        }
    }
    for record in records {
        let sequence = record.get("sequence").and_then(Value::as_u64)?;
        match (record["kind"].as_str()?, record["phase"].as_str()?) {
            ("frame", "start") => {
                let linked = record["data"]["causes"].as_array().is_some_and(|causes| {
                    causes
                        .iter()
                        .filter_map(Value::as_u64)
                        .any(|cause| trace_is_descendant(cause, root_sequence, parents))
                });
                if linked {
                    linked_frames.insert(sequence);
                }
            }
            ("layout", "start") => {
                if let Some(frame_sequence) = record["parentSequence"].as_u64() {
                    layout_parents.insert(sequence, frame_sequence);
                }
            }
            ("layout", "end") if record["data"]["outcome"] == "resolved" => {
                if let Some(layout_sequence) = record["parentSequence"].as_u64() {
                    if let Some(frame_sequence) = layout_parents.remove(&layout_sequence) {
                        resolved_layouts.insert(frame_sequence);
                    }
                }
            }
            ("frame", "end") => {
                if let Some(frame_sequence) = record["parentSequence"].as_u64() {
                    if linked_frames.remove(&frame_sequence)
                        && resolved_layouts.remove(&frame_sequence)
                    {
                        return Some(record.clone());
                    }
                }
            }
            _ => {}
        }
    }
    None
}

pub fn wait_for_render_with(
    request: &Value,
    mut read_trace: impl FnMut(Value, Duration) -> Result<Value, ActionError>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<Value, ActionError> {
    let invalid = |message: &str| ActionError::new("invalid_request", message);
    let fields = request
        .as_object()
        .ok_or_else(|| invalid("waitForRender requires an object"))?;
    if fields.keys().any(|key| {
        ![
            "method",
            "documentEpoch",
            "afterSequence",
            "timeoutMs",
            "waitId",
        ]
        .contains(&key.as_str())
    }) {
        return Err(invalid("unknown waitForRender field"));
    }
    let epoch = request
        .get("documentEpoch")
        .and_then(Value::as_u64)
        .filter(|epoch| *epoch > 0)
        .ok_or_else(|| invalid("documentEpoch must be a positive integer"))?;
    let root_sequence = request
        .get("afterSequence")
        .and_then(Value::as_u64)
        .filter(|sequence| *sequence > 0)
        .ok_or_else(|| invalid("afterSequence must be a positive trace sequence"))?;
    let timeout = request
        .get("timeoutMs")
        .map(|value| {
            value
                .as_u64()
                .filter(|millis| *millis <= MAX_CONTROL_WAIT.as_millis() as u64)
                .map(Duration::from_millis)
                .ok_or_else(|| invalid("timeoutMs must be between 0 and 4000"))
        })
        .transpose()?
        .unwrap_or(Duration::from_secs(1));
    let deadline = std::time::Instant::now() + timeout;
    if request
        .get("waitId")
        .and_then(Value::as_str)
        .is_none_or(|id| id.is_empty() || id.len() > 128)
    {
        return Err(invalid("waitId must be 1..128 bytes"));
    }
    let mut cursor = root_sequence - 1;
    let mut session = None;
    let mut saw_root = false;
    let mut parents = std::collections::HashMap::new();
    let mut linked_frames = std::collections::HashSet::new();
    let mut layout_parents = std::collections::HashMap::new();
    let mut resolved_layouts = std::collections::HashSet::new();
    loop {
        if is_cancelled() {
            return Err(ActionError::new(
                "wait_cancelled",
                "render wait was cancelled",
            ));
        }
        let page = read_trace(
            json!({"method":"debugTrace.read","documentEpoch":epoch,"afterSequence":cursor,"limit":128}),
            Duration::from_secs(1),
        )?;
        if page.get("documentEpoch").and_then(Value::as_u64) != Some(epoch) {
            return Err(ActionError::new(
                "stale_document",
                "document changed during render wait",
            ));
        }
        if page["enabled"] != true {
            return Err(ActionError::new(
                "unsupported_capability",
                "debug trace must stay enabled while waiting for render",
            ));
        }
        let current_session = page["session"]
            .as_u64()
            .ok_or_else(|| invalid("trace session is missing"))?;
        if session.is_some_and(|session| session != current_session) {
            return Err(ActionError::new(
                "trace_reset",
                "debug trace session changed during render wait",
            ));
        }
        session = Some(current_session);
        if page["resyncRequired"] == true {
            return Err(ActionError::new(
                "trace_resync",
                "trace records needed for this wait were evicted",
            ));
        }
        let records = page["records"]
            .as_array()
            .ok_or_else(|| invalid("trace records are missing"))?;
        if let Some(root) = records
            .iter()
            .find(|record| record["sequence"] == root_sequence)
        {
            let is_control_root = root["kind"] == "control" && root["phase"] == "start";
            let is_page_change_root = root["kind"] == "page_change" && root["phase"] == "instant";
            if !is_control_root && !is_page_change_root {
                return Err(ActionError::new(
                    "invalid_request",
                    "afterSequence must identify a control or page-change trace root",
                ));
            }
            saw_root = true;
        }
        if !saw_root && page["latestSequence"].as_u64().unwrap_or(0) < root_sequence {
            return Err(ActionError::new(
                "invalid_request",
                "afterSequence is ahead of the current trace",
            ));
        }
        let frame_end = consume_render_trace_page(
            records,
            root_sequence,
            &mut parents,
            &mut linked_frames,
            &mut layout_parents,
            &mut resolved_layouts,
        );
        if !saw_root && page["latestSequence"].as_u64().unwrap_or(0) >= root_sequence {
            return Err(ActionError::new(
                "trace_resync",
                "root control trace is not retained",
            ));
        }
        if let Some(frame) = frame_end {
            let outcome = frame["data"]["outcome"].as_str().unwrap_or("unknown");
            return Ok(
                json!({"status":if outcome == "renderer_returned" {"rendered"} else {"render_unavailable"},
                "documentEpoch":epoch,"rootSequence":root_sequence,"frameSequence":frame["sequence"],
                "frameOutcome":outcome,"boundary":"renderer_returned","physicalPresentation":"unknown"}),
            );
        }
        if is_cancelled() {
            return Err(ActionError::new(
                "wait_cancelled",
                "render wait was cancelled",
            ));
        }
        cursor = page["nextSequence"].as_u64().unwrap_or(cursor);
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(
                json!({"status":"timed_out","documentEpoch":epoch,"rootSequence":root_sequence,
                "latestSequence":page["latestSequence"],"boundary":"renderer_returned","physicalPresentation":"unknown"}),
            );
        }
        std::thread::sleep(CONTROL_WAIT_POLL.min(deadline.saturating_duration_since(now)));
    }
}

#[cfg(test)]
mod revision_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn revision_wait_succeeds_without_debug_trace() {
        let reads = Cell::new(0);
        let result = wait_for_render_revision_with(
            &json!({"method":"waitForRenderRevision","documentEpoch":7,"afterRevision":3,"timeoutMs":100,"waitId":"ready-1"}),
            |request, _| {
                assert_eq!(request["method"], "renderStatus");
                assert_eq!(request["afterRevision"], 3);
                let read = reads.get() + 1;
                reads.set(read);
                Ok(json!({"documentEpoch":7,"afterRevision":3,"issuedRevision":3,
                    "completedRevision":if read == 1 {0} else {3},
                    "status":if read == 1 {"pending"} else {"rendered"},
                    "frameOutcome":"renderer_returned","boundary":"renderer_returned",
                    "physicalPresentation":"unknown"}))
            },
            || false,
        ).unwrap();
        assert_eq!(result["status"], "rendered");
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn revision_wait_rejects_out_of_range_and_times_out_boundedly() {
        let ahead = wait_for_render_revision_with(
            &json!({"documentEpoch":7,"afterRevision":4,"timeoutMs":0,"waitId":"ready-2"}),
            |_, _| Ok(json!({"documentEpoch":7,"status":"pending","issuedRevision":3,"completedRevision":0})),
            || false,
        ).unwrap_err();
        assert_eq!(ahead.code, "invalid_request");

        let result = wait_for_render_revision_with(
            &json!({"documentEpoch":7,"afterRevision":3,"timeoutMs":0,"waitId":"ready-3"}),
            |_, _| Ok(json!({"documentEpoch":7,"status":"pending","issuedRevision":3,"completedRevision":0})),
            || false,
        ).unwrap();
        assert_eq!(result["status"], "timed_out");
    }
}
