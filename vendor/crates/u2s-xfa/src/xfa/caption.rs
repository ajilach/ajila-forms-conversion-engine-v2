//! A field's caption (XFA 3.3 "The caption element"): its label, drawn in a
//! region of the field's box beside, above or below the value.

use super::{Font, Num, Para, XfaNode, XfaNodeKind};
use crate::flattened::CaptionPlacement;

/// A `<caption>` with text to show.
#[derive(Debug, Clone, PartialEq)]
pub struct Caption {
    pub placement: CaptionPlacement,
    /// The space the caption takes along its placement side, in points;
    /// `None` for `reserve="-1"` or no `reserve`: the caption takes what its
    /// text needs.
    pub reserve: Option<Num>,
    pub text: String,
    /// `invisible`: the caption takes its space but is not drawn.
    pub drawn: bool,
    pub font: Option<Font>,
    pub para: Option<Para>,
}

impl Caption {
    /// The caption of `node` (a field, draw or exclusion group), when it has
    /// one with text that takes part in layout: a `hidden` or `inactive`
    /// caption takes no space (XFA 3.3 "The presence property").
    pub fn of(node: &XfaNode) -> Option<Caption> {
        let caption = node.children.iter().find(
            |c| matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "caption"),
        )?;
        let presence = caption.attributes.get("presence").map(String::as_str);
        if matches!(presence, Some("hidden" | "inactive")) {
            return None;
        }
        let text = caption.template_value()?.trim().to_string();
        if text.is_empty() {
            return None;
        }
        let reserve = caption
            .attributes
            .get("reserve")
            .and_then(|r| XfaNode::parse_dimension(r).ok())
            .filter(|r| *r >= Num::ZERO);
        Some(Caption {
            placement: match caption.attributes.get("placement").map(String::as_str) {
                Some("right") => CaptionPlacement::Right,
                Some("top") => CaptionPlacement::Top,
                Some("bottom") => CaptionPlacement::Bottom,
                Some("inline") => CaptionPlacement::Inline,
                _ => CaptionPlacement::Left,
            },
            reserve,
            text,
            drawn: presence != Some("invisible"),
            font: caption.font.clone(),
            para: caption.para.clone(),
        })
    }
}

/// A box: x, y, width, height.
pub type CaptionBox = (f32, f32, f32, f32);

/// `bx` split into the caption's region and the value's: the caption takes
/// `reserve` along the side `placement` names (an inline caption precedes
/// the value on its left), capped at the box's own extent.
pub fn split_caption_box(bx: CaptionBox, placement: CaptionPlacement, reserve: f32) -> (CaptionBox, CaptionBox) {
    let (x, y, w, h) = bx;
    match placement {
        CaptionPlacement::Left | CaptionPlacement::Inline => {
            let r = reserve.clamp(0.0, w);
            ((x, y, r, h), (x + r, y, w - r, h))
        }
        CaptionPlacement::Right => {
            let r = reserve.clamp(0.0, w);
            ((x + w - r, y, r, h), (x, y, w - r, h))
        }
        CaptionPlacement::Top => {
            let r = reserve.clamp(0.0, h);
            ((x, y, w, r), (x, y + r, w, h - r))
        }
        CaptionPlacement::Bottom => {
            let r = reserve.clamp(0.0, h);
            ((x, y + h - r, w, r), (x, y, w, h - r))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: CaptionBox = (10.0, 20.0, 100.0, 30.0);

    #[test]
    fn each_placement_takes_its_side() {
        assert_eq!(
            split_caption_box(BOX, CaptionPlacement::Left, 25.0),
            ((10.0, 20.0, 25.0, 30.0), (35.0, 20.0, 75.0, 30.0))
        );
        assert_eq!(
            split_caption_box(BOX, CaptionPlacement::Right, 25.0),
            ((85.0, 20.0, 25.0, 30.0), (10.0, 20.0, 75.0, 30.0))
        );
        assert_eq!(
            split_caption_box(BOX, CaptionPlacement::Top, 10.0),
            ((10.0, 20.0, 100.0, 10.0), (10.0, 30.0, 100.0, 20.0))
        );
        assert_eq!(
            split_caption_box(BOX, CaptionPlacement::Bottom, 10.0),
            ((10.0, 40.0, 100.0, 10.0), (10.0, 20.0, 100.0, 20.0))
        );
        assert_eq!(
            split_caption_box(BOX, CaptionPlacement::Inline, 25.0),
            split_caption_box(BOX, CaptionPlacement::Left, 25.0)
        );
    }

    #[test]
    fn a_reserve_larger_than_the_box_takes_all_of_it() {
        assert_eq!(
            split_caption_box(BOX, CaptionPlacement::Left, 500.0),
            ((10.0, 20.0, 100.0, 30.0), (110.0, 20.0, 0.0, 30.0))
        );
    }

    fn caption_of(xml: &str) -> Option<Caption> {
        let nodes = XfaNode::parse(format!("<xdp><field name=\"F\">{xml}</field></xdp>").as_bytes()).expect("xml");
        Caption::of(&nodes[0].children[0])
    }

    #[test]
    fn a_caption_reads_its_text_reserve_and_placement() {
        let c = caption_of(r#"<caption reserve="50mm" placement="right"><value><text>Unternehmen</text></value></caption>"#).expect("caption");
        assert_eq!(c.text, "Unternehmen");
        assert_eq!(c.placement, CaptionPlacement::Right);
        assert!(c.reserve.is_some_and(|r| (r - Num::from(141)).abs() < Num::ONE), "50mm is about 141.7pt");
        assert!(c.drawn);
        assert_eq!(caption_of(r#"<caption reserve="-1"><value><text>A</text></value></caption>"#).expect("caption").reserve, None);
    }

    #[test]
    fn hidden_or_empty_captions_take_no_space() {
        assert_eq!(caption_of(r#"<caption presence="hidden"><value><text>A</text></value></caption>"#), None);
        assert_eq!(caption_of(r#"<caption reserve="2mm"><font typeface="X"/></caption>"#), None);
        let invisible = caption_of(r#"<caption presence="invisible"><value><text>A</text></value></caption>"#).expect("caption");
        assert!(!invisible.drawn);
    }
}
