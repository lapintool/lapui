use crate::action::{ActionError, ActionRegistry, ActionScope, InvokeOptions};
use crate::control::{self, DocumentController, DocumentRequest};
use crate::debug_trace::{DebugTrace, Span};
use crate::fetch_work::{FetchRequest, FetchWork};
use crate::frames::Frames;
use crate::host_work::HostWork;
use crate::lifecycle::Cancellation;
use crate::script_budget::{
    ScriptBudget, CALLBACK_LIMIT, CHECKPOINT_SLICE, INTERRUPTED_MESSAGE, STARTUP_LIMIT,
};
use crate::scripts::{self, ScriptDiagnostics, StartupScript};
use crate::stream_work::{Data as StreamData, StreamWork};
use crate::timers::Timers;
use blitz::dom::{
    BaseDocument, DocGuard, DocGuardMut, Document, DocumentConfig, EventDriver, EventHandler,
    FontContext, LocalName, QualName,
};
use blitz::html::HtmlDocument;
use blitz::shell::{BlitzShellEvent, BlitzShellProxy};
use blitz::traits::events::{DomEvent, EventState, UiEvent};
use blitz::traits::net::{Bytes, NetHandler, NetProvider, Request, Url};
use blitz::traits::node_id::NodeId;
use fontique::Blob;
use rquickjs::{
    context::EvalOptions, function::Func, Context, Exception, FromJs, Function, Module, Runtime,
};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::task::{Context as TaskContext, Waker};
use std::time::{Duration, Instant};
use style::servo_arc::Arc as ServoArc;

const HTML: &str = include_str!("../ui/index.html");
const SCRIPT: &str = include_str!("../ui/app.js");
const BRIDGE: &str = include_str!("../ui/bridge.js");
const RESIZE_OBSERVER: &str = include_str!("../ui/resize-observer.js");
const INTERSECTION_OBSERVER: &str = include_str!("../ui/intersection-observer.js");
const FORM_BRIDGE: &str = include_str!("../ui/forms.js");
const MUTATION_OBSERVER: &str = include_str!("../ui/mutation-observer.js");

struct LocalDirectoryNetProvider {
    root: PathBuf,
}

fn read_local_resource(root: &Path, request: &Request) -> Result<Vec<u8>, String> {
    if request.method != blitz::traits::net::Method::GET || request.url.scheme() != "file" {
        return Err("only local file GET requests are supported".into());
    }
    let path = request
        .url
        .to_file_path()
        .map_err(|_| "resource URL could not be converted to a local path")?
        .canonicalize()
        .map_err(|error| format!("resource file could not be resolved: {error}"))?;
    if !path.starts_with(root) {
        return Err("resource path is outside the application directory".into());
    }
    if request
        .signal
        .as_ref()
        .is_some_and(|signal| signal.aborted())
    {
        return Err("resource request was cancelled".into());
    }
    let metadata = std::fs::metadata(&path)
        .map_err(|error| format!("resource metadata is unavailable: {error}"))?;
    if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
        return Err("resource is not a file or exceeds 32 MiB".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(|error| error.to_string())?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("resource read failed: {error}"))?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("resource exceeds 32 MiB".into());
    }
    Ok(bytes)
}

impl NetProvider for LocalDirectoryNetProvider {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let root = self.root.clone();
        std::thread::spawn(move || {
            if request
                .signal
                .as_ref()
                .is_some_and(|signal| signal.aborted())
            {
                return;
            }
            let url = request.url.to_string();
            let bytes = match read_local_resource(&root, &request) {
                Ok(bytes) => bytes,
                Err(error) => {
                    eprintln!("Lapui local resource failed for {url}: {error}");
                    Vec::new()
                }
            };
            handler.bytes(url, Bytes::from(bytes));
        });
    }
}

struct Completion {
    trace_origin: Option<(u64, u64)>,
    request_id: Option<i32>,
    error_code: Option<String>,
    result: Result<Value, String>,
}

#[derive(Clone)]
enum DomMutation {
    Text(String, String),
    Style(String, String, String),
    RemoveStyle(String, String),
}

pub(crate) fn resolve_node_ref(doc: &BaseDocument, reference: &str) -> Option<NodeId> {
    if let Some(raw) = reference.strip_prefix("node:") {
        let (document_id, raw) = raw.split_once(':')?;
        if document_id.parse::<usize>().ok()? != doc.id() {
            return None;
        }
        return raw
            .parse::<u64>()
            .ok()
            .map(NodeId::from_u64)
            .filter(|node| doc.get_node(*node).is_some());
    }
    doc.get_element_by_id(reference)
}

pub(crate) fn canonical_node_ref(document_id: usize, node: NodeId) -> String {
    format!("node:{document_id}:{}", node.as_u64())
}

fn detached_root(doc: &BaseDocument, node: NodeId) -> Option<NodeId> {
    let mut current = node;
    let mut visited = HashSet::new();
    loop {
        if current == doc.root_node().id || !visited.insert(current) {
            return None;
        }
        match doc.get_node(current)?.parent {
            Some(parent) => current = parent,
            None => return Some(current),
        }
    }
}

fn can_insert_node(doc: &BaseDocument, parent: NodeId, child: NodeId) -> bool {
    if child == doc.root_node().id
        || doc
            .get_node(child)
            .is_none_or(|node| matches!(node.data, blitz::dom::NodeData::Document(_)))
        || doc.get_node(parent).is_none_or(|node| {
            !matches!(
                node.data,
                blitz::dom::NodeData::Document(_) | blitz::dom::NodeData::Element(_)
            )
        })
    {
        return false;
    }
    let mut current = Some(parent);
    let mut visited = HashSet::new();
    while let Some(node) = current {
        if node == child || !visited.insert(node) {
            return false;
        }
        current = doc.get_node(node).and_then(|node| node.parent);
    }
    true
}

fn javascript_error(context: &Context, error: &rquickjs::Error, budget: &ScriptBudget) -> String {
    if budget.interrupted() {
        context.with(|ctx| {
            let _ = ctx.catch();
        });
        return INTERRUPTED_MESSAGE.into();
    }
    if !error.is_exception() {
        return error.to_string();
    }
    let detail = budget.run(CALLBACK_LIMIT, || {
        context.with(|ctx| {
            let thrown = ctx.catch();
            if let Some(exception) = thrown
                .clone()
                .into_object()
                .and_then(Exception::from_object)
            {
                format!(
                    "{}\n{}",
                    exception.message().unwrap_or_default(),
                    exception.stack().unwrap_or_default()
                )
            } else {
                rquickjs::Coerced::<String>::from_js(&ctx, thrown)
                    .map(|value| value.0)
                    .unwrap_or_else(|_| error.to_string())
            }
        })
    });
    if budget.interrupted() {
        INTERRUPTED_MESSAGE.into()
    } else {
        detail
    }
}

fn shallow_clone_node(doc: &mut BaseDocument, source: NodeId) -> Option<NodeId> {
    let mut data = doc.get_node(source)?.data.clone();
    if let blitz::dom::NodeData::Element(element) | blitz::dom::NodeData::AnonymousBlock(element) =
        &mut data
    {
        if let Some(style) = element.style_attribute.as_mut() {
            let guard = doc.guard();
            let read = guard.read();
            *style = ServoArc::new(guard.wrap(style.read_with(&read).clone()));
        }
    }
    Some(doc.create_node(data))
}

fn serialized_inline_style(doc: &BaseDocument, reference: &str) -> String {
    let Some(node) = resolve_node_ref(doc, reference).and_then(|id| doc.get_node(id)) else {
        return String::new();
    };
    let Some(element) = node.data.downcast_element() else {
        return String::new();
    };
    let Some(style) = element.style_attribute.as_ref() else {
        return String::new();
    };
    let guard = doc.guard().read();
    let style = style.read_with(&guard);
    let mut serialized = String::new();
    let _ = style.to_css(&mut serialized);
    serialized
}

fn escape_html(value: &str, attribute: bool) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' if attribute => escaped.push_str("&quot;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn serialize_html_node(doc: &BaseDocument, node_id: NodeId, output: &mut String) {
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    if node.is_text_node() {
        output.push_str(&escape_html(&node.text_content(), false));
        return;
    }
    if let blitz::dom::NodeData::Comment { contents } = &node.data {
        output.push_str("<!--");
        output.push_str(&contents.replace("-->", "--&gt;"));
        output.push_str("-->");
        return;
    }
    let Some(element) = node.data.downcast_element() else {
        return;
    };
    let tag = element.name.local.to_string();
    output.push('<');
    output.push_str(&tag);
    for attribute in element.attrs.iter() {
        output.push(' ');
        output.push_str(attribute.name.local.as_ref());
        output.push_str("=\"");
        output.push_str(&escape_html(&attribute.value, true));
        output.push('"');
    }
    output.push('>');
    if matches!(
        tag.as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    ) {
        return;
    }
    for child in node.children.iter().copied() {
        serialize_html_node(doc, child, output);
    }
    output.push_str("</");
    output.push_str(&tag);
    output.push('>');
}

fn serialized_inner_html(doc: &BaseDocument, reference: &str) -> String {
    let Some(node) = resolve_node_ref(doc, reference).and_then(|id| doc.get_node(id)) else {
        return String::new();
    };
    let mut output = String::new();
    for child in node.children.iter().copied() {
        serialize_html_node(doc, child, &mut output);
    }
    output
}

#[derive(Default)]
struct MutationBatch {
    depth: usize,
    pending: Vec<DomMutation>,
}

fn apply_mutations(dom: &Rc<RefCell<BaseDocument>>, mutations: Vec<DomMutation>) {
    if mutations.is_empty() {
        return;
    }
    let mut doc = dom.borrow_mut();
    let mut mutator = doc.mutate();
    for mutation in mutations {
        match mutation {
            DomMutation::Text(id, value) => {
                if let Some(node) = resolve_node_ref(mutator.doc, &id) {
                    if mutator
                        .doc
                        .get_node(node)
                        .is_some_and(|node| node.is_text_node())
                    {
                        if mutator
                            .doc
                            .get_node(node)
                            .is_some_and(|node| node.text_content() != value)
                        {
                            mutator.set_node_text(node, &value);
                        }
                        continue;
                    }
                    if mutator.doc.get_node(node).is_some_and(|node| {
                        matches!(&node.data, blitz::dom::NodeData::Comment { .. })
                    }) {
                        if let Some(blitz::dom::NodeData::Comment { contents }) =
                            mutator.doc.get_node_mut(node).map(|node| &mut node.data)
                        {
                            *contents = value;
                        }
                        continue;
                    }
                    if value.is_empty() {
                        if mutator
                            .doc
                            .get_node(node)
                            .is_some_and(|node| !node.children.is_empty())
                        {
                            mutator.replace_children(node, &[]);
                        }
                        continue;
                    }
                    let text_child = mutator.doc.get_node(node).and_then(|node| {
                        (node.children.len() == 1)
                            .then(|| node.children[0])
                            .filter(|&child| {
                                mutator
                                    .doc
                                    .get_node(child)
                                    .is_some_and(|node| node.is_text_node())
                            })
                    });
                    if let Some(text_child) = text_child {
                        if mutator
                            .doc
                            .get_node(text_child)
                            .is_some_and(|n| n.text_content() != value)
                        {
                            mutator.set_node_text(text_child, &value);
                        }
                    } else {
                        let text = mutator.create_text_node(&value);
                        mutator.replace_children(node, &[text]);
                    }
                }
            }
            DomMutation::Style(id, name, value) => {
                if let Some(node) = resolve_node_ref(mutator.doc, &id) {
                    mutator.set_style_property(node, &name, &value);
                }
            }
            DomMutation::RemoveStyle(id, name) => {
                if let Some(node) = resolve_node_ref(mutator.doc, &id) {
                    mutator.remove_style_property(node, &name);
                }
            }
        }
    }
}

fn control_snapshot(doc: &BaseDocument) -> Value {
    control_snapshot_filtered(doc, None)
}

fn control_snapshot_for_nodes(doc: &BaseDocument, nodes: &[NodeId]) -> Value {
    let included = nodes.iter().copied().collect::<HashSet<_>>();
    control_snapshot_filtered(doc, Some(&included))
}

fn control_snapshot_filtered(doc: &BaseDocument, included: Option<&HashSet<NodeId>>) -> Value {
    fn attr<'a>(element: &'a blitz::dom::ElementData, name: &str) -> Option<&'a str> {
        element
            .attrs
            .iter()
            .find(|attribute| attribute.name.local.to_string() == name)
            .map(|attribute| attribute.value.as_str())
    }
    fn sensitive_value(element: &blitz::dom::ElementData, tag: &str, input_type: &str) -> bool {
        if tag == "input" && input_type == "password" {
            return true;
        }
        const SENSITIVE_AUTOCOMPLETE: &[&str] = &[
            "current-password",
            "new-password",
            "one-time-code",
            "cc-number",
            "cc-csc",
            "cc-exp",
            "cc-exp-month",
            "cc-exp-year",
        ];
        attr(element, "autocomplete").is_some_and(|value| {
            value.split_ascii_whitespace().any(|token| {
                SENSITIVE_AUTOCOMPLETE
                    .iter()
                    .any(|sensitive| token.eq_ignore_ascii_case(sensitive))
            })
        })
    }
    fn visit(
        doc: &BaseDocument,
        id: NodeId,
        controls: &mut Vec<Value>,
        labels: &HashMap<NodeId, String>,
        included: Option<&HashSet<NodeId>>,
    ) {
        let Some(node) = doc.get_node(id) else { return };
        if let Some(element) = node.data.downcast_element() {
            let tag = element.name.local.to_string();
            let input_type = attr(element, "type").unwrap_or("").to_ascii_lowercase();
            let role = attr(element, "role")
                .map(str::to_owned)
                .or_else(|| match (tag.as_str(), input_type.as_str()) {
                    ("button", _) | ("input", "button" | "submit" | "reset" | "image") => {
                        Some("button".into())
                    }
                    ("input", "checkbox") => Some("checkbox".into()),
                    ("input", "radio") => Some("radio".into()),
                    ("input", "range") => Some("slider".into()),
                    ("input", "number") => Some("spinbutton".into()),
                    ("input", "hidden") => None,
                    ("input", _) => Some("textbox".into()),
                    ("textarea", _) => Some("textbox".into()),
                    _ => None,
                })
                .or_else(|| match tag.as_str() {
                    "textarea" => Some("textbox".into()),
                    "select" => Some("combobox".into()),
                    "a" if attr(element, "href").is_some() => Some("link".into()),
                    _ => None,
                });
            let hidden =
                attr(element, "hidden").is_some() || attr(element, "aria-hidden") == Some("true");
            if let Some(role) =
                role.filter(|_| !hidden && included.is_none_or(|included| included.contains(&id)))
            {
                let html_id = attr(element, "id");
                let name = attr(element, "aria-labelledby")
                    .and_then(|references| {
                        let names: Vec<_> = references
                            .split_ascii_whitespace()
                            .filter_map(|id| {
                                doc.get_element_by_id(id).and_then(|id| doc.get_node(id))
                            })
                            .map(|node| node.text_content().trim().to_owned())
                            .filter(|text| !text.is_empty())
                            .collect();
                        (!names.is_empty()).then(|| names.join(" "))
                    })
                    .or_else(|| {
                        attr(element, "aria-label")
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned)
                    })
                    .or_else(|| labels.get(&id).cloned())
                    .or_else(|| {
                        if tag == "input"
                            && matches!(input_type.as_str(), "button" | "submit" | "reset")
                        {
                            attr(element, "value").map(str::to_owned)
                        } else {
                            let text = node.text_content().trim().to_owned();
                            (!text.is_empty()).then_some(text)
                        }
                    })
                    .or_else(|| attr(element, "title").map(str::to_owned))
                    .unwrap_or_default();
                let disabled = !crate::forms::enabled(doc, id);
                let mut control = json!({
                    "ref": canonical_node_ref(doc.id(), id),
                    "id": html_id.map(str::to_owned),
                    "tag": tag,
                    "role": role,
                    "name": name,
                    "focused": doc.get_focussed_node_id() == Some(id),
                    "enabled": !disabled,
                    "type": input_type.clone(),
                    "required": attr(element, "required").is_some() || attr(element, "aria-required") == Some("true"),
                    "readOnly": attr(element, "readonly").is_some() || attr(element, "aria-readonly") == Some("true"),
                });
                if matches!(tag.as_str(), "input" | "textarea" | "select") {
                    if let Some(placeholder) = attr(element, "placeholder") {
                        control["placeholder"] = json!(placeholder);
                    }
                    if matches!(input_type.as_str(), "checkbox" | "radio") {
                        control["checked"] = json!(element
                            .checkbox_input_checked()
                            .unwrap_or_else(|| attr(element, "checked").is_some()));
                    }
                }
                if !sensitive_value(element, &tag, &input_type) {
                    let value = crate::forms::value(doc, id);
                    if let Some(value) = value {
                        control["value"] = json!(value);
                    }
                }
                if let Some(selected) = attr(element, "aria-selected").and_then(|value| match value
                {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                }) {
                    control["selected"] = json!(selected);
                }
                controls.push(control);
            }
        }
        for child in node.children.iter().copied() {
            visit(doc, child, controls, labels, included);
        }
    }
    let mut controls = Vec::new();
    visit(
        doc,
        doc.root_node().id,
        &mut controls,
        &crate::forms::label_names(doc),
        included,
    );
    json!({ "documentEpoch": doc.id(), "controls": controls })
}

pub struct LapuiDocument {
    debug_trace: DebugTrace,
    actions: ActionRegistry,
    action_scopes: Vec<ActionScope>,
    dom: Rc<RefCell<BaseDocument>>,
    js_runtime: Runtime,
    js_context: Context,
    script_budget: ScriptBudget,
    script_stopped: Cell<bool>,
    completions: mpsc::Receiver<Completion>,
    waker: Arc<Mutex<Option<Waker>>>,
    streams: Rc<RefCell<Option<StreamWork>>>,
    fetch_requests: Arc<Mutex<HashMap<i32, Cancellation>>>,
    controller: DocumentController,
    control_requests: mpsc::Receiver<DocumentRequest>,
    script_diagnostics: ScriptDiagnostics,
    lifetime: Cancellation,
    forward_shutdown: mpsc::Sender<Result<Value, String>>,
    timers: Timers,
    proxy: Option<BlitzShellProxy>,
    gc_requested: Rc<Cell<bool>>,
    frames: Rc<RefCell<Frames>>,
}

impl Drop for LapuiDocument {
    fn drop(&mut self) {
        self.controller.page_change_notifier().close();
        self.lifetime.cancel();
        self.frames.borrow_mut().stop();
        self.action_scopes.clear();
        let _ = self.forward_shutdown.send(Err("document closed".into()));
    }
}

impl LapuiDocument {
    /// Run animation callbacks at one rendering opportunity, before layout/paint.
    /// Window embedders should use LapuiApplication. Headless embedders call this
    /// themselves after establishing a viewport and before resolving the frame.
    /// Callback completion does not acknowledge physical screen presentation.
    pub fn animation_frame(&mut self) -> bool {
        let time = self.frames.borrow().now() / 1000.0;
        self.animation_frame_at(time)
    }

    /// Supply the embedding renderer's CSS animation time in seconds. JS frame
    /// timestamps still use the document's independent performance clock.
    pub fn animation_frame_at(&mut self, layout_time: f64) -> bool {
        if layout_time.is_finite() && layout_time >= 0.0 {
            self.frames.borrow_mut().layout_time = layout_time;
        }
        let callback_trace = self
            .debug_trace
            .span("animation_callbacks", json!({}), None);
        self.resume_pending_jobs();
        if self.script_budget.interrupted() {
            return false;
        }
        self.frames.borrow_mut().in_frame = true;
        let (timestamp, callbacks) = self.frames.borrow().snapshot();
        let deadline = Instant::now() + CHECKPOINT_SLICE;
        let mut changed = false;
        for id in callbacks {
            if self.script_budget.interrupted() || Instant::now() >= deadline {
                break;
            }
            if !self.frames.borrow_mut().take(id) {
                continue;
            }
            let result = self.script_budget.run(CALLBACK_LIMIT, || {
                self.js_context.with(|ctx| {
                    let function: Function = ctx.globals().get("__lapui_fire_animation_frame")?;
                    function.call::<_, ()>((id, timestamp))
                })
            });
            if let Err(error) = result {
                scripts::report(
                    &self.script_diagnostics,
                    &format!("animation-frame:{id}"),
                    "animation-frame",
                    javascript_error(&self.js_context, &error, &self.script_budget),
                );
            }
            if let Err(error) = drain_jobs(&self.js_runtime, &self.script_budget) {
                scripts::report(&self.script_diagnostics, "microtask", "evaluate", error);
            }
            changed = true;
        }
        self.frames.borrow_mut().in_frame = false;
        self.stop_faulted_script();
        callback_trace.finish(
            json!({"callbacksRan":changed,"scriptSuspended":self.script_budget.interrupted()}),
            false,
        );
        changed
    }

    /// Deliver window/scroll notifications and native-size observations after
    /// animation callbacks, before painting. Custom/offscreen hosts call this
    /// once per rendering opportunity; bare polling does not deliver observers.
    pub fn rendering_update(&mut self) -> bool {
        self.rendering_update_at(self.layout_animation_time())
    }

    pub fn rendering_update_at(&mut self, layout_time: f64) -> bool {
        if layout_time.is_finite() && layout_time >= 0.0 {
            self.frames.borrow_mut().layout_time = layout_time;
        }
        let observer_trace = self
            .debug_trace
            .span("rendering_notifications", json!({}), None);
        self.resume_pending_jobs();
        if self.script_budget.interrupted() {
            return false;
        }
        {
            let mut frames = self.frames.borrow_mut();
            frames.in_frame = true;
            frames.rendering_pending = false;
        }
        let result = self.script_budget.run(CALLBACK_LIMIT, || {
            self.js_context.with(|ctx| {
                let function: Function = ctx.globals().get("__lapui_rendering_update")?;
                function.call::<_, bool>(())
            })
        });
        let changed = match result {
            Ok(changed) => changed,
            Err(error) => {
                scripts::report(
                    &self.script_diagnostics,
                    "rendering-update",
                    "rendering-update",
                    javascript_error(&self.js_context, &error, &self.script_budget),
                );
                false
            }
        };
        if let Err(error) = drain_jobs(&self.js_runtime, &self.script_budget) {
            scripts::report(&self.script_diagnostics, "microtask", "evaluate", error);
        }
        self.frames.borrow_mut().in_frame = false;
        self.stop_faulted_script();
        observer_trace.finish(
            json!({"changed":changed,"scriptSuspended":self.script_budget.interrupted()}),
            false,
        );
        changed
    }

    pub(crate) fn defer_rendering_update(&mut self) {
        self.frames.borrow_mut().rendering_pending = true;
    }

    pub fn has_pending_rendering_update(&self) -> bool {
        self.frames.borrow().rendering_pending
    }

    pub fn has_animation_callbacks(&self) -> bool {
        self.frames.borrow().is_pending()
    }

    /// Last sampled embedding CSS animation time, in seconds, for resolve/paint.
    pub fn layout_animation_time(&self) -> f64 {
        self.frames.borrow().layout_time
    }

    /// Keep a Rust action scope with this document. Full replacement/drop
    /// retires these actions; application registrations remain independent.
    pub fn create_action_scope(&mut self, name: &str) -> Result<&ActionScope, ActionError> {
        self.action_scopes.push(self.actions.create_scope(name)?);
        Ok(self.action_scopes.last().unwrap())
    }

    /// Transfer a pre-registered scope after construction, allowing startup
    /// handlers before script loading. A failed transfer drops the supplied
    /// scope; only live scopes belonging to this registry can be attached.
    pub fn attach_action_scope(&mut self, scope: ActionScope) -> Result<(), ActionError> {
        if !scope.belongs_to(&self.actions) {
            return Err(ActionError::new(
                "invalid_scope",
                "scope is closed or belongs to another action registry",
            ));
        }
        self.action_scopes.push(scope);
        Ok(())
    }

    pub fn debug_trace_enabled(&self) -> bool {
        self.debug_trace.enabled()
    }
    pub fn configure_debug_trace(&self, enabled: bool, clear: bool) {
        self.debug_trace.configure(enabled, clear);
        if enabled {
            self.dom.borrow().shell_provider.request_redraw();
        }
    }
    pub fn debug_trace(&self, after_sequence: u64, limit: usize) -> Result<Value, ActionError> {
        let mut page = self.debug_trace.read(after_sequence, limit)?;
        page["documentEpoch"] = json!(self.dom.borrow().id());
        Ok(page)
    }
    pub fn debug_trace_limits() -> Value {
        DebugTrace::limits()
    }
    pub(crate) fn frame_trace(&self, target: &'static str) -> Span {
        if !self.debug_trace.enabled() {
            return self.debug_trace.span("frame", Value::Null, None);
        }
        let (causes, lost) = self.debug_trace.take_causes();
        self.debug_trace.span(
            "frame",
            json!({"target":target,"causes":causes,"causesTruncated":lost,
            "observedStateVersion":self.actions.version(),"physicalPresentation":"unknown"}),
            None,
        )
    }
    pub(crate) fn layout_trace(&self) -> Span {
        self.debug_trace.span("layout", json!({}), None)
    }

    /// Cooperative script limits advertised by the development control protocol.
    pub fn script_limits() -> Value {
        json!({
            "heapBytes": crate::script_budget::HEAP_LIMIT,
            "callbackMillis": CALLBACK_LIMIT.as_millis(),
            "startupMillis": STARTUP_LIMIT.as_millis(),
            "checkpointSliceMillis": CHECKPOINT_SLICE.as_millis(),
            "animationCallbacks": crate::frames::MAX_CALLBACKS,
            "animationSliceMillis": CHECKPOINT_SLICE.as_millis(),
            "renderingObserverLimits": Self::rendering_limits(),
            "formLimits": Self::form_limits(),
            "cooperative": true,
            "interruption": "suspendUntilReload"
        })
    }

    /// String-based local-form bounds, measured in UTF-16 code units.
    pub fn form_limits() -> Value {
        json!({"patternCodeUnits":1024,"patternValueCodeUnits":65536,
            "customMessageCodeUnits":4096,"formDataEntries":1024,"formDataCodeUnits":2097152})
    }

    /// Preview observation bounds; not a hard frame or native-layout budget.
    pub fn rendering_limits() -> Value {
        json!({"resizeObservers":128,"resizeTargets":1024,"scrollTargets":1024,
            "intersectionObservers":128,"intersectionTargets":1024,"intersectionThresholds":256,"intersectionMarginPx":1000000,
            "mutationObservers":128,"mutationTargets":1024,"mutationRecords":4096,
            "deliveryPasses":32,"deliverySliceMillis":CHECKPOINT_SLICE.as_millis()})
    }

    /// Bounds of the document-owned WebSocket/SSE transport, not total heap use.
    pub fn stream_limits() -> Value {
        json!({"outstanding":crate::stream_work::MAX_STREAMS,
            "queuedEvents":crate::stream_work::MAX_EVENTS,
            "eventByteBudget":crate::stream_work::EVENT_BYTES,
            "outgoingPerSocket":crate::stream_work::MAX_OUTGOING,
            "outgoingByteBudget":crate::stream_work::OUTGOING_BYTES,
            "webSocketMessageBytes":crate::stream_work::MAX_MESSAGE,
            "urlBytes":crate::stream_work::MAX_URL,
            "sseLineBytes":crate::stream_work::SSE_LINE,
            "sseDataBytes":crate::stream_work::SSE_DATA,
            "sseFieldBytes":1024,"closeFlushMillis":1000,
            "deliveriesPerPoll":16,"deliverySliceMillis":CHECKPOINT_SLICE.as_millis()})
    }

    fn quickjs_memory_usage(runtime: &Runtime) -> Value {
        let usage = runtime.memory_usage();
        json!({
            "allocatorBytes": usage.malloc_size,
            "allocatorLimitBytes": usage.malloc_limit,
            "heapUsedBytes": usage.memory_used_size,
            "allocatorBlocks": usage.malloc_count,
            "heapObjects": usage.memory_used_count,
            "atoms": {"count":usage.atom_count,"bytes":usage.atom_size},
            "strings": {"count":usage.str_count,"bytes":usage.str_size},
            "objects": {"count":usage.obj_count,"bytes":usage.obj_size},
            "properties": {"count":usage.prop_count,"bytes":usage.prop_size},
            "shapes": {"count":usage.shape_count,"bytes":usage.shape_size},
            "javascriptFunctions": {"count":usage.js_func_count,"bytes":usage.js_func_size,"codeBytes":usage.js_func_code_size,"pc2lineCount":usage.js_func_pc2line_count,"pc2lineBytes":usage.js_func_pc2line_size},
            "cFunctions": usage.c_func_count,
            "arrays": {"count":usage.array_count,"fastCount":usage.fast_array_count,"fastElements":usage.fast_array_elements},
            "binaryObjects": {"count":usage.binary_object_count,"bytes":usage.binary_object_size}
        })
    }

    fn stop_faulted_script(&self) {
        if !self.script_budget.interrupted() || self.script_stopped.replace(true) {
            return;
        }
        self.lifetime.cancel();
        self.timers.handle.stop();
        self.frames.borrow_mut().stop();
        let _ = self
            .forward_shutdown
            .send(Err("scripting suspended".into()));
    }

    fn resume_pending_jobs(&self) {
        self.stop_faulted_script();
        if self.script_budget.interrupted() {
            return;
        }
        // Collect cycles once after a turn that detached nodes, never inside
        // a native DOM borrow or while an event handler is still executing.
        if self.gc_requested.replace(false) {
            self.js_runtime.run_gc();
            if let Err(message) = drain_jobs(&self.js_runtime, &self.script_budget) {
                scripts::report(&self.script_diagnostics, "dom-gc", "evaluate", message);
            }
        }
        self.stop_faulted_script();
        if !self.script_budget.interrupted() && self.js_runtime.is_job_pending() {
            wake_document(&self.waker, &self.proxy, self.dom.borrow().id());
        }
    }

    /// Returns a handle for structured requests from a background client.
    pub fn controller(&self) -> DocumentController {
        self.controller.clone()
    }

    fn semantic_controls(&self) -> Value {
        self.enrich_semantic_controls(control_snapshot(&self.dom.borrow()))
    }

    fn semantic_controls_for_nodes(&self, doc: &BaseDocument, nodes: &[NodeId]) -> Value {
        self.enrich_semantic_controls(control_snapshot_for_nodes(doc, nodes))
    }

    fn enrich_semantic_controls(&self, mut snapshot: Value) -> Value {
        if self.script_budget.interrupted() {
            snapshot["validationAvailable"] = json!(false);
            return snapshot;
        }
        let result = self.script_budget.run(CALLBACK_LIMIT, || {
            self.js_context.with(|ctx| {
                let function: Function = ctx.globals().get("__lapui_form_snapshot_json")?;
                function.call::<_, String>((snapshot.to_string(),))
            })
        });
        match result {
            Ok(value) => serde_json::from_str(&value).unwrap_or(snapshot),
            Err(error) => {
                scripts::report(
                    &self.script_diagnostics,
                    "controls-validation",
                    "control",
                    javascript_error(&self.js_context, &error, &self.script_budget),
                );
                snapshot["validationAvailable"] = json!(false);
                snapshot
            }
        }
    }

    fn execute_control_command(&mut self, request: &Value) -> Result<Value, ActionError> {
        let failure = |code: &str, message: &str| ActionError {
            code: code.into(),
            message: message.into(),
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("invalid_request", "missing document method"))?;
        if matches!(method, "debugTrace.read" | "debugTrace.configure") {
            let allowed = if method == "debugTrace.read" {
                &["method", "documentEpoch", "afterSequence", "limit"][..]
            } else {
                &["method", "documentEpoch", "enabled", "clear"][..]
            };
            if request
                .as_object()
                .unwrap()
                .keys()
                .any(|key| !allowed.contains(&key.as_str()))
            {
                return Err(failure("invalid_request", "unknown debug trace field"));
            }
            if request.get("documentEpoch").and_then(Value::as_u64)
                != Some(self.dom.borrow().id() as u64)
            {
                return Err(failure(
                    "stale_document",
                    "debug trace requires the current documentEpoch",
                ));
            }
            if method == "debugTrace.configure" {
                let enabled = request
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| failure("invalid_request", "enabled must be a boolean"))?;
                let clear = request
                    .get("clear")
                    .map(|value| {
                        value
                            .as_bool()
                            .ok_or_else(|| failure("invalid_request", "clear must be a boolean"))
                    })
                    .transpose()?
                    .unwrap_or(false);
                self.configure_debug_trace(enabled, clear);
                return self.debug_trace(0, 128);
            }
            let after = request
                .get("afterSequence")
                .map(|value| {
                    value.as_u64().ok_or_else(|| {
                        failure("invalid_request", "afterSequence must be nonnegative")
                    })
                })
                .transpose()?
                .unwrap_or(0);
            let limit = request
                .get("limit")
                .map(|value| {
                    value
                        .as_u64()
                        .ok_or_else(|| failure("invalid_request", "limit must be 1..128"))
                })
                .transpose()?
                .unwrap_or(64);
            if limit > 128 {
                return Err(failure("invalid_request", "limit must be 1..128"));
            }
            return self.debug_trace(after, limit as usize);
        }
        if method == "controls" {
            return Ok(self.semantic_controls());
        }
        if method == "scroll" {
            let allowed = ["method", "documentEpoch", "ref", "x", "y", "relative"];
            if request
                .as_object()
                .unwrap()
                .keys()
                .any(|key| !allowed.contains(&key.as_str()))
            {
                return Err(failure("invalid_request", "unknown scroll field"));
            }
            if request.get("documentEpoch").and_then(Value::as_u64)
                != Some(self.dom.borrow().id() as u64)
            {
                return Err(failure(
                    "stale_document",
                    "scroll requires the current documentEpoch",
                ));
            }
            let reference = request
                .get("ref")
                .and_then(Value::as_str)
                .ok_or_else(|| failure("invalid_request", "canonical element ref is required"))?;
            let id = resolve_node_ref(&self.dom.borrow(), reference).ok_or_else(|| {
                failure(
                    "stale_reference",
                    "scroll target is no longer in this document",
                )
            })?;
            if !crate::geometry::has_boxes(&self.dom.borrow(), id) {
                return Err(failure(
                    "control_unavailable",
                    "scroll target has no rendered box",
                ));
            }
            let coordinate = |name: &str| -> Result<Option<f64>, ActionError> {
                let Some(value) = request.get(name) else {
                    return Ok(None);
                };
                let value = value
                    .as_f64()
                    .filter(|value| value.is_finite() && value.abs() <= 100_000.0)
                    .ok_or_else(|| {
                        failure(
                            "invalid_request",
                            "scroll coordinates must be finite and within 100000 CSS pixels",
                        )
                    })?;
                Ok(Some(value))
            };
            let x = coordinate("x")?;
            let y = coordinate("y")?;
            if x.is_none() && y.is_none() {
                return Err(failure("invalid_request", "scroll requires x or y"));
            }
            let relative = match request.get("relative") {
                None => true,
                Some(Value::Bool(value)) => *value,
                Some(_) => return Err(failure("invalid_request", "relative must be a boolean")),
            };
            let mut dom = self.dom.borrow_mut();
            let scrolled = crate::geometry::scroll(&mut dom, id, x, y, relative);
            return Ok(
                json!({"documentEpoch":dom.id(),"ref":reference,"scrolled":scrolled,"metrics":crate::geometry::metrics(&dom,id)}),
            );
        }
        if method == "pageSnapshot" {
            return crate::ai_snapshot::page_snapshot(&self.dom.borrow(), request, |doc, nodes| {
                self.semantic_controls_for_nodes(doc, nodes)
            });
        }
        if method == "pageChanges" {
            if request.as_object().is_none_or(|fields| {
                fields.keys().any(|key| {
                    !["method", "documentEpoch", "cursor", "limit"].contains(&key.as_str())
                })
            }) {
                return Err(failure(
                    "invalid_request",
                    "pageChanges accepts only method, documentEpoch, cursor, and limit",
                ));
            }
            let document_id = self.dom.borrow().id();
            if request.get("documentEpoch").and_then(Value::as_u64) != Some(document_id as u64) {
                return Err(failure(
                    "stale_document",
                    "pageChanges requires the current documentEpoch",
                ));
            }
            let after_sequence = match request.get("cursor") {
                None => 0,
                Some(Value::String(cursor)) => {
                    let Some((prefix, sequence)) = cursor.rsplit_once(':') else {
                        return Err(failure("invalid_cursor", "invalid page change cursor"));
                    };
                    if prefix != format!("page:{document_id}") {
                        return Err(failure(
                            "stale_document",
                            "page change cursor belongs to a different document",
                        ));
                    }
                    sequence.parse::<u64>().map_err(|_| {
                        failure("invalid_cursor", "invalid page change cursor sequence")
                    })?
                }
                Some(_) => return Err(failure("invalid_cursor", "cursor must be a string")),
            };
            let limit = request
                .get("limit")
                .map(|value| {
                    value
                        .as_u64()
                        .ok_or_else(|| failure("invalid_request", "limit must be 1..64"))
                })
                .transpose()?
                .unwrap_or(32);
            if !(1..=64).contains(&limit) {
                return Err(failure("invalid_request", "limit must be 1..64"));
            }
            let encoded = self
                .script_budget
                .run(CALLBACK_LIMIT, || {
                    self.js_context.with(|ctx| {
                        let function: Function = ctx.globals().get("__lapui_page_changes")?;
                        function.call::<_, String>((after_sequence, limit))
                    })
                })
                .map_err(|error| {
                    failure(
                        "page_changes_unavailable",
                        &javascript_error(&self.js_context, &error, &self.script_budget),
                    )
                })?;
            let mut page: Value = serde_json::from_str(&encoded).map_err(|_| {
                failure(
                    "page_changes_unavailable",
                    "invalid page change journal result",
                )
            })?;
            let canonicalize = |value: &mut Value| {
                if let Some(raw) = value.as_str() {
                    if let Ok(raw) = raw.parse::<u64>() {
                        *value = json!(canonical_node_ref(document_id, NodeId::from_u64(raw)));
                    }
                }
            };
            if let Some(records) = page["records"].as_array_mut() {
                for record in records {
                    canonicalize(&mut record["target"]);
                    for key in ["added", "removed"] {
                        if let Some(nodes) = record[key].as_array_mut() {
                            for node in nodes {
                                canonicalize(node);
                            }
                        }
                    }
                }
            }
            let response_limit = 24 * 1024;
            while page.to_string().len() > response_limit
                && page["records"]
                    .as_array()
                    .is_some_and(|records| records.len() > 1)
            {
                page["records"].as_array_mut().unwrap().pop();
            }
            let original_next = page["nextSequence"].as_u64().unwrap_or(after_sequence);
            let next_sequence = if page["resyncRequired"] == true {
                original_next
            } else {
                page["records"]
                    .as_array()
                    .and_then(|records| records.last())
                    .and_then(|record| record["sequence"].as_u64())
                    .unwrap_or(after_sequence)
            };
            page["hasMore"] =
                json!(page["latestSequence"].as_u64().unwrap_or(next_sequence) > next_sequence);
            page["documentEpoch"] = json!(document_id);
            page["cursor"] = json!(format!("page:{document_id}:{next_sequence}"));
            if page.to_string().len() > response_limit {
                return Err(failure(
                    "response_too_large",
                    "a single page change record exceeds the 24 KiB result budget",
                ));
            }
            return Ok(page);
        }
        if method == "screenshot" {
            if request
                .as_object()
                .is_none_or(|fields| fields.keys().any(|key| key != "method"))
            {
                return Err(failure(
                    "invalid_request",
                    "screenshot accepts only the method field",
                ));
            }
            #[cfg(feature = "software-renderer")]
            {
                use base64::Engine;
                use image::ImageEncoder;
                const MAX_PNG_BYTES: usize = 4 * 1024 * 1024;
                let (width, height, rgba) = crate::snapshot::render_current_rgba_without_poll(self)
                    .map_err(|message| failure("screenshot_unavailable", &message))?;
                let mut png = Vec::new();
                image::codecs::png::PngEncoder::new(&mut png)
                    .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
                    .map_err(|error| failure("screenshot_failed", &error.to_string()))?;
                if png.len() > MAX_PNG_BYTES {
                    return Err(failure(
                        "screenshot_too_large",
                        "PNG image exceeds the 4 MiB MCP screenshot limit",
                    ));
                }
                return Ok(json!({
                    "documentEpoch":self.dom.borrow().id(),
                    "width":width,
                    "height":height,
                    "boundary":"cpu_rendered",
                    "physicalPresentation":"not_confirmed",
                    "pngBase64":base64::engine::general_purpose::STANDARD.encode(png)
                }));
            }
            #[cfg(not(feature = "software-renderer"))]
            return Err(failure(
                "unsupported_capability",
                "MCP screenshots require the software-renderer feature",
            ));
        }
        if method == "networkStatus" {
            return Ok(
                json!({"documentEpoch":self.dom.borrow().id(),"streams":stream_status(&self.streams),"streamLimits":Self::stream_limits()}),
            );
        }
        if method == "runtime.memoryUsage" {
            if request.as_object().is_none_or(|fields| {
                fields
                    .keys()
                    .any(|key| !["method", "collectGarbage"].contains(&key.as_str()))
            }) {
                return Err(failure(
                    "invalid_request",
                    "runtime.memoryUsage accepts only method and collectGarbage",
                ));
            }
            let collect_garbage = request
                .get("collectGarbage")
                .map(|value| {
                    value.as_bool().ok_or_else(|| {
                        failure("invalid_request", "collectGarbage must be a boolean")
                    })
                })
                .transpose()?
                .unwrap_or(false);
            let before = Self::quickjs_memory_usage(&self.js_runtime);
            if collect_garbage {
                self.js_runtime.run_gc();
                let after = Self::quickjs_memory_usage(&self.js_runtime);
                return Ok(
                    json!({"documentEpoch":self.dom.borrow().id(),"collectionRequested":true,"beforeCollection":before,"afterCollection":after}),
                );
            }
            return Ok(
                json!({"documentEpoch":self.dom.borrow().id(),"collectionRequested":false,"usage":before}),
            );
        }
        if method == "diagnostics" {
            return Ok(
                json!({"documentEpoch": self.dom.borrow().id(), "scriptStatus": if self.script_budget.interrupted() { "suspended" } else { "running" }, "errors": &*self.script_diagnostics.borrow()}),
            );
        }
        if !matches!(method, "activate" | "fill" | "check" | "focus") {
            return Err(failure("invalid_request", "unsupported document method"));
        }
        if self.script_budget.interrupted() {
            return Err(failure("script_suspended", INTERRUPTED_MESSAGE));
        }
        if request.get("requestId").is_some() || request.get("expectedVersion").is_some() {
            return Err(failure("invalid_request", "requestId and expectedVersion are supported only by invoke; control mutations require documentEpoch"));
        }
        let epoch = request
            .get("documentEpoch")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                failure(
                    "invalid_request",
                    "documentEpoch is required for control mutations",
                )
            })?;
        let reference = request
            .get("ref")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("invalid_request", "canonical control ref is required"))?;
        let snapshot = control_snapshot(&self.dom.borrow());
        if snapshot["documentEpoch"].as_u64() != Some(epoch) {
            return Err(failure(
                "stale_document",
                "control request belongs to a different document",
            ));
        }
        let target = snapshot["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|control| control["ref"] == reference)
            .ok_or_else(|| failure("stale_reference", "control is no longer in this document"))?;
        if target["enabled"] != true || (method == "fill" && target["readOnly"] == true) {
            return Err(failure(
                "control_unavailable",
                "control is disabled or read-only",
            ));
        }
        if (method == "fill" && request.get("value").and_then(Value::as_str).is_none())
            || (method == "check" && request.get("checked").and_then(Value::as_bool).is_none())
        {
            return Err(failure(
                "invalid_request",
                "fill requires a string value; check requires a boolean checked",
            ));
        }
        let diagnostics_start = self
            .script_diagnostics
            .borrow()
            .back()
            .map_or(0, |entry| entry.sequence);
        let result = self
            .script_budget
            .run(CALLBACK_LIMIT, || {
                self.js_context.with(|ctx| -> rquickjs::Result<bool> {
                    let lapui: rquickjs::Object = ctx.globals().get("lapui")?;
                    let function: Function = lapui.get(method)?;
                    match method {
                        "fill" => {
                            let value = request.get("value").and_then(Value::as_str);
                            match value {
                                Some(value) => function.call((reference, value)),
                                None => Ok(false),
                            }
                        }
                        "check" => {
                            let checked = request.get("checked").and_then(Value::as_bool);
                            match checked {
                                Some(checked) => function.call((reference, checked)),
                                None => Ok(false),
                            }
                        }
                        _ => function.call((reference,)),
                    }
                })
            })
            .map_err(|error| {
                let message = javascript_error(&self.js_context, &error, &self.script_budget);
                scripts::report(
                    &self.script_diagnostics,
                    reference,
                    "control",
                    message.clone(),
                );
                failure("script_error", &message)
            })?;
        if !result {
            return Err(failure(
                "control_unavailable",
                "control rejected the operation or its arguments",
            ));
        }
        drain_jobs(&self.js_runtime, &self.script_budget).map_err(|message| {
            scripts::report(
                &self.script_diagnostics,
                reference,
                "microtask",
                message.clone(),
            );
            failure("script_error", &message)
        })?;
        let dispatch_errors: Vec<_> = self
            .script_diagnostics
            .borrow()
            .iter()
            .filter(|entry| entry.sequence > diagnostics_start)
            .cloned()
            .collect();
        Ok(
            json!({"status":"dispatched", "documentEpoch":epoch, "controls":self.semantic_controls()["controls"], "dispatchErrors":dispatch_errors}),
        )
    }

    pub fn new(
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
    ) -> Result<(Self, mpsc::Sender<Result<Value, String>>), String> {
        Self::new_with_source(actions, proxy, HTML, SCRIPT)
    }

    pub fn new_with_source(
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
        html: &str,
        script: &str,
    ) -> Result<(Self, mpsc::Sender<Result<Value, String>>), String> {
        Self::new_with_source_and_font_context(
            actions,
            proxy,
            html,
            script,
            Self::new_font_context(),
        )
    }

    /// Build one application font context that can be shared by document reloads.
    pub fn new_font_context() -> FontContext {
        let mut font_context = FontContext::default();
        font_context
            .collection
            .register_fonts(Blob::new(Arc::new(blitz::dom::BULLET_FONT) as _), None);
        font_context.collection.make_shared();
        font_context.source_cache.make_shared();
        font_context
    }

    pub fn new_with_source_and_font_context(
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
        html: &str,
        script: &str,
        font_context: FontContext,
    ) -> Result<(Self, mpsc::Sender<Result<Value, String>>), String> {
        Self::new_with_config(
            actions,
            proxy,
            html,
            script,
            DocumentConfig {
                font_ctx: Some(font_context),
                ..DocumentConfig::default()
            },
            None,
        )
    }

    pub fn new_with_local_source(
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
        html: &str,
        script: &str,
        app_root: &Path,
    ) -> Result<(Self, mpsc::Sender<Result<Value, String>>), String> {
        Self::new_with_local_source_and_font_context(
            actions,
            proxy,
            html,
            script,
            app_root,
            Self::new_font_context(),
        )
    }

    pub fn new_with_local_source_and_font_context(
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
        html: &str,
        script: &str,
        app_root: &Path,
        font_context: FontContext,
    ) -> Result<(Self, mpsc::Sender<Result<Value, String>>), String> {
        let root = app_root.canonicalize().map_err(|error| {
            format!("could not resolve app root {}: {error}", app_root.display())
        })?;
        let base_url = Url::from_directory_path(&root)
            .map_err(|_| format!("app root cannot be used as a file URL: {}", root.display()))?;
        let config = DocumentConfig {
            base_url: Some(base_url.to_string()),
            net_provider: Some(Arc::new(LocalDirectoryNetProvider { root: root.clone() })),
            font_ctx: Some(font_context),
            ..DocumentConfig::default()
        };
        Self::new_with_config(actions, proxy, html, script, config, Some(root))
    }

    /// Runs supplied trusted markup with an HTTP(S) base for relative fetch URLs.
    /// This does not navigate to or download the document or remote JS modules.
    pub fn new_with_http_base(
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
        html: &str,
        script: &str,
        base_url: &str,
    ) -> Result<(Self, mpsc::Sender<Result<Value, String>>), String> {
        let base = Url::parse(base_url).map_err(|error| error.to_string())?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err("HTTP document base requires an absolute http or https URL".into());
        }
        Self::new_with_config(
            actions,
            proxy,
            html,
            script,
            DocumentConfig {
                base_url: Some(base.to_string()),
                ..DocumentConfig::default()
            },
            None,
        )
    }

    fn new_with_config(
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
        html: &str,
        script: &str,
        config: DocumentConfig,
        app_root: Option<PathBuf>,
    ) -> Result<(Self, mpsc::Sender<Result<Value, String>>), String> {
        let dom = Rc::new(RefCell::new(
            HtmlDocument::from_html(html, config).into_inner(),
        ));
        let debug_trace = DebugTrace::new();
        let diagnostics: ScriptDiagnostics = Rc::new(RefCell::new(Default::default()));
        let mut startup_scripts =
            scripts::html_scripts(&dom.borrow(), app_root.as_deref(), &diagnostics);
        let fetch_base_url = {
            let doc = dom.borrow();
            if app_root.is_some() || matches!(doc.base_url().scheme(), "http" | "https") {
                Some(doc.base_url().to_string())
            } else {
                None
            }
        };
        if !script.is_empty() {
            startup_scripts.push(StartupScript::Classic {
                name: format!("{}#cli-bundle", dom.borrow().base_url()),
                source: script.to_owned(),
            });
        }
        let js_runtime = Runtime::new().map_err(|e| e.to_string())?;
        let script_budget = ScriptBudget::install(&js_runtime);
        scripts::install_loader(
            &js_runtime,
            app_root.clone(),
            dom.borrow().base_url().to_string(),
        );
        let js_context = Context::full(&js_runtime).map_err(|e| e.to_string())?;
        let gc_requested = Rc::new(Cell::new(false));
        let (tx, completions) = mpsc::channel::<Completion>();
        let lifetime = Cancellation::default();
        let (external_tx, external_rx) = mpsc::channel::<Result<Value, String>>();
        let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let doc_id = dom.borrow().id();
        let control_waker = waker.clone();
        let control_proxy = proxy.clone();
        let (controller, control_requests) = control::channel(move || {
            wake_document(&control_waker, &control_proxy, doc_id);
        });
        let page_change_notifier = controller.page_change_notifier();
        let page_change_trace = debug_trace.clone();
        let timer_waker = waker.clone();
        let timer_proxy = proxy.clone();
        let timers = Timers::new(move || wake_document(&timer_waker, &timer_proxy, doc_id));
        let frames = Rc::new(RefCell::new(Frames::new(debug_trace.clone())));

        let get_dom = dom.clone();
        let set_dom = dom.clone();
        let style_dom = dom.clone();
        let style_remove_dom = dom.clone();
        let mutation_dom = dom.clone();
        let mutation_batch = Rc::new(RefCell::new(MutationBatch::default()));
        let text_batch = mutation_batch.clone();
        let style_batch = mutation_batch.clone();
        let style_remove_batch = mutation_batch.clone();
        let batch_begin = mutation_batch.clone();
        let batch_end = mutation_batch.clone();
        let invoke_actions = actions.clone();
        let invoke_work = HostWork::new(lifetime.clone()).map_err(|error| error.to_string())?;
        let wait_work = invoke_work.clone();
        let changes_work = invoke_work.clone();
        let catalog_work = invoke_work.clone();
        let observe_actions = actions.clone();
        let trace_actions = actions.clone();
        let operation_queries = actions.operations();
        let operation_waits = actions.operations();
        let invoke_tx = tx.clone();
        let invoke_waker = waker.clone();
        let invoke_proxy = proxy.clone();
        let controls_dom = dom.clone();
        let streams = Rc::new(RefCell::new(None::<StreamWork>));
        let fetch_requests = Arc::new(Mutex::new(HashMap::<i32, Cancellation>::new()));
        js_context
            .with(|ctx| -> rquickjs::Result<()> {
                let globals = ctx.globals();
                let page_change_notifications = page_change_notifier.clone();
                let page_change_trace = page_change_trace.clone();
                globals.set(
                    "__lapui_notify_page_change",
                    Func::from(move |revision: u64| -> u64 {
                        page_change_notifications.notify();
                        if revision == 0 {
                            0
                        } else {
                            page_change_trace
                                .instant(
                                    "page_change",
                                    json!({"documentRevision":revision}),
                                    true,
                                )
                                .unwrap_or(0)
                        }
                    }),
                )?;
                let frame_clock = frames.clone();
                globals.set("__lapui_now", Func::from(move || frame_clock.borrow().now()))?;
                globals.set("__lapui_time_origin", frames.borrow().time_origin)?;
                let frame_requests = frames.clone();
                let frame_dom = dom.clone();
                globals.set("__lapui_animation_request", Func::from(move |id: i32| -> bool {
                    if !frame_requests.borrow_mut().request(id) { return false; }
                    // The window driver schedules successors at its next deadline.
                    if !frame_requests.borrow().in_frame {
                        frame_dom.borrow().shell_provider.request_redraw();
                    }
                    true
                }))?;
                let frame_cancellations = frames.clone();
                globals.set("__lapui_animation_cancel", Func::from(move |id: i32| { frame_cancellations.borrow_mut().cancel(id); }))?;
                let read_trace=debug_trace.clone();
                globals.set("__lapui_debug_trace_read",Func::from(move |after:String,limit:usize| {
                    let result=after.parse::<u64>().map_err(|_|ActionError::new("invalid_request","invalid trace cursor"))
                        .and_then(|after|read_trace.read(after,limit));
                    match result { Ok(mut page)=>{page["documentEpoch"]=json!(doc_id);json!({"ok":true,"observation":page})},Err(error)=>json!({"ok":false,"error":error}) }.to_string()
                }))?;
                let configure_trace=debug_trace.clone();
                let trace_dom=dom.clone();
                globals.set("__lapui_debug_trace_configure",Func::from(move |enabled:bool,clear:bool| {
                    configure_trace.configure(enabled,clear);
                    if enabled { trace_dom.borrow().shell_provider.request_redraw(); }
                }))?;
                globals.set("__lapui_observer_limits", Self::rendering_limits().to_string())?;
                globals.set("__lapui_form_limits", Self::form_limits().to_string())?;
                let rendering_dom = dom.clone();
                let rendering_frames = frames.clone();
                globals.set("__lapui_render_request", Func::from(move || {
                    let mut frames = rendering_frames.borrow_mut();
                    frames.rendering_pending = true;
                    if !frames.in_frame { rendering_dom.borrow().shell_provider.request_redraw(); }
                }))?;
                let viewport_dom = dom.clone();
                globals.set("__lapui_viewport", Func::from(move || -> Vec<f64> {
                    let doc = viewport_dom.borrow();
                    let viewport = doc.viewport();
                    let scroll = doc.viewport_scroll();
                    vec![f64::from(viewport.window_size.0)/viewport.scale_f64(),
                        f64::from(viewport.window_size.1)/viewport.scale_f64(),viewport.scale_f64(),scroll.x,scroll.y]
                }))?;
                let resize_dom = dom.clone();
                let resize_frames = frames.clone();
                let resize_batch = mutation_batch.clone();
                globals.set("__lapui_resize_samples", Func::from(move |references: Vec<String>| -> String {
                    if references.len() > 2048 { return "{}".into(); }
                    flush_layout(&resize_dom, &resize_batch, &resize_frames);
                    let doc = resize_dom.borrow();
                    let samples: serde_json::Map<String,Value> = references.into_iter().filter_map(|reference| {
                        resolve_node_ref(&doc, &reference).map(|id| (reference, json!(crate::geometry::resize_sample(&doc,id))))
                    }).collect();
                    Value::Object(samples).to_string()
                }))?;
                let intersection_dom = dom.clone();
                let intersection_frames = frames.clone();
                let intersection_batch = mutation_batch.clone();
                globals.set("__lapui_intersection_samples", Func::from(move |references: Vec<String>| -> String {
                    if references.len() > 2048 { return "{}".into(); }
                    flush_layout(&intersection_dom, &intersection_batch, &intersection_frames);
                    let doc = intersection_dom.borrow();
                    let samples: serde_json::Map<String,Value> = references.into_iter().map(|reference| {
                        let sample = resolve_node_ref(&doc, &reference).map_or_else(
                            || vec![0.0; 5],
                            |id| crate::geometry::intersection_sample(&doc, id),
                        );
                        (reference, json!(sample))
                    }).collect();
                    Value::Object(samples).to_string()
                }))?;
                let offset_dom = dom.clone();
                let offset_frames = frames.clone();
                let offset_batch = mutation_batch.clone();
                globals.set("__lapui_offset_metrics",Func::from(move |reference: String| -> Vec<f64> {
                    flush_layout(&offset_dom,&offset_batch,&offset_frames);
                    let doc = offset_dom.borrow();
                    resolve_node_ref(&doc,&reference).map_or_else(||vec![0.0;4],|id|crate::geometry::offset_metrics(&doc,id))
                }))?;
                let offset_parent_dom = dom.clone();
                let offset_parent_frames = frames.clone();
                let offset_parent_batch = mutation_batch.clone();
                globals.set("__lapui_offset_parent",Func::from(move |reference: String| -> String {
                    flush_layout(&offset_parent_dom,&offset_parent_batch,&offset_parent_frames);
                    let doc = offset_parent_dom.borrow();
                    resolve_node_ref(&doc,&reference).and_then(|id|crate::geometry::offset_parent(&doc,id))
                        .map_or_else(String::new,|id|canonical_node_ref(doc.id(),id))
                }))?;
                let computed_dom = dom.clone();
                let computed_frames = frames.clone();
                let computed_batch = mutation_batch.clone();
                globals.set("__lapui_computed_value",Func::from(move |reference: String,name: String| -> String {
                    flush_layout(&computed_dom,&computed_batch,&computed_frames);
                    let doc = computed_dom.borrow();
                    resolve_node_ref(&doc,&reference).map_or_else(String::new, |id| crate::computed_style::value(&doc,id,&name))
                }))?;
                let computed_names_dom = dom.clone();
                let computed_names_frames = frames.clone();
                let computed_names_batch = mutation_batch.clone();
                globals.set("__lapui_computed_names",Func::from(move |reference: String| -> Vec<String> {
                    flush_layout(&computed_names_dom,&computed_names_batch,&computed_names_frames);
                    let doc = computed_names_dom.borrow();
                    resolve_node_ref(&doc,&reference).map_or_else(Vec::new, |id| crate::computed_style::names(&doc,id))
                }))?;
                let metrics_dom = dom.clone();
                let metrics_frames = frames.clone();
                let metrics_batch = mutation_batch.clone();
                globals.set("__lapui_layout_metrics", Func::from(move |reference: String| -> Vec<f64> {
                    flush_layout(&metrics_dom, &metrics_batch, &metrics_frames);
                    let doc = metrics_dom.borrow();
                    resolve_node_ref(&doc, &reference).map_or_else(|| vec![0.0;8], |id| crate::geometry::metrics(&doc,id))
                }))?;
                let scroll_dom = dom.clone();
                let scroll_frames = frames.clone();
                let scroll_batch = mutation_batch.clone();
                globals.set("__lapui_scroll", Func::from(move |reference: String, x: Option<f64>, y: Option<f64>, relative: bool| -> bool {
                    flush_layout(&scroll_dom, &scroll_batch, &scroll_frames);
                    let mut doc = scroll_dom.borrow_mut();
                    resolve_node_ref(&doc,&reference).is_some_and(|id| crate::geometry::scroll(&mut doc,id,x,y,relative))
                }))?;
                let rects_dom = dom.clone();
                let rects_frames = frames.clone();
                let rects_batch = mutation_batch.clone();
                globals.set("__lapui_client_rects", Func::from(move |reference: String| -> Vec<Vec<f64>> {
                    flush_layout(&rects_dom,&rects_batch,&rects_frames);
                    let doc = rects_dom.borrow();
                    let Some(id) = resolve_node_ref(&doc,&reference) else { return Vec::new(); };
                    if !crate::geometry::has_boxes(&doc,id) { return Vec::new(); }
                    doc.node_client_rects(id).into_iter()
                        .map(|rect| vec![rect.x,rect.y,rect.width,rect.height]).collect()
                }))?;
                let document_root = canonical_node_ref(doc_id, dom.borrow().root_node().id);
                globals.set(
                    "__lapui_document_ref",
                    Func::from(move || document_root.clone()),
                )?;
                let geometry_dom = dom.clone();
                let geometry_frames = frames.clone();
                let geometry_batch = mutation_batch.clone();
                globals.set("__lapui_bounding_rect", Func::from(move |reference: String| -> Vec<f64> {
                    flush_layout(&geometry_dom, &geometry_batch, &geometry_frames);
                    let doc = geometry_dom.borrow();
                    let Some(id) = resolve_node_ref(&doc, &reference) else { return vec![0.0;4]; };
                    if !crate::geometry::has_boxes(&doc,id) { return vec![0.0;4]; }
                    doc.get_client_bounding_rect(id).map_or_else(|| vec![0.0;4], |rect| vec![rect.x,rect.y,rect.width,rect.height])
                }))?;
                let timer_arm = timers.handle.clone();
                globals.set(
                    "__lapui_timer_arm",
                    Func::from(move |id: i32, delay: i32| -> bool { timer_arm.arm(id, delay) }),
                )?;
                let timer_clear = timers.handle.clone();
                globals.set(
                    "__lapui_timer_clear",
                    Func::from(move |id: i32| {
                        timer_clear.clear(id);
                    }),
                )?;
                globals.set(
                    "__lapui_exists",
                    Func::from(move |id: String| -> bool {
                        resolve_node_ref(&get_dom.borrow(), &id).is_some()
                    }),
                )?;
                globals.set(
                    "__lapui_get_active_element",
                    Func::from({
                        let dom = dom.clone();
                        move || -> String {
                            let doc = dom.borrow();
                            doc.get_focussed_node_id()
                                .map(|node| canonical_node_ref(doc_id, node))
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_set_focus",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> bool {
                            let mut doc = dom.borrow_mut();
                            let Some(node_id) = resolve_node_ref(&doc, &reference) else {
                                return false;
                            };
                            let Some(node) = doc.get_node(node_id) else {
                                return false;
                            };
                            let root_id = doc.root_node().id;
                            let mut ancestor = Some(node_id);
                            let mut connected = false;
                            while let Some(id) = ancestor {
                                if id == root_id {
                                    connected = true;
                                    break;
                                }
                                ancestor = doc.get_node(id).and_then(|node| node.parent);
                            }
                            if !connected {
                                return false;
                            }
                            let Some(element) = node.data.downcast_element() else {
                                return false;
                            };
                            let tag = element.name.local.to_string();
                            let focusable =
                                matches!(tag.as_str(), "button" | "input" | "select" | "textarea")
                                    || (tag == "a"
                                        && element.attr(LocalName::from("href")).is_some())
                                    || element.attr(LocalName::from("tabindex")).is_some();
                            let enabled = crate::forms::enabled(&doc, node_id);
                            if !focusable || !enabled {
                                return false;
                            }
                            doc.set_focus_to(node_id)
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_clear_focus",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> bool {
                            let mut doc = dom.borrow_mut();
                            let target = resolve_node_ref(&doc, &reference);
                            if target.is_some() && target == doc.get_focussed_node_id() {
                                doc.clear_focus();
                                true
                            } else {
                                false
                            }
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_resolve",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> String {
                            let doc = dom.borrow();
                            resolve_node_ref(&doc, &reference)
                                .map(|node| canonical_node_ref(doc_id, node))
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_get_attribute",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String, name: String| -> String {
                            let doc = dom.borrow();
                            let Some(node_id) = resolve_node_ref(&doc, &reference) else {
                                return String::new();
                            };
                            let Some(element) = doc
                                .get_node(node_id)
                                .and_then(|node| node.data.downcast_element())
                            else {
                                return String::new();
                            };
                            if name.eq_ignore_ascii_case("style")
                                && element.style_attribute.is_some()
                            {
                                return serialized_inline_style(&doc, &reference);
                            }
                            element
                                .attr(LocalName::from(name))
                                .unwrap_or_default()
                                .to_owned()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_has_attribute",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String, name: String| -> bool {
                            let doc = dom.borrow();
                            resolve_node_ref(&doc, &reference)
                                .and_then(|id| doc.get_node(id))
                                .and_then(|node| node.data.downcast_element())
                                .is_some_and(|element| {
                                    element.attr(LocalName::from(name.clone())).is_some()
                                        || (name.eq_ignore_ascii_case("style")
                                            && element.style_attribute.is_some())
                                })
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_tag",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> String {
                            let doc = dom.borrow();
                            resolve_node_ref(&doc, &reference)
                                .and_then(|id| doc.get_node(id))
                                .and_then(|node| node.data.downcast_element())
                                .map(|element| element.name.local.to_string())
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_node_type",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> i32 {
                            let doc = dom.borrow();
                            let Some(node) =
                                resolve_node_ref(&doc, &reference).and_then(|id| doc.get_node(id))
                            else {
                                return 0;
                            };
                            match &node.data {
                                blitz::dom::NodeData::Document(_) => 9,
                                blitz::dom::NodeData::Element(_)
                                | blitz::dom::NodeData::AnonymousBlock(_) => 1,
                                blitz::dom::NodeData::Text(_) => 3,
                                blitz::dom::NodeData::Comment { .. } => 8,
                            }
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_parent",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> String {
                            let doc = dom.borrow();
                            resolve_node_ref(&doc, &reference)
                                .and_then(|id| doc.get_node(id))
                                .and_then(|node| node.parent)
                                .map(|node| canonical_node_ref(doc_id, node))
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_sibling",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String, offset: i32| -> String {
                            let doc = dom.borrow();
                            let Some(node) =
                                resolve_node_ref(&doc, &reference).and_then(|id| doc.get_node(id))
                            else {
                                return String::new();
                            };
                            let Some(parent) = node.parent.and_then(|id| doc.get_node(id)) else {
                                return String::new();
                            };
                            let Some(index) = parent.children.iter().position(|id| *id == node.id)
                            else {
                                return String::new();
                            };
                            let sibling_index = if offset < 0 {
                                index.checked_sub(1)
                            } else {
                                index.checked_add(1)
                            };
                            sibling_index
                                .and_then(|index| parent.children.get(index).copied())
                                .map(|node| canonical_node_ref(doc_id, node))
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_query",
                    Func::from({
                        let dom = dom.clone();
                        move |selector: String, scope: String| -> String {
                            let doc = dom.borrow();
                            let result = if scope.is_empty() {
                                doc.query_selector(&selector).ok().flatten()
                            } else {
                                resolve_node_ref(&doc, &scope).and_then(|scope| {
                                    doc.query_selector_in(scope, &selector).ok().flatten()
                                })
                            };
                            result
                                .map(|node| canonical_node_ref(doc_id, node))
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_query_all",
                    Func::from({
                        let dom = dom.clone();
                        move |selector: String, scope: String| -> String {
                            let doc = dom.borrow();
                            let results = if scope.is_empty() {
                                doc.query_selector_all(&selector).ok()
                            } else {
                                resolve_node_ref(&doc, &scope).and_then(|scope| {
                                    doc.query_selector_all_in(scope, &selector).ok()
                                })
                            };
                            let refs = results
                                .into_iter()
                                .flatten()
                                .map(|node| canonical_node_ref(doc_id, node))
                                .collect::<Vec<_>>();
                            serde_json::to_string(&refs).unwrap_or_else(|_| "[]".into())
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_get_text",
                    Func::from({
                        let dom = dom.clone();
                        move |id: String| -> String {
                            let doc = dom.borrow();
                            resolve_node_ref(&doc, &id)
                                .and_then(|node| doc.get_node(node))
                                .map(|node| match &node.data {
                                    blitz::dom::NodeData::Comment { contents } => contents.clone(),
                                    _ => node.text_content(),
                                })
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_get_value",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> String {
                            let doc = dom.borrow();
                            resolve_node_ref(&doc, &reference)
                                .and_then(|id| crate::forms::value(&doc, id))
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_set_value",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String, value: String| -> bool {
                            let mut doc = dom.borrow_mut();
                            let Some(id) = resolve_node_ref(&doc, &reference) else {
                                return false;
                            };
                            crate::forms::set_value(&mut doc,id,&value)
                        }
                    }),
                )?;
                let selection_dom = dom.clone();
                globals.set(
                    "__lapui_get_selection",
                    Func::from(move |reference: String| -> String {
                        let doc = selection_dom.borrow();
                        resolve_node_ref(&doc, &reference)
                            .and_then(|id| crate::forms::selection(&doc, id))
                            .map_or_else(
                                || "null".to_owned(),
                                |(start, end, direction)| {
                                    json!([start, end, direction]).to_string()
                                },
                            )
                    }),
                )?;
                let selection_dom = dom.clone();
                globals.set(
                    "__lapui_set_selection",
                    Func::from(
                        move |reference: String,
                              start: i64,
                              end: i64,
                              direction: String|
                              -> bool {
                            if start < 0 || end < 0 {
                                return false;
                            }
                            let mut doc = selection_dom.borrow_mut();
                            let Some(id) = resolve_node_ref(&doc, &reference) else {
                                return false;
                            };
                            crate::forms::set_selection(
                                &mut doc,
                                id,
                                start as usize,
                                end as usize,
                                &direction,
                            )
                        },
                    ),
                )?;
                globals.set(
                    "__lapui_get_checked",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> bool {
                            let doc = dom.borrow();
                            resolve_node_ref(&doc, &reference)
                                .and_then(|id| doc.get_node(id))
                                .and_then(|node| node.data.downcast_element())
                                .map(|element| {
                                    element.checkbox_input_checked().unwrap_or_else(|| {
                                        element.attr(LocalName::from("checked")).is_some()
                                    })
                                })
                                .unwrap_or(false)
                        }
                    }),
                )?;
                globals.set("__lapui_set_checked", Func::from({
                    let dom = dom.clone();
                    move |reference: String, checked: bool| -> bool {
                        let mut doc = dom.borrow_mut();
                        let Some(id) = resolve_node_ref(&doc, &reference) else { return false; };
                        crate::forms::set_checked(&mut doc, id, checked)
                    }
                }))?;
                globals.set("__lapui_restore_checked", Func::from({
                    let dom = dom.clone();
                    move |reference: String, checked: bool| -> bool {
                        let mut doc = dom.borrow_mut();
                        let Some(id) = resolve_node_ref(&doc, &reference) else { return false; };
                        crate::forms::set_checked_raw(&mut doc, id, checked)
                    }
                }))?;
                globals.set("__lapui_form_owner",Func::from({let dom=dom.clone();move |reference:String| -> String {
                    let doc=dom.borrow();resolve_node_ref(&doc,&reference).and_then(|id|crate::forms::owner(&doc,id))
                        .map(|id|canonical_node_ref(doc.id(),id)).unwrap_or_default()
                }}))?;
                globals.set("__lapui_form_controls",Func::from({let dom=dom.clone();move |reference:String| -> Vec<String> {
                    let doc=dom.borrow();resolve_node_ref(&doc,&reference).map(|id|crate::forms::controls(&doc,id))
                        .unwrap_or_default().into_iter().map(|id|canonical_node_ref(doc.id(),id)).collect()
                }}))?;
                globals.set("__lapui_url_valid",Func::from(move |value:String| -> bool {Url::parse(&value).is_ok()}))?;
                globals.set("__lapui_label_control", Func::from({
                    let dom = dom.clone();
                    move |reference: String| -> String {
                        let doc = dom.borrow();
                        resolve_node_ref(&doc, &reference).and_then(|id| crate::forms::label_control(&doc, id))
                            .map(|id| canonical_node_ref(doc.id(), id)).unwrap_or_default()
                    }
                }))?;
                globals.set("__lapui_is_connected", Func::from({
                    let dom = dom.clone();
                    move |reference: String| -> bool {
                        let doc = dom.borrow();
                        resolve_node_ref(&doc, &reference).and_then(|id| doc.get_node(id))
                            .is_some_and(|node| node.flags.is_in_document())
                    }
                }))?;
                globals.set("__lapui_radio_group", Func::from({
                    let dom = dom.clone();
                    move |reference: String| -> String {
                        let doc = dom.borrow();
                        let Some(id) = resolve_node_ref(&doc, &reference) else { return String::new(); };
                        crate::forms::radio_group(&doc, id).into_iter().map(|id| canonical_node_ref(doc.id(), id)).collect::<Vec<_>>().join("\n")
                    }
                }))?;
                globals.set("__lapui_is_enabled", Func::from({
                    let dom = dom.clone();
                    move |reference: String| -> bool {
                        let doc = dom.borrow();
                        resolve_node_ref(&doc, &reference).is_some_and(|id| crate::forms::enabled(&doc, id))
                    }
                }))?;
                globals.set(
                    "__lapui_body",
                    Func::from({
                        let dom = dom.clone();
                        move || -> String {
                            dom.borrow()
                                .find_body_node()
                                .map(|node| canonical_node_ref(doc_id, node.id))
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_event_path",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> String {
                            let doc = dom.borrow();
                            let Some(mut node_id) = resolve_node_ref(&doc, &reference) else {
                                return String::new();
                            };
                            let mut path = Vec::new();
                            while let Some(node) = doc.get_node(node_id) {
                                path.push(canonical_node_ref(doc_id, node_id));
                                let Some(parent) = node.parent else { break };
                                node_id = parent;
                            }
                            path.join("\n")
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_create_element",
                    Func::from({
                        let dom = dom.clone();
                        move |tag: String| -> String {
                            if tag.is_empty()
                                || !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                            {
                                return String::new();
                            }
                            let mut doc = dom.borrow_mut();
                            let mut mutator = doc.mutate();
                            let name = QualName::new(
                                None,
                                blitz::dom::ns!(html),
                                LocalName::from(tag.to_ascii_lowercase()),
                            );
                            let node = mutator.create_element(name, Vec::new());
                            canonical_node_ref(doc_id, node)
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_create_text",
                    Func::from({
                        let dom = dom.clone();
                        move |value: String| -> String {
                            let node = dom.borrow_mut().mutate().create_text_node(&value);
                            canonical_node_ref(doc_id, node)
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_create_comment",
                    Func::from({
                        let dom = dom.clone();
                        move |value: String| -> String {
                            let node = dom.borrow_mut().mutate().create_comment_node(&value);
                            canonical_node_ref(doc_id, node)
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_dom_retired",
                    Func::from({
                        let gc_requested = gc_requested.clone();
                        move || gc_requested.set(true)
                    }),
                )?;
                globals.set(
                    "__lapui_detached_root",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> String {
                            let doc = dom.borrow();
                            resolve_node_ref(&doc, &reference)
                                .and_then(|node| detached_root(&doc, node))
                                .map(|node| canonical_node_ref(doc_id, node))
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_collect_detached",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> bool {
                            let mut doc = dom.borrow_mut();
                            let Some(node) = resolve_node_ref(&doc, &reference) else {
                                return false;
                            };
                            if detached_root(&doc, node) != Some(node) {
                                return false;
                            }
                            let removed = doc.mutate().remove_and_drop_node(node).is_some();
                            removed
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_children",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> String {
                            let doc = dom.borrow();
                            let Some(node) = resolve_node_ref(&doc, &reference)
                                .and_then(|node| doc.get_node(node))
                            else {
                                return "[]".into();
                            };
                            serde_json::to_string(
                                &node
                                    .children
                                    .iter()
                                    .copied()
                                    .map(|node| canonical_node_ref(doc_id, node))
                                    .collect::<Vec<_>>(),
                            )
                            .unwrap_or_else(|_| "[]".into())
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_get_inner_html",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> String {
                            serialized_inner_html(&dom.borrow(), &reference)
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_set_inner_html",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String, html: String| -> bool {
                            let mut doc = dom.borrow_mut();
                            let Some(node) = resolve_node_ref(&doc, &reference) else {
                                return false;
                            };
                            if doc
                                .get_node(node)
                                .is_none_or(|node| node.data.downcast_element().is_none())
                            {
                                return false;
                            }
                            let mut mutator = doc.mutate();
                            mutator.replace_children(node, &[]);
                            blitz::html::DocumentHtmlParser::parse_inner_html_into_mutator(
                                &mut mutator,
                                node,
                                &html,
                            );
                            true
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_clone",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String, deep: bool| -> String {
                            let mut doc = dom.borrow_mut();
                            let Some(source) = resolve_node_ref(&doc, &reference) else {
                                return String::new();
                            };
                            let clone = if deep {
                                doc.deep_clone_node(source)
                            } else if let Some(clone) = shallow_clone_node(&mut doc, source) {
                                clone
                            } else {
                                return String::new();
                            };
                            canonical_node_ref(doc_id, clone)
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_append_child",
                    Func::from({
                        let dom = dom.clone();
                        move |parent: String, child: String| -> bool {
                            let mut doc = dom.borrow_mut();
                            let (Some(parent), Some(child)) = (
                                resolve_node_ref(&doc, &parent),
                                resolve_node_ref(&doc, &child),
                            ) else {
                                return false;
                            };
                            if !can_insert_node(&doc, parent, child) {
                                return false;
                            }
                            doc.mutate().append_children(parent, &[child]);
                            true
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_insert_before",
                    Func::from({
                        let dom = dom.clone();
                        move |parent: String, child: String, before: String| -> bool {
                            let mut doc = dom.borrow_mut();
                            let (Some(parent), Some(child)) = (
                                resolve_node_ref(&doc, &parent),
                                resolve_node_ref(&doc, &child),
                            ) else {
                                return false;
                            };
                            if !can_insert_node(&doc, parent, child) {
                                return false;
                            }
                            if before.is_empty() {
                                doc.mutate().append_children(parent, &[child]);
                                return true;
                            }
                            let Some(before) = resolve_node_ref(&doc, &before) else {
                                return false;
                            };
                            if doc.get_node(before).and_then(|node| node.parent) != Some(parent) {
                                return false;
                            }
                            if child == before {
                                return true;
                            }
                            doc.mutate().insert_nodes_before(before, &[child]);
                            true
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_remove_child",
                    Func::from({
                        let dom = dom.clone();
                        move |parent: String, child: String| -> bool {
                            let mut doc = dom.borrow_mut();
                            let (Some(parent), Some(child)) = (
                                resolve_node_ref(&doc, &parent),
                                resolve_node_ref(&doc, &child),
                            ) else {
                                return false;
                            };
                            if doc.get_node(child).and_then(|node| node.parent) != Some(parent) {
                                return false;
                            }
                            doc.mutate().remove_node(child);
                            true
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_remove_node",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> bool {
                            let mut doc = dom.borrow_mut();
                            let Some(node) = resolve_node_ref(&doc, &reference) else {
                                return false;
                            };
                            doc.mutate().remove_node(node);
                            true
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_set_attribute",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String, name: String, value: String| {
                            let mut doc = dom.borrow_mut();
                            let Some(node) = resolve_node_ref(&doc, &reference) else {
                                return;
                            };
                            let name =
                                QualName::new(None, blitz::dom::ns!(), LocalName::from(name));
                            doc.mutate().set_attribute(node, name, &value);
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_remove_attribute",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String, name: String| -> bool {
                            let mut doc = dom.borrow_mut();
                            let Some(node) = resolve_node_ref(&doc, &reference) else {
                                return false;
                            };
                            let name = QualName::new(
                                None,
                                blitz::dom::ns!(),
                                LocalName::from(name.to_ascii_lowercase()),
                            );
                            doc.mutate().clear_attribute(node, name);
                            true
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_controls",
                    Func::from(move || -> String {
                        serde_json::to_string(&control_snapshot(&controls_dom.borrow()))
                            .unwrap_or_else(|_| "{\"controls\":[]}".into())
                    }),
                )?;
                globals.set(
                    "__lapui_set_text",
                    Func::from(move |id: String, value: String| {
                        if text_batch.borrow().depth > 0 {
                            text_batch
                                .borrow_mut()
                                .pending
                                .push(DomMutation::Text(id, value));
                        } else {
                            apply_mutations(&set_dom, vec![DomMutation::Text(id, value)]);
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_set_style",
                    Func::from(move |id: String, name: String, value: String| {
                        if style_batch.borrow().depth > 0 {
                            style_batch
                                .borrow_mut()
                                .pending
                                .push(DomMutation::Style(id, name, value));
                        } else {
                            apply_mutations(&style_dom, vec![DomMutation::Style(id, name, value)]);
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_get_style",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String, name: String| -> String {
                            let doc = dom.borrow();
                            serialized_inline_style(&doc, &reference)
                                .split(';')
                                .filter_map(|declaration| declaration.split_once(':'))
                                .find(|(property, _)| property.trim().eq_ignore_ascii_case(&name))
                                .map(|(_, value)| value.trim().to_owned())
                                .unwrap_or_default()
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_get_css_text",
                    Func::from({
                        let dom = dom.clone();
                        move |reference: String| -> String {
                            serialized_inline_style(&dom.borrow(), &reference)
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_remove_style",
                    Func::from(move |reference: String, name: String| -> bool {
                        if style_remove_batch.borrow().depth > 0 {
                            style_remove_batch
                                .borrow_mut()
                                .pending
                                .push(DomMutation::RemoveStyle(reference, name));
                            true
                        } else {
                            let mut doc = style_remove_dom.borrow_mut();
                            resolve_node_ref(&doc, &reference).is_some_and(|node| {
                                doc.mutate().remove_style_property(node, &name);
                                true
                            })
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_batch_begin",
                    Func::from(move || batch_begin.borrow_mut().depth += 1),
                )?;
                globals.set(
                    "__lapui_batch_end",
                    Func::from(move || {
                        let mutations = {
                            let mut batch = batch_end.borrow_mut();
                            batch.depth = batch.depth.saturating_sub(1);
                            if batch.depth == 0 {
                                std::mem::take(&mut batch.pending)
                            } else {
                                Vec::new()
                            }
                        };
                        apply_mutations(&mutation_dom, mutations);
                    }),
                )?;
                globals.set(
                    "__lapui_host_observe",
                    Func::from(move || serde_json::to_string(&observe_actions.observe()).unwrap()),
                )?;
                globals.set(
                    "__lapui_host_trace",
                    Func::from(move |after: String| {
                        let after = after.parse::<u64>().unwrap_or(u64::MAX);
                        serde_json::to_string(&trace_actions.trace(after)).unwrap()
                    }),
                )?;
                let invoke_trace=debug_trace.clone();
                globals.set(
                    "__lapui_host_invoke",
                    Func::from(
                        move |name: String,
                              request_id: i32,
                              args: String,
                              options: rquickjs::function::Opt<String>| {
                            let actions = invoke_actions.clone();
                            let tx = invoke_tx.clone();
                            let waker = invoke_waker.clone();
                            let proxy = invoke_proxy.clone();
                            if name.len() > 128 || args.len() > 64 * 1024 || options.0.as_ref().is_some_and(|options| options.len() > 1024) {
                                let _ = tx.send(Completion { trace_origin:None, request_id:Some(request_id),error_code:Some("invalid_arguments".into()),result:Err("action name/arguments/options exceed their 128-byte/64-KiB/1-KiB limits".into()) });
                                wake_document(&waker, &proxy, doc_id);
                                return;
                            }
                            let trace_origin=invoke_trace.origin(invoke_trace.instant("host_request",json!({"action":name}),false));
                            let failure_tx = tx.clone();
                            let failure_waker = waker.clone();
                            let failure_proxy = proxy.clone();
                            if let Err(error) = invoke_work.submit(move || {
                                let result = serde_json::from_str::<Value>(&args)
                                    .map_err(|error| {
                                        ActionError::new("invalid_arguments", error.to_string())
                                    })
                                    .and_then(|args| {
                                        let options = serde_json::from_str::<Value>(
                                            options.0.as_deref().unwrap_or("{}"),
                                        )
                                        .map_err(|error| {
                                            ActionError::new("invalid_request", error.to_string())
                                        })?;
                                        let options = InvokeOptions::parse(&options)?;
                                        actions
                                            .invoke_checked(
                                                &name,
                                                &args,
                                                options.request_id.as_deref(),
                                                options.expected_version,
                                            )
                                            .map(|observation| json!(observation))
                                    });
                                let error_code =
                                    result.as_ref().err().map(|error| error.code.clone());
                                let result = result.map_err(|error| error.message);
                                let _ = tx.send(Completion {
                                    trace_origin,
                                    request_id: Some(request_id),
                                    error_code,
                                    result,
                                });
                                wake_document(&waker, &proxy, doc_id);
                            }) {
                                let _ = failure_tx.send(Completion {
                                    trace_origin,
                                    request_id: Some(request_id),
                                    error_code: Some(error.code),
                                    result: Err(error.message),
                                });
                                wake_document(&failure_waker, &failure_proxy, doc_id);
                            }
                        },
                    ),
                )?;
                globals.set(
                    "__lapui_operation",
                    Func::from(move |id: String, cancel: bool| {
                        match if cancel {
                            operation_queries.cancel(&id)
                        } else {
                            operation_queries.get(&id)
                        } {
                            Ok(snapshot) => json!({"ok":true,"snapshot":snapshot}).to_string(),
                            Err(error) => json!({"ok":false,"error":error}).to_string(),
                        }
                    }),
                )?;
                let wait_tx = tx.clone();
                let wait_waker = waker.clone();
                let wait_proxy = proxy.clone();
                globals.set(
                    "__lapui_operation_wait",
                    Func::from(move |id: String, request_id: i32, after: String| {
                        let operations = operation_waits.clone();
                        let tx = wait_tx.clone();
                        let waker = wait_waker.clone();
                        let proxy = wait_proxy.clone();
                        let failure_tx = tx.clone();
                        let failure_waker = waker.clone();
                        let failure_proxy = proxy.clone();
                        if let Err(error) = wait_work.submit(move || {
                            let result = after
                                .parse::<u64>()
                                .map_err(|_| {
                                    ActionError::new(
                                        "invalid_request",
                                        "afterRevision must be a non-negative integer",
                                    )
                                })
                                .and_then(|revision| {
                                    operations.wait(&id, revision, Duration::from_secs(1))
                                })
                                .map(|snapshot| json!(snapshot));
                            let error_code = result.as_ref().err().map(|error| error.code.clone());
                            let _ = tx.send(Completion {
                                trace_origin:None,
                                request_id: Some(request_id),
                                error_code,
                                result: result.map_err(|error| error.message),
                            });
                            wake_document(&waker, &proxy, doc_id);
                        }) {
                            let _ = failure_tx.send(Completion {
                                trace_origin:None,
                                request_id: Some(request_id),
                                error_code: Some(error.code),
                                result: Err(error.message),
                            });
                            wake_document(&failure_waker, &failure_proxy, doc_id);
                        }
                    }),
                )?;
                globals.set("__lapui_changes_subscribe", Func::from({
                    let actions = actions.clone();
                    let work = changes_work.clone();
                    let tx = tx.clone();
                    let waker = waker.clone();
                    let proxy = proxy.clone();
                    let lifetime = lifetime.clone();
                    move |request_id: i32, input: String| -> String {
                        let parsed = if input.len() > 64 * 1024 {
                            Err(ActionError::new("invalid_request", "change request exceeds 64 KiB"))
                        } else {
                            serde_json::from_str::<crate::changes::ChangesRequest>(&input)
                                .map_err(|error| ActionError::new("invalid_request", error.to_string()))
                                .and_then(|request| { request.validate()?; Ok(request) })
                        };
                        let request = match parsed { Ok(request) => request, Err(error) => return json!(error).to_string() };
                        let actions = actions.clone();
                        let tx = tx.clone();
                        let waker = waker.clone();
                        let proxy = proxy.clone();
                        let lifetime = lifetime.clone();
                        match work.submit(move || {
                            let result = actions.subscribe_changes(request).map(|page| json!(page));
                            if lifetime.is_cancelled() { return; }
                            let error_code = result.as_ref().err().map(|error| error.code.clone());
                            let _ = tx.send(Completion { trace_origin:None, request_id: Some(request_id), error_code,
                                result: result.map_err(|error| error.message) });
                            wake_document(&waker, &proxy, doc_id);
                        }) {
                            Ok(()) => String::new(),
                            Err(error) => json!(error).to_string(),
                        }
                    }
                }))?;
                globals.set("__lapui_action_catalog", Func::from({
                    let actions = actions.clone();
                    let work = catalog_work.clone();
                    let tx = tx.clone();
                    let waker = waker.clone();
                    let proxy = proxy.clone();
                    let lifetime = lifetime.clone();
                    move |request_id: i32, input: String| -> String {
                        if input.len() > 64 * 1024 { return json!(ActionError::new("invalid_request", "action catalog request exceeds 64 KiB")).to_string(); }
                        let request: Value = match serde_json::from_str(&input) { Ok(value) => value,
                            Err(error) => return json!(ActionError::new("invalid_request", error.to_string())).to_string() };
                        let actions = actions.clone(); let tx = tx.clone(); let waker = waker.clone();
                        let proxy = proxy.clone(); let lifetime = lifetime.clone();
                        match work.submit(move || {
                            let result = actions.action_catalog_request(&request);
                            if lifetime.is_cancelled() { return; }
                            let error_code = result.as_ref().err().map(|error| error.code.clone());
                            let _ = tx.send(Completion { trace_origin:None, request_id: Some(request_id), error_code, result: result.map_err(|error| error.message) });
                            wake_document(&waker, &proxy, doc_id);
                        }) { Ok(()) => String::new(), Err(error) => json!(error).to_string() }
                    }
                }))?;
                let fetch_tx = tx.clone();
                let fetch_waker = waker.clone();
                let fetch_proxy = proxy.clone();
                let fetch_base_url = fetch_base_url.clone();
                let fetch_app_root = app_root.clone();
                let fetch_lifetime = lifetime.clone();
                let fetch_flags = fetch_requests.clone();
                let fetch_work = Rc::new(RefCell::new(None::<FetchWork>));
                let cancel_fetch = fetch_requests.clone();
                globals.set("__lapui_fetch_abort", Func::from(move |request_id: i32| {
                    if let Some(cancellation) = cancel_fetch.lock().unwrap().get(&request_id) {
                        cancellation.cancel();
                    }
                }))?;
                globals.set(
                    "__lapui_fetch",
                    Func::from(move |request_id: i32, input: String| -> String {
                        if input.len() > 64 * 1024 { return "invalid_arguments".into(); }
                        if fetch_flags.lock().unwrap().len() >= crate::fetch_work::MAX_OUTSTANDING {
                            return "network_busy".into();
                        }
                        if fetch_work.borrow().is_none() {
                            let work = match FetchWork::new(fetch_lifetime.clone()) {
                                Ok(work) => work,
                                Err(_) => return "network_error".into(),
                            };
                            *fetch_work.borrow_mut() = Some(work);
                        }
                        let tx = fetch_tx.clone();
                        let waker = fetch_waker.clone();
                        let proxy = fetch_proxy.clone();
                        let base_url = fetch_base_url.clone();
                        let app_root = fetch_app_root.clone();
                        let lifetime = fetch_lifetime.clone();
                        let cancellation = Cancellation::default();
                        fetch_flags.lock().unwrap().insert(request_id, cancellation.clone());
                        let result = fetch_work.borrow().as_ref().unwrap().submit(FetchRequest {
                            input, base_url, app_root, cancellation: cancellation.clone(),
                            complete: Box::new(move |result| {
                            if lifetime.is_cancelled() {
                                return;
                            }
                            let error_code = result.as_ref().err().map(|_| if cancellation.is_cancelled() { "abort_error" } else { "network_error" }.to_owned());
                            let _ = tx.send(Completion {
                                trace_origin:None,
                                request_id: Some(request_id),
                                error_code,
                                result,
                            });
                            wake_document(&waker, &proxy, doc_id);
                            }),
                        });
                        if result.is_err() {
                            fetch_flags.lock().unwrap().remove(&request_id);
                            return "network_error".into();
                        }
                        String::new()
                    }),
                )?;
                // One lazy reactor for the document's WebSocket and SSE transports.
                for (name, websocket) in [("__lapui_event_source_open", false), ("__lapui_websocket_open", true)] {
                    let streams = streams.clone();
                    let lifetime = lifetime.clone();
                    let waker = waker.clone();
                    let proxy = proxy.clone();
                    globals.set(name, Func::from(move |id: i32, url: String| -> String {
                        let result = (|| {
                            let mut streams = streams.borrow_mut();
                            if streams.is_none() {
                                let waker = waker.clone();
                                let proxy = proxy.clone();
                                *streams = Some(StreamWork::new(lifetime.clone(), move || wake_document(&waker, &proxy, doc_id))?);
                            }
                            streams.as_ref().unwrap().open(id, url, websocket)
                        })();
                        stream_error(result)
                    }))?;
                }
                for name in ["__lapui_event_source_close", "__lapui_websocket_close"] {
                    let streams = streams.clone();
                    globals.set(name, Func::from(move |id: i32| {
                        if let Some(streams) = streams.borrow().as_ref() { streams.close(id); }
                    }))?;
                }
                let text_streams = streams.clone();
                globals.set("__lapui_websocket_send_text", Func::from(move |id: i32, text: String| -> String {
                    stream_error(text_streams.borrow().as_ref().ok_or_else(|| ActionError::new("stream_closed", "WebSocket is closed"))
                        .and_then(|streams| streams.send(id, tokio_tungstenite::tungstenite::Message::Text(text.into()))))
                }))?;
                let binary_streams = streams.clone();
                globals.set("__lapui_websocket_send_binary", Func::from(move |id: i32, buffer: rquickjs::ArrayBuffer<'_>, offset: usize, length: usize| -> String {
                    let result = (|| {
                        if length > crate::stream_work::MAX_MESSAGE { return Err(ActionError::new("message_too_large", "WebSocket message exceeds 8 MiB")); }
                        // SAFETY: this synchronous native callback neither runs JS nor
                        // re-enters the runtime while reading. The argument keeps the
                        // buffer alive; copy before any callback/runtime execution.
                        let bytes = unsafe { buffer.as_bytes() }.ok_or_else(|| ActionError::new("invalid_arguments", "binary buffer is detached"))?;
                        let end = offset.checked_add(length).ok_or_else(|| ActionError::new("invalid_arguments", "binary view is outside its buffer"))?;
                        let bytes = bytes.get(offset..end).ok_or_else(|| ActionError::new("invalid_arguments", "binary view is outside its buffer"))?.to_vec();
                        binary_streams.borrow().as_ref().ok_or_else(|| ActionError::new("stream_closed", "WebSocket is closed"))?
                            .send(id, tokio_tungstenite::tungstenite::Message::Binary(bytes.into()))
                    })();
                    stream_error(result)
                }))?;
                let buffered_streams = streams.clone();
                globals.set("__lapui_websocket_buffered", Func::from(move |id: i32| -> usize {
                    buffered_streams.borrow().as_ref().map_or(0, |streams| streams.buffered(id))
                }))?;
                let status_streams = streams.clone();
                globals.set("__lapui_network_status", Func::from(move || -> String {
                    stream_status(&status_streams).to_string()
                }))?;
                globals.set(
                    "__lapui_report_script_error",
                    Func::from({
                        let diagnostics = diagnostics.clone();
                        move |source: String,
                              message: String,
                              stack: String,
                              phase: rquickjs::function::Opt<String>| {
                            let detail = if stack.is_empty() {
                                message
                            } else {
                                format!("{message}\n{stack}")
                            };
                            scripts::report(
                                &diagnostics,
                                &source,
                                phase.0.as_deref().unwrap_or("evaluate"),
                                detail,
                            );
                        }
                    }),
                )?;
                globals.set(
                    "__lapui_script_diagnostics",
                    Func::from({
                        let diagnostics = diagnostics.clone();
                        move || -> String {
                            serde_json::to_string(&*diagnostics.borrow())
                                .unwrap_or_else(|_| "[]".into())
                        }
                    }),
                )?;
                script_budget.run(STARTUP_LIMIT, || ctx.eval::<(), _>(BRIDGE))?;
                script_budget.run(STARTUP_LIMIT, || ctx.eval::<(), _>(RESIZE_OBSERVER))?;
                script_budget.run(STARTUP_LIMIT, || ctx.eval::<(), _>(INTERSECTION_OBSERVER))?;
                script_budget.run(STARTUP_LIMIT, || ctx.eval::<(), _>(FORM_BRIDGE))?;
                script_budget.run(STARTUP_LIMIT, || ctx.eval::<(), _>(MUTATION_OBSERVER))?;
                for script in &startup_scripts {
                    if script_budget.interrupted() { break; }
                    let result = script_budget.run(STARTUP_LIMIT, || -> rquickjs::Result<()> {
                        match script {
                            StartupScript::Classic { name, source } => {
                                let mut options = EvalOptions::default();
                                options.filename = Some(name.clone());
                                ctx.eval_with_options::<(), _>(source.as_str(), options)?;
                            }
                            StartupScript::Module { name, source } => {
                                let promise = if let Some(source) = source {
                                    let module = Module::declare(
                                        ctx.clone(),
                                        name.as_str(),
                                        source.as_str(),
                                    )?;
                                    module.meta()?.set("url", name.as_str())?;
                                    module.eval()?.1
                                } else {
                                    Module::import(&ctx, name.as_str())?
                                };
                                let tracker: Function = globals.get("__lapui_track_module")?;
                                tracker.call::<_, ()>((name.as_str(), promise))?;
                            }
                        }
                        Ok(())
                    });
                    if let Err(error) = result {
                        let detail = script_budget.run(CALLBACK_LIMIT, || ctx
                            .catch()
                            .into_object()
                            .and_then(Exception::from_object)
                            .map(|exception| {
                                format!(
                                    "{}\n{}",
                                    exception.message().unwrap_or_default(),
                                    exception.stack().unwrap_or_default()
                                )
                            }));
                        scripts::report(
                            &diagnostics,
                            script.name(),
                            "evaluate",
                            if script_budget.interrupted() { INTERRUPTED_MESSAGE.into() } else { detail.unwrap_or_else(|| error.to_string()) },
                        );
                    } else if script_budget.interrupted() {
                        scripts::report(&diagnostics, script.name(), "evaluate", INTERRUPTED_MESSAGE.into());
                    }
                }
                Ok(())
            })
            .map_err(|e| {
                lifetime.cancel();
                format!("JavaScript setup failed: {e}")
            })?;
        if let Err(message) = drain_jobs(&js_runtime, &script_budget) {
            scripts::report(&diagnostics, "startup-microtask", "evaluate", message);
        }
        if !script_budget.interrupted() && gc_requested.replace(false) {
            js_runtime.run_gc();
            if let Err(message) = drain_jobs(&js_runtime, &script_budget) {
                scripts::report(&diagnostics, "startup-gc", "evaluate", message);
            }
        }
        if !script_budget.interrupted() && js_runtime.is_job_pending() {
            wake_document(&waker, &proxy, doc_id);
        }

        let forward_tx = tx;
        let forward_waker = waker.clone();
        let forward_proxy = proxy.clone();
        let forward_lifetime = lifetime.clone();
        std::thread::spawn(move || {
            while let Ok(result) = external_rx.recv() {
                if forward_lifetime.is_cancelled() {
                    break;
                }
                if forward_tx
                    .send(Completion {
                        trace_origin: None,
                        request_id: None,
                        error_code: Some("action_failed".into()),
                        result,
                    })
                    .is_err()
                {
                    break;
                }
                wake_document(&forward_waker, &forward_proxy, doc_id);
            }
        });

        let document = Self {
            debug_trace,
            actions,
            action_scopes: Vec::new(),
            dom,
            js_runtime,
            js_context,
            script_budget,
            script_stopped: Cell::new(false),
            completions,
            waker,
            streams,
            fetch_requests,
            controller,
            control_requests,
            script_diagnostics: diagnostics,
            lifetime,
            forward_shutdown: external_tx.clone(),
            timers,
            proxy,
            gc_requested,
            frames,
        };
        document.stop_faulted_script();
        Ok((document, external_tx))
    }
}

pub(crate) async fn perform_fetch(
    input: &str,
    base_url: Option<&str>,
    app_root: Option<&Path>,
    client: &reqwest::Client,
) -> Result<Value, String> {
    let request: Value = serde_json::from_str(input).map_err(|error| error.to_string())?;
    let url = request
        .get("url")
        .and_then(Value::as_str)
        .ok_or("fetch requires a URL")?;
    let parsed = match reqwest::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(parse_error) => {
            let Some(base_url) = base_url else {
                return Err(format!("invalid fetch URL: {parse_error}"));
            };
            reqwest::Url::parse(base_url)
                .and_then(|base| base.join(url))
                .map_err(|error| format!("invalid fetch URL: {error}"))?
        }
    };
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET");
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|error| format!("invalid fetch method: {error}"))?;
    if parsed.scheme() == "file" {
        if method != reqwest::Method::GET {
            return Err("local file fetch supports GET only".into());
        }
        let root = app_root.ok_or("local file fetch requires a local app document")?;
        let path = parsed
            .to_file_path()
            .map_err(|_| "invalid local file URL")?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if !path.starts_with(root) {
            return Err("local file fetch is outside the app directory".into());
        }
        let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("local file fetch target is not a file".into());
        }
        const MAX_LOCAL_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
        if metadata.len() > MAX_LOCAL_RESPONSE_BYTES {
            return Err("local fetch response exceeds the 8 MiB limit".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .map_err(|error| error.to_string())?
            .take(MAX_LOCAL_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_LOCAL_RESPONSE_BYTES {
            return Err("local fetch response exceeds the 8 MiB limit".into());
        }
        let content_type = match path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "css" => "text/css",
            "html" | "htm" => "text/html",
            "js" | "mjs" => "text/javascript",
            "json" => "application/json",
            "svg" => "image/svg+xml",
            "txt" => "text/plain",
            _ => "application/octet-stream",
        };
        return Ok(json!({
            "status": 200,
            "ok": true,
            "url": parsed.as_str(),
            "headers": {"content-type": content_type, "content-length": bytes.len().to_string()},
            "body": String::from_utf8_lossy(&bytes).into_owned(),
        }));
    }
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("fetch supports only http, https, and app-local file URLs".into());
    }
    let headers = request.get("headers").and_then(Value::as_object);
    let body = request
        .get("body")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut request_builder = client.request(method, parsed);
    if let Some(headers) = headers {
        for (name, value) in headers {
            let value = value
                .as_str()
                .ok_or("fetch header values must be strings")?;
            request_builder = request_builder.header(name, value);
        }
    }
    if let Some(body) = body {
        request_builder = request_builder.body(body);
    }
    let mut response = request_builder
        .send()
        .await
        .map_err(|error| error.to_string())?;
    const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("fetch response exceeds the 8 MiB limit".into());
    }
    let status = response.status();
    let final_url = response.url().to_string();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.to_string(), json!(value)))
        })
        .collect::<serde_json::Map<_, _>>();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("fetch response exceeds the 8 MiB limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(json!({
        "status": status.as_u16(),
        "ok": status.is_success(),
        "url": final_url,
        "headers": headers,
        "body": String::from_utf8_lossy(&bytes).into_owned(),
    }))
}

fn stream_error(result: Result<(), ActionError>) -> String {
    result
        .err()
        .map(|error| serde_json::to_string(&error).unwrap())
        .unwrap_or_default()
}

fn stream_value<'js>(
    ctx: rquickjs::Ctx<'js>,
    data: StreamData,
) -> rquickjs::Result<rquickjs::Object<'js>> {
    let object = rquickjs::Object::new(ctx.clone())?;
    match data {
        StreamData::Binary(bytes) => {
            object.set("type", "message")?;
            object.set("dataType", "binary")?;
            // A managed copy is accounted by QuickJS's heap budget, unlike
            // an externally backed ArrayBuffer allocated on the Rust heap.
            object.set("data", rquickjs::ArrayBuffer::new_copy(ctx, bytes)?)?;
        }
        StreamData::Fields(value) => {
            for (name, value) in value.as_object().expect("native stream event fields") {
                match value {
                    Value::String(value) => object.set(name.as_str(), value.as_str())?,
                    Value::Bool(value) => object.set(name.as_str(), *value)?,
                    Value::Number(value) => object.set(name.as_str(), value.as_f64().unwrap())?,
                    Value::Null => {
                        object.set(name.as_str(), rquickjs::Value::new_null(ctx.clone()))?
                    }
                    _ => {
                        return Err(rquickjs::Error::new_from_js(
                            "nested stream data",
                            "primitive event field",
                        ))
                    }
                }
            }
        }
    }
    Ok(object)
}

fn flush_layout(
    dom: &Rc<RefCell<BaseDocument>>,
    batch: &Rc<RefCell<MutationBatch>>,
    frames: &Rc<RefCell<Frames>>,
) {
    // Synchronous reads commit writes without ending an enclosing app batch.
    let pending = std::mem::take(&mut batch.borrow_mut().pending);
    if !pending.is_empty() {
        apply_mutations(dom, pending);
    }
    let span = frames
        .borrow()
        .debug_trace
        .span("layout_read", json!({}), None);
    dom.borrow_mut().resolve(frames.borrow().layout_time);
    span.finish(json!({"outcome":"resolved"}), false);
}

fn stream_status(streams: &Rc<RefCell<Option<StreamWork>>>) -> Value {
    streams.borrow().as_ref().map_or_else(|| json!({"streams":0,"queuedEvents":0,"queuedEventBytes":0,"outgoingBytes":0,"reactorStarted":false}), StreamWork::stats)
}

fn wake_document(
    waker: &Arc<Mutex<Option<Waker>>>,
    proxy: &Option<BlitzShellProxy>,
    doc_id: usize,
) {
    if let Some(waker) = waker.lock().unwrap().as_ref() {
        waker.wake_by_ref();
    } else if let Some(proxy) = proxy {
        // Before the first poll there is no waker. A redraw generates a window event,
        // which gives Blitz an opportunity to poll the queued completion.
        proxy.send_event(BlitzShellEvent::RequestRedraw { doc_id });
    }
}

fn drain_jobs(runtime: &Runtime, budget: &ScriptBudget) -> Result<(), String> {
    if budget.interrupted() {
        return Ok(());
    }
    let started = Instant::now();
    for index in 0..1024 {
        if index > 0 && started.elapsed() >= CHECKPOINT_SLICE {
            return Ok(());
        }
        let result = budget.run(CALLBACK_LIMIT, || runtime.execute_pending_job());
        match result {
            Ok(_) if budget.interrupted() => return Err(INTERRUPTED_MESSAGE.into()),
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(err) => {
                return Err(javascript_error(
                    &err.0,
                    &rquickjs::Error::Exception,
                    budget,
                ))
            }
        }
    }
    eprintln!("JavaScript microtask checkpoint exceeded 1024 jobs");
    Ok(())
}

struct JsHandler {
    runtime: Runtime,
    context: Context,
    budget: ScriptBudget,
    diagnostics: ScriptDiagnostics,
}

impl EventHandler for JsHandler {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        doc: &mut dyn Document,
        state: &mut EventState,
    ) {
        if self.budget.interrupted() {
            state.prevent_default();
            return;
        }
        let inner = doc.inner();
        let doc_id = inner.id();
        if inner.get_node(event.target).is_none() {
            return;
        }
        if let Some(control) = crate::forms::interaction_control(&inner, event.target) {
            let editing = match &event.data {
                blitz::traits::events::DomEventData::Ime(_) => true,
                blitz::traits::events::DomEventData::KeyDown(key) => {
                    let shortcut = key.modifiers.ctrl() || key.modifiers.meta();
                    (!shortcut && key.text.is_some())
                        || matches!(
                            key.key,
                            keyboard_types::Key::Backspace
                                | keyboard_types::Key::Delete
                                | keyboard_types::Key::Enter
                        )
                        || (shortcut
                            && matches!(
                                key.key.to_string().to_ascii_lowercase().as_str(),
                                "v" | "x"
                            ))
                }
                _ => false,
            };
            let tab = matches!(&event.data, blitz::traits::events::DomEventData::KeyDown(key) if key.key == keyboard_types::Key::Tab);
            let interaction = matches!(
                &event.data,
                blitz::traits::events::DomEventData::PointerDown(_)
                    | blitz::traits::events::DomEventData::MouseDown(_)
                    | blitz::traits::events::DomEventData::KeyDown(_)
                    | blitz::traits::events::DomEventData::Ime(_)
            );
            if (!crate::forms::enabled(&inner, control) && interaction && !tab)
                || (crate::forms::read_only(&inner, control) && editing)
            {
                state.prevent_default();
            }
        }
        let target_id = canonical_node_ref(doc_id, event.target);
        let path = chain
            .iter()
            .filter(|node_id| inner.get_node(**node_id).is_some())
            .map(|node_id| canonical_node_ref(doc_id, *node_id))
            .collect::<Vec<_>>()
            .join("\n");
        drop(inner);

        let detail = match &event.data {
            blitz::traits::events::DomEventData::KeyDown(key)
            | blitz::traits::events::DomEventData::KeyUp(key) => json!({
                "key": key.key.to_string(),
                "code": key.code.to_string(),
                "location": key.location as u8,
                "ctrlKey": key.modifiers.ctrl(),
                "shiftKey": key.modifiers.shift(),
                "altKey": key.modifiers.alt(),
                "metaKey": key.modifiers.meta(),
                "repeat": key.is_auto_repeating,
                "isComposing": key.is_composing,
                "text": key.text.as_ref().map(ToString::to_string),
            }),
            _ => json!({}),
        }
        .to_string();
        let result = self.budget.run(CALLBACK_LIMIT, || {
            self.context.with(|ctx| {
                let function: Function = ctx.globals().get("__lapui_dispatch")?;
                function.call::<_, String>((event.name(), target_id, path, detail))
            })
        });
        let outcome = match result {
            Ok(outcome) => serde_json::from_str::<Value>(&outcome).unwrap_or_default(),
            Err(err) => {
                let message = javascript_error(&self.context, &err, &self.budget);
                scripts::report(&self.diagnostics, event.name(), "event", message);
                state.prevent_default();
                state.request_redraw();
                Value::Null
            }
        };
        let dispatched = outcome["dispatched"].as_bool().unwrap_or(false);
        if outcome["nativeDefaultHandled"].as_bool().unwrap_or(false)
            || (event.cancelable && outcome["defaultPrevented"].as_bool().unwrap_or(false))
        {
            state.prevent_default();
        }
        if let Err(err) = drain_jobs(&self.runtime, &self.budget) {
            scripts::report(&self.diagnostics, "microtask", "evaluate", err);
        }
        if self.budget.interrupted() {
            state.prevent_default();
            state.request_redraw();
            return;
        }
        if let blitz::traits::events::DomEventData::KeyDown(key) = &event.data {
            if key.key == keyboard_types::Key::Tab && !state.is_cancelled() {
                crate::forms::focus_step(&mut doc.inner_mut(), key.modifiers.shift());
                state.prevent_default();
            }
        }
        if dispatched || outcome["nativeDefaultHandled"].as_bool().unwrap_or(false) {
            state.request_redraw();
        }
    }
}

impl Document for LapuiDocument {
    fn inner(&self) -> DocGuard<'_> {
        DocGuard::RefCell(self.dom.borrow())
    }

    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        DocGuardMut::RefCell(self.dom.borrow_mut())
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        if self.script_budget.interrupted() {
            self.stop_faulted_script();
            return;
        }
        let kind = match &event {
            UiEvent::PointerMove(_) => "pointerMove",
            UiEvent::PointerUp(_) => "pointerUp",
            UiEvent::PointerDown(_) => "pointerDown",
            UiEvent::PointerCancel(_) => "pointerCancel",
            UiEvent::Wheel(_) => "wheel",
            UiEvent::KeyUp(_) => "keyUp",
            UiEvent::KeyDown(_) => "keyDown",
            UiEvent::Ime(_) => "ime",
            UiEvent::AppleStandardKeybinding(_) => "standardKeybinding",
        };
        let input_trace = self
            .debug_trace
            .span("native_input", json!({"kind":kind}), None);
        let doc_id = self.dom.borrow().id();
        let tab_pressed = matches!(
            &event,
            UiEvent::KeyDown(key) if key.key == keyboard_types::Key::Tab
        );
        let previous_focus = tab_pressed
            .then(|| self.dom.borrow().get_focussed_node_id())
            .flatten();
        // The driver drops its document guard before calling the handler, so JS may mutate the DOM.
        let handler = JsHandler {
            runtime: self.js_runtime.clone(),
            context: self.js_context.clone(),
            budget: self.script_budget.clone(),
            diagnostics: self.script_diagnostics.clone(),
        };
        let mut driver = EventDriver::new(self, handler);
        driver.handle_ui_event(event);
        if tab_pressed && !self.script_budget.interrupted() {
            let current_focus = self.dom.borrow().get_focussed_node_id();
            if previous_focus != current_focus {
                let notified_focus = self.script_budget.run(CALLBACK_LIMIT, || {
                    self.js_context.with(|ctx| {
                        let function: Function =
                            ctx.globals().get("__lapui_take_focus_transition")?;
                        function.call::<_, String>(())
                    })
                });
                let notified_focus = notified_focus.unwrap_or_default();
                let previous_ref = match notified_focus.as_str() {
                    "" => previous_focus
                        .map(|node| canonical_node_ref(doc_id, node))
                        .unwrap_or_default(),
                    "lapui-focus-cleared" => String::new(),
                    reference => reference.to_owned(),
                };
                let current_ref = current_focus
                    .map(|node| canonical_node_ref(doc_id, node))
                    .unwrap_or_default();
                if previous_ref != current_ref {
                    let transition = self.script_budget.run(CALLBACK_LIMIT, || {
                        self.js_context.with(|ctx| {
                            let function: Function =
                                ctx.globals().get("__lapui_dispatch_focus_transition")?;
                            function.call::<_, ()>((previous_ref, current_ref))
                        })
                    });
                    if let Err(error) = transition {
                        let message =
                            javascript_error(&self.js_context, &error, &self.script_budget);
                        scripts::report(
                            &self.script_diagnostics,
                            "focus-transition",
                            "event",
                            message,
                        );
                    }
                    if let Err(error) = drain_jobs(&self.js_runtime, &self.script_budget) {
                        scripts::report(&self.script_diagnostics, "microtask", "evaluate", error);
                    }
                }
            }
        }
        self.resume_pending_jobs();
        input_trace.finish(
            json!({"outcome":if self.script_budget.interrupted(){"suspended"}else{"handled"}}),
            true,
        );
    }

    fn poll(&mut self, context: Option<TaskContext>) -> bool {
        if let Some(context) = context {
            *self.waker.lock().unwrap() = Some(context.waker().clone());
        }
        let mut changed = false;
        if !self.script_budget.interrupted() && self.js_runtime.is_job_pending() {
            let jobs_trace = self.debug_trace.span("microtasks", json!({}), None);
            if let Err(message) = drain_jobs(&self.js_runtime, &self.script_budget) {
                scripts::report(&self.script_diagnostics, "microtask", "evaluate", message);
            }
            jobs_trace.finish(
                json!({"scriptSuspended":self.script_budget.interrupted()}),
                true,
            );
            changed = true;
        }
        for _ in 0..1024 {
            let Ok(id) = self.timers.expired.try_recv() else {
                break;
            };
            if self.script_budget.interrupted() {
                continue;
            }
            let timer_trace = self
                .debug_trace
                .span("timer", json!({"callbackId":id}), None);
            let result = self.script_budget.run(CALLBACK_LIMIT, || {
                self.js_context.with(|ctx| {
                    let callback: Function = ctx.globals().get("__lapui_fire_timer")?;
                    callback.call::<_, ()>((id,))
                })
            });
            if let Err(error) = result {
                let message = javascript_error(&self.js_context, &error, &self.script_budget);
                scripts::report(
                    &self.script_diagnostics,
                    &format!("timer:{id}"),
                    "timer",
                    message,
                );
            }
            if let Err(error) = drain_jobs(&self.js_runtime, &self.script_budget) {
                scripts::report(&self.script_diagnostics, "microtask", "evaluate", error);
            }
            timer_trace.finish(
                json!({"scriptSuspended":self.script_budget.interrupted()}),
                true,
            );
            changed = true;
        }
        for _ in 0..64 {
            let Ok(request) = self.control_requests.try_recv() else {
                break;
            };
            if !request.start() {
                continue;
            }
            let method = request
                .command
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("");
            let mutating = matches!(method, "activate" | "fill" | "check" | "focus");
            let diagnostic_before = self
                .script_diagnostics
                .borrow()
                .back()
                .map_or(0, |item| item.sequence);
            let span = mutating.then(|| {
                self.debug_trace
                    .span("control", json!({"method":method}), None)
            });
            let mut result = self.execute_control_command(&request.command);
            if let (Some(sequence), Ok(value)) =
                (span.as_ref().and_then(Span::sequence), &mut result)
            {
                value["debugTraceSequence"] = json!(sequence);
            }
            if let Some(span) = span {
                span.finish(
                    json!({"outcome":if result.is_ok(){"dispatched"}else{"rejected"},"diagnosticsBeforeSequence":diagnostic_before,"diagnosticsAfterSequence":self.script_diagnostics.borrow().back().map_or(0,|item|item.sequence)}),
                    true,
                );
            }
            // Read-only trace queries do not create their own redraw/trace loop.
            changed |= !matches!(
                method,
                "debugTrace.read" | "debugTrace.configure" | "runtime.memoryUsage" | "pageChanges"
            );
            request.finish(result);
        }
        let mut completion_count = 0;
        for _ in 0..64 {
            let Ok(completion) = self.completions.try_recv() else {
                break;
            };
            completion_count += 1;
            if let Some(request_id) = completion.request_id {
                self.fetch_requests.lock().unwrap().remove(&request_id);
            }
            if self.script_budget.interrupted() {
                continue;
            }
            let completion_trace=self.debug_trace.span("completion",json!({"hostRequestLinked":self.debug_trace.live_origin(completion.trace_origin).is_some(),
                "promiseId":completion.request_id,"observedStateVersion":self.actions.version()}),self.debug_trace.live_origin(completion.trace_origin));
            let (ok, payload) = match completion.result {
                Ok(value) => (true, value.to_string()),
                Err(message) => (
                    false,
                    json!({"code":completion.error_code.unwrap_or_else(|| "action_failed".into()),"message":message}).to_string(),
                ),
            };
            let result = self.script_budget.run(CALLBACK_LIMIT, || {
                self.js_context.with(|ctx| {
                    let globals = ctx.globals();
                    let function: Function = globals.get("__lapui_complete")?;
                    function.call::<_, ()>((completion.request_id.unwrap_or(0), ok, payload))
                })
            });
            if let Err(err) = result {
                let message = javascript_error(&self.js_context, &err, &self.script_budget);
                scripts::report(
                    &self.script_diagnostics,
                    "host-completion",
                    "completion",
                    message,
                );
            }
            if let Err(err) = drain_jobs(&self.js_runtime, &self.script_budget) {
                scripts::report(&self.script_diagnostics, "microtask", "evaluate", err);
            }
            completion_trace.finish(json!({"outcome":if ok{"resolved"}else{"rejected"},"scriptSuspended":self.script_budget.interrupted()}),true);
            changed = true;
        }
        // Release the queue borrow before entering JS: stream handlers can
        // send, close, or construct replacement streams. This time slice is
        // checked between deliveries; individual callbacks have their own limit.
        let stream_deadline = Instant::now() + CHECKPOINT_SLICE;
        for _ in 0..16 {
            if Instant::now() >= stream_deadline {
                break;
            }
            let queued = self
                .streams
                .borrow_mut()
                .as_mut()
                .and_then(|streams| streams.events.try_recv().ok());
            let Some(queued) = queued else { break };
            let (id, data, _delivery_bytes) = queued.take();
            if self.script_budget.interrupted() {
                continue;
            }
            let stream_trace =
                self.debug_trace
                    .span("stream_delivery", json!({"streamId":id}), None);
            let result = self.script_budget.run(CALLBACK_LIMIT, || {
                self.js_context.with(|ctx| {
                    let event = stream_value(ctx.clone(), data)?;
                    let function: Function = ctx.globals().get("__lapui_stream_event")?;
                    function.call::<_, ()>((id, event))
                })
            });
            if let Err(error) = result {
                scripts::report(
                    &self.script_diagnostics,
                    "network-stream",
                    "event",
                    javascript_error(&self.js_context, &error, &self.script_budget),
                );
            }
            if let Err(error) = drain_jobs(&self.js_runtime, &self.script_budget) {
                scripts::report(&self.script_diagnostics, "microtask", "evaluate", error);
            }
            stream_trace.finish(
                json!({"scriptSuspended":self.script_budget.interrupted()}),
                true,
            );
            changed = true;
        }
        self.resume_pending_jobs();
        if completion_count == 64
            || self
                .streams
                .borrow()
                .as_ref()
                .is_some_and(|streams| !streams.events.is_empty())
        {
            wake_document(&self.waker, &self.proxy, self.dom.borrow().id());
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz::traits::shell::{ColorScheme, Viewport};
    use cursor_icon::CursorIcon;
    use keyboard_types::{Code, Key, Location, Modifiers};
    use std::time::Duration;

    #[test]
    fn quickjs_memory_usage_exposes_bounded_heap_counters_and_explicit_collection() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body><p>memory</p></body></html>",
            "globalThis.__lapui_memory_probe = Array.from({length:4096}, (_, i) => ({i, value:'x'.repeat(64)}));",
        )
        .unwrap();
        let measured = control_request(&mut doc, json!({"method":"runtime.memoryUsage"})).unwrap();
        assert_eq!(measured["collectionRequested"], false);
        let usage = &measured["usage"];
        assert!(usage["allocatorBytes"].as_i64().unwrap() > 0);
        assert!(usage["heapUsedBytes"].as_i64().unwrap() > 0);
        assert!(usage["arrays"]["count"].as_i64().unwrap() > 0);
        assert!(usage["javascriptFunctions"]["count"].as_i64().unwrap() > 0);

        let collected = control_request(
            &mut doc,
            json!({"method":"runtime.memoryUsage","collectGarbage":true}),
        )
        .unwrap();
        assert_eq!(collected["collectionRequested"], true);
        assert!(
            collected["beforeCollection"]["heapUsedBytes"]
                .as_i64()
                .unwrap()
                > 0
        );
        assert!(
            collected["afterCollection"]["allocatorBytes"]
                .as_i64()
                .unwrap()
                > 0
        );
        assert_eq!(
            control_request(
                &mut doc,
                json!({"method":"runtime.memoryUsage","unexpected":true}),
            )
            .unwrap_err()
            .code,
            "invalid_request"
        );
        assert_eq!(
            control_request(
                &mut doc,
                json!({"method":"runtime.memoryUsage","collectGarbage":"yes"}),
            )
            .unwrap_err()
            .code,
            "invalid_request"
        );
    }

    #[test]
    fn debug_trace_shared_controls_async_host_frames_privacy_and_configuration_are_coherent() {
        let (mut doc,_)=LapuiDocument::new_with_source(ActionRegistry::default(),None,
            "<html><head><style>html,body{margin:0}#out{width:40px;height:40px;background:red}</style></head><body><div id='out'></div><input id='secret' type='password'><button id='run'>Run</button><button id='bad'>Error</button></body></html>",
            "document.getElementById('run').onclick=async()=>{await lapui.invoke('counter.increment',{});document.getElementById('out').style.backgroundColor='lime';globalThis.done=true;};document.getElementById('bad').onclick=()=>{throw Error('private-error-message');};").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(240, 240, 1.0, ColorScheme::Light));
        let epoch = doc.inner().id();
        let snapshot = control_request(&mut doc, json!({"method":"controls"})).unwrap();
        let reference = |name: &str| {
            snapshot["controls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == name)
                .unwrap()["ref"]
                .clone()
        };
        assert_eq!(doc.debug_trace(0, 128).unwrap()["latestSequence"], 0);
        let configured = control_request(
            &mut doc,
            json!({"method":"debugTrace.configure","documentEpoch":epoch,"enabled":true}),
        )
        .unwrap();
        assert_eq!(configured["enabled"], true);
        control_request(&mut doc,json!({"method":"fill","documentEpoch":epoch,"ref":reference("secret"),"value":"private-input-value"})).unwrap();
        doc.inner_mut().resolve(0.0);
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("document.getElementById('secret').focus()"))
            .unwrap();
        doc.handle_ui_event(UiEvent::Ime(blitz::traits::events::BlitzImeEvent::Commit(
            "private-ime-value".into(),
        )));
        let dispatched = control_request(
            &mut doc,
            json!({"method":"activate","documentEpoch":epoch,"ref":reference("run")}),
        )
        .unwrap();
        let control_sequence = dispatched["debugTraceSequence"].as_u64().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("globalThis.done===true"))
            .unwrap()
        {
            doc.poll(None);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        control_request(
            &mut doc,
            json!({"method":"activate","documentEpoch":epoch,"ref":reference("bad")}),
        )
        .unwrap();
        #[cfg(feature = "software-renderer")]
        {
            let pixels = crate::snapshot::render_rgba(&mut doc, 240, 240).unwrap();
            assert_eq!(
                &pixels[(10 * 240 + 10) * 4..(10 * 240 + 10) * 4 + 4],
                &[0, 255, 0, 255]
            );
        }
        #[cfg(not(feature = "software-renderer"))]
        {
            let frame = doc.frame_trace("test");
            doc.animation_frame();
            doc.rendering_update();
            let layout = doc.layout_trace();
            doc.inner_mut().resolve(0.0);
            layout.finish(json!({"outcome":"resolved"}), false);
            frame.finish(
                json!({"outcome":"no_renderer","physicalPresentation":"unknown"}),
                false,
            );
        }
        let page = control_request(
            &mut doc,
            json!({"method":"debugTrace.read","documentEpoch":epoch,"limit":128}),
        )
        .unwrap();
        let records = page["records"].as_array().unwrap();
        let host = records
            .iter()
            .find(|item| item["kind"] == "host_request")
            .unwrap();
        assert_eq!(host["parentSequence"], control_sequence);
        assert_eq!(host["data"]["action"], "counter.increment");
        let completion = records
            .iter()
            .find(|item| item["kind"] == "completion" && item["phase"] == "start")
            .unwrap();
        assert_eq!(completion["parentSequence"], host["sequence"]);
        assert_eq!(completion["data"]["hostRequestLinked"], true);
        assert_eq!(completion["data"]["observedStateVersion"], 1);
        let completed = records
            .iter()
            .find(|item| item["kind"] == "completion" && item["phase"] == "end")
            .unwrap();
        let frame = records
            .iter()
            .find(|item| item["kind"] == "frame" && item["phase"] == "start")
            .unwrap();
        assert!(frame["data"]["causes"]
            .as_array()
            .unwrap()
            .contains(&completed["sequence"]));
        assert_eq!(frame["data"]["observedStateVersion"], 1);
        let layout = records
            .iter()
            .find(|item| item["kind"] == "layout" && item["phase"] == "start")
            .unwrap();
        assert_eq!(layout["parentSequence"], frame["sequence"]);
        let error = records
            .iter()
            .find(|item| {
                item["kind"] == "control"
                    && item["phase"] == "end"
                    && item["data"]["diagnosticsAfterSequence"].as_u64().unwrap() > 0
            })
            .unwrap();
        assert_eq!(error["data"]["diagnosticsBeforeSequence"], 0);
        for secret in [
            "private-input-value",
            "private-error-message",
            "private-ime-value",
        ] {
            assert!(!page.to_string().contains(secret));
        }
        let last = page["latestSequence"].as_u64().unwrap();
        let read = doc.controller();
        let caller = std::thread::spawn(move || {
            read.request(
                json!({"method":"debugTrace.read","documentEpoch":epoch}),
                Duration::from_secs(2),
            )
        });
        // Process the read through the normal poll without fabricating a redraw.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !caller.is_finished() {
            assert!(!doc.poll(None));
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        caller.join().unwrap().unwrap();
        assert_eq!(doc.debug_trace(0, 128).unwrap()["latestSequence"], last);
        for request in [
            json!({"method":"debugTrace.read","documentEpoch":epoch,"limit":129}),
            json!({"method":"debugTrace.read","documentEpoch":epoch,"limit":-1}),
            json!({"method":"debugTrace.configure","documentEpoch":epoch,"enabled":"true"}),
            json!({"method":"debugTrace.read","documentEpoch":epoch,"extra":"ignored?"}),
        ] {
            assert_eq!(
                control_request(&mut doc, request).unwrap_err().code,
                "invalid_request"
            );
        }
        assert_eq!(
            control_request(
                &mut doc,
                json!({"method":"debugTrace.read","documentEpoch":epoch+1})
            )
            .unwrap_err()
            .code,
            "stale_document"
        );
        control_request(
            &mut doc,
            json!({"method":"debugTrace.configure","documentEpoch":epoch,"enabled":false}),
        )
        .unwrap();
        let dispatched=control_request(&mut doc,json!({"method":"fill","documentEpoch":epoch,"ref":reference("secret"),"value":"private-new-value"})).unwrap();
        assert!(dispatched.get("debugTraceSequence").is_none());
        assert_eq!(doc.debug_trace(0, 128).unwrap()["latestSequence"], last);
    }

    #[test]
    fn debug_trace_cleared_session_rejects_delayed_host_parent_and_survives_script_suspension() {
        let (mut doc,_)=LapuiDocument::new_with_source(ActionRegistry::default(),None,
            "<html><body><button id='run'>Run</button><button id='loop'>Loop</button></body></html>",
            "document.getElementById('run').onclick=()=>{lapui.invoke('counter.increment',{}).then(()=>globalThis.done=true);lapui.debugTrace.configure({enabled:false});lapui.debugTrace.configure({enabled:true,clear:true});};document.getElementById('loop').onclick=()=>{while(true){}};").unwrap();
        doc.configure_debug_trace(true, false);
        let epoch = doc.inner().id();
        let snapshot = control_snapshot(&doc.inner());
        let reference = |name: &str| {
            snapshot["controls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == name)
                .unwrap()["ref"]
                .clone()
        };
        control_request(
            &mut doc,
            json!({"method":"activate","documentEpoch":epoch,"ref":reference("run")}),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("globalThis.done===true"))
            .unwrap()
        {
            doc.poll(None);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let page = doc.debug_trace(0, 128).unwrap();
        assert_eq!(page["resyncRequired"], true);
        let completion = page["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"] == "completion" && item["phase"] == "start")
            .unwrap();
        assert_eq!(completion["parentSequence"], Value::Null);
        assert_eq!(completion["data"]["hostRequestLinked"], false);
        assert_eq!(
            control_request(
                &mut doc,
                json!({"method":"activate","documentEpoch":epoch,"ref":reference("loop")})
            )
            .unwrap_err()
            .code,
            "script_error"
        );
        let page = control_request(
            &mut doc,
            json!({"method":"debugTrace.read","documentEpoch":epoch}),
        )
        .unwrap();
        assert_eq!(page["enabled"], true);
        assert!(page["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "control" && item["data"]["outcome"] == "rejected"));
        assert_eq!(control_request(&mut doc,json!({"method":"debugTrace.configure","documentEpoch":epoch,"enabled":false,"clear":true})).unwrap()["records"],json!([]));
    }

    #[test]
    fn local_form_native_enter_submission_obeys_cancel_repeat_composition_and_default_button() {
        use blitz::traits::events::{BlitzKeyEvent, KeyState};
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None,
            "<html><body><form id='form'><input id='name' value='Ada'><button id='send'>Send</button><button id='later'>Later</button></form></body></html>",
            "globalThis.calls=[];document.getElementById('form').onsubmit=e=>{e.preventDefault();calls.push(e.submitter?.id??null);};document.getElementById('name').focus();").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(480, 400, 1.0, ColorScheme::Light));
        doc.inner_mut().resolve(0.0);
        let enter = |repeat, composing| {
            UiEvent::KeyDown(BlitzKeyEvent {
                key: Key::Enter,
                code: Code::Enter,
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                is_auto_repeating: repeat,
                is_composing: composing,
                state: KeyState::Pressed,
                text: Some("\r".into()),
            })
        };
        let evaluate = |doc: &LapuiDocument, source: &str| {
            doc.js_context
                .with(|ctx| ctx.eval::<(), _>(source))
                .unwrap()
        };
        doc.handle_ui_event(enter(false, false));
        doc.handle_ui_event(enter(true, false));
        doc.handle_ui_event(enter(false, true));
        evaluate(&doc,"if(JSON.stringify(calls)!=='[\"send\"]'||document.getElementById('name').value!=='Ada')throw Error('repeat/composition');document.getElementById('name').onkeydown=e=>e.preventDefault();");
        doc.handle_ui_event(enter(false, false));
        evaluate(&doc,"if(calls.length!==1)throw Error('canceled enter');document.getElementById('name').onkeydown=null;document.getElementById('send').disabled=true;");
        doc.handle_ui_event(enter(false, false));
        evaluate(&doc,"if(calls.length!==1)throw Error('disabled default bypass');document.getElementById('send').remove();document.getElementById('later').remove();");
        doc.handle_ui_event(enter(false, false));
        evaluate(&doc,"if(calls.length!==2||calls[1]!==null)throw Error('implicit null submitter');const second=document.createElement('input');document.getElementById('form').append(second);");
        doc.handle_ui_event(enter(false, false));
        evaluate(
            &doc,
            "if(calls.length!==2)throw Error('multiple blocking controls');",
        );
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn local_forms_unsupported_reset_preflight_formdata_bounds_and_native_editor_layouts() {
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None,
            "<html><body><form id='form'><input id='name' value='initial'><textarea id='area'>Initial text</textarea><select id='select' name='choice'><option>One</option></select></form><input id='fallback' type='invalid'></body></html>", "").unwrap();
        let area = doc.inner().get_element_by_id("area").unwrap();
        assert_eq!(
            crate::forms::value(&doc.inner(), area).unwrap(),
            "Initial text"
        );
        let result=doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"(()=>{
            const form=document.getElementById('form'),name=document.getElementById('name');name.value='current';
            let rejected=0;try{form.reset();}catch(e){if(e.name==='NotSupportedError')rejected++;}
            if(name.value!=='initial'||form.elements.namedItem('choice').value!=='One')throw Error('select reset');
            if(new FormData(form).get('choice')!=='One')throw Error('select FormData');
            if(rejected!==0)throw Error('supported select rejected');document.getElementById('select').remove();
            const data=new FormData();for(let i=0;i<1024;i++)data.append('name',String(i));
            try{data.append('overflow','value');}catch(e){if(e instanceof RangeError)rejected++;}
            if([...data].length!==1024)throw Error('partial FormData append');
            try{data.set('name','x'.repeat(2097152));}catch(e){if(e instanceof RangeError)rejected++;}
            if(data.get('name')!=='0'||data.getAll('name').length!==1024)throw Error('partial FormData set');
            try{new FormData().append('file','text','filename');}catch(e){if(e.name==='NotSupportedError')rejected++;}
            if(rejected!==3)throw Error('limits');
            const fallback=document.getElementById('fallback');fallback.value='fallback text';if(fallback.value!=='fallback text')throw Error('invalid type default');
            const area=document.getElementById('area');area.firstChild.nodeValue='Changed default';if(area.value!=='Changed default')throw Error('child text default');
            const inserted=document.createElement('div');inserted.innerHTML='<textarea id="inserted">Inserted text</textarea>';document.body.append(inserted);
            globalThis.insertedRef=inserted.querySelector('textarea').__ref;
            return true;
        })()"#)).unwrap_or_else(|error|panic!("{}",javascript_error(&doc.js_context,&error,&doc.script_budget)));
        assert!(result);
        doc.inner_mut()
            .set_viewport(Viewport::new(480, 400, 1.0, ColorScheme::Light));
        doc.inner_mut().resolve(0.0);
        let inserted = doc.inner().get_element_by_id("inserted").unwrap();
        assert_eq!(
            crate::forms::value(&doc.inner(), inserted).unwrap(),
            "Inserted text"
        );
        #[cfg(feature = "software-renderer")]
        for width in [480, 620, 480] {
            crate::snapshot::render_rgba(&mut doc, width, 400).unwrap();
            for id in ["name", "area", "inserted", "fallback"] {
                let dom = doc.inner();
                let input = dom.get_element_by_id(id).unwrap();
                assert!(dom
                    .get_node(input)
                    .unwrap()
                    .element_data()
                    .unwrap()
                    .text_input_data()
                    .unwrap()
                    .editor
                    .try_layout()
                    .is_some());
            }
        }
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn local_forms_separate_native_current_values_defaults_dirty_flags_reset_and_cloning() {
        let (mut doc,_)=LapuiDocument::new_with_source(ActionRegistry::default(),None,
            "<html><body><form id='form'><input id='text' name='text' value='initial'><textarea id='area' name='area'>Initial\ntext</textarea><input id='check' type='checkbox' checked><input id='first' type='radio' name='choice' checked><input id='second' type='radio' name='choice'></form></body></html>","").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        let result=doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"(()=>{
          const form=document.getElementById('form'),text=document.getElementById('text'),area=document.getElementById('area'),check=document.getElementById('check'),first=document.getElementById('first'),second=document.getElementById('second');
          if(text.value!=='initial'||area.value!=='Initial\ntext')throw Error('initial current values');
          text.value='current';text.defaultValue='new default';if(text.value!=='current'||text.getAttribute('value')!=='new default')throw Error('dirty input');
          text.removeAttribute('value');if(text.value!=='current'||text.defaultValue!=='')throw Error('remove default');
          text.defaultValue='restore';area.value='Edited\r\narea';area.defaultValue='New\r\ndefault';if(area.value!=='Edited\narea'||area.defaultValue!=='New\r\ndefault')throw Error('textarea dirty/default');
          check.checked=false;check.defaultChecked=true;if(check.checked||!check.defaultChecked)throw Error('dirty checked');
          second.checked=true;
          text.setCustomValidity('custom original');
          const clone=form.cloneNode(true);document.body.appendChild(clone);
          const clonedText=clone.querySelector('#text');if(clonedText.value!=='current'||clonedText.defaultValue!=='restore'||clonedText.validity.customError)throw Error('clone values');
          if(clone.querySelector('#area').value!=='Edited\narea'||clone.querySelector('#check').checked||!clone.querySelector('#second').checked)throw Error('clone widgets');
          clonedText.defaultValue='clone default';if(clonedText.value!=='current')throw Error('cloned dirty flag');
          let changes=0;form.addEventListener('input',()=>changes++);form.addEventListener('change',()=>changes++);
          form.addEventListener('reset',event=>{if(text.value!=='current')throw Error('reset before restoration');event.preventDefault();},{once:true});
          form.reset();if(text.value!=='current'||changes)throw Error('canceled reset');
          form.reset();if(text.value!=='restore'||area.value!=='New\ndefault'||!check.checked||!first.checked||second.checked||changes)throw Error('native reset');
          text.defaultValue='clean default';check.defaultChecked=false;if(text.value!=='clean default'||check.checked)throw Error('reset cleared dirty flags');
          if(!text.validity.customError)throw Error('reset incorrectly cleared custom validity');
          clone.reset();if(clonedText.value!=='clone default')throw Error('clone independent reset');
          return true;
        })()"#)).unwrap_or_else(|error|panic!("{}",javascript_error(&doc.js_context,&error,&doc.script_budget)));
        assert!(result);
        doc.inner_mut().resolve(0.0);
        let text = doc.inner().get_element_by_id("text").unwrap();
        assert_eq!(
            crate::forms::value(&doc.inner(), text).unwrap(),
            "clean default"
        );
    }

    #[test]
    fn local_form_constraints_live_validity_invalid_events_and_ai_fields_share_values() {
        let (mut doc,_)=LapuiDocument::new_with_source(ActionRegistry::default(),None,
            "<html><body><form id='form'><input id='text' name='text' required minlength='3' maxlength='5' pattern='[A-Z]+'><input id='email' type='email' multiple><input id='url' type='url'><input id='number' type='number' min='2' max='10' step='2'><input id='check' type='checkbox' required><input id='radio1' type='radio' name='choice' required><input id='radio2' type='radio' name='choice'><input id='disabled' disabled required><input id='readonly' readonly required><input id='password' type='password' value='private-value'></form></body></html>","").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"(()=>{
          globalThis.form=document.getElementById('form');globalThis.text=document.getElementById('text');globalThis.invalid=[];
          form.addEventListener('invalid',e=>{invalid.push(e.target.id);if(e.bubbles||!e.cancelable)throw Error('invalid event flags');},true);
          form.addEventListener('invalid',()=>{throw Error('invalid bubbled');});
          globalThis.liveValidity=text.validity;
          if(!(liveValidity instanceof ValidityState)||!liveValidity.valueMissing||text.willValidate!==true)throw Error('initial required');
          text.value='AB';if(liveValidity.tooShort||liveValidity.valueMissing)throw Error('programmatic length');
          lapui.fill(text.__ref,'ab');if(!liveValidity.tooShort||!liveValidity.patternMismatch)throw Error('user length/pattern');
          lapui.fill(text.__ref,'ABCDEF');if(!liveValidity.tooLong)throw Error('maximum user length');
          text.value='ABC';if(!liveValidity.valid)throw Error('valid text');
          const email=document.getElementById('email');email.value='bad';if(!email.validity.typeMismatch)throw Error('email');
          email.value=' one@example.test, two@example.test ';if(!email.validity.valid)throw Error('multiple email');
          const url=document.getElementById('url');url.value='relative/path';if(!url.validity.typeMismatch)throw Error('url');url.value='https://example.test/path';
          const number=document.getElementById('number');number.value='1';if(!number.validity.rangeUnderflow||!number.validity.stepMismatch)throw Error('numeric range');
          number.value='11';if(!number.validity.rangeOverflow)throw Error('numeric maximum');number.valueAsNumber=6;if(number.value!=='6'||!number.validity.valid)throw Error('numeric property');
          number.value='not a number';if(number.value!==''||number.validity.badInput)throw Error('programmatic numeric sanitation');
          lapui.fill(number.__ref,'not a number');if(number.value!==''||!number.validity.badInput)throw Error('interactive bad input');number.value='6';
          if(form.checkValidity()||invalid.join(',')!=='check,radio1,radio2')throw Error('form invalid controls '+invalid);
          document.getElementById('check').checked=true;document.getElementById('radio2').checked=true;
          if(!form.checkValidity())throw Error('valid controls');
          text.setCustomValidity('Choose a different name');if(!liveValidity.customError||liveValidity.valid||text.validationMessage!=='Choose a different name')throw Error('custom validity');
          text.addEventListener('invalid',e=>e.preventDefault(),{once:true});form.reportValidity();if(document.activeElement===text)throw Error('canceled invalid report focus');
          form.reportValidity();if(document.activeElement!==text)throw Error('report focus');
          document.getElementById('password').setCustomValidity('private-message');
          const secret=lapui.controls().controls.find(item=>item.id==='password');
          if('value' in secret||'validationMessage' in secret||!secret.validity.customError)throw Error('password projection');
          return document.getElementById('disabled').willValidate===false&&document.getElementById('readonly').willValidate===false;
        })()"#)).unwrap_or_else(|error|panic!("{}",javascript_error(&doc.js_context,&error,&doc.script_budget))));
        let snapshot = control_request(&mut doc, json!({"method":"controls"})).unwrap();
        let text = snapshot["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == "text")
            .unwrap();
        assert_eq!(text["validity"]["customError"], true);
        assert_eq!(text["validationMessage"], "Choose a different name");
        assert_eq!(text["willValidate"], true);
        assert!(text["formRef"].as_str().unwrap().starts_with("node:"));
        let secret = snapshot["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == "password")
            .unwrap();
        assert!(secret.get("value").is_none());
        assert!(secret.get("validationMessage").is_none());
        assert_eq!(secret["validity"]["customError"], true);
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn local_form_submit_reset_external_ownership_formdata_and_shared_ai_activation() {
        let html="<html><body><form id='form'><input id='name' name='name' required><input id='hidden' type='hidden' name='extra' value='secret'><input id='checked' type='checkbox' name='choice' value='yes' checked><input name='choice' type='checkbox' value='no'><fieldset disabled><input name='excluded' value='omit'><legend><input name='legend' value='keep'></legend></fieldset><textarea name='notes'>Initial text</textarea><button id='send' name='command' value='save'><span id='nested'>Save</span></button><button id='reset' type='reset'>Reset</button><button id='skip' formnovalidate name='command' value='skip'>Skip validation</button></form><input id='external' form='form' name='external' value='outside'><form id='other'><button id='wrong'>Wrong</button></form></body></html>";
        let (mut doc,_)=LapuiDocument::new_with_source(ActionRegistry::default(),None,html,r#"
          globalThis.form=document.getElementById('form');globalThis.submissions=[];globalThis.entries=[];globalThis.resets=0;
          form.addEventListener('submit',e=>{e.preventDefault();submissions.push([e.submitter?.id??null,e.bubbles,e.cancelable]);entries.push([...new FormData(form,e.submitter)]);form.requestSubmit();});
          form.addEventListener('formdata',e=>e.formData.append('added','event'));
          form.addEventListener('reset',()=>resets++);
        "#).unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"(()=>{
          if(!(form instanceof HTMLFormElement)||!(form.elements instanceof HTMLFormControlsCollection))throw Error('form markers');
          const collection=form.elements;if(collection!==form.elements||collection.namedItem('external')!==document.getElementById('external')||collection[0]!==document.getElementById('name'))throw Error('live collection');
          const before=collection.length;const extra=document.createElement('input');extra.name='dynamic';extra.value='new';form.appendChild(extra);if(collection.length!==before+1)throw Error('collection insertion');extra.remove();
          document.getElementById('name').value='Ada';document.getElementById('nested').click();
          if(submissions.length!==1||submissions[0][0]!=='send')throw Error('nested click submitter');
          form.requestSubmit();if(submissions.at(-1)[0]!==null)throw Error('explicit null submitter');
          let invalid=0;try{form.requestSubmit(document.getElementById('name'));}catch(e){if(e instanceof TypeError)invalid++;}
          try{form.requestSubmit(document.getElementById('wrong'));}catch(e){if(e.name==='NotFoundError')invalid++;}
          try{form.submit();}catch(e){if(e.name==='NotSupportedError')invalid++;}if(invalid!==3)throw Error('submit errors');
          form.reset();if(resets!==1||document.getElementById('name').value!==''||document.getElementById('external').value!=='outside')throw Error('reset owner');
          form.requestSubmit();if(submissions.length!==2)throw Error('invalid submission');
          document.getElementById('skip').click();if(submissions.length!==3||submissions.at(-1)[0]!=='skip')throw Error('submitter noValidate');
          form.noValidate=true;form.requestSubmit();if(submissions.length!==4)throw Error('form noValidate');form.noValidate=false;
          const data=new FormData();data.append('a','one');data.append('b','two');data.append('a','three');data.set('a','replace');
          if(JSON.stringify([...data])!==JSON.stringify([['a','replace'],['b','two']])||data.getAll('a').length!==1||data.get('absent')!==null)throw Error('FormData mutations');
          data.append('\ud800','\udfff');if(data.get('\uFFFD')!=='\uFFFD')throw Error('scalar strings');
          return true;
        })()"#)).unwrap_or_else(|error|panic!("{}",javascript_error(&doc.js_context,&error,&doc.script_budget))));
        let entries: Value = serde_json::from_str(
            &doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(entries[0])"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            entries,
            json!([
                ["name", "Ada"],
                ["extra", "secret"],
                ["choice", "yes"],
                ["legend", "keep"],
                ["notes", "Initial text"],
                ["command", "save"],
                ["external", "outside"],
                ["added", "event"]
            ])
        );
        let snapshot = control_request(&mut doc, json!({"method":"controls"})).unwrap();
        let named = |id: &str| {
            snapshot["controls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == id)
                .unwrap()["ref"]
                .clone()
        };
        control_request(&mut doc,json!({"method":"fill","documentEpoch":snapshot["documentEpoch"],"ref":named("name"),"value":"AI name"})).unwrap();
        control_request(&mut doc,json!({"method":"activate","documentEpoch":snapshot["documentEpoch"],"ref":named("send")})).unwrap();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("submissions.length"))
                .unwrap(),
            5
        );
        control_request(&mut doc,json!({"method":"activate","documentEpoch":snapshot["documentEpoch"],"ref":named("reset")})).unwrap();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("document.getElementById('name').value"))
                .unwrap(),
            ""
        );
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn resize_observers_use_native_box_sizes_snapshots_and_selected_box_changes() {
        let html="<html><head><style>html,body{margin:0}#box{width:100.5px;height:30.25px;padding:5px;border:2px solid}#hidden{display:none}#zero{width:0;height:0}</style></head><body><div id='box'></div><span id='inline'>Inline text</span><div id='hidden'></div><div id='zero'></div></body></html>";
        let (mut doc,_)=LapuiDocument::new_with_source(ActionRegistry::default(),None,html,r#"
            globalThis.box=document.getElementById('box');globalThis.logs=[];globalThis.saved=null;
            for(const mode of ['content-box','border-box','device-pixel-content-box']){
              const ro=new ResizeObserver(function(entries,observer){
                if(this!==observer)throw new Error('Wrong callback receiver');
                logs.push([mode,entries.map(e=>[e.target.id,e.contentRect.x,e.contentRect.y,e.contentRect.width,e.contentRect.height,e.contentBoxSize[0].inlineSize,e.borderBoxSize[0].inlineSize,e.devicePixelContentBoxSize[0].inlineSize])]);
                saved=entries[0];
              });
              for(const id of ['box','inline','hidden','zero'])ro.observe(document.getElementById(id),{box:mode});
            }
        "#).unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 2.0, ColorScheme::Light));
        doc.poll(None);
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("logs.length"))
                .unwrap(),
            0
        );
        assert!(doc.rendering_update());
        let log = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("JSON.stringify(logs)"))
            .unwrap();
        let log: Value = serde_json::from_str(&log).unwrap();
        for index in 0..3 {
            assert_eq!(
                log[index][1],
                json!([["box", 5, 5, 100.5, 30.25, 100.5, 114.5, 201]])
            );
        }
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"saved instanceof ResizeObserverEntry && saved.contentRect instanceof DOMRectReadOnly && saved.contentBoxSize[0] instanceof ResizeObserverSize && Object.isFrozen(saved.contentBoxSize) && (()=>{try{saved.contentBoxSize.push(1);return false;}catch{}return true;})()"#)).unwrap());
        assert!(!doc.rendering_update());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("logs=[];box.style.padding='10px'"))
            .unwrap();
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(logs.map(row=>row[0]))"))
                .unwrap(),
            "[\"border-box\"]"
        );
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<f64, _>("saved.contentRect.x"))
                .unwrap(),
            10.0
        );
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("globalThis.oldEntry=saved;logs=[];box.style.width='120px'")
            })
            .unwrap();
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("logs.length"))
                .unwrap(),
            3
        );
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<f64, _>("oldEntry.contentRect.width"))
                .unwrap(),
            100.5
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("logs=[];box.style.transform='translateX(20px)'"))
            .unwrap();
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("logs.length"))
                .unwrap(),
            0
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("box.style.display='none'"))
            .unwrap();
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("logs.length"))
                .unwrap(),
            3
        );
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<f64, _>("saved.contentRect.width"))
                .unwrap(),
            0.0
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("logs=[];box.style.display='block'"))
            .unwrap();
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("logs.length"))
                .unwrap(),
            3
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("logs=[]"))
            .unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(logs.map(row=>row[0]))"))
                .unwrap(),
            "[\"device-pixel-content-box\"]"
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("logs=[];box.remove()"))
            .unwrap();
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("logs.length"))
                .unwrap(),
            3
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("logs=[];document.body.appendChild(box)"))
            .unwrap();
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("logs.length"))
                .unwrap(),
            3
        );
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn mutation_observer_batches_attribute_character_and_child_changes() {
        let html = "<html><body><main id='root'><p id='child'>old</p></main></body></html>";
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, r#"
              globalThis.root=document.getElementById('root');
              globalThis.child=document.getElementById('child');
              globalThis.deliveries=[];
              globalThis.observer=new MutationObserver(function(records,owner){
                if(this!==owner)throw new Error('wrong callback receiver');
                deliveries.push(records);
              });
              observer.observe(root,{subtree:true,childList:true,attributes:true,characterData:true,
                attributeOldValue:true,characterDataOldValue:true,attributeFilter:['data-x','style']});
            "#).unwrap();
        doc.js_context.with(|ctx|ctx.eval::<(),_>(r#"
          child.setAttribute('data-x','one');child.setAttribute('data-x','two');
          child.style.width='20px';child.firstChild.nodeValue='new';
          globalThis.added=document.createElement('span');added.id='added';root.appendChild(added);
          globalThis.callbacksBeforeCheckpoint=deliveries.length;
          globalThis.taken=observer.takeRecords();
          globalThis.takenSummary=taken.map(item=>[
            item instanceof MutationRecord,item.type,item.target.id||item.target.nodeName,item.attributeName,
            item.oldValue,item.addedNodes.map(node=>node.id),item.removedNodes.length,
            item.previousSibling?.id||null,item.nextSibling?.id||null,Object.isFrozen(item)
          ]);
        "#)).unwrap();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("callbacksBeforeCheckpoint"))
                .unwrap(),
            0
        );
        let taken: Value = serde_json::from_str(
            &doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(takenSummary)"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(taken.as_array().unwrap().len(), 5);
        assert_eq!(taken[0][1], "attributes");
        assert_eq!(taken[0][3], "data-x");
        assert_eq!(taken[0][4], Value::Null);
        assert_eq!(taken[1][1], "attributes");
        assert_eq!(taken[1][4], "one");
        assert_eq!(taken[2][1], "attributes");
        assert_eq!(taken[2][3], "style");
        assert_eq!(taken[3][1], "characterData");
        assert_eq!(taken[3][2], "#text");
        assert_eq!(taken[3][4], "old");
        assert_eq!(taken[4][1], "childList");
        assert_eq!(taken[4][2], "root");
        assert_eq!(taken[4][5][0], "added");
        assert_eq!(taken[4][7], "child");
        assert!(taken
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record[0] == true && record[9] == true));
        doc.poll(None);
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("deliveries.length"))
                .unwrap(),
            0
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("child.setAttribute('data-x','three')"))
            .unwrap();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("deliveries.length"))
                .unwrap(),
            0
        );
        doc.poll(None);
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>("deliveries.length===1&&deliveries[0].length===1&&deliveries[0][0].oldValue==='two'")).unwrap());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("child.innerHTML='<b id=\"html-child\">markup</b>'"))
            .unwrap();
        doc.poll(None);
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>("deliveries.length===2&&deliveries[1].length===1&&deliveries[1][0].type==='childList'&&deliveries[1][0].target===child&&deliveries[1][0].addedNodes[0].id==='html-child'&&deliveries[1][0].removedNodes.length===1")).unwrap());
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "lapui.batch(()=>{child.style.height='12px';child.textContent='batched'})",
                )
            })
            .unwrap();
        doc.poll(None);
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>("deliveries.length===3&&deliveries[2].length===2&&deliveries[2][0].type==='attributes'&&deliveries[2][0].attributeName==='style'&&deliveries[2][1].type==='childList'")).unwrap());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("added.remove()"))
            .unwrap();
        doc.poll(None);
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>("deliveries.length===4&&deliveries[3].length===1&&deliveries[3][0].type==='childList'&&deliveries[3][0].removedNodes[0]===added")).unwrap());
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn mutation_observer_validates_options_and_reclaims_capacity() {
        let (doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(),None,
            "<html><body><div id='target'></div></body></html>",r#"
            globalThis.target=document.getElementById('target');globalThis.invalid=0;globalThis.range=0;
            for(const options of [{},{attributes:false,attributeOldValue:true},{characterData:false,characterDataOldValue:true}]){
              try{new MutationObserver(()=>{}).observe(target,options)}catch(error){if(error instanceof TypeError)invalid++}
            }
            globalThis.all=Array.from({length:128},()=>new MutationObserver(()=>{}));
            all.forEach(observer=>observer.observe(target,{childList:true}));
            globalThis.replacement=new MutationObserver(()=>{});
            try{replacement.observe(target,{childList:true})}catch(error){range=error instanceof RangeError}
            all[0].disconnect();replacement.observe(target,{childList:true});
            "#).unwrap();
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("invalid===3&&range"))
            .unwrap());
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "all.forEach(observer=>observer.disconnect());replacement.disconnect()",
                )
            })
            .unwrap();
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn intersection_observer_tracks_viewport_thresholds_and_readonly_entries() {
        let html = "<html><head><style>html,body{margin:0}#target{position:absolute;left:0;top:180px;width:40px;height:40px}</style></head><body><div id='target'></div></body></html>";
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            html,
            r#"
              globalThis.target=document.getElementById('target');globalThis.entries=[];
              globalThis.observer=new IntersectionObserver(function(batch,owner){
                if(this!==owner)throw new Error('wrong callback receiver');
                entries.push(...batch);
              },{rootMargin:'10% 0%',threshold:[1,.5,0,.5]});
              observer.observe(target);
            "#,
        )
        .unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(100, 200, 1.0, ColorScheme::Light));
        assert!(doc.rendering_update());
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("entries.length"))
                .unwrap(),
            1
        );
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"
            entries[0] instanceof IntersectionObserverEntry && entries[0].target===target &&
            observer.root===null && observer.thresholds.join(',')==='0,0.5,1' &&
            observer.rootMargin==='10% 0% 10% 0%' && entries[0].isIntersecting &&
            Math.abs(entries[0].intersectionRatio-.75)<.001 && entries[0].rootBounds.y===-10 &&
            entries[0].rootBounds.height===220 && entries[0].intersectionRect.height===30 &&
            Object.isFrozen(entries[0]) && (()=>{try{entries[0].time=-1;return false}catch{return true}})()
        "#)).unwrap());
        assert!(!doc.rendering_update());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("target.style.top='190px'"))
            .unwrap();
        assert!(doc.rendering_update());
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("entries.length"))
                .unwrap(),
            2
        );
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>(
                "Math.abs(entries[1].intersectionRatio-.5)<.001 && entries[1].isIntersecting"
            ))
            .unwrap());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("target.style.top='200px'"))
            .unwrap();
        assert!(!doc.rendering_update());
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("entries.length"))
                .unwrap(),
            2
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("target.style.top='240px'"))
            .unwrap();
        assert!(doc.rendering_update());
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("entries.length"))
                .unwrap(),
            3
        );
        assert!(doc
            .js_context
            .with(|ctx| ctx
                .eval::<bool, _>("!entries[2].isIntersecting && entries[2].intersectionRatio===0"))
            .unwrap());
        assert!(!doc.rendering_update());
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn intersection_observer_validates_options_and_reclaims_capacity() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body><div id='target'></div></body></html>",
            r#"
              globalThis.target=document.getElementById('target');globalThis.typeErrors=0;globalThis.rangeErrors=0;globalThis.notSupported=0;
              for(const options of [{root:target},{rootMargin:'2em'},{threshold:-1},{threshold:[0,2]}]){
                try{new IntersectionObserver(()=>{},options)}catch(error){
                  if(error instanceof TypeError||error instanceof SyntaxError)typeErrors++;
                  else if(error instanceof RangeError)rangeErrors++;
                  else if(error.name==='NotSupportedError')notSupported++;
                }
              }
              const emptyThreshold=new IntersectionObserver(()=>{},{threshold:[]});
              globalThis.all=Array.from({length:128},()=>new IntersectionObserver(()=>{}));
              all.forEach(observer=>observer.observe(target));
              globalThis.replacement=new IntersectionObserver(()=>{});globalThis.capacityError=false;
              try{replacement.observe(target)}catch(error){capacityError=error instanceof RangeError}
              all[0].disconnect();replacement.observe(target);
            "#,
        )
        .unwrap();
        assert!(doc.rendering_update());
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>("typeErrors===1&&rangeErrors===2&&notSupported===1&&emptyThreshold.thresholds[0]===0&&capacityError&&replacement.takeRecords().length===0")).unwrap());
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "all.forEach(observer=>observer.disconnect());replacement.disconnect()",
                )
            })
            .unwrap();
        assert!(!doc.rendering_update());
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn resize_observers_deliver_deeper_changes_and_defer_self_resize_loops() {
        let (mut doc,_)=LapuiDocument::new_with_source(ActionRegistry::default(),None,
            "<html><body><div id='outer' style='width:100px;height:100px'><div id='child' style='width:50px;height:20px'></div></div></body></html>",r#"
            globalThis.outer=document.getElementById('outer');globalThis.child=document.getElementById('child');
            globalThis.logs=[];globalThis.looping=false;
            globalThis.observer=new ResizeObserver(entries=>{
                logs.push(entries.map(e=>[e.target.id,e.contentRect.width]));
                if(entries.some(e=>e.target===outer)){
                    if(looping)outer.style.width=(outer.getBoundingClientRect().width+1)+'px';
                    else child.style.width='80px';
                }
            });observer.observe(outer);observer.observe(child);
        "#).unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(logs)"))
                .unwrap(),
            "[[[\"outer\",100],[\"child\",50]],[[\"child\",80]]]"
        );
        assert!(doc.script_diagnostics.borrow().is_empty());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("logs=[];looping=true;outer.style.width='120px'"))
            .unwrap();
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("logs.length"))
                .unwrap(),
            1
        );
        assert!(doc.has_pending_rendering_update());
        assert_eq!(
            doc.script_diagnostics.borrow().back().unwrap().phase,
            "resize-observer"
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("looping=false"))
            .unwrap();
        doc.rendering_update();
        assert!(!doc.has_pending_rendering_update());
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(logs.at(-1))"))
                .unwrap(),
            "[[\"outer\",121]]"
        );
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("observer.disconnect();logs=[];outer.style.width='130px'")
            })
            .unwrap();
        assert!(!doc.rendering_update());
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("logs.length"))
                .unwrap(),
            0
        );
    }

    #[test]
    fn resize_observer_capacity_is_reclaimed_and_disconnection_releases_detached_nodes() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body></body></html>",
            "",
        )
        .unwrap();
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"(()=>{
          let invalid=0;
          for(const run of [()=>new ResizeObserver(null),()=>new ResizeObserver(()=>{}).observe(document),()=>new ResizeObserver(()=>{}).observe(document.body,{box:'invalid'})])try{run();}catch(e){if(e instanceof TypeError)invalid++;}
          if(invalid!==3)return false;
          const all=Array.from({length:128},()=>new ResizeObserver(()=>{}));
          for(const ro of all)ro.observe(document.createElement('div'));
          const replacement=new ResizeObserver(()=>{});
          try{replacement.observe(document.createElement('div'));return false;}catch(e){if(!(e instanceof RangeError))return false;}
          all[0].disconnect();replacement.observe(document.createElement('div'));
          for(const ro of all)ro.disconnect();replacement.disconnect();
          const nodes=Array.from({length:1024},()=>document.createElement('div'));
          for(const node of nodes)replacement.observe(node);
          replacement.observe(nodes[0],{box:'border-box'});
          try{replacement.observe(document.createElement('div'));return false;}catch(e){if(!(e instanceof RangeError))return false;}
          replacement.unobserve(nodes[0]);replacement.observe(document.createElement('div'));replacement.disconnect();
          const node=document.createElement('div');node.style.cssText='width:20px;height:20px';node.innerHTML='<span>retained child</span>';
          document.body.appendChild(node);globalThis.rootRef=node.__ref;globalThis.childRef=node.firstChild.__ref;
          globalThis.retainedObserver=new ResizeObserver(()=>{});retainedObserver.observe(node);node.remove();
          return true;
        })()"#)).unwrap());
        doc.js_runtime.run_gc();
        doc.poll(None);
        let reference = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("rootRef"))
            .unwrap();
        let child = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("childRef"))
            .unwrap();
        assert!(resolve_node_ref(&doc.dom.borrow(), &reference).is_some());
        assert!(resolve_node_ref(&doc.dom.borrow(), &child).is_some());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("retainedObserver.disconnect();retainedObserver=null"))
            .unwrap();
        doc.js_runtime.run_gc();
        doc.poll(None);
        assert!(resolve_node_ref(&doc.dom.borrow(), &reference).is_none());
        assert!(resolve_node_ref(&doc.dom.borrow(), &child).is_none());
    }

    #[test]
    fn resize_callback_errors_are_diagnosed_and_runaway_callbacks_suspend_the_document() {
        let (mut doc,_)=LapuiDocument::new_with_source(ActionRegistry::default(),None,"<html><body><div id='box' style='width:20px;height:20px'></div></body></html>",r#"
          globalThis.calls=0;
          new ResizeObserver(()=>{throw new Error('observer failure')}).observe(document.getElementById('box'));
          new ResizeObserver(()=>{calls++}).observe(document.getElementById('box'));
        "#).unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("calls"))
                .unwrap(),
            1
        );
        assert!(doc
            .script_diagnostics
            .borrow()
            .iter()
            .any(|entry| entry.phase == "resize-observer"
                && entry.message.contains("observer failure")));
        doc.js_context.with(|ctx|ctx.eval::<(),_>("new ResizeObserver(()=>{while(true){}}).observe(document.getElementById('box'))")).unwrap();
        doc.rendering_update();
        assert!(doc.script_budget.interrupted());
        assert!(!doc.has_pending_rendering_update());
        assert_eq!(
            control_request(&mut doc, json!({"method":"diagnostics"})).unwrap()["scriptStatus"],
            "suspended"
        );
    }

    #[test]
    fn window_listeners_receive_resize_native_scroll_and_shared_capture_bubble_paths() {
        let (mut doc,_)=LapuiDocument::new_with_source(ActionRegistry::default(),None,
          "<html><head><style>html,body{margin:0}body{height:1000px}#scroll{width:100px;height:50px;overflow:auto}#content{width:100px;height:300px}</style></head><body><div id='scroll'><div id='content'></div></div><button id='button'>Button</button></body></html>",r#"
          globalThis.events=[];globalThis.scroller=document.getElementById('scroll');
          const resize=function(e){events.push(['resize',e.target===window,e.currentTarget===window,e.bubbles,e.cancelable,e.eventPhase,this===window]);};
          window.addEventListener('resize',resize,{once:true});window.addEventListener('resize',resize);
          window.addEventListener('scroll',e=>events.push(['capture-scroll',e.target===scroller,e.eventPhase]),true);
          window.addEventListener('scroll',e=>events.push(['bubble-scroll',e.target===document,e.eventPhase]));
          scroller.addEventListener('scroll',e=>events.push(['element-scroll',e.bubbles,e.cancelable]));
          window.addEventListener('click',e=>events.push(['capture-click',e.eventPhase,e.composedPath().at(-1)===window]),true);
          window.addEventListener('click',e=>events.push(['bubble-click',e.eventPhase]));
        "#).unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 2.0, ColorScheme::Light));
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(events)"))
                .unwrap(),
            "[[\"resize\",true,true,false,false,2,true]]"
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("events=[];document.getElementById('button').click()"))
            .unwrap();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(events)"))
                .unwrap(),
            "[[\"capture-click\",1,true],[\"bubble-click\",3]]"
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("events=[]"))
            .unwrap();
        let id = doc.inner().get_element_by_id("scroll").unwrap();
        doc.inner_mut()
            .scroll_to(id, 0.0, 100.0, blitz::dom::ScrollBehavior::Instant);
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(events)"))
                .unwrap(),
            "[[\"capture-scroll\",true,1],[\"element-scroll\",false,false]]"
        );
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("events=[];scroller.scrollTop=120;scroller.scrollTop=130")
            })
            .unwrap();
        doc.poll(None);
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("events.length"))
                .unwrap(),
            2
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("events=[]"))
            .unwrap();
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("scroller.style.display='none'"))
            .unwrap();
        doc.rendering_update();
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("scroller.style.display='block'"))
            .unwrap();
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("events.length"))
                .unwrap(),
            0
        );
        let mut scroll = doc.inner().viewport_scroll();
        scroll.y = 20.0;
        doc.inner_mut().set_viewport_scroll(scroll);
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(events)"))
                .unwrap(),
            "[[\"capture-scroll\",false,1],[\"bubble-scroll\",true,3]]"
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("events=[]"))
            .unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(900, 600, 2.0, ColorScheme::Light));
        doc.rendering_update();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("events.length"))
                .unwrap(),
            0
        );
        assert!(!doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("window instanceof Node"))
            .unwrap());
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn floating_ui_dom_bundle_positions_flips_shifts_and_accepts_through_shared_controls() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/floating-demo");
        let html = std::fs::read_to_string(root.join("index.html")).unwrap();
        let (mut doc, _) =
            LapuiDocument::new_with_local_source(ActionRegistry::default(), None, &html, "", &root)
                .unwrap();
        doc.js_context.with(|ctx|ctx.eval::<(),_>(r#"
          globalThis.__intersectionConstructors=[];
          const OriginalIntersectionObserver=IntersectionObserver;
          globalThis.IntersectionObserver=class extends OriginalIntersectionObserver{
            constructor(callback,options){__intersectionConstructors.push(options||{});super(callback,options)}
          };
        "#)).unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(900, 850, 1.0, ColorScheme::Light));
        fn field(doc: &LapuiDocument, id: &str) -> String {
            doc.js_context
                .with(|ctx| {
                    ctx.eval::<String, _>(format!(
                        "document.getElementById({}).value",
                        serde_json::to_string(id).unwrap()
                    ))
                })
                .unwrap()
        }
        fn activate(doc: &mut LapuiDocument, id: &str) {
            let snapshot = control_request(doc, json!({"method":"controls"})).unwrap();
            let reference = snapshot["controls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == id)
                .unwrap()["ref"]
                .clone();
            control_request(doc,json!({"method":"activate","documentEpoch":snapshot["documentEpoch"],"ref":reference})).unwrap();
        }
        fn settle(doc: &mut LapuiDocument) {
            for _ in 0..100 {
                doc.poll(None);
                doc.animation_frame();
                doc.rendering_update();
                if field(doc, "status") != "Positioning" {
                    return;
                }
            }
            panic!("floating UI did not settle: {}", field(doc, "status"));
        }
        activate(&mut doc, "anchor");
        settle(&mut doc);
        assert_eq!(
            field(&doc, "status"),
            "Open",
            "diagnostics: {}",
            serde_json::to_string(&*doc.script_diagnostics.borrow()).unwrap()
        );
        assert_eq!(field(&doc, "placement"), "bottom-start");
        assert_eq!(field(&doc, "bounds"), "Yes");
        assert_eq!(field(&doc, "coordinates"), "24.0 / 150.0");
        assert_eq!(field(&doc, "style-width"), "220px");
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>("__intersectionConstructors.length>=2 && __intersectionConstructors.at(-1).root===undefined")).unwrap(), "Floating UI should retry its viewport-root observer after element-root rejection");
        #[cfg(feature = "software-renderer")]
        {
            let pixels = crate::snapshot::render_rgba(&mut doc, 900, 850).unwrap();
            let popover = doc.inner().get_element_by_id("popover").unwrap();
            let bounds = doc.inner().get_client_bounding_rect(popover).unwrap();
            let x = (bounds.x + 12.0) as usize;
            let y = (bounds.y + 5.0) as usize;
            let pixel = (y * 900 + x) * 4;
            assert_eq!(&pixels[pixel..pixel + 4], &[215, 229, 255, 255]);
        }
        activate(&mut doc, "edge");
        settle(&mut doc);
        assert_eq!(field(&doc, "status"), "Open");
        assert_eq!(field(&doc, "placement"), "top-end");
        assert_eq!(field(&doc, "bounds"), "Yes");
        let stage_width = doc
            .js_context
            .with(|ctx| ctx.eval::<f64, _>("document.getElementById('stage').clientWidth"))
            .unwrap();
        let coordinates = field(&doc, "coordinates");
        let numbers: Vec<f64> = coordinates
            .split('/')
            .map(|value| value.trim().parse().unwrap())
            .collect();
        assert_eq!(numbers, vec![stage_width - 230.0, 122.0]);
        doc.inner_mut()
            .set_viewport(Viewport::new(600, 850, 1.0, ColorScheme::Light));
        // Resize is handled by the real autoUpdate window listener.
        settle(&mut doc);
        assert_eq!(field(&doc, "bounds"), "Yes");
        assert_ne!(field(&doc, "coordinates"), coordinates);
        let before_resize = field(&doc, "coordinates");
        activate(&mut doc, "resize-anchor");
        settle(&mut doc);
        assert_ne!(field(&doc, "coordinates"), before_resize);
        assert_eq!(field(&doc, "bounds"), "Yes");
        activate(&mut doc, "accept");
        assert_eq!(field(&doc, "status"), "Accepted");
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>(
                "getComputedStyle(document.getElementById('popover')).display==='none'"
            ))
            .unwrap());
        activate(&mut doc, "anchor");
        settle(&mut doc);
        activate(&mut doc, "center");
        settle(&mut doc);
        assert_eq!(field(&doc, "coordinates"), "24.0 / 150.0");
        activate(&mut doc, "left-edge");
        settle(&mut doc);
        assert_eq!(field(&doc, "placement"), "bottom-start");
        assert_eq!(field(&doc, "coordinates"), "8.0 / 150.0");
        assert_eq!(field(&doc, "bounds"), "Yes");
        activate(&mut doc, "close");
        assert_eq!(field(&doc, "status"), "Closed");
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn computed_style_reads_the_live_cascade_variables_shorthands_and_used_css_sizes() {
        let html = "<html><head><style>html,body{margin:0}body{color:rgb(12,34,56);font-size:16px;--Tone:rgb(255,0,0);--gap:5%}#box{width:50%;height:40px;padding:var(--gap);border:3px solid currentColor;background-color:var(--Tone);overflow:hidden auto;margin:10px 11px}#box.compact{width:120px;box-sizing:border-box;background-color:rgb(0,0,255)}@media(min-width:600px){#box{width:75%}}</style></head><body><div id='box'>content</div><span id='inline'>inline</span></body></html>";
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 2.0, ColorScheme::Light));
        let result: String = doc.js_context.with(|ctx|ctx.eval(r#"
            globalThis.box = document.getElementById('box');
            globalThis.computed = getComputedStyle(box);
            JSON.stringify([computed.width,computed.height,computed.paddingLeft,computed.color,computed.backgroundColor,computed.borderTopColor,computed.overflow,computed.margin,computed.fontSize,computed.boxSizing])
        "#)).unwrap_or_else(|error|panic!("{}",javascript_error(&doc.js_context,&error,&doc.script_budget)));
        assert_eq!(result,"[\"200px\",\"40px\",\"20px\",\"rgb(12, 34, 56)\",\"rgb(255, 0, 0)\",\"rgb(12, 34, 56)\",\"hidden auto\",\"10px 11px\",\"16px\",\"content-box\"]");
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"(() => {
            if(computed.getPropertyValue('COLOR') !== computed.color || computed['background-color'] !== computed.backgroundColor || computed.cssFloat !== computed.getPropertyValue('float')) return false;
            if(computed.getPropertyValue('not-a-property')!=='' || computed.getPropertyValue('--tone')!=='') return false;
            if(computed.getPropertyValue('--Tone').replace(/\s/g,'')!=='rgb(255,0,0)' || !computed.getPropertyValue('--gap').includes('5%')) return false;
            const names=Array.from(computed);
            if(names.length!==computed.length || computed[0]!==computed.item(0) || computed.item(1e6)!=='' || !names.includes('width') || !names.includes('direction') || !names.includes('--Tone')) return false;
            if(Object.keys(computed).filter(key=>/^[0-9]+$/.test(key)).length!==names.length) return false;
            if(!(computed instanceof CSSStyleDeclaration) || !(box.style instanceof CSSStyleDeclaration) || computed.parentRule!==null || computed.cssText!=='' || computed.getPropertyPriority('color')!=='') return false;
            for(const change of [()=>computed.width='1px',()=>computed.setProperty('width','1px'),()=>computed.removeProperty('width'),()=>computed.cssText='width:1px',()=>delete computed.width,()=>Object.defineProperty(computed,'width',{value:'1px'})]) {
                try { change(); return false; } catch(error) { if(error.name!=='NoModificationAllowedError') return false; }
            }
            for(const invalid of [null,{},document,document.createTextNode('text')]) {
                try { getComputedStyle(invalid); return false; } catch(error) { if(!(error instanceof TypeError)) return false; }
            }
            try { getComputedStyle(box,'::before'); return false; } catch(error) { if(error.name!=='NotSupportedError') return false; }
            if(getComputedStyle(box,null).width!==computed.width || getComputedStyle(box,'').width!==computed.width) return false;
            box.classList.add('compact');
            if(computed.width!=='120px' || computed.boxSizing!=='border-box' || computed.backgroundColor!=='rgb(0, 0, 255)') return false;
            box.classList.remove('compact');
            let inside;
            lapui.batch(()=>{box.style.width='40%';inside=computed.width;box.style.height='70px';});
            if(inside!=='160px' || computed.height!=='70px') return false;
            document.body.style.color='rgb(9,8,7)';
            if(computed.color!=='rgb(9, 8, 7)' || computed.borderTopColor!==computed.color) return false;
            box.style.removeProperty('width');
            box.style.display='none';
            if(computed.display!=='none' || computed.width!=='50%') return false;
            box.style.display='block';
            if(computed.width!=='200px' || getComputedStyle(document.getElementById('inline')).width!=='auto') return false;
            return document instanceof Node && box instanceof Node && document.createTextNode('x') instanceof Node && Node.ELEMENT_NODE===1;
        })()"#)).unwrap_or_else(|error|panic!("{}",javascript_error(&doc.js_context,&error,&doc.script_budget))));
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("computed.width"))
                .unwrap(),
            "600px"
        );
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>(
                r#"(() => {
            box.remove();
            if(computed.length!==0 || computed.width!=='') return false;
            document.body.appendChild(box);
            return computed.width==='600px';
        })()"#
            ))
            .unwrap());
    }

    #[test]
    fn a_retained_computed_view_keeps_detached_nodes_only_until_the_view_is_collected() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body></body></html>",
            "",
        )
        .unwrap();
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    r#"
            (() => {
                const root=document.createElement('div');
                root.innerHTML='<span>Retained subtree</span>';
                document.body.appendChild(root);
                globalThis.rootReference=root.__ref;
                globalThis.childReference=root.firstChild.__ref;
                globalThis.retainedComputed=getComputedStyle(root);
                root.remove();
            })();
        "#,
                )
            })
            .unwrap();
        doc.js_runtime.run_gc();
        doc.poll(None);
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>("Boolean(__lapui_resolve(rootReference)) && Boolean(__lapui_resolve(childReference)) && retainedComputed.length===0")).unwrap());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("globalThis.retainedComputed=null"))
            .unwrap();
        doc.js_runtime.run_gc();
        doc.poll(None);
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>(
                "!__lapui_resolve(rootReference) && !__lapui_resolve(childReference)"
            ))
            .unwrap());
    }

    #[test]
    fn offset_metrics_use_native_parent_padding_edges_ignore_scroll_and_hide_boxless_nodes() {
        let html = "<html><head><style>html,body{margin:0}#parent{position:absolute;left:30px;top:40px;width:120px;height:80px;padding:10px;border:2px solid;overflow:hidden}#child{position:absolute;left:25px;top:35px;width:50px;height:20px;padding:3px;border:1px solid}#tall{height:400px}#fixed{position:fixed;left:5px;top:6px;width:10px;height:11px}</style></head><body><div id='parent'><div id='child'></div><div id='tall'></div></div><div id='fixed'></div></body></html>";
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 2.0, ColorScheme::Light));
        let value: String = doc.js_context.with(|ctx|ctx.eval(r#"
            globalThis.parent = document.getElementById('parent');
            globalThis.child = document.getElementById('child');
            JSON.stringify([child.offsetLeft,child.offsetTop,child.offsetWidth,child.offsetHeight,child.offsetParent.id,parent.offsetLeft,parent.offsetTop,parent.offsetParent.nodeName])
        "#)).unwrap();
        assert_eq!(value, "[25,35,58,28,\"parent\",30,40,\"BODY\"]");
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"(() => {
            parent.scrollTop=60;
            if(child.offsetTop!==35 || child.getBoundingClientRect().y!==17 || child.offsetParent!==parent) return false;
            if(document.getElementById('fixed').offsetParent!==null || document.body.offsetParent!==null || document.documentElement.offsetParent!==null) return false;
            parent.style.display='none';
            if(child.offsetWidth!==0 || child.offsetTop!==0 || child.offsetParent!==null) return false;
            parent.style.display='block';
            if(child.offsetWidth!==58 || child.offsetParent!==parent) return false;
            child.remove();
            return child.offsetWidth===0 && child.offsetParent===null;
        })()"#)).unwrap());
    }

    #[test]
    fn css_scroll_metrics_programmatic_clamping_and_coalesced_non_bubbling_events() {
        let html = "<html><head><style>html,body{margin:0}body{height:900px}#scroller{position:absolute;left:40px;top:50px;width:120px;height:80px;padding:10px;border:2px solid;overflow:hidden}#content{width:400px;height:300px}#clip{width:50px;height:40px;overflow:clip}#large{height:100px}</style></head><body><div id='scroller'><div id='content'></div></div><div id='clip'><div id='large'></div></div></body></html>";
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 2.0, ColorScheme::Light));
        let initial: String = doc.js_context.with(|ctx| ctx.eval(r#"
            globalThis.scroller = document.getElementById('scroller');
            globalThis.content = document.getElementById('content');
            globalThis.events = [];
            globalThis.bubbled = 0;
            scroller.addEventListener('scroll', event => events.push([event.bubbles,event.cancelable,scroller.scrollTop]));
            document.body.addEventListener('scroll',()=>bubbled++);
            JSON.stringify([scroller.clientWidth,scroller.clientHeight,scroller.clientLeft,scroller.clientTop,scroller.scrollWidth,scroller.scrollHeight,innerWidth,innerHeight,content.getBoundingClientRect().x,content.getBoundingClientRect().y])
        "#)).unwrap();
        assert_eq!(initial, "[140,100,2,2,420,320,400,300,52,62]");
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool,_>(r#"(() => {
            scroller.scrollTop = 60;
            scroller.scrollLeft = 20;
            if (scroller.scrollTop !== 60 || scroller.scrollLeft !== 20 || scrollY !== 0 || events.length !== 0) return false;
            const rect = content.getBoundingClientRect();
            if (rect.x !== 32 || rect.y !== 2) return false;
            scroller.scrollBy({top:15});
            return scroller.scrollTop === 75 && scroller.scrollLeft === 20;
        })()"#)).unwrap_or_else(|error| panic!("{}",javascript_error(&doc.js_context,&error,&doc.script_budget))));
        doc.poll(None);
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(events)"))
                .unwrap(),
            "[[false,false,75]]"
        );
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<u32, _>("bubbled"))
                .unwrap(),
            0
        );
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"(() => {
            scroller.scrollTo(1e9,1e9);
            if (scroller.scrollLeft !== 280 || scroller.scrollTop !== 220 || scrollY !== 0) return false;
            scroller.scrollTo({top:-5});
            if (scroller.scrollTop !== 0 || scroller.scrollLeft !== 280) return false;
            scroller.scrollBy(-1e9,-1e9);
            if (scroller.scrollLeft !== 0 || scroller.scrollTop !== 0) return false;
            scroller.scrollTop = Infinity;
            document.getElementById('clip').scrollTop = 50;
            if(document.getElementById('clip').scrollTop !== 0) return false;
            try { scroller.scrollTo({top:1,behavior:'smooth'}); return false; } catch(error) { if(error.name !== 'NotSupportedError') return false; }
            try { scroller.scrollTo({top:1,behavior:'invalid'}); return false; } catch(error) { if(!(error instanceof TypeError)) return false; }
            globalThis.rootEvents = 0;
            document.addEventListener('scroll',()=>rootEvents++);
            scrollTo({top:100});
            scrollBy({top:20});
            if(scrollY !== 120 || pageYOffset !== 120 || document.scrollingElement.scrollTop !== 120) return false;
            if(content.getBoundingClientRect().y !== -58) return false;
            return scroller.scrollTop === 0;
        })()"#)).unwrap());
        doc.poll(None);
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<u32, _>("rootEvents"))
                .unwrap(),
            1
        );
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"(() => {
            scroller.scrollTop=100;
            scroller.style.display='none';
            scroller.scrollTop=200;
            if(scroller.scrollTop!==0 || scroller.getClientRects().length!==0) return false;
            const hidden=scroller.getBoundingClientRect();
            if(hidden.x!==0 || hidden.y!==0 || hidden.width!==0 || hidden.height!==0) return false;
            scroller.style.display='block';
            if(scroller.scrollTop!==100) return false;
            scroller.remove();
            scroller.scrollTop = 200;
            return scroller.clientWidth === 0 && scroller.scrollHeight === 0 && scroller.scrollTop === 0 && scroller.getClientRects().length === 0;
        })()"#)).unwrap());
    }

    #[test]
    fn client_rects_return_css_pixel_inline_fragments_and_stable_list_snapshots() {
        let html = "<html><head><style>html,body{margin:0}#text{width:120px;font:16px sans-serif}</style></head><body><div id='text'><span id='inline'>one two three four five six seven eight nine ten eleven twelve</span></div><div id='box' style='width:50px;height:20px'></div></body></html>";
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 2.0, ColorScheme::Light));
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>(r#"(() => {
            globalThis.inline = document.getElementById('inline');
            globalThis.rects = inline.getClientRects();
            if(rects.length < 2 || rects.item(0) !== rects[0] || rects.item(1e6) !== null) return false;
            const union = inline.getBoundingClientRect();
            const minX = Math.min(...rects.map(rect=>rect.left)), minY=Math.min(...rects.map(rect=>rect.top));
            const maxX = Math.max(...rects.map(rect=>rect.right)), maxY=Math.max(...rects.map(rect=>rect.bottom));
            if(Math.abs(union.x-minX)>0.01 || Math.abs(union.y-minY)>0.01 || Math.abs(union.right-maxX)>0.01 || Math.abs(union.bottom-maxY)>0.01) return false;
            const box = document.getElementById('box');
            const before = box.getClientRects();
            lapui.batch(()=>{box.style.width='70px';if(box.clientWidth!==70)throw new Error('batch metric did not flush');box.style.height='30px';});
            if(before.length!==1 || before[0].width!==50 || box.getClientRects()[0].width!==70 || box.clientHeight!==30) return false;
            box.style.width='0'; box.style.height='0';
            if(box.getClientRects().length!==1 || box.getClientRects()[0].width!==0) return false;
            box.style.display='none';
            if(box.getClientRects().length!==0 || box.clientWidth!==0) return false;
            document.getElementById('text').style.display='none';
            return inline.getClientRects().length===0;
        })()"#)).unwrap());
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        assert!(doc.js_context.with(|ctx|ctx.eval::<bool,_>("document.getElementById('text').style.display='block';innerWidth===800 && innerHeight===600 && rects.length>1 && inline.getClientRects().length===rects.length")).unwrap());
    }

    #[test]
    fn bounding_rect_flushes_style_batches_uses_css_pixels_and_snapshots_detached_nodes() {
        let html = "<html><head><style>html,body{margin:0}body{height:600px}#box{position:absolute;left:30px;top:40px;box-sizing:content-box;width:100px;height:50px;padding:10px;border:2px solid}</style></head><body><div id='box'></div></body></html>";
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(400, 300, 2.0, ColorScheme::Light));
        let initial: String = doc
            .js_context
            .with(|ctx| {
                ctx.eval(
                    r#"
            globalThis.box = document.getElementById('box');
            globalThis.initial = box.getBoundingClientRect();
            JSON.stringify(initial)
        "#,
                )
            })
            .unwrap();
        let initial: Value = serde_json::from_str(&initial).unwrap();
        assert_eq!(
            initial,
            json!({"x":30,"y":40,"width":124,"height":74,"top":40,"left":30,"right":154,"bottom":114})
        );
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool,_>(r#"(() => {
            box.style.width = '160px';
            const changed = box.getBoundingClientRect();
            if(changed.width !== 184 || initial.width !== 124 || !(initial instanceof DOMRectReadOnly) || !(initial instanceof DOMRect)) return false;
            initial.x = 1;
            if(box.getBoundingClientRect().x !== 30) return false;
            let insideWidth = 0;
            lapui.batch(() => { box.style.width = '200px'; insideWidth = box.getBoundingClientRect().width; box.style.height = '60px'; });
            if(insideWidth !== 224 || box.getBoundingClientRect().height !== 84) return false;
            box.style.display = 'none';
            if(box.getBoundingClientRect().width !== 0) return false;
            box.style.display = 'block';
            box.remove();
            if(Object.values(box.getBoundingClientRect().toJSON()).some(value => value !== 0)) return false;
            document.body.appendChild(box);
            if(box.getBoundingClientRect().width !== 224) return false;
            const negative = DOMRect.fromRect({x:10,y:20,width:-4,height:-6});
            const readonly = new DOMRectReadOnly(1,2,3,4);
            return negative.left === 6 && negative.top === 14 && negative.right === 10 && negative.bottom === 20 && readonly.width === 3;
        })()"#)).unwrap());
        let mut scroll = doc.inner().viewport_scroll();
        scroll.x = 12.0;
        scroll.y = 15.0;
        doc.inner_mut().set_viewport_scroll(scroll);
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>(
                "box.getBoundingClientRect().x === 18 && box.getBoundingClientRect().y === 25"
            ))
            .unwrap());
        // Responsive CSS layout is recalculated after a viewport change.
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("box.style.boxSizing = 'border-box'; box.style.width = '50%'")
            })
            .unwrap();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<f64, _>("box.getBoundingClientRect().width"))
                .unwrap(),
            100.0
        );
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 300, 2.0, ColorScheme::Light));
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<f64, _>("box.getBoundingClientRect().width"))
                .unwrap(),
            200.0
        );
    }

    #[test]
    fn animation_frames_share_timestamp_checkpoint_microtasks_and_defer_nested_requests() {
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None,
            "<html><body><p id='status'>initial</p></body></html>", r#"
            globalThis.events = [];
            globalThis.times = [];
            globalThis.clockValid = false;
            let cancelled;
            requestAnimationFrame(function(timestamp) {
                clockValid = this === globalThis && performance.now() >= timestamp && performance.timeOrigin > 1000000000000;
                events.push('a'); times.push(timestamp);
                cancelAnimationFrame(cancelled);
                requestAnimationFrame(() => events.push('nested'));
                queueMicrotask(() => {
                    events.push('microtask');
                    requestAnimationFrame(() => events.push('microtask-frame'));
                });
                document.getElementById('status').textContent = 'frame';
            });
            cancelled = requestAnimationFrame(() => events.push('cancelled'));
            requestAnimationFrame(() => { events.push('throw'); throw new Error('animation fixture error'); });
            requestAnimationFrame(timestamp => { times.push(timestamp); events.push('last'); });
        "#).unwrap();
        assert!(doc.has_animation_callbacks());
        doc.poll(None);
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("events.length === 0"))
            .unwrap());
        assert!(doc.animation_frame());
        assert_eq!(text(&doc, "status"), "frame");
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool,_>("clockValid && events.join(',') === 'a,microtask,throw,last' && times.length === 2 && times[0] === times[1]")).unwrap());
        assert!(doc.has_animation_callbacks());
        assert!(doc.animation_frame());
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>(
                "events.join(',') === 'a,microtask,throw,last,nested,microtask-frame'"
            ))
            .unwrap());
        assert!(!doc.has_animation_callbacks());
        assert!(!doc.animation_frame());
        assert_eq!(doc.script_diagnostics.borrow().len(), 1);
        assert_eq!(doc.script_diagnostics.borrow()[0].phase, "animation-frame");
    }

    #[test]
    fn animation_capacity_cancellation_and_document_shutdown_release_native_requests() {
        let (doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body></body></html>",
            "",
        )
        .unwrap();
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool,_>(r#"(() => {
            let typeError = false, rangeError = false;
            try { requestAnimationFrame('invalid'); } catch(error) { typeError = error instanceof TypeError; }
            const handles = [];
            for(let index=0;index<1024;index++) handles.push(requestAnimationFrame(() => {}));
            try { requestAnimationFrame(() => {}); } catch(error) { rangeError = error instanceof RangeError; }
            handles.forEach(cancelAnimationFrame);
            cancelAnimationFrame(NaN); cancelAnimationFrame(-1); cancelAnimationFrame(0);
            const replacement = requestAnimationFrame(() => {});
            cancelAnimationFrame(String(replacement));
            return typeError && rangeError && replacement > handles[handles.length - 1];
        })()"#)).unwrap());
        assert!(!doc.has_animation_callbacks());
        let frames = doc.frames.clone();
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("requestAnimationFrame(() => {})"))
            .unwrap();
        assert!(frames.borrow().is_pending());
        drop(doc);
        assert!(!frames.borrow().is_pending());
        assert!(!frames.borrow_mut().request(12345));
    }

    #[test]
    fn animation_slice_defers_remaining_callbacks_and_interrupt_suspends_the_document() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body></body></html>",
            r#"
            globalThis.steps = [];
            requestAnimationFrame(timestamp => {
                steps.push(timestamp);
                const end = performance.now() + 30;
                while(performance.now() < end) {}
            });
            requestAnimationFrame(timestamp => steps.push(timestamp));
        "#,
        )
        .unwrap();
        doc.animation_frame();
        assert!(doc.has_animation_callbacks());
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("steps.length === 1"))
            .unwrap());
        doc.animation_frame();
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("steps.length === 2 && steps[1] > steps[0]"))
            .unwrap());
        assert!(!doc.has_animation_callbacks());
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    r#"
            globalThis.ranAfterInterrupt = false;
            requestAnimationFrame(() => { while(true) {} });
            requestAnimationFrame(() => ranAfterInterrupt = true);
        "#,
                )
            })
            .unwrap();
        doc.animation_frame();
        assert!(doc.script_budget.interrupted());
        assert!(!doc.has_animation_callbacks());
        assert!(!doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("ranAfterInterrupt"))
            .unwrap());
        let diagnostic = control_request(&mut doc, json!({"method":"diagnostics"})).unwrap();
        assert_eq!(diagnostic["scriptStatus"], "suspended");
        assert!(diagnostic["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error["source"]
                .as_str()
                .unwrap_or_default()
                .starts_with("animation-frame:")));
    }

    #[test]
    fn javascript_action_discovery_and_parameterized_availability_share_rust_handlers() {
        let actions = crate::demo::files().unwrap();
        let startup = actions.create_scope("startup").unwrap();
        startup
            .register_query(
                crate::action::ActionInfo {
                    id: "startup.query".into(),
                    description: "Startup scope".into(),
                    input_schema: json!({"type":"object"}),
                    output_schema: json!({"type":"string"}),
                    kind: crate::action::ActionKind::Read,
                },
                crate::action_catalog::ActionOptions::default(),
                |_, _| Ok(json!("startup")),
            )
            .unwrap();
        let (mut doc, _) = LapuiDocument::new_with_source(actions.clone(), None,
            "<html><body><p id='status' role='status'>waiting</p></body></html>", r#"
            (async()=>{
                if((await lapui.invoke('startup.query')).result!=='startup')throw Error('startup registration');
                const listed=await lapui.actions.list({prefix:'files.',limit:1});
                if(listed.items.length!==1 || !listed.hasMore || listed.items[0].input_schema)throw Error('summary pagination');
                const later=await lapui.actions.list({prefix:'files.',cursor:listed.nextCursor});
                if(later.items.length!==2)throw Error('resume page');
                const description=await lapui.actions.describe('files.rename');
                if(!description.hasAvailabilityCheck || !description.input_schema.required.includes('fileId'))throw Error('description');
                const args={fileId:'file-1',name:'AI renamed.md',expectedFileVersion:1};
                if(!(await lapui.actions.check('files.rename',args)).available)throw Error('available');
                await lapui.invoke('files.rename',args,{requestId:'js-rename'});
                const blocked=await lapui.actions.check('files.rename',args);
                if(blocked.available || blocked.reason.code!=='stale_entity')throw Error('stale check');
                try{await lapui.invoke('files.rename',args);throw Error('stale write ran');}catch(error){if(error.code!=='stale_entity')throw error;}
                const replay=await lapui.invoke('files.rename',args,{requestId:'js-rename'});
                if(replay.version!==1)throw Error('replay');
                for(const options of [null,[],1,{method:'actions.list'},{limit:0}]) {
                    try{await lapui.actions.list(options);throw Error('bad options accepted');}catch(error){if(error.code!=='invalid_request')throw error;}
                }
                document.getElementById('status').textContent='discovered, checked, renamed, blocked, replayed';
            })().catch(error=>document.getElementById('status').textContent='ERROR '+error.message);
            "#).unwrap();
        doc.attach_action_scope(startup).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while text(&doc, "status") == "waiting" {
            assert!(Instant::now() < deadline);
            doc.poll(None);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            text(&doc, "status"),
            "discovered, checked, renamed, blocked, replayed"
        );
        assert_eq!(actions.observe().version, 1);
        assert!(doc.script_diagnostics.borrow().is_empty());
        doc.create_action_scope("document")
            .unwrap()
            .register_query(
                crate::action::ActionInfo {
                    id: "document.query".into(),
                    description: "Document action".into(),
                    input_schema: json!({"type":"object"}),
                    output_schema: json!({"type":"string"}),
                    kind: crate::action::ActionKind::Read,
                },
                crate::action_catalog::ActionOptions::default(),
                |_, _| Ok(json!("document")),
            )
            .unwrap();
        assert!(actions.describe_action("document.query").is_ok());
        drop(doc);
        assert_eq!(
            actions.describe_action("startup.query").unwrap_err().code,
            "unknown_action"
        );
        assert_eq!(
            actions.describe_action("document.query").unwrap_err().code,
            "unknown_action"
        );
        assert!(actions.describe_action("files.rename").is_ok());
    }

    #[test]
    fn resumable_demo_updates_from_feed_and_replays_changes_after_human_pause() {
        let actions = ActionRegistry::default();
        let (mut doc, _) = LapuiDocument::new_with_source(
            actions.clone(),
            None,
            include_str!("../examples/changes-demo/index.html"),
            "",
        )
        .unwrap();
        fn wait_text(doc: &mut LapuiDocument, id: &str, expected: &str) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while text(doc, id) != expected {
                assert!(Instant::now() < deadline, "{}: {}", id, text(doc, id));
                doc.poll(None);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        fn activate(doc: &mut LapuiDocument, id: &str) {
            let snapshot = control_snapshot(&doc.dom.borrow());
            let target = snapshot["controls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|control| control["id"] == id)
                .unwrap();
            control_request(doc, json!({"method":"activate", "documentEpoch":snapshot["documentEpoch"], "ref":target["ref"]})).unwrap();
        }
        wait_text(&mut doc, "status", "Following shared state");
        activate(&mut doc, "increment");
        wait_text(&mut doc, "counter", "Count: 1");
        activate(&mut doc, "pause");
        actions.invoke("counter.increment", &json!({})).unwrap();
        actions.invoke("counter.increment", &json!({})).unwrap();
        for _ in 0..20 {
            doc.poll(None);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(text(&doc, "counter"), "Count: 1");
        activate(&mut doc, "resume");
        wait_text(&mut doc, "counter", "Count: 3");
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn javascript_change_subscriptions_deliver_rust_state_and_explicit_host_events_off_ui_thread() {
        let actions = ActionRegistry::default();
        let (mut doc,_)=LapuiDocument::new_with_source(actions.clone(),None,
            "<html><body><p id='state' role='status'>waiting</p><p id='host' role='status'>waiting</p></body></html>",r#"
            globalThis.stateReady=false;globalThis.hostReady=false;globalThis.invalidChanges=0;
            (async()=>{
                const base=await lapui.changes.subscribe({scope:'state'});
                stateReady=true;
                const page=await lapui.changes.subscribe({scope:'state',cursor:base.cursor,waitMs:1000});
                const change=page.records.find(record=>record.kind==='state_changed');
                document.getElementById('state').textContent='Count: '+change.data.delta.set.count;
            })().catch(error=>{document.getElementById('state').textContent='ERROR '+error.message});
            (async()=>{
                const base=await lapui.changes.subscribe({scope:'host'});
                hostReady=true;
                const page=await lapui.changes.subscribe({scope:'host',cursor:base.cursor,waitMs:1000});
                document.getElementById('host').textContent=page.records[0].data.payload.message;
            })().catch(error=>{document.getElementById('host').textContent='ERROR '+error.message});
            for(const request of [{waitMs:1001},{limit:0},{cursor:'bad'},{scope:'private'},{unsupported:true}])
                lapui.changes.subscribe(request).catch(error=>{if(error.code) invalidChanges++});
        "#).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("stateReady && hostReady && invalidChanges===5"))
            .unwrap()
        {
            assert!(Instant::now() < deadline);
            doc.poll(None);
            std::thread::sleep(Duration::from_millis(1));
        }
        actions
            .emit_event("notice", json!({"message":"Native host event"}))
            .unwrap();
        actions.invoke("counter.increment", &json!({})).unwrap();
        while text(&doc, "state") != "Count: 1" || text(&doc, "host") != "Native host event" {
            assert!(
                Instant::now() < deadline,
                "{} / {}",
                text(&doc, "state"),
                text(&doc, "host")
            );
            doc.poll(None);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn option_value_properties_support_framework_updates_without_text_fill_or_checked_changes() {
        let (doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None,
            "<html><body><input id='r' type='radio' checked><input id='c' type='checkbox'><input id='text'><input id='hidden' type='hidden'></body></html>",
            r#"
              const r=document.getElementById('r'),c=document.getElementById('c');
              if(r.value!=='on'||c.value!=='on') throw new Error('missing default option value');
              r.value='choice'; c.value='accepted'; document.getElementById('hidden').value='token';
              if(lapui.fill('r','overwrite')||lapui.fill('c','overwrite')||lapui.fill('hidden','overwrite')) throw new Error('non-text fill accepted');
              if(!r.checked||c.checked||r.value!=='choice'||c.value!=='accepted') throw new Error('option property update failed');
              if(!lapui.fill('text','editable')) throw new Error('text fill rejected');
              c.removeAttribute('value'); if(c.value!=='on') throw new Error('option default did not restore');
            "#).unwrap();
        assert!(doc.script_diagnostics.borrow().is_empty());
        doc.dom.borrow_mut().resolve(0.0);
        let snapshot = control_snapshot(&doc.dom.borrow());
        let controls = snapshot["controls"].as_array().unwrap();
        assert_eq!(
            controls.iter().find(|c| c["id"] == "r").unwrap()["value"],
            "choice"
        );
        assert_eq!(
            controls.iter().find(|c| c["id"] == "c").unwrap()["value"],
            "on"
        );
    }

    #[test]
    fn startup_infinite_loop_suspends_scripts_but_retains_structured_diagnostics_and_controls() {
        for kind in ["", "type='module'", "microtask"] {
            let started = Instant::now();
            let html = if kind == "microtask" {
                "<html><body><button id='run'>Run</button><script>queueMicrotask(()=>{while(true){}});</script></body></html>".to_owned()
            } else {
                format!("<html><body><button id='run'>Run</button><script {kind}>while(true){{try {{throw 1;}}catch{{}}}}</script><script {kind}>document.getElementById('run').textContent='must not execute';</script></body></html>")
            };
            let (mut doc, _) =
                LapuiDocument::new_with_source(ActionRegistry::default(), None, &html, "").unwrap();
            assert!(started.elapsed() < Duration::from_secs(8), "{kind}");
            assert_eq!(text(&doc, "run"), "Run", "{kind}");
            let diagnostics = control_request(&mut doc, json!({"method":"diagnostics"})).unwrap();
            assert_eq!(diagnostics["scriptStatus"], "suspended", "{kind}");
            assert!(
                diagnostics["errors"][0]["message"]
                    .as_str()
                    .unwrap()
                    .contains("execution limit"),
                "{kind}"
            );
            let snapshot = control_request(&mut doc, json!({"method":"controls"})).unwrap();
            assert!(!snapshot["controls"].as_array().unwrap().is_empty());
            assert_eq!(snapshot["validationAvailable"], false);
            assert!(!doc.timers.handle.arm(1, 1));
            assert!(doc.lifetime.is_cancelled());
            assert!(!doc.poll(None));
        }
    }

    #[test]
    fn callback_limits_cover_control_microtasks_timers_and_host_render_completion() {
        for mode in ["click", "microtask", "timer", "completion"] {
            let script = match mode {
                "click" => "document.getElementById('run').onclick=()=>{while(true){}};",
                "microtask" => "document.getElementById('run').onclick=()=>queueMicrotask(()=>{while(true){}});",
                "timer" => "setTimeout(()=>{while(true){}},1);",
                _ => "globalThis.__lapui_render=()=>{while(true){}};",
            };
            let (mut doc, notify) = LapuiDocument::new_with_source(
                ActionRegistry::default(),
                None,
                "<html><body><button id='run'>Run</button></body></html>",
                script,
            )
            .unwrap();
            let snapshot = control_snapshot(&doc.dom.borrow());
            let command = json!({"method":"activate","documentEpoch":snapshot["documentEpoch"],"ref":snapshot["controls"][0]["ref"]});
            let started = Instant::now();
            if matches!(mode, "click" | "microtask") {
                assert_eq!(
                    control_request(&mut doc, command.clone()).unwrap_err().code,
                    "script_error"
                );
            } else {
                if mode == "completion" {
                    notify.send(Ok(json!({"counter":1}))).unwrap();
                }
                let deadline = started + Duration::from_secs(4);
                while !doc.script_budget.interrupted() && Instant::now() < deadline {
                    doc.poll(None);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            assert!(doc.script_budget.interrupted(), "{mode}");
            assert!(started.elapsed() < Duration::from_secs(4), "{mode}");
            assert_eq!(
                control_request(&mut doc, command).unwrap_err().code,
                "script_suspended",
                "{mode}"
            );
            let diagnostics = control_request(&mut doc, json!({"method":"diagnostics"})).unwrap();
            assert_eq!(diagnostics["scriptStatus"], "suspended", "{mode}");
            assert!(
                diagnostics["errors"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["message"]
                        .as_str()
                        .unwrap()
                        .contains("execution limit")),
                "{mode}"
            );
            assert!(doc.lifetime.is_cancelled(), "{mode}");
            assert!(!doc.timers.handle.arm(7, 1), "{mode}");
        }
    }

    #[test]
    fn native_event_interrupt_prevents_default_and_never_reenters_partial_bridge_state() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body><input id='check' type='checkbox'></body></html>",
            "document.getElementById('check').onclick=()=>{while(true){}};",
        )
        .unwrap();
        let target = doc.dom.borrow().get_element_by_id("check").unwrap();
        let handler = JsHandler {
            runtime: doc.js_runtime.clone(),
            context: doc.js_context.clone(),
            budget: doc.script_budget.clone(),
            diagnostics: doc.script_diagnostics.clone(),
        };
        let event = doc
            .dom
            .borrow()
            .get_node(target)
            .unwrap()
            .synthetic_click_event(Modifiers::empty());
        // This is the native dispatch path, not a direct JS invocation.
        EventDriver::new(&mut doc, handler).handle_dom_event(DomEvent::new(target, event));
        assert!(crate::forms::checked(&doc.dom.borrow(), target).unwrap());
        doc.poll(None);
        assert!(doc.script_budget.interrupted());
        assert!(doc.lifetime.is_cancelled());
        assert_eq!(doc.script_diagnostics.borrow().len(), 1);
        assert!(!doc.poll(None));
    }

    #[test]
    fn file_tool_preserves_human_drafts_on_ai_conflict_and_displays_scan_completion() {
        let actions = crate::demo::files().unwrap();
        let (mut doc, notify) = LapuiDocument::new_with_source(
            actions.clone(),
            None,
            include_str!("../ui/files/index.html"),
            include_str!("../ui/files/app.js"),
        )
        .unwrap();
        let snapshot = control_request(&mut doc, json!({"method":"controls"})).unwrap();
        let epoch = snapshot["documentEpoch"].clone();
        let reference = |id: &str| {
            snapshot["controls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|control| control["id"] == id)
                .unwrap()["ref"]
                .clone()
        };
        control_request(&mut doc, json!({"method":"fill","documentEpoch":epoch,"ref":reference("rename"),"value":"人工草稿.md"})).unwrap();
        let result = actions
            .invoke(
                "files.rename",
                &json!({"fileId":"file-1","name":"AI名称.md","expectedFileVersion":1}),
            )
            .unwrap();
        notify.send(Ok(json!(result))).unwrap();
        for _ in 0..100 {
            doc.poll(None);
            if text(&doc, "selected").contains("AI名称") {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(text(&doc, "selected").contains("AI名称"));
        let value: String = doc
            .js_context
            .with(|ctx| ctx.eval("document.getElementById('rename').value"))
            .unwrap();
        assert_eq!(value, "人工草稿.md");
        control_request(
            &mut doc,
            json!({"method":"activate","documentEpoch":epoch,"ref":reference("save")}),
        )
        .unwrap();
        for _ in 0..500 {
            doc.poll(None);
            if text(&doc, "rename-status").contains("草稿已保留") {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(text(&doc, "rename-status").contains("草稿已保留"));
        assert_eq!(actions.observe().version, 1);
        control_request(
            &mut doc,
            json!({"method":"activate","documentEpoch":epoch,"ref":reference("refresh")}),
        )
        .unwrap();
        control_request(&mut doc, json!({"method":"fill","documentEpoch":epoch,"ref":reference("rename"),"value":"最终名称.md"})).unwrap();
        control_request(
            &mut doc,
            json!({"method":"activate","documentEpoch":epoch,"ref":reference("save")}),
        )
        .unwrap();
        for _ in 0..500 {
            doc.poll(None);
            if text(&doc, "rename-status") == "名称已保存" {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(text(&doc, "rename-status"), "名称已保存");
        assert_eq!(actions.observe().state["files"][0]["version"], 3);
        control_request(
            &mut doc,
            json!({"method":"activate","documentEpoch":epoch,"ref":reference("scan")}),
        )
        .unwrap();
        for _ in 0..1000 {
            doc.poll(None);
            if text(&doc, "scan-status").starts_with("扫描完成") {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(text(&doc, "scan-status"), "扫描完成：3 个示例文件");
        assert_eq!(actions.observe().version, 2);
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn js_waits_for_operation_revisions_and_cancels_cooperatively() {
        let actions = ActionRegistry::new(json!({})).unwrap();
        let info = crate::action::ActionInfo {
            id: "scan".into(),
            description: "Test scan".into(),
            input_schema: json!({"type":"object","additionalProperties":false}),
            output_schema: json!({"type":"string"}),
            kind: crate::action::ActionKind::Operation,
        };
        actions
            .register_operation(info.clone(), |_, _, context| {
                context.report(0.5, "half")?;
                context.delay(Duration::from_millis(10))?;
                Ok(json!("complete"))
            })
            .unwrap();
        let mut info = info;
        info.id = "slow".into();
        actions
            .register_operation(info, |_, _, context| {
                context.delay(Duration::from_secs(30))?;
                Ok(json!("unexpected"))
            })
            .unwrap();
        let script = r#"
          globalThis.operationTest = 'pending';
          (async () => {
            const accepted = await lapui.invoke('scan', {}, {requestId:'scan-js'});
            const id = accepted.result.operationId;
            let job = lapui.operation(id);
            while (!['completed','failed','cancelled'].includes(job.execution)) job = await lapui.waitOperation(id, job.revision);
            if (job.output !== 'complete' || job.execution !== 'completed') throw new Error('completion');
            const slow = await lapui.invoke('slow');
            job = lapui.cancelOperation(slow.result.operationId);
            while (!['completed','failed','cancelled'].includes(job.execution)) job = await lapui.waitOperation(job.operationId, job.revision);
            if (job.execution !== 'cancelled') throw new Error('cancel');
            if (lapui.observe().version !== 0) throw new Error('job wrote state');
            operationTest = 'passed';
          })().catch(error => operationTest = 'failed:' + error);
        "#;
        let (mut doc, _) =
            LapuiDocument::new_with_source(actions, None, "<html><body></body></html>", script)
                .unwrap();
        for _ in 0..2000 {
            doc.poll(None);
            let status: String = doc
                .js_context
                .with(|ctx| ctx.globals().get("operationTest"))
                .unwrap();
            if status != "pending" {
                assert_eq!(status, "passed");
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.globals().get::<_, String>("operationTest"))
                .unwrap(),
            "passed"
        );
    }

    #[test]
    fn registered_host_action_shares_js_preconditions_state_and_trace() {
        let actions = ActionRegistry::new(json!({"name":"original"})).unwrap();
        actions.register(crate::action::ActionInfo {
            id: "name.set".into(), description: "Set display name".into(),
            input_schema: json!({"type":"object","required":["name"],"additionalProperties":false,"properties":{"name":{"type":"string","minLength":1}}}),
                output_schema: json!({"type":"string"}),
                kind: crate::action::ActionKind::Write,
        }, |state, args| { state["name"] = args["name"].clone(); Ok(args["name"].clone()) }).unwrap();
        let script = r#"
          globalThis.hostTest = 'pending';
          (async () => {
            if (lapui.observe().state.name !== 'original') throw new Error('observe');
            const options = {requestId:'shared-rename', expectedVersion:0};
            const first = await lapui.invoke('name.set', {name:'中文'}, options);
            const retry = await lapui.invoke('name.set', {name:'中文'}, options);
            if (first.version !== 1 || retry.version !== 1 || retry.result !== '中文') throw new Error('dedup');
            try { await lapui.invoke('name.set', {name:'overwritten'}, {expectedVersion:0}); throw new Error('stale accepted'); }
            catch (error) { if (error.code !== 'stale_state') throw error; }
            try { await lapui.invoke('name.set', {name:''}); throw new Error('invalid accepted'); }
            catch (error) { if (error.code !== 'invalid_arguments') throw error; }
            if (lapui.observe().state.name !== '中文') throw new Error('state');
            if (!lapui.trace().records.some(item => item.outcome === 'replayed')) throw new Error('trace');
            hostTest = 'passed';
          })().catch(error => hostTest = 'failed:' + error);
        "#;
        let (mut doc, _) = LapuiDocument::new_with_source(
            actions.clone(),
            None,
            "<html><body></body></html>",
            script,
        )
        .unwrap();
        for _ in 0..2000 {
            doc.poll(None);
            let status: String = doc
                .js_context
                .with(|ctx| ctx.globals().get("hostTest"))
                .unwrap();
            if status != "pending" {
                assert_eq!(status, "passed");
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.globals().get::<_, String>("hostTest"))
                .unwrap(),
            "passed"
        );
        assert_eq!(
            actions
                .invoke_checked(
                    "name.set",
                    &json!({"name":"中文"}),
                    Some("shared-rename"),
                    Some(0)
                )
                .unwrap()
                .version,
            1
        );
        assert_eq!(actions.observe().state["name"], "中文");
    }

    fn text(doc: &LapuiDocument, id: &str) -> String {
        let inner = doc.inner();
        inner
            .get_element_by_id(id)
            .and_then(|node| inner.get_node(node))
            .map(|node| node.text_content())
            .unwrap_or_default()
    }

    #[test]
    fn dom_gc_preserves_connected_listeners_and_retained_detached_trees() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(), None,
            "<html><body><button id='connected'>Run</button></body></html>",
            r#"
              globalThis.clicks = 0;
              (() => {
                const button = document.getElementById('connected');
                button.addEventListener('click', () => { if (button.id === 'connected') clicks++; });
                const root = document.createElement('section');
                const child = document.createElement('button');
                root.appendChild(child);
                document.body.appendChild(root);
                root.remove();
                child.addEventListener('click', () => { root.setAttribute('data-clicks', '1'); });
                globalThis.keptChild = child;
                globalThis.rootRef = root.__ref;
                globalThis.childRef = child.__ref;
              })();
            "#,
        ).unwrap();
        doc.js_runtime.run_gc();
        doc.poll(None);
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>(
                r#"
          lapui.activate('connected') && clicks === 1 &&
          keptChild.parentNode.__ref === rootRef &&
          keptChild.parentNode.firstChild === keptChild
        "#
            ))
            .unwrap());
        doc.js_context.with(|ctx| ctx.eval::<(), _>(r#"
          document.body.appendChild(keptChild.parentNode);
          lapui.activate(childRef);
          if (keptChild.parentNode.getAttribute('data-clicks') !== '1') throw new Error('detached listener lost');
          keptChild.parentNode.remove();
          globalThis.keptStyle = keptChild.style;
          globalThis.keptChild = null;
        "#)).unwrap();
        doc.poll(None);
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>(
                "Boolean(__lapui_resolve(rootRef)) && Boolean(__lapui_resolve(childRef))"
            ))
            .unwrap());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("keptStyle.color = 'red'; globalThis.keptStyle = null;"))
            .unwrap();
        doc.js_runtime.run_gc();
        doc.poll(None);
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("!__lapui_resolve(rootRef) && !__lapui_resolve(childRef) && lapui.activate('connected') && clicks === 2")).unwrap());
    }

    #[test]
    fn dom_gc_collects_listener_cycles_fragments_clones_and_replaced_children() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body><div id='mount'></div></body></html>",
            "",
        )
        .unwrap();
        for round in 0..10 {
            doc.js_context.with(|ctx| ctx.eval::<(), _>(r#"
              globalThis.retiredRefs = [];
              (() => {
                const mount = document.getElementById('mount');
                for (let i = 0; i < 100; i++) {
                  const fragment = document.createDocumentFragment();
                  const root = document.createElement('div');
                  root.innerHTML = '<button>Temporary</button><span>Text</span>';
                  const button = root.firstChild;
                  button.addEventListener('click', () => root.setAttribute('data-cycle', button.textContent));
                  retiredRefs.push(fragment.__ref, root.__ref, ...root.childNodes.map(node => node.__ref));
                  fragment.append(root);
                  const clone = fragment.cloneNode(true);
                  retiredRefs.push(clone.__ref, clone.firstChild.__ref);
                  mount.append(fragment, clone);
                  lapui.batch(() => lapui.batch(() => { mount.textContent = ''; }));
                  const standalone = document.createElement('div');
                  standalone.onclick = () => standalone.id;
                  retiredRefs.push(standalone.__ref);
                }
                mount.innerHTML = '<p>Replaced</p>';
                retiredRefs.push(mount.firstChild.__ref);
                mount.innerHTML = '';
              })();
            "#)).unwrap();
            doc.poll(None);
            assert!(
                doc.js_context
                    .with(|ctx| ctx.eval::<bool, _>(
                        "retiredRefs.every(reference => !__lapui_resolve(reference))"
                    ))
                    .unwrap(),
                "round {round}"
            );
        }
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn dom_insertion_rejects_cycles_and_preserves_self_insert_identity() {
        let (doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body><div id='parent'><span id='child'></span></div></body></html>",
            "",
        )
        .unwrap();
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>(
                r#"
          (() => {
            const parent = document.getElementById('parent');
            const child = parent.firstChild;
            let rejected = 0;
            for (const action of [() => child.appendChild(parent), () => parent.appendChild(parent),
              () => parent.appendChild(document)]) {
              try { action(); } catch (error) { rejected++; }
            }
            parent.insertBefore(child, child);
            return rejected === 3 && parent.firstChild === child && child.parentNode === parent;
          })()
        "#
            ))
            .unwrap());
    }

    #[test]
    fn dom_gc_reclaims_laid_out_subtrees_and_rejects_reused_stale_handles() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body><div id='mount'></div></body></html>",
            "",
        )
        .unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(600, 400, 1.0, ColorScheme::Light));
        let mut previous = String::new();
        for _ in 0..50 {
            let reference = doc.js_context.with(|ctx| ctx.eval::<String, _>(r#"
              (() => {
                const mount = document.getElementById('mount');
                mount.innerHTML = '<section>before<div style="display:block">Block</div>after<button>Run</button></section>';
                const root = mount.firstChild;
                const button = root.lastChild;
                button.onclick = () => root.id;
                return root.__ref;
              })()
            "#)).unwrap();
            assert_ne!(reference, previous);
            if !previous.is_empty() {
                assert!(resolve_node_ref(&doc.dom.borrow(), &previous).is_none());
            }
            doc.inner_mut().resolve(0.0);
            doc.js_context
                .with(|ctx| {
                    ctx.eval::<(), _>("document.getElementById('mount').replaceChildren();")
                })
                .unwrap();
            doc.poll(None);
            doc.inner_mut().resolve(0.0);
            assert!(resolve_node_ref(&doc.dom.borrow(), &reference).is_none());
            previous = reference;
        }
    }

    fn control_request(doc: &mut LapuiDocument, command: Value) -> Result<Value, ActionError> {
        let controller = doc.controller();
        let caller =
            std::thread::spawn(move || controller.request(command, Duration::from_secs(2)));
        for _ in 0..2000 {
            doc.poll(None);
            if caller.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        caller.join().unwrap()
    }

    #[test]
    fn background_controls_share_dom_events_and_reject_cross_document_or_detached_refs() {
        let html = r#"<html><body>
          <label for="name">Name</label><input id="name">
          <input id="password" type="password" value="secret">
          <input id="readonly" readonly><button id="disabled" disabled>Disabled</button>
          <input id="agree" type="checkbox"><button id="button">Run</button>
          <button id="throwing">Throw</button>
          <p id="status">initial</p>
          <script>throw new Error('fixture startup failure')</script>
          </body></html>"#;
        let script = r#"
          globalThis.controlEvents = [];
          globalThis.throwCount = 0;
          document.getElementById('throwing').addEventListener('click', () => {
            if (++throwCount === 1) throw new Error('control fixture failure');
            throw 'primitive control failure';
          });
          const name = document.getElementById('name');
          name.addEventListener('input', () => controlEvents.push('input:' + name.value));
          name.addEventListener('change', () => controlEvents.push('change:' + name.value));
          const agree = document.getElementById('agree');
          agree.addEventListener('change', () => controlEvents.push('check:' + agree.checked));
          document.getElementById('button').addEventListener('click', () => {
            document.getElementById('status').textContent = 'activated';
          });
        "#;
        let (old, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        let old_snapshot = control_snapshot(&old.dom.borrow());
        let old_ref = old_snapshot["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|control| control["id"] == "button")
            .unwrap()["ref"]
            .clone();
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, script).unwrap();
        let snapshot = control_request(&mut doc, json!({"method":"controls"})).unwrap();
        let epoch = snapshot["documentEpoch"].as_u64().unwrap();
        assert_ne!(old_snapshot["documentEpoch"], epoch);
        let reference = |id: &str| {
            snapshot["controls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|control| control["id"] == id)
                .unwrap()["ref"]
                .clone()
        };
        assert!(snapshot["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|control| control["id"] == "password")
            .unwrap()
            .get("value")
            .is_none());
        let diagnostics = control_request(&mut doc, json!({"method":"diagnostics"})).unwrap();
        assert_eq!(diagnostics["errors"].as_array().unwrap().len(), 1);
        assert!(diagnostics["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("fixture startup failure"));
        for message in ["control fixture failure", "primitive control failure"] {
            let result = control_request(
                &mut doc,
                json!({"method":"activate", "documentEpoch":epoch, "ref":reference("throwing")}),
            )
            .unwrap();
            assert_eq!(result["status"], "dispatched");
            assert!(result["dispatchErrors"][0]["message"]
                .as_str()
                .unwrap()
                .contains(message));
        }
        assert_eq!(doc.script_diagnostics.borrow().len(), 3);
        assert_eq!(doc.script_diagnostics.borrow()[2].phase, "event");
        assert_eq!(control_request(&mut doc, json!({"method":"activate", "documentEpoch":epoch, "ref":reference("button"), "requestId":"not-an-invoke"})).unwrap_err().code, "invalid_request");
        assert_eq!(text(&doc, "status"), "initial");
        assert_eq!(control_request(&mut doc, json!({"method":"activate", "documentEpoch":old_snapshot["documentEpoch"], "ref":old_ref})).unwrap_err().code, "stale_document");
        assert_eq!(
            control_request(
                &mut doc,
                json!({"method":"activate", "documentEpoch":epoch, "ref":old_ref})
            )
            .unwrap_err()
            .code,
            "stale_reference"
        );
        assert_eq!(
            control_request(
                &mut doc,
                json!({"method":"activate", "documentEpoch":epoch, "ref":"button"})
            )
            .unwrap_err()
            .code,
            "stale_reference"
        );
        assert_eq!(
            control_request(
                &mut doc,
                json!({"method":"activate", "ref":reference("button")})
            )
            .unwrap_err()
            .code,
            "invalid_request"
        );
        assert_eq!(control_request(&mut doc, json!({"method":"check", "documentEpoch":epoch, "ref":reference("agree"), "checked":"yes"})).unwrap_err().code, "invalid_request");
        assert_eq!(
            control_request(
                &mut doc,
                json!({"method":"activate", "documentEpoch":epoch, "ref":reference("disabled")})
            )
            .unwrap_err()
            .code,
            "control_unavailable"
        );
        assert_eq!(control_request(&mut doc, json!({"method":"fill", "documentEpoch":epoch, "ref":reference("readonly"), "value":"invalid"})).unwrap_err().code, "control_unavailable");
        assert_eq!(control_request(&mut doc, json!({"method":"fill", "documentEpoch":epoch, "ref":reference("name"), "value":"张三"})).unwrap()["status"], "dispatched");
        control_request(&mut doc, json!({"method":"check", "documentEpoch":epoch, "ref":reference("agree"), "checked":true})).unwrap();
        let focused = control_request(
            &mut doc,
            json!({"method":"focus", "documentEpoch":epoch, "ref":reference("name")}),
        )
        .unwrap();
        assert_eq!(
            focused["controls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|control| control["id"] == "name")
                .unwrap()["focused"],
            true
        );
        control_request(
            &mut doc,
            json!({"method":"activate", "documentEpoch":epoch, "ref":reference("button")}),
        )
        .unwrap();
        assert_eq!(text(&doc, "status"), "activated");
        let events: String = doc
            .js_context
            .with(|ctx| ctx.eval("JSON.stringify(controlEvents)"))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&events).unwrap(),
            json!(["input:张三", "change:张三", "check:true"])
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("document.getElementById('button').remove()"))
            .unwrap();
        assert_eq!(
            control_request(
                &mut doc,
                json!({"method":"activate", "documentEpoch":epoch, "ref":reference("button")})
            )
            .unwrap_err()
            .code,
            "stale_reference"
        );
        let controller = doc.controller();
        drop(doc);
        assert_eq!(
            controller
                .request(json!({"method":"controls"}), Duration::from_secs(1))
                .unwrap_err()
                .code,
            "document_closed"
        );
    }

    #[test]
    fn document_drop_releases_stalled_fetch_sse_and_websocket_and_notification_worker() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let accepted = accepted_tx.clone();
                workers.push(std::thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut first_line = String::new();
                    reader.read_line(&mut first_line).unwrap();
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" { break; }
                    }
                    if first_line.contains("/events") {
                        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
                        stream.flush().unwrap();
                    }
                    accepted.send(first_line).unwrap();
                    let mut byte = [0];
                    match reader.read(&mut byte) {
                        Ok(0) => {},
                        Err(error) if matches!(error.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted) => {},
                        result => panic!("document drop must close the idle connection: {result:?}"),
                    }
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let script = format!(
            r#"
            fetch('http://{address}/fetch').catch(() => {{}});
            globalThis.idleEvents = new EventSource('http://{address}/events');
            globalThis.connectingSocket = new WebSocket('ws://{address}/socket');
        "#
        );
        let (doc, notify) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body></body></html>",
            &script,
        )
        .unwrap();
        for _ in 0..3 {
            accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        drop(doc);
        server.join().unwrap();
        for _ in 0..100 {
            if notify.send(Ok(json!({"count":999}))).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("closed document retained its external notification worker");
    }

    #[test]
    fn timers_use_ui_callbacks_microtasks_arguments_cancellation_and_bounded_capacity() {
        let script = r#"
            globalThis.timerSteps = ['sync'];
            queueMicrotask(() => timerSteps.push('microtask'));
            Promise.resolve().then(() => timerSteps.push('promise'));
            setTimeout(function(a, b) {
                timerSteps.push(`${a}:${b}:${this === globalThis}`);
                queueMicrotask(() => timerSteps.push('timer-microtask'));
                setTimeout(() => {
                    timerSteps.push('nested');
                    document.getElementById('status').textContent = 'done';
                });
            }, 0, 'arg', 42);
            const cancelled = setTimeout(() => timerSteps.push('cancelled'), 20);
            clearInterval(cancelled);
            globalThis.intervalCount = 0;
            const interval = setInterval(() => {
                intervalCount++;
                if (intervalCount === 1) throw new Error('timer fixture failure');
                if (intervalCount === 3) clearTimeout(interval);
            }, 1);
        "#;
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body><p id='status'>initial</p></body></html>",
            script,
        )
        .unwrap();
        let startup: String = doc
            .js_context
            .with(|ctx| ctx.eval("JSON.stringify(timerSteps)"))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&startup).unwrap(),
            json!(["sync", "microtask", "promise"])
        );
        for _ in 0..100 {
            doc.poll(None);
            let count: i32 = doc
                .js_context
                .with(|ctx| ctx.eval("intervalCount"))
                .unwrap();
            if text(&doc, "status") == "done" && count == 3 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(text(&doc, "status"), "done");
        let steps: String = doc
            .js_context
            .with(|ctx| ctx.eval("JSON.stringify(timerSteps)"))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&steps).unwrap(),
            json!([
                "sync",
                "microtask",
                "promise",
                "arg:42:true",
                "timer-microtask",
                "nested"
            ])
        );
        assert_eq!(doc.script_diagnostics.borrow().len(), 1);
        assert!(doc.script_diagnostics.borrow()[0]
            .message
            .contains("timer fixture failure"));
        assert!(doc.script_diagnostics.borrow()[0]
            .source
            .starts_with("timer:"));
        std::thread::sleep(Duration::from_millis(30));
        doc.poll(None);
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<i32, _>("intervalCount"))
                .unwrap(),
            3
        );
        let capacity: bool = doc.js_context.with(|ctx| ctx.eval(r#"
            (() => {
                let ids = [];
                for (let i = 0; i < 1024; i++) ids.push(setTimeout(() => {}, 30000));
                let limited = false;
                try { setTimeout(() => {}, 30000); } catch (error) { limited = error instanceof RangeError; }
                for (const id of ids) clearTimeout(id);
                const resumed = setTimeout(() => {}, 30000);
                clearTimeout(resumed);
                let stringsRejected = false;
                try { setTimeout('throw 1', 0); } catch (error) { stringsRejected = error instanceof TypeError; }
                return limited && stringsRejected;
            })()
        "#)).unwrap();
        assert!(capacity);
    }

    #[test]
    fn microtask_budget_yields_and_resumes_without_stranding_jobs_or_polling_when_idle() {
        let script = r#"
            globalThis.jobCount = 0;
            function next() {
                if (++jobCount < 3000) queueMicrotask(next);
                else document.getElementById('status').textContent = 'done';
            }
            queueMicrotask(next);
        "#;
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body><p id='status'>waiting</p></body></html>",
            script,
        )
        .unwrap();
        assert_ne!(text(&doc, "status"), "done");
        assert!(doc.js_runtime.is_job_pending());
        for _ in 0..4 {
            doc.poll(None);
        }
        assert_eq!(text(&doc, "status"), "done");
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<i32, _>("jobCount"))
                .unwrap(),
            3000
        );
        assert!(!doc.js_runtime.is_job_pending());
        assert!(!doc.poll(None));
    }

    #[test]
    fn react_dom_bundle_mounts_updates_controlled_input_lists_effects_and_rust_action() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/react-demo");
        let html = std::fs::read_to_string(root.join("index.html")).unwrap();
        let actions = ActionRegistry::default();
        let (mut doc, _) =
            LapuiDocument::new_with_local_source(actions.clone(), None, &html, "", &root).unwrap();
        for _ in 0..300 {
            doc.poll(None);
            if text(&doc, "react-status") == "React effect completed" {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            text(&doc, "react-status"),
            "React effect completed",
            "diagnostics: {}",
            serde_json::to_string(&*doc.script_diagnostics.borrow()).unwrap()
        );
        let snapshot = control_request(&mut doc, json!({"method":"controls"})).unwrap();
        let epoch = snapshot["documentEpoch"].clone();
        let page_baseline = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"limit":64}),
        )
        .unwrap();
        let page_cursor = page_baseline["cursor"].clone();
        let reference = |id: &str| {
            snapshot["controls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|control| control["id"] == id)
                .unwrap()["ref"]
                .clone()
        };
        control_request(&mut doc, json!({"method":"fill", "documentEpoch":epoch, "ref":reference("react-input"), "value":"中文任务"})).unwrap();
        for _ in 0..100 {
            doc.poll(None);
            let enabled: bool = doc
                .js_context
                .with(|ctx| ctx.eval("!document.getElementById('react-add').disabled"))
                .unwrap();
            if enabled {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        control_request(
            &mut doc,
            json!({"method":"activate", "documentEpoch":epoch, "ref":reference("react-add")}),
        )
        .unwrap();
        let page_delta = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"cursor":page_cursor,"limit":64}),
        )
        .unwrap();
        assert!(page_delta["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| {
                record["type"] == "property"
                    && record["propertyName"] == "value"
                    && record["target"] == reference("react-input")
            }));
        for _ in 0..100 {
            doc.poll(None);
            if text(&doc, "react-list").contains("中文任务") {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(text(&doc, "react-list").contains("中文任务"));
        control_request(
            &mut doc,
            json!({"method":"activate", "documentEpoch":epoch, "ref":reference("react-remove-0")}),
        )
        .unwrap();
        for _ in 0..100 {
            doc.poll(None);
            if !text(&doc, "react-list").contains("Local React DOM") {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!text(&doc, "react-list").contains("Local React DOM"));
        control_request(&mut doc, json!({"method":"activate", "documentEpoch":epoch, "ref":reference("react-details-toggle")})).unwrap();
        for _ in 0..100 {
            doc.poll(None);
            if doc
                .dom
                .borrow()
                .get_element_by_id("react-details")
                .is_some()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(text(&doc, "react-details"), "Conditional React content");
        control_request(&mut doc, json!({"method":"activate", "documentEpoch":epoch, "ref":reference("react-details-toggle")})).unwrap();
        for _ in 0..100 {
            doc.poll(None);
            if doc
                .dom
                .borrow()
                .get_element_by_id("react-details")
                .is_none()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(doc
            .dom
            .borrow()
            .get_element_by_id("react-details")
            .is_none());
        control_request(
            &mut doc,
            json!({"method":"activate", "documentEpoch":epoch, "ref":reference("react-increment")}),
        )
        .unwrap();
        for _ in 0..200 {
            doc.poll(None);
            if text(&doc, "react-count") == "Count: 1" {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(text(&doc, "react-count"), "Count: 1");
        assert_eq!(actions.observe().count, 1);
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn event_capture_target_bubble_options_and_errors_preserve_dispatch_semantics() {
        let html =
            "<html><body><main id='outer'><button id='button'>Run</button></main></body></html>";
        let script = r#"
          globalThis.eventOrder = [];
          const outer = document.getElementById('outer');
          const button = document.getElementById('button');
          document.addEventListener('click', event => eventOrder.push(`document-capture:${event.eventPhase}`), true);
          const outerCapture = event => eventOrder.push(`outer-capture:${event.eventPhase}`);
          outer.addEventListener('click', outerCapture, {capture:true, once:true});
          outer.addEventListener('click', outerCapture, true);
          button.addEventListener('click', function(event) {
            eventOrder.push(`target-capture:${event.eventPhase}:${this === button}:${event.composedPath().at(-1) === window}`);
          }, true);
          button.addEventListener('click', event => eventOrder.push(`target-bubble:${event.eventPhase}`));
          button.addEventListener('click', {handleEvent(event) { eventOrder.push('object-once'); }}, {once:true});
          button.onclick = () => { eventOrder.push('onclick'); return false; };
          outer.addEventListener('click', event => eventOrder.push(`outer-bubble:${event.eventPhase}`));
          document.addEventListener('click', event => eventOrder.push(`document-bubble:${event.eventPhase}`));
        "#;
        let (doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, script).unwrap();
        let dispatch = "__lapui_dispatch('click', document.getElementById('button').__ref, __lapui_event_path(document.getElementById('button').__ref))";
        let outcome: String = doc.js_context.with(|ctx| ctx.eval(dispatch)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&outcome).unwrap()["defaultPrevented"],
            true
        );
        let order: String = doc
            .js_context
            .with(|ctx| ctx.eval("JSON.stringify(eventOrder)"))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&order).unwrap(),
            json!([
                "document-capture:1",
                "outer-capture:1",
                "target-capture:2:true:true",
                "target-bubble:2",
                "object-once",
                "onclick",
                "outer-bubble:3",
                "document-bubble:3"
            ])
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("eventOrder.length = 0"))
            .unwrap();
        doc.js_context
            .with(|ctx| ctx.eval::<String, _>(dispatch))
            .unwrap();
        let order: String = doc
            .js_context
            .with(|ctx| ctx.eval("JSON.stringify(eventOrder)"))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&order).unwrap(),
            json!([
                "document-capture:1",
                "target-capture:2:true:true",
                "target-bubble:2",
                "onclick",
                "outer-bubble:3",
                "document-bubble:3"
            ])
        );
        let result: String = doc.js_context.with(|ctx| ctx.eval(r#"
          (() => {
            let passivePrevented = null;
            button.addEventListener('submit', event => {
                event.preventDefault(); passivePrevented = event.defaultPrevented;
            }, {passive:true});
            const passive = JSON.parse(__lapui_dispatch('submit', button.__ref, __lapui_event_path(button.__ref)));
            const steps = [];
            const removed = () => steps.push('removed');
            button.addEventListener('keydown', () => { button.removeEventListener('keydown', removed); steps.push('remove'); });
            button.addEventListener('keydown', removed);
            button.addEventListener('keydown', event => { event.preventDefault(); throw new Error('listener fixture failure'); });
            button.addEventListener('keydown', event => { event.stopImmediatePropagation(); steps.push('immediate'); });
            button.addEventListener('keydown', () => steps.push('unreachable'));
            const key = JSON.parse(__lapui_dispatch('keydown', button.__ref, __lapui_event_path(button.__ref)));
            let onceCalls = 0;
            button.addEventListener('custom', () => {
                onceCalls++; __lapui_dispatch('custom', button.__ref, __lapui_event_path(button.__ref));
            }, {once:true});
            __lapui_dispatch('custom', button.__ref, __lapui_event_path(button.__ref));
            return JSON.stringify({passivePrevented, passive:passive.defaultPrevented, key:key.defaultPrevented, steps, onceCalls});
          })()
        "#)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&result).unwrap(),
            json!({"passivePrevented":false, "passive":false, "key":true, "steps":["remove", "immediate"], "onceCalls":1})
        );
        assert_eq!(doc.script_diagnostics.borrow().len(), 1);
        assert_eq!(doc.script_diagnostics.borrow()[0].phase, "event");
        assert!(doc.script_diagnostics.borrow()[0]
            .message
            .contains("listener fixture failure"));
    }

    #[test]
    fn quickjs_event_promise_and_external_action_update_blitz_document() {
        let actions = ActionRegistry::default();
        let (mut doc, external) = LapuiDocument::new(actions.clone(), None).unwrap();
        {
            let mut inner = doc.dom.borrow_mut();
            inner.set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
            inner.resolve(0.0);
        }
        assert_eq!(text(&doc, "status"), "状态版本 0");
        let form_result = doc.js_context
            .with(|ctx| {
                ctx.eval::<String, _>(
                    r#"
                  const dynamicButton = document.createElement('button');
                  dynamicButton.setAttribute('id', 'dynamic-control');
                  dynamicButton.textContent = 'Dynamic action';
                  document.body.appendChild(dynamicButton);
                  dynamicButton.remove();
                  document.body.appendChild(dynamicButton);
                  if (document.querySelector('#dynamic-control') !== dynamicButton) throw new Error('detached node identity changed');
                  const idless = document.createElement('span');
                  document.body.appendChild(idless);
                  globalThis.__idlessClick = 0;
                  idless.addEventListener('click', () => __idlessClick++);
                  if (document.querySelector('#dynamic-control').textContent !== 'Dynamic action') throw new Error('querySelector failed');
                  if (document.querySelectorAll('button').length !== 2) throw new Error('querySelectorAll failed');
                  if (!lapui.activate(idless.__ref) || __idlessClick !== 1) throw new Error('id-less activation failed');
                  let removedListenerCalls = 0;
                  const removedListener = () => removedListenerCalls++;
                  idless.addEventListener('custom', removedListener);
                  idless.removeEventListener('custom', removedListener);
                  __lapui_dispatch('custom', idless.__ref, __lapui_event_path(idless.__ref));
                  if (removedListenerCalls !== 0) throw new Error('removeEventListener failed');
                  const host = document.createElement('div');
                  globalThis.__bridgeTestStep = 'node-insert';
                  const firstText = document.createTextNode('A');
                  const secondText = document.createTextNode('B');
                  const comment = document.createComment('renderer anchor');
                  host.appendChild(firstText);
                  host.appendChild(secondText);
                  host.insertBefore(comment, firstText);
                  host.insertBefore(secondText, firstText);
                  if (host.textContent !== 'BA') throw new Error('insertBefore move order failed');
                  globalThis.__bridgeTestStep = 'node-types';
                  if (host.nodeType !== 1 || firstText.nodeType !== 3 || comment.nodeType !== 8) throw new Error('nodeType mismatch');
                  globalThis.__nodeValues = JSON.stringify([firstText.parentNode === host, firstText.previousSibling?.nodeName, comment.nextSibling?.nodeName, secondText.parentNode === host]);
                  if (firstText.parentNode !== host || firstText.previousSibling !== secondText || comment.nextSibling !== secondText) throw new Error('node relationship lookup failed');
                  const fragment = document.createDocumentFragment();
                  const fragmentChild = document.createElement('section');
                  fragmentChild.setAttribute('data-clone', 'yes');
                  fragmentChild.style.color = 'red';
                  fragmentChild.appendChild(document.createTextNode('fragment content'));
                  fragment.appendChild(fragmentChild);
                  if (!(fragment instanceof DocumentFragment) || fragment instanceof Element || fragment.nodeType !== 11 || fragment.firstChild !== fragmentChild || fragmentChild.parentNode !== fragment) throw new Error('DocumentFragment creation/parent failed');
                  const shallowClone = fragmentChild.cloneNode(false);
                  const deepClone = fragmentChild.cloneNode(true);
                  if (shallowClone.childNodes.length !== 0 || shallowClone.getAttribute('data-clone') !== 'yes' || shallowClone.style.color !== 'red' || deepClone.textContent !== 'fragment content') throw new Error('cloneNode depth, attributes, or style failed');
                  shallowClone.style.color = 'blue';
                  if (fragmentChild.style.color !== 'red') throw new Error('cloneNode shared mutable CSS state');
                  const fragmentClone = fragment.cloneNode(true);
                  if (fragmentClone.nodeType !== 11 || fragmentClone.firstChild.textContent !== 'fragment content') throw new Error('DocumentFragment clone failed');
                  const fragmentHost = document.createElement('div');
                  document.body.appendChild(fragmentHost);
                  if (fragmentHost.appendChild(fragment) !== fragment || fragment.childNodes.length !== 0 || fragmentChild.parentNode !== fragmentHost) throw new Error('fragment append expansion failed');
                  const secondFragment = document.createDocumentFragment();
                  const beforeAnchor = document.createElement('b');
                  secondFragment.appendChild(document.createElement('i'));
                  secondFragment.appendChild(document.createElement('u'));
                  fragmentHost.appendChild(beforeAnchor);
                  fragmentHost.insertBefore(secondFragment, beforeAnchor);
                  if (fragmentHost.childNodes.map(node => node.tagName || node.nodeName).join(',') !== 'SECTION,I,U,B' || secondFragment.childNodes.length !== 0) throw new Error('fragment insertBefore order failed');
                  const htmlTarget = document.createElement('div');
                  htmlTarget.innerHTML = '<strong id="inner-html-child" title="A &quot;quote&quot;">A &amp; B</strong><!--marker--><br>';
                  if (htmlTarget.childNodes.length !== 3 || htmlTarget.querySelector('#inner-html-child').textContent !== 'A & B') throw new Error('innerHTML fragment parsing failed');
                  if (!htmlTarget.innerHTML.includes('title="A &quot;quote&quot;"') || !htmlTarget.innerHTML.includes('A &amp; B')) throw new Error('innerHTML serialization escaping failed');
                  const replacedNode = htmlTarget.firstChild;
                  htmlTarget.innerHTML = '<em>replacement</em>';
                  if (htmlTarget.innerHTML !== '<em>replacement</em>' || htmlTarget.childNodes.length !== 1 || replacedNode.parentNode !== null) throw new Error('innerHTML replacement failed');
                  const convenienceHost = document.createElement('div');
                  const convenienceChild = document.createElement('span');
                  convenienceHost.append('middle', convenienceChild, '!');
                  convenienceHost.prepend('start ');
                  if (convenienceHost.textContent !== 'start middle!' || !convenienceHost.contains(convenienceChild)) throw new Error('append/prepend/contains failed');
                  convenienceHost.replaceChildren('replaced', document.createTextNode(' content'));
                  if (convenienceHost.textContent !== 'replaced content' || convenienceHost.contains(convenienceChild)) throw new Error('replaceChildren failed');
                  if (comment.nodeValue !== 'renderer anchor') throw new Error('comment nodeValue read failed');
                  globalThis.__bridgeTestStep = 'comment-node-value';
                  comment.nodeValue = 'updated anchor';
                  if (comment.textContent !== 'updated anchor') throw new Error('comment nodeValue write failed');
                  globalThis.__bridgeTestStep = 'remove-child';
                  if (host.removeChild(secondText) !== secondText || host.textContent !== 'A') throw new Error('removeChild failed');
                  const replaceHost = document.createElement('div');
                  const survivingChild = document.createElement('span');
                  survivingChild.id = 'surviving-child';
                  replaceHost.appendChild(survivingChild);
                  replaceHost.textContent = 'replacement text';
                  if (replaceHost.textContent !== 'replacement text') throw new Error('textContent replacement failed');
                  document.body.appendChild(survivingChild);
                  if (document.querySelector('#surviving-child') !== survivingChild) throw new Error('textContent dropped detached child');
                  const disabled = document.createElement('button');
                  disabled.setAttribute('disabled', '');
                  if (lapui.activate(disabled.__ref)) throw new Error('disabled control activated');
                  disabled.removeAttribute('disabled');
                  if (disabled.hasAttribute('disabled') || !lapui.activate(disabled.__ref)) throw new Error('removeAttribute did not update disabled state');
                  const dynamicInput = document.createElement('input');
                  dynamicInput.setAttribute('type', 'text');
                  dynamicInput.setAttribute('id', 'dynamic-input');
                  document.body.appendChild(dynamicInput);
                  if (!lapui.fill('dynamic-input', 'Grace') || dynamicInput.value !== 'Grace') throw new Error('dynamic input fill failed');
                  const classTarget = document.createElement('div');
                  document.body.appendChild(classTarget);
                  if (classTarget.getAttribute('missing') !== null || classTarget.hasAttribute('class')) throw new Error('attribute presence failed');
                  classTarget.classList.add('card', 'active');
                  if (!classTarget.classList.contains('active') || classTarget.classList.length !== 2) throw new Error('classList add failed');
                  classTarget.classList.toggle('active', false);
                  if (classTarget.classList.replace('card', 'panel') !== true || classTarget.className !== 'panel') throw new Error('classList mutation failed');
                  if (document.querySelector('.panel') !== classTarget) throw new Error('class selector did not observe classList mutation');
                  classTarget.removeAttribute('class');
                  if (classTarget.hasAttribute('class') || classTarget.getAttribute('class') !== null || document.querySelector('.panel') !== null) throw new Error('removeAttribute did not update class selector');
                  let invalidToken = false;
                  try { classTarget.classList.add('two words'); } catch (error) { invalidToken = error.name === 'InvalidCharacterError'; }
                  if (!invalidToken) throw new Error('classList token validation failed');
                  globalThis.__bridgeTestStep = 'style-direct-set';
                  classTarget.style.backgroundColor = 'red';
                  globalThis.__bridgeTestStep = 'style-set-property';
                  classTarget.style.setProperty('--accent', 'blue');
                  globalThis.__bridgeTestStep = 'style-get-property';
                  globalThis.__styleValues = JSON.stringify([classTarget.style.getPropertyValue('background-color'), classTarget.style['--accent'], classTarget.style.cssText]);
                  if (classTarget.style.getPropertyValue('background-color') !== 'red' || classTarget.style['--accent'] !== 'blue') throw new Error('style declaration read failed');
                  if (!classTarget.getAttribute('style').includes('background-color: red')) throw new Error('style attribute did not reflect CSSOM changes');
                  globalThis.__bridgeTestStep = 'style-important';
                  classTarget.style.setProperty('font-weight', 'bold', 'important');
                  globalThis.__styleAfterImportant = JSON.stringify([classTarget.style.fontWeight, classTarget.style.cssText]);
                  if (classTarget.style.fontWeight !== 'bold' || !classTarget.style.cssText.includes('!important')) throw new Error('important style priority failed');
                  globalThis.__bridgeTestStep = 'style-remove-property';
                  if (classTarget.style.removeProperty('--accent') !== 'blue') throw new Error('style property removal failed');
                  globalThis.__bridgeTestStep = 'style-delete';
                  classTarget.style.width = '12px';
                  delete classTarget.style.width;
                  globalThis.__bridgeTestStep = 'style-verify-delete';
                  if (classTarget.style.width !== '') throw new Error('style property delete failed');
                  const input = document.getElementById('name-input');
                  globalThis.__formEvents = [];
                  input.addEventListener('input', () => __formEvents.push('input'));
                  input.addEventListener('change', () => __formEvents.push('change'));
                  const before = input.value;
                  const filled = lapui.fill('name-input', 'Ada');
                  JSON.stringify({ before, filled, after: input.value, events: __formEvents });
                "#,
                )
            });
        let form_result = form_result.unwrap_or_else(|error| {
            let step = doc
                .js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify({step: globalThis.__bridgeTestStep || 'unknown', nodes: globalThis.__nodeValues || null, values: globalThis.__styleValues || null, important: globalThis.__styleAfterImportant || null})"))
                .unwrap_or_else(|_| "unknown".into());
            panic!("bridge test failed at {step}: {error}");
        });
        let form_result: Value = serde_json::from_str(&form_result).unwrap();
        assert_eq!(form_result["before"], "Lapui");
        assert_eq!(form_result["filled"], true);
        assert_eq!(form_result["after"], "Ada");
        assert_eq!(form_result["events"], json!(["input", "change"]));
        let controls = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("JSON.stringify(lapui.controls())"))
            .unwrap();
        let controls: Value = serde_json::from_str(&controls).unwrap();
        let button = controls["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|control| control["id"] == "increment")
            .unwrap();
        assert_eq!(button["role"], "button");
        assert_eq!(button["name"], "增加计数");
        let dynamic = controls["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|control| control["id"] == "dynamic-control")
            .unwrap();
        assert_eq!(dynamic["name"], "Dynamic action");
        let filled_input = controls["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|control| control["id"] == "name-input")
            .unwrap();
        assert_eq!(filled_input["value"], "Ada");

        doc.js_context.with(|ctx| ctx.eval::<(), _>(r#"
          globalThis.__eventTrace = [];
          document.getElementById('increment').addEventListener('custom', e => __eventTrace.push(`target:${e.target.id}`));
          document.getElementById('surface').addEventListener('custom', e => __eventTrace.push(`bubble:${e.currentTarget.id}`));
          __lapui_dispatch('custom', __lapui_resolve('increment'), `${__lapui_resolve('increment')}\n${__lapui_resolve('surface')}`);
          if (__eventTrace.join(',') !== 'target:increment,bubble:surface') throw new Error(__eventTrace.join(','));
          lapui.batch(() => {
            document.getElementById('count').textContent = '0';
            document.getElementById('status').textContent = '状态版本 0';
          });
        "#)).unwrap();
        let original_text_node = {
            let inner = doc.inner();
            let count = inner.get_element_by_id("count").unwrap();
            inner.get_node(count).unwrap().children[0]
        };

        doc.js_context
            .with(|ctx| ctx.eval::<bool, _>("lapui.activate('increment')"))
            .unwrap();
        for _ in 0..100 {
            doc.poll(None);
            if text(&doc, "count") == "1" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(text(&doc, "count"), "1");
        let updated_text_node = {
            let inner = doc.inner();
            let count = inner.get_element_by_id("count").unwrap();
            inner.get_node(count).unwrap().children[0]
        };
        assert_eq!(updated_text_node, original_text_node);
        assert_eq!(actions.observe().version, 1);

        let observation = actions.invoke("counter.increment", &json!({})).unwrap();
        external.send(Ok(json!(observation))).unwrap();
        for _ in 0..100 {
            doc.poll(None);
            if text(&doc, "count") == "2" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(text(&doc, "count"), "2");
        assert_eq!(text(&doc, "status"), "状态版本 2");
    }

    #[test]
    fn single_select_properties_formdata_validation_ai_and_click_share_selection() {
        let (doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body><form id='form'><label for='choice'>Choice</label><select id='choice' name='choice' required><option value='a'>Alpha</option><option value='b' selected>Beta</option></select></form></body></html>",
            r#"
              const form=document.getElementById('form'),select=document.getElementById('choice');
              if(select.value!=='b'||select.selectedIndex!==1||select.options.length!==2||!select.options[1].defaultSelected)throw Error('parsed selection');
              try{select.value='missing';throw Error('unmatched value accepted');}catch(error){if(error.message==='unmatched value accepted'||error.name!=='NotSupportedError')throw error;}
              try{select.selectedIndex=-1;throw Error('unmatched index accepted');}catch(error){if(error.message==='unmatched index accepted'||error.name!=='NotSupportedError')throw error;}
              if(select.value!=='b'||select.selectedIndex!==1)throw Error('rejected selection partially changed state');
              let changes=0;select.addEventListener('change',()=>changes++);
              select.selectedIndex=0;if(select.value!=='a'||!select.options[0].selected||select.options[1].selected)throw Error('selectedIndex write');
              if(!form.checkValidity()||new FormData(form).get('choice')!=='a')throw Error('selected option form value');
              form.reset();if(select.value!=='b'||select.selectedIndex!==1)throw Error('select reset to default');
              if(!lapui.fill(select.__ref,'a')||select.value!=='a'||new FormData(form).get('choice')!=='a')throw Error('AI select');
              __lapui_dispatch('keydown',select.__ref,__lapui_event_path(select.__ref),'{}',{key:'ArrowDown'});
              if(select.value!=='b'||changes!==2)throw Error('keyboard option selection');
              if(!lapui.activate(select.options[1].__ref)||select.value!=='b'||changes!==2)throw Error('option click selection');
            "#,
        )
        .unwrap();
        assert!(doc.script_diagnostics.borrow().is_empty());
        let snapshot = control_snapshot(&doc.dom.borrow());
        let choice = snapshot["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|control| control["id"] == "choice")
            .unwrap();
        assert_eq!(choice["value"], "b");
        assert_eq!(choice["role"], "combobox");
    }

    #[test]
    fn form_demo_listbox_projects_selection_and_hides_its_form_backing_select() {
        let html = include_str!("../examples/forms-demo/index.html");
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("globalThis.contactChangeCount=0;document.getElementById('first-select').addEventListener('change',()=>contactChangeCount++);if(document.getElementById('first-contact-email').getAttribute('tabindex')!=='0'||document.getElementById('first-contact-signal').getAttribute('tabindex')!=='-1')throw Error('listbox must expose one tab stop at the selected option');")
            })
            .unwrap();
        let snapshot = control_request(&mut doc, json!({"method":"controls"})).unwrap();
        let controls = snapshot["controls"].as_array().unwrap();
        assert!(!controls
            .iter()
            .any(|control| control["id"] == "first-select"));
        assert_eq!(
            controls
                .iter()
                .find(|control| control["id"] == "first-contact-email")
                .unwrap()["selected"],
            true
        );
        let target = controls
            .iter()
            .find(|control| control["id"] == "first-contact-signal")
            .unwrap();
        assert_eq!(target["role"], "option");
        let request = json!({"method":"activate","documentEpoch":snapshot["documentEpoch"],"ref":target["ref"]});
        let response = control_request(&mut doc, request).unwrap();
        assert!(response["dispatchErrors"].as_array().unwrap().is_empty());
        let controls = response["controls"].as_array().unwrap();
        assert_eq!(
            controls
                .iter()
                .find(|control| control["id"] == "contact-state")
                .unwrap()["name"],
            "Contact: signal"
        );
        assert_eq!(
            controls
                .iter()
                .find(|control| control["id"] == "first-contact-signal")
                .unwrap()["selected"],
            true
        );
        assert_eq!(
            controls
                .iter()
                .find(|control| control["id"] == "first-contact-email")
                .unwrap()["selected"],
            false
        );
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("if(contactChangeCount!==1)throw Error('new listbox selection should dispatch one change');")
            })
            .unwrap();
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("if(document.getElementById('first-contact-signal').getAttribute('tabindex')!=='0'||document.getElementById('first-contact-email').getAttribute('tabindex')!=='-1')throw Error('listbox tab stop did not follow selection');")
            })
            .unwrap();
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("const signal=document.getElementById('first-contact-signal');__lapui_dispatch('keydown',signal.__ref,__lapui_event_path(signal.__ref),'{}',{key:'ArrowLeft'});")
            })
            .unwrap();
        let controls = control_snapshot(&doc.dom.borrow());
        let controls = controls["controls"].as_array().unwrap();
        assert_eq!(
            controls
                .iter()
                .find(|control| control["id"] == "contact-state")
                .unwrap()["name"],
            "Contact: email"
        );
        assert_eq!(
            controls
                .iter()
                .find(|control| control["id"] == "first-contact-email")
                .unwrap()["selected"],
            true
        );
        assert_eq!(
            controls
                .iter()
                .find(|control| control["id"] == "first-contact-signal")
                .unwrap()["selected"],
            false
        );
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("if(document.getElementById('first-contact-email').getAttribute('tabindex')!=='0'||document.getElementById('first-contact-signal').getAttribute('tabindex')!=='-1')throw Error('keyboard selection did not update the listbox tab stop');__lapui_dispatch('keydown',document.getElementById('first-contact-email').__ref,__lapui_event_path(document.getElementById('first-contact-email').__ref),'{}',{key:'End'});if(document.getElementById('first-contact-signal').getAttribute('tabindex')!=='0'||contactChangeCount!==3)throw Error('End did not move and select the listbox option');lapui.activate(document.getElementById('first-contact-signal').__ref);if(contactChangeCount!==3)throw Error('re-activating the selected listbox option emitted change');")
            })
            .unwrap();
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn form_submit_demo_listbox_uses_a_single_keyboard_tab_stop() {
        let html = include_str!("../examples/form-submit-demo/index.html");
        let (doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        let state: String = doc
            .js_context
            .with(|ctx| {
                ctx.eval("JSON.stringify(['contact-email','contact-signal','contact-none'].map(id=>document.getElementById(id).getAttribute('tabindex')))")
            })
            .unwrap();
        assert_eq!(state, "[\"-1\",\"0\",\"-1\"]");
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("const signal=document.getElementById('contact-signal');__lapui_dispatch('keydown',signal.__ref,__lapui_event_path(signal.__ref),'{}',{key:'Home'});")
            })
            .unwrap();
        let state: String = doc
            .js_context
            .with(|ctx| {
                ctx.eval("JSON.stringify([document.getElementById('contact').value,...['contact-email','contact-signal','contact-none'].map(id=>document.getElementById(id).getAttribute('tabindex'))])")
            })
            .unwrap();
        assert_eq!(state, "[\"email\",\"0\",\"-1\",\"-1\"]");
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn ai_page_snapshot_is_hierarchical_paged_bounded_and_redacts_passwords() {
        let html = r#"<!doctype html><html><body><main id="app"><h1>本地工具</h1>
            <button id="submit"><span>提交任务</span></button>
            <div aria-hidden="true"><p>隐藏的秘密</p></div>
            <p style="display:none">CSS 隐藏内容</p>
            <p style="visibility:hidden">不可见内容</p>
            <input id="secret" type="password" value="do-not-leak">
            <input id="otp" aria-label="验证码" autocomplete="one-time-code" value="otp-do-not-leak">
            <input id="card" aria-label="银行卡号" autocomplete="cc-number" value="card-do-not-leak">
            <input autocomplete="section-login current-password" value="current-password-do-not-leak">
            <input autocomplete="new-password" value="new-password-do-not-leak">
            <input autocomplete="cc-csc" value="card-csc-do-not-leak">
            <input autocomplete="cc-exp" value="card-exp-do-not-leak">
            <input autocomplete="cc-exp-month" value="card-exp-month-do-not-leak">
            <input autocomplete="cc-exp-year" value="card-exp-year-do-not-leak">
            <input id="query" aria-label="搜索" value="可见值">
            </main></body></html>"#;
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        doc.dom.borrow_mut().resolve(0.0);
        let first = control_request(&mut doc, json!({"method":"pageSnapshot","limit":5})).unwrap();
        assert_eq!(first["documentEpoch"], doc.dom.borrow().id());
        assert_eq!(first["items"].as_array().unwrap().len(), 5);
        let items = first["items"].as_array().unwrap();
        let heading = items.iter().find(|item| item["id"] == "app").unwrap();
        assert_eq!(heading["tag"], "main");
        assert_eq!(heading["role"], "main");
        let button = items.iter().find(|item| item["id"] == "submit").unwrap();
        assert_eq!(button["role"], "button");
        assert_eq!(button["name"], "提交任务");
        assert!(button["bounds"]["width"].is_number());
        assert!(!first.to_string().contains("隐藏的秘密"));
        assert!(!first.to_string().contains("CSS 隐藏内容"));
        assert!(!first.to_string().contains("不可见内容"));
        assert!(!first.to_string().contains("do-not-leak"));
        assert_eq!(first["nextAfter"], button["ref"]);

        let next = control_request(
            &mut doc,
            json!({"method":"pageSnapshot","documentEpoch":first["documentEpoch"],"afterRef":first["nextAfter"],"limit":16}),
        ).unwrap();
        assert!(next["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "query"));
        assert!(!next.to_string().contains("do-not-leak"));
        let full = control_request(
            &mut doc,
            json!({"method":"pageSnapshot","documentEpoch":first["documentEpoch"],"limit":64}),
        )
        .unwrap();
        for sentinel in [
            "otp-do-not-leak",
            "card-do-not-leak",
            "current-password-do-not-leak",
            "new-password-do-not-leak",
            "card-csc-do-not-leak",
            "card-exp-do-not-leak",
            "card-exp-month-do-not-leak",
            "card-exp-year-do-not-leak",
        ] {
            assert!(
                !full.to_string().contains(sentinel),
                "pageSnapshot leaked {sentinel}"
            );
        }
        assert!(full.to_string().contains("可见值"));
        let controls = control_request(&mut doc, json!({"method":"controls"})).unwrap();
        for sentinel in [
            "otp-do-not-leak",
            "card-do-not-leak",
            "current-password-do-not-leak",
            "new-password-do-not-leak",
            "card-csc-do-not-leak",
            "card-exp-do-not-leak",
            "card-exp-month-do-not-leak",
            "card-exp-year-do-not-leak",
        ] {
            assert!(
                !controls.to_string().contains(sentinel),
                "controls leaked {sentinel}"
            );
        }
        assert!(controls.to_string().contains("可见值"));
        let stale = control_request(
            &mut doc,
            json!({"method":"pageSnapshot","documentEpoch":999999,"limit":1}),
        )
        .unwrap_err();
        assert_eq!(stale.code, "stale_document");
        let stale_cursor = control_request(
            &mut doc,
            json!({"method":"pageSnapshot","afterRef":"node:stale:999","limit":1}),
        )
        .unwrap_err();
        assert_eq!(stale_cursor.code, "stale_cursor");
    }

    #[test]
    fn page_change_journal_is_value_free_cursored_bounded_and_epoch_scoped() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body><main id='app'><p id='status'>ready</p><input id='field'><input id='check' type='checkbox'></main></body></html>",
            "",
        )
        .unwrap();
        let epoch = doc.inner().id() as u64;
        let baseline = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"limit":8}),
        )
        .unwrap();
        assert_eq!(baseline["latestSequence"], 0);
        let baseline_cursor = baseline["cursor"].as_str().unwrap().to_owned();
        control_request(
            &mut doc,
            json!({"method":"debugTrace.configure","documentEpoch":epoch,"enabled":true}),
        )
        .unwrap();

        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "const status=document.getElementById('status'); status.setAttribute('data-token','never-export-this-value'); status.textContent='updated private text'; const button=document.createElement('button'); button.id='new-action'; button.textContent='Run'; document.getElementById('app').appendChild(button);",
                )
            })
            .unwrap();
        drain_jobs(&doc.js_runtime, &doc.script_budget).unwrap();
        let first = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"cursor":baseline_cursor,"limit":1}),
        )
        .unwrap();
        assert_eq!(first["records"].as_array().unwrap().len(), 1);
        assert_eq!(first["hasMore"], true);
        assert!(first["records"][0]["target"]
            .as_str()
            .unwrap()
            .starts_with(&format!("node:{epoch}:")));
        assert!(!first.to_string().contains("never-export-this-value"));
        assert!(!first.to_string().contains("updated private text"));

        let mut cursor = first["cursor"].as_str().unwrap().to_owned();
        let mut records = first["records"].as_array().unwrap().clone();
        while first["hasMore"] == true && records.len() < 8 {
            let next = control_request(
                &mut doc,
                json!({"method":"pageChanges","documentEpoch":epoch,"cursor":cursor,"limit":8}),
            )
            .unwrap();
            cursor = next["cursor"].as_str().unwrap().to_owned();
            records.extend(next["records"].as_array().unwrap().iter().cloned());
            if next["hasMore"] != true {
                break;
            }
        }
        assert!(records.iter().any(|record| record["type"] == "attributes"));
        assert!(records
            .iter()
            .any(|record| record["type"] == "characterData"));
        assert!(records.iter().any(|record| record["type"] == "childList"));
        assert!(records
            .iter()
            .any(|record| { record["debugTraceSequence"].as_u64().is_some() }));
        assert!(records.iter().all(|record| record["target"]
            .as_str()
            .is_some_and(|reference| reference.starts_with(&format!("node:{epoch}:")))));
        assert!(records.iter().any(|record| {
            record["type"] == "childList"
                && record["addedCount"].as_u64().unwrap_or_default() > 0
                && record["added"].as_array().is_some_and(|nodes| {
                    nodes.iter().any(|node| {
                        node.as_str().is_some_and(|reference| {
                            reference.starts_with(&format!("node:{epoch}:"))
                        })
                    })
                })
        }));
        assert!(records
            .iter()
            .all(|record| record.get("attributeValue").is_none()));
        let records_json = Value::Array(records.clone()).to_string();
        assert!(!records_json.contains("never-export-this-value"));
        assert!(!records_json.contains("updated private text"));

        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "const field=document.getElementById('field'),check=document.getElementById('check'); field.value='private form value'; field.value='private form value'; check.checked=true; check.checked=true;",
                )
            })
            .unwrap();
        let properties = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"cursor":cursor,"limit":8}),
        )
        .unwrap();
        assert_eq!(
            properties["records"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|record| {
                    record["type"] == "property" && record["propertyName"] == "value"
                })
                .count(),
            1
        );
        assert_eq!(
            properties["records"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|record| {
                    record["type"] == "property" && record["propertyName"] == "checked"
                })
                .count(),
            1
        );
        assert!(!properties.to_string().contains("private form value"));
        cursor = properties["cursor"].as_str().unwrap().to_owned();

        let controls = control_request(&mut doc, json!({"method":"controls"})).unwrap();
        let control_ref = controls["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|control| control["id"] == "field")
            .unwrap()["ref"]
            .clone();
        control_request(
            &mut doc,
            json!({"method":"fill","documentEpoch":epoch,"ref":control_ref,"value":"changed"}),
        )
        .unwrap();
        let control_events = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"cursor":cursor,"limit":8}),
        )
        .unwrap();
        assert!(control_events["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| { record["type"] == "control" && record["controlEvent"] == "input" }));

        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "const bulk=document.createElement('div'); bulk.id='bulk'; bulk.innerHTML='<i>node</i>'.repeat(300); document.getElementById('app').appendChild(bulk);",
                )
            })
            .unwrap();
        drain_jobs(&doc.js_runtime, &doc.script_budget).unwrap();
        let bulk_changes = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"cursor":control_events["cursor"],"limit":64}),
        )
        .unwrap();
        let bulk_record = bulk_changes["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["nodesTruncated"] == true)
            .unwrap();
        assert_eq!(bulk_record["addedCount"], 300);
        assert_eq!(bulk_record["added"].as_array().unwrap().len(), 32);
        assert!(bulk_changes.to_string().len() <= 24 * 1024);

        let stale = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch+1}),
        )
        .unwrap_err();
        assert_eq!(stale.code, "stale_document");
        let foreign_cursor = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"cursor":"page:0:1"}),
        )
        .unwrap_err();
        assert_eq!(foreign_cursor.code, "stale_document");

        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "const target=document.getElementById('bulk'); for(let i=0;i<270;i++) target.setAttribute('data-change-'+i,String(i));",
                )
            })
            .unwrap();
        drain_jobs(&doc.js_runtime, &doc.script_budget).unwrap();
        let resync = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"cursor":baseline["cursor"],"limit":64}),
        )
        .unwrap();
        assert_eq!(resync["resyncRequired"], true);
        assert_eq!(resync["records"].as_array().unwrap().len(), 0);
        let recovered = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"cursor":resync["cursor"],"limit":64}),
        )
        .unwrap();
        assert_eq!(recovered["resyncRequired"], false);
        assert!(recovered["records"].as_array().unwrap().is_empty());
    }

    #[cfg(feature = "software-renderer")]
    #[test]
    fn screenshot_command_returns_a_bounded_png_without_changing_viewport() {
        use base64::Engine;

        let html = r#"<!doctype html><html><body><main style="background:#f00;width:100%;height:100%"><h1>Screenshot</h1></main></body></html>"#;
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(320, 200, 1.5, ColorScheme::Light));
        doc.inner_mut().resolve(0.0);
        let before = doc.inner().viewport().clone();
        let screenshot = control_request(&mut doc, json!({"method":"screenshot"})).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(screenshot["pngBase64"].as_str().unwrap())
            .unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(screenshot["width"], 320);
        assert_eq!(screenshot["height"], 200);
        assert_eq!(screenshot["boundary"], "cpu_rendered");
        assert_eq!(screenshot["physicalPresentation"], "not_confirmed");
        assert_eq!(screenshot["documentEpoch"], doc.dom.borrow().id());
        assert_eq!(doc.inner().viewport(), &before);
    }

    #[test]
    fn structured_scroll_checks_epoch_reference_and_coordinate_budget() {
        let html = r#"<html><body><div id="scroller" style="width:120px;height:40px;overflow:auto">
            <div style="width:1000px;height:400px">content</div></div></body></html>"#;
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.inner_mut()
            .set_viewport(Viewport::new(400, 300, 1.0, ColorScheme::Light));
        doc.dom.borrow_mut().resolve(0.0);
        let node = doc.dom.borrow().get_element_by_id("scroller").unwrap();
        let reference = canonical_node_ref(doc.dom.borrow().id(), node);
        let epoch = doc.dom.borrow().id();
        let response = control_request(
            &mut doc,
            json!({"method":"scroll","documentEpoch":epoch,"ref":reference,"y":30}),
        )
        .unwrap();
        assert!(response["scrolled"].as_bool().unwrap());
        assert!(response["metrics"][1].as_f64().unwrap() > 0.0);
        let stale = control_request(
            &mut doc,
            json!({"method":"scroll","documentEpoch":epoch+1,"ref":reference,"y":30}),
        )
        .unwrap_err();
        assert_eq!(stale.code, "stale_document");
        let excessive = control_request(
            &mut doc,
            json!({"method":"scroll","documentEpoch":epoch,"ref":reference,"x":100001}),
        )
        .unwrap_err();
        assert_eq!(excessive.code, "invalid_request");
    }

    #[test]
    fn form_radio_ownership_and_property_updates_preserve_attributes_and_other_controls() {
        let html = r#"<html><body>
          <form id="one"><input id="a" type="radio" name="choice" checked><input id="b" type="radio" name="choice"><input id="checkbox" type="checkbox" name="choice" checked></form>
          <form id="two"><input id="c" type="radio" name="choice" checked><input id="d" type="radio" name="choice"></form>
          <input id="external" type="radio" form="one" name="choice"><input id="outside" type="radio" name="choice" checked>
          <input id="unnamed-one" type="radio"><input id="unnamed-two" type="radio">
        </body></html>"#;
        let (doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>(r#"
            const a=document.getElementById('a'), b=document.getElementById('b'), c=document.getElementById('c'), d=document.getElementById('d'), external=document.getElementById('external'), outside=document.getElementById('outside'), checkbox=document.getElementById('checkbox');
            b.checked = true;
            if (a.checked || !b.checked || !c.checked || !outside.checked || !checkbox.checked) throw new Error('implicit form group');
            if (!a.hasAttribute('checked') || b.hasAttribute('checked')) throw new Error('checked property changed default attribute');
            external.checked = true;
            if (b.checked || !external.checked || !c.checked || !outside.checked) throw new Error('explicit form group');
            external.setAttribute('form','two'); external.checked = true;
            if (c.checked || !external.checked || !outside.checked || !checkbox.checked) throw new Error('updated form group');
            external.setAttribute('form','missing'); external.checked = true;
            if (outside.checked || !external.checked || !checkbox.checked) throw new Error('invalid form owner');
            document.getElementById('unnamed-one').checked = true; document.getElementById('unnamed-two').checked = true;
            if (!document.getElementById('unnamed-one').checked || !document.getElementById('unnamed-two').checked) throw new Error('unnamed radios grouped');
            globalThis.detachedRadioHost = document.createElement('div');
            detachedRadioHost.innerHTML='<input type="radio" name="choice" checked><input type="radio" name="choice">';
            detachedRadioHost.children[1].checked = true;
            !detachedRadioHost.children[0].checked && detachedRadioHost.children[1].checked && external.checked;
        "#)).unwrap());
        doc.dom.borrow_mut().resolve(0.0);
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("!document.getElementById('a').checked && document.getElementById('external').checked && document.getElementById('checkbox').checked && document.querySelector('#external:checked') !== null")).unwrap());
    }

    #[test]
    fn form_click_defaults_are_shared_by_ai_javascript_native_events_and_labels() {
        let html = r#"<html><body><input id="check" type="checkbox"><label id="label" for="check"><span id="label-text">Toggle</span></label><input id="r1" type="radio" name="choice" checked><input id="r2" type="radio" name="choice"></body></html>"#;
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None, html, r#"
          globalThis.checkEvents=[];
          const observedCheck=document.getElementById('check');
          for(const type of ['click','input','change']) observedCheck.addEventListener(type,event=>checkEvents.push(type+':'+observedCheck.checked+':'+event.cancelable));
        "#).unwrap();
        doc.dom.borrow_mut().resolve(0.0);
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>(
                "lapui.activate('check') && document.getElementById('check').checked"
            ))
            .unwrap());
        let target = doc.dom.borrow().get_element_by_id("check").unwrap();
        let event = doc
            .dom
            .borrow()
            .get_node(target)
            .unwrap()
            .synthetic_click_event(keyboard_types::Modifiers::empty());
        let handler = JsHandler {
            runtime: doc.js_runtime.clone(),
            context: doc.js_context.clone(),
            budget: doc.script_budget.clone(),
            diagnostics: doc.script_diagnostics.clone(),
        };
        EventDriver::new(&mut doc, handler).handle_dom_event(DomEvent::new(target, event));
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("!document.getElementById('check').checked && JSON.stringify(checkEvents) === JSON.stringify(['click:true:true','input:true:false','change:true:false','click:false:true','input:false:false','change:false:false'])")).unwrap());
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>(r#"
            const check=document.getElementById('check');
            check.onclick=event=>event.preventDefault(); check.click();
            if(check.checked || checkEvents.length!==7 || checkEvents[6]!=='click:true:true') throw new Error('cancelled checkbox');
            check.onclick=null;
            document.getElementById('label-text').click();
            if(!check.checked || checkEvents.length!==10 || document.activeElement!==check) throw new Error('label forwarded or toggled twice');
            let reentrant=0; check.onclick=()=>{reentrant++;check.click();}; check.click();
            if(check.checked || reentrant!==1) throw new Error('click recursion');
            const r1=document.getElementById('r1'),r2=document.getElementById('r2');
            r2.onclick=event=>{if(!r2.checked||r1.checked) throw new Error('radio state before listener');event.preventDefault();};
            r2.click();
            r1.checked && !r2.checked;
        "#)).unwrap());
        for html_id in ["label-text", "r2"] {
            let target = doc.dom.borrow().get_element_by_id(html_id).unwrap();
            let event = doc
                .dom
                .borrow()
                .get_node(target)
                .unwrap()
                .synthetic_click_event(keyboard_types::Modifiers::empty());
            let handler = JsHandler {
                runtime: doc.js_runtime.clone(),
                context: doc.js_context.clone(),
                budget: doc.script_budget.clone(),
                diagnostics: doc.script_diagnostics.clone(),
            };
            EventDriver::new(&mut doc, handler).handle_dom_event(DomEvent::new(target, event));
        }
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("document.getElementById('check').checked && document.getElementById('r1').checked && !document.getElementById('r2').checked")).unwrap());
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn form_disabled_fieldsets_legend_exceptions_and_readonly_fill_match_ai_snapshot() {
        let html = r#"<html><body><fieldset id="fields" disabled>
            <legend><input id="legend"><fieldset disabled><input id="nested"></fieldset></legend>
            <legend><input id="second-legend"></legend><input id="blocked" type="checkbox"><button id="button"><span id="inside">Run</span></button>
          </fieldset><input id="readonly" readonly value="keep"></body></html>"#;
        let (doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.dom.borrow_mut().resolve(0.0);
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>(r#"
            const blocked=document.getElementById('blocked');
            if(lapui.check('blocked',true)||lapui.fill('second-legend','no')||lapui.focus('nested')||lapui.fill('readonly','no')) throw new Error('disabled or readonly accepted');
            blocked.focus(); blocked.click();
            if(blocked.checked||document.activeElement===blocked) throw new Error('disabled control focus/click');
            globalThis.buttonClicks=0; document.getElementById('button').onclick=()=>buttonClicks++;
            document.getElementById('inside').click();
            if(buttonClicks!==0) throw new Error('disabled button descendant');
            if(!lapui.fill('legend','allowed')||!lapui.focus('legend')) throw new Error('first legend should be enabled');
            const snapshot=lapui.controls().controls;
            if(snapshot.find(c=>c.id==='blocked').enabled||snapshot.find(c=>c.id==='second-legend').enabled||!snapshot.find(c=>c.id==='legend').enabled) throw new Error('AI enabled snapshot');
            document.getElementById('fields').removeAttribute('disabled');
            lapui.activate('blocked') && blocked.checked && document.getElementById('readonly').value==='keep';
        "#)).unwrap());
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn form_native_keyboard_and_ime_guards_tab_skip_and_widget_navigation_share_activation() {
        use blitz::traits::events::{BlitzImeEvent, BlitzKeyEvent, DomEventData, KeyState};
        let html = r#"<html><body><input id="start"><fieldset disabled><legend><button id="legend" type="button">Legend</button></legend><input id="blocked"></fieldset>
          <input id="readonly" readonly value="keep"><button id="action" type="button">Action</button>
          <input id="r1" type="radio" name="choice" checked><input id="r-disabled" type="radio" name="choice" disabled><input id="r2" type="radio" name="choice">
          <form><input id="other" type="radio" name="choice" checked></form><input id="check" type="checkbox"></body></html>"#;
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            html,
            r#"
            globalThis.actionClicks=0; globalThis.checkInputs=0;
            document.getElementById('action').onclick=()=>actionClicks++;
            document.getElementById('check').oninput=()=>checkInputs++;
        "#,
        )
        .unwrap();
        doc.dom
            .borrow_mut()
            .set_viewport(Viewport::new(480, 480, 1.0, ColorScheme::Light));
        doc.dom.borrow_mut().resolve(0.0);
        let key = |key: Key, code: Code, pressed: bool, repeat: bool| {
            let text = if pressed {
                if let Key::Character(value) = &key {
                    Some(value.clone().into())
                } else {
                    None
                }
            } else {
                None
            };
            let event = BlitzKeyEvent {
                key,
                code,
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                is_auto_repeating: repeat,
                is_composing: false,
                state: if pressed {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                },
                text,
            };
            if pressed {
                UiEvent::KeyDown(event)
            } else {
                UiEvent::KeyUp(event)
            }
        };
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("document.getElementById('start').focus()"))
            .unwrap();
        doc.handle_ui_event(key(Key::Tab, Code::Tab, true, false));
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("document.activeElement.id"))
                .unwrap(),
            "legend"
        );
        doc.handle_ui_event(key(Key::Tab, Code::Tab, true, false));
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("document.activeElement.id"))
                .unwrap(),
            "readonly"
        );
        let blocked = doc.dom.borrow().get_element_by_id("blocked").unwrap();
        let pointer = doc
            .dom
            .borrow()
            .get_node(blocked)
            .unwrap()
            .synthetic_click_event_data(Modifiers::empty());
        let handler = JsHandler {
            runtime: doc.js_runtime.clone(),
            context: doc.js_context.clone(),
            budget: doc.script_budget.clone(),
            diagnostics: doc.script_diagnostics.clone(),
        };
        EventDriver::new(&mut doc, handler)
            .handle_dom_event(DomEvent::new(blocked, DomEventData::PointerDown(pointer)));
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("document.activeElement.id"))
                .unwrap(),
            "readonly"
        );
        for html_id in ["readonly", "blocked"] {
            let id = doc.dom.borrow().get_element_by_id(html_id).unwrap();
            doc.dom.borrow_mut().set_focus_to(id);
            doc.handle_ui_event(key(Key::Character("a".into()), Code::KeyA, true, false));
            doc.handle_ui_event(UiEvent::Ime(BlitzImeEvent::Commit("改变".to_owned())));
        }
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("document.getElementById('readonly').value==='keep' && document.getElementById('blocked').value===''")).unwrap());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("document.getElementById('action').focus()"))
            .unwrap();
        doc.handle_ui_event(key(Key::Enter, Code::Enter, true, false));
        doc.handle_ui_event(key(Key::Enter, Code::Enter, true, true));
        doc.handle_ui_event(key(Key::Character(" ".into()), Code::Space, true, false));
        doc.handle_ui_event(key(Key::Character(" ".into()), Code::Space, true, true));
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("actionClicks"))
                .unwrap(),
            1
        );
        doc.handle_ui_event(key(Key::Character(" ".into()), Code::Space, false, false));
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("actionClicks"))
                .unwrap(),
            2
        );
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("document.getElementById('check').focus()"))
            .unwrap();
        doc.handle_ui_event(key(Key::Character(" ".into()), Code::Space, true, false));
        doc.handle_ui_event(key(Key::Character(" ".into()), Code::Space, true, true));
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("!document.getElementById('check').checked"))
            .unwrap());
        doc.handle_ui_event(key(Key::Character(" ".into()), Code::Space, false, false));
        assert!(doc
            .js_context
            .with(|ctx| ctx
                .eval::<bool, _>("document.getElementById('check').checked && checkInputs===1"))
            .unwrap());
        doc.js_context.with(|ctx| ctx.eval::<(), _>("document.getElementById('check').addEventListener('keyup',e=>e.preventDefault(),{once:true})")).unwrap();
        doc.handle_ui_event(key(Key::Character(" ".into()), Code::Space, true, false));
        doc.handle_ui_event(key(Key::Character(" ".into()), Code::Space, false, false));
        assert!(doc
            .js_context
            .with(|ctx| ctx
                .eval::<bool, _>("document.getElementById('check').checked && checkInputs===1"))
            .unwrap());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("document.getElementById('r1').focus()"))
            .unwrap();
        doc.handle_ui_event(key(Key::ArrowRight, Code::ArrowRight, true, false));
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("document.activeElement.id==='r2' && document.getElementById('r2').checked && !document.getElementById('r1').checked && document.getElementById('other').checked")).unwrap());
        doc.handle_ui_event(key(Key::ArrowLeft, Code::ArrowLeft, true, false));
        doc.js_context.with(|ctx| ctx.eval::<(), _>("document.getElementById('r1').addEventListener('keydown',e=>e.preventDefault(),{once:true})")).unwrap();
        doc.handle_ui_event(key(Key::ArrowRight, Code::ArrowRight, true, false));
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("document.activeElement.id==='r1' && document.getElementById('r1').checked && document.getElementById('other').checked")).unwrap());
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn form_semantic_names_use_implicit_multiple_labels_and_aria_references_without_password_values(
    ) {
        let html = r#"<html><body><label>Implicit name <input id="implicit"></label>
          <label for="multiple">First</label><label for="multiple">Second</label><input id="multiple" title="fallback">
          <span id="name">Referenced</span><span id="suffix">name</span><input id="referenced" aria-labelledby="name suffix" aria-label="lower priority">
          <input id="action" type="button" value="Run"><label>Password <input id="password" type="password" value="secret"></label>
        </body></html>"#;
        let (doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        let snapshot = control_snapshot(&doc.dom.borrow());
        let controls = snapshot["controls"].as_array().unwrap();
        let control = |id: &str| controls.iter().find(|control| control["id"] == id).unwrap();
        assert_eq!(control("implicit")["name"], "Implicit name");
        assert_eq!(control("multiple")["name"], "First Second");
        assert_eq!(control("referenced")["name"], "Referenced name");
        assert_eq!(control("action")["name"], "Run");
        assert_eq!(control("password")["name"], "Password");
        assert!(control("password").get("value").is_none());
        assert!(!snapshot.to_string().contains("secret"));
        doc.dom.borrow_mut().resolve(0.0);
        let laid_out = control_snapshot(&doc.dom.borrow());
        assert!(!laid_out.to_string().contains("secret"));
    }

    #[test]
    fn checked_controls_sync_properties_events_radio_groups_and_ai_snapshot() {
        let html = r#"<!doctype html><html><body>
          <label for="agree">同意条款</label><input id="agree" type="checkbox" checked>
          <label for="first">第一项</label><input id="first" type="radio" name="choice" checked>
          <label for="second">第二项</label><input id="second" type="radio" name="choice">
          <label for="locked">锁定</label><input id="locked" type="checkbox" disabled>
        </body></html>"#;
        let (doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        let initial: String = doc
            .js_context
            .with(|ctx| ctx.eval("JSON.stringify(lapui.controls())"))
            .unwrap();
        let initial: Value = serde_json::from_str(&initial).unwrap();
        let controls = initial["controls"].as_array().unwrap();
        let control = |id: &str| controls.iter().find(|control| control["id"] == id).unwrap();
        assert_eq!(control("agree")["role"], "checkbox");
        assert_eq!(control("agree")["name"], "同意条款");
        assert_eq!(control("agree")["checked"], true);
        assert_eq!(control("first")["role"], "radio");
        assert_eq!(control("second")["checked"], false);
        doc.dom.borrow_mut().resolve(0.0);

        let evaluated = doc.js_context.with(|ctx| {
                ctx.eval(
                    r#"
                      const agree = document.getElementById('agree');
                      globalThis.__checkedStep = 'listeners';
                      globalThis.__checkedEvents = [];
                      agree.addEventListener('input', () => __checkedEvents.push('input'));
                      agree.addEventListener('change', () => __checkedEvents.push('change'));
                      globalThis.__checkedStep = 'property-false';
                      agree.checked = false;
                      if (agree.checked) throw new Error('checked property did not update current value');
                      globalThis.__checkedStep = 'ai-check';
                      if (!lapui.check('agree', true) || !agree.checked) throw new Error('AI check operation failed');
                      globalThis.__checkedStep = 'disabled-check';
                      if (lapui.check('locked', true)) throw new Error('disabled checkbox was modified');
                      globalThis.__checkedStep = 'radio-check';
                      if (!lapui.check('second', true)) throw new Error('radio selection failed');
                      if (document.getElementById('first').checked || !document.getElementById('second').checked) throw new Error('radio group did not become exclusive');
                      globalThis.__checkedStep = 'wrong-type';
                      if (lapui.check('agree', 'not a checkbox')) throw new Error('non-checkable control accepted check operation');
                      globalThis.__checkedStep = 'snapshot';
                      JSON.stringify({ events: __checkedEvents, controls: lapui.controls().controls });
                    "#,
                )
            });
        let result: String = evaluated.unwrap_or_else(|error| {
            let stack = doc.js_context.with(|ctx| {
                ctx.catch()
                    .into_object()
                    .and_then(Exception::from_object)
                    .and_then(|exception| exception.stack().or_else(|| exception.message()))
            });
            let step = doc
                .js_context
                .with(|ctx| ctx.eval::<String, _>("String(globalThis.__checkedStep || '')"))
                .unwrap_or_default();
            panic!("checkbox integration step '{step}' failed: {error}; {stack:?}");
        });
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["events"], json!(["input", "change"]));
        let controls = result["controls"].as_array().unwrap();
        assert_eq!(
            controls
                .iter()
                .find(|control| control["id"] == "agree")
                .unwrap()["checked"],
            true
        );
        assert_eq!(
            controls
                .iter()
                .find(|control| control["id"] == "first")
                .unwrap()["checked"],
            false
        );
        assert_eq!(
            controls
                .iter()
                .find(|control| control["id"] == "second")
                .unwrap()["checked"],
            true
        );
    }

    #[test]
    #[cfg(feature = "complex-scripts")]
    fn cjk_paragraph_uses_complex_script_line_break_data() {
        let html = r#"<!doctype html><html><body>
          <p id="cjk" style="width:96px;margin:0;font-size:16px;line-height:20px">中文界面需要正确处理连续文字的断行，不能只按西文空格拆分。</p>
        </body></html>"#;
        let (doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        {
            let mut inner = doc.dom.borrow_mut();
            inner.set_viewport(Viewport::new(240, 160, 1.0, ColorScheme::Light));
            inner.resolve(0.0);
            let paragraph = inner.get_element_by_id("cjk").unwrap();
            let bounds = inner.get_client_bounding_rect(paragraph).unwrap();
            assert!(
                bounds.width >= 96.0,
                "unexpected CJK paragraph width: {bounds:?}"
            );
            assert!(
                bounds.height >= 40.0,
                "CJK text did not wrap into multiple lines: {bounds:?}"
            );
            assert!(
                bounds.height < 160.0,
                "CJK line layout exceeded its viewport: {bounds:?}"
            );
        }
    }

    #[test]
    fn focused_text_input_accepts_ime_composition_and_commit() {
        let html = r#"<!doctype html><html><body><input id="ime" type="text"></body></html>"#;
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        {
            let mut inner = doc.dom.borrow_mut();
            inner.set_viewport(Viewport::new(320, 120, 1.0, ColorScheme::Light));
            inner.resolve(0.0);
            let input = inner.get_element_by_id("ime").unwrap();
            assert!(inner.set_focus_to(input));
            assert!(inner
                .get_node(input)
                .and_then(|node| node.element_data())
                .and_then(|element| element.text_input_data())
                .is_some());
        }

        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "globalThis.__imeInputEvents = 0; document.getElementById('ime').addEventListener('input', () => __imeInputEvents++);",
                )
            })
            .unwrap();
        doc.handle_ui_event(UiEvent::Ime(blitz::traits::events::BlitzImeEvent::Preedit(
            "にほん".to_string(),
            Some((0, 9)),
        )));
        doc.handle_ui_event(UiEvent::Ime(blitz::traits::events::BlitzImeEvent::Preedit(
            String::new(),
            None,
        )));

        doc.handle_ui_event(UiEvent::Ime(blitz::traits::events::BlitzImeEvent::Commit(
            "日本語".to_string(),
        )));
        let inner = doc.dom.borrow();
        let input = inner.get_element_by_id("ime").unwrap();
        let text = inner
            .get_node(input)
            .and_then(|node| node.element_data())
            .and_then(|element| element.text_input_data())
            .map(|input| input.editor.text().to_string())
            .unwrap();
        assert_eq!(text, "日本語");
        drop(inner);
        let observed = doc.js_context.with(|ctx| {
            ctx.eval::<String, _>(
                "JSON.stringify([document.getElementById('ime').value, globalThis.__imeInputEvents])",
            )
        });
        assert_eq!(observed.unwrap(), r#"["日本語",1]"#);
    }

    #[test]
    fn text_selection_uses_dom_utf16_offsets_and_ime_commit_replaces_selection() {
        let html = r#"<!doctype html><html><body><input id="text"><textarea id="notes"></textarea><input id="number" type="number"></body></html>"#;
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(r#"
                  const input=document.getElementById('text');
                  input.value='A😀中Z';
                  input.setSelectionRange(1,4,'backward');
                  const notes=document.getElementById('notes');
                  notes.value='文字';notes.selectionStart=1;
                  globalThis.selectionState=[input.selectionStart,input.selectionEnd,input.selectionDirection,
                    notes.selectionStart,notes.selectionEnd];
                  input.setSelectionRange(1,4,'none');globalThis.selectionNone=input.selectionDirection;
                  input.setSelectionRange(1,4,'backward');
                  try { document.getElementById('number').setSelectionRange(0,1); }
                  catch (error) { globalThis.selectionError=error.name; }
                "#)
            })
            .unwrap();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(selectionState)"))
                .unwrap(),
            "[1,4,\"backward\",1,1]"
        );
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("selectionError"))
                .unwrap(),
            "InvalidStateError"
        );
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("selectionNone"))
                .unwrap(),
            "none"
        );
        let input = doc.dom.borrow().get_element_by_id("text").unwrap();
        doc.dom.borrow_mut().set_focus_to(input);
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "globalThis.selectionInputs=0;document.getElementById('text').addEventListener('input',()=>selectionInputs++)",
                )
            })
            .unwrap();
        doc.handle_ui_event(UiEvent::Ime(blitz::traits::events::BlitzImeEvent::Commit(
            "X".to_owned(),
        )));
        assert_eq!(
            doc.js_context
                .with(|ctx| {
                    ctx.eval::<String, _>(
                        "JSON.stringify([document.getElementById('text').value,document.getElementById('text').selectionStart,document.getElementById('text').selectionEnd,document.getElementById('text').selectionDirection,selectionInputs])",
                    )
                })
                .unwrap(),
            r#"["AXZ",2,2,"none",1]"#
        );
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn dom_focus_api_tracks_active_element_and_dispatches_focus_events() {
        let html = r#"<!doctype html><html><body><input id="first"><input id="second"><input id="disabled" disabled><div id="plain"></div></body></html>"#;
        let (doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        let result = doc.js_context.with(|ctx| {
            ctx.eval::<String, _>(
                r#"
                  const first = document.getElementById('first');
                  const second = document.getElementById('second');
                  const plain = document.getElementById('plain');
                  const events = [];
                  first.addEventListener('focus', () => events.push('first:focus'));
                  first.addEventListener('focusin', () => events.push('first:focusin'));
                  first.addEventListener('blur', () => events.push('first:blur'));
                  first.addEventListener('focusout', () => events.push('first:focusout'));
                  second.addEventListener('focus', () => events.push('second:focus'));
                  second.addEventListener('focusin', () => events.push('second:focusin'));
                  second.addEventListener('blur', () => events.push('second:blur'));
                  second.addEventListener('focusout', () => events.push('second:focusout'));
                  document.body.addEventListener('focusin', event => events.push(`${event.target.id}:bubble-in`));
                  document.body.addEventListener('focusout', event => events.push(`${event.target.id}:bubble-out`));
                  if (document.activeElement !== document.body) throw new Error('initial activeElement should be body');
                  globalThis.__focusTestStep = 'plain-focus';
                  plain.focus();
                  if (document.activeElement !== document.body) throw new Error('non-focusable element received focus');
                  document.getElementById('disabled').focus();
                  if (document.activeElement !== document.body || lapui.focus('disabled')) throw new Error('disabled control received focus');
                  const detached = document.createElement('input');
                  detached.focus();
                  if (document.activeElement !== document.body) throw new Error('detached control received focus');
                  globalThis.__focusTestStep = 'first-focus';
                  if (!lapui.focus(first.__ref)) throw new Error('AI focus operation failed');
                  if (document.activeElement !== first) throw new Error('focus did not update activeElement');
                  if (!lapui.controls().controls.find(control => control.id === 'first').focused) throw new Error('control snapshot did not expose focus state');
                  globalThis.__focusTestStep = 'second-focus';
                  second.focus();
                  if (document.activeElement !== second) throw new Error('focus transfer failed');
                  if (lapui.controls().controls.find(control => control.id === 'first').focused || !lapui.controls().controls.find(control => control.id === 'second').focused) throw new Error('focus snapshot was not updated after transfer');
                  globalThis.__focusTestStep = 'blur-inactive';
                  first.blur();
                  if (document.activeElement !== second) throw new Error('blur of inactive element cleared focus');
                  globalThis.__focusTestStep = 'blur-active';
                  second.blur();
                  if (document.activeElement !== document.body) throw new Error('blur did not restore body activeElement');
                  if (lapui.controls().controls.some(control => control.focused)) throw new Error('blur left stale focus state in controls snapshot');
                  JSON.stringify(events);
                "#,
            )
        });
        let result = result.unwrap_or_else(|error| {
            let details = doc.js_context.with(|ctx| {
                ctx.catch()
                    .into_object()
                    .and_then(Exception::from_object)
                    .map(|exception| (exception.message(), exception.stack()))
            });
            let step = doc
                .js_context
                .with(|ctx| ctx.eval::<String, _>("String(globalThis.__focusTestStep || '')"))
                .unwrap_or_default();
            panic!("focus bridge test step '{step}' failed: {error}; {details:?}");
        });
        assert_eq!(
            result,
            r#"["first:focus","first:focusin","first:bubble-in","first:blur","first:focusout","first:bubble-out","second:focus","second:focusin","second:bubble-in","second:blur","second:focusout","second:bubble-out"]"#
        );
    }

    #[test]
    fn keyboard_events_expose_web_fields_and_prevent_blitz_default_editing() {
        let html = r#"<!doctype html><html><body><input id="editor"></body></html>"#;
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        let input_id = {
            let mut inner = doc.dom.borrow_mut();
            inner.set_viewport(Viewport::new(320, 120, 1.0, ColorScheme::Light));
            inner.resolve(0.0);
            let input = inner.get_element_by_id("editor").unwrap();
            inner.set_focus_to(input);
            input
        };
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    r#"
                      globalThis.__keyboardEvents = [];
                      const editor = document.getElementById('editor');
                      editor.addEventListener('keydown', event => {
                        __keyboardEvents.push({
                          key: event.key,
                          code: event.code,
                          shiftKey: event.shiftKey,
                          ctrlKey: event.ctrlKey,
                          repeat: event.repeat,
                          isComposing: event.isComposing,
                          text: event.text,
                          bubbles: event.bubbles,
                          cancelable: event.cancelable
                        });
                        if (event.key === 'Q') event.preventDefault();
                      });
                    "#,
                )
            })
            .unwrap();

        let key_event = |key: &str, code: Code, modifiers: Modifiers, repeating: bool| {
            UiEvent::KeyDown(blitz::traits::events::BlitzKeyEvent {
                key: Key::Character(key.into()),
                code,
                modifiers,
                location: Location::Standard,
                is_auto_repeating: repeating,
                is_composing: false,
                state: blitz::traits::events::KeyState::Pressed,
                text: Some(key.into()),
            })
        };
        doc.handle_ui_event(key_event("Q", Code::KeyQ, Modifiers::SHIFT, true));
        let value_after_prevented = doc
            .dom
            .borrow()
            .get_node(input_id)
            .and_then(|node| node.element_data())
            .and_then(|element| element.text_input_data())
            .map(|input| input.editor.text().to_string())
            .unwrap();
        assert_eq!(value_after_prevented, "");

        doc.handle_ui_event(key_event("z", Code::KeyZ, Modifiers::empty(), false));
        let value_after_default = doc
            .dom
            .borrow()
            .get_node(input_id)
            .and_then(|node| node.element_data())
            .and_then(|element| element.text_input_data())
            .map(|input| input.editor.text().to_string())
            .unwrap();
        assert_eq!(value_after_default, "z");

        let events = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("JSON.stringify(globalThis.__keyboardEvents)"));
        let events: Value = serde_json::from_str(&events.unwrap()).unwrap();
        assert_eq!(events[0]["key"], "Q");
        assert_eq!(events[0]["code"], "KeyQ");
        assert_eq!(events[0]["shiftKey"], true);
        assert_eq!(events[0]["ctrlKey"], false);
        assert_eq!(events[0]["repeat"], true);
        assert_eq!(events[0]["isComposing"], false);
        assert_eq!(events[0]["text"], "Q");
        assert_eq!(events[0]["bubbles"], true);
        assert_eq!(events[0]["cancelable"], true);
        assert_eq!(events[1]["key"], "z");
        assert_eq!(events[1]["code"], "KeyZ");
        assert_eq!(events[1]["repeat"], false);
    }

    #[test]
    fn tab_and_shift_tab_navigate_focus_and_respect_prevent_default() {
        let html = r#"<!doctype html><html><body><input id="first"><input id="second"><button id="third">Third</button></body></html>"#;
        let (mut doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, "").unwrap();
        {
            let mut inner = doc.dom.borrow_mut();
            inner.set_viewport(Viewport::new(320, 160, 1.0, ColorScheme::Light));
            inner.resolve(0.0);
            let first = inner.get_element_by_id("first").unwrap();
            assert!(inner.set_focus_to(first));
        }
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    r#"
                      globalThis.__allowTab = false;
                      globalThis.__tabFocusEvents = [];
                      const first = document.getElementById('first');
                      const second = document.getElementById('second');
                      first.addEventListener('keydown', event => { if (event.key === 'Tab' && !__allowTab) event.preventDefault(); });
                      for (const [element, id] of [[first, 'first'], [second, 'second']]) {
                        element.addEventListener('focus', () => __tabFocusEvents.push(`${id}:focus`));
                        element.addEventListener('focusin', () => __tabFocusEvents.push(`${id}:focusin`));
                        element.addEventListener('blur', () => __tabFocusEvents.push(`${id}:blur`));
                        element.addEventListener('focusout', () => __tabFocusEvents.push(`${id}:focusout`));
                      }
                    "#,
                )
            })
            .unwrap();

        let tab = |modifiers| {
            UiEvent::KeyDown(blitz::traits::events::BlitzKeyEvent {
                key: Key::Tab,
                code: Code::Tab,
                modifiers,
                location: Location::Standard,
                is_auto_repeating: false,
                is_composing: false,
                state: blitz::traits::events::KeyState::Pressed,
                text: None,
            })
        };
        doc.handle_ui_event(tab(Modifiers::empty()));
        let first = doc.dom.borrow().get_element_by_id("first").unwrap();
        assert_eq!(doc.dom.borrow().get_focussed_node_id(), Some(first));
        let prevented_events = doc
            .js_context
            .with(|ctx| ctx.eval::<usize, _>("globalThis.__tabFocusEvents.length"));
        assert_eq!(prevented_events.unwrap(), 0);

        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("globalThis.__allowTab = true;"))
            .unwrap();
        doc.handle_ui_event(tab(Modifiers::empty()));
        let second = doc.dom.borrow().get_element_by_id("second").unwrap();
        assert_eq!(doc.dom.borrow().get_focussed_node_id(), Some(second));

        doc.handle_ui_event(tab(Modifiers::SHIFT));
        assert_eq!(doc.dom.borrow().get_focussed_node_id(), Some(first));
        let active_id = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("document.activeElement.id"));
        assert_eq!(active_id.unwrap(), "first");
        let events = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("JSON.stringify(globalThis.__tabFocusEvents)"));
        assert_eq!(
            events.unwrap(),
            r#"["first:blur","first:focusout","second:focus","second:focusin","second:blur","second:focusout","first:focus","first:focusin"]"#
        );
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    r#"
          const third = document.getElementById('third');
          third.addEventListener('focus', () => __tabFocusEvents.push('third:focus'));
          third.addEventListener('focusin', () => __tabFocusEvents.push('third:focusin'));
          globalThis.__redirectTab = event => { third.focus(); event.preventDefault(); };
          first.addEventListener('keydown', __redirectTab);
          __tabFocusEvents.length = 0;
        "#,
                )
            })
            .unwrap();
        doc.handle_ui_event(tab(Modifiers::empty()));
        let events = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("JSON.stringify(__tabFocusEvents)"));
        assert_eq!(
            events.unwrap(),
            r#"["first:blur","first:focusout","third:focus","third:focusin"]"#
        );
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    r#"
          first.focus();
          first.removeEventListener('keydown', __redirectTab);
          first.addEventListener('keydown', () => second.focus());
          __tabFocusEvents.length = 0;
        "#,
                )
            })
            .unwrap();
        doc.handle_ui_event(tab(Modifiers::empty()));
        let events = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("JSON.stringify(__tabFocusEvents)"));
        assert_eq!(
            events.unwrap(),
            r#"["first:blur","first:focusout","second:focus","second:focusin","second:blur","second:focusout","third:focus","third:focusin"]"#
        );
    }

    #[test]
    fn surface_fills_resized_viewport_and_button_has_pointer_cursor() {
        let (doc, _) = LapuiDocument::new(ActionRegistry::default(), None).unwrap();
        let mut inner = doc.dom.borrow_mut();
        inner.set_viewport(Viewport::new(800, 600, 1.0, ColorScheme::Light));
        inner.resolve(0.0);

        let surface = inner.get_element_by_id("surface").unwrap();
        let button = inner.get_element_by_id("increment").unwrap();
        let initial = inner.get_client_bounding_rect(surface).unwrap();
        assert!(
            initial.width >= 790.0,
            "initial surface width: {}",
            initial.width
        );
        assert!(
            initial.height >= 590.0,
            "initial surface height: {}",
            initial.height
        );

        inner.set_hover_to(790.0, 590.0);
        assert_eq!(inner.get_cursor(), Some(CursorIcon::Default));
        let button_rect = inner.get_client_bounding_rect(button).unwrap();
        inner.set_hover_to(
            (button_rect.x + button_rect.width / 2.0) as f32,
            (button_rect.y + button_rect.height / 2.0) as f32,
        );
        assert_eq!(inner.get_cursor(), Some(CursorIcon::Pointer));

        inner.set_viewport(Viewport::new(1100, 700, 1.0, ColorScheme::Light));
        inner.resolve(0.0);
        let resized = inner.get_client_bounding_rect(surface).unwrap();
        assert!(
            resized.width > initial.width + 250.0,
            "resized width: {}",
            resized.width
        );
        assert!(
            resized.height > initial.height + 90.0,
            "resized height: {}",
            resized.height
        );
    }

    #[test]
    fn password_value_is_never_in_semantic_control_snapshot() {
        let doc = HtmlDocument::from_html(
            r#"<html><body><label for="secret">Account password</label><input id="secret" type="password" value="do-not-expose" required aria-disabled="true"><input id="email" type="email" placeholder="name@example.test"></body></html>"#,
            DocumentConfig::default(),
        )
        .into_inner();
        let snapshot = control_snapshot(&doc);
        let secret = &snapshot["controls"][0];
        assert_eq!(secret["type"], "password");
        assert_eq!(secret["name"], "Account password");
        assert_eq!(secret["enabled"], false);
        assert_eq!(secret["required"], true);
        assert!(secret.get("value").is_none());
        assert_eq!(snapshot["controls"][1]["placeholder"], "name@example.test");
    }

    #[test]
    fn sensitive_autocomplete_values_are_omitted_from_semantic_snapshot() {
        let doc = HtmlDocument::from_html(
            r#"<html><body>
                <input autocomplete="section-login current-password" value="pw-secret">
                <input autocomplete="new-password" value="new-secret">
                <input autocomplete="one-time-code" value="otp-secret">
                <input autocomplete="cc-number" value="card-secret">
                <input autocomplete="cc-csc" value="cvc-secret">
                <input autocomplete="cc-exp-month" value="exp-secret">
                <input autocomplete="email" value="visible@example.test">
            </body></html>"#,
            DocumentConfig::default(),
        )
        .into_inner();
        let snapshot = control_snapshot(&doc);
        let serialized = serde_json::to_string(&snapshot).unwrap();
        for secret in [
            "pw-secret",
            "new-secret",
            "otp-secret",
            "card-secret",
            "cvc-secret",
            "exp-secret",
        ] {
            assert!(!serialized.contains(secret), "snapshot exposed {secret}");
        }
        assert!(serialized.contains("visible@example.test"));
    }

    #[test]
    fn custom_html_and_javascript_sources_bootstrap_together() {
        let html = r#"<!doctype html><html><body><script>globalThis.scriptOrder = ['html-inline'];</script><main id="root"><p id="message" style="color: red">waiting</p></main></body></html>"#;
        let script = "globalThis.scriptOrder.push('entry'); document.getElementById('message').textContent = 'loaded';";
        let (doc, _) =
            LapuiDocument::new_with_source(ActionRegistry::default(), None, html, script).unwrap();
        assert_eq!(text(&doc, "message"), "loaded");
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("document.getElementById('message').style.color"))
                .unwrap(),
            "red"
        );
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("globalThis.scriptOrder.join(',')"))
                .unwrap(),
            "html-inline,entry"
        );
    }

    #[test]
    fn local_html_scripts_execute_in_document_order_with_root_boundary() {
        let test_root =
            std::env::temp_dir().join(format!("lapui-script-test-{}", std::process::id()));
        let app_root = test_root.join("app");
        std::fs::create_dir_all(&app_root).unwrap();
        std::fs::write(
            app_root.join("one.js"),
            "globalThis.scriptOrder.push('external');",
        )
        .unwrap();
        std::fs::write(
            app_root.join("data.json"),
            r#"{"label":"local fetch works"}"#,
        )
        .unwrap();
        std::fs::write(
            test_root.join("outside.js"),
            "globalThis.scriptOrder.push('outside');",
        )
        .unwrap();
        let html = r#"<!doctype html><html><body>
          <script>globalThis.scriptOrder = ['inline-1'];</script>
          <script src="one.js"></script>
          <script>throw new Error('one script fails');</script>
          <script>globalThis.scriptOrder.push('inline-2');</script>
          <script src="../outside.js"></script>
          <p id="message">waiting</p>
        </body></html>"#;
        let script = "globalThis.scriptOrder.push('entry'); globalThis.localFetchBoundary = 'pending'; fetch('data.json').then(response => response.json()).then(data => document.getElementById('message').textContent = data.label).catch(error => document.getElementById('message').textContent = 'fetch failed: ' + error.message); fetch('../outside.js').then(() => globalThis.localFetchBoundary = 'unexpected-success').catch(() => globalThis.localFetchBoundary = 'rejected');";
        let (mut doc, _) = LapuiDocument::new_with_local_source(
            ActionRegistry::default(),
            None,
            html,
            script,
            &app_root,
        )
        .unwrap();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<String, _>("globalThis.scriptOrder.join(',')"))
                .unwrap(),
            "inline-1,external,inline-2,entry"
        );
        for _ in 0..100 {
            doc.poll(None);
            let boundary_checked = doc
                .js_context
                .with(|ctx| ctx.eval::<bool, _>("globalThis.localFetchBoundary === 'rejected'"))
                .unwrap_or(false);
            if text(&doc, "message") == "local fetch works" && boundary_checked {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(text(&doc, "message"), "local fetch works");
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("globalThis.localFetchBoundary === 'rejected'"))
            .unwrap());
        std::fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn es_module_graph_preserves_import_identity_cycles_scope_and_async_evaluation() {
        let app_root =
            std::env::temp_dir().join(format!("lapui-module-graph-{}", std::process::id()));
        std::fs::create_dir_all(app_root.join("nested")).unwrap();
        for (name, source) in [
            ("shared.mjs", "globalThis.moduleLoads = (globalThis.moduleLoads || 0) + 1; export let count = 0; export const increment = () => count++;"),
            ("nested/reexport.mjs", "export { count, increment } from '../shared.mjs';"),
            ("a.mjs", "import { getB } from './b.mjs'; export function getA() { return 'A'; } export function fromB() { return getB(); }"),
            ("b.mjs", "import { getA } from './a.mjs'; export function getB() { return getA() + 'B'; }"),
            ("main.mjs", r#"
              import { count, increment } from './nested/reexport.mjs';
              import * as one from './shared.mjs';
              import * as two from './nested/../shared.mjs';
              import { getB } from './b.mjs';
              const modulePrivate = 42;
              increment();
              globalThis.moduleGraph = { count, identity: one === two, cycle: getB(), url: import.meta.url };
              globalThis.scriptOrder.push('module');
              const result = await lapui.invoke('counter.increment');
              globalThis.moduleHostCount = result.count;
              const lazy = await import('./nested/../shared.mjs');
              globalThis.moduleDynamicIdentity = lazy === one;
            "#),
        ] {
            std::fs::write(app_root.join(name), source).unwrap();
        }
        let html = r#"<!doctype html><html><body>
          <script>globalThis.scriptOrder = ['classic-1'];</script>
          <script type="module" src="./main.mjs"></script>
          <script type="module" src="./shared.mjs"></script>
          <script type="module" src="././shared.mjs"></script>
          <script nomodule>globalThis.nomoduleRan = true;</script>
          <script>globalThis.scriptOrder.push('classic-2');</script>
          <script type="module">import { count } from './shared.mjs'; globalThis.inlineModuleCount = count; globalThis.inlineModuleUrl = import.meta.url;</script>
        </body></html>"#;
        let (mut doc, _) = LapuiDocument::new_with_local_source(
            ActionRegistry::default(),
            None,
            html,
            "",
            &app_root,
        )
        .unwrap();
        for _ in 0..100 {
            doc.poll(None);
            if doc
                .js_context
                .with(|ctx| ctx.eval::<bool, _>("globalThis.moduleDynamicIdentity === true"))
                .unwrap_or(false)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let result = doc.js_context.with(|ctx| ctx.eval::<String, _>(r#"JSON.stringify({
            graph: moduleGraph, loads: moduleLoads, hostCount: moduleHostCount,
            dynamicIdentity: moduleDynamicIdentity, inlineCount: inlineModuleCount,
            inlineUrl: inlineModuleUrl, order: scriptOrder,
            leakedScope: typeof modulePrivate !== 'undefined', nomodule: globalThis.nomoduleRan === true,
            errors: lapui.diagnostics()
        })"#)).unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["graph"]["count"], 1);
        assert_eq!(result["graph"]["identity"], true);
        assert_eq!(result["graph"]["cycle"], "AB");
        assert!(result["graph"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/main.mjs"));
        assert_eq!(result["loads"], 1);
        assert_eq!(result["hostCount"], 1);
        assert_eq!(result["dynamicIdentity"], true);
        assert_eq!(result["inlineCount"], 1);
        assert!(result["inlineUrl"]
            .as_str()
            .unwrap()
            .contains("#inline-script-"));
        assert_eq!(result["order"], json!(["classic-1", "classic-2", "module"]));
        assert_eq!(result["leakedScope"], false);
        assert_eq!(result["nomodule"], false);
        assert_eq!(result["errors"], json!([]));
        drop(doc);
        std::fs::remove_dir_all(app_root).unwrap();
    }

    #[test]
    fn module_load_failures_obey_app_boundary_and_have_structured_diagnostics() {
        let test_root =
            std::env::temp_dir().join(format!("lapui-module-errors-{}", std::process::id()));
        let app_root = test_root.join("app");
        std::fs::create_dir_all(&app_root).unwrap();
        std::fs::write(
            test_root.join("outside.mjs"),
            "globalThis.outsideModuleExecuted = true;",
        )
        .unwrap();
        std::fs::write(app_root.join("invalid.mjs"), [0xff, 0xfe]).unwrap();
        let oversized = std::fs::File::create(app_root.join("large.mjs")).unwrap();
        oversized.set_len(32 * 1024 * 1024 + 1).unwrap();
        drop(oversized);
        let html = r#"<!doctype html><html><body>
          <script type="module">import '../outside.mjs';</script>
          <script type="module">import 'unknown-package';</script>
          <script type="module">import 'https://example.test/remote.mjs';</script>
          <script type="module">import './missing.mjs';</script>
          <script type="module" src="./invalid.mjs"></script>
          <script type="module" src="./large.mjs"></script>
          <script type="module">await Promise.resolve(); throw new Error('asynchronous module failed');</script>
          <script type="module">globalThis.goodModuleRan = true; import('../outside.mjs').catch(error => globalThis.dynamicImportError = String(error));</script>
        </body></html>"#;
        let (doc, _) = LapuiDocument::new_with_local_source(
            ActionRegistry::default(),
            None,
            html,
            "",
            &app_root,
        )
        .unwrap();
        let result = doc.js_context.with(|ctx| ctx.eval::<String, _>(r#"JSON.stringify({
            good: globalThis.goodModuleRan === true, leaked: globalThis.outsideModuleExecuted === true,
            dynamicError: globalThis.dynamicImportError, errors: lapui.diagnostics()
        })"#)).unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["good"], true);
        assert_eq!(result["leaked"], false);
        assert!(result["dynamicError"]
            .as_str()
            .unwrap()
            .contains("outside the application directory"));
        let errors = result["errors"].as_array().unwrap();
        let messages = errors
            .iter()
            .map(|error| error["message"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        for required in [
            "outside the application directory",
            "bare module specifiers",
            "app-local file",
            "could not be resolved",
            "not valid UTF-8",
            "32 MiB",
            "asynchronous module failed",
        ] {
            assert!(
                messages.contains(required),
                "missing diagnostic '{required}': {messages}"
            );
        }
        assert!(errors
            .iter()
            .all(|error| error["source"].is_string() && error["phase"].is_string()));
        drop(doc);
        std::fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn es_module_example_resumes_top_level_await_and_lazy_rust_action() {
        let app_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/modules-demo");
        let (mut doc, _) = LapuiDocument::new_with_local_source(
            ActionRegistry::default(),
            None,
            include_str!("../examples/modules-demo/index.html"),
            "",
            &app_root,
        )
        .unwrap();
        for _ in 0..100 {
            doc.poll(None);
            if text(&doc, "module-status") == "Local modules and top-level await are ready." {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            text(&doc, "module-status"),
            "Local modules and top-level await are ready."
        );
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("document.getElementById('module-status').getAttribute('data-module-url').endsWith('/main.mjs') && lapui.activate('module-increment')")).unwrap());
        drain_jobs(&doc.js_runtime, &doc.script_budget).unwrap();
        for _ in 0..100 {
            doc.poll(None);
            if text(&doc, "module-count") == "Count: 1" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(text(&doc, "module-count"), "Count: 1");
        assert_eq!(
            text(&doc, "module-status"),
            "Rust action completed at version 1"
        );
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("!document.getElementById('module-increment').disabled && lapui.diagnostics().length === 0")).unwrap());
    }

    #[test]
    fn vue3_runtime_dom_bundle_renders_and_patches_the_blitz_tree() {
        let app_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/vue-demo");
        let html = include_str!("../examples/vue-demo/index.html");
        let (mut doc, _) = LapuiDocument::new_with_local_source(
            ActionRegistry::default(),
            None,
            html,
            "",
            &app_root,
        )
        .unwrap();

        let initial = text(&doc, "app");
        assert!(initial.contains("任务面板"));
        assert!(initial.contains("探索 QuickJS"));
        assert!(initial.contains("响应式列表更新"));

        let controls = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("JSON.stringify(lapui.controls())"))
            .unwrap();
        let controls: Value = serde_json::from_str(&controls).unwrap();
        assert!(controls["controls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|control| { control["id"] == "task-input" && control["name"] == "新任务" }));
        assert!(controls["controls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|control| {
                control["id"] == "task-done-1"
                    && control["role"] == "checkbox"
                    && control["checked"] == false
            }));

        let epoch = doc.inner().id() as u64;
        let page_baseline = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"limit":64}),
        )
        .unwrap();
        let page_cursor = page_baseline["cursor"].clone();

        let filled = doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("lapui.fill('task-input', '写 Vue 集成回归')"))
            .unwrap();
        assert!(filled);
        drain_jobs(&doc.js_runtime, &doc.script_budget).unwrap();
        let activated = doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("lapui.activate('add-task')"))
            .unwrap();
        assert!(activated);
        drain_jobs(&doc.js_runtime, &doc.script_budget).unwrap();
        assert!(text(&doc, "app").contains("写 Vue 集成回归"));
        let page_delta = control_request(
            &mut doc,
            json!({"method":"pageChanges","documentEpoch":epoch,"cursor":page_cursor,"limit":64}),
        )
        .unwrap();
        assert!(page_delta["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| {
                record["type"] == "property"
                    && record["propertyName"] == "value"
                    && record["target"]
                        == controls["controls"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|control| control["id"] == "task-input")
                            .unwrap()["ref"]
            }));

        let checked = doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("lapui.check('task-done-1', true)"))
            .unwrap();
        assert!(checked);
        drain_jobs(&doc.js_runtime, &doc.script_budget).unwrap();
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("document.querySelector('.item-name.done') !== null"))
            .unwrap());
        let checked_control = doc
            .js_context
            .with(|ctx| ctx.eval::<String, _>("JSON.stringify(lapui.controls().controls.find(control => control.id === 'task-done-1'))"))
            .unwrap();
        let checked_control: Value = serde_json::from_str(&checked_control).unwrap();
        assert_eq!(checked_control["checked"], true);

        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("lapui.activate('remove-task-3');"))
            .unwrap();
        drain_jobs(&doc.js_runtime, &doc.script_budget).unwrap();
        assert_eq!(
            doc.js_context
                .with(|ctx| ctx.eval::<usize, _>("document.querySelectorAll('.item').length"))
                .unwrap(),
            2
        );

        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("lapui.activate('toggle-details');"))
            .unwrap();
        drain_jobs(&doc.js_runtime, &doc.script_budget).unwrap();
        assert!(text(&doc, "app").contains("一个本地界面"));
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("lapui.activate('close-details');"))
            .unwrap();
        drain_jobs(&doc.js_runtime, &doc.script_budget).unwrap();
        assert!(!text(&doc, "app").contains("一个本地界面"));
    }

    #[test]
    fn local_resource_provider_reads_only_inside_app_root() {
        use blitz::traits::net::NetHandler;
        use std::sync::mpsc;

        struct Handler(mpsc::Sender<(String, Vec<u8>)>);
        impl NetHandler for Handler {
            fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
                let _ = self.0.send((resolved_url, bytes.to_vec()));
            }
        }

        let test_root =
            std::env::temp_dir().join(format!("lapui-resource-test-{}", std::process::id()));
        let app_root = test_root.join("app");
        let outside = test_root.join("secret.txt");
        std::fs::create_dir_all(&app_root).unwrap();
        std::fs::write(app_root.join("theme.css"), b"body { color: red }").unwrap();
        std::fs::write(&outside, b"private").unwrap();
        let provider = LocalDirectoryNetProvider {
            root: app_root.canonicalize().unwrap(),
        };
        let (tx, rx) = mpsc::channel();
        let css_url = Url::from_file_path(app_root.join("theme.css")).unwrap();
        provider.fetch(0, Request::get(css_url), Box::new(Handler(tx.clone())));
        let (resolved_url, bytes) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(resolved_url.ends_with("/theme.css"));
        assert_eq!(bytes, b"body { color: red }");

        let outside_url = Url::from_file_path(&outside).unwrap();
        let outside_url_string = outside_url.to_string();
        provider.fetch(0, Request::get(outside_url), Box::new(Handler(tx)));
        let (rejected_url, empty_body) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(rejected_url, outside_url_string);
        assert!(empty_body.is_empty());
        std::fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn fetch_abort_releases_active_and_queued_requests_with_bounded_concurrency() {
        use std::io::{BufRead, BufReader, Read};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted, connections) = mpsc::channel();
        let (closed, closures) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..crate::fetch_work::MAX_ACTIVE {
                let (stream, _) = listener.accept().unwrap();
                let accepted = accepted.clone();
                let closed = closed.clone();
                workers.push(std::thread::spawn(move || {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut first = String::new();
                    reader.read_line(&mut first).unwrap();
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                    }
                    accepted.send(first.clone()).unwrap();
                    let mut byte = [0];
                    match reader.read(&mut byte) {
                        Ok(0) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::ConnectionAborted
                            ) => {}
                        result => panic!("abort did not close the request socket: {result:?}"),
                    }
                    closed.send(first).unwrap();
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let script = format!(
            r#"
            globalThis.abortResults = [];
            globalThis.controllers = [];
            globalThis.busy = null;
            for (let i = 0; i < 16; i++) {{
                const controller = new AbortController();
                controllers.push(controller);
                fetch('http://{address}/' + i, {{signal: controller.signal}})
                    .catch(error => abortResults.push(error));
            }}
            fetch('http://{address}/overflow').catch(error => busy = error.code);
        "#
        );
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body></body></html>",
            &script,
        )
        .unwrap();
        for _ in 0..8 {
            connections.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        assert!(connections.recv_timeout(Duration::from_millis(80)).is_err());
        assert_eq!(doc.fetch_requests.lock().unwrap().len(), 16);
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("controllers[15].abort('queued');"))
            .unwrap();
        for _ in 0..200 {
            doc.poll(None);
            if doc.fetch_requests.lock().unwrap().len() == 15 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            doc.fetch_requests.lock().unwrap().len(),
            15,
            "queued cancellation must complete while all eight active requests are stalled"
        );
        assert!(closures.try_recv().is_err());
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("controllers[0].abort('active');"))
            .unwrap();
        assert!(closures
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .contains("GET /0 "));
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>("controllers.forEach(controller => controller.abort('rest'));")
            })
            .unwrap();
        for _ in 0..7 {
            closures.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        server.join().unwrap();
        for _ in 0..200 {
            doc.poll(None);
            if doc.fetch_requests.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(doc.fetch_requests.lock().unwrap().is_empty());
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("busy === 'network_busy' && abortResults.length === 16 && abortResults.includes('queued') && abortResults.includes('active')")).unwrap());
        doc.js_context.with(|ctx| ctx.eval::<(), _>("globalThis.reused = null; fetch('unsupported:resource').catch(error => reused = error.code);")).unwrap();
        for _ in 0..200 {
            doc.poll(None);
            if doc
                .js_context
                .with(|ctx| ctx.eval::<bool, _>("reused === 'network_error'"))
                .unwrap()
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("fetch executor did not accept a request after cancellation");
    }

    #[test]
    fn fetch_signal_timeout_interrupts_a_stalled_body_and_signal_errors_are_isolated() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (started, ready) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).unwrap() > 0);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nx")
                .unwrap();
            started.send(()).unwrap();
            let mut byte = [0];
            match stream.read(&mut byte) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                    ) => {}
                result => panic!("timeout did not release the stalled body socket: {result:?}"),
            }
        });
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None, "<html><body></body></html>", &format!(r#"
            globalThis.timeoutResult = null;
            globalThis.bodyController = new AbortController();
            fetch('http://{address}/body', {{signal: bodyController.signal}}).catch(error => timeoutResult = error.name);
            globalThis.events = [];
            const signal = bodyController.signal;
            signal.addEventListener('abort', () => {{ throw new Error('isolated abort listener'); }});
            const record = event => events.push(event.target === signal);
            signal.addEventListener('abort', record, {{once: true}});
            signal.addEventListener('abort', record, {{once: true}});
            signal.onabort = () => events.push(signal.aborted);
            globalThis.preaborted = null;
            const reason = {{custom: true}};
            fetch('http://{address}/never', {{signal: AbortSignal.abort(reason)}}).catch(error => preaborted = error === reason);
            globalThis.circularResult = null;
            const headers = {{get broken() {{ throw new TypeError('header conversion failed'); }} }};
            fetch('http://{address}/never', {{headers}}).catch(error => circularResult = error instanceof TypeError);
            globalThis.invalidSignal = null;
            fetch('http://{address}/never', {{signal: {{aborted: false}}}}).catch(error => invalidSignal = error instanceof TypeError);
        "#)).unwrap();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    r#"
            const timeout = AbortSignal.timeout(5);
            timeout.addEventListener('abort', () => bodyController.abort(timeout.reason));
        "#,
                )
            })
            .unwrap();
        for _ in 0..200 {
            doc.poll(None);
            if doc
                .js_context
                .with(|ctx| ctx.eval::<bool, _>("timeoutResult === 'TimeoutError'"))
                .unwrap()
                && doc.fetch_requests.lock().unwrap().is_empty()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        server.join().unwrap();
        assert!(doc.fetch_requests.lock().unwrap().is_empty());
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("timeoutResult === 'TimeoutError' && events.length === 2 && events.every(Boolean) && preaborted === true && circularResult === true && invalidSignal === true")).unwrap());
        assert!(doc
            .script_diagnostics
            .borrow()
            .iter()
            .any(|item| item.message.contains("isolated abort listener")));
    }

    #[test]
    fn fetch_http_base_resolves_relative_urls_and_response_body_is_consumed_once() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..size])
                .starts_with("GET /ui/data.json?value=1 HTTP/1.1"));
            assert!(String::from_utf8_lossy(&request[..size]).contains("x-request: first, second"));
            let body = r#"{"error":"missing"}"#;
            write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let (mut doc, _) = LapuiDocument::new_with_http_base(ActionRegistry::default(), None, "<html><body></body></html>", r#"
            globalThis.relativeResult = null;
            const requestHeaders = new Headers([['X-Request', ' first ']]);
            requestHeaders.append('x-request', 'second');
            const copied = new Headers(requestHeaders);
            copied.set('temporary', 'value'); copied.delete('temporary');
            if (copied.get('X-REQUEST') !== 'first, second' || copied.has('temporary') || [...copied.keys()].length !== 1) throw new Error('header collection failed');
            fetch('data.json?value=1', {headers: copied}).then(async response => {
                const cloned = response.clone();
                const unused = !response.bodyUsed;
                const data = await response.json();
                let rejectedRead = false, rejectedClone = false;
                try { await response.text(); } catch(error) { rejectedRead = error instanceof TypeError; }
                try { response.clone(); } catch(error) { rejectedClone = error instanceof TypeError; }
                relativeResult = { unused, data, text: await cloned.text(), used: response.bodyUsed, rejectedRead, rejectedClone, status: response.status, ok: response.ok, url: response.url };
            });
        "#, &format!("http://{address}/ui/index.html")).unwrap();
        for _ in 0..200 {
            doc.poll(None);
            if doc
                .js_context
                .with(|ctx| ctx.eval::<bool, _>("relativeResult !== null"))
                .unwrap()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        server.join().unwrap();
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("relativeResult.unused && relativeResult.used && relativeResult.rejectedRead && relativeResult.rejectedClone && relativeResult.status === 404 && !relativeResult.ok && relativeResult.data.error === 'missing' && JSON.parse(relativeResult.text).error === 'missing' && relativeResult.url.endsWith('/ui/data.json?value=1')")).unwrap());
        assert!(LapuiDocument::new_with_http_base(
            ActionRegistry::default(),
            None,
            "",
            "",
            "file:///outside"
        )
        .is_err());
    }

    #[test]
    fn fetch_rejects_oversized_requests_and_declared_and_chunked_response_bodies() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                workers.push(std::thread::spawn(move || {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut request = [0; 4096];
                    let size = stream.read(&mut request).unwrap();
                    if String::from_utf8_lossy(&request[..size]).contains("/declared ") {
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8388609\r\n\r\n")
                            .unwrap();
                    } else {
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                            .unwrap();
                        let block = vec![b'x'; 64 * 1024];
                        for _ in 0..128 {
                            stream.write_all(b"10000\r\n").unwrap();
                            stream.write_all(&block).unwrap();
                            stream.write_all(b"\r\n").unwrap();
                        }
                        stream.write_all(b"1\r\nx\r\n").unwrap();
                        // The client may close as soon as its byte limit is exceeded.
                        let _ = stream.write_all(b"0\r\n\r\n");
                    }
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None, "<html><body></body></html>", &format!(r#"
            globalThis.limitResults = {{}};
            for (const path of ['declared','chunked']) fetch('http://{address}/' + path).catch(error => limitResults[path] = {{code: error.code, message: error.message}});
            fetch('http://{address}/never', {{body: 'x'.repeat(65536)}}).catch(error => limitResults.request = error.code);
        "#)).unwrap();
        for _ in 0..400 {
            doc.poll(None);
            if doc
                .js_context
                .with(|ctx| ctx.eval::<bool, _>("Object.keys(limitResults).length === 3"))
                .unwrap()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        server.join().unwrap();
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool, _>("limitResults.request === 'invalid_arguments' && ['declared','chunked'].every(path => limitResults[path]?.code === 'network_error' && limitResults[path].message.includes('8 MiB'))")).unwrap());
        assert!(doc.fetch_requests.lock().unwrap().is_empty());
    }

    #[test]
    fn fetch_runs_off_thread_and_resolves_http_response_promises() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0_u8; 4096];
            let size = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..size]).starts_with("POST /api HTTP/1.1"));
            let body = r#"{"value":42}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let (mut doc, _) = LapuiDocument::new(ActionRegistry::default(), None).unwrap();
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(format!(
                    r#"
                      globalThis.__fetchResult = null;
                      fetch('http://{address}/api', {{
                        method: 'POST',
                        headers: {{ 'content-type': 'application/json' }},
                        body: '{{}}'
                      }}).then(async response => {{
                        __fetchResult = {{ status: response.status, ok: response.ok, body: await response.json() }};
                      }});
                    "#
                ))
            })
            .unwrap();

        let mut result = Value::Null;
        for _ in 0..200 {
            doc.poll(None);
            result = doc
                .js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(__fetchResult)"))
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or(Value::Null);
            if !result.is_null() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        server.join().unwrap();
        assert_eq!(result["status"], 200);
        assert_eq!(result["ok"], true);
        assert_eq!(result["body"]["value"], 42);
    }

    #[test]
    fn sse_decoder_preserves_utf8_and_multiline_data_across_chunks() {
        let mut parser = crate::stream_work::SseParser::default();
        assert!(parser
            .feed(b"\xEF\xBB\xBFid: 7\r\nevent: update\r\ndata: \xE4")
            .unwrap()
            .is_empty());
        let events = parser
            .feed(b"\xB8\xAD\r\ndata: second\r\nretry: 750\r\n\r\n")
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "update");
        assert_eq!(events[0].data, "中\nsecond");
        assert_eq!(events[0].last_event_id, "7");
        assert_eq!(parser.retry_ms, Some(750));
    }

    #[test]
    fn event_source_receives_sse_messages_and_close_stops_the_stream() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0_u8; 4096];
            let size = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..size]).starts_with("GET /events HTTP/1.1"));
            let body = "id: 7\r\nretry: 250\r\nevent: update\r\ndata: first\r\ndata: second\r\n\r\ndata: next\n\n";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            )
            .unwrap();
        });

        let (mut doc, _) = LapuiDocument::new(ActionRegistry::default(), None).unwrap();
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(format!(
                    r#"
                      globalThis.__sseMessages = [];
                      globalThis.__source = new EventSource('http://{address}/events');
                      __source.addEventListener('update', event => __sseMessages.push({{type:event.type,data:event.data,id:event.lastEventId}}));
                      __source.onmessage = event => {{
                        __sseMessages.push({{type:event.type,data:event.data,id:event.lastEventId}});
                        if (__sseMessages.length >= 2) __source.close();
                      }};
                    "#
                ))
            })
            .unwrap();

        let mut messages = Value::Null;
        for _ in 0..200 {
            doc.poll(None);
            messages = doc
                .js_context
                .with(|ctx| ctx.eval::<String, _>("JSON.stringify(__sseMessages)"))
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or(Value::Null);
            if messages
                .as_array()
                .is_some_and(|messages| messages.len() >= 2)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        server.join().unwrap();
        assert_eq!(messages[0]["type"], "update");
        assert_eq!(messages[0]["data"], "first\nsecond");
        assert_eq!(messages[0]["id"], "7");
        assert_eq!(messages[1]["type"], "message");
        assert_eq!(messages[1]["data"], "next");
        let state = doc
            .js_context
            .with(|ctx| ctx.eval::<u32, _>("__source.readyState"))
            .unwrap();
        assert_eq!(state, 2);
    }

    fn poll_stream_until(doc: &mut LapuiDocument, condition: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            doc.poll(None);
            if doc
                .js_context
                .with(|ctx| ctx.eval::<bool, _>(condition))
                .unwrap()
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!(
            "network condition timed out: {condition}; status={}; diagnostics={}",
            stream_status(&doc.streams),
            serde_json::to_string(&*doc.script_diagnostics.borrow()).unwrap()
        );
    }

    fn read_stream_test_request(stream: &mut std::net::TcpStream) {
        use std::io::{BufRead, BufReader};
        let mut reader = BufReader::new(stream);
        let mut bytes = 0;
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).unwrap();
            assert!(read > 0, "request ended before its headers");
            bytes += read;
            assert!(bytes <= 16384, "request headers exceed fixture limit");
            if line == "\r\n" {
                return;
            }
        }
    }

    #[test]
    fn stream_constructors_enforce_combined_capacity_and_close_reclaims_native_slots() {
        let (mut doc, _) = LapuiDocument::new_with_source(
            ActionRegistry::default(),
            None,
            "<html><body></body></html>",
            "",
        )
        .unwrap();
        assert_eq!(stream_status(&doc.streams)["reactorStarted"], false);
        doc.js_context.with(|ctx| ctx.eval::<(),_>(r#"
            globalThis.streams = [];
            for (let index = 0; index < 8; index++) streams.push(index % 2 ? new WebSocket('ws://127.0.0.1:0/') : new EventSource('http://127.0.0.1:0/'));
            globalThis.busy = ''; globalThis.invalid = '';
            try { new EventSource('http://127.0.0.1:0/'); } catch(error) { busy = error.code; }
            try { new WebSocket('file:///bad'); } catch(error) { invalid = error.code; }
        "#)).unwrap();
        assert_eq!(stream_status(&doc.streams)["streams"], 8);
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("busy === 'network_busy' && invalid === 'invalid_url'"))
            .unwrap());
        let response = control_request(&mut doc, json!({"method":"networkStatus"})).unwrap();
        assert_eq!(response["streamLimits"]["outstanding"], 8);
        doc.js_context
            .with(|ctx| ctx.eval::<(), _>("streams.forEach(stream => stream.close())"))
            .unwrap();
        poll_stream_until(&mut doc, "lapui.networkStatus().streams === 0");
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn sse_transport_backpressure_delivers_every_message_in_order_after_ui_resumes() {
        use std::io::Write;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            read_stream_test_request(&mut stream);
            let body: String = (0..100).map(|index| format!("data:{index}\n\n")).collect();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        });
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None, "<html><body></body></html>", &format!(r#"
            globalThis.messages = [];
            globalThis.source = new EventSource('http://{address}/');
            source.onmessage = event => {{ messages.push(Number(event.data)); if(messages.length === 100) source.close(); }};
        "#)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        // Intentionally do not poll the UI while native transport fills its queue.
        while stream_status(&doc.streams)["queuedEvents"] != 64 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(stream_status(&doc.streams)["queuedEvents"], 64);
        assert!(doc
            .js_context
            .with(|ctx| ctx.eval::<bool, _>("messages.length === 0"))
            .unwrap());
        poll_stream_until(
            &mut doc,
            "messages.length === 100 && lapui.networkStatus().streams === 0",
        );
        server.join().unwrap();
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool,_>("messages.every((value,index) => value === index) && source.readyState === EventSource.CLOSED && lapui.networkStatus().queuedEventBytes === 0")).unwrap());
    }

    #[test]
    fn websocket_typed_view_sends_flush_before_immediate_close_and_restore_buffered_amount() {
        use std::net::TcpListener;
        use tokio_tungstenite::tungstenite::{accept, Message};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut socket = accept(stream).unwrap();
            assert_eq!(socket.read().unwrap().into_text().unwrap(), "first");
            assert_eq!(socket.read().unwrap().into_data().as_ref(), &[1, 2, 255]);
            assert_eq!(socket.read().unwrap().into_text().unwrap(), "last");
            assert!(matches!(socket.read().unwrap(), Message::Close(_)));
        });
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None, "<html><body></body></html>", &format!(r#"
            globalThis.closed = false;
            globalThis.limitError = '';
            globalThis.socket = new WebSocket('ws://{address}/');
            socket.onopen = () => {{
                try {{ socket.send(new ArrayBuffer(8388609)); }} catch(error) {{ limitError = error.code; }}
                socket.send('first');
                const buffer = new Uint8Array([77,1,2,255,88]).buffer;
                socket.send(new DataView(buffer, 1, 3));
                socket.send('last');
                socket.close();
            }};
            socket.onclose = () => closed = true;
        "#)).unwrap();
        poll_stream_until(&mut doc, "closed");
        server.join().unwrap();
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool,_>("limitError === 'message_too_large' && socket.bufferedAmount === 0 && lapui.networkStatus().streams === 0")).unwrap());
        assert!(doc.script_diagnostics.borrow().is_empty());
    }

    #[test]
    fn websocket_rejects_oversized_declared_frames_without_waiting_for_payload() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        for opcode in [0x81_u8, 0x82] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut socket = tokio_tungstenite::tungstenite::accept(stream).unwrap();
                let mut header = vec![opcode, 127];
                header
                    .extend_from_slice(&(crate::stream_work::MAX_MESSAGE as u64 + 1).to_be_bytes());
                socket.get_mut().write_all(&header).unwrap();
                // No payload is sent; rejection must release the transport.
                let mut byte = [0];
                let result = socket.get_mut().read(&mut byte);
                assert!(matches!(result, Ok(0)) || result.is_err());
            });
            let (mut doc, _) = LapuiDocument::new_with_source(
                ActionRegistry::default(),
                None,
                "<html><body></body></html>",
                &format!(
                    r#"
                globalThis.closed = false;
                globalThis.code = '';
                globalThis.closeCode = 0;
                globalThis.socket = new WebSocket('ws://{address}/');
                socket.onerror = event => code = event.code;
                socket.onclose = event => {{ closeCode = event.code; closed = true; }};
            "#
                ),
            )
            .unwrap();
            poll_stream_until(&mut doc, "closed");
            server.join().unwrap();
            assert!(doc.js_context.with(|ctx| ctx.eval::<bool,_>("code === 'message_too_large' && closeCode === 1009 && lapui.networkStatus().streams === 0")).unwrap());
        }
    }

    #[test]
    fn sse_oversized_lines_and_unterminated_events_fail_closed_and_reclaim_slots() {
        use std::io::Write;
        use std::net::TcpListener;
        let long_line = "x".repeat(crate::stream_work::SSE_LINE + 1);
        let long_event =
            format!("data:{}\n", "x".repeat(crate::stream_work::SSE_LINE - 6)).repeat(3);
        for body in [long_line, long_event] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                read_stream_test_request(&mut stream);
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",body.len()).unwrap();
                let _ = stream.write_all(body.as_bytes());
            });
            let (mut doc, _) = LapuiDocument::new_with_source(
                ActionRegistry::default(),
                None,
                "<html><body></body></html>",
                &format!(
                    r#"
                globalThis.fatalCode = '';
                globalThis.source = new EventSource('http://{address}/');
                source.onerror = event => {{ if(event.fatal) fatalCode = event.code; }};
            "#
                ),
            )
            .unwrap();
            poll_stream_until(&mut doc, "source.readyState === EventSource.CLOSED");
            server.join().unwrap();
            assert!(doc
                .js_context
                .with(|ctx| ctx.eval::<bool, _>(
                    "fatalCode === 'message_too_large' && lapui.networkStatus().streams === 0"
                ))
                .unwrap());
        }
    }

    #[test]
    fn sse_reconnection_preserves_and_clears_ids_and_named_errors_do_not_change_state() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    request.push_str(&line.to_ascii_lowercase());
                }
                assert_eq!(request.contains("last-event-id: 7\r\n"), index == 1);
                if index == 2 {
                    stream
                        .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                        .unwrap();
                } else {
                    let body = if index == 0 {
                        "retry: 250\nid: 7\nevent: open\ndata: named-open\n\nevent: error\ndata: named-error\n\n"
                    } else {
                        "id:\ndata: reset\n\n"
                    };
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                }
            }
        });
        let (mut doc, _) = LapuiDocument::new_with_source(ActionRegistry::default(), None, "<html><body></body></html>", &format!(r#"
            globalThis.namedStates = [];
            globalThis.resetId = 'missing';
            globalThis.source = new EventSource('http://{address}/');
            source.addEventListener('open', event => {{ if(event.data === 'named-open') namedStates.push(source.readyState); }});
            source.addEventListener('error', event => {{ if(event.data === 'named-error') namedStates.push(source.readyState); }});
            source.onmessage = event => resetId = event.lastEventId;
        "#)).unwrap();
        poll_stream_until(&mut doc, "source.readyState === EventSource.CLOSED");
        server.join().unwrap();
        assert!(doc.js_context.with(|ctx| ctx.eval::<bool,_>("namedStates.length === 2 && namedStates.every(value => value === EventSource.OPEN) && resetId === '' && lapui.networkStatus().streams === 0")).unwrap());
    }

    #[test]
    fn websocket_roundtrips_text_binary_frames_and_close_events() {
        use futures_util::{SinkExt, StreamExt};
        use std::net::TcpListener as StdTcpListener;
        use tokio_tungstenite::tungstenite::Message;

        let probe = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let address = probe.local_addr().unwrap();
        drop(probe);
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(address).await.unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let first = socket.next().await.unwrap().unwrap();
                assert_eq!(first.into_text().unwrap(), "hello");
                socket.send(Message::Text("reply".into())).await.unwrap();
                let second = socket.next().await.unwrap().unwrap();
                assert_eq!(second.into_data().as_ref(), &[1, 2, 255]);
                socket
                    .send(Message::Binary(vec![9, 8, 7].into()))
                    .await
                    .unwrap();
                let _ = socket.next().await;
            });
        });

        let (mut doc, _) = LapuiDocument::new(ActionRegistry::default(), None).unwrap();
        doc.js_context
            .with(|ctx| {
                ctx.eval::<(), _>(format!(
                    r#"
                      globalThis.__wsMessages = [];
                      globalThis.__wsClosed = false;
                      globalThis.__ws = new WebSocket('ws://{address}/echo');
                      __ws.onopen = () => {{ __ws.send('hello'); __ws.send(new Uint8Array([1, 2, 255])); }};
                      __ws.onmessage = event => {{
                        __wsMessages.push(typeof event.data === 'string' ? event.data : Array.from(event.data));
                        if (__wsMessages.length === 2) __ws.close();
                      }};
                      __ws.onclose = () => __wsClosed = true;
                    "#
                ))
            })
            .unwrap();

        let mut messages = Value::Null;
        let mut closed = false;
        for _ in 0..500 {
            doc.poll(None);
            (messages, closed) = doc.js_context.with(|ctx| {
                let messages = ctx
                    .eval::<String, _>("JSON.stringify(__wsMessages)")
                    .ok()
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or(Value::Null);
                let closed = ctx.eval::<bool, _>("__wsClosed").unwrap_or(false);
                (messages, closed)
            });
            if closed {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        server.join().unwrap();
        assert_eq!(messages[0], "reply");
        assert_eq!(messages[1], json!([9, 8, 7]));
        assert!(closed);
    }
}
