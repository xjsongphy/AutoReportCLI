# AutoReportCLI

A **codex-style, multi-agent command-line tool** for automatically writing
physics-experiment reports in LaTeX. It is a Rust rewrite of the
[AutoReport](../AutoReport) desktop app — no GUI, no MCP, no image recognition.
The terminal is the interface; the working directory is the project.

```
┌──────────────────────────────────────────────────────────────────┐
│ AutoReportCLI  <workspace>   anthropic/claude-…   focused: Main   │
├──────────────────────────────────────────────────────────────────┤
│ ▸ Main                                                          │
│ I'll start with theory, then data analysis…                     │
│   ⚒ Theory · write_file(…/formulas.md)                          │
│     {"success": true}                                           │
│ ▸ Data Analysis                                                 │
│ analyzing power-curve data…                                     │
├──────────────────────────────────────────────────────────────────┤
│  message to Data Analysis                                       │
└ Tab: switch agent   Enter: send   ↑/↓: scroll   /help   Ctrl+C ┘
```

## Why

AutoReport coordinates several specialized agents (Main, Theory, Data Analysis,
Plotting, Report) that collaborate to produce a complete LaTeX report from raw
data and references. AutoReportCLI keeps that agent model and its proprietary
prompts, but swaps the PyQt GUI for a fast, codex-like terminal UI and ports the
runtime to Rust.

## Quick start

```bash
# 1. Build
cargo build --release

# 2. Configure a provider (env var is enough for a quick start)
export ANTHROPIC_API_KEY=sk-ant-...

# 3. Run inside an experiment folder (the working directory *is* the project)
cd my-experiment
/path/to/autoreport
```

On first run AutoReportCLI creates the fixed project layout if any are missing:

```
.
├── data/            raw data (+ data/processed/ analysis output)
├── references/      reference PDFs/images, custom templates, skills
├── theory/          Theory agent output
├── code/            Plotting agent scripts + figures
├── tex/             Report agent LaTeX + compiled PDF
├── outline/         Main agent's report outline
└── .autoreport/     manifests + internal metadata
```

Configure providers in `autoreport.config.yaml` (see
`autoreport.config.example.yaml`). Env vars (`ANTHROPIC_API_KEY`,
`OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`) override empty YAML
fields, and providers auto-register from env vars when no config file exists.

## Agents & permissions

| Agent          | Writes to          | Extra tools                                   |
|----------------|--------------------|-----------------------------------------------|
| Main           | `outline/`         | `send_to_agent`, `manage_tasks`, exec         |
| Theory         | `theory/`          | `manage_tasks`, `report_issue`                |
| Data Analysis  | `data/processed/`  | `manage_tasks`, `report_issue`, exec          |
| Plotting       | `code/`            | `manage_tasks`, `report_issue`, exec          |
| Report         | `tex/`             | `manage_tasks`, `report_issue`, exec (LaTeX)  |

All agents can read every directory. Write tools refuse paths outside the
agent's assigned folder and block `..` traversal and `.autoreport`.

Every agent also has `read`, `write_file`, `edit_file`, `delete_file`,
`manifest`, `load_skill`, `list_skills`.

## Sub-agents run forever

Agents are persistent for the life of the process — you don't open or close
them. Use **Tab** (or `/switch <agent>`) to focus one, and clear its memory
without stopping it:

| Command            | Effect                                            |
|--------------------|---------------------------------------------------|
| `/agents`          | list agents + live statuses                       |
| `/switch <agent>`  | focus an agent                                    |
| `/clear`           | clear the focused agent's context (agent keeps running) |
| `/compact`         | compact the focused agent's context               |
| `/new`             | reset the focused agent                            |
| `/manifest`        | show produced files per agent                     |
| `/help`            | command list                                      |
| `/quit`            | exit                                              |

## Skills

Drop Markdown skill files (with `name` / `description` YAML frontmatter) into
`references/skills/` (or `.autoreport/skills/`). Agents discover them via
`list_skills` and load full instructions with `load_skill`. Built-in agent
prompts live in `templates/agents/` and can be overridden by placing a file of
the same name in `references/agents/`.

## Architecture

```
src/
  main.rs            CLI entry: config → folder init → LoopManager → TUI
  lib.rs             public modules
  config/            schema, YAML + env loading, workspace auto-init
  provider/          LLMProvider trait; Anthropic + OpenAI-compat (streaming)
  tools/             file / exec / task / agent-comm / manifest / skill tools
  runtime/           LoopManager + AgentLoop (stream + tool-call iteration)
  bus.rs             broadcast message bus (pub/sub spine)
  taskboard.rs       shared coordination task board
  skills/  prompts/  skill + prompt loaders
  tui.rs             codex-style ratatui/crossterm interface
templates/agents/    built-in agent prompts (the proprietary prompts)
```

The bus broadcasts every [`BusMessage`](src/types.rs) to all agent loops and the
TUI; each agent filters messages addressed to it. Tool calls iterate up to
`max_tool_iterations`, streaming each model turn to the UI.

## Tests

```bash
cargo test
```

Includes an end-to-end test (mock provider → tool call → file write → reply) and
write-isolation tests.

## Status

Foundation complete and compiling: providers (Anthropic + OpenAI-compat with
SSE streaming), the tool system with per-agent isolation, the message-bus agent
runtime with `/clear` and `/compact`, skills, prompt templates, and the codex
TUI. Provider/model coverage and the richer markdown rendering of the original
codex TUI are areas to extend next.
