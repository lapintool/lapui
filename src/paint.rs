use blitz::dom::{BaseDocument, NodeId};

/// Preserve fractional CSS border widths while building the paint scene.
///
/// Taffy rounds layout edges to integer coordinates without the paint scale.
/// At fractional DPI, this can round a non-zero edge (for example 1 CSS px at
/// 150% = 0.667 layout px) to zero on the far sides of a box. Paint from the
/// unrounded edge widths, then restore the authoritative layout before
/// returning to the event loop.
pub(crate) fn paint_scene(
    scene: &mut impl anyrender::PaintScene,
    document: &mut BaseDocument,
    scale: f64,
    width: u32,
    height: u32,
    x_offset: u32,
    y_offset: u32,
) {
    let mut saved_borders = Vec::new();
    collect_layout_nodes(document, document.root_node().id, &mut saved_borders);

    for (id, _, unrounded_border) in &saved_borders {
        if let Some(node) = document.get_node_mut(*id) {
            let border = &mut node.final_layout_mut().border;
            border.left = unrounded_border[0];
            border.right = unrounded_border[1];
            border.top = unrounded_border[2];
            border.bottom = unrounded_border[3];
        }
    }

    blitz_paint::paint_scene(scene, document, scale, width, height, x_offset, y_offset);

    for (id, rounded_border, _) in saved_borders {
        if let Some(node) = document.get_node_mut(id) {
            let border = &mut node.final_layout_mut().border;
            border.left = rounded_border[0];
            border.right = rounded_border[1];
            border.top = rounded_border[2];
            border.bottom = rounded_border[3];
        }
    }
}

fn collect_layout_nodes(
    document: &BaseDocument,
    id: NodeId,
    output: &mut Vec<(NodeId, [f32; 4], [f32; 4])>,
) {
    let Some(node) = document.get_node(id) else {
        return;
    };
    let children = node.children.clone();
    if node.stylo_element_data_opt().is_some() {
        output.push((
            id,
            [
                node.final_layout().border.left,
                node.final_layout().border.right,
                node.final_layout().border.top,
                node.final_layout().border.bottom,
            ],
            [
                node.unrounded_layout().border.left,
                node.unrounded_layout().border.right,
                node.unrounded_layout().border.top,
                node.unrounded_layout().border.bottom,
            ],
        ));
    }
    for child in children {
        collect_layout_nodes(document, child, output);
    }
}
