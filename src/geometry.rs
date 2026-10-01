//! CSS-pixel geometry from the authoritative Blitz layout, never a DOM mirror.
use blitz::dom::{BaseDocument, ScrollBehavior};
use blitz::traits::node_id::NodeId;

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

pub(crate) fn metrics(doc: &BaseDocument, id: NodeId) -> Vec<f64> {
    if !has_boxes(doc, id) {
        return vec![0.0; 8];
    }
    let node = doc.get_node(id).expect("checked layout node");
    let layout = node.final_layout();
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
    vec![
        offset.x,
        offset.y,
        f64::from(node.client_width().max(0.0).round()),
        f64::from(node.client_height().max(0.0).round()),
        f64::from(layout.border.left.round()),
        f64::from(layout.border.top.round()),
        f64::from(node.scroll_width().max(0.0).round()),
        f64::from(node.scroll_height().max(0.0).round()),
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
    let rect = doc.get_client_bounding_rect(id);
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
