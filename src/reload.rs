//! Full document replacement for local development; application state survives.

use crate::action::{ActionError, ActionRegistry};
use crate::control::{self, DocumentController, DocumentRequest};
use crate::runtime::LapuiDocument;
use crate::watch::{WatchSignal, WatchTarget};
use blitz::dom::FontContext;
use blitz::dom::{DocGuard, DocGuardMut, Document};
use blitz::shell::{BlitzShellEvent, BlitzShellProxy};
use blitz::traits::events::UiEvent;
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::task::{Context, Waker};
use std::time::Duration;

const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone)]
pub enum DocumentSource {
    Embedded {
        html: Arc<str>,
        script: Arc<str>,
    },
    EmbeddedWithScript {
        html: Arc<str>,
        script: PathBuf,
    },
    Local {
        html: PathBuf,
        script: Option<PathBuf>,
    },
}

fn read_source(path: &std::path::Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(format!("{} exceeds 32 MiB", path.display()));
    }
    String::from_utf8(bytes).map_err(|error| format!("{} is not UTF-8: {error}", path.display()))
}

impl DocumentSource {
    pub fn load(
        &self,
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
    ) -> Result<(LapuiDocument, mpsc::Sender<Result<Value, String>>), String> {
        self.load_with_font_context(actions, proxy, LapuiDocument::new_font_context())
    }

    pub fn load_with_font_context(
        &self,
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
        font_context: FontContext,
    ) -> Result<(LapuiDocument, mpsc::Sender<Result<Value, String>>), String> {
        match self {
            Self::Embedded { html, script } => LapuiDocument::new_with_source_and_font_context(
                actions,
                proxy,
                html,
                script,
                font_context,
            ),
            Self::EmbeddedWithScript { html, script } => {
                LapuiDocument::new_with_source_and_font_context(
                    actions,
                    proxy,
                    html,
                    &read_source(script)?,
                    font_context,
                )
            }
            Self::Local { html, script } => {
                let html_file = html.canonicalize().map_err(|error| error.to_string())?;
                let app_root = html_file.parent().ok_or("HTML path has no parent")?;
                let html_text = read_source(&html_file)?;
                let script_text = script
                    .as_deref()
                    .map(read_source)
                    .transpose()?
                    .unwrap_or_default();
                LapuiDocument::new_with_local_source_and_font_context(
                    actions,
                    proxy,
                    &html_text,
                    &script_text,
                    app_root,
                    font_context,
                )
            }
        }
    }
}

/// Current document endpoint. Old clones close on a successful reload.
#[derive(Clone)]
pub struct DocumentEndpoint {
    pub controller: DocumentController,
    pub notify: mpsc::Sender<Result<Value, String>>,
}

#[derive(Clone)]
pub struct ReloadHandle {
    endpoint: Arc<Mutex<DocumentEndpoint>>,
    reload: DocumentController,
    status: Arc<Mutex<Value>>,
}

impl ReloadHandle {
    pub fn endpoint(&self) -> DocumentEndpoint {
        self.endpoint.lock().unwrap().clone()
    }

    /// Blocks the calling background client, using the bounded control queue.
    /// Requires the current document epoch; a completed retry is rejected as stale.
    pub fn reload(&self, request: Value, timeout: Duration) -> Result<Value, ActionError> {
        self.reload.request(request, timeout)
    }

    pub fn status(&self) -> Value {
        self.status.lock().unwrap().clone()
    }
}

pub struct ReloadDocument {
    document: LapuiDocument,
    source: DocumentSource,
    font_context: FontContext,
    actions: ActionRegistry,
    proxy: Option<BlitzShellProxy>,
    endpoint: Arc<Mutex<DocumentEndpoint>>,
    reload_requests: mpsc::Receiver<DocumentRequest>,
    epoch: Arc<AtomicUsize>,
    waker: Arc<Mutex<Option<Waker>>>,
    watch: Option<WatchSignal>,
    status: Arc<Mutex<Value>>,
}

impl ReloadDocument {
    pub(crate) fn document_for_frame(&mut self) -> &mut LapuiDocument {
        &mut self.document
    }
    pub fn new(
        document: LapuiDocument,
        notify: mpsc::Sender<Result<Value, String>>,
        source: DocumentSource,
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
    ) -> (Self, ReloadHandle) {
        Self::new_with_font_context(
            document,
            notify,
            source,
            actions,
            proxy,
            LapuiDocument::new_font_context(),
        )
    }

    pub fn new_with_font_context(
        document: LapuiDocument,
        notify: mpsc::Sender<Result<Value, String>>,
        source: DocumentSource,
        actions: ActionRegistry,
        proxy: Option<BlitzShellProxy>,
        font_context: FontContext,
    ) -> (Self, ReloadHandle) {
        let epoch = Arc::new(AtomicUsize::new(document.inner().id()));
        let waker: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let reload_epoch = epoch.clone();
        let reload_waker = waker.clone();
        let reload_proxy = proxy.clone();
        let (reload, reload_requests) = control::channel(move || {
            if let Some(waker) = reload_waker.lock().unwrap().as_ref() {
                waker.wake_by_ref();
            } else if let Some(proxy) = &reload_proxy {
                proxy.send_event(BlitzShellEvent::RequestRedraw {
                    doc_id: reload_epoch.load(Ordering::Acquire),
                });
            }
        });
        let endpoint = Arc::new(Mutex::new(DocumentEndpoint {
            controller: document.controller(),
            notify,
        }));
        let status = Arc::new(Mutex::new(
            json!({"documentEpoch":epoch.load(Ordering::Acquire),"watching":false,"lastReload":null}),
        ));
        let handle = ReloadHandle {
            endpoint: endpoint.clone(),
            reload,
            status: status.clone(),
        };
        (
            Self {
                document,
                source,
                font_context,
                actions,
                proxy,
                endpoint,
                reload_requests,
                epoch,
                waker,
                watch: None,
                status,
            },
            handle,
        )
    }

    /// Watch the local application directory, plus an external bundle's file.
    /// Native notifications trigger full replacement after 150 ms of quiet.
    pub fn watch(&mut self) -> Result<(), String> {
        let (root, script) = match &self.source {
            DocumentSource::Local { html, script } => (
                Some(
                    html.canonicalize()
                        .map_err(|error| error.to_string())?
                        .parent()
                        .ok_or("HTML path has no parent")?
                        .to_path_buf(),
                ),
                script.as_ref(),
            ),
            DocumentSource::EmbeddedWithScript { script, .. } => (None, Some(script)),
            DocumentSource::Embedded { .. } => {
                return Err("--watch requires a local HTML page or script".into())
            }
        };
        let mut targets = Vec::new();
        if let Some(directory) = &root {
            targets.push(WatchTarget {
                directory: directory.clone(),
                file: None,
            });
        }
        if let Some(script) = script {
            let file = script.canonicalize().map_err(|error| error.to_string())?;
            if root.as_ref().is_none_or(|root| !file.starts_with(root)) {
                targets.push(WatchTarget {
                    directory: file
                        .parent()
                        .ok_or("script path has no parent")?
                        .to_path_buf(),
                    file: Some(file),
                });
            }
        }
        let waker = self.waker.clone();
        let epoch = self.epoch.clone();
        let proxy = self.proxy.clone();
        self.watch = Some(WatchSignal::new(targets, move || {
            if let Some(waker) = waker.lock().unwrap().as_ref() {
                waker.wake_by_ref();
            } else if let Some(proxy) = &proxy {
                proxy.send_event(BlitzShellEvent::RequestRedraw {
                    doc_id: epoch.load(Ordering::Acquire),
                });
            }
        })?);
        self.status.lock().unwrap()["watching"] = json!(true);
        Ok(())
    }

    fn record_reload(&self, result: &Result<Value, ActionError>, trigger: &str) {
        let mut status = self.status.lock().unwrap();
        status["documentEpoch"] = json!(self.document.inner().id());
        status["lastReload"] = match result {
            Ok(result) => json!({"ok":true,"trigger":trigger,"result":result}),
            Err(error) => json!({"ok":false,"trigger":trigger,"error":error}),
        };
    }

    fn replace(&mut self, request: &Value) -> Result<Value, ActionError> {
        let invalid = |message| ActionError::new("invalid_request", message);
        let object = request
            .as_object()
            .ok_or_else(|| invalid("reload requires an object"))?;
        if object.get("method").and_then(Value::as_str) != Some("reload")
            || object
                .keys()
                .any(|key| !matches!(key.as_str(), "method" | "documentEpoch"))
        {
            return Err(invalid("reload accepts only method and documentEpoch"));
        }
        let expected = object
            .get("documentEpoch")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid("reload requires the current documentEpoch"))?;
        let previous = self.document.inner().id();
        if expected != previous as u64 {
            return Err(ActionError::new(
                "stale_document",
                "reload belongs to an older document",
            ));
        }
        // Read and construct before replacing the usable document. Script errors
        // are represented by the new document's diagnostics, as on first launch.
        let (mut new_document, notify) = self
            .source
            .load_with_font_context(
                self.actions.clone(),
                self.proxy.clone(),
                self.font_context.clone(),
            )
            .map_err(|message| ActionError::new("reload_failed", message))?;
        let old = self.document.inner();
        let viewport = old.viewport().clone();
        let provider = old.shell_provider.clone();
        drop(old);
        {
            let mut inner = new_document.inner_mut();
            inner.set_viewport(viewport);
            inner.set_shell_provider(provider.clone());
        }
        new_document.configure_debug_trace(self.document.debug_trace_enabled(), false);
        let current = new_document.inner().id();
        let new_endpoint = DocumentEndpoint {
            controller: new_document.controller(),
            notify,
        };
        provider.set_ime_enabled(false);
        provider.set_cursor(Some(Default::default()));
        self.document = new_document;
        self.epoch.store(current, Ordering::Release);
        *self.endpoint.lock().unwrap() = new_endpoint;
        provider.request_redraw();
        Ok(
            json!({"previousDocumentEpoch":previous, "documentEpoch":current,
            "execution":"completed", "applicationVersion":self.actions.observe().version}),
        )
    }
}

impl Document for ReloadDocument {
    fn inner(&self) -> DocGuard<'_> {
        self.document.inner()
    }
    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        self.document.inner_mut()
    }
    fn handle_ui_event(&mut self, event: UiEvent) {
        self.document.handle_ui_event(event);
    }

    fn poll(&mut self, context: Option<Context>) -> bool {
        if let Some(context) = &context {
            *self.waker.lock().unwrap() = Some(context.waker().clone());
        }
        // At most one replacement per poll. Wake once more so queued requests
        // cannot be stranded if their wake events were coalesced by the OS.
        let mut replaced = false;
        let mut client_processed = false;
        if let Ok(request) = self.reload_requests.try_recv() {
            client_processed = true;
            if request.start() {
                let result = self.replace(&request.command);
                self.record_reload(&result, "client");
                replaced = result.is_ok();
                request.finish(result);
            }
            if let Some(waker) = self.waker.lock().unwrap().as_ref() {
                waker.wake_by_ref();
            }
        }
        if !client_processed
            && self
                .watch
                .as_ref()
                .is_some_and(|watch| watch.ready.swap(false, Ordering::AcqRel))
        {
            let result = self
                .replace(&json!({"method":"reload","documentEpoch":self.document.inner().id()}));
            replaced |= result.is_ok();
            if let Err(error) = &result {
                eprintln!("Lapui watched reload failed: {}", error.message);
            }
            self.record_reload(&result, "file");
        }
        self.document.poll(context) || replaced
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz::traits::shell::{ColorScheme, Viewport};
    use std::time::Instant;

    #[test]
    fn reload_recovers_suspended_script_with_new_epoch_and_preserved_application_state() {
        let path = std::env::temp_dir().join(format!(
            "lapui-script-recovery-{}-{}.html",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let broken = "<html><body><button id='run'>Run</button><p id='status' role='status'>waiting</p><script>document.getElementById('run').onclick=()=>{while(true){}};</script></body></html>";
        std::fs::write(&path, broken).unwrap();
        let source = DocumentSource::Local {
            html: path.clone(),
            script: None,
        };
        let actions = ActionRegistry::default();
        let (document, notify) = source.load(actions.clone(), None).unwrap();
        let (mut document, handle) =
            ReloadDocument::new(document, notify, source, actions.clone(), None);
        let old_epoch = document.inner().id();
        let old = handle.endpoint();
        let call =
            |document: &mut ReloadDocument, controller: DocumentController, command: Value| {
                let caller =
                    std::thread::spawn(move || controller.request(command, Duration::from_secs(3)));
                let deadline = Instant::now() + Duration::from_secs(4);
                while !caller.is_finished() && Instant::now() < deadline {
                    document.poll(None);
                    std::thread::sleep(Duration::from_millis(1));
                }
                caller.join().unwrap()
            };
        let reference = crate::runtime::canonical_node_ref(
            old_epoch,
            document.inner().get_element_by_id("run").unwrap(),
        );
        assert_eq!(
            call(
                &mut document,
                old.controller.clone(),
                json!({"method":"activate","documentEpoch":old_epoch,"ref":reference})
            )
            .unwrap_err()
            .code,
            "script_error"
        );
        assert_eq!(
            call(
                &mut document,
                old.controller.clone(),
                json!({"method":"diagnostics"})
            )
            .unwrap()["scriptStatus"],
            "suspended"
        );
        actions.invoke("counter.increment", &json!({})).unwrap();
        std::fs::write(
            &path,
            broken.replace(
                "while(true){}",
                "document.getElementById('status').textContent='recovered'",
            ),
        )
        .unwrap();
        let reloaded = request(
            &mut document,
            &handle,
            json!({"method":"reload","documentEpoch":old_epoch}),
        )
        .unwrap();
        let epoch = document.inner().id();
        assert_ne!(old_epoch, epoch);
        assert_eq!(reloaded["applicationVersion"], 1);
        assert_eq!(
            old.controller
                .request(json!({"method":"controls"}), Duration::from_millis(100))
                .unwrap_err()
                .code,
            "document_closed"
        );
        let current = handle.endpoint().controller;
        let diagnostics = call(
            &mut document,
            current.clone(),
            json!({"method":"diagnostics"}),
        )
        .unwrap();
        assert_eq!(diagnostics["scriptStatus"], "running");
        assert!(diagnostics["errors"].as_array().unwrap().is_empty());
        let reference = crate::runtime::canonical_node_ref(
            epoch,
            document.inner().get_element_by_id("run").unwrap(),
        );
        call(
            &mut document,
            current,
            json!({"method":"activate","documentEpoch":epoch,"ref":reference}),
        )
        .unwrap();
        assert_eq!(
            document
                .inner()
                .get_node(document.inner().get_element_by_id("status").unwrap())
                .unwrap()
                .text_content(),
            "recovered"
        );
        drop(document);
        std::fs::remove_file(path).unwrap();
    }

    fn request(
        document: &mut ReloadDocument,
        handle: &ReloadHandle,
        command: Value,
    ) -> Result<Value, ActionError> {
        let handle = handle.clone();
        let caller = std::thread::spawn(move || handle.reload(command, Duration::from_secs(2)));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !caller.is_finished() && Instant::now() < deadline {
            document.poll(None);
            std::thread::sleep(Duration::from_millis(1));
        }
        caller.join().unwrap()
    }

    #[test]
    fn reload_preserves_trace_policy_but_discards_records_and_rejects_old_trace_epoch() {
        let actions = ActionRegistry::default();
        let source = DocumentSource::Embedded {
            html: "<html><body><button id='run'>Run</button></body></html>".into(),
            script: "".into(),
        };
        let (document, notify) = source.load(actions.clone(), None).unwrap();
        document.configure_debug_trace(true, false);
        let (mut document, handle) = ReloadDocument::new(document, notify, source, actions, None);
        let epoch = document.inner().id();
        let reference = crate::runtime::canonical_node_ref(
            epoch,
            document.inner().get_element_by_id("run").unwrap(),
        );
        let controller = handle.endpoint().controller;
        let caller = std::thread::spawn(move || {
            controller.request(
                json!({"method":"activate","documentEpoch":epoch,"ref":reference}),
                Duration::from_secs(2),
            )
        });
        while !caller.is_finished() {
            document.poll(None);
            std::thread::sleep(Duration::from_millis(1));
        }
        caller.join().unwrap().unwrap();
        assert!(
            document.document.debug_trace(0, 128).unwrap()["latestSequence"]
                .as_u64()
                .unwrap()
                > 0
        );
        request(
            &mut document,
            &handle,
            json!({"method":"reload","documentEpoch":epoch}),
        )
        .unwrap();
        assert!(document.document.debug_trace_enabled());
        assert_eq!(
            document.document.debug_trace(0, 128).unwrap()["latestSequence"],
            0
        );
        let controller = handle.endpoint().controller;
        let caller = std::thread::spawn(move || {
            controller.request(
                json!({"method":"debugTrace.read","documentEpoch":epoch,"afterSequence":1}),
                Duration::from_secs(2),
            )
        });
        while !caller.is_finished() {
            document.poll(None);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(caller.join().unwrap().unwrap_err().code, "stale_document");
        document.document.configure_debug_trace(false, false);
        let epoch = document.inner().id();
        request(
            &mut document,
            &handle,
            json!({"method":"reload","documentEpoch":epoch}),
        )
        .unwrap();
        assert!(!document.document.debug_trace_enabled());
    }

    #[test]
    fn reload_preserves_app_state_and_viewport_but_invalidates_old_document_channels() {
        let actions = ActionRegistry::default();
        let source = DocumentSource::Embedded {
            html: "<html><body><button id='run'>Run</button></body></html>".into(),
            script: "".into(),
        };
        let (document, notify) = source.load(actions.clone(), None).unwrap();
        let (mut document, handle) =
            ReloadDocument::new(document, notify, source, actions.clone(), None);
        document
            .inner_mut()
            .set_viewport(Viewport::new(840, 620, 1.5, ColorScheme::Light));
        actions.invoke("counter.increment", &json!({})).unwrap();
        for _ in 0..20 {
            document
                .document
                .create_action_scope("form")
                .unwrap()
                .register_query(
                    crate::action::ActionInfo {
                        id: "form.read".into(),
                        description: "Transient form action".into(),
                        input_schema: json!({"type":"object"}),
                        output_schema: json!({"type":"string"}),
                        kind: crate::action::ActionKind::Read,
                    },
                    crate::action_catalog::ActionOptions::default(),
                    |_, _| Ok(json!("form")),
                )
                .unwrap();
            assert!(actions.describe_action("form.read").is_ok());
            let epoch = document.inner().id();
            let old = handle.endpoint();
            let reference = crate::runtime::canonical_node_ref(
                epoch,
                document.inner().get_element_by_id("run").unwrap(),
            );
            let result = request(
                &mut document,
                &handle,
                json!({"method":"reload","documentEpoch":epoch}),
            )
            .unwrap();
            assert_ne!(result["documentEpoch"], json!(epoch));
            assert_eq!(
                actions.describe_action("form.read").unwrap_err().code,
                "unknown_action"
            );
            assert_eq!(result["applicationVersion"], 1);
            assert_eq!(document.inner().viewport().window_size, (840, 620));
            assert_eq!(
                old.controller
                    .request(json!({"method":"controls"}), Duration::from_secs(1))
                    .unwrap_err()
                    .code,
                "document_closed"
            );
            assert_eq!(
                request(
                    &mut document,
                    &handle,
                    json!({"method":"reload","documentEpoch":epoch})
                )
                .unwrap_err()
                .code,
                "stale_document"
            );
            let current = handle.endpoint().controller;
            let caller = std::thread::spawn(move || {
                current.request(
                    json!({"method":"activate","documentEpoch":epoch,"ref":reference}),
                    Duration::from_secs(1),
                )
            });
            while !caller.is_finished() {
                document.poll(None);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(caller.join().unwrap().unwrap_err().code, "stale_document");
        }
        assert_eq!(actions.observe().count, 1);
        drop(document);
        assert_eq!(
            handle
                .reload(
                    json!({"method":"reload","documentEpoch":0}),
                    Duration::from_secs(1)
                )
                .unwrap_err()
                .code,
            "document_closed"
        );
    }

    #[test]
    fn reload_discards_old_animation_callbacks_and_runs_only_the_replacement_frame() {
        let actions = ActionRegistry::default();
        let source = DocumentSource::Embedded {
            html:"<html><body><button id='run'>initial</button></body></html>".into(),
            script:"requestAnimationFrame(() => document.getElementById('run').textContent = 'new frame');".into(),
        };
        let (document, notify) = LapuiDocument::new_with_source(actions.clone(),None,
            "<html><body><button id='run'>old</button></body></html>",
            "requestAnimationFrame(() => { document.getElementById('run').textContent = 'old frame'; lapui.invoke('counter.increment', {}); });").unwrap();
        let (mut document, handle) =
            ReloadDocument::new(document, notify, source, actions.clone(), None);
        assert!(document.document_for_frame().has_animation_callbacks());
        let old_epoch = document.inner().id();
        request(
            &mut document,
            &handle,
            json!({"method":"reload","documentEpoch":old_epoch}),
        )
        .unwrap();
        assert!(document.document_for_frame().has_animation_callbacks());
        document.document_for_frame().animation_frame();
        assert!(!document.document_for_frame().has_animation_callbacks());
        let inner = document.inner();
        assert_eq!(
            inner
                .get_node(inner.get_element_by_id("run").unwrap())
                .unwrap()
                .text_content(),
            "new frame"
        );
        assert_eq!(actions.observe().count, 0);
    }

    #[test]
    fn failed_local_read_keeps_current_document_and_module_cache_restarts_after_reload() {
        let root = std::env::temp_dir().join(format!(
            "lapui-reload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let html = root.join("index.html");
        let module = root.join("value.mjs");
        std::fs::write(&html, "<html><body><p id='value'></p><script type='module'>import {value} from './value.mjs';document.getElementById('value').textContent=value;</script></body></html>").unwrap();
        std::fs::write(&module, "export const value = 'first';").unwrap();
        let actions = ActionRegistry::default();
        let source = DocumentSource::Local {
            html: html.clone(),
            script: None,
        };
        let (document, notify) = source.load(actions.clone(), None).unwrap();
        let (mut document, handle) = ReloadDocument::new(document, notify, source, actions, None);
        let epoch = document.inner().id();
        std::fs::rename(&html, root.join("saved.html")).unwrap();
        assert_eq!(
            request(
                &mut document,
                &handle,
                json!({"method":"reload","documentEpoch":epoch})
            )
            .unwrap_err()
            .code,
            "reload_failed"
        );
        assert_eq!(document.inner().id(), epoch);
        std::fs::rename(root.join("saved.html"), &html).unwrap();
        std::fs::write(&module, "export const value = 'second';").unwrap();
        request(
            &mut document,
            &handle,
            json!({"method":"reload","documentEpoch":epoch}),
        )
        .unwrap();
        document.poll(None);
        let inner = document.inner();
        assert_eq!(
            inner
                .get_node(inner.get_element_by_id("value").unwrap())
                .unwrap()
                .text_content(),
            "second"
        );
        drop(inner);
        drop(document);
        std::fs::remove_file(&html).unwrap();
        std::fs::remove_file(&module).unwrap();
        std::fs::remove_file(root.join("saved.html")).ok();
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn watched_files_reload_report_failures_and_recover_without_idle_reload_loops() {
        let root = std::env::temp_dir().join(format!(
            "lapui-watch-reload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let html = root.join("index.html");
        let content = |value| format!("<html><body><p id='value'>{value}</p></body></html>");
        std::fs::write(&html, content("first")).unwrap();
        let actions = ActionRegistry::default();
        let source = DocumentSource::Local {
            html: html.clone(),
            script: None,
        };
        let (document, notify) = source.load(actions.clone(), None).unwrap();
        let (mut document, handle) = ReloadDocument::new(document, notify, source, actions, None);
        document.watch().unwrap();
        let epoch = document.inner().id();
        std::fs::write(&html, content("second")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while document.inner().id() == epoch && Instant::now() < deadline {
            document.poll(None);
            std::thread::sleep(Duration::from_millis(5));
        }
        let current = document.inner().id();
        assert_ne!(current, epoch);
        assert_eq!(handle.status()["lastReload"]["trigger"], "file");
        assert_eq!(handle.status()["lastReload"]["ok"], true);
        std::fs::remove_file(&html).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while handle.status()["lastReload"]["ok"] != false && Instant::now() < deadline {
            document.poll(None);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            handle.status()["lastReload"]["error"]["code"],
            "reload_failed"
        );
        assert_eq!(document.inner().id(), current);
        std::fs::write(&html, content("recovered")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while document.inner().id() == current && Instant::now() < deadline {
            document.poll(None);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_ne!(document.inner().id(), current);
        assert_eq!(handle.status()["lastReload"]["ok"], true);
        let inner = document.inner();
        assert_eq!(
            inner
                .get_node(inner.get_element_by_id("value").unwrap())
                .unwrap()
                .text_content(),
            "recovered"
        );
        drop(inner);
        std::thread::sleep(Duration::from_millis(250));
        assert!(!document.poll(None));
        drop(document);
        std::fs::remove_file(html).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
