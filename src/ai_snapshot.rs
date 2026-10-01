//! Bounded, layout-backed page projection for AI and automation clients.

use blitz::dom::BaseDocument;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

use crate::{
    action::ActionError,
    geometry,
    runtime::{canonical_node_ref, resolve_node_ref},
};

const MAX_PAGE_SIZE: usize = 64;
const DEFAULT_PAGE_SIZE: usize = 32;
const MAX_SCAN_NODES: usize = 8192;
const MAX_TREE_DEPTH: usize = 64;
const MAX_FIELD_TEXT_BYTES: usize = 512;
const MAX_RESPONSE_BYTES: usize = 24 * 1024;

struct Candidate {
    id: blitz::traits::node_id::NodeId,
    parent_ref: Option<String>,
    depth: usize,
}

struct CandidateCollector<'a> {
    doc: &'a BaseDocument,
    after_ref: Option<String>,
    cursor_id: Option<blitz::traits::node_id::NodeId>,
    cursor_ancestors: HashSet<blitz::traits::node_id::NodeId>,
    candidate_limit: usize,
    candidates: Vec<Candidate>,
    cursor_found: bool,
    scanned: usize,
    scan_limit_reached: bool,
}

impl CandidateCollector<'_> {
    fn collect(
        &mut self,
        id: blitz::traits::node_id::NodeId,
        depth: usize,
        parent_ref: Option<String>,
        inherited_hidden: bool,
    ) {
        if self.candidates.len() >= self.candidate_limit || depth > MAX_TREE_DEPTH {
            return;
        }
        if self.scanned >= MAX_SCAN_NODES {
            self.scan_limit_reached = true;
            return;
        }
        self.scanned += 1;
        let Some(node) = self.doc.get_node(id) else {
            return;
        };
        let element = node.element_data();
        let locally_hidden = element.is_some_and(|element| {
            attr(element, "hidden").is_some() || attr(element, "aria-hidden") == Some("true")
        }) || node.primary_styles().is_some_and(|style| {
            style.clone_display().is_none()
                || matches!(
                    style.clone_visibility(),
                    style::computed_values::visibility::T::Hidden
                        | style::computed_values::visibility::T::Collapse
                )
        });
        let hidden = inherited_hidden || locally_hidden;
        let cursor_or_ancestor = self.cursor_id == Some(id) || self.cursor_ancestors.contains(&id);
        let needs_bounds = self.after_ref.is_none() || self.cursor_found || cursor_or_ancestor;
        let visible_box = needs_bounds
            && !hidden
            && geometry::has_boxes(self.doc, id)
            && self
                .doc
                .get_client_bounding_rect(id)
                .is_some_and(|rect| rect.width > 0.0 && rect.height > 0.0);
        let own_ref = visible_box.then(|| canonical_node_ref(self.doc.id(), id));
        if let Some(reference) = own_ref.as_deref() {
            if self.after_ref.as_deref() == Some(reference) {
                self.cursor_found = true;
            } else if self.cursor_found {
                self.candidates.push(Candidate {
                    id,
                    parent_ref: parent_ref.clone(),
                    depth,
                });
            }
        }
        let children = node.children.iter().copied().collect::<Vec<_>>();
        let next_parent = own_ref.or(parent_ref);
        for child in children {
            self.collect(child, depth + 1, next_parent.clone(), hidden);
            if self.candidates.len() >= self.candidate_limit || self.scan_limit_reached {
                break;
            }
        }
    }
}

fn attr<'a>(element: &'a blitz::dom::ElementData, name: &str) -> Option<&'a str> {
    element
        .attrs
        .iter()
        .find(|attribute| attribute.name.local.as_ref() == name)
        .map(|attribute| attribute.value.as_str())
}

fn bounded_text(doc: &BaseDocument, root: blitz::traits::node_id::NodeId) -> String {
    fn visit(
        doc: &BaseDocument,
        id: blitz::traits::node_id::NodeId,
        output: &mut String,
        budget: &mut usize,
        depth: usize,
    ) {
        if *budget == 0 || depth > 12 {
            return;
        }
        let Some(node) = doc.get_node(id) else { return };
        if node.element_data().is_some_and(|element| {
            attr(element, "hidden").is_some() || attr(element, "aria-hidden") == Some("true")
        }) {
            return;
        }
        if node.primary_styles().is_some_and(|style| {
            style.clone_display().is_none()
                || matches!(
                    style.clone_visibility(),
                    style::computed_values::visibility::T::Hidden
                        | style::computed_values::visibility::T::Collapse
                )
        }) {
            return;
        }
        if let blitz::dom::NodeData::Text(text) = &node.data {
            for character in text.content.chars() {
                let bytes = character.len_utf8();
                if bytes > *budget {
                    break;
                }
                output.push(character);
                *budget -= bytes;
            }
            return;
        }
        for child in node.children.iter().copied() {
            visit(doc, child, output, budget, depth + 1);
            if *budget == 0 {
                break;
            }
        }
    }
    let mut output = String::new();
    let mut budget = MAX_FIELD_TEXT_BYTES;
    visit(doc, root, &mut output, &mut budget, 0);
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn semantic_role(tag: &str, explicit: Option<&str>) -> Option<String> {
    explicit.map(str::to_owned).or_else(|| match tag {
        "a" => Some("link".into()),
        "button" => Some("button".into()),
        "main" => Some("main".into()),
        "nav" => Some("navigation".into()),
        "header" => Some("banner".into()),
        "footer" => Some("contentinfo".into()),
        "aside" => Some("complementary".into()),
        "form" => Some("form".into()),
        "ul" | "ol" => Some("list".into()),
        "li" => Some("listitem".into()),
        "table" => Some("table".into()),
        "tr" => Some("row".into()),
        "th" => Some("columnheader".into()),
        "td" => Some("cell".into()),
        "h1" => Some("heading".into()),
        "h2" => Some("heading".into()),
        "h3" => Some("heading".into()),
        "h4" => Some("heading".into()),
        "h5" => Some("heading".into()),
        "h6" => Some("heading".into()),
        "input" | "textarea" => Some("textbox".into()),
        "select" => Some("combobox".into()),
        "img" => Some("img".into()),
        "label" => Some("label".into()),
        "p" => Some("paragraph".into()),
        _ => None,
    })
}

fn page_request_error(code: &str, message: &str) -> ActionError {
    ActionError::new(code, message)
}

/// Build one pre-order page of the rendered DOM. Pagination uses the last
/// returned node ref as a cursor; clients should restart after observed changes.
pub(crate) fn page_snapshot(
    doc: &BaseDocument,
    request: &Value,
    controls_for_nodes: impl FnOnce(&BaseDocument, &[blitz::traits::node_id::NodeId]) -> Value,
) -> Result<Value, ActionError> {
    let object = request.as_object().ok_or_else(|| {
        page_request_error("invalid_request", "pageSnapshot request must be an object")
    })?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "method" | "documentEpoch" | "rootRef" | "afterRef" | "limit"
        )
    }) {
        return Err(page_request_error(
            "invalid_request",
            "unknown pageSnapshot field",
        ));
    }
    if object.get("method").and_then(Value::as_str) != Some("pageSnapshot") {
        return Err(page_request_error(
            "invalid_request",
            "method must be pageSnapshot",
        ));
    }
    if let Some(epoch) = object.get("documentEpoch") {
        if epoch.as_u64() != Some(doc.id() as u64) {
            return Err(page_request_error(
                "stale_document",
                "page snapshot belongs to a different document epoch",
            ));
        }
    }
    let limit = match object.get("limit") {
        None => DEFAULT_PAGE_SIZE,
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=MAX_PAGE_SIZE as u64).contains(value))
            .ok_or_else(|| page_request_error("invalid_request", "limit must be 1..64"))?
            as usize,
    };
    let root_id = match object.get("rootRef") {
        None => doc.root_node().id,
        Some(value) => {
            let reference = value
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= 256)
                .ok_or_else(|| {
                    page_request_error("invalid_request", "rootRef must be 1..256 bytes")
                })?;
            resolve_node_ref(doc, reference).ok_or_else(|| {
                page_request_error(
                    "stale_reference",
                    "page snapshot root is no longer in this document",
                )
            })?
        }
    };
    let root_ref = canonical_node_ref(doc.id(), root_id);

    let after_ref = object
        .get("afterRef")
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= 256)
                .ok_or_else(|| {
                    page_request_error("invalid_request", "afterRef must be 1..256 bytes")
                })
        })
        .transpose()?;
    let cursor_id = after_ref.and_then(|reference| resolve_node_ref(doc, reference));
    let mut cursor_ancestors = HashSet::new();
    let mut ancestor = cursor_id.and_then(|id| doc.get_node(id).and_then(|node| node.parent));
    while let Some(id) = ancestor {
        cursor_ancestors.insert(id);
        if id == root_id {
            break;
        }
        ancestor = doc.get_node(id).and_then(|node| node.parent);
    }
    let mut collector = CandidateCollector {
        doc,
        after_ref: after_ref.map(str::to_owned),
        cursor_id,
        cursor_ancestors,
        candidate_limit: limit.saturating_add(1),
        candidates: Vec::with_capacity(limit.saturating_add(1)),
        cursor_found: after_ref.is_none(),
        scanned: 0,
        scan_limit_reached: false,
    };
    collector.collect(root_id, 0, None, false);
    if !collector.cursor_found {
        return Err(page_request_error(
            "stale_cursor",
            "afterRef is not present in the current rendered subtree; restart observation",
        ));
    }
    let candidates = collector.candidates;
    let scan_limit_reached = collector.scan_limit_reached;
    let candidate_ids = candidates
        .iter()
        .map(|candidate| candidate.id)
        .collect::<Vec<_>>();
    let controls = controls_for_nodes(doc, &candidate_ids);
    let control_by_ref: HashMap<String, Value> = controls
        .get("controls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|control| {
            control
                .get("ref")
                .and_then(Value::as_str)
                .map(|reference| (reference.to_owned(), control.clone()))
        })
        .collect();
    let viewport = doc.viewport();
    let width = f64::from(viewport.window_size.0) / viewport.scale_f64();
    let height = f64::from(viewport.window_size.1) / viewport.scale_f64();
    let end = limit.min(candidates.len());
    let mut items = Vec::new();
    let mut used_text_bytes = 0usize;
    let mut next_after = None;
    let mut truncated_by_bytes = false;
    for candidate in &candidates[..end] {
        let Some(node) = doc.get_node(candidate.id) else {
            continue;
        };
        let Some(element) = node.element_data() else {
            continue;
        };
        let tag = element.name.local.to_string();
        let reference = canonical_node_ref(doc.id(), candidate.id);
        let raw_text = bounded_text(doc, candidate.id);
        let text_budget =
            MAX_FIELD_TEXT_BYTES.min((16 * 1024usize).saturating_sub(used_text_bytes));
        let text: String = raw_text
            .chars()
            .scan(0usize, |bytes, character| {
                let next = *bytes + character.len_utf8();
                if next > text_budget {
                    None
                } else {
                    *bytes = next;
                    Some(character)
                }
            })
            .collect();
        used_text_bytes += text.len();
        let labelled_by = attr(element, "aria-labelledby")
            .map(|ids| {
                ids.split_ascii_whitespace()
                    .filter_map(|id| doc.get_element_by_id(id).and_then(|id| doc.get_node(id)))
                    .map(|node| bounded_text(doc, node.id))
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|text| !text.is_empty());
        let semantic_control = control_by_ref.get(&reference);
        let name = labelled_by
            .or_else(|| {
                semantic_control.and_then(|control| control["name"].as_str().map(str::to_owned))
            })
            .or_else(|| attr(element, "aria-label").map(str::to_owned))
            .or_else(|| attr(element, "alt").map(str::to_owned))
            .or_else(|| attr(element, "title").map(str::to_owned))
            .unwrap_or_else(|| text.clone());
        let rect = doc.get_client_bounding_rect(candidate.id);
        let bounds =
            rect.map(|rect| json!({"x":rect.x,"y":rect.y,"width":rect.width,"height":rect.height}));
        let on_screen = rect.is_some_and(|rect| {
            rect.x < width
                && rect.y < height
                && rect.x + rect.width > 0.0
                && rect.y + rect.height > 0.0
        });
        let mut item = Map::new();
        item.insert("ref".into(), json!(reference));
        if let Some(parent_ref) = &candidate.parent_ref {
            item.insert("parentRef".into(), json!(parent_ref));
        }
        item.insert("depth".into(), json!(candidate.depth));
        item.insert("tag".into(), json!(tag));
        if let Some(role) = semantic_control
            .and_then(|control| control["role"].as_str().map(str::to_owned))
            .or_else(|| semantic_role(&tag, attr(element, "role")))
        {
            item.insert("role".into(), json!(role));
        }
        if !name.is_empty() {
            item.insert(
                "name".into(),
                json!(name.chars().take(512).collect::<String>()),
            );
        }
        if !text.is_empty() {
            item.insert("text".into(), json!(text));
        }
        if let Some(id) = attr(element, "id") {
            item.insert("id".into(), json!(id.chars().take(128).collect::<String>()));
        }
        if let Some(bounds) = bounds {
            item.insert("bounds".into(), bounds);
        }
        item.insert("onScreen".into(), json!(on_screen));
        if let Some(control) = semantic_control {
            item.insert("control".into(), control.clone());
        }
        let item = Value::Object(item);
        items.push(item);
        let response = json!({
            "documentEpoch":doc.id(), "rootRef":root_ref, "items":items,
            "nextAfter":next_after, "truncated":false,
            "scanLimitReached":scan_limit_reached,
            "cursorConsistency":"restart after page mutations"
        });
        if response.to_string().len() > MAX_RESPONSE_BYTES {
            items.pop();
            truncated_by_bytes = true;
            break;
        }
        next_after = Some(reference);
    }
    let has_more = items.len() < candidates.len() || scan_limit_reached;
    let truncated = truncated_by_bytes || scan_limit_reached;
    Ok(json!({
        "documentEpoch":doc.id(), "rootRef":root_ref, "items":items,
        "nextAfter":if has_more { next_after } else { None },
        "truncated":truncated, "scanLimitReached":scan_limit_reached,
        "cursorConsistency":"restart after page mutations"
    }))
}
