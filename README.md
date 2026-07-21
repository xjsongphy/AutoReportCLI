<div align="center">

![title](https://github.com/xjsongphy/AutoReport/blob/master/assets/screenshots/title.png?raw=1)

### A Codex-Style Multi-Agent CLI for Automated Physics Experiment Report Writing

[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg)](#)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/xjsongphy/AutoreportCLI/blob/master/LICENSE)

English | [中文](README_zh.md)

</div>

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

## Overview

AutoReportCLI keeps the multi-agent workflow of [AutoReport](../AutoReport), but
rebuilds it as a terminal-first tool in Rust. You work inside an experiment
folder, feed in raw data and references, and coordinate five agents from a
single codex-style TUI.

## Features

### Core Capabilities
- **Multi-Agent Collaboration** — Main, Theory, Data Analysis, Plotting, and Report agents work on the same experiment with separate responsibilities
- **Project-Oriented Workspace** — each run operates on the current folder and creates the standard report layout automatically
- **Provider Flexibility** — supports Anthropic and OpenAI-compatible providers, with config-file, env-var, and first-run interactive setup
- **Built-In Defaults** — ships bundled report templates and default skills so a fresh workspace can run immediately

### TUI Experience
- **Codex-Style Interface** — full-screen terminal UI with agent panes, streaming output, markdown rendering, and keyboard-first navigation
- **Persistent Agent Sessions** — each agent keeps its own conversation history and resumes on the next launch
- **`@` File Mentions** — fuzzy-search workspace files and inject them into prompts directly from the input box
- **Slash Commands** — `/agents`, `/switch`, `/config`, `/models`, `/clear`, `/compact`, `/new`, `/manifest`, `/index`, `/help`

## Quick Start

**Prerequisites:** Rust 1.85+, a TeX distribution, and at least one LLM provider API key.

Build from source:

```bash
git clone <this-repo> AutoReportCLI && cd AutoReportCLI
cargo build --release
```

Install globally if you want `autoreport` available from any directory:

```bash
cargo install --path autoreport-rs/cli
```

On Linux, install the companion sandbox launcher into the same Cargo bin
directory as well. Restricted `exec` commands fail closed if it is absent:

```bash
cargo install --path autoreport-rs/linux-sandbox
```

Or run the built binary directly:

```bash
./target/release/autoreport
```

Create a project folder and start:

```bash
mkdir ~/my-experiment && cd ~/my-experiment
autoreport
```

If `autoreport` is not in your `PATH`, use the binary path instead:

```bash
/path/to/AutoReportCLI/target/release/autoreport
```

On first launch, AutoReportCLI configures the API and models, asks you to
confirm the workspace, then creates the workspace folders and opens the TUI.
Built-in templates and synced presets/skills live in the global AutoReport home.

## Configuration

Configure a provider in any of these ways:

- Set an API key environment variable such as `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`, or `GEMINI_API_KEY`
- Copy `autoreport.config.example.toml` to `$AUTOREPORT_HOME/config.toml` (default:
  `~/.autoreport/config.toml`), or use `/config`. `AUTOREPORT_HOME` can point to
  another global AutoReport home.
- Use `/config` to add API entries from presets (presets are additive, so one provider kind can have multiple API keys) and optionally override each entry's alias, then use `/models` to bind main and sub-agent model names only to configured API entries. First launch opens these pages in that order when needed.
- Let the first-run full-screen setup page guide you through provider selection and saving

Useful CLI flags:

- `--workspace <dir>` to run on a different project folder
- `--no-sync` to skip startup sync and use cache only
- `--sync-presets` to force a refresh and exit
- `-v` for verbose logs

## Workspace Layout

```text
.
├── Data/            raw data and processed results
├── References/      papers, images, templates, custom skills
├── Theory/          theory agent output
├── Plots/           plotting figures (Plots/Fig) and scripts (Plots/Scripts)
├── Tex/             LaTeX sources and compiled PDF
├── Outline/         main agent planning output
└── (no AutoReport metadata files; program state lives in ~/.autoreport/)
```

Global program state follows Codex's home-directory model:

```text
~/.autoreport/
├── config.toml
├── auth.json         provider credentials (mode 0600 where supported)
├── history.jsonl                       append-only conversation history
├── skills/          global skills and synced skills
├── external/        synced provider presets
├── templates/       bundled report templates
├── agents/          global prompt overrides
└── workspaces/<id>/                    manifests, rules, and workspace metadata
```

## Development

The Rust source follows Codex's workspace layout rather than a monolithic
`src/` tree:

```text
autoreport-rs/
├── cli/          executable entry point
├── core/         configuration, providers, agents, skills, and domain types
├── protocol/     shared policy and sandbox protocol types
├── rollout/      Codex-compatible session persistence
├── runtime/      persistent agent loops and orchestration
├── sandboxing/   cross-platform execution policy
├── tools/        tool definitions and local handlers
├── tui/          terminal UI, rendering, and IDE context
└── utils/        absolute-path and path-URI crates
```

Run tests with:

```bash
cargo test
```

Build an npm package with a native binary for the current Rust target:

```bash
npm run build:npm
(cd autoreport-cli && npm pack --dry-run)
```

For implementation status and parity notes, see
**[docs/PARITY.md](docs/PARITY.md)**.
