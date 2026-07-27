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
        name: "models",
        description: "alias for /model",
    },
    SlashCommandItem {
        name: "env",
        description: "select Python and inspect local tool readiness",
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
    fn filters_by_prefix_in_presentation_order() {
        let commands = matches("c");
        assert_eq!(commands[0].name, "config");
        assert_eq!(commands[1].name, "compact");
        assert_eq!(commands[2].name, "clear");
    }
}
