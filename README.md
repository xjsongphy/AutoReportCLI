# AutoReportCLI

> English | [简体中文](README_zh.md)

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

AutoReport coordinates several specialized agents (Main, Theory, Data Analysis, Plotting, Report) that collaborate to produce a complete LaTeX report from raw data and references. AutoReportCLI keeps that agent model and its proprietary prompts, but swaps the PyQt GUI for a fast, codex-like terminal UI and ports the runtime to Rust.

## Quick start

### 1. Build

```bash
git clone <this-repo> AutoReportCLI && cd AutoReportCLI
cargo build --release        # binary: target/release/autoreport
# optional: ln -s "$PWD/target/release/autoreport" /usr/local/bin/autoreport
```

### 2. Configure a provider (any one of these works)

- **Env var** (simplest): `export ANTHROPIC_API_KEY=sk-ant-...`
  (or `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`, `GEMINI_API_KEY`).
- **Config file**: copy `autoreport.config.example.yaml` to
  `autoreport.config.yaml` in your project and edit `providers` /
  `active_provider`. See `autoreport.config.example.yaml`.
- **Nothing**: on first run AutoReportCLI pulls the **cc-switch** preset catalog
  and the **skills** repo (see *Startup sync* below); the synced presets become
  selectable providers — you then only need to set the matching env var, e.g.
  `export ANTHROPIC_AUTH_TOKEN=...` for a cc-switch Claude preset.
- **Interactive (codex-style)**: if no `autoreport.config.yaml` exists and no
  provider key is resolvable, AutoReportCLI opens a full-screen provider-setup
  screen on launch (like codex's login page). Pick a provider, set its
  model / API base / API key, mark it active, and save. From inside the running
  TUI, `/config` reopens the same screen to view or edit providers; changes are
  written to `autoreport.config.yaml` and apply on the next start.

### 3. Create / enter a project folder and run

The working directory **is** the project — each experiment lives in its own
folder with the fixed layout below.

```bash
mkdir ~/my-experiment && cd ~/my-experiment
# drop raw data into data/, references into references/, then:
autoreport                     # or: /path/to/AutoReportCLI/target/release/autoreport
```

On launch AutoReportCLI:
1. Creates any missing project folders (`data/`, `references/`, `theory/`,
   `code/`, `tex/`, `outline/`, `.autoreport/`).
2. Materializes the bundled report template into `references/templates/`.
3. Syncs the two upstream repos (cc-switch presets + skills) — needs network on
   first run; thereafter the cache under `.autoreport/external` is reused.
4. Starts one persistent agent loop per agent type and opens the TUI.

```
.
├── data/            raw data (+ data/processed/ analysis output)
├── references/      reference PDFs/images, custom templates, skills
├── theory/          Theory agent output
├── code/            Plotting agent scripts + figures
├── tex/             Report agent LaTeX + compiled PDF
├── outline/         Main agent's report outline
└── .autoreport/     manifests, synced skills/presets, internal metadata
```

### 4. Drive it (in the TUI)

| Key / command | Effect |
|---|---|
| type, **Enter** | send a message to the focused agent |
| **Tab** / **BackTab** | cycle focus across agents |
| **`@`** | mention a workspace file (fuzzy popup, **Tab** to accept) |
| **Esc** | interrupt the focused agent's running turn (codex-style) |
| **↑/↓**, **PgUp/PgDn** | scroll history |
| `/agents` | list agents + live status |
| `/switch <agent>` | focus `main|data_analysis|plotting|theory|report` |
| `/clear` | clear the focused agent's context (it keeps running) |
| `/compact` | compact the focused agent's context |
| `/manifest` | show files each agent has produced |
| `/help`, `/quit` | help, exit |

CLI flags: `--workspace <dir>` (default: cwd), `--provider <key>`,
`--sync-presets` (force a full repo fetch then exit), `--no-sync` (use cache
only), `-v` (verbose logging).

Env vars (besides API keys): `ANTHROPIC_BASE_URL` etc. are honored when set
via a cc-switch preset's `api_key_env`; you can also pin `active_provider` with
`--provider`.

Configure providers in `autoreport.config.yaml` (see
`autoreport.config.example.yaml`). Env vars override empty YAML fields, and
providers auto-register from env vars when no config file exists.


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
`apply_patch` (codex `*** Begin Patch` format), `manifest`, `load_skill`,
`list_skills`.

## Standalone defaults

The binary ships embedded defaults, materialized on first run (never overwriting
user files):

- **Skills** → `.autoreport/skills/`: `experiment-report-writer`,
  `latex-compile`, `md-report-writer`, `mineru`. Drop your own in
  `references/skills/` to override.
- **Report template** → `references/templates/`: `template_mpl.tex` + `mpltx.cls`,
  the built-in template the Report agent starts from.

So a fresh project is immediately runnable: `load_skill` works out of the box and
the Report agent has a template to copy into `tex/`.

## `@` mentions & markdown rendering (codex-style)

- Type `@` in the input to fuzzy-search workspace files; a popup lists matches,
  arrow keys move, **Tab** accepts. On send, each `@rel/path` is expanded — the
  file's contents are appended to the message the model receives (codex expands
  mentions into context), while the visible text keeps the `@path`.
- Assistant output is rendered as **markdown** via `pulldown-cmark` (the same
  library codex uses): headings, bold/italic, inline & fenced code, lists,
  blockquotes, links. A braille spinner animates while an agent thinks.

## Sub-agents run forever

Agents are persistent for the life of the process — you don't open or close
them. Use **Tab** (or `/switch <agent>`) to focus one, and clear its memory
without stopping it. The agent runtime follows codex's session design:
conversation items are codex `ResponseItem`s, the prompt is assembled as
`instructions + items`, input is queued through a codex-style `Op`/mailbox
channel (new input interrupts the active turn), and **every item is appended to
a rollout file** (`.autoreport/sessions/rollout-*.jsonl`) so each agent resumes
its last conversation on the next launch. **Esc** interrupts the focused
agent's running turn.

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
  runtime/           LoopManager + AgentLoop (codex session: Op queue, ResponseItem
                     history, instructions+items prompt, interrupt, rollout resume)
  bus.rs             broadcast message bus (pub/sub spine)
  taskboard.rs       shared coordination task board
  rollout.rs         codex-format conversation persistence (ResponseItem JSONL + resume)
  codex_render/      vendored codex markdown/wrapping/highlight (pulldown-cmark + syntect)
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
SSE streaming), the tool system with per-agent isolation + codex `apply_patch`,
the message-bus agent runtime with `/clear` and codex-style `/compact`, bundled
skills + report template (standalone), `@` file mentions, markdown rendering,
and the codex-style TUI with thinking spinner. See **[docs/PARITY.md](docs/PARITY.md)**
for the full parity checklist and roadmap (checkpoints/rollback, first-class PDF
tool, preset sync, plotting validator, syntax highlighting, session resume).
