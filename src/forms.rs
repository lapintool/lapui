//! Form ownership and checked state over the authoritative native DOM.
use blitz::dom::{node::SpecialElementData, BaseDocument, LocalName};
use blitz::traits::node_id::NodeId;
use std::collections::HashMap;

fn attr<'a>(doc: &'a BaseDocument, id: NodeId, name: &str) -> Option<&'a str> {
    doc.get_node(id)?
        .data
        .downcast_element()?
        .attr(LocalName::from(name))
}

fn tag(doc: &BaseDocument, id: NodeId, name: &str) -> bool {
    doc.get_node(id)
        .and_then(|node| node.data.downcast_element())
        .is_some_and(|element| element.name.local.as_ref() == name)
}

pub(crate) fn value(doc: &BaseDocument, id: NodeId) -> Option<String> {
    let element = doc.get_node(id)?.data.downcast_element()?;
    element
        .text_input_data()
        .map(|input| input.editor.text().to_string())
        .or_else(|| attr(doc, id, "value").map(str::to_owned))
        .or_else(|| {
            (tag(doc, id, "input")
                && attr(doc, id, "type").is_some_and(|kind| {
                    kind.eq_ignore_ascii_case("checkbox") || kind.eq_ignore_ascii_case("radio")
                }))
            .then(|| "on".to_owned())
        })
}

pub(crate) fn supports_value_write(doc: &BaseDocument, id: NodeId) -> bool {
    tag(doc, id, "textarea")
        || (tag(doc, id, "input")
            && matches!(
                attr(doc, id, "type")
                    .unwrap_or("text")
                    .to_ascii_lowercase()
                    .as_str(),
                "text"
                    | "password"
                    | "email"
                    | "number"
                    | "search"
                    | "tel"
                    | "url"
                    | "checkbox"
                    | "radio"
                    | "button"
                    | "submit"
                    | "reset"
                    | "hidden"
            ))
}

fn root(doc: &BaseDocument, mut id: NodeId) -> NodeId {
    while let Some(parent) = doc.get_node(id).and_then(|node| node.parent) {
        id = parent;
    }
    id
}

fn subtree(doc: &BaseDocument, id: NodeId) -> Vec<NodeId> {
    let mut output = Vec::new();
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        if let Some(node) = doc.get_node(id) {
            output.push(id);
            stack.extend(node.children.iter().rev().copied());
        }
    }
    output
}

fn tree_ids<'a>(doc: &'a BaseDocument, nodes: &[NodeId]) -> HashMap<&'a str, NodeId> {
    let mut ids = HashMap::new();
    for node in nodes {
        if let Some(name) = attr(doc, *node, "id") {
            ids.entry(name).or_insert(*node);
        }
    }
    ids
}

fn form_owner(doc: &BaseDocument, id: NodeId, ids: &HashMap<&str, NodeId>) -> Option<NodeId> {
    if let Some(name) = attr(doc, id, "form") {
        let found = *ids.get(name)?;
        return tag(doc, found, "form").then_some(found);
    }
    let mut parent = doc.get_node(id)?.parent;
    while let Some(id) = parent {
        if tag(doc, id, "form") {
            return Some(id);
        }
        parent = doc.get_node(id)?.parent;
    }
    None
}

pub(crate) fn label_control(doc: &BaseDocument, id: NodeId) -> Option<NodeId> {
    let nodes = subtree(doc, root(doc, id));
    label_control_with_ids(doc, id, &tree_ids(doc, &nodes))
}

fn label_control_with_ids(
    doc: &BaseDocument,
    id: NodeId,
    ids: &HashMap<&str, NodeId>,
) -> Option<NodeId> {
    let labelable = |node| {
        ["input", "button", "select", "textarea"]
            .iter()
            .any(|name| tag(doc, node, name))
            && !(tag(doc, node, "input")
                && attr(doc, node, "type")
                    .is_some_and(|value| value.eq_ignore_ascii_case("hidden")))
    };
    if let Some(name) = attr(doc, id, "for") {
        let found = *ids.get(name)?;
        return labelable(found).then_some(found);
    }
    subtree(doc, id)
        .into_iter()
        .skip(1)
        .find(|node| labelable(*node))
}

pub(crate) fn label_names(doc: &BaseDocument) -> HashMap<NodeId, String> {
    let nodes = subtree(doc, doc.root_node().id);
    let ids = tree_ids(doc, &nodes);
    let mut labels = HashMap::<NodeId, String>::new();
    for node in nodes
        .iter()
        .copied()
        .filter(|node| tag(doc, *node, "label"))
    {
        let Some(control) = label_control_with_ids(doc, node, &ids) else {
            continue;
        };
        let text = doc.get_node(node).unwrap().text_content().trim().to_owned();
        if text.is_empty() {
            continue;
        }
        let combined = labels.entry(control).or_default();
        if !combined.is_empty() {
            combined.push(' ');
        }
        combined.push_str(&text);
    }
    labels
}

pub(crate) fn checked(doc: &BaseDocument, id: NodeId) -> Option<bool> {
    if !tag(doc, id, "input")
        || !matches!(
            attr(doc, id, "type")
                .unwrap_or("text")
                .to_ascii_lowercase()
                .as_str(),
            "checkbox" | "radio"
        )
    {
        return None;
    }
    Some(
        doc.get_node(id)?
            .data
            .downcast_element()?
            .checkbox_input_checked()
            .unwrap_or_else(|| attr(doc, id, "checked").is_some()),
    )
}

pub(crate) fn radio_group(doc: &BaseDocument, id: NodeId) -> Vec<NodeId> {
    let name = attr(doc, id, "name").unwrap_or("");
    if name.is_empty()
        || !attr(doc, id, "type").is_some_and(|kind| kind.eq_ignore_ascii_case("radio"))
    {
        return vec![id];
    }
    let nodes = subtree(doc, root(doc, id));
    let ids = tree_ids(doc, &nodes);
    let owner = form_owner(doc, id, &ids);
    nodes
        .iter()
        .copied()
        .filter(|node| {
            tag(doc, *node, "input")
                && attr(doc, *node, "type").is_some_and(|kind| kind.eq_ignore_ascii_case("radio"))
                && attr(doc, *node, "name") == Some(name)
                && form_owner(doc, *node, &ids) == owner
        })
        .collect()
}

pub(crate) fn set_checked_raw(doc: &mut BaseDocument, id: NodeId, value: bool) -> bool {
    let Some(current) = checked(doc, id) else {
        return false;
    };
    if current == value {
        return true;
    }
    doc.snapshot_node_state_only(id);
    let node = doc.get_node_mut(id).unwrap();
    let element = node.data.downcast_element_mut().unwrap();
    if element.checkbox_input_checked().is_none() {
        element.special_data = SpecialElementData::CheckboxInput(current);
    }
    element.set_checkbox_input_checked(value);
    node.mark_ancestors_dirty();
    doc.shell_provider.request_redraw();
    true
}

pub(crate) fn set_checked(doc: &mut BaseDocument, id: NodeId, value: bool) -> bool {
    if checked(doc, id).is_none() {
        return false;
    }
    if value {
        for other in radio_group(doc, id) {
            if other != id {
                set_checked_raw(doc, other, false);
            }
        }
    }
    set_checked_raw(doc, id, value)
}

pub(crate) fn enabled(doc: &BaseDocument, id: NodeId) -> bool {
    if doc
        .get_node(id)
        .and_then(|node| node.data.downcast_element())
        .is_none()
        || attr(doc, id, "disabled").is_some()
        || attr(doc, id, "aria-disabled") == Some("true")
    {
        return false;
    }
    if !["input", "button", "select", "textarea"]
        .iter()
        .any(|name| tag(doc, id, name))
    {
        return true;
    }
    let mut child = id;
    while let Some(parent) = doc.get_node(child).and_then(|node| node.parent) {
        if tag(doc, parent, "fieldset") && attr(doc, parent, "disabled").is_some() {
            let legend = doc
                .get_node(parent)
                .unwrap()
                .children
                .iter()
                .find(|node| tag(doc, **node, "legend"));
            if legend != Some(&child) {
                return false;
            }
        }
        child = parent;
    }
    true
}

pub(crate) fn interaction_control(doc: &BaseDocument, mut id: NodeId) -> Option<NodeId> {
    loop {
        if ["input", "button", "select", "textarea"]
            .iter()
            .any(|name| tag(doc, id, name))
        {
            return Some(id);
        }
        id = doc.get_node(id)?.parent?;
    }
}

pub(crate) fn read_only(doc: &BaseDocument, id: NodeId) -> bool {
    (tag(doc, id, "textarea")
        || (tag(doc, id, "input")
            && !matches!(
                attr(doc, id, "type")
                    .unwrap_or("text")
                    .to_ascii_lowercase()
                    .as_str(),
                "checkbox"
                    | "radio"
                    | "button"
                    | "submit"
                    | "reset"
                    | "file"
                    | "range"
                    | "color"
                    | "hidden"
            )))
        && (attr(doc, id, "readonly").is_some() || attr(doc, id, "aria-readonly") == Some("true"))
}

pub(crate) fn focus_step(doc: &mut BaseDocument, reverse: bool) {
    let eligible = |node: &blitz::dom::Node| {
        node.is_focussable()
            && node.has_boxes()
            && enabled(doc, node.id)
            && attr(doc, node.id, "tabindex")
                .and_then(|value| value.parse::<i32>().ok())
                .is_none_or(|index| index >= 0)
    };
    let next = if let Some(current) = doc.get_focussed_node_id().and_then(|id| doc.get_node(id)) {
        if reverse {
            doc.prev_node(current, eligible)
        } else {
            doc.next_node(current, eligible)
        }
    } else {
        let mut candidates = subtree(doc, doc.root_node().id);
        if reverse {
            candidates.reverse();
        }
        candidates
            .into_iter()
            .find(|id| doc.get_node(*id).is_some_and(eligible))
    };
    if let Some(id) = next {
        doc.set_focus_to(id);
    }
}
