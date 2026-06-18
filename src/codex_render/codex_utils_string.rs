//! Stub for codex `codex_utils_string::normalize_markdown_hash_location_suffix`.
//! Minimal identity implementation: keep the suffix unchanged.

pub fn normalize_markdown_hash_location_suffix(suffix: &str) -> Option<String> {
    Some(suffix.to_string())
}
