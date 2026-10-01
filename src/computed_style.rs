//! Thin computed-style access to pinned Stylo serialization and Blitz layout.
use blitz::dom::BaseDocument;
use blitz::traits::node_id::NodeId;
use std::sync::OnceLock;
use style::properties::{
    ComputedValues, Importance, LonghandId, PropertyDeclarationBlock, PropertyId, ShorthandId,
};
use style::values::resolved::Context;

pub(crate) fn value(doc: &BaseDocument, id: NodeId, name: &str) -> String {
    if name.len() > 1024 {
        return String::new();
    }
    let Some(node) = doc
        .get_node(id)
        .filter(|node| node.flags.is_in_document() && node.is_element())
    else {
        return String::new();
    };
    let Some(styles) = node.primary_styles() else {
        return String::new();
    };
    let Ok(property) = PropertyId::parse_enabled_for_all_content(name) else {
        return String::new();
    };
    match property.as_shorthand() {
        Err(longhand_or_custom) => {
            if let Some(longhand) = longhand_or_custom.as_longhand() {
                if let Some(value) = used_value(doc, id, &styles, longhand) {
                    return value;
                }
            }
            styles.computed_value_to_string(longhand_or_custom)
        }
        Ok(shorthand) => {
            // Let Stylo choose valid CSS shorthand forms and resolve colors.
            let mut declarations = PropertyDeclarationBlock::new();
            let mut context = Context {
                style: &styles,
                for_property: property.clone(),
                current_longhand: None,
            };
            for longhand in shorthand.longhands() {
                declarations.push(
                    styles.computed_or_resolved_declaration(longhand, Some(&mut context)),
                    Importance::Normal,
                );
            }
            let mut output = String::new();
            if declarations
                .property_value_to_css(&property, &mut output)
                .is_err()
            {
                return String::new();
            }
            output
        }
    }
}

fn used_value(
    doc: &BaseDocument,
    id: NodeId,
    styles: &ComputedValues,
    longhand: LonghandId,
) -> Option<String> {
    let physical = longhand.to_physical(styles.writing_mode);
    if !matches!(
        physical,
        LonghandId::Width
            | LonghandId::Height
            | LonghandId::PaddingLeft
            | LonghandId::PaddingRight
            | LonghandId::PaddingTop
            | LonghandId::PaddingBottom
            | LonghandId::MarginLeft
            | LonghandId::MarginRight
            | LonghandId::MarginTop
            | LonghandId::MarginBottom
    ) {
        return None;
    }
    if !crate::geometry::has_boxes(doc, id) {
        return None;
    }
    let node = doc.get_node(id)?;
    // Non-atomic inline spans have per-line fragments, not one used-size box.
    // Preserve their computed width/height instead of inventing zero dimensions.
    if doc.inline_fragment_rects(id).is_some() {
        return None;
    }
    let layout = node.unrounded_layout();
    let border_box = styles.clone_box_sizing()
        == style::properties::generated::longhands::box_sizing::computed_value::T::BorderBox;
    let value = match physical {
        LonghandId::Width => {
            if border_box {
                layout.size.width
            } else {
                layout.content_box_width()
            }
        }
        LonghandId::Height => {
            if border_box {
                layout.size.height
            } else {
                layout.content_box_height()
            }
        }
        LonghandId::PaddingLeft => layout.padding.left,
        LonghandId::PaddingRight => layout.padding.right,
        LonghandId::PaddingTop => layout.padding.top,
        LonghandId::PaddingBottom => layout.padding.bottom,
        LonghandId::MarginLeft => layout.margin.left,
        LonghandId::MarginRight => layout.margin.right,
        LonghandId::MarginTop => layout.margin.top,
        LonghandId::MarginBottom => layout.margin.bottom,
        _ => return None,
    };
    Some(format!("{}px", if value == 0.0 { 0.0 } else { value }))
}

pub(crate) fn names(doc: &BaseDocument, id: NodeId) -> Vec<String> {
    let Some(node) = doc
        .get_node(id)
        .filter(|node| node.flags.is_in_document() && node.is_element())
    else {
        return Vec::new();
    };
    let Some(styles) = node.primary_styles() else {
        return Vec::new();
    };
    static LONGHANDS: OnceLock<Vec<String>> = OnceLock::new();
    let mut names = LONGHANDS
        .get_or_init(|| {
            // CSS all excludes direction and unicode-bidi; include them explicitly.
            let mut names: Vec<_> = ShorthandId::All
                .longhands()
                .chain([LonghandId::Direction, LonghandId::UnicodeBidi])
                .filter(|id| PropertyId::NonCustom((*id).into()).enabled_for_all_content())
                .map(|id| id.name().to_owned())
                .collect();
            names.sort_unstable();
            names.dedup();
            names
        })
        .clone();
    let custom = styles.custom_properties();
    let mut custom_names: Vec<_> = custom
        .inherited
        .iter()
        .chain(custom.non_inherited.iter())
        .filter(|(_, value)| value.is_some())
        .map(|(name, _)| format!("--{name}"))
        .collect();
    custom_names.sort_unstable();
    custom_names.dedup();
    names.extend(custom_names);
    names
}
