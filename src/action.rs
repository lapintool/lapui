use crate::action_catalog::{
    ActionAvailability, ActionDescription, ActionOptions, ActionSummary, ActionsPage,
    ActionsRequest,
};
use crate::changes::{ChangeEvent, ChangeLog, ChangePage, ChangeScope, ChangesRequest, StateDelta};
use crate::operation::{OperationContext, Operations};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

const REQUEST_CACHE_CAPACITY: usize = 1024;
const REQUEST_CACHE_BYTES: usize = 4 * 1024 * 1024;
const TRACE_CAPACITY: usize = 256;
const MAX_STATE_BYTES: usize = 1024 * 1024;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_ACTIONS: usize = 256;

type Handler = dyn Fn(&mut Value, &Value) -> Result<Value, ActionError> + Send + Sync;
type JobHandler =
    dyn Fn(Value, Value, OperationContext) -> Result<Value, ActionError> + Send + Sync;

thread_local! {
    static EXECUTING: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

struct ExecutionGuard(usize);
impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        EXECUTING.with(|active| {
            active.borrow_mut().remove(&self.0);
        });
    }
}

/// A process-local, bounded registry shared by application UI and automation.
/// Handlers edit a private JSON state copy; only validated successful results
/// commit. Handlers must not perform irreversible effects or wait for another
/// action on this registry. Read-only `observe` calls are safe within handlers.
#[derive(Clone)]
pub struct ActionRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    execution: Mutex<()>,
    operations: Operations,
    changes: ChangeLog,
}

struct RegisteredAction {
    info: ActionInfo,
    handler: RegisteredHandler,
    options: ActionOptions,
    scope: Option<u64>,
    metadata_bytes: usize,
}

#[derive(Clone)]
enum RegisteredHandler {
    Transaction(Arc<Handler>),
    Job(Arc<JobHandler>),
}

struct State {
    data: Value,
    version: u64,
    actions: BTreeMap<String, RegisteredAction>,
    request_cache: HashMap<String, (String, Observation, usize, ActionKind)>,
    request_order: VecDeque<String>,
    request_bytes: usize,
    metadata_bytes: usize,
    traces: VecDeque<ActionTrace>,
    trace_sequence: u64,
    catalog_identity: String,
    catalog_revision: u64,
    next_scope: u64,
    scopes: BTreeMap<u64, String>,
}

/// Dropping or explicitly closing this owner retires its registered actions.
/// It holds a weak registry reference; already admitted calls and jobs can finish.
#[must_use = "keep the scope alive for as long as its actions are needed"]
pub struct ActionScope {
    registry: Weak<Inner>,
    id: u64,
}

impl ActionScope {
    pub(crate) fn belongs_to(&self, registry: &ActionRegistry) -> bool {
        Weak::ptr_eq(&self.registry, &Arc::downgrade(&registry.inner))
            && registry
                .inner
                .state
                .lock()
                .unwrap()
                .scopes
                .contains_key(&self.id)
    }
    pub fn id(&self) -> String {
        format!("scope:{}", self.id)
    }

    pub fn close(&self) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        // Drop captured handlers outside the state lock: their Rust destructors
        // may themselves observe the registry or retire another scope.
        let removed = {
            let mut state = registry.state.lock().unwrap();
            if state.scopes.remove(&self.id).is_none() {
                return;
            }
            let ids: Vec<_> = state
                .actions
                .iter()
                .filter(|(_, action)| action.scope == Some(self.id))
                .map(|(id, _)| id.clone())
                .collect();
            let mut removed = Vec::new();
            for action_id in ids {
                let action = state.actions.remove(&action_id).unwrap();
                state.metadata_bytes -= action.metadata_bytes;
                state.catalog_revision = state.catalog_revision.saturating_add(1);
                let _ = registry
                    .changes
                    .publish(ChangeEvent::ActionUnregistered { action_id });
                removed.push(action);
            }
            removed
        };
        drop(removed);
    }

    fn registry(&self) -> Result<ActionRegistry, ActionError> {
        self.registry
            .upgrade()
            .map(|inner| ActionRegistry { inner })
            .ok_or_else(|| ActionError::new("scope_closed", "action registry was dropped"))
    }

    pub fn register(
        &self,
        mut info: ActionInfo,
        options: ActionOptions,
        handler: impl Fn(&mut Value, &Value) -> Result<Value, ActionError> + Send + Sync + 'static,
    ) -> Result<(), ActionError> {
        info.kind = ActionKind::Write;
        self.registry()?.register_handler(
            info,
            RegisteredHandler::Transaction(Arc::new(handler)),
            options,
            Some(self.id),
        )
    }

    pub fn register_query(
        &self,
        mut info: ActionInfo,
        options: ActionOptions,
        handler: impl Fn(&Value, &Value) -> Result<Value, ActionError> + Send + Sync + 'static,
    ) -> Result<(), ActionError> {
        info.kind = ActionKind::Read;
        self.registry()?.register_handler(
            info,
            RegisteredHandler::Transaction(Arc::new(move |state, args| handler(state, args))),
            options,
            Some(self.id),
        )
    }

    pub fn register_operation(
        &self,
        mut info: ActionInfo,
        options: ActionOptions,
        handler: impl Fn(Value, Value, OperationContext) -> Result<Value, ActionError>
            + Send
            + Sync
            + 'static,
    ) -> Result<(), ActionError> {
        info.kind = ActionKind::Operation;
        self.registry()?.register_handler(
            info,
            RegisteredHandler::Job(Arc::new(handler)),
            options,
            Some(self.id),
        )
    }
}

impl Drop for ActionScope {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionError {
    pub code: String,
    pub message: String,
}

impl ActionError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for ActionError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub version: u64,
    /// Legacy demo projection. Custom registries normally use `state`.
    pub count: u64,
    pub state: Value,
    pub actions: Vec<ActionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionInfo {
    pub id: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
    #[serde(default)]
    pub kind: ActionKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Read,
    #[default]
    Write,
    Operation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionTrace {
    pub sequence: u64,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub outcome: String,
    pub version: u64,
    pub duration_micros: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TracePage {
    pub after_sequence: u64,
    pub next_sequence: u64,
    pub resync_required: bool,
    pub records: Vec<ActionTrace>,
}

/// The same invocation preconditions apply to JavaScript and TCP clients.
#[derive(Debug, Clone, Default)]
pub struct InvokeOptions {
    pub request_id: Option<String>,
    pub expected_version: Option<u64>,
}

impl InvokeOptions {
    pub fn parse(value: &Value) -> Result<Self, ActionError> {
        let object = value.as_object().ok_or_else(|| {
            ActionError::new("invalid_request", "invocation options must be an object")
        })?;
        for key in object.keys() {
            if !matches!(key.as_str(), "requestId" | "expectedVersion") {
                return Err(ActionError::new(
                    "invalid_request",
                    format!("unsupported invocation option: {key}"),
                ));
            }
        }
        let request_id = object
            .get("requestId")
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    ActionError::new("invalid_request", "requestId must be a string")
                })
            })
            .transpose()?;
        let expected_version = object
            .get("expectedVersion")
            .map(|value| {
                value.as_u64().ok_or_else(|| {
                    ActionError::new(
                        "invalid_request",
                        "expectedVersion must be a non-negative integer",
                    )
                })
            })
            .transpose()?;
        Ok(Self {
            request_id,
            expected_version,
        })
    }
}

impl Default for ActionRegistry {
    fn default() -> Self {
        let registry = Self::new(json!({"count":0})).expect("valid built-in state");
        registry
            .register(
                ActionInfo {
                    id: "counter.increment".into(),
                    description: "Increase the counter by one".into(),
                    input_schema: json!({"type":"object","additionalProperties":false}),
                    output_schema: json!({"type":"integer","minimum":0}),
                    kind: ActionKind::Write,
                },
                |state, _args| {
                    let count = state["count"]
                        .as_u64()
                        .ok_or_else(|| {
                            ActionError::new(
                                "invalid_state",
                                "counter state is not an unsigned integer",
                            )
                        })?
                        .checked_add(1)
                        .ok_or_else(|| ActionError::new("counter_overflow", "counter overflow"))?;
                    state["count"] = json!(count);
                    Ok(json!(count))
                },
            )
            .expect("valid built-in action");
        registry
    }
}

impl ActionRegistry {
    pub fn new(initial_state: Value) -> Result<Self, ActionError> {
        if initial_state.to_string().len() > MAX_STATE_BYTES {
            return Err(ActionError::new("invalid_state", "state exceeds 1 MiB"));
        }
        let changes = ChangeLog::default();
        Ok(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    data: initial_state,
                    version: 0,
                    actions: BTreeMap::new(),
                    request_cache: HashMap::new(),
                    request_order: VecDeque::new(),
                    request_bytes: 0,
                    metadata_bytes: 0,
                    traces: VecDeque::new(),
                    trace_sequence: 0,
                    catalog_identity: changes.checkpoint(ChangeScope::Actions),
                    catalog_revision: 0,
                    next_scope: 0,
                    scopes: BTreeMap::new(),
                }),
                execution: Mutex::new(()),
                operations: Operations::with_changes(changes.clone()),
                changes,
            }),
        })
    }

    /// Schemas are checked at registration and enforced on each invocation.
    pub fn register(
        &self,
        info: ActionInfo,
        handler: impl Fn(&mut Value, &Value) -> Result<Value, ActionError> + Send + Sync + 'static,
    ) -> Result<(), ActionError> {
        self.register_with_options(info, ActionOptions::default(), handler)
    }

    pub fn register_with_options(
        &self,
        mut info: ActionInfo,
        options: ActionOptions,
        handler: impl Fn(&mut Value, &Value) -> Result<Value, ActionError> + Send + Sync + 'static,
    ) -> Result<(), ActionError> {
        info.kind = ActionKind::Write;
        self.register_handler(
            info,
            RegisteredHandler::Transaction(Arc::new(handler)),
            options,
            None,
        )
    }

    pub fn register_query(
        &self,
        info: ActionInfo,
        handler: impl Fn(&Value, &Value) -> Result<Value, ActionError> + Send + Sync + 'static,
    ) -> Result<(), ActionError> {
        self.register_query_with_options(info, ActionOptions::default(), handler)
    }

    pub fn register_query_with_options(
        &self,
        mut info: ActionInfo,
        options: ActionOptions,
        handler: impl Fn(&Value, &Value) -> Result<Value, ActionError> + Send + Sync + 'static,
    ) -> Result<(), ActionError> {
        info.kind = ActionKind::Read;
        self.register_handler(
            info,
            RegisteredHandler::Transaction(Arc::new(move |state, args| handler(state, args))),
            options,
            None,
        )
    }

    /// Jobs receive a state snapshot and return an output. They do not commit
    /// application state or acknowledge presentation of a rendered frame.
    pub fn register_operation(
        &self,
        info: ActionInfo,
        handler: impl Fn(Value, Value, OperationContext) -> Result<Value, ActionError>
            + Send
            + Sync
            + 'static,
    ) -> Result<(), ActionError> {
        self.register_operation_with_options(info, ActionOptions::default(), handler)
    }

    pub fn register_operation_with_options(
        &self,
        mut info: ActionInfo,
        options: ActionOptions,
        handler: impl Fn(Value, Value, OperationContext) -> Result<Value, ActionError>
            + Send
            + Sync
            + 'static,
    ) -> Result<(), ActionError> {
        info.kind = ActionKind::Operation;
        self.register_handler(
            info,
            RegisteredHandler::Job(Arc::new(handler)),
            options,
            None,
        )
    }

    pub fn create_scope(&self, name: &str) -> Result<ActionScope, ActionError> {
        if name.is_empty() || name.len() > 128 {
            return Err(ActionError::new(
                "invalid_scope",
                "scope name must contain 1..128 bytes",
            ));
        }
        let mut state = self.inner.state.lock().unwrap();
        if state.scopes.len() == MAX_ACTIONS {
            return Err(ActionError::new(
                "scope_limit",
                "registry supports at most 256 action scopes",
            ));
        }
        let id = state
            .next_scope
            .checked_add(1)
            .ok_or_else(|| ActionError::new("scope_limit", "action scope identity exhausted"))?;
        state.next_scope = id;
        state.scopes.insert(id, name.into());
        Ok(ActionScope {
            registry: Arc::downgrade(&self.inner),
            id,
        })
    }

    pub fn action_limits() -> Value {
        json!({"actions":MAX_ACTIONS,"scopes":MAX_ACTIONS,"pageItems":64,"metadataBytes":MAX_STATE_BYTES,
            "availability":"state-and-arguments","scopeClose":"retire-future-invocations","durable":false})
    }

    /// Shared read-only protocol for Rust, JavaScript workers and TCP clients.
    pub fn action_catalog_request(&self, request: &Value) -> Result<Value, ActionError> {
        let object = request.as_object().ok_or_else(|| {
            ActionError::new(
                "invalid_request",
                "action catalog request must be an object",
            )
        })?;
        match object.get("method").and_then(Value::as_str) {
            Some("actions.list") => {
                let mut options = object.clone();
                options.remove("method");
                let options = serde_json::from_value(Value::Object(options))
                    .map_err(|error| ActionError::new("invalid_request", error.to_string()))?;
                self.list_actions(options).map(|page| json!(page))
            }
            Some(method @ ("actions.describe" | "actions.check")) => {
                if object.keys().any(|key| {
                    key != "method"
                        && key != "action"
                        && !(method == "actions.check" && key == "args")
                }) {
                    return Err(ActionError::new(
                        "invalid_request",
                        "unsupported action catalog field",
                    ));
                }
                let name = object
                    .get("action")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty() && name.len() <= 128)
                    .ok_or_else(|| {
                        ActionError::new("invalid_action", "action must contain 1..128 bytes")
                    })?;
                if method == "actions.describe" {
                    self.describe_action(name)
                        .map(|description| json!(description))
                } else {
                    self.check_action(name, object.get("args").unwrap_or(&json!({})))
                        .map(|availability| json!(availability))
                }
            }
            _ => Err(ActionError::new(
                "invalid_request",
                "unknown action catalog method",
            )),
        }
    }

    /// Short, sorted metadata pages; registration changes invalidate cursors.
    pub fn list_actions(&self, request: ActionsRequest) -> Result<ActionsPage, ActionError> {
        request.validate()?;
        let state = self.inner.state.lock().unwrap();
        let context = format!(
            "{}:{}:{}",
            state.catalog_identity,
            state.catalog_revision,
            json!({"prefix":request.prefix,"scope":request.scope})
        );
        let offset = match &request.cursor {
            None => 0,
            Some(cursor) => {
                let (prefix, offset) = cursor.rsplit_once(':').ok_or_else(|| {
                    ActionError::new("invalid_cursor", "malformed action-list cursor")
                })?;
                let offset = offset.parse::<usize>().map_err(|_| {
                    ActionError::new("invalid_cursor", "malformed action-list cursor")
                })?;
                if prefix != context {
                    return Err(ActionError::new(
                        "stale_action_cursor",
                        "action catalog or filters changed; restart without cursor",
                    ));
                }
                offset
            }
        };
        let matches: Vec<_> = state
            .actions
            .values()
            .filter(|action| {
                action.info.id.starts_with(&request.prefix)
                    && request.scope.as_ref().is_none_or(|scope| {
                        action
                            .scope
                            .is_some_and(|id| *scope == format!("scope:{id}"))
                    })
            })
            .collect();
        if offset > matches.len() {
            return Err(ActionError::new(
                "invalid_cursor",
                "action-list cursor exceeds the catalog",
            ));
        }
        let items: Vec<_> = matches
            .iter()
            .skip(offset)
            .take(request.limit)
            .map(|action| ActionSummary {
                id: action.info.id.clone(),
                description: action.info.description.clone(),
                kind: action.info.kind,
                scope: action.scope.map(|id| format!("scope:{id}")),
                scope_name: action.scope.and_then(|id| state.scopes.get(&id).cloned()),
                has_availability_check: action.options.availability.is_some(),
            })
            .collect();
        let next = offset + items.len();
        let has_more = next < matches.len();
        Ok(ActionsPage {
            revision: state.catalog_revision,
            has_more,
            next_cursor: has_more.then(|| format!("{context}:{next}")),
            items,
        })
    }

    pub fn describe_action(&self, name: &str) -> Result<ActionDescription, ActionError> {
        let state = self.inner.state.lock().unwrap();
        let action = state
            .actions
            .get(name)
            .ok_or_else(|| ActionError::new("unknown_action", "action is not registered"))?;
        Ok(ActionDescription {
            info: action.info.clone(),
            scope: action.scope.map(|id| format!("scope:{id}")),
            scope_name: action.scope.and_then(|id| state.scopes.get(&id).cloned()),
            has_availability_check: action.options.availability.is_some(),
        })
    }

    /// A snapshot check, not authorization to execute later. Fresh invocations
    /// validate arguments and evaluate the same predicate under execution ordering.
    pub fn check_action(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<ActionAvailability, ActionError> {
        if args.to_string().len() > MAX_PAYLOAD_BYTES {
            return Err(ActionError::new(
                "invalid_arguments",
                "arguments exceed 64 KiB",
            ));
        }
        let identity = Arc::as_ptr(&self.inner) as usize;
        if !EXECUTING.with(|active| active.borrow_mut().insert(identity)) {
            return Err(ActionError::new(
                "reentrant_action",
                "cannot check actions recursively on the same registry",
            ));
        }
        let _guard = ExecutionGuard(identity);
        let _execution = self.inner.execution.lock().unwrap();
        let state = self.inner.state.lock().unwrap();
        let action = state
            .actions
            .get(name)
            .ok_or_else(|| ActionError::new("unknown_action", "action is not registered"))?;
        crate::schema::validate(&action.info.input_schema, args, "invalid_arguments")?;
        let options = action.options.clone();
        let snapshot = state.data.clone();
        let version = state.version;
        drop(state);
        let reason = Self::check_availability(&options, &snapshot, args).err();
        Ok(ActionAvailability {
            action_id: name.into(),
            version,
            available: reason.is_none(),
            reason,
        })
    }

    fn check_availability(
        options: &ActionOptions,
        state: &Value,
        args: &Value,
    ) -> Result<(), ActionError> {
        let Some(check) = &options.availability else {
            return Ok(());
        };
        catch_unwind(AssertUnwindSafe(|| check(state, args)))
            .unwrap_or_else(|_| {
                Err(ActionError::new(
                    "availability_panicked",
                    "Rust availability check panicked",
                ))
            })
            .map_err(Self::bounded_error)
    }

    fn bounded_error(mut error: ActionError) -> ActionError {
        for (text, limit) in [(&mut error.code, 128), (&mut error.message, 16 * 1024)] {
            if text.len() > limit {
                let mut end = limit;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
            }
        }
        error
    }

    pub fn operations(&self) -> Operations {
        self.inner.operations.clone()
    }

    pub fn change_limits() -> Value {
        ChangeLog::limits()
    }

    /// Initial and resync responses include a consistent snapshot/checkpoint.
    /// Wait off the UI thread; the same registry/cursors survive document reload.
    pub fn subscribe_changes(&self, request: ChangesRequest) -> Result<ChangePage, ActionError> {
        request.validate()?;
        if request.cursor.is_none() {
            let (baseline, cursor) = self.change_baseline(request.scope);
            return Ok(ChangePage {
                scope: request.scope,
                cursor,
                resync_required: false,
                resync_reason: None,
                has_more: false,
                records: Vec::new(),
                baseline: Some(baseline),
            });
        }
        let mut page = self.inner.changes.page(&request)?;
        if page.resync_required {
            let (baseline, cursor) = self.change_baseline(request.scope);
            page.baseline = Some(baseline);
            page.cursor = cursor;
        }
        Ok(page)
    }

    fn change_baseline(&self, scope: ChangeScope) -> (Value, String) {
        let state = self.inner.state.lock().unwrap();
        let checkpoint = |operations| {
            let baseline = match scope {
                ChangeScope::Application => {
                    json!({"observation": Self::observation(&state), "operations":operations,"traceSequence":state.trace_sequence})
                }
                ChangeScope::State => json!({"version":state.version,"state":state.data}),
                ChangeScope::Actions => {
                    json!({"version":state.version,"actions":state.actions.values().map(|action| &action.info).collect::<Vec<_>>(),"traceSequence":state.trace_sequence})
                }
                ChangeScope::Operations => json!({"operations":operations}),
                ChangeScope::Host => json!({"snapshotAvailable":false}),
            };
            (baseline, self.inner.changes.checkpoint(scope))
        };
        if matches!(scope, ChangeScope::Application | ChangeScope::Operations) {
            self.inner.operations.with_summaries(checkpoint)
        } else {
            checkpoint(Vec::new())
        }
    }

    /// Explicitly expose a host event. Event payloads must be safe for every
    /// caller allowed to read this registry; arbitrary private state is not exported.
    pub fn emit_event(&self, name: &str, payload: Value) -> Result<(), ActionError> {
        if name.is_empty() || name.len() > 128 {
            return Err(ActionError::new(
                "invalid_event",
                "host event names must contain 1..128 bytes",
            ));
        }
        self.inner.changes.publish(ChangeEvent::HostEvent {
            name: name.into(),
            payload,
        })
    }

    fn register_handler(
        &self,
        info: ActionInfo,
        handler: RegisteredHandler,
        options: ActionOptions,
        scope: Option<u64>,
    ) -> Result<(), ActionError> {
        if info.id.is_empty() || info.id.len() > 128 || info.description.len() > 4096 {
            return Err(ActionError::new(
                "invalid_action",
                "action id must contain 1 to 128 bytes and description at most 4 KiB",
            ));
        }
        crate::schema::check(&info.input_schema)?;
        crate::schema::check(&info.output_schema)?;
        let mut state = self.inner.state.lock().unwrap();
        if scope.is_some_and(|id| !state.scopes.contains_key(&id)) {
            return Err(ActionError::new("scope_closed", "action scope is closed"));
        }
        if state.actions.contains_key(&info.id) {
            return Err(ActionError::new(
                "duplicate_action",
                "action id is already registered",
            ));
        }
        if state.actions.len() == MAX_ACTIONS {
            return Err(ActionError::new(
                "action_limit",
                "registry supports at most 256 actions",
            ));
        }
        let metadata_bytes = serde_json::to_vec(&info)
            .expect("JSON action metadata")
            .len();
        if state.metadata_bytes + metadata_bytes > MAX_STATE_BYTES {
            return Err(ActionError::new(
                "action_limit",
                "action metadata exceeds 1 MiB",
            ));
        }
        state.catalog_revision = state
            .catalog_revision
            .checked_add(1)
            .ok_or_else(|| ActionError::new("action_limit", "catalog revision exhausted"))?;
        state.metadata_bytes += metadata_bytes;
        let action_id = info.id.clone();
        state.actions.insert(
            info.id.clone(),
            RegisteredAction {
                info,
                handler,
                options,
                scope,
                metadata_bytes,
            },
        );
        let _ = self
            .inner
            .changes
            .publish(ChangeEvent::ActionRegistered { action_id });
        Ok(())
    }

    /// Read only the application revision without cloning exposed state.
    pub fn version(&self) -> u64 {
        self.inner.state.lock().unwrap().version
    }

    pub fn observe(&self) -> Observation {
        let state = self.inner.state.lock().unwrap();
        Self::observation(&state)
    }

    fn observation(state: &State) -> Observation {
        Observation {
            version: state.version,
            count: state.data.get("count").and_then(Value::as_u64).unwrap_or(0),
            state: state.data.clone(),
            actions: state
                .actions
                .values()
                .map(|action| action.info.clone())
                .collect(),
            result: None,
        }
    }

    pub fn trace(&self, after_sequence: u64) -> TracePage {
        let state = self.inner.state.lock().unwrap();
        TracePage {
            after_sequence,
            next_sequence: state.trace_sequence,
            resync_required: after_sequence > state.trace_sequence
                || state
                    .traces
                    .front()
                    .is_some_and(|trace| after_sequence < trace.sequence.saturating_sub(1)),
            records: state
                .traces
                .iter()
                .filter(|trace| trace.sequence > after_sequence)
                .cloned()
                .collect(),
        }
    }

    pub fn invoke(&self, name: &str, args: &Value) -> Result<Observation, ActionError> {
        self.invoke_with_request_id(name, args, None)
    }

    pub fn invoke_with_request_id(
        &self,
        name: &str,
        args: &Value,
        request_id: Option<&str>,
    ) -> Result<Observation, ActionError> {
        self.invoke_checked(name, args, request_id, None)
    }

    pub fn invoke_checked(
        &self,
        name: &str,
        args: &Value,
        request_id: Option<&str>,
        expected_version: Option<u64>,
    ) -> Result<Observation, ActionError> {
        if name.is_empty() || name.len() > 128 {
            return Err(ActionError::new(
                "invalid_action",
                "action name must contain 1 to 128 bytes",
            ));
        }
        if request_id.is_some_and(|id| id.is_empty() || id.len() > 128) {
            return Err(ActionError::new(
                "invalid_request_id",
                "requestId must contain 1 to 128 bytes",
            ));
        }
        if args.to_string().len() > MAX_PAYLOAD_BYTES {
            return Err(ActionError::new(
                "invalid_arguments",
                "arguments exceed 64 KiB",
            ));
        }
        let fingerprint = serde_json::to_string(&json!({
            "action": name,
            "args": args,
            "expectedVersion": expected_version,
        }))
        .unwrap_or_default();
        let identity = Arc::as_ptr(&self.inner) as usize;
        if !EXECUTING.with(|active| active.borrow_mut().insert(identity)) {
            return Err(ActionError::new(
                "reentrant_action",
                "a handler cannot invoke another action on the same registry",
            ));
        }
        let _guard = ExecutionGuard(identity);
        let _execution = self.inner.execution.lock().unwrap();
        let started = Instant::now();
        let replayed = request_id.is_some_and(|id| {
            self.inner
                .state
                .lock()
                .unwrap()
                .request_cache
                .get(id)
                .is_some_and(|cached| cached.0 == fingerprint)
        });
        let result = self
            .execute(name, args, request_id, expected_version, &fingerprint)
            .map_err(Self::bounded_error);
        let mut state = self.inner.state.lock().unwrap();
        state.trace_sequence = state.trace_sequence.saturating_add(1);
        let sequence = state.trace_sequence;
        let version = state.version;
        let accepted = result
            .as_ref()
            .is_ok_and(|(_, kind)| *kind == ActionKind::Operation);
        state.traces.push_back(ActionTrace {
            sequence,
            action: name.into(),
            request_id: request_id.map(str::to_owned),
            outcome: result.as_ref().map_or_else(
                |error| error.code.clone(),
                |_| {
                    if replayed {
                        "replayed".into()
                    } else if accepted {
                        "accepted".into()
                    } else {
                        "completed".into()
                    }
                },
            ),
            version,
            duration_micros: started.elapsed().as_micros().min(u64::MAX as u128) as u64,
            operation_id: if accepted {
                result
                    .as_ref()
                    .ok()
                    .and_then(|(result, _)| result.result.as_ref())
                    .and_then(|result| result.get("operationId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            } else {
                None
            },
        });
        while state.traces.len() > TRACE_CAPACITY {
            state.traces.pop_front();
        }
        let _ = self.inner.changes.publish(ChangeEvent::ActionFinished(
            state.traces.back().unwrap().clone(),
        ));
        result.map(|(observation, _)| observation)
    }

    fn execute(
        &self,
        name: &str,
        args: &Value,
        request_id: Option<&str>,
        expected_version: Option<u64>,
        fingerprint: &str,
    ) -> Result<(Observation, ActionKind), ActionError> {
        let state = self.inner.state.lock().unwrap();
        if let Some(request_id) = request_id {
            if let Some((previous_fingerprint, result, _bytes, kind)) =
                state.request_cache.get(request_id)
            {
                if previous_fingerprint != fingerprint {
                    return Err(ActionError::new(
                        "request_id_conflict",
                        "requestId was already used with a different action or payload",
                    ));
                }
                return Ok((result.clone(), *kind));
            }
        }
        let action = state
            .actions
            .get(name)
            .ok_or_else(|| ActionError::new("unknown_action", format!("unknown action: {name}")))?;
        crate::schema::validate(&action.info.input_schema, args, "invalid_arguments")?;
        if let Some(expected) = expected_version {
            if expected != state.version {
                return Err(ActionError::new(
                    "stale_state",
                    format!(
                        "expected version {expected}, current version is {}",
                        state.version
                    ),
                ));
            }
        }
        let kind = action.info.kind;
        let next_version = if kind == ActionKind::Write {
            state
                .version
                .checked_add(1)
                .ok_or_else(|| ActionError::new("version_overflow", "state revision overflow"))?
        } else {
            state.version
        };
        let mut candidate = state.data.clone();
        let handler = action.handler.clone();
        let output_schema = action.info.output_schema.clone();
        let options = action.options.clone();
        drop(state);
        Self::check_availability(&options, &candidate, args)?;
        let output = match handler {
            RegisteredHandler::Transaction(handler) => {
                let output = catch_unwind(AssertUnwindSafe(|| handler(&mut candidate, args)))
                    .map_err(|_| {
                        ActionError::new(
                            "action_panicked",
                            "Rust action panicked; state was not committed",
                        )
                    })??;
                crate::schema::validate(&output_schema, &output, "invalid_output")?;
                output
            }
            RegisteredHandler::Job(handler) => {
                let snapshot = candidate.clone();
                let args = args.clone();
                let operation = self.inner.operations.start(name, move |context| {
                    let output = handler(snapshot, args, context)?;
                    crate::schema::validate(&output_schema, &output, "invalid_output")?;
                    if output.to_string().len() > MAX_PAYLOAD_BYTES {
                        return Err(ActionError::new(
                            "invalid_output",
                            "operation result exceeds 64 KiB",
                        ));
                    }
                    Ok(output)
                })?;
                json!({"operationId":operation.operation_id,"execution":"accepted"})
            }
        };
        if candidate.to_string().len() > MAX_STATE_BYTES {
            return Err(ActionError::new("invalid_state", "state exceeds 1 MiB"));
        }
        if output.to_string().len() > MAX_PAYLOAD_BYTES {
            return Err(ActionError::new("invalid_output", "result exceeds 64 KiB"));
        }
        let mut state = self.inner.state.lock().unwrap();
        let state_change = if kind == ActionKind::Write {
            let delta = StateDelta::between(&state.data, &candidate);
            Some(ChangeEvent::StateChanged {
                action_id: name.into(),
                request_id: request_id.map(str::to_owned),
                base_version: state.version,
                version: next_version,
                requires_snapshot: delta.is_none(),
                delta,
            })
        } else {
            None
        };
        state.data = candidate;
        state.version = next_version;
        if let Some(event) = state_change {
            let _ = self.inner.changes.publish(event);
        }
        let mut result = Self::observation(&state);
        result.result = Some(output);
        if let Some(request_id) = request_id {
            let bytes = fingerprint.len()
                + request_id.len()
                + serde_json::to_vec(&result).expect("JSON observation").len();
            state.request_bytes += bytes;
            state.request_cache.insert(
                request_id.to_owned(),
                (fingerprint.to_owned(), result.clone(), bytes, kind),
            );
            state.request_order.push_back(request_id.to_owned());
            while state.request_order.len() > REQUEST_CACHE_CAPACITY
                || state.request_bytes > REQUEST_CACHE_BYTES
            {
                if let Some(expired) = state.request_order.pop_front() {
                    if let Some((_fingerprint, _result, bytes, _kind)) =
                        state.request_cache.remove(&expired)
                    {
                        state.request_bytes -= bytes;
                    }
                }
            }
        }
        Ok((result, kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_scopes_retire_registrations_reclaim_limits_and_publish_catalog_changes() {
        let registry = ActionRegistry::new(json!({})).unwrap();
        let baseline = registry
            .subscribe_changes(ChangesRequest {
                scope: ChangeScope::Actions,
                ..Default::default()
            })
            .unwrap();
        let scope = registry.create_scope("dialog").unwrap();
        scope
            .register_query(info("dialog.read"), ActionOptions::default(), |_, _| {
                Ok(json!("first"))
            })
            .unwrap();
        let first = registry
            .list_actions(ActionsRequest {
                scope: Some(scope.id()),
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(first.items[0].scope_name.as_deref(), Some("dialog"));
        assert!(registry.inner.state.lock().unwrap().metadata_bytes > 0);
        scope.close();
        scope.close();
        assert_eq!(registry.inner.state.lock().unwrap().metadata_bytes, 0);
        assert_eq!(
            registry.invoke("dialog.read", &json!({})).unwrap_err().code,
            "unknown_action"
        );
        assert_eq!(
            scope
                .register_query(info("new"), ActionOptions::default(), |_, _| Ok(json!(
                    "new"
                )))
                .unwrap_err()
                .code,
            "scope_closed"
        );
        let replacement = registry.create_scope("dialog").unwrap();
        replacement
            .register_query(info("dialog.read"), ActionOptions::default(), |_, _| {
                Ok(json!("second"))
            })
            .unwrap();
        drop(scope); // A stale owner must not remove the replacement.
        assert_eq!(
            registry.invoke("dialog.read", &json!({})).unwrap().result,
            Some(json!("second"))
        );
        drop(replacement);
        let changes = follow(&registry, &baseline, 128);
        assert_eq!(
            changes
                .records
                .iter()
                .filter(|record| matches!(record.event, ChangeEvent::ActionUnregistered { .. }))
                .count(),
            2
        );
        assert!(registry.observe().actions.is_empty());
        let mut scopes = Vec::new();
        for _ in 0..256 {
            scopes.push(registry.create_scope("temporary").unwrap());
        }
        assert_eq!(
            registry.create_scope("full").err().unwrap().code,
            "scope_limit"
        );
        drop(scopes);
        let retained = registry.create_scope("retained").unwrap();
        let weak = Arc::downgrade(&registry.inner);
        drop(registry);
        assert!(weak.upgrade().is_none());
        assert_eq!(
            retained
                .register_query(info("gone"), ActionOptions::default(), |_, _| Ok(json!(
                    "gone"
                )))
                .unwrap_err()
                .code,
            "scope_closed"
        );
    }

    #[test]
    fn action_discovery_pages_bind_catalog_filters_and_registry_but_not_state_writes() {
        let registry = ActionRegistry::default();
        for id in ["files.a", "files.b", "files.c"] {
            registry
                .register_query(info(id), |_, _| Ok(json!("ok")))
                .unwrap();
        }
        let first = registry
            .list_actions(ActionsRequest {
                prefix: "files.".into(),
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(first.items[0].id, "files.a");
        assert!(first.has_more);
        registry.invoke("counter.increment", &json!({})).unwrap();
        let second = registry
            .list_actions(ActionsRequest {
                prefix: "files.".into(),
                cursor: first.next_cursor.clone(),
                limit: 64,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            second
                .items
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            ["files.b", "files.c"]
        );
        assert!(!second.has_more);
        assert!(second.next_cursor.is_none());
        assert_eq!(second.revision, first.revision);
        for other in [
            ActionsRequest {
                prefix: "other".into(),
                cursor: first.next_cursor.clone(),
                ..Default::default()
            },
            ActionsRequest {
                prefix: "files.".into(),
                scope: Some("scope:1".into()),
                cursor: first.next_cursor.clone(),
                ..Default::default()
            },
        ] {
            assert_eq!(
                registry.list_actions(other).unwrap_err().code,
                "stale_action_cursor"
            );
        }
        let foreign = ActionRegistry::default();
        assert_eq!(
            foreign
                .list_actions(ActionsRequest {
                    prefix: "files.".into(),
                    cursor: first.next_cursor.clone(),
                    ..Default::default()
                })
                .unwrap_err()
                .code,
            "stale_action_cursor"
        );
        registry
            .register_query(info("files.d"), |_, _| Ok(json!("ok")))
            .unwrap();
        assert_eq!(
            registry
                .list_actions(ActionsRequest {
                    prefix: "files.".into(),
                    cursor: first.next_cursor,
                    ..Default::default()
                })
                .unwrap_err()
                .code,
            "stale_action_cursor"
        );
        for invalid in [
            json!({"method":"actions.list","limit":0}),
            json!({"method":"actions.list","limit":65}),
            json!({"method":"actions.list","prefix":"x".repeat(129)}),
            json!({"method":"actions.list","unexpected":true}),
            json!({"method":"actions.describe","action":"files.a","args":{}}),
            json!({"method":"actions.check","action":"files.a","args":{},"expectedVersion":0}),
        ] {
            assert!(registry.action_catalog_request(&invalid).is_err());
        }
    }

    #[test]
    fn availability_checks_revalidate_business_state_without_writes_and_bound_errors() {
        let registry = ActionRegistry::new(json!({"locked":false,"writes":0})).unwrap();
        let check = ActionOptions::default().with_availability(|state, _| {
            if state["locked"] == true {
                Err(ActionError::new(
                    "locked",
                    "Unlock the workspace before editing",
                ))
            } else {
                Ok(())
            }
        });
        registry
            .register_with_options(info("edit"), check.clone(), |state, _| {
                state["writes"] = json!(state["writes"].as_u64().unwrap() + 1);
                Ok(json!("edited"))
            })
            .unwrap();
        registry
            .register_query_with_options(info("read"), check.clone(), |_, _| Ok(json!("read")))
            .unwrap();
        registry
            .register_operation_with_options(info("job"), check, |_, _, _| Ok(json!("job")))
            .unwrap();
        registry
            .register(info("lock"), |state, _| {
                state["locked"] = json!(true);
                Ok(json!("locked"))
            })
            .unwrap();
        assert!(registry.check_action("edit", &json!({})).unwrap().available);
        let description = registry.describe_action("edit").unwrap();
        assert!(description.has_availability_check);
        registry
            .invoke_with_request_id("edit", &json!({}), Some("edit-1"))
            .unwrap();
        registry.invoke("lock", &json!({})).unwrap();
        for id in ["edit", "read", "job"] {
            let status = registry.check_action(id, &json!({})).unwrap();
            assert!(!status.available);
            assert_eq!(status.reason.unwrap().code, "locked");
            assert_eq!(status.version, 2);
            assert_eq!(registry.invoke(id, &json!({})).unwrap_err().code, "locked");
        }
        assert_eq!(
            registry
                .invoke_with_request_id("edit", &json!({}), Some("edit-1"))
                .unwrap()
                .version,
            1
        );
        assert_eq!(registry.observe().version, 2);
        assert_eq!(registry.observe().state["writes"], 1);
        assert_eq!(
            registry
                .check_action("edit", &json!({"unexpected":true}))
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
        registry
            .register_with_options(
                info("panic"),
                ActionOptions::default().with_availability(|_, _| panic!("guard")),
                |_, _| unreachable!(),
            )
            .unwrap();
        assert_eq!(
            registry
                .check_action("panic", &json!({}))
                .unwrap()
                .reason
                .unwrap()
                .code,
            "availability_panicked"
        );
        assert_eq!(
            registry.invoke("panic", &json!({})).unwrap_err().code,
            "availability_panicked"
        );
        registry
            .register_with_options(
                info("oversize"),
                ActionOptions::default().with_availability(|_, _| {
                    Err(ActionError::new(&"界".repeat(100), "界".repeat(6000)))
                }),
                |_, _| unreachable!(),
            )
            .unwrap();
        let reason = registry
            .check_action("oversize", &json!({}))
            .unwrap()
            .reason
            .unwrap();
        assert!(reason.code.len() <= 128 && reason.message.len() <= 16384);
        assert_eq!(registry.observe().version, 2);
    }

    #[test]
    fn retiring_scope_during_admitted_calls_preserves_results_jobs_and_retry_kind() {
        use std::sync::mpsc;
        use std::time::Duration;
        let registry = ActionRegistry::new(json!({"name":"before"})).unwrap();
        let scope = registry.create_scope("form").unwrap();
        let (entered, started) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let blocked = Mutex::new(blocked);
        scope
            .register(info("write"), ActionOptions::default(), move |state, _| {
                entered.send(()).unwrap();
                blocked.lock().unwrap().recv().unwrap();
                state["name"] = json!("after");
                Ok(json!("after"))
            })
            .unwrap();
        let invocation = std::thread::spawn({
            let registry = registry.clone();
            move || registry.invoke_with_request_id("write", &json!({}), Some("write-1"))
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let (queued, observed) = mpsc::channel();
        let pending = std::thread::spawn({
            let registry = registry.clone();
            move || {
                queued.send(()).unwrap();
                registry.invoke("write", &json!({}))
            }
        });
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        scope.close();
        release.send(()).unwrap();
        assert_eq!(invocation.join().unwrap().unwrap().state["name"], "after");
        assert_eq!(pending.join().unwrap().unwrap_err().code, "unknown_action");
        assert_eq!(
            registry.invoke("write", &json!({})).unwrap_err().code,
            "unknown_action"
        );
        assert_eq!(
            registry
                .invoke_with_request_id("write", &json!({}), Some("write-1"))
                .unwrap()
                .state["name"],
            "after"
        );
        let job_scope = registry.create_scope("job").unwrap();
        let (release, blocked) = mpsc::channel();
        let blocked = Mutex::new(blocked);
        job_scope
            .register_operation(info("job"), ActionOptions::default(), move |_, _, _| {
                blocked.lock().unwrap().recv().unwrap();
                Ok(json!("finished"))
            })
            .unwrap();
        let accepted = registry
            .invoke_with_request_id("job", &json!({}), Some("job-1"))
            .unwrap();
        let id = accepted.result.unwrap()["operationId"]
            .as_str()
            .unwrap()
            .to_owned();
        job_scope.close();
        registry
            .register_query(info("job"), |_, _| Ok(json!("replacement")))
            .unwrap();
        let replay = registry
            .invoke_with_request_id("job", &json!({}), Some("job-1"))
            .unwrap();
        assert_eq!(replay.result.unwrap()["operationId"], id);
        assert_eq!(
            registry
                .trace(0)
                .records
                .last()
                .unwrap()
                .operation_id
                .as_deref(),
            Some(id.as_str())
        );
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = registry.operations().get(&id).unwrap();
            if snapshot.execution.terminal() {
                assert_eq!(snapshot.output, Some(json!("finished")));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            registry
                .operations()
                .wait(&id, snapshot.revision, Duration::from_millis(50))
                .unwrap();
        }
    }

    #[test]
    fn scoped_handler_destructors_run_without_state_lock_and_checks_reject_reentrancy() {
        let registry = ActionRegistry::new(json!({})).unwrap();
        struct ObservingDrop(Weak<Inner>, Arc<std::sync::atomic::AtomicBool>);
        impl Drop for ObservingDrop {
            fn drop(&mut self) {
                let inner = self.0.upgrade().unwrap();
                assert!(inner.state.try_lock().is_ok());
                self.1.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let marker = ObservingDrop(Arc::downgrade(&registry.inner), dropped.clone());
        let scope = registry.create_scope("drop").unwrap();
        scope
            .register_query(info("scoped"), ActionOptions::default(), move |_, _| {
                let _ = &marker;
                Ok(json!("ok"))
            })
            .unwrap();
        drop(scope);
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        let weak = Arc::downgrade(&registry.inner);
        registry
            .register_query_with_options(
                info("recursive"),
                ActionOptions::default().with_availability(move |_, _| {
                    let registry = ActionRegistry {
                        inner: weak.upgrade().unwrap(),
                    };
                    assert_eq!(
                        registry
                            .check_action("recursive", &json!({}))
                            .unwrap_err()
                            .code,
                        "reentrant_action"
                    );
                    assert_eq!(
                        registry.invoke("recursive", &json!({})).unwrap_err().code,
                        "reentrant_action"
                    );
                    assert_eq!(registry.observe().version, 0);
                    Ok(())
                }),
                |_, _| Ok(json!("ok")),
            )
            .unwrap();
        assert!(
            registry
                .check_action("recursive", &json!({}))
                .unwrap()
                .available
        );
        registry.invoke("recursive", &json!({})).unwrap();
    }

    fn follow(registry: &ActionRegistry, previous: &ChangePage, limit: usize) -> ChangePage {
        registry
            .subscribe_changes(ChangesRequest {
                scope: previous.scope,
                cursor: Some(previous.cursor.clone()),
                limit,
                wait_ms: 0,
            })
            .unwrap()
    }
    fn apply_delta(state: &mut Value, delta: &StateDelta) {
        match delta {
            StateDelta::Fields { set, remove } => {
                let fields = state.as_object_mut().unwrap();
                for key in remove {
                    fields.remove(key);
                }
                fields.extend(set.clone());
            }
            StateDelta::Replace { value } => *state = value.clone(),
        }
    }

    #[test]
    fn change_feed_reconstructs_committed_state_and_does_not_repeat_writes_on_retry_or_failure() {
        let registry = ActionRegistry::default();
        let baseline = registry
            .subscribe_changes(ChangesRequest {
                scope: ChangeScope::State,
                ..Default::default()
            })
            .unwrap();
        let mut state = baseline.baseline.as_ref().unwrap()["state"].clone();
        let mut version = baseline.baseline.as_ref().unwrap()["version"]
            .as_u64()
            .unwrap();
        registry
            .invoke_checked(
                "counter.increment",
                &json!({}),
                Some("same-request"),
                Some(0),
            )
            .unwrap();
        registry
            .invoke_checked(
                "counter.increment",
                &json!({}),
                Some("same-request"),
                Some(0),
            )
            .unwrap();
        assert_eq!(
            registry
                .invoke_checked("counter.increment", &json!({}), None, Some(0))
                .unwrap_err()
                .code,
            "stale_state"
        );
        registry.invoke("counter.increment", &json!({})).unwrap();
        let mut page = baseline;
        let mut changes = 0;
        loop {
            page = follow(&registry, &page, 1);
            for record in &page.records {
                let ChangeEvent::StateChanged {
                    base_version,
                    version: next,
                    delta,
                    requires_snapshot,
                    ..
                } = &record.event
                else {
                    panic!("unexpected event")
                };
                assert_eq!(*base_version, version);
                assert!(!requires_snapshot);
                apply_delta(&mut state, delta.as_ref().unwrap());
                version = *next;
                changes += 1;
            }
            if !page.has_more {
                break;
            }
        }
        assert_eq!(changes, 2);
        assert_eq!(state, registry.observe().state);
        assert_eq!(version, registry.observe().version);
        let actions = registry
            .subscribe_changes(ChangesRequest {
                scope: ChangeScope::Actions,
                ..Default::default()
            })
            .unwrap();
        registry
            .invoke_checked(
                "counter.increment",
                &json!({}),
                Some("same-request"),
                Some(0),
            )
            .unwrap();
        let replay = follow(&registry, &actions, 64);
        assert!(
            matches!(&replay.records[0].event, ChangeEvent::ActionFinished(trace) if trace.outcome == "replayed")
        );
        assert!(!serde_json::to_string(&replay)
            .unwrap()
            .contains("arguments"));
    }

    #[test]
    fn state_delta_handles_removed_keys_root_replacement_large_changes_and_resync_baselines() {
        let registry = ActionRegistry::new(json!({"old":true,"keep":"值"})).unwrap();
        let definition = ActionInfo {
            id: "replace".into(),
            description: "replace".into(),
            input_schema: json!({}),
            output_schema: json!({"type":"null"}),
            kind: ActionKind::Write,
        };
        registry
            .register(definition, |state, args| {
                *state = args.clone();
                Ok(Value::Null)
            })
            .unwrap();
        let mut page = registry
            .subscribe_changes(ChangesRequest {
                scope: ChangeScope::State,
                ..Default::default()
            })
            .unwrap();
        let mut reconstructed = page.baseline.as_ref().unwrap()["state"].clone();
        for next in [json!({"keep":"值","added":[1,2]}), json!(["root", 3])] {
            registry.invoke("replace", &next).unwrap();
            page = follow(&registry, &page, 64);
            let ChangeEvent::StateChanged {
                delta: Some(delta), ..
            } = &page.records[0].event
            else {
                panic!("missing delta")
            };
            apply_delta(&mut reconstructed, delta);
            assert_eq!(reconstructed, next);
        }
        registry
            .invoke("replace", &json!({"large":"x".repeat(5000)}))
            .unwrap();
        page = follow(&registry, &page, 64);
        assert!(matches!(
            &page.records[0].event,
            ChangeEvent::StateChanged {
                delta: None,
                requires_snapshot: true,
                ..
            }
        ));
        let refreshed = registry
            .subscribe_changes(ChangesRequest {
                scope: ChangeScope::State,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            refreshed.baseline.unwrap()["state"],
            registry.observe().state
        );
        let foreign = ActionRegistry::default()
            .subscribe_changes(ChangesRequest::default())
            .unwrap();
        let recovered = registry
            .subscribe_changes(ChangesRequest {
                cursor: Some(foreign.cursor),
                ..Default::default()
            })
            .unwrap();
        assert!(recovered.resync_required);
        assert_eq!(
            recovered.baseline.unwrap()["observation"]["state"],
            registry.observe().state
        );
    }

    #[test]
    fn operation_change_feed_tracks_revisions_cancellation_and_eviction_without_outputs() {
        use crate::operation::Execution;
        use std::time::Duration;
        let registry = ActionRegistry::new(json!({"name":"fixture"})).unwrap();
        let mut definition = info("job");
        definition.output_schema = json!({"type":"string"});
        registry
            .register_operation(definition, |_, _, context| {
                context.report(0.5, "hidden-progress-text")?;
                context.delay(Duration::from_secs(10))?;
                Ok(json!("hidden-output"))
            })
            .unwrap();
        let initial = registry
            .subscribe_changes(ChangesRequest {
                scope: ChangeScope::Operations,
                ..Default::default()
            })
            .unwrap();
        let accepted = registry
            .invoke_with_request_id("job", &json!({}), Some("job-once"))
            .unwrap();
        let id = accepted.result.unwrap()["operationId"]
            .as_str()
            .unwrap()
            .to_owned();
        let operations = registry.operations();
        let mut snapshot = operations.get(&id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while snapshot.progress != 0.5 {
            assert!(Instant::now() < deadline);
            snapshot = operations
                .wait(&id, snapshot.revision, Duration::from_millis(50))
                .unwrap();
        }
        snapshot = operations.cancel(&id).unwrap();
        while !snapshot.execution.terminal() {
            assert!(Instant::now() < deadline);
            snapshot = operations
                .wait(&id, snapshot.revision, Duration::from_millis(50))
                .unwrap();
        }
        assert_eq!(snapshot.execution, Execution::Cancelled);
        let page = follow(&registry, &initial, 128);
        assert!(!page.resync_required);
        let revisions: Vec<_> = page
            .records
            .iter()
            .filter_map(|record| match &record.event {
                ChangeEvent::OperationChanged(summary) => {
                    Some((summary.revision, summary.execution))
                }
                _ => None,
            })
            .collect();
        assert_eq!(revisions.first(), Some(&(0, Execution::Accepted)));
        assert_eq!(
            revisions.last(),
            Some(&(snapshot.revision, Execution::Cancelled))
        );
        assert!(revisions.windows(2).all(|pair| pair[0].0 < pair[1].0));
        let serialized = serde_json::to_string(&page).unwrap();
        assert!(
            !serialized.contains("hidden-progress-text") && !serialized.contains("hidden-output")
        );
        // Each new job is consumed before starting another, so only the final
        // terminal-record eviction is in this bounded page.
        let mut definition = info("done");
        definition.output_schema = json!({"type":"string"});
        registry
            .register_operation(definition, |_, _, _| Ok(json!("hidden-output")))
            .unwrap();
        let mut page = page;
        let mut saw_removed = false;
        for _ in 0..128 {
            let started = registry.invoke("done", &json!({})).unwrap();
            let next_id = started.result.unwrap()["operationId"]
                .as_str()
                .unwrap()
                .to_owned();
            let mut next = operations.get(&next_id).unwrap();
            while !next.execution.terminal() {
                next = operations
                    .wait(&next_id, next.revision, Duration::from_secs(1))
                    .unwrap();
            }
            assert_eq!(next.execution, Execution::Completed);
            page = follow(&registry, &page, 128);
            assert!(!page.resync_required);
            saw_removed |= page.records.iter().any(|record|matches!(&record.event,ChangeEvent::OperationRemoved {operation_id} if operation_id==&id));
            assert!(!serde_json::to_string(&page)
                .unwrap()
                .contains("hidden-output"));
        }
        assert!(saw_removed);
    }

    #[test]
    fn concurrent_state_and_operation_baselines_have_no_changes_before_their_checkpoint() {
        use std::sync::mpsc;
        use std::time::Duration;
        let registry = ActionRegistry::default();
        let mut definition = info("progress");
        definition.output_schema = json!({"type":"string"});
        let (release, released) = mpsc::channel();
        let released = Arc::new(Mutex::new(released));
        registry
            .register_operation(definition, move |_, _, context| {
                released.lock().unwrap().recv().unwrap();
                for step in 0..80 {
                    context.report(step as f64 / 80.0, "progress")?;
                    std::thread::yield_now();
                }
                Ok(json!("done"))
            })
            .unwrap();
        let accepted = registry.invoke("progress", &json!({})).unwrap();
        let operation_id = accepted.result.unwrap()["operationId"]
            .as_str()
            .unwrap()
            .to_owned();
        let writer = std::thread::spawn({
            let registry = registry.clone();
            move || {
                for _ in 0..100 {
                    registry.invoke("counter.increment", &json!({})).unwrap();
                    std::thread::yield_now();
                }
            }
        });
        release.send(()).unwrap();
        let mut baselines = Vec::new();
        for _ in 0..30 {
            baselines.push(
                registry
                    .subscribe_changes(ChangesRequest::default())
                    .unwrap(),
            );
            std::thread::yield_now();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while !writer.is_finished() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        writer.join().unwrap();
        let operations = registry.operations();
        let mut operation = operations.get(&operation_id).unwrap();
        while !operation.execution.terminal() {
            assert!(Instant::now() < deadline);
            operation = operations
                .wait(&operation_id, operation.revision, Duration::from_millis(50))
                .unwrap();
        }
        for mut page in baselines {
            let baseline = page.baseline.as_ref().unwrap();
            let mut reconstructed = baseline["observation"]["state"].clone();
            let mut version = baseline["observation"]["version"].as_u64().unwrap();
            let revisions: HashMap<_, _> = baseline["operations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|snapshot| {
                    (
                        snapshot["operationId"].as_str().unwrap().to_owned(),
                        snapshot["revision"].as_u64().unwrap(),
                    )
                })
                .collect();
            loop {
                page = follow(&registry, &page, 128);
                assert!(!page.resync_required);
                for record in &page.records {
                    match &record.event {
                        ChangeEvent::StateChanged {
                            base_version,
                            version: next,
                            delta,
                            ..
                        } => {
                            assert_eq!(*base_version, version);
                            apply_delta(&mut reconstructed, delta.as_ref().unwrap());
                            version = *next;
                        }
                        ChangeEvent::OperationChanged(summary) => {
                            assert!(summary.revision > revisions[&summary.operation_id]);
                        }
                        _ => {}
                    }
                }
                if !page.has_more {
                    break;
                }
            }
            assert_eq!(reconstructed, registry.observe().state);
            assert_eq!(version, 100);
        }
    }

    fn info(id: &str) -> ActionInfo {
        ActionInfo {
            id: id.into(),
            description: id.into(),
            input_schema: json!({"type":"object","additionalProperties":false}),
            output_schema: json!({"type":"string"}),
            kind: ActionKind::Write,
        }
    }

    #[test]
    fn queries_and_operations_do_not_write_state_and_accepted_retries_share_a_job() {
        let registry = ActionRegistry::new(json!({"name":"中文"})).unwrap();
        registry
            .register_query(info("name.get"), |state, _| Ok(state["name"].clone()))
            .unwrap();
        assert_eq!(registry.invoke("name.get", &json!({})).unwrap().version, 0);
        registry
            .register_operation(info("scan"), |state, _, context| {
                context.report(0.5, "scanning")?;
                context.delay(std::time::Duration::from_millis(10))?;
                Ok(state["name"].clone())
            })
            .unwrap();
        let first = registry
            .invoke_checked("scan", &json!({}), Some("scan-1"), Some(0))
            .unwrap();
        let retry = registry
            .invoke_checked("scan", &json!({}), Some("scan-1"), Some(0))
            .unwrap();
        assert_eq!(first.result, retry.result);
        assert_eq!(first.result.as_ref().unwrap()["execution"], "accepted");
        let id = first.result.as_ref().unwrap()["operationId"]
            .as_str()
            .unwrap();
        let operations = registry.operations();
        let mut job = operations.get(id).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while !job.execution.terminal() {
            assert!(Instant::now() < deadline);
            job = operations
                .wait(id, job.revision, std::time::Duration::from_millis(100))
                .unwrap();
        }
        assert_eq!(job.output, Some(json!("中文")));
        assert_eq!(registry.observe().version, 0);
        assert_eq!(registry.trace(0).records[1].outcome, "accepted");
        assert_eq!(
            registry.trace(0).records[1].operation_id.as_deref(),
            Some(id)
        );
        registry
            .register_operation(info("invalid"), |_, _, _| Ok(json!(123)))
            .unwrap();
        let accepted = registry.invoke("invalid", &json!({})).unwrap();
        let id = accepted.result.unwrap()["operationId"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut job = operations.get(&id).unwrap();
        while !job.execution.terminal() {
            assert!(Instant::now() < deadline);
            job = operations
                .wait(&id, job.revision, std::time::Duration::from_millis(100))
                .unwrap();
        }
        assert_eq!(job.error.unwrap().code, "invalid_output");
    }

    #[test]
    fn registered_transactions_validate_roll_back_and_survive_panics() {
        let registry = ActionRegistry::new(json!({"name":"中文", "revision":0})).unwrap();
        registry
            .register(info("bad.output"), |state, _| {
                state["name"] = json!("changed");
                Ok(json!(123))
            })
            .unwrap();
        registry
            .register(info("failed"), |state, _| {
                state["name"] = json!("changed");
                Err(ActionError::new("fixture_failure", "failed"))
            })
            .unwrap();
        registry
            .register(info("panicked"), |state, _| {
                state["name"] = json!("changed");
                panic!("fixture panic")
            })
            .unwrap();
        registry
            .register(info("oversized"), |state, _| {
                state["name"] = json!("x".repeat(MAX_STATE_BYTES));
                Ok(json!("ok"))
            })
            .unwrap();
        for (name, code) in [
            ("bad.output", "invalid_output"),
            ("failed", "fixture_failure"),
            ("panicked", "action_panicked"),
            ("oversized", "invalid_state"),
        ] {
            assert_eq!(registry.invoke(name, &json!({})).unwrap_err().code, code);
            assert_eq!(registry.observe().version, 0);
            assert_eq!(registry.observe().state["name"], "中文");
        }
        assert_eq!(
            registry
                .invoke("failed", &json!({"extra":1}))
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
        let observing = registry.clone();
        registry
            .register(info("valid"), move |state, _| {
                assert_eq!(observing.observe().version, 0);
                assert_eq!(
                    observing.invoke("failed", &json!({})).unwrap_err().code,
                    "reentrant_action"
                );
                state["name"] = json!("成功");
                Ok(json!("done"))
            })
            .unwrap();
        let result = registry
            .invoke_checked("valid", &json!({}), Some("valid-1"), Some(0))
            .unwrap();
        assert_eq!(result.result, Some(json!("done")));
        assert_eq!(result.state["name"], "成功");
        assert_eq!(result.version, 1);
        assert_eq!(
            registry
                .register(info("valid"), |_, _| Ok(json!("")))
                .unwrap_err()
                .code,
            "duplicate_action"
        );
        assert_eq!(
            registry.trace(0).records.last().unwrap().outcome,
            "completed"
        );
    }

    #[test]
    fn concurrent_same_request_is_committed_once_and_trace_cursors_resync() {
        let registry = ActionRegistry::default();
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let callers: Vec<_> = (0..16)
            .map(|_| {
                let registry = registry.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    registry
                        .invoke_checked("counter.increment", &json!({}), Some("same"), Some(0))
                        .unwrap()
                })
            })
            .collect();
        for caller in callers {
            assert_eq!(caller.join().unwrap().version, 1);
        }
        assert_eq!(registry.observe().count, 1);
        let trace = registry.trace(0);
        assert_eq!(
            trace
                .records
                .iter()
                .filter(|trace| trace.outcome == "completed")
                .count(),
            1
        );
        assert_eq!(
            trace
                .records
                .iter()
                .filter(|trace| trace.outcome == "replayed")
                .count(),
            15
        );
        for _ in 0..TRACE_CAPACITY {
            registry.invoke("missing", &json!({})).unwrap_err();
        }
        let page = registry.trace(0);
        assert!(page.resync_required);
        assert_eq!(page.records.len(), TRACE_CAPACITY);
        assert!(registry.trace(page.next_sequence).records.is_empty());
        assert!(!registry.trace(page.next_sequence).resync_required);
        assert!(registry.trace(page.next_sequence + 1).resync_required);
    }

    #[test]
    fn large_observations_evict_request_cache_by_bytes_and_options_are_strict() {
        let registry = ActionRegistry::new(json!({"large":"x".repeat(512 * 1024)})).unwrap();
        registry
            .register(info("noop"), |_, _| Ok(json!("ok")))
            .unwrap();
        for index in 0..12 {
            registry
                .invoke_with_request_id("noop", &json!({}), Some(&format!("request-{index}")))
                .unwrap();
        }
        let state = registry.inner.state.lock().unwrap();
        assert!(state.request_bytes <= REQUEST_CACHE_BYTES);
        assert!(state.request_order.len() < 12);
        assert!(!state.request_cache.contains_key("request-0"));
        drop(state);
        assert_eq!(
            registry
                .invoke_with_request_id("noop", &json!({}), Some("request-11"))
                .unwrap()
                .version,
            12
        );
        for value in [
            json!({"requestId":null}),
            json!({"expectedVersion":-1}),
            json!({"expectedVersion":1.5}),
            json!({"unexpected":true}),
            json!([]),
        ] {
            assert!(InvokeOptions::parse(&value).is_err());
        }
    }

    #[test]
    fn action_has_shared_validation_and_versioned_observation() {
        let registry = ActionRegistry::default();
        assert_eq!(registry.observe().count, 0);
        assert!(registry
            .invoke("counter.increment", &json!({"unexpected":1}))
            .is_err());
        let result = registry.invoke("counter.increment", &json!({})).unwrap();
        assert_eq!((result.count, result.version), (1, 1));
        assert_eq!(registry.observe().count, 1);
    }

    #[test]
    fn request_id_deduplicates_retries_and_rejects_payload_reuse() {
        let registry = ActionRegistry::default();
        let first = registry
            .invoke_with_request_id("counter.increment", &json!({}), Some("turn-7"))
            .unwrap();
        let retry = registry
            .invoke_with_request_id("counter.increment", &json!({}), Some("turn-7"))
            .unwrap();
        assert_eq!(first.version, 1);
        assert_eq!(retry.version, 1);
        assert_eq!(registry.observe().count, 1);
        let conflict = registry
            .invoke_with_request_id("other.action", &json!({}), Some("turn-7"))
            .unwrap_err();
        assert_eq!(conflict.code, "request_id_conflict");
        let conflict = registry
            .invoke_with_request_id(
                "counter.increment",
                &json!({"different":true}),
                Some("turn-7"),
            )
            .unwrap_err();
        assert_eq!(conflict.code, "request_id_conflict");
    }

    #[test]
    fn expected_version_prevents_stale_writes() {
        let registry = ActionRegistry::default();
        let first = registry
            .invoke_checked("counter.increment", &json!({}), Some("write-1"), Some(0))
            .unwrap();
        assert_eq!(first.version, 1);
        let retry = registry
            .invoke_checked("counter.increment", &json!({}), Some("write-1"), Some(0))
            .unwrap();
        assert_eq!(retry.version, 1);
        let changed_precondition = registry
            .invoke_checked("counter.increment", &json!({}), Some("write-1"), Some(1))
            .unwrap_err();
        assert_eq!(changed_precondition.code, "request_id_conflict");
        let error = registry
            .invoke_checked("counter.increment", &json!({}), None, Some(0))
            .unwrap_err();
        assert_eq!(error.code, "stale_state");
        assert_eq!(registry.observe().count, 1);
    }
}
