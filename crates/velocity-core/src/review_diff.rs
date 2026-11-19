//! Bounded diff pages for landed-change review.

/// Limit each JSON-encoded patch page. Five first pages leave room in a 20k-token
/// review even when patch text tokenizes poorly.
pub fn review_page(diff: &str, offset: usize) -> Option<(&str, Option<usize>)> {
    crate::text_budget::json_text_page(diff, offset, 3_000)
}
