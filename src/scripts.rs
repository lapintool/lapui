use blitz::dom::{BaseDocument, LocalName};
use blitz::traits::net::Url;
use blitz::traits::node_id::NodeId;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::{module::Declared, Ctx, Error, Module, Runtime};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const MAX_SCRIPT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 128;
const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_DIAGNOSTIC_SOURCE_BYTES: usize = 4 * 1024;

#[derive(Clone, Serialize)]
pub(crate) struct ScriptDiagnostic {
    pub sequence: u64,
    pub source: String,
    pub phase: String,
    pub message: String,
}

pub(crate) type ScriptDiagnostics = Rc<RefCell<VecDeque<ScriptDiagnostic>>>;

fn bounded_text(text: &str, limit: usize) -> String {
    const SUFFIX: &str = "\n[truncated]";
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit - SUFFIX.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{SUFFIX}", &text[..end])
}

pub(crate) fn report(diagnostics: &ScriptDiagnostics, source: &str, phase: &str, message: String) {
    let source = bounded_text(source, MAX_DIAGNOSTIC_SOURCE_BYTES);
    let phase = bounded_text(phase, 64);
    let message = bounded_text(&message, MAX_DIAGNOSTIC_MESSAGE_BYTES);
    eprintln!("Lapui script {phase} failed for {source}: {message}");
    let mut diagnostics = diagnostics.borrow_mut();
    let sequence = diagnostics
        .back()
        .map_or(1, |entry| entry.sequence.saturating_add(1));
    if diagnostics.len() == MAX_DIAGNOSTICS {
        diagnostics.pop_front();
    }
    diagnostics.push_back(ScriptDiagnostic {
        sequence,
        source,
        phase,
        message,
    });
}

pub(crate) enum StartupScript {
    Classic {
        name: String,
        source: String,
    },
    Module {
        name: String,
        source: Option<String>,
    },
}

impl StartupScript {
    pub fn name(&self) -> &str {
        match self {
            Self::Classic { name, .. } | Self::Module { name, .. } => name,
        }
    }
}

fn local_module_url(root: Option<&Path>, url: &Url) -> Result<Url, String> {
    if url.scheme() != "file" {
        return Err("only app-local file scripts and modules are supported".into());
    }
    let root = root.ok_or("file scripts and modules require a local application directory")?;
    let path = url
        .to_file_path()
        .map_err(|_| "invalid local script URL")?
        .canonicalize()
        .map_err(|error| format!("script file could not be resolved: {error}"))?;
    if !path.starts_with(root) {
        return Err("script path is outside the application directory".into());
    }
    let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_SCRIPT_BYTES {
        return Err("script is not a file or exceeds 32 MiB".into());
    }
    let mut canonical = Url::from_file_path(path).map_err(|_| "invalid canonical script URL")?;
    canonical.set_query(url.query());
    canonical.set_fragment(url.fragment());
    Ok(canonical)
}

fn read_script(root: Option<&Path>, url: &Url) -> Result<String, String> {
    let url = local_module_url(root, url)?;
    let path = url.to_file_path().map_err(|_| "invalid local script URL")?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_SCRIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_SCRIPT_BYTES {
        return Err("script exceeds 32 MiB".into());
    }
    String::from_utf8(bytes).map_err(|error| format!("script is not valid UTF-8: {error}"))
}

// Import specifiers are URLs, not filesystem search paths. Package resolution belongs
// to an application's build tool; the runtime never searches node_modules or its cwd.
struct LocalModuleResolver {
    root: Option<PathBuf>,
    base_url: String,
}

impl Resolver for LocalModuleResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if attributes
            .as_ref()
            .is_some_and(|attrs| attrs.keys().next().is_some())
        {
            return Err(Error::new_resolving_message(
                base,
                name,
                "import attributes are not supported",
            ));
        }
        let result = (|| {
            let url = if let Ok(url) = Url::parse(name) {
                url
            } else if name.starts_with("./") || name.starts_with("../") || name.starts_with('/') {
                Url::parse(base)
                    .or_else(|_| Url::parse(&self.base_url))
                    .and_then(|base| base.join(name))
                    .map_err(|error| error.to_string())?
            } else {
                return Err(
                    "bare module specifiers are unsupported; use relative URLs or a bundler".into(),
                );
            };
            local_module_url(self.root.as_deref(), &url).map(|url| url.to_string())
        })();
        result.map_err(|message: String| Error::new_resolving_message(base, name, message))
    }
}

struct LocalModuleLoader {
    root: Option<PathBuf>,
}

impl Loader for LocalModuleLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        if attributes
            .as_ref()
            .is_some_and(|attrs| attrs.keys().next().is_some())
        {
            return Err(Error::new_loading_message(
                name,
                "import attributes are not supported",
            ));
        }
        let source = Url::parse(name)
            .map_err(|error| error.to_string())
            .and_then(|url| read_script(self.root.as_deref(), &url))
            .map_err(|message| Error::new_loading_message(name, message))?;
        let module = Module::declare(ctx.clone(), name, source)?;
        module.meta()?.set("url", name)?;
        Ok(module)
    }
}

pub(crate) fn install_loader(runtime: &Runtime, root: Option<PathBuf>, base_url: String) {
    runtime.set_loader(
        LocalModuleResolver {
            root: root.clone(),
            base_url,
        },
        LocalModuleLoader { root },
    );
}

pub(crate) fn html_scripts(
    doc: &BaseDocument,
    root: Option<&Path>,
    diagnostics: &ScriptDiagnostics,
) -> Vec<StartupScript> {
    fn visit(
        doc: &BaseDocument,
        id: NodeId,
        root: Option<&Path>,
        diagnostics: &ScriptDiagnostics,
        classic: &mut Vec<StartupScript>,
        modules: &mut Vec<StartupScript>,
        ordinal: &mut usize,
    ) {
        let Some(node) = doc.get_node(id) else { return };
        if let Some(element) = node.data.downcast_element() {
            if element.name.local.as_ref() == "script" {
                *ordinal += 1;
                let kind = element
                    .attr(LocalName::from("type"))
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                let is_module = kind == "module";
                if !is_module
                    && !matches!(
                        kind.as_str(),
                        "" | "text/javascript" | "application/javascript"
                    )
                {
                    return;
                }
                if !is_module && element.attr(LocalName::from("nomodule")).is_some() {
                    return;
                }
                if element.attr(LocalName::from("async")).is_some()
                    || (!is_module && element.attr(LocalName::from("defer")).is_some())
                {
                    eprintln!("Lapui executes classic scripts in document order and defers module initiation until after them; async/defer scheduling is incomplete");
                }
                let name = format!("{}#inline-script-{}", doc.base_url(), ordinal);
                let script = if let Some(src) = element.attr(LocalName::from("src")) {
                    let result = doc
                        .base_url()
                        .join(src)
                        .map_err(|error| error.to_string())
                        .and_then(|url| local_module_url(root, &url));
                    match result {
                        Ok(url) if is_module => StartupScript::Module {
                            name: url.to_string(),
                            source: None,
                        },
                        Ok(url) => match read_script(root, &url) {
                            Ok(source) => StartupScript::Classic {
                                name: url.to_string(),
                                source,
                            },
                            Err(message) => {
                                report(diagnostics, url.as_str(), "load", message);
                                return;
                            }
                        },
                        Err(message) => {
                            report(diagnostics, src, "load", message);
                            return;
                        }
                    }
                } else if is_module {
                    StartupScript::Module {
                        name,
                        source: Some(node.text_content()),
                    }
                } else {
                    StartupScript::Classic {
                        name,
                        source: node.text_content(),
                    }
                };
                if is_module {
                    modules.push(script);
                } else {
                    classic.push(script);
                }
                return;
            }
        }
        for child in node.children.iter().copied() {
            visit(doc, child, root, diagnostics, classic, modules, ordinal);
        }
    }
    let mut classic = Vec::new();
    let mut modules = Vec::new();
    visit(
        doc,
        doc.root_node().id,
        root,
        diagnostics,
        &mut classic,
        &mut modules,
        &mut 0,
    );
    classic.extend(modules);
    classic
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_diagnostics_drop_old_entries_and_bound_utf8_payloads() {
        let diagnostics: ScriptDiagnostics = Rc::new(RefCell::new(Default::default()));
        for index in 0..130 {
            report(
                &diagnostics,
                &format!("script-{index}"),
                "evaluate",
                "failure".into(),
            );
        }
        {
            let buffer = diagnostics.borrow();
            assert_eq!(buffer.len(), MAX_DIAGNOSTICS);
            assert_eq!(buffer.front().unwrap().source, "script-2");
            assert_eq!(buffer.back().unwrap().source, "script-129");
            assert_eq!(buffer.front().unwrap().sequence, 3);
            assert_eq!(buffer.back().unwrap().sequence, 130);
        }
        report(
            &diagnostics,
            &"路径".repeat(2000),
            &"处理".repeat(100),
            "中文错误".repeat(5000),
        );
        let buffer = diagnostics.borrow();
        let newest = buffer.back().unwrap();
        assert_eq!(buffer.len(), MAX_DIAGNOSTICS);
        assert!(newest.source.len() <= MAX_DIAGNOSTIC_SOURCE_BYTES);
        assert!(newest.message.len() <= MAX_DIAGNOSTIC_MESSAGE_BYTES);
        assert!(newest.phase.len() <= 64);
        assert!(newest.source.ends_with("[truncated]"));
        assert!(newest.message.ends_with("[truncated]"));
        assert!(serde_json::to_string(&*buffer).is_ok());
    }
}
