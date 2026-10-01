use crate::action::ActionError;
use serde_json::{json, Value};
use std::time::Duration;

const MAX_CONTROL_WAIT: Duration = Duration::from_secs(4);
const CONTROL_WAIT_POLL: Duration = Duration::from_millis(20);

pub fn wait_for_control_with(
    request: &Value,
    mut read_controls: impl FnMut(Duration) -> Result<Value, ActionError>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<Value, ActionError> {
    let invalid = |message: &str| ActionError::new("invalid_request", message);
    let fields = request
        .as_object()
        .ok_or_else(|| invalid("waitForControl requires an object"))?;
    if fields.keys().any(|key| {
        ![
            "method",
            "documentEpoch",
            "id",
            "ref",
            "field",
            "equals",
            "contains",
            "timeoutMs",
            "waitId",
        ]
        .contains(&key.as_str())
    }) {
        return Err(invalid("unknown waitForControl field"));
    }
    let epoch = request
        .get("documentEpoch")
        .and_then(Value::as_u64)
        .filter(|epoch| *epoch > 0)
        .ok_or_else(|| invalid("documentEpoch must be a positive integer"))?;
    let has_id = fields.contains_key("id");
    let has_ref = fields.contains_key("ref");
    if has_id == has_ref {
        return Err(invalid("provide exactly one bounded id or ref selector"));
    }
    let id = request
        .get("id")
        .map(|value| value.as_str().ok_or_else(|| invalid("id must be a string")))
        .transpose()?;
    let reference = request
        .get("ref")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| invalid("ref must be a string"))
        })
        .transpose()?;
    if id.is_some_and(|value| value.is_empty() || value.len() > 128)
        || reference.is_some_and(|value| value.is_empty() || value.len() > 256)
    {
        return Err(invalid("provide exactly one bounded id or ref selector"));
    }
    let field = request
        .get("field")
        .and_then(Value::as_str)
        .filter(|field| {
            matches!(
                *field,
                "value" | "checked" | "focused" | "enabled" | "name" | "role"
            )
        })
        .ok_or_else(|| invalid("field must be value, checked, focused, enabled, name, or role"))?;
    let has_equals = fields.contains_key("equals");
    let has_contains = fields.contains_key("contains");
    if has_equals == has_contains {
        return Err(invalid("provide exactly one of equals or contains"));
    }
    let expected = request.get("equals").filter(|value| {
        if matches!(field, "checked" | "focused" | "enabled") {
            value.is_boolean()
        } else {
            value.is_string()
        }
    });
    let contains = request
        .get("contains")
        .and_then(Value::as_str)
        .filter(|_| !matches!(field, "checked" | "focused" | "enabled"));
    if (has_equals && expected.is_none())
        || (has_contains && contains.is_none_or(|part| part.is_empty() || part.len() > 256))
    {
        return Err(invalid(
            "condition must match the selected control field's value type",
        ));
    }
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
    let selector = |control: &Value| {
        id.is_some_and(|id| control.get("id").and_then(Value::as_str) == Some(id))
            || reference.is_some_and(|reference| {
                control.get("ref").and_then(Value::as_str) == Some(reference)
            })
    };
    loop {
        if is_cancelled() {
            return Err(ActionError::new(
                "wait_cancelled",
                "control wait was cancelled",
            ));
        }
        let snapshot = read_controls(Duration::from_secs(1))?;
        if snapshot.get("documentEpoch").and_then(Value::as_u64) != Some(epoch) {
            return Err(ActionError::new(
                "stale_document",
                "document changed while waiting for a control state",
            ));
        }
        let control = snapshot
            .get("controls")
            .and_then(Value::as_array)
            .and_then(|controls| controls.iter().find(|control| selector(control)))
            .ok_or_else(|| {
                ActionError::new("stale_reference", "control is not in this document")
            })?;
        let matches = expected.is_some_and(|expected| control.get(field) == Some(expected))
            || contains.is_some_and(|part| {
                control
                    .get(field)
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.contains(part))
            });
        if matches {
            return Ok(
                json!({"status":"matched","documentEpoch":epoch,"control":control,
                "boundary":"semantic_control_snapshot","physicalPresentation":"unsupported"}),
            );
        }
        if is_cancelled() {
            return Err(ActionError::new(
                "wait_cancelled",
                "control wait was cancelled",
            ));
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(
                json!({"status":"timed_out","documentEpoch":epoch,"control":control,
                "boundary":"semantic_control_snapshot","physicalPresentation":"unsupported"}),
            );
        }
        std::thread::sleep(CONTROL_WAIT_POLL.min(deadline.saturating_duration_since(now)));
    }
}
