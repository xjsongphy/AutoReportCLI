//! Ported verbatim from codex `utils/string/src/lib.rs`.

/// Convert a markdown-style `#L..` location suffix into a terminal-friendly
/// `:line[:column][-line[:column]]` suffix.
pub fn normalize_markdown_hash_location_suffix(suffix: &str) -> Option<String> {
    let fragment = suffix.strip_prefix('#')?;
    let (start, end) = match fragment.split_once('-') {
        Some((start, end)) => (start, Some(end)),
        None => (fragment, None),
    };
    let (start_line, start_column) = parse_markdown_hash_location_point(start)?;
    let mut normalized = String::from(":");
    normalized.push_str(start_line);
    if let Some(column) = start_column {
        normalized.push(':');
        normalized.push_str(column);
    }
    if let Some(end) = end {
        let (end_line, end_column) = parse_markdown_hash_location_point(end)?;
        normalized.push('-');
        normalized.push_str(end_line);
        if let Some(column) = end_column {
            normalized.push(':');
            normalized.push_str(column);
        }
    }
    Some(normalized)
}

fn parse_markdown_hash_location_point(point: &str) -> Option<(&str, Option<&str>)> {
    let point = point.strip_prefix('L')?;
    match point.split_once('C') {
        Some((line, column)) => Some((line, Some(column))),
        None => Some((point, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_single_location() {
        assert_eq!(
            normalize_markdown_hash_location_suffix("#L42"),
            Some(":42".to_string())
        );
        assert_eq!(
            normalize_markdown_hash_location_suffix("#L42C7"),
            Some(":42:7".to_string())
        );
    }

    #[test]
    fn converts_ranges() {
        assert_eq!(
            normalize_markdown_hash_location_suffix("#L42-L48"),
            Some(":42-48".to_string())
        );
        assert_eq!(
            normalize_markdown_hash_location_suffix("#L10C3-L12C5"),
            Some(":10:3-12:5".to_string())
        );
    }

    #[test]
    fn rejects_non_location() {
        assert_eq!(normalize_markdown_hash_location_suffix("section"), None);
    }
}
