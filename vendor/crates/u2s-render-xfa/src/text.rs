//! Per-page text.
//!
//! The engine has no text-extraction API, but `Flattened` carries positioned
//! nodes, so a page's text is the nodes whose band it falls in, read in
//! layout order.
//!
//! Two things are deliberate. Field *labels* are not used: verified against the
//! real corpus, `FlattenedNode::Field.label` merely mirrors the field's name —
//! human-readable captions arrive as separate `Text` nodes. And the text is
//! **logical, not visual**: line breaking happens at render time, so this is
//! what the page says, not how it wraps.
//!
//! [`page_texts`] is the primitive and [`page_text`] is one page of it. That
//! way round because reading a node's band costs a walk of every node in the
//! document: doing it once for every page beats doing it once per page, which
//! is what a search across pages would otherwise pay.

use u2s_xfa::{Flattened, FlattenedNodeKind};

use crate::bands::Band;

fn to_f32(d: u2s_xfa::Num) -> f32 {
    use std::str::FromStr;
    f32::from_str(&d.to_string()).unwrap_or(0.0)
}

/// Everything readable on each of `bands`, in layout order (top to bottom,
/// then left to right), one node per line — one entry per band, in the order
/// given.
///
/// One pass over the document's nodes, bucketing each into its band, rather
/// than one pass per band.
pub fn page_texts(flat: &Flattened, bands: &[Band]) -> Vec<String> {
    let mut buckets: Vec<Vec<(f32, f32, String)>> = vec![Vec::new(); bands.len()];

    for node in flat.iter_nodes() {
        let y = to_f32(node.y);
        let Some(index) = band_index(bands, y) else {
            continue;
        };
        let content = match &node.kind {
            FlattenedNodeKind::Text { content, .. } => content.trim(),
            FlattenedNodeKind::Field { value, .. } => value.trim(),
            // An image says nothing readable; a page of logos is an empty page.
            FlattenedNodeKind::Image(_) => "",
        };
        if !content.is_empty() {
            buckets[index].push((y, to_f32(node.x), content.to_string()));
        }
    }

    buckets
        .into_iter()
        .map(|mut items| {
            items.sort_by(|a, b| {
                a.0.partial_cmp(&b.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            });
            items
                .into_iter()
                .map(|(_, _, t)| t)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

/// Everything readable on one page, in layout order.
pub fn page_text(flat: &Flattened, band: &Band) -> String {
    page_texts(flat, std::slice::from_ref(band))
        .pop()
        .unwrap_or_default()
}

/// Which of `bands` a node at `y` belongs to.
///
/// Half-open on the top edge, so a node sitting exactly on a break belongs to
/// the page it starts and never to both. `bands` is ascending and tiles the
/// column without gaps (asserted in [`crate::bands`]), so a binary search is
/// exact — and keeps bucketing linear in nodes rather than nodes times pages.
fn band_index(bands: &[Band], y: f32) -> Option<usize> {
    let index = bands.partition_point(|b| b.bottom <= y);
    let band = bands.get(index)?;
    (y >= band.top && y < band.bottom).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bands::Band;

    fn band(page: u32, top: f32, bottom: f32) -> Band {
        Band {
            page,
            top,
            bottom,
            width: 612.0,
        }
    }

    /// The invariant that makes [`page_texts`] a safe replacement for a loop
    /// over [`page_text`]: bucketing must place every node exactly where the
    /// per-band filter would have.
    #[test]
    fn a_node_on_a_break_belongs_to_the_page_it_starts() {
        let bands = [band(1, 0.0, 100.0), band(2, 100.0, 200.0)];

        assert_eq!(band_index(&bands, 0.0), Some(0));
        assert_eq!(band_index(&bands, 99.9), Some(0));
        // Half-open on the top edge: exactly on the break is the next page.
        assert_eq!(band_index(&bands, 100.0), Some(1));
        assert_eq!(band_index(&bands, 199.9), Some(1));
        // Past the last band, and before the first, belong to no page.
        assert_eq!(band_index(&bands, 200.0), None);
        assert_eq!(band_index(&bands, -1.0), None);
    }

    #[test]
    fn a_single_band_document_puts_everything_on_one_page() {
        let bands = [band(1, 0.0, 100.0)];
        assert_eq!(band_index(&bands, 50.0), Some(0));
    }
}
