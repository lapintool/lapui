//! Model Context Protocol adapter for a running Lapui document.
//!
//! Only bounded, allow-listed semantic operations are exposed. There is no
//! JavaScript evaluation or arbitrary DOM access.

use crate::{
    action::ActionRegistry, control::DocumentController, control_wait::wait_for_control_with,
    page_change_wait::wait_for_page_changes_with, reload::ReloadHandle,
    render_wait::wait_for_render_with,
};
use rmcp::{
    handler::server::tool::schema_for_type,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const MAX_TOOL_RESULT_BYTES: usize = 24 * 1024;
const MAX_ACTION_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_ACTIVE_WAITS: usize = 4;

/// MCP structured outputs are JSON objects whose operation-specific fields can
/// vary by document state and action result. Keep the declared boundary honest:
/// constrain the top-level container while allowing those documented fields.
#[derive(schemars::JsonSchema)]
#[schemars(transparent)]
#[allow(dead_code)] // Used as a schema-only output type.
struct StructuredToolOutput(HashMap<String, Value>);

#[derive(schemars::JsonSchema)]
#[schemars(rename_all = "camelCase")]
#[allow(dead_code)] // Used as a schema-only output type.
struct AppDescribeOutput {
    protocol_version: u32,
    runtime: String,
    capabilities: Vec<String>,
    script_evaluation: bool,
}

#[derive(schemars::JsonSchema)]
#[schemars(rename_all = "camelCase")]
#[allow(dead_code)] // Used as a schema-only output type.
struct PageObserveOutput {
    document_epoch: u64,
    root_ref: Option<String>,
    items: Vec<Value>,
    next_after: Option<String>,
    truncated: bool,
    scan_limit_reached: bool,
    cursor_consistency: String,
}

#[derive(schemars::JsonSchema)]
#[schemars(rename_all = "camelCase")]
#[allow(dead_code)] // Used as a schema-only output type.
struct PageChangesOutput {
    latest_sequence: u64,
    next_sequence: u64,
    oldest_sequence: u64,
    has_more: bool,
    resync_required: bool,
    sequence_exhausted: bool,
    records: Vec<Value>,
    document_epoch: u64,
    cursor: String,
}

static ACTIVE_WAITS: OnceLock<Mutex<HashMap<String, Arc<WaitToken>>>> = OnceLock::new();

struct WaitToken {
    cancelled: AtomicBool,
    page_change_notifier: Mutex<Option<crate::control::PageChangeNotifier>>,
}

impl WaitToken {
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            page_change_notifier: Mutex::new(None),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(notifier) = self.page_change_notifier.lock().unwrap().as_ref() {
            notifier.notify();
        }
    }

    fn bind_page_change_notifier(&self, notifier: crate::control::PageChangeNotifier) {
        *self.page_change_notifier.lock().unwrap() = Some(notifier.clone());
        if self.cancelled.load(Ordering::Acquire) {
            notifier.notify();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

struct WaitGuard {
    id: String,
    token: Arc<WaitToken>,
}

impl WaitGuard {
    fn register(id: &str) -> Result<Self, crate::action::ActionError> {
        if id.is_empty() || id.len() > 128 {
            return Err(crate::action::ActionError::new(
                "invalid_request",
                "waitId must be 1..128 bytes",
            ));
        }
        let mut active = ACTIVE_WAITS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        if active.len() >= MAX_ACTIVE_WAITS {
            return Err(crate::action::ActionError::new(
                "wait_busy",
                "maximum active waits reached",
            ));
        }
        if active.contains_key(id) {
            return Err(crate::action::ActionError::new(
                "wait_conflict",
                "waitId is already active",
            ));
        }
        let token = Arc::new(WaitToken::new());
        active.insert(id.to_owned(), token.clone());
        Ok(Self {
            id: id.to_owned(),
            token,
        })
    }

    fn token(&self) -> Arc<WaitToken> {
        self.token.clone()
    }

    fn cancel(id: &str) -> bool {
        let token = ACTIVE_WAITS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .get(id)
            .cloned();
        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        if let Some(active) = ACTIVE_WAITS.get() {
            active.lock().unwrap().remove(&self.id);
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActionListInput {
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default = "default_page_size")]
    limit: u8,
}

fn default_page_size() -> u8 {
    32
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActionInput {
    action: String,
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    #[serde(default)]
    expected_version: Option<u64>,
    #[serde(default = "empty_object")]
    args: Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeMemoryInput {
    #[serde(default)]
    collect_garbage: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActionReferenceInput {
    #[schemars(length(min = 1, max = 128))]
    action: String,
}

fn empty_object() -> Value {
    json!({})
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
enum ControlOperation {
    Activate,
    Fill,
    Check,
    Focus,
    Scroll,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControlInput {
    operation: ControlOperation,
    #[schemars(length(min = 1, max = 256))]
    control_ref: String,
    document_epoch: u64,
    #[serde(default)]
    #[schemars(length(max = 8192))]
    value: Option<String>,
    #[serde(default)]
    checked: Option<bool>,
    #[serde(default)]
    #[schemars(range(min = -100000, max = 100000))]
    x: Option<f64>,
    #[serde(default)]
    #[schemars(range(min = -100000, max = 100000))]
    y: Option<f64>,
    #[serde(default)]
    relative: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitForRenderInput {
    document_epoch: u64,
    #[schemars(range(min = 1))]
    after_sequence: u64,
    #[serde(default)]
    #[schemars(range(max = 4000))]
    timeout_ms: Option<u16>,
    #[schemars(length(min = 1, max = 128))]
    wait_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReloadInput {
    document_epoch: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitForControlInput {
    document_epoch: u64,
    #[serde(default)]
    #[schemars(length(min = 1, max = 128))]
    id: Option<String>,
    #[serde(default, rename = "ref")]
    #[schemars(rename = "ref", length(min = 1, max = 256))]
    reference: Option<String>,
    field: String,
    #[serde(default)]
    equals: Option<Value>,
    #[serde(default)]
    #[schemars(length(min = 1, max = 256))]
    contains: Option<String>,
    #[serde(default)]
    #[schemars(range(max = 4000))]
    timeout_ms: Option<u16>,
    #[schemars(length(min = 1, max = 128))]
    wait_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelWaitInput {
    #[schemars(length(min = 1, max = 128))]
    wait_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageObserveInput {
    #[serde(default)]
    #[schemars(length(max = 256))]
    root_ref: Option<String>,
    #[serde(default)]
    #[schemars(length(max = 256))]
    after_ref: Option<String>,
    #[serde(default)]
    document_epoch: Option<u64>,
    #[serde(default = "default_observe_page_size")]
    #[schemars(range(min = 1, max = 64))]
    limit: u8,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageChangesInput {
    document_epoch: u64,
    #[serde(default)]
    #[schemars(length(max = 256))]
    cursor: Option<String>,
    #[serde(default = "default_observe_page_size")]
    #[schemars(range(min = 1, max = 64))]
    limit: u8,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitForPageChangesInput {
    document_epoch: u64,
    #[schemars(length(min = 1, max = 256))]
    cursor: String,
    #[serde(default = "default_observe_page_size")]
    #[schemars(range(min = 1, max = 64))]
    limit: u8,
    #[serde(default)]
    #[schemars(range(max = 4000))]
    timeout_ms: Option<u16>,
    #[schemars(length(min = 1, max = 128))]
    wait_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
enum OperationCommand {
    Status,
    Wait,
    Cancel,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationInput {
    operation: OperationCommand,
    #[schemars(length(min = 1, max = 128))]
    operation_id: String,
    #[serde(default)]
    after_revision: Option<u64>,
    #[serde(default)]
    #[schemars(range(max = 4000))]
    timeout_ms: Option<u16>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChangesInput {
    #[serde(default = "default_change_scope")]
    scope: String,
    #[serde(default)]
    #[schemars(length(max = 256))]
    cursor: Option<String>,
    #[serde(default = "default_change_limit")]
    #[schemars(range(min = 1, max = 128))]
    limit: u16,
    #[serde(default)]
    #[schemars(range(max = 1000))]
    wait_ms: u16,
}

fn default_change_scope() -> String {
    "application".into()
}

fn default_change_limit() -> u16 {
    64
}

fn default_observe_page_size() -> u8 {
    32
}

fn app_capabilities() -> Vec<&'static str> {
    #[cfg(feature = "software-renderer")]
    {
        vec![
            "semantic_controls",
            "page_observation",
            "page_changes",
            "page_change_wait",
            "screenshot",
            "registered_actions",
            "diagnostics",
            "quickjs_memory_usage",
        ]
    }
    #[cfg(not(feature = "software-renderer"))]
    {
        vec![
            "semantic_controls",
            "page_observation",
            "page_changes",
            "page_change_wait",
            "registered_actions",
            "diagnostics",
            "quickjs_memory_usage",
        ]
    }
}

#[derive(Clone)]
pub struct LapuiMcpServer {
    reload: ReloadHandle,
    actions: ActionRegistry,
}

impl LapuiMcpServer {
    pub fn new(reload: ReloadHandle, actions: ActionRegistry) -> Self {
        Self { reload, actions }
    }

    fn controller(&self) -> DocumentController {
        self.reload.endpoint().controller
    }
}

fn mcp_result(value: impl serde::Serialize) -> CallToolResult {
    let mut value = serde_json::to_value(value).unwrap_or_else(
        |error| json!({"ok":false,"code":"serialization_error","message":error.to_string()}),
    );
    if value.to_string().len() > MAX_TOOL_RESULT_BYTES {
        value = json!({"ok":false,"code":"response_too_large","message":"structured tool result exceeds 24 KiB; use bounded page observation or action pagination"});
    }
    if value.get("ok") == Some(&Value::Bool(false)) {
        CallToolResult::structured_error(value)
    } else {
        CallToolResult::structured(value)
    }
}

#[tool_router]
impl LapuiMcpServer {
    #[tool(output_schema = schema_for_type::<AppDescribeOutput>(), description = "Describe the connected Lapui application and its semantic capabilities.")]
    fn app_describe(&self) -> CallToolResult {
        mcp_result(json!({
            "protocolVersion":1,
            "runtime":"lapui",
            "capabilities":app_capabilities(),
            "scriptEvaluation":false
        }))
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "List visible semantic controls with stable references and current values. Sensitive values are redacted by the runtime."
    )]
    async fn page_controls(&self) -> CallToolResult {
        let controller = self.controller();
        match tokio::task::spawn_blocking(move || {
            controller.request(json!({"method":"controls"}), Duration::from_secs(5))
        })
        .await
        {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<PageObserveOutput>(),
        description = "Observe a bounded preorder page of rendered HTML elements with parent refs, roles, names, visible text, viewport bounds, and semantic control state. Use nextAfter and documentEpoch to continue; restart observation after page changes."
    )]
    async fn page_observe(
        &self,
        Parameters(input): Parameters<PageObserveInput>,
    ) -> CallToolResult {
        let mut request = json!({"method":"pageSnapshot","limit":input.limit});
        if let Some(root_ref) = input.root_ref {
            request["rootRef"] = json!(root_ref);
        }
        if let Some(after_ref) = input.after_ref {
            request["afterRef"] = json!(after_ref);
        }
        if let Some(document_epoch) = input.document_epoch {
            request["documentEpoch"] = json!(document_epoch);
        }
        let controller = self.controller();
        match tokio::task::spawn_blocking(move || {
            controller.request(request, Duration::from_secs(5))
        })
        .await
        {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<PageChangesOutput>(),
        description = "Read a bounded, value-free journal of DOM attribute, text, child-list, form value/checked property, and input/change events. Continue with the returned cursor and current documentEpoch; on resyncRequired, take a fresh page_observe snapshot before continuing. Native Rust DOM mutations are not included."
    )]
    async fn page_changes(
        &self,
        Parameters(input): Parameters<PageChangesInput>,
    ) -> CallToolResult {
        let mut request = json!({"method":"pageChanges","documentEpoch":input.document_epoch,"limit":input.limit});
        if let Some(cursor) = input.cursor {
            request["cursor"] = json!(cursor);
        }
        let controller = self.controller();
        match tokio::task::spawn_blocking(move || {
            controller.request(request, Duration::from_secs(5))
        })
        .await
        {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Wait from a page_changes cursor for the next bounded batch of value-free DOM/control changes, a resync signal, or timeout. A reload returns stale_document. This is a semantic journal wait and does not confirm layout, rendering, or physical presentation."
    )]
    async fn page_wait_for_changes(
        &self,
        Parameters(input): Parameters<WaitForPageChangesInput>,
    ) -> CallToolResult {
        let guard = match WaitGuard::register(&input.wait_id) {
            Ok(guard) => guard,
            Err(error) => {
                return mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
        };
        let request = json!({"documentEpoch":input.document_epoch,"cursor":input.cursor,
            "limit":input.limit,"timeoutMs":input.timeout_ms,"waitId":input.wait_id});
        let epoch = input.document_epoch;
        let reload = self.reload.clone();
        let token = guard.token();
        let notifier_token = token.clone();
        match tokio::task::spawn_blocking(move || {
            let _guard = guard;
            wait_for_page_changes_with(&request, |cursor, limit, timeout| {
                let controller = reload.endpoint().controller;
                let notifier = controller.page_change_notifier();
                notifier_token.bind_page_change_notifier(notifier.clone());
                let generation = notifier.generation();
                let page = controller.request(
                    json!({"method":"pageChanges","documentEpoch":epoch,"cursor":cursor,"limit":limit}),
                    timeout,
                )?;
                Ok((page, notifier, generation))
            }, || token.is_cancelled())
        })
        .await
        {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Wait until one current semantic control matches a bounded state condition. Provide exactly one of id or ref, and exactly one of equals or contains. This observes the semantic control snapshot; it does not confirm rendering or physical presentation."
    )]
    async fn page_wait_for_control(
        &self,
        Parameters(input): Parameters<WaitForControlInput>,
    ) -> CallToolResult {
        let guard = match WaitGuard::register(&input.wait_id) {
            Ok(guard) => guard,
            Err(error) => {
                return mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
        };
        let mut request = json!({"method":"waitForControl","documentEpoch":input.document_epoch,
            "field":input.field,"waitId":input.wait_id});
        if let Some(id) = input.id {
            request["id"] = json!(id);
        }
        if let Some(reference) = input.reference {
            request["ref"] = json!(reference);
        }
        if let Some(equals) = input.equals {
            request["equals"] = equals;
        }
        if let Some(contains) = input.contains {
            request["contains"] = json!(contains);
        }
        if let Some(timeout_ms) = input.timeout_ms {
            request["timeoutMs"] = json!(timeout_ms);
        }
        let controller = self.controller();
        let token = guard.token();
        match tokio::task::spawn_blocking(move || {
            let _guard = guard;
            wait_for_control_with(
                &request,
                |timeout| controller.request(json!({"method":"controls"}), timeout),
                || token.is_cancelled(),
            )
        })
        .await
        {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Wait for the renderer to return from the frame causally linked to a page_control mutation or a page_changes record. Requires --debug-trace and a debugTraceSequence returned by the mutation or journal record; this does not confirm physical screen presentation."
    )]
    async fn page_wait_for_render(
        &self,
        Parameters(input): Parameters<WaitForRenderInput>,
    ) -> CallToolResult {
        let guard = match WaitGuard::register(&input.wait_id) {
            Ok(guard) => guard,
            Err(error) => {
                return mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
        };
        let mut request = json!({"method":"waitForRender","documentEpoch":input.document_epoch,
            "afterSequence":input.after_sequence,"waitId":input.wait_id});
        if let Some(timeout_ms) = input.timeout_ms {
            request["timeoutMs"] = json!(timeout_ms);
        }
        let controller = self.controller();
        let token = guard.token();
        match tokio::task::spawn_blocking(move || {
            let _guard = guard;
            wait_for_render_with(
                &request,
                |command, timeout| controller.request(command, timeout),
                || token.is_cancelled(),
            )
        })
        .await
        {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Request cancellation of an active page_wait_for_changes, page_wait_for_control, or page_wait_for_render call by its waitId. The active wait returns wait_cancelled; unknown or completed identifiers return found=false."
    )]
    async fn page_cancel_wait(
        &self,
        Parameters(input): Parameters<CancelWaitInput>,
    ) -> CallToolResult {
        if input.wait_id.is_empty() || input.wait_id.len() > 128 {
            return mcp_result(
                json!({"ok":false,"code":"invalid_request","message":"waitId must be 1..128 bytes"}),
            );
        }
        let found = WaitGuard::cancel(&input.wait_id);
        mcp_result(
            json!({"found":found,"status":if found{"cancel_requested"}else{"not_found"},"waitId":input.wait_id}),
        )
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Reload the current trusted local document source. All prior page references become stale and must be observed again."
    )]
    async fn page_reload(&self, Parameters(input): Parameters<ReloadInput>) -> CallToolResult {
        let reload = self.reload.clone();
        match tokio::task::spawn_blocking(move || {
            reload.reload(
                json!({"method":"reload","documentEpoch":input.document_epoch}),
                Duration::from_secs(10),
            )
        })
        .await
        {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Capture the current viewport as a bounded PNG image. This is a CPU-rendered document snapshot and does not confirm native screen presentation."
    )]
    async fn page_screenshot(&self) -> CallToolResult {
        let controller = self.controller();
        match tokio::task::spawn_blocking(move || {
            controller.request(json!({"method":"screenshot"}), Duration::from_secs(15))
        })
        .await
        {
            Ok(Ok(mut value)) => {
                let Some(encoded) = value
                    .get("pngBase64")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    return mcp_result(json!({
                        "ok":false,
                        "code":"invalid_screenshot",
                        "message":"runtime returned no PNG image"
                    }));
                };
                if let Some(object) = value.as_object_mut() {
                    object.remove("pngBase64");
                }
                let mut result =
                    CallToolResult::success(vec![ContentBlock::image(encoded, "image/png")]);
                result.structured_content = Some(value);
                result
            }
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Read runtime diagnostics, including script errors and network stream status."
    )]
    async fn page_diagnostics(&self) -> CallToolResult {
        let controller = self.controller();
        match tokio::task::spawn_blocking(move || {
            controller.request(json!({"method":"diagnostics"}), Duration::from_secs(5))
        })
        .await
        {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Read QuickJS runtime allocation and heap counters. Set collectGarbage only when a diagnostic cycle collection is intended; this runs on the UI thread. These counters exclude Rust, DOM, renderer, GPU, and process allocator-retained memory."
    )]
    async fn runtime_memory_usage(
        &self,
        Parameters(input): Parameters<RuntimeMemoryInput>,
    ) -> CallToolResult {
        let controller = self.controller();
        match tokio::task::spawn_blocking(move || {
            controller.request(
                json!({"method":"runtime.memoryUsage","collectGarbage":input.collect_garbage}),
                Duration::from_secs(5),
            )
        })
        .await
        {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Activate, fill, check, or focus a semantic control, or scroll a rendered element. Use the current documentEpoch and canonical ref from page_observe/page_controls; stale references are rejected."
    )]
    async fn page_control(&self, Parameters(input): Parameters<ControlInput>) -> CallToolResult {
        if (matches!(input.operation, ControlOperation::Fill) != input.value.is_some())
            || (matches!(input.operation, ControlOperation::Check) != input.checked.is_some())
            || (matches!(input.operation, ControlOperation::Scroll)
                != (input.x.is_some() || input.y.is_some()))
            || (!matches!(input.operation, ControlOperation::Scroll)
                && (input.x.is_some() || input.y.is_some() || input.relative.is_some()))
        {
            return mcp_result(
                json!({"ok":false,"code":"invalid_request","message":"fill requires value, check requires checked, scroll requires x or y, and other operations accept none of these fields"}),
            );
        }
        let method = match input.operation {
            ControlOperation::Activate => "activate",
            ControlOperation::Fill => "fill",
            ControlOperation::Check => "check",
            ControlOperation::Focus => "focus",
            ControlOperation::Scroll => "scroll",
        };
        let mut request =
            json!({"method":method,"ref":input.control_ref,"documentEpoch":input.document_epoch});
        if let Some(value) = input.value {
            request["value"] = json!(value);
        }
        if let Some(checked) = input.checked {
            request["checked"] = json!(checked);
        }
        if let Some(x) = input.x {
            request["x"] = json!(x);
        }
        if let Some(y) = input.y {
            request["y"] = json!(y);
        }
        if let Some(relative) = input.relative {
            request["relative"] = json!(relative);
        }
        let controller = self.controller();
        match tokio::task::spawn_blocking(move || {
            controller.request(request, Duration::from_secs(5))
        })
        .await
        {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Discover registered business actions. Results are paged and restricted by the runtime's action catalog."
    )]
    fn actions_list(&self, Parameters(input): Parameters<ActionListInput>) -> CallToolResult {
        let mut request =
            json!({"method":"actions.list","prefix":input.prefix,"limit":input.limit});
        if let Some(scope) = input.scope {
            request["scope"] = json!(scope);
        }
        if let Some(cursor) = input.cursor {
            request["cursor"] = json!(cursor);
        }
        mcp_result(
            self.actions
                .action_catalog_request(&request)
                .unwrap_or_else(
                    |error| json!({"ok":false,"code":error.code,"message":error.message}),
                ),
        )
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Read the full description and input/output schemas for a registered action before invoking it."
    )]
    fn actions_describe(
        &self,
        Parameters(input): Parameters<ActionReferenceInput>,
    ) -> CallToolResult {
        mcp_result(
            self.actions
                .action_catalog_request(&json!({"method":"actions.describe","action":input.action}))
                .unwrap_or_else(
                    |error| json!({"ok":false,"code":error.code,"message":error.message}),
                ),
        )
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Invoke one previously registered Lapui business action by its exact action id. Provide a stable requestId for retry deduplication and, for writes based on an observed state, expectedVersion to reject stale writes. Returns only the committed version and action result, not the whole application state."
    )]
    async fn action_invoke(&self, Parameters(input): Parameters<ActionInput>) -> CallToolResult {
        if input.args.to_string().len() > MAX_ACTION_ARGUMENT_BYTES {
            return mcp_result(
                json!({"ok":false,"code":"invalid_arguments","message":"arguments exceed 64 KiB"}),
            );
        }
        let request_id = input.request_id.clone();
        match self.actions.invoke_checked(
            &input.action,
            &input.args,
            Some(&input.request_id),
            input.expected_version,
        ) {
            Ok(observation) => {
                let result = json!({"version":observation.version,"requestId":request_id,"result":observation.result});
                if result.to_string().len() > MAX_TOOL_RESULT_BYTES {
                    mcp_result(
                        json!({"ok":false,"code":"outcome_unknown","requestId":request_id,"message":"action completed or was accepted but its result exceeds the tool budget; inspect application state or the operation using this requestId, and do not retry with a new requestId"}),
                    )
                } else {
                    mcp_result(result)
                }
            }
            Err(error) => mcp_result(json!({"ok":false,"code":error.code,"message":error.message})),
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Read, wait for a newer revision of, or request cancellation of a registered asynchronous operation. A wait blocks only for the bounded timeout."
    )]
    async fn operation(&self, Parameters(input): Parameters<OperationInput>) -> CallToolResult {
        let operations = self.actions.operations();
        let result = tokio::task::spawn_blocking(move || {
            match input.operation {
                OperationCommand::Status if input.after_revision.is_none() && input.timeout_ms.is_none() => {
                    operations.get(&input.operation_id)
                }
                OperationCommand::Wait if input.after_revision.is_some() => {
                    let timeout = input.timeout_ms.unwrap_or(1000);
                    if timeout > 4000 {
                        return Err(crate::action::ActionError::new("invalid_request", "timeoutMs must be 0..4000"));
                    }
                    operations.wait(
                        &input.operation_id,
                        input.after_revision.unwrap(),
                        Duration::from_millis(u64::from(timeout)),
                    )
                }
                OperationCommand::Cancel if input.after_revision.is_none() && input.timeout_ms.is_none() => {
                    operations.cancel(&input.operation_id)
                }
                _ => Err(crate::action::ActionError::new(
                    "invalid_request",
                    "status/cancel accept only operationId; wait requires afterRevision and optional timeoutMs",
                )),
            }
            .map(|snapshot| json!(snapshot))
        })
        .await;
        match result {
            Ok(Ok(value)) => mcp_result(value),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }

    #[tool(
        output_schema = schema_for_type::<StructuredToolOutput>(),
        description = "Subscribe to bounded application, state, action, operation, or host changes. Omit cursor for a baseline; continue with the returned cursor and restart from the supplied baseline when resyncRequired is true."
    )]
    async fn changes(&self, Parameters(input): Parameters<ChangesInput>) -> CallToolResult {
        if !matches!(
            input.scope.as_str(),
            "application" | "state" | "actions" | "operations" | "host"
        ) {
            return mcp_result(
                json!({"ok":false,"code":"invalid_request","message":"scope must be application, state, actions, operations, or host"}),
            );
        }
        let request = serde_json::from_value(json!({
            "scope":input.scope,"cursor":input.cursor,"limit":input.limit,"waitMs":input.wait_ms
        }));
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                return mcp_result(
                    json!({"ok":false,"code":"invalid_request","message":error.to_string()}),
                )
            }
        };
        let actions = self.actions.clone();
        match tokio::task::spawn_blocking(move || actions.subscribe_changes(request)).await {
            Ok(Ok(page)) => mcp_result(json!(page)),
            Ok(Err(error)) => {
                mcp_result(json!({"ok":false,"code":error.code,"message":error.message}))
            }
            Err(error) => {
                mcp_result(json!({"ok":false,"code":"adapter_failure","message":error.to_string()}))
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for LapuiMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("lapui", env!("CARGO_PKG_VERSION")))
            .with_instructions("Operate a running Lapui local UI through bounded semantic tools. Arbitrary JavaScript execution is unavailable.")
    }
}

/// Serve on inherited stdin/stdout. Call from a dedicated thread; stdout is
/// reserved exclusively for MCP messages.
pub fn serve_stdio(reload: ReloadHandle, actions: ActionRegistry) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        LapuiMcpServer::new(reload, actions)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|error| error.to_string())?
            .waiting()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    static WAIT_GUARD_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn tool_catalog_is_small_and_does_not_expose_script_evaluation() {
        let tools = LapuiMcpServer::tool_router().list_all();
        let names: Vec<_> = tools.iter().map(|tool| tool.name.to_string()).collect();
        assert_eq!(
            names,
            [
                "action_invoke",
                "actions_describe",
                "actions_list",
                "app_describe",
                "changes",
                "operation",
                "page_cancel_wait",
                "page_changes",
                "page_control",
                "page_controls",
                "page_diagnostics",
                "page_observe",
                "page_reload",
                "page_screenshot",
                "page_wait_for_changes",
                "page_wait_for_control",
                "page_wait_for_render",
                "runtime_memory_usage"
            ]
        );
        assert!(!names
            .iter()
            .any(|name| name.contains("script") || name.contains("eval")));
        for tool in &tools {
            let schema = tool.output_schema.as_ref().unwrap_or_else(|| {
                panic!("tool {} is missing its structured output schema", tool.name)
            });
            assert_eq!(
                schema.get("type").and_then(Value::as_str),
                Some("object"),
                "tool {} output schema",
                tool.name
            );
            if tool.name == "app_describe" {
                let properties = schema.get("properties").and_then(Value::as_object).unwrap();
                for (field, expected_type) in [
                    ("protocolVersion", "integer"),
                    ("runtime", "string"),
                    ("capabilities", "array"),
                    ("scriptEvaluation", "boolean"),
                ] {
                    assert_eq!(
                        properties
                            .get(field)
                            .and_then(|value| value.get("type"))
                            .and_then(Value::as_str),
                        Some(expected_type),
                        "app_describe output property {field}"
                    );
                }
            } else if tool.name == "page_observe" {
                let properties = schema.get("properties").and_then(Value::as_object).unwrap();
                for (field, expected_type) in [
                    ("documentEpoch", "integer"),
                    ("items", "array"),
                    ("truncated", "boolean"),
                    ("scanLimitReached", "boolean"),
                    ("cursorConsistency", "string"),
                ] {
                    assert_eq!(
                        properties
                            .get(field)
                            .and_then(|value| value.get("type"))
                            .and_then(Value::as_str),
                        Some(expected_type),
                        "page_observe output property {field}"
                    );
                }
                for field in ["rootRef", "nextAfter"] {
                    assert!(
                        properties.contains_key(field),
                        "page_observe output missing {field}"
                    );
                }
            } else if tool.name == "page_changes" {
                let properties = schema.get("properties").and_then(Value::as_object).unwrap();
                for (field, expected_type) in [
                    ("latestSequence", "integer"),
                    ("nextSequence", "integer"),
                    ("oldestSequence", "integer"),
                    ("hasMore", "boolean"),
                    ("resyncRequired", "boolean"),
                    ("sequenceExhausted", "boolean"),
                    ("records", "array"),
                    ("documentEpoch", "integer"),
                    ("cursor", "string"),
                ] {
                    assert_eq!(
                        properties
                            .get(field)
                            .and_then(|value| value.get("type"))
                            .and_then(Value::as_str),
                        Some(expected_type),
                        "page_changes output property {field}"
                    );
                }
            } else {
                assert!(
                    schema.contains_key("additionalProperties"),
                    "tool {} output schema must leave dynamic response fields open",
                    tool.name
                );
            }
        }
        let invoke = tools
            .iter()
            .find(|tool| tool.name == "action_invoke")
            .unwrap();
        let schema = serde_json::to_value(&invoke.input_schema).unwrap();
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("requestId")));
        assert!(schema["properties"]["expectedVersion"]["type"]
            .as_array()
            .unwrap()
            .contains(&json!("integer")));
        let memory_tool = tools
            .iter()
            .find(|tool| tool.name == "runtime_memory_usage")
            .unwrap();
        let memory_schema = serde_json::to_value(&memory_tool.input_schema).unwrap();
        assert_eq!(
            memory_schema["properties"]["collectGarbage"]["type"],
            "boolean"
        );
        let page_changes = tools
            .iter()
            .find(|tool| tool.name == "page_changes")
            .unwrap();
        let page_changes_schema = serde_json::to_value(&page_changes.input_schema).unwrap();
        assert!(page_changes_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("documentEpoch")));
        let page_wait = tools
            .iter()
            .find(|tool| tool.name == "page_wait_for_changes")
            .unwrap();
        let wait_schema = serde_json::to_value(&page_wait.input_schema).unwrap();
        for required in ["documentEpoch", "cursor", "waitId"] {
            assert!(wait_schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!(required)));
        }
        let cancel_wait = tools
            .iter()
            .find(|tool| tool.name == "page_cancel_wait")
            .unwrap();
        let cancel_schema = serde_json::to_value(&cancel_wait.input_schema).unwrap();
        assert!(cancel_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("waitId")));
    }

    #[test]
    fn cancel_wait_wakes_page_change_waiter_and_releases_wait_id() {
        let _serial = WAIT_GUARD_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let guard = WaitGuard::register("page-cancel-test-unique").unwrap();
        let token = guard.token();
        let notifier = crate::control::PageChangeNotifier::default();
        token.bind_page_change_notifier(notifier.clone());
        let generation = notifier.generation();
        let waiter =
            std::thread::spawn(move || notifier.wait_after(generation, Duration::from_secs(2)));
        assert!(WaitGuard::cancel("page-cancel-test-unique"));
        assert!(waiter.join().unwrap());
        assert!(token.is_cancelled());
        drop(guard);
        assert!(!WaitGuard::cancel("page-cancel-test-unique"));
    }

    #[test]
    fn active_wait_budget_rejects_fifth_wait_and_releases_slot_on_drop() {
        let _serial = WAIT_GUARD_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut guards: Vec<_> = (0..MAX_ACTIVE_WAITS)
            .map(|index| WaitGuard::register(&format!("page-wait-capacity-{index}")).unwrap())
            .collect();

        let rejected = match WaitGuard::register("page-wait-capacity-overflow") {
            Ok(guard) => {
                drop(guard);
                panic!("a fifth active wait must be rejected");
            }
            Err(error) => error,
        };
        assert_eq!(rejected.code, "wait_busy");
        assert_eq!(
            ACTIVE_WAITS.get().unwrap().lock().unwrap().len(),
            MAX_ACTIVE_WAITS
        );

        drop(guards.pop().unwrap());
        assert_eq!(
            ACTIVE_WAITS.get().unwrap().lock().unwrap().len(),
            MAX_ACTIVE_WAITS - 1
        );
        let replacement = WaitGuard::register("page-wait-capacity-replacement").unwrap();
        assert_eq!(
            ACTIVE_WAITS.get().unwrap().lock().unwrap().len(),
            MAX_ACTIVE_WAITS
        );

        drop(replacement);
        drop(guards);
        assert_eq!(ACTIVE_WAITS.get().unwrap().lock().unwrap().len(), 0);
    }

    #[test]
    fn describe_reports_screenshot_only_when_the_renderer_is_compiled() {
        assert_eq!(
            app_capabilities().contains(&"screenshot"),
            cfg!(feature = "software-renderer")
        );
    }

    #[test]
    fn tool_results_include_machine_readable_json_and_mark_failures() {
        let ok = mcp_result(json!({"value":7}));
        assert_eq!(ok.structured_content, Some(json!({"value":7})));
        assert_eq!(ok.is_error, Some(false));
        let error = mcp_result(json!({"ok":false,"code":"stale_document"}));
        assert_eq!(error.is_error, Some(true));
        assert_eq!(error.structured_content.unwrap()["code"], "stale_document");
        let too_large = mcp_result(json!({"payload":"x".repeat(MAX_TOOL_RESULT_BYTES + 1)}));
        assert_eq!(too_large.is_error, Some(true));
        assert_eq!(
            too_large.structured_content.unwrap()["code"],
            "response_too_large"
        );
    }
}
