//! CSS-pixel geometry from the authoritative Blitz layout, never a DOM mirror.
use blitz::dom::{BaseDocument, ScrollBehavior};
use blitz::traits::node_id::NodeId;

#[derive(Clone, Copy)]
pub(crate) struct ClientRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The element's border box excludes its own content scroll, while ancestor
/// scroll still moves it. Use unrounded native CSS layout, before paint snapping.
/// Inline fragments already use the containing text root's scrolled origin.
pub(crate) fn bounding_rect(doc: &BaseDocument, id: NodeId) -> Option<ClientRect> {
    if !has_boxes(doc, id) {
        return None;
    }
    if doc.inline_fragment_rects(id).is_some() {
        let rect = doc.get_client_bounding_rect(id)?;
        return Some(ClientRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        });
    }
    let node = doc.get_node(id)?;
    let layout = node.unrounded_layout();
    let viewport_scroll = doc.viewport_scroll();
    let mut rect = ClientRect {
        x: -viewport_scroll.x,
        y: -viewport_scroll.y,
        width: f64::from(layout.size.width),
        height: f64::from(layout.size.height),
    };
    let mut current = Some(id);
    while let Some(current_id) = current {
        let node = doc.get_node(current_id)?;
        let location = node.unrounded_layout().location;
        rect.x += f64::from(location.x);
        rect.y += f64::from(location.y);
        if current_id != id {
            let scroll = node.scroll_offset();
            rect.x -= scroll.x;
            rect.y -= scroll.y;
        }
        current = node.layout_parent.get();
    }
    Some(rect)
}

pub(crate) fn client_rects(doc: &BaseDocument, id: NodeId) -> Vec<ClientRect> {
    if !has_boxes(doc, id) {
        return Vec::new();
    }
    match doc.inline_fragment_rects(id) {
        Some(rects) => rects
            .into_iter()
            .map(|rect| ClientRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
            .collect(),
        None => bounding_rect(doc, id).into_iter().collect(),
    }
}

// [scrollLeft, scrollTop, clientWidth, clientHeight, clientLeft, clientTop,
//  scrollWidth, scrollHeight]. These are CSS extents, not Taffy max offsets.
pub(crate) fn has_boxes(doc: &BaseDocument, id: NodeId) -> bool {
    let Some(node) = doc
        .get_node(id)
        .filter(|node| node.element_data().is_some() && node.has_boxes())
    else {
        return false;
    };
    let mut parent = node.parent;
    while let Some(id) = parent {
        let Some(node) = doc.get_node(id) else {
            return false;
        };
        if node
            .primary_styles()
            .is_some_and(|styles| styles.clone_display().is_none())
        {
            return false;
        }
        parent = node.parent;
    }
    true
}

pub(crate) fn is_hidden(doc: &BaseDocument, id: NodeId) -> bool {
    let mut current = Some(id);
    while let Some(current_id) = current {
        let Some(node) = doc.get_node(current_id) else {
            return true;
        };
        if is_hidden_self(doc, current_id) {
            return true;
        }
        current = node.parent;
    }
    false
}

pub(crate) fn is_hidden_self(doc: &BaseDocument, id: NodeId) -> bool {
    let Some(node) = doc.get_node(id) else {
        return true;
    };
    node.element_data().is_some_and(|element| {
        element
            .attr(blitz::dom::LocalName::from("hidden"))
            .is_some()
            || element.attr(blitz::dom::LocalName::from("aria-hidden")) == Some("true")
    }) || node.primary_styles().is_some_and(|style| {
        style.clone_display().is_none()
            || matches!(
                style.clone_visibility(),
                style::computed_values::visibility::T::Hidden
                    | style::computed_values::visibility::T::Collapse
            )
    })
}

pub(crate) fn metrics(doc: &BaseDocument, id: NodeId) -> Vec<f64> {
    if !has_boxes(doc, id) {
        return vec![0.0; 8];
    }
    let node = doc.get_node(id).expect("checked layout node");
    let layout = node.unrounded_layout();
    if doc.try_root_element().is_some_and(|root| root.id == id) {
        let viewport = doc.viewport();
        let width = f64::from(viewport.window_size.0) / viewport.scale_f64();
        let height = f64::from(viewport.window_size.1) / viewport.scale_f64();
        let scroll = doc.viewport_scroll();
        return vec![
            scroll.x,
            scroll.y,
            width.round(),
            height.round(),
            0.0,
            0.0,
            width
                .max(f64::from(
                    layout.size.width.max(layout.scrollable_overflow_rect.right),
                ))
                .round(),
            height
                .max(f64::from(
                    layout
                        .size
                        .height
                        .max(layout.scrollable_overflow_rect.bottom),
                ))
                .round(),
        ];
    }
    let offset = node.scroll_offset();
    let client_width = (layout.size.width
        - layout.border.left
        - layout.border.right
        - layout.scrollbar_size.width)
        .max(0.0);
    let client_height = (layout.size.height
        - layout.border.top
        - layout.border.bottom
        - layout.scrollbar_size.height)
        .max(0.0);
    vec![
        offset.x,
        offset.y,
        f64::from(client_width.round()),
        f64::from(client_height.round()),
        f64::from(layout.border.left.round()),
        f64::from(layout.border.top.round()),
        f64::from(
            client_width
                .max(layout.scrollable_overflow_rect.right)
                .round(),
        ),
        f64::from(
            client_height
                .max(layout.scrollable_overflow_rect.bottom)
                .round(),
        ),
    ]
}

pub(crate) fn scroll(
    doc: &mut BaseDocument,
    id: NodeId,
    x: Option<f64>,
    y: Option<f64>,
    relative: bool,
) -> bool {
    if !has_boxes(doc, id) {
        return false;
    }
    let before = metrics(doc, id);
    let finite = |value: f64| if value.is_finite() { value } else { 0.0 };
    if relative {
        doc.scroll_by(
            id,
            finite(x.unwrap_or(0.0)),
            finite(y.unwrap_or(0.0)),
            ScrollBehavior::Instant,
        );
    } else {
        doc.scroll_to(
            id,
            finite(x.unwrap_or(before[0])),
            finite(y.unwrap_or(before[1])),
            ScrollBehavior::Instant,
        );
    }
    let after = metrics(doc, id);
    before[0] != after[0] || before[1] != after[1]
}

pub(crate) fn offset_metrics(doc: &BaseDocument, id: NodeId) -> Vec<f64> {
    if !has_boxes(doc, id) {
        return vec![0.0; 4];
    }
    let node = doc.get_node(id).expect("checked layout node");
    let position = node.offset_top_left();
    let rect = bounding_rect(doc, id);
    vec![
        f64::from(position.x.round()),
        f64::from(position.y.round()),
        rect.map_or(0.0, |rect| rect.width.round()),
        rect.map_or(0.0, |rect| rect.height.round()),
    ]
}

pub(crate) fn offset_parent(doc: &BaseDocument, id: NodeId) -> Option<NodeId> {
    if !has_boxes(doc, id) {
        return None;
    }
    let node = doc.get_node(id)?;
    let tag = node.element_data()?.name.local.as_ref();
    if tag == "body"
        || tag == "html"
        || node.primary_styles()?.get_box().position
            == style::properties::generated::longhands::position::computed_value::T::Fixed
    {
        return None;
    }
    node.offset_parent().map(|parent| parent.id)
}

/// [padding x/y, content width/height, border width/height, vertical, depth,
///  scroll x/y]. Sizes precede CSS transforms and retain native fractions.
pub(crate) fn resize_sample(doc: &BaseDocument, id: NodeId) -> Vec<f64> {
    let mut sample = vec![0.0; 10];
    let Some(node) = doc.get_node(id) else {
        return sample;
    };
    let mut parent = node.parent;
    let mut depth = 1;
    while let Some(id) = parent {
        depth += 1;
        parent = doc.get_node(id).and_then(|node| node.parent);
    }
    sample[7] = depth as f64;
    if !node.flags.is_in_document() {
        return sample;
    }
    let scroll = node.scroll_offset();
    sample[8] = scroll.x;
    sample[9] = scroll.y;
    if !has_boxes(doc, id) {
        return sample;
    }
    // Non-replaced inline fragments do not participate in resize observation.
    if doc.inline_fragment_rects(id).is_some() {
        return sample;
    }
    let layout = node.unrounded_layout();
    sample[0] = f64::from(layout.padding.left);
    sample[1] = f64::from(layout.padding.top);
    sample[2] = f64::from(layout.content_box_width().max(0.0));
    sample[3] = f64::from(layout.content_box_height().max(0.0));
    sample[4] = f64::from(layout.size.width.max(0.0));
    sample[5] = f64::from(layout.size.height.max(0.0));
    sample[6] = f64::from(
        node.primary_styles()
            .is_some_and(|style| style.writing_mode.is_vertical()),
    );
    sample
}

/// [has layout box, x, y, width, height] in viewport-relative CSS pixels.
/// IntersectionObserver uses the same authoritative layout rectangle exposed
/// by getBoundingClientRect; ancestor clipping/occlusion are not represented.
pub(crate) fn intersection_sample(doc: &BaseDocument, id: NodeId) -> Vec<f64> {
    if !has_boxes(doc, id) {
        return vec![0.0; 5];
    }
    let Some(rect) = bounding_rect(doc, id) else {
        return vec![0.0; 5];
    };
    vec![
        1.0,
        rect.x,
        rect.y,
        rect.width.max(0.0),
        rect.height.max(0.0),
    ]
}
