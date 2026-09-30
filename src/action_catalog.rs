//! Small action discovery responses and explicit business availability.
use crate::action::{ActionError, ActionInfo, ActionKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

pub(crate) type AvailabilityCheck = dyn Fn(&Value, &Value) -> Result<(), ActionError> + Send + Sync;

/// Checks must be read-only, fast and use the supplied state/arguments.
/// They are enforced again on fresh invocations before the handler runs.
#[derive(Clone, Default)]
pub struct ActionOptions {
    pub(crate) availability: Option<Arc<AvailabilityCheck>>,
}

impl ActionOptions {
    pub fn with_availability(
        mut self,
        check: impl Fn(&Value, &Value) -> Result<(), ActionError> + Send + Sync + 'static,
    ) -> Self {
        self.availability = Some(Arc::new(check));
        self
    }
}

fn default_limit() -> usize {
    32
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionsRequest {
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

impl Default for ActionsRequest {
    fn default() -> Self {
        Self {
            prefix: String::new(),
            scope: None,
            cursor: None,
            limit: default_limit(),
        }
    }
}

impl ActionsRequest {
    pub(crate) fn validate(&self) -> Result<(), ActionError> {
        if self.prefix.len() > 128
            || self
                .scope
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > 128)
            || self.cursor.as_ref().is_some_and(|value| value.len() > 1024)
            || !(1..=64).contains(&self.limit)
        {
            return Err(ActionError::new("invalid_request", "action filters must be at most 128 bytes, cursor at most 1024 bytes and limit 1..64"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionSummary {
    pub id: String,
    pub description: String,
    pub kind: ActionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_name: Option<String>,
    pub has_availability_check: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionsPage {
    pub revision: u64,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub items: Vec<ActionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionDescription {
    #[serde(flatten)]
    pub info: ActionInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_name: Option<String>,
    pub has_availability_check: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionAvailability {
    pub action_id: String,
    pub version: u64,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<ActionError>,
}
