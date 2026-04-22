//! Thin wrapper around [`neo_frizbee`] — the single fuzzy matcher used
//! across rc-cli. This avoids scattering `match_list` calls with inline
//! `Config` plumbing throughout pickers and search tools.
//!
//! Scores are exposed as `u32` to keep call-site signatures unchanged
//! from the previous nucleo-based API.

use neo_frizbee::{Config, match_list};

/// Shared scoring config: smart subsequence match with a small typo
/// tolerance. Matches the practical behavior of the old nucleo default
/// for short needles; results are always sorted descending by score.
fn config() -> Config {
    Config {
        max_typos: Some(2),
        sort: true,
        ..Default::default()
    }
}

/// Score `query` against one haystack. Returns `None` when the needle
/// is empty or no match is found.
pub fn score_one(query: &str, haystack: &str) -> Option<u32> {
    if query.is_empty() {
        return None;
    }
    match_list(query, &[haystack], &config())
        .first()
        .map(|m| m.score as u32)
}

/// Take the best score across several haystacks (for an item with
/// multiple fuzzy-matchable fields, e.g. name + description).
pub fn score_best(query: &str, haystacks: &[&str]) -> Option<u32> {
    haystacks.iter().filter_map(|h| score_one(query, h)).max()
}

/// Score `query` against each string in `items`, returning
/// `(score, index)` pairs sorted descending.
pub fn match_strings<S: AsRef<str>>(query: &str, items: &[S]) -> Vec<(u32, usize)> {
    if query.is_empty() || items.is_empty() {
        return Vec::new();
    }
    let refs: Vec<&str> = items.iter().map(|s| s.as_ref()).collect();
    match_list(query, &refs, &config())
        .into_iter()
        .map(|m| (m.score as u32, m.index as usize))
        .collect()
}
