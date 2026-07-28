pub const PLAN: &str = include_str!("../templates/plan.md");
pub const DEFAULT: &str = include_str!("../templates/default.md");
pub const EXECUTE: &str = include_str!("../templates/execute.md");
pub const PAIR_PROGRAMMING: &str = include_str!("../templates/pair_programming.md");

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shipped template must be non-empty — an empty `include_str!`
    /// result would mean a missing/overwritten template file slipped through,
    /// which currently only fails at consumer-render time. Catch it here.
    #[test]
    fn templates_are_non_empty() {
        assert!(!DEFAULT.trim().is_empty(), "DEFAULT template is empty");
        assert!(!PLAN.trim().is_empty(), "PLAN template is empty");
        assert!(!EXECUTE.trim().is_empty(), "EXECUTE template is empty");
        assert!(
            !PAIR_PROGRAMMING.trim().is_empty(),
            "PAIR_PROGRAMMING template is empty"
        );
    }

    /// Templates are Markdown assets; each should contain at least one heading
    /// so downstream prompt rendering has structure to work with.
    #[test]
    fn templates_contain_markdown_heading() {
        for (name, template) in [
            ("DEFAULT", DEFAULT),
            ("PLAN", PLAN),
            ("EXECUTE", EXECUTE),
            ("PAIR_PROGRAMMING", PAIR_PROGRAMMING),
        ] {
            assert!(
                template.lines().any(|line| line.trim_start().starts_with('#')),
                "{name} template has no Markdown heading"
            );
        }
    }
}
