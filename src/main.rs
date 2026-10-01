use anyrender_vello::VelloWindowRenderer;
mod connection_pool;
use blitz::shell::{create_default_event_loop, BlitzShellProxy, WindowConfig};
use connection_pool::ConnectionPool;
use lapui::action::{ActionError, ActionRegistry};
use lapui::control::DocumentController;
use lapui::control_wait::wait_for_control_with;
use lapui::reload::{DocumentSource, ReloadDocument, ReloadHandle};
#[cfg(test)]
use lapui::render_wait::consume_render_trace_page;
use lapui::render_wait::wait_for_render_with;
use lapui::runtime::LapuiDocument;
use lapui::shell::LapuiApplication;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Mutex, OnceLock};
use std::time::Duration;

const MAX_CONTROL_REQUEST_BYTES: usize = 64 * 1024;
const MAX_CONTROL_WAIT: Duration = Duration::from_secs(4);
const MAX_ACTIVE_WAITS: usize = 4;

#[derive(Default)]
struct WaitRegistry {
    active: Mutex<HashMap<String, std::sync::Arc<AtomicBool>>>,
}

struct ActiveWait<'a> {
    registry: &'a WaitRegistry,
    id: String,
}

impl Drop for ActiveWait<'_> {
    fn drop(&mut self) {
        self.registry.active.lock().unwrap().remove(&self.id);
    }
}

impl WaitRegistry {
    fn register(
        &self,
        id: &str,
    ) -> Result<(std::sync::Arc<AtomicBool>, ActiveWait<'_>), ActionError> {
        if id.is_empty() || id.len() > 128 {
            return Err(ActionError::new(
                "invalid_request",
                "waitId must be 1..128 bytes",
            ));
        }
        let mut active = self.active.lock().unwrap();
        if active.len() >= MAX_ACTIVE_WAITS {
            return Err(ActionError::new(
                "wait_busy",
                "maximum active waits reached",
            ));
        }
        if active.contains_key(id) {
            return Err(ActionError::new(
                "wait_conflict",
                "waitId is already active",
            ));
        }
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        active.insert(id.to_owned(), cancelled.clone());
        Ok((
            cancelled,
            ActiveWait {
                registry: self,
                id: id.to_owned(),
            },
        ))
    }

    fn cancel(&self, id: &str) -> bool {
        if let Some(cancelled) = self.active.lock().unwrap().get(id) {
            cancelled.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }
}

fn wait_registry() -> &'static WaitRegistry {
    static REGISTRY: OnceLock<WaitRegistry> = OnceLock::new();
    REGISTRY.get_or_init(WaitRegistry::default)
}

fn cancel_wait(request: &Value) -> Result<Value, ActionError> {
    let fields = request
        .as_object()
        .ok_or_else(|| ActionError::new("invalid_request", "cancelWait requires an object"))?;
    if fields
        .keys()
        .any(|key| !["method", "waitId"].contains(&key.as_str()))
    {
        return Err(ActionError::new(
            "invalid_request",
            "unknown cancelWait field",
        ));
    }
    let id = request
        .get("waitId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .ok_or_else(|| ActionError::new("invalid_request", "waitId must be 1..128 bytes"))?;
    if wait_registry().cancel(id) {
        Ok(json!({"status":"cancel_requested","waitId":id}))
    } else {
        Err(ActionError::new("unknown_wait", "waitId is not active"))
    }
}

fn wait_for_control(
    controller: &DocumentController,
    request: &Value,
) -> Result<Value, ActionError> {
    let wait_id = request
        .get("waitId")
        .and_then(Value::as_str)
        .ok_or_else(|| ActionError::new("invalid_request", "waitId is required"))?;
    let (cancelled, _active) = wait_registry().register(wait_id)?;
    wait_for_control_with(
        request,
        |timeout| controller.request(json!({"method":"controls"}), timeout),
        || cancelled.load(Ordering::Acquire),
    )
}

fn wait_for_render(controller: &DocumentController, request: &Value) -> Result<Value, ActionError> {
    let wait_id = request
        .get("waitId")
        .and_then(Value::as_str)
        .ok_or_else(|| ActionError::new("invalid_request", "waitId is required"))?;
    let (cancelled, _active) = wait_registry().register(wait_id)?;
    wait_for_render_with(
        request,
        |command, timeout| controller.request(command, timeout),
        || cancelled.load(Ordering::Acquire),
    )
}

fn respond(
    stream: &mut TcpStream,
    actions: &ActionRegistry,
    notify: &Sender<Result<Value, String>>,
    controller: &DocumentController,
    reload: Option<&ReloadHandle>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut request = String::new();
    BufReader::new(
        stream
            .try_clone()?
            .take(MAX_CONTROL_REQUEST_BYTES as u64 + 1),
    )
    .read_line(&mut request)?;
    let result: Result<Value, ActionError> = (|| {
        if request.len() > MAX_CONTROL_REQUEST_BYTES {
            return Err(ActionError {
                code: "invalid_request".into(),
                message: "request exceeds 64 KiB".into(),
            });
        }
        let request: Value = serde_json::from_str(&request).map_err(|e| ActionError {
            code: "invalid_request".into(),
            message: e.to_string(),
        })?;
        match request.get("method").and_then(Value::as_str) {
            Some("describe") => {
                let mut methods = vec![
                    "describe",
                    "observe",
                    "invoke",
                    "trace",
                    "operation",
                    "cancelOperation",
                    "cancelWait",
                    "controls",
                    "waitForControl",
                    "waitForRender",
                    "diagnostics",
                    "networkStatus",
                    "activate",
                    "fill",
                    "check",
                    "focus",
                    "changes.subscribe",
                    "actions.list",
                    "actions.describe",
                    "actions.check",
                    "debugTrace.configure",
                    "debugTrace.read",
                ];
                if reload.is_some() {
                    methods.push("reload");
                    methods.push("reloadStatus");
                }
                Ok(json!({
                    "protocolVersion": 1,
                    "transport": "newline-delimited-json-over-tcp",
                    "methods": methods,
                    "capabilities": {
                        "requestIdDeduplication": true,
                        "expectedVersion": true,
                        "asynchronousOperations": true,
                        "operationCancellation": true,
                        "operationWaiting": true,
                        "controlWaitMaximumMs": MAX_CONTROL_WAIT.as_millis(),
                        "subscriptions": true,
                        "controlChangeStream": false,
                        "frameEvents": false,
                        "debugTrace": true,
                        "renderingTimingTrace": true,
                        "physicalPresentationAck": false,
                        "durableRecovery": false,
                        "controlSnapshot": true,
                        "controlConditionWait": true,
                        "waitCancellation": true,
                        "activeWaitLimit": MAX_ACTIVE_WAITS,
                        "controlWaitBoundary": "semantic_snapshot_only",
                        "renderWait": true,
                        "renderWaitBoundary": "renderer_returned_only",
                        "controlMutations": true,
                        "scriptDiagnostics": true,
                        "networkStreamStatus": true,
                        "scriptExecutionLimits": true,
                        "registeredActions": true,
                        "scopedActions": true,
                        "actionAvailability": true,
                        "actionDiscoveryPages": true,
                        "actionTrace": true,
                        "documentEpoch": true,
                    "documentReload": reload.is_some(),
                    "fileWatching": reload.is_some_and(|handle| handle.status()["watching"] == true)
                    },
                    "requestIdMethods": ["invoke"],
                    "expectedVersionMethods": ["invoke"],
                    "controlMutationPreconditions": ["documentEpoch"],
                    "scriptExecutionLimits": LapuiDocument::script_limits(),
                    "debugTraceLimits": LapuiDocument::debug_trace_limits(),
                    "debugTraceMethodSchemas":debug_trace_schemas(),
                    "networkStreamLimits": LapuiDocument::stream_limits(),
                    "changeSubscriptionLimits": ActionRegistry::change_limits(),
                    "actionCatalogLimits": ActionRegistry::action_limits(),
                    "actionCatalogSchemas": {"actions.list":{"type":"object","additionalProperties":false,"required":["method"],"properties":{
                        "method":{"const":"actions.list"},"prefix":{"type":"string","maxLength":128,"default":""},"scope":{"type":["string","null"],"maxLength":128},
                        "cursor":{"type":["string","null"],"maxLength":1024},"limit":{"type":"integer","minimum":1,"maximum":64,"default":32}}},
                        "actions.describe":{"type":"object","additionalProperties":false,"required":["method","action"],"properties":{"method":{"const":"actions.describe"},"action":{"type":"string","minLength":1,"maxLength":128}}},
                        "actions.check":{"type":"object","additionalProperties":false,"required":["method","action"],"properties":{"method":{"const":"actions.check"},"action":{"type":"string","minLength":1,"maxLength":128},"args":{"description":"Validated against the discovered action input schema; defaults to an empty object"}}}},
                    "changeSubscriptionSchema": {"type":"object","required":["method"],"additionalProperties":false,"properties":{
                        "method":{"const":"changes.subscribe"},"scope":{"enum":["application","state","actions","operations","host"],"default":"application"},
                        "cursor":{"type":["string","null"],"maxLength":256},"limit":{"type":"integer","minimum":1,"maximum":128,"default":64},
                        "waitMs":{"type":"integer","minimum":0,"maximum":1000,"default":1000}
                    }},
                    "controlConnectionLimits":{"workers":connection_pool::WORKERS,"queued":connection_pool::QUEUE},
                    "controlMethodSchemas": control_method_schemas(),
                    "waitForControlSchema": {"type":"object","additionalProperties":false,"required":["method","documentEpoch","field","waitId"],
                        "allOf":[
                            {"oneOf":[{"required":["id"],"not":{"required":["ref"]}},{"required":["ref"],"not":{"required":["id"]}}]},
                            {"oneOf":[{"required":["equals"],"not":{"required":["contains"]}},{"required":["contains"],"not":{"required":["equals"]}}]}],
                        "properties":{
                        "method":{"const":"waitForControl"},"documentEpoch":{"type":"integer","minimum":1},
                        "id":{"type":"string","minLength":1,"maxLength":128},"ref":{"type":"string","minLength":1,"maxLength":256},
                        "field":{"enum":["value","checked","focused","enabled","name","role"]},
                        "equals":{"type":["string","boolean"]},"contains":{"type":"string","minLength":1,"maxLength":256},
                        "waitId":{"type":"string","minLength":1,"maxLength":128},
                        "timeoutMs":{"type":"integer","minimum":0,"maximum":4000,"default":1000}}},
                    "waitForRenderSchema":{"type":"object","additionalProperties":false,"required":["method","documentEpoch","afterSequence","waitId"],"properties":{
                        "method":{"const":"waitForRender"},"documentEpoch":{"type":"integer","minimum":1},
                        "afterSequence":{"type":"integer","minimum":1,"description":"debugTraceSequence returned by a control mutation"},
                        "waitId":{"type":"string","minLength":1,"maxLength":128},
                        "timeoutMs":{"type":"integer","minimum":0,"maximum":4000,"default":1000}}},
                    "cancelWaitSchema":{"type":"object","additionalProperties":false,"required":["method","waitId"],"properties":{
                        "method":{"const":"cancelWait"},"waitId":{"type":"string","minLength":1,"maxLength":128}}},
                "reloadMethodSchema": {"type":"object","required":["method","documentEpoch"],"additionalProperties":false,"properties":{"method":{"const":"reload"},"documentEpoch":{"type":"integer","minimum":1}}}
                }))
            }
            Some("reload") => reload
                .ok_or_else(|| {
                    ActionError::new("unsupported_capability", "document reload is unavailable")
                })?
                .reload(request, Duration::from_secs(5)),
            Some("reloadStatus") => Ok(reload
                .ok_or_else(|| {
                    ActionError::new("unsupported_capability", "document reload is unavailable")
                })?
                .status()),
            Some("observe") => Ok(json!(actions.observe())),
            Some("actions.list" | "actions.describe" | "actions.check") => {
                actions.action_catalog_request(&request)
            }
            Some("changes.subscribe") => {
                let mut options = request.clone();
                options.as_object_mut().unwrap().remove("method");
                let request = serde_json::from_value(options)
                    .map_err(|error| ActionError::new("invalid_request", error.to_string()))?;
                actions.subscribe_changes(request).map(|page| json!(page))
            }
            Some("waitForControl") => wait_for_control(controller, &request),
            Some("waitForRender") => wait_for_render(controller, &request),
            Some("cancelWait") => cancel_wait(&request),
            Some(method @ ("operation" | "cancelOperation")) => {
                let id = request
                    .get("operationId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ActionError::new("invalid_request", "operationId must be a string")
                    })?;
                let after = request
                    .get("afterRevision")
                    .map(|value| {
                        value.as_u64().ok_or_else(|| {
                            ActionError::new(
                                "invalid_request",
                                "afterRevision must be a non-negative integer",
                            )
                        })
                    })
                    .transpose()?;
                let operations = actions.operations();
                if method == "cancelOperation" {
                    if after.is_some() {
                        return Err(ActionError::new(
                            "invalid_request",
                            "cancelOperation does not support afterRevision",
                        ));
                    }
                    operations.cancel(id).map(|snapshot| json!(snapshot))
                } else if let Some(after) = after {
                    operations
                        .wait(id, after, Duration::from_secs(1))
                        .map(|snapshot| json!(snapshot))
                } else {
                    operations.get(id).map(|snapshot| json!(snapshot))
                }
            }
            Some("trace") => {
                let after = request
                    .get("afterSequence")
                    .map(|value| {
                        value.as_u64().ok_or_else(|| {
                            ActionError::new(
                                "invalid_request",
                                "afterSequence must be a non-negative integer",
                            )
                        })
                    })
                    .transpose()?
                    .unwrap_or(0);
                Ok(json!(actions.trace(after)))
            }
            Some("invoke") => {
                let name = request
                    .get("action")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ActionError {
                        code: "invalid_request".into(),
                        message: "missing action".into(),
                    })?;
                let args = request.get("args").cloned().unwrap_or_else(|| json!({}));
                let request_id = match request.get("requestId") {
                    Some(Value::String(request_id)) => Some(request_id.as_str()),
                    Some(_) => {
                        return Err(ActionError {
                            code: "invalid_request".into(),
                            message: "requestId must be a string".into(),
                        })
                    }
                    None => None,
                };
                let expected_version = match request.get("expectedVersion") {
                    Some(Value::Number(version)) => version.as_u64(),
                    Some(_) => {
                        return Err(ActionError {
                            code: "invalid_request".into(),
                            message: "expectedVersion must be a non-negative integer".into(),
                        })
                    }
                    None => None,
                };
                if request.get("expectedVersion").is_some() && expected_version.is_none() {
                    return Err(ActionError {
                        code: "invalid_request".into(),
                        message: "expectedVersion must be a non-negative integer".into(),
                    });
                }
                let observation = actions
                    .invoke_checked(name, &args, request_id, expected_version)
                    .map(|result| json!(result));
                if let Ok(value) = &observation {
                    let _ = notify.send(Ok(value.clone()));
                }
                observation
            }
            Some(
                "controls"
                | "diagnostics"
                | "networkStatus"
                | "activate"
                | "fill"
                | "check"
                | "focus"
                | "debugTrace.read"
                | "debugTrace.configure",
            ) => controller.request(request, Duration::from_secs(5)),
            _ => Err(ActionError {
                code: "invalid_request".into(),
                message: "unsupported protocol method; call describe to discover methods".into(),
            }),
        }
    })();
    let response = match result {
        Ok(value) => json!({"ok":true,"observation":value}),
        Err(error) => json!({"ok":false,"error":error}),
    };
    writeln!(stream, "{response}")
}

fn debug_trace_schemas() -> Value {
    json!({"debugTrace.configure":{"type":"object","additionalProperties":false,"required":["method","documentEpoch","enabled"],
        "properties":{"method":{"const":"debugTrace.configure"},"documentEpoch":{"type":"integer","minimum":1},"enabled":{"type":"boolean"},"clear":{"type":"boolean","default":false}}},
        "debugTrace.read":{"type":"object","additionalProperties":false,"required":["method","documentEpoch"],
        "properties":{"method":{"const":"debugTrace.read"},"documentEpoch":{"type":"integer","minimum":1},"afterSequence":{"type":"integer","minimum":0,"default":0},"limit":{"type":"integer","minimum":1,"maximum":128,"default":64}}}})
}

fn control_method_schemas() -> Value {
    let mut schemas = serde_json::Map::new();
    for method in ["activate", "fill", "check", "focus"] {
        let mut required = vec!["method", "documentEpoch", "ref"];
        let mut properties = json!({
            "method": {"const":method},
            "documentEpoch": {"type":"integer", "minimum":1},
            "ref": {"type":"string", "description":"Canonical ref from the current controls observation"}
        });
        if method == "fill" {
            required.push("value");
            properties["value"] = json!({"type":"string"});
        } else if method == "check" {
            required.push("checked");
            properties["checked"] = json!({"type":"boolean"});
        }
        schemas.insert(
            method.into(),
            json!({"type":"object", "required":required, "properties":properties}),
        );
    }
    Value::Object(schemas)
}

fn run_client(
    address: &str,
    method: &str,
    request_file: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if method == "reload" {
        let mut stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        writeln!(stream, "{{\"method\":\"controls\"}}")?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response)?;
        let response: Value = serde_json::from_str(&response)?;
        if response["ok"] != true {
            return Err(format!("could not discover reload epoch: {response}").into());
        }
        let epoch = response["observation"]["documentEpoch"]
            .as_u64()
            .ok_or("missing document epoch")?;
        let mut stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        writeln!(
            stream,
            "{}",
            json!({"method":"reload","documentEpoch":epoch})
        )?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response)?;
        println!("{}", response.trim_end());
        return Ok(());
    }
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = match method {
        "describe" => json!({"method":"describe"}),
        "observe" => json!({"method":"observe"}),
        "actions" => json!({"method":"actions.list"}),
        "describe-action" => json!({"method":"actions.describe","action":request_file.ok_or("describe-action requires an action ID")?}),
        "controls" => json!({"method":"controls"}),
        "diagnostics" => json!({"method":"diagnostics"}),
        "network-status" => json!({"method":"networkStatus"}),
        "trace" => json!({"method":"trace"}),
        "debug-trace" => json!({"method":"debugTrace.read","documentEpoch":request_file.ok_or("debug-trace requires the document epoch from controls")?.parse::<u64>()?}),
        "changes" => if let Some(cursor) = request_file { json!({"method":"changes.subscribe","cursor":cursor}) } else { json!({"method":"changes.subscribe"}) },
        "reload-status" => json!({"method":"reloadStatus"}),
        "operation" => json!({"method":"operation","operationId":request_file.ok_or("operation requires an operation ID")?}),
        "cancel-operation" => json!({"method":"cancelOperation","operationId":request_file.ok_or("cancel-operation requires an operation ID")?}),
        "increment" => json!({"method":"invoke","action":"counter.increment","args":{}}),
        "request-file" => {
            let mut bytes = Vec::new();
            std::fs::File::open(request_file.ok_or("request-file requires a JSON file path")?)?
                .take(MAX_CONTROL_REQUEST_BYTES as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > MAX_CONTROL_REQUEST_BYTES { return Err("request file exceeds 64 KiB".into()); }
            serde_json::from_slice(&bytes)?
        }
        _ => return Err("client command must be describe, observe, actions, describe-action <id>, increment, changes [cursor], trace, controls, diagnostics, network-status, reload, reload-status, operation <id>, cancel-operation <id>, or request-file <path>".into()),
    };
    writeln!(stream, "{request}")?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    println!("{}", response.trim_end());
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("Lapui {}\n\nRun: lapui [--demo files | --html <index.html> [--js <bundle.js>]]\n     [--renderer cpu|gpu] [--watch] [--debug-trace] [--mcp-stdio | --mcp-bridge-id <id>]\nMCP stdio adapter: lapui mcp-stdio <bridge-id>\nExport current state: lapui [app options] --snapshot <image.png> [--width <pixels> --height <pixels>]\nClient: lapui client <address> describe|observe|increment|controls|diagnostics|network-status|trace|debug-trace <documentEpoch>|actions\n        lapui client <address> describe-action <action-id>\n        lapui client <address> changes [cursor]\n        lapui client <address> reload|reload-status\n        lapui client <address> operation|cancel-operation <operation-id>\n        lapui client <address> request-file <request.json>\n\n--mcp-stdio serves the current local UI over MCP stdio; stdout is reserved for the protocol.\n--mcp-bridge-id exposes an authenticated, loopback-only MCP bridge for reconnectable stdio adapters.\nCPU drawing and PNG export require the software-renderer feature (enabled by default).\n--watch uses local files and a window; it performs full document reloads.\nThe control address is printed when the window starts. Snapshots do not wait for all asynchronous work.", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--version") {
        println!("lapui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "client") {
        return run_client(
            args.get(2).ok_or("missing address")?,
            args.get(3).ok_or("missing command")?,
            args.get(4).map(String::as_str),
        );
    }
    if args.get(1).is_some_and(|arg| arg == "mcp-stdio") {
        let id = args.get(2).ok_or("mcp-stdio requires a bridge id")?;
        if args.len() != 3 {
            return Err("mcp-stdio accepts exactly one bridge id".into());
        }
        lapui::mcp_bridge::run_stdio(id)
            .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
        return Ok(());
    }

    let mut html_path: Option<PathBuf> = None;
    let mut js_path: Option<PathBuf> = None;
    let mut demo_files = false;
    let mut watch_files = false;
    let mut debug_trace = false;
    let mut mcp_stdio = false;
    let mut mcp_bridge_id: Option<String> = None;
    let mut software_renderer = cfg!(feature = "software-renderer");
    let mut snapshot_path: Option<PathBuf> = None;
    let mut snapshot_width = 1000u32;
    let mut snapshot_height = 700u32;
    let mut snapshot_dimensions_given = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--watch" => watch_files = true,
            "--debug-trace" => debug_trace = true,
            "--mcp-stdio" => mcp_stdio = true,
            "--mcp-bridge-id" => {
                index += 1;
                mcp_bridge_id = Some(
                    args.get(index)
                        .ok_or("--mcp-bridge-id requires an id")?
                        .clone(),
                );
            }
            "--snapshot" => {
                index += 1;
                snapshot_path = Some(
                    args.get(index)
                        .ok_or("--snapshot requires a PNG path")?
                        .into(),
                );
            }
            "--width" => {
                snapshot_dimensions_given = true;
                index += 1;
                snapshot_width = args.get(index).ok_or("--width requires pixels")?.parse()?;
            }
            "--height" => {
                snapshot_dimensions_given = true;
                index += 1;
                snapshot_height = args.get(index).ok_or("--height requires pixels")?.parse()?;
            }
            "--renderer" => {
                index += 1;
                software_renderer = match args.get(index).map(String::as_str) {
                    Some("gpu") => false,
                    Some("cpu") => true,
                    _ => return Err("--renderer requires gpu or cpu".into()),
                };
            }
            "--html" => {
                index += 1;
                html_path = Some(args.get(index).ok_or("--html requires a path")?.into());
            }
            "--js" => {
                index += 1;
                js_path = Some(args.get(index).ok_or("--js requires a path")?.into());
            }
            "--demo" => {
                index += 1;
                if args.get(index).is_none_or(|value| value != "files") {
                    return Err("--demo supports files".into());
                }
                demo_files = true;
            }
            other => {
                return Err(format!(
                "unknown argument: {other}; use --html <path> [--js <bundle.js>] or --demo files"
            )
                .into())
            }
        }
        index += 1;
    }

    if demo_files && (html_path.is_some() || js_path.is_some()) {
        return Err("--demo cannot be combined with --html or --js".into());
    }
    if mcp_stdio && mcp_bridge_id.is_some() {
        return Err("--mcp-stdio and --mcp-bridge-id are mutually exclusive".into());
    }
    if snapshot_path.is_some() && (mcp_stdio || mcp_bridge_id.is_some()) {
        return Err(
            "MCP transports require a running window and cannot be combined with --snapshot".into(),
        );
    }
    if snapshot_dimensions_given && snapshot_path.is_none() {
        return Err("--width and --height apply only to --snapshot".into());
    }
    if watch_files && snapshot_path.is_some() {
        return Err("--watch requires a window and cannot be combined with --snapshot".into());
    }
    #[cfg(not(feature = "software-renderer"))]
    if software_renderer || snapshot_path.is_some() {
        return Err("CPU rendering requires rebuilding with --features software-renderer".into());
    }
    let actions = if demo_files {
        lapui::demo::files()?
    } else {
        ActionRegistry::default()
    };
    let event_loop = if snapshot_path.is_some() {
        None
    } else {
        Some(create_default_event_loop())
    };
    let shell = event_loop
        .as_ref()
        .map(|event_loop| BlitzShellProxy::new(event_loop.create_proxy()));
    let proxy = shell.as_ref().map(|(proxy, _)| proxy.clone());
    let source = if let Some(html) = html_path {
        DocumentSource::Local {
            html: html.canonicalize()?,
            script: js_path.map(|path| path.canonicalize()).transpose()?,
        }
    } else {
        let html = if demo_files {
            include_str!("../ui/files/index.html")
        } else {
            include_str!("../ui/index.html")
        };
        if let Some(script) = js_path {
            DocumentSource::EmbeddedWithScript {
                html: html.into(),
                script: script.canonicalize()?,
            }
        } else {
            DocumentSource::Embedded {
                html: html.into(),
                script: if demo_files {
                    include_str!("../ui/files/app.js")
                } else {
                    include_str!("../ui/app.js")
                }
                .into(),
            }
        }
    };
    let (document, notify) = source.load(actions.clone(), proxy.clone())?;
    document.configure_debug_trace(debug_trace, false);
    #[cfg(feature = "software-renderer")]
    if let Some(path) = snapshot_path {
        let mut document = document;
        lapui::snapshot::save_png(&mut document, &path, snapshot_width, snapshot_height)?;
        println!("LAPUI_SNAPSHOT={}", path.display());
        return Ok(());
    }
    #[cfg(not(feature = "software-renderer"))]
    let _ = (snapshot_width, snapshot_height);
    let (proxy, events) = shell.ok_or("missing window event queue")?;
    let event_loop = event_loop.ok_or("missing window event loop")?;
    let listener = if mcp_stdio || mcp_bridge_id.is_some() {
        None
    } else {
        Some(TcpListener::bind("127.0.0.1:0")?)
    };
    let (mut document, reload) = ReloadDocument::new(
        document,
        notify,
        source,
        actions.clone(),
        Some(proxy.clone()),
    );
    let _mcp_bridge = mcp_bridge_id
        .as_deref()
        .map(|id| lapui::mcp_bridge::start(reload.clone(), actions.clone(), id))
        .transpose()
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    if watch_files {
        document.watch()?;
    }
    if let Some(listener) = listener {
        println!("LAPUI_CONTROL={}", listener.local_addr()?);
        let pool_reload = reload.clone();
        let pool_actions = actions.clone();
        let pool = ConnectionPool::new(move |mut stream| {
            let endpoint = pool_reload.endpoint();
            if let Err(err) = respond(
                &mut stream,
                &pool_actions,
                &endpoint.notify,
                &endpoint.controller,
                Some(&pool_reload),
            ) {
                eprintln!("Control request failed: {err}");
            }
        })?;
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => pool.submit(stream),
                    Err(err) => eprintln!("Control connection failed: {err}"),
                }
            }
        });
    }
    if mcp_stdio {
        let mcp_reload = reload.clone();
        let mcp_actions = actions.clone();
        std::thread::Builder::new()
            .name("lapui-mcp-stdio".into())
            .spawn(move || {
                if let Err(error) = lapui::mcp::serve_stdio(mcp_reload, mcp_actions) {
                    eprintln!("MCP stdio server stopped: {error}");
                }
            })?;
    }

    if software_renderer {
        #[cfg(feature = "software-renderer")]
        {
            let mut app = LapuiApplication::new(proxy, events);
            app.add_window(WindowConfig::new(
                Box::new(document),
                anyrender_vello_cpu::VelloCpuWindowRenderer::new(),
            ));
            event_loop.run_app(app)?;
        }
    } else {
        let mut app = LapuiApplication::new(proxy, events);
        app.add_window(WindowConfig::new(
            Box::new(document),
            VelloWindowRenderer::new(),
        ));
        event_loop.run_app(app)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_for_control_matches_a_semantic_condition_and_bounds_requests() {
        let request = json!({"method":"waitForControl","documentEpoch":7,"id":"scan-status",
            "field":"name","contains":"扫描完成","timeoutMs":500,"waitId":"condition-match-test"});
        let mut reads = 0;
        let result = wait_for_control_with(
            &request,
            |_| {
                reads += 1;
                Ok(
                    json!({"documentEpoch":7,"controls":[{"id":"scan-status","name":
                if reads == 1 {"扫描中"} else {"扫描完成"}}]}),
                )
            },
            || false,
        )
        .unwrap();
        assert_eq!(result["status"], "matched");
        assert_eq!(result["boundary"], "semantic_control_snapshot");
        assert_eq!(result["physicalPresentation"], "unsupported");
        assert!(reads >= 2);

        let immediate = json!({"method":"waitForControl","documentEpoch":7,"ref":"node:7:2:1",
            "field":"name","equals":"done","timeoutMs":0,"waitId":"condition-timeout-test"});
        let timeout = wait_for_control_with(
            &immediate,
            |_| Ok(json!({"documentEpoch":7,"controls":[{"ref":"node:7:2:1","name":"busy"}]})),
            || false,
        )
        .unwrap();
        assert_eq!(timeout["status"], "timed_out");
        assert_eq!(timeout["control"]["name"], "busy");

        let invalid = json!({"method":"waitForControl","documentEpoch":7,"id":"x","ref":"node:7:2:1",
            "field":"name","equals":"done"});
        assert_eq!(
            wait_for_control_with(
                &invalid,
                |_| panic!("invalid request must not poll"),
                || false
            )
            .unwrap_err()
            .code,
            "invalid_request"
        );
        let invalid_condition = json!({"method":"waitForControl","documentEpoch":7,"id":"x",
            "field":"name","equals":"done","contains":"done"});
        assert_eq!(
            wait_for_control_with(
                &invalid_condition,
                |_| panic!("invalid request must not poll"),
                || false
            )
            .unwrap_err()
            .code,
            "invalid_request"
        );
    }

    #[test]
    fn cancel_wait_cancels_an_active_condition_and_releases_its_identifier() {
        let wait_id = format!("cancel-test-{}", std::process::id());
        let (cancelled, active) = wait_registry().register(&wait_id).unwrap();
        let request = json!({"method":"waitForControl","documentEpoch":7,"id":"scan-status",
            "field":"name","equals":"done","timeoutMs":3000,"waitId":wait_id});
        let result = wait_for_control_with(
            &request,
            |_| {
                assert_eq!(
                    cancel_wait(&json!({"method":"cancelWait","waitId":wait_id})).unwrap()
                        ["status"],
                    "cancel_requested"
                );
                Ok(json!({"documentEpoch":7,"controls":[{"id":"scan-status","name":"busy"}]}))
            },
            || cancelled.load(Ordering::Acquire),
        )
        .unwrap_err();
        assert_eq!(result.code, "wait_cancelled");
        drop(active);
        assert_eq!(
            cancel_wait(&json!({"method":"cancelWait","waitId":wait_id}))
                .unwrap_err()
                .code,
            "unknown_wait"
        );
    }

    #[test]
    fn render_trace_wait_requires_a_frame_caused_by_the_requested_control() {
        let records = vec![
            json!({"sequence":10,"kind":"control","phase":"start","parentSequence":null,"data":{}}),
            json!({"sequence":11,"kind":"control","phase":"end","parentSequence":10,"data":{}}),
            json!({"sequence":12,"kind":"host_request","phase":"instant","parentSequence":10,"data":{}}),
            json!({"sequence":13,"kind":"completion","phase":"start","parentSequence":12,"data":{}}),
            json!({"sequence":14,"kind":"completion","phase":"end","parentSequence":13,"data":{}}),
            json!({"sequence":15,"kind":"frame","phase":"start","parentSequence":null,"data":{"causes":[14]}}),
            json!({"sequence":16,"kind":"layout","phase":"start","parentSequence":15,"data":{}}),
            json!({"sequence":17,"kind":"layout","phase":"end","parentSequence":16,"data":{"outcome":"resolved"}}),
            json!({"sequence":18,"kind":"frame","phase":"end","parentSequence":15,"data":{"outcome":"renderer_returned"}}),
        ];
        let mut parents = std::collections::HashMap::new();
        let mut frames = std::collections::HashSet::new();
        let mut layout_parents = std::collections::HashMap::new();
        let mut resolved_layouts = std::collections::HashSet::new();
        let result = consume_render_trace_page(
            &records,
            10,
            &mut parents,
            &mut frames,
            &mut layout_parents,
            &mut resolved_layouts,
        )
        .unwrap();
        assert_eq!(result["sequence"], 18);
        assert_eq!(result["data"]["outcome"], "renderer_returned");

        let unrelated = vec![
            json!({"sequence":20,"kind":"frame","phase":"start","parentSequence":null,"data":{"causes":[5]}}),
            json!({"sequence":21,"kind":"frame","phase":"end","parentSequence":20,"data":{"outcome":"renderer_returned"}}),
        ];
        assert!(consume_render_trace_page(
            &unrelated,
            10,
            &mut parents,
            &mut frames,
            &mut layout_parents,
            &mut resolved_layouts,
        )
        .is_none());
        let missing_layout = vec![
            json!({"sequence":22,"kind":"frame","phase":"start","parentSequence":null,"data":{"causes":[14]}}),
            json!({"sequence":23,"kind":"frame","phase":"end","parentSequence":22,"data":{"outcome":"renderer_returned"}}),
        ];
        assert!(consume_render_trace_page(
            &missing_layout,
            10,
            &mut parents,
            &mut frames,
            &mut layout_parents,
            &mut resolved_layouts,
        )
        .is_none());
    }

    #[test]
    fn wait_for_render_reports_only_causally_linked_renderer_return() {
        let request = json!({"method":"waitForRender","documentEpoch":7,"afterSequence":10,
            "timeoutMs":100,"waitId":"render-match-test"});
        let mut calls = 0;
        let result = wait_for_render_with(&request, |command, _| {
            calls += 1;
            assert_eq!(command["afterSequence"], 9);
            Ok(json!({"documentEpoch":7,"enabled":true,"session":2,"resyncRequired":false,
                "nextSequence":18,"latestSequence":18,"records":[
                    {"sequence":10,"kind":"control","phase":"start","parentSequence":null,"data":{}},
                    {"sequence":11,"kind":"control","phase":"end","parentSequence":10,"data":{}},
                    {"sequence":12,"kind":"host_request","phase":"instant","parentSequence":10,"data":{}},
                    {"sequence":13,"kind":"completion","phase":"start","parentSequence":12,"data":{}},
                    {"sequence":14,"kind":"completion","phase":"end","parentSequence":13,"data":{}},
                    {"sequence":15,"kind":"frame","phase":"start","parentSequence":null,"data":{"causes":[14]}},
                    {"sequence":16,"kind":"layout","phase":"start","parentSequence":15,"data":{}},
                    {"sequence":17,"kind":"layout","phase":"end","parentSequence":16,"data":{"outcome":"resolved"}},
                    {"sequence":18,"kind":"frame","phase":"end","parentSequence":15,"data":{"outcome":"renderer_returned"}}
                ]}))
        }, || false)
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result["status"], "rendered");
        assert_eq!(result["frameSequence"], 18);
        assert_eq!(result["physicalPresentation"], "unknown");

        let disabled = wait_for_render_with(
            &request,
            |_, _| Ok(json!({"documentEpoch":7,"enabled":false})),
            || false,
        )
        .unwrap_err();
        assert_eq!(disabled.code, "unsupported_capability");

        let ahead = json!({"method":"waitForRender","documentEpoch":7,"afterSequence":99,
            "timeoutMs":0,"waitId":"render-ahead-test"});
        let invalid_root = wait_for_render_with(
            &ahead,
            |_, _| {
                Ok(
                    json!({"documentEpoch":7,"enabled":true,"session":2,"resyncRequired":false,
                "nextSequence":0,"latestSequence":0,"records":[]}),
                )
            },
            || false,
        )
        .unwrap_err();
        assert_eq!(invalid_root.code, "invalid_request");
    }

    #[test]
    fn tcp_action_discovery_availability_and_scope_retirement_use_shared_registry() {
        use lapui::action_catalog::ActionOptions;
        let actions = lapui::demo::files().unwrap();
        let (doc, notify) = LapuiDocument::new(actions.clone(), None).unwrap();
        let controller = doc.controller();
        let call = |value| request(&actions, &controller, &notify, value);
        let first = call(json!({"method":"actions.list","prefix":"files.","limit":1}));
        assert_eq!(first["observation"]["items"][0]["id"], "files.query");
        assert_eq!(first["observation"]["hasMore"], true);
        let second = call(
            json!({"method":"actions.list","prefix":"files.","cursor":first["observation"]["nextCursor"]}),
        );
        assert_eq!(second["observation"]["items"][0]["id"], "files.rename");
        let description = call(json!({"method":"actions.describe","action":"files.rename"}));
        assert_eq!(description["observation"]["hasAvailabilityCheck"], true);
        let args = json!({"fileId":"file-1","name":"new.md","expectedFileVersion":1});
        let command = json!({"method":"actions.check","action":"files.rename","args":args});
        assert_eq!(call(command.clone())["observation"]["available"], true);
        assert_eq!(
            call(json!({"method":"invoke","action":"files.rename","args":args}))["ok"],
            true
        );
        let check = call(command);
        assert_eq!(check["observation"]["available"], false);
        assert_eq!(check["observation"]["reason"]["code"], "stale_entity");
        let scope = actions.create_scope("panel").unwrap();
        scope
            .register_query(
                lapui::action::ActionInfo {
                    id: "panel.read".into(),
                    description: "Panel read".into(),
                    input_schema: json!({"type":"object"}),
                    output_schema: json!({"type":"string"}),
                    kind: lapui::action::ActionKind::Read,
                },
                ActionOptions::default(),
                |_, _| Ok(json!("panel")),
            )
            .unwrap();
        assert_eq!(
            call(json!({"method":"actions.list","scope":scope.id()}))["observation"]["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        drop(scope);
        assert_eq!(
            call(json!({"method":"actions.describe","action":"panel.read"}))["error"]["code"],
            "unknown_action"
        );
    }

    #[test]
    fn tcp_change_subscription_allows_concurrent_writes_resume_and_idempotent_retry() {
        use lapui::changes::{ChangeScope, ChangesRequest};
        let actions = ActionRegistry::default();
        let (document, notify) = LapuiDocument::new(actions.clone(), None).unwrap();
        let controller = document.controller();
        let initial = request(
            &actions,
            &controller,
            &notify,
            json!({"method":"changes.subscribe","scope":"state"}),
        );
        assert_eq!(initial["observation"]["baseline"]["version"], 0);
        let cursor = initial["observation"]["cursor"]
            .as_str()
            .unwrap()
            .to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let pool = ConnectionPool::new({
            let actions = actions.clone();
            let controller = controller.clone();
            let notify = notify.clone();
            move |mut stream| {
                respond(&mut stream, &actions, &notify, &controller, None).unwrap();
            }
        })
        .unwrap();
        let mut waiting = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        waiting
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        writeln!(
            waiting,
            "{}",
            json!({"method":"changes.subscribe","scope":"state","cursor":cursor,"waitMs":1000})
        )
        .unwrap();
        pool.submit(listener.accept().unwrap().0);
        let reader = std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(waiting).read_line(&mut line).unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        });
        // The first connection waits; another connection can commit and wake it.
        let mut writing = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        writing
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        writeln!(writing,"{}",json!({"method":"invoke","action":"counter.increment","args":{},"requestId":"reconnect-write","expectedVersion":0})).unwrap();
        pool.submit(listener.accept().unwrap().0);
        let mut written = String::new();
        BufReader::new(writing).read_line(&mut written).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&written).unwrap()["observation"]["count"],
            1
        );
        let resumed = reader.join().unwrap();
        assert_eq!(
            resumed["observation"]["records"][0]["kind"],
            "state_changed"
        );
        let saved = resumed["observation"]["cursor"].as_str().unwrap();
        let retry = request(
            &actions,
            &controller,
            &notify,
            json!({"method":"invoke","action":"counter.increment","args":{},"requestId":"reconnect-write","expectedVersion":0}),
        );
        assert_eq!(retry["observation"]["count"], 1);
        let page = request(
            &actions,
            &controller,
            &notify,
            json!({"method":"changes.subscribe","scope":"state","cursor":saved,"waitMs":0}),
        );
        assert!(page["observation"]["records"]
            .as_array()
            .unwrap()
            .is_empty());
        for invalid in [
            json!({"method":"changes.subscribe","waitMs":1001}),
            json!({"method":"changes.subscribe","limit":0}),
            json!({"method":"changes.subscribe","scope":"private"}),
            json!({"method":"changes.subscribe","requestId":"not-an-action"}),
            json!({"method":"changes.subscribe","cursor":"malformed"}),
        ] {
            assert_eq!(
                request(&actions, &controller, &notify, invalid)["ok"],
                false
            );
        }
        let foreign = ActionRegistry::default()
            .subscribe_changes(ChangesRequest {
                scope: ChangeScope::State,
                ..Default::default()
            })
            .unwrap();
        let reset = request(
            &actions,
            &controller,
            &notify,
            json!({"method":"changes.subscribe","scope":"state","cursor":foreign.cursor}),
        );
        assert_eq!(reset["observation"]["resyncRequired"], true);
        assert_eq!(reset["observation"]["baseline"]["state"]["count"], 1);
    }

    fn request(
        actions: &ActionRegistry,
        controller: &DocumentController,
        notify: &Sender<Result<Value, String>>,
        request: Value,
    ) -> Value {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let actions = actions.clone();
        let controller = controller.clone();
        let notify = notify.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            respond(&mut stream, &actions, &notify, &controller, None).unwrap();
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        server.join().unwrap();
        serde_json::from_str(&response).unwrap()
    }

    #[test]
    fn tcp_registered_actions_job_retries_reconnect_queries_and_trace_share_state() {
        let actions = lapui::demo::files().unwrap();
        let (document, notify) =
            LapuiDocument::new_with_source(actions.clone(), None, "<html><body></body></html>", "")
                .unwrap();
        let controller = document.controller();
        let call = |command| request(&actions, &controller, &notify, command);
        let scan = json!({"method":"invoke","action":"files.scan","args":{},"requestId":"tcp-scan","expectedVersion":0});
        let accepted = call(scan.clone());
        assert_eq!(accepted["ok"], true);
        let retry = call(scan);
        assert_eq!(retry["observation"], accepted["observation"]);
        let id = accepted["observation"]["result"]["operationId"]
            .as_str()
            .unwrap();
        let mut job = call(json!({"method":"operation","operationId":id}));
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while job["observation"]["execution"] != "completed" {
            assert!(std::time::Instant::now() < deadline);
            assert_eq!(job["ok"], true);
            job = call(
                json!({"method":"operation","operationId":id,"afterRevision":job["observation"]["revision"]}),
            );
        }
        assert_eq!(job["observation"]["output"]["total"], 3);
        assert_eq!(
            call(json!({"method":"cancelOperation","operationId":id}))["observation"]["execution"],
            "completed"
        );
        assert_eq!(
            call(json!({"method":"operation","operationId":"missing"}))["error"]["code"],
            "unknown_operation"
        );
        let rename = json!({"method":"invoke","action":"files.rename","args":{"fileId":"file-1","name":"TCP改名.md","expectedFileVersion":1},"requestId":"tcp-rename","expectedVersion":0});
        let result = call(rename.clone());
        assert_eq!(result["ok"], true);
        assert_eq!(result["observation"]["version"], 1);
        assert_eq!(call(rename)["observation"], result["observation"]);
        let stale = call(
            json!({"method":"invoke","action":"files.rename","args":{"fileId":"file-1","name":"overwrite.md","expectedFileVersion":1}}),
        );
        assert_eq!(stale["error"]["code"], "stale_entity");
        let trace = call(json!({"method":"trace","afterSequence":0}));
        assert_eq!(trace["ok"], true);
        assert!(trace["observation"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["operationId"] == id && record["outcome"] == "accepted"));
        assert_eq!(actions.observe().state["files"][0]["name"], "TCP改名.md");
    }

    #[test]
    fn describe_exposes_protocol_capabilities_without_claiming_missing_features() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let actions = ActionRegistry::default();
        let (notify, _notifications) = std::sync::mpsc::channel();
        let (document, _) =
            LapuiDocument::new_with_source(actions.clone(), None, "<html><body></body></html>", "")
                .unwrap();
        let controller = document.controller();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            respond(&mut stream, &actions, &notify, &controller, None).unwrap();
        });

        let mut client = TcpStream::connect(address).unwrap();
        writeln!(client, "{{\"method\":\"describe\"}}").unwrap();
        let mut response = String::new();
        BufReader::new(client).read_line(&mut response).unwrap();
        server.join().unwrap();

        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["ok"], true);
        assert_eq!(response["observation"]["protocolVersion"], 1);
        assert_eq!(
            response["observation"]["methods"],
            json!([
                "describe",
                "observe",
                "invoke",
                "trace",
                "operation",
                "cancelOperation",
                "cancelWait",
                "controls",
                "waitForControl",
                "waitForRender",
                "diagnostics",
                "networkStatus",
                "activate",
                "fill",
                "check",
                "focus",
                "changes.subscribe",
                "actions.list",
                "actions.describe",
                "actions.check",
                "debugTrace.configure",
                "debugTrace.read"
            ])
        );
        assert_eq!(
            response["observation"]["capabilities"]["expectedVersion"],
            true
        );
        assert_eq!(response["observation"]["capabilities"]["debugTrace"], true);
        assert_eq!(
            response["observation"]["capabilities"]["controlConditionWait"],
            true
        );
        assert_eq!(
            response["observation"]["capabilities"]["controlWaitMaximumMs"],
            4000
        );
        assert_eq!(
            response["observation"]["capabilities"]["waitCancellation"],
            true
        );
        assert_eq!(
            response["observation"]["capabilities"]["activeWaitLimit"],
            4
        );
        assert_eq!(response["observation"]["capabilities"]["renderWait"], true);
        assert_eq!(
            response["observation"]["capabilities"]["renderWaitBoundary"],
            "renderer_returned_only"
        );
        assert_eq!(
            response["observation"]["waitForRenderSchema"]["properties"]["afterSequence"]
                ["minimum"],
            1
        );
        assert_eq!(
            response["observation"]["cancelWaitSchema"]["properties"]["waitId"]["maxLength"],
            128
        );
        assert_eq!(
            response["observation"]["waitForControlSchema"]["properties"]["field"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            6
        );
        assert_eq!(
            response["observation"]["capabilities"]["physicalPresentationAck"],
            false
        );
        assert_eq!(
            response["observation"]["capabilities"]["frameEvents"],
            false
        );
        assert_eq!(
            response["observation"]["debugTraceLimits"]["defaultEnabled"],
            false
        );
        assert_eq!(
            response["observation"]["debugTraceLimits"]["serializedBytes"],
            262144
        );
        assert_eq!(
            response["observation"]["debugTraceMethodSchemas"]["debugTrace.read"]["properties"]
                ["limit"]["maximum"],
            128
        );
        assert_eq!(
            response["observation"]["capabilities"]["subscriptions"],
            true
        );
        assert_eq!(
            response["observation"]["capabilities"]["asynchronousOperations"],
            true
        );
        assert_eq!(
            response["observation"]["capabilities"]["scriptExecutionLimits"],
            true
        );
        assert_eq!(
            response["observation"]["scriptExecutionLimits"]["interruption"],
            "suspendUntilReload"
        );
        assert_eq!(
            response["observation"]["scriptExecutionLimits"]["cooperative"],
            true
        );
        assert_eq!(
            response["observation"]["requestIdMethods"],
            json!(["invoke"])
        );
        assert_eq!(
            response["observation"]["controlMethodSchemas"]["fill"]["properties"]["value"]["type"],
            "string"
        );
        assert_eq!(
            response["observation"]["controlMethodSchemas"]["check"]["required"],
            json!(["method", "documentEpoch", "ref", "checked"])
        );
    }

    #[test]
    fn tcp_reload_routes_future_requests_to_the_new_document_and_keeps_shared_state() {
        use blitz::dom::Document;
        let actions = ActionRegistry::default();
        let source = DocumentSource::Embedded {
            html: include_str!("../ui/index.html").into(),
            script: include_str!("../ui/app.js").into(),
        };
        let (document, notify) = source.load(actions.clone(), None).unwrap();
        let (mut document, handle) =
            ReloadDocument::new(document, notify, source, actions.clone(), None);
        let epoch = document.inner().id();
        actions.invoke("counter.increment", &json!({})).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_handle = handle.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                let endpoint = server_handle.endpoint();
                respond(
                    &mut stream,
                    &actions,
                    &endpoint.notify,
                    &endpoint.controller,
                    Some(&server_handle),
                )
                .unwrap();
            }
        });
        let client = std::thread::spawn(move || {
            let call = |command: Value| {
                let mut stream = TcpStream::connect(address).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                writeln!(stream, "{command}").unwrap();
                let mut response = String::new();
                BufReader::new(stream).read_line(&mut response).unwrap();
                serde_json::from_str::<Value>(&response).unwrap()
            };
            assert_eq!(
                call(json!({"method":"describe"}))["observation"]["capabilities"]["documentReload"],
                true
            );
            let result = call(json!({"method":"reload","documentEpoch":epoch}));
            assert_eq!(result["ok"], true);
            let current = result["observation"]["documentEpoch"].as_u64().unwrap();
            assert_ne!(current, epoch as u64);
            let controls = call(json!({"method":"controls"}));
            assert_eq!(controls["observation"]["documentEpoch"], current);
            assert!(controls["observation"]["controls"]
                .as_array()
                .unwrap()
                .iter()
                .any(|control| control["id"] == "count" && control["name"] == "1"));
            assert_eq!(
                call(json!({"method":"reload","documentEpoch":epoch}))["error"]["code"],
                "stale_document"
            );
            assert_eq!(
                call(json!({"method":"reloadStatus"}))["observation"]["documentEpoch"],
                current
            );
            assert_eq!(call(json!({"method":"observe"}))["observation"]["count"], 1);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !client.is_finished() && std::time::Instant::now() < deadline {
            document.poll(None);
            std::thread::sleep(Duration::from_millis(1));
        }
        client.join().unwrap();
        server.join().unwrap();
    }
}
