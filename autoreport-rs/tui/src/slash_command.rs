//! Slash-command catalog and completion, mirroring Codex's `slash_command`.

#[derive(Clone, Copy)]
pub(crate) struct SlashCommandItem {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
}

pub(crate) struct SlashCompletion {
    pub(crate) matches: Vec<SlashCommandItem>,
    pub(crate) selected: usize,
}

const LIMIT: usize = 8;

const COMMANDS: &[SlashCommandItem] = &[
    SlashCommandItem {
        name: "help",
        description: "show commands",
    },
    SlashCommandItem {
        name: "agent",
        description: "switch the active agent thread",
    },
    SlashCommandItem {
        name: "agents",
        description: "alias for /agent",
    },
    SlashCommandItem {
        name: "sessions",
        description: "list persisted project sessions",
    },
    SlashCommandItem {
        name: "switch",
        description: "focus an agent",
    },
    SlashCommandItem {
        name: "config",
        description: "view and edit API settings",
    },
    SlashCommandItem {
        name: "model",
        description: "assign main/sub APIs and model names",
    },
    SlashCommandItem {
        name: "env",
        description: "configure Python and report language",
    },
    SlashCommandItem {
        name: "compact",
        description: "summarize focused agent context",
    },
    SlashCommandItem {
        name: "pager",
        description: "open the transcript pager",
    },
    SlashCommandItem {
        name: "new",
        description: "reset focused agent context",
    },
    SlashCommandItem {
        name: "clear",
        description: "clear focused agent context",
    },
    SlashCommandItem {
        name: "copy",
        description: "copy the last assistant response",
    },
    SlashCommandItem {
        name: "manifest",
        description: "show produced files",
    },
    SlashCommandItem {
        name: "index",
        description: "rebuild @ file index",
    },
    SlashCommandItem {
        name: "ide",
        description: "toggle IDE context injection",
    },
    SlashCommandItem {
        name: "quit",
        description: "exit",
    },
];

pub(crate) fn matches(query: &str) -> Vec<SlashCommandItem> {
    COMMANDS
        .iter()
        .copied()
        .filter(|command| command.name.starts_with(query))
        .take(LIMIT)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn removes_only_models_alias() {
        assert_eq!(matches("model")[0].name, "model");
        assert_eq!(matches("env")[0].name, "env");
        assert!(matches("models").is_empty());
        assert_eq!(matches("sess")[0].name, "sessions");
        assert!(matches("co").iter().any(|item| item.name == "config"));
    }
}
