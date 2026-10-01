use crate::{action::ActionError, control::PageChangeNotifier};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const MAX_WAIT: Duration = Duration::from_secs(4);
pub fn wait_for_page_changes_with(
    request: &Value,
    mut read_changes: impl FnMut(
        &str,
        u64,
        Duration,
    ) -> Result<(Value, PageChangeNotifier, u64), ActionError>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<Value, ActionError> {
    let invalid = |message: &str| ActionError::new("invalid_request", message);
    let epoch = request
        .get("documentEpoch")
        .and_then(Value::as_u64)
        .filter(|epoch| *epoch > 0)
        .ok_or_else(|| invalid("documentEpoch must be a positive integer"))?;
    let cursor = request
        .get("cursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty() && cursor.len() <= 256)
        .ok_or_else(|| invalid("cursor must be a non-empty page_changes cursor"))?;
    if request
        .get("waitId")
        .and_then(Value::as_str)
        .is_none_or(|id| id.is_empty() || id.len() > 128)
    {
        return Err(invalid("waitId must be 1..128 bytes"));
    }
    let timeout = request
        .get("timeoutMs")
        .map(|value| {
            value
                .as_u64()
                .filter(|millis| *millis <= MAX_WAIT.as_millis() as u64)
                .map(Duration::from_millis)
                .ok_or_else(|| invalid("timeoutMs must be between 0 and 4000"))
        })
        .transpose()?
        .unwrap_or(Duration::from_secs(1));
    let limit = request
        .get("limit")
        .and_then(Value::as_u64)
        .filter(|limit| (1..=64).contains(limit))
        .ok_or_else(|| invalid("limit must be between 1 and 64"))?;
    let deadline = Instant::now() + timeout;

    loop {
        if is_cancelled() {
            return Err(ActionError::new(
                "wait_cancelled",
                "page change wait was cancelled",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let request_timeout = Duration::from_secs(1).min(remaining.max(Duration::from_millis(50)));
        let (page, notifier, generation) = read_changes(cursor, limit, request_timeout)?;
        if is_cancelled() {
            return Err(ActionError::new(
                "wait_cancelled",
                "page change wait was cancelled",
            ));
        }
        if page.get("documentEpoch").and_then(Value::as_u64) != Some(epoch) {
            return Err(ActionError::new(
                "stale_document",
                "document changed while waiting for page changes",
            ));
        }
        if page.get("resyncRequired") == Some(&Value::Bool(true)) {
            return Ok(with_status(page, "resync_required"));
        }
        if page.get("sequenceExhausted") == Some(&Value::Bool(true)) {
            return Ok(with_status(page, "resync_required"));
        }
        if page
            .get("records")
            .and_then(Value::as_array)
            .is_some_and(|records| !records.is_empty())
        {
            return Ok(with_status(page, "changed"));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(with_status(page, "timed_out"));
        }
        // Capture the generation before reading the page so a change between
        // the query and the wait cannot be lost.
        notifier.wait_after(generation, deadline.saturating_duration_since(now));
        if is_cancelled() {
            return Err(ActionError::new(
                "wait_cancelled",
                "page change wait was cancelled",
            ));
        }
    }
}

fn with_status(mut page: Value, status: &str) -> Value {
    if let Some(object) = page.as_object_mut() {
        object.insert("status".into(), json!(status));
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn request(timeout_ms: u64) -> Value {
        json!({"documentEpoch":7,"cursor":"page:7:3","limit":8,"waitId":"test-wait","timeoutMs":timeout_ms})
    }

    #[test]
    fn returns_changes_resync_timeout_and_rejects_epoch_changes() {
        let calls = AtomicUsize::new(0);
        let notifier = PageChangeNotifier::default();
        let changed = wait_for_page_changes_with(&request(100), |cursor, limit, _| {
            assert_eq!(cursor, "page:7:3");
            assert_eq!(limit, 8);
            let generation = notifier.generation();
            let call = calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                // Force the notification into the query-to-wait gap. The
                // captured generation must make the next wait return at once.
                notifier.notify();
            }
            let page = json!({"documentEpoch":7,"records":if call == 0 {vec![]} else {vec![json!({"sequence":4})]},"resyncRequired":false,"sequenceExhausted":false});
            Ok((page, notifier.clone(), generation))
        }, || false)
        .unwrap();
        assert_eq!(changed["status"], "changed");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let resync = wait_for_page_changes_with(&request(0), |_, _, _| {
            Ok((json!({"documentEpoch":7,"records":[],"resyncRequired":true,"sequenceExhausted":false}), PageChangeNotifier::default(), 0))
        }, || false)
        .unwrap();
        assert_eq!(resync["status"], "resync_required");

        let timeout = wait_for_page_changes_with(&request(0), |_, _, _| {
            Ok((json!({"documentEpoch":7,"records":[],"resyncRequired":false,"sequenceExhausted":false}), PageChangeNotifier::default(), 0))
        }, || false)
        .unwrap();
        assert_eq!(timeout["status"], "timed_out");

        let stale = wait_for_page_changes_with(
            &request(0),
            |_, _, _| {
                Ok((
                    json!({"documentEpoch":8,"records":[],"resyncRequired":false}),
                    PageChangeNotifier::default(),
                    0,
                ))
            },
            || false,
        )
        .unwrap_err();
        assert_eq!(stale.code, "stale_document");
    }

    #[test]
    fn validates_wait_bounds_and_cursor() {
        let mut invalid = request(4001);
        assert_eq!(
            wait_for_page_changes_with(&invalid, |_, _, _| unreachable!(), || false)
                .unwrap_err()
                .code,
            "invalid_request"
        );
        invalid = request(0);
        invalid["cursor"] = json!("");
        assert_eq!(
            wait_for_page_changes_with(&invalid, |_, _, _| unreachable!(), || false)
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }
}
