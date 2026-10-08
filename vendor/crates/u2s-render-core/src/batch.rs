//! Cursor pagination, shared by every renderer.
//!
//! The contract a caller depends on: walking `next_from` from the first page to
//! exhaustion yields every requested page exactly once, in order, whatever the
//! caps are. Two caps apply and both are reported — a count cap and a byte
//! budget — because a count alone cannot bound a response's size, and a silent
//! stop is indistinguishable from the end of the document.

use crate::error::RenderError;
use crate::types::{RenderedPage, RenderedPages};

#[derive(Debug, Clone, Copy)]
pub struct BatchLimits {
    pub max_images: usize,
    pub max_bytes: usize,
}

/// Render `pages` in order under `limits`, stopping at whichever cap binds
/// first. `render` produces one page; it is called lazily so a stopped batch
/// costs nothing for the pages it did not reach.
///
/// A single image larger than the whole budget is returned alone rather than
/// refused — the caller asked for a page and gets it. The byte cap exists to
/// bound *batches*, not to make a page unreachable.
pub fn render_batch<F>(
    pages: &[u32],
    limits: BatchLimits,
    warning: Option<String>,
    mut render: F,
) -> Result<RenderedPages, RenderError>
where
    F: FnMut(u32) -> Result<RenderedPage, RenderError>,
{
    let mut out: Vec<RenderedPage> = Vec::new();
    let mut bytes = 0usize;
    let mut budget_hit = None;
    let mut next_from = None;

    for (i, &page_number) in pages.iter().enumerate() {
        if out.len() >= limits.max_images {
            budget_hit = Some("count");
            next_from = Some(page_number);
            break;
        }
        let rendered = render(page_number)?;

        if !out.is_empty() && bytes + rendered.data.len() > limits.max_bytes {
            budget_hit = Some("bytes");
            next_from = Some(page_number);
            break;
        }
        bytes += rendered.data.len();
        out.push(rendered);

        if bytes >= limits.max_bytes && i + 1 < pages.len() {
            budget_hit = Some("bytes");
            next_from = Some(pages[i + 1]);
            break;
        }
    }

    Ok(RenderedPages {
        pages: out,
        next_from,
        budget_hit,
        warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(n: u32, size: usize) -> RenderedPage {
        RenderedPage {
            page: n,
            width_px: 10,
            height_px: 10,
            dpi_effective: 72.0,
            mime: "image/jpeg",
            data: vec![0u8; size],
        }
    }

    #[test]
    fn a_complete_walk_reports_no_cursor() {
        let out = render_batch(
            &[1, 2, 3],
            BatchLimits {
                max_images: 4,
                max_bytes: 1 << 20,
            },
            None,
            |n| Ok(page(n, 10)),
        )
        .unwrap();
        assert_eq!(out.pages.len(), 3);
        assert_eq!(out.next_from, None);
        assert_eq!(out.budget_hit, None);
    }

    #[test]
    fn the_count_cap_is_reported_and_never_exceeded() {
        let out = render_batch(
            &[1, 2, 3, 4],
            BatchLimits {
                max_images: 2,
                max_bytes: 1 << 20,
            },
            None,
            |n| Ok(page(n, 10)),
        )
        .unwrap();
        assert_eq!(out.pages.len(), 2);
        assert_eq!(out.budget_hit, Some("count"));
        assert_eq!(out.next_from, Some(3));
    }

    #[test]
    fn an_oversized_single_page_still_comes_back_alone() {
        let out = render_batch(
            &[1, 2],
            BatchLimits {
                max_images: 4,
                max_bytes: 1,
            },
            None,
            |n| Ok(page(n, 10_000)),
        )
        .unwrap();
        assert_eq!(out.pages.len(), 1, "the page asked for is never withheld");
        assert_eq!(out.budget_hit, Some("bytes"));
        assert_eq!(out.next_from, Some(2));
    }

    #[test]
    fn walking_the_cursor_covers_every_page_exactly_once() {
        // The invariant the tool contract rests on, over every cap combination.
        for (max_images, max_bytes) in [(1usize, 1 << 20), (3, 1 << 20), (4, 25), (2, 1)] {
            let all: Vec<u32> = (1..=10).collect();
            let mut seen = Vec::new();
            let mut from = Some(1u32);
            let mut hops = 0;

            while let Some(start) = from {
                let remaining: Vec<u32> = all.iter().copied().filter(|&p| p >= start).collect();
                let out = render_batch(
                    &remaining,
                    BatchLimits {
                        max_images,
                        max_bytes,
                    },
                    None,
                    |n| Ok(page(n, 10)),
                )
                .unwrap();
                assert!(!out.pages.is_empty(), "a batch must make progress");
                seen.extend(out.pages.iter().map(|p| p.page));
                from = out.next_from;
                hops += 1;
                assert!(hops < 50, "cursor failed to terminate");
            }
            assert_eq!(
                seen, all,
                "caps ({max_images}, {max_bytes}) lost or repeated a page"
            );
        }
    }

    #[test]
    fn a_render_failure_propagates_rather_than_truncating_silently() {
        let out = render_batch(
            &[1, 2, 3],
            BatchLimits {
                max_images: 4,
                max_bytes: 1 << 20,
            },
            None,
            |n| {
                if n == 2 {
                    Err(RenderError::backend("test", "boom"))
                } else {
                    Ok(page(n, 10))
                }
            },
        );
        assert!(
            out.is_err(),
            "a failed page must not look like the end of the walk"
        );
    }
}
