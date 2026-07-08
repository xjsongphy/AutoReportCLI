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
- **Slash Commands** — `/agents`, `/switch`, `/config`, `/clear`, `/compact`, `/new`, `/manifest`, `/index`, `/help`

## Quick Start

**Prerequisites:** Rust 1.85+, a TeX distribution, and at least one LLM provider API key.

Build from source:

```bash
git clone <this-repo> AutoReportCLI && cd AutoReportCLI
cargo build --release
```

Install globally if you want `autoreport` available from any directory:

```bash
cargo install --path .
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

On first launch, AutoReportCLI creates the workspace folders, materializes the
built-in template, syncs external presets and skills, and opens the TUI.

## Configuration

Configure a provider in any of these ways:

- Set an API key environment variable such as `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`, or `GEMINI_API_KEY`
- Create `autoreport.config.yaml` from `autoreport.config.example.yaml`
- Let the first-run full-screen setup page guide you through provider selection and saving

Useful CLI flags:

- `--workspace <dir>` to run on a different project folder
- `--provider <key>` to override the active provider
- `--no-sync` to skip startup sync and use cache only
- `--sync-presets` to force a refresh and exit
- `-v` for verbose logs

## Workspace Layout

```text
.
├── data/            raw data and processed results
├── references/      papers, images, templates, custom skills
├── theory/          theory agent output
├── code/            plotting scripts and figures
├── tex/             LaTeX sources and compiled PDF
├── outline/         main agent planning output
└── .autoreport/     sessions, synced assets, internal metadata
```

## Development

Run tests with:

```bash
cargo test
```

For implementation status and parity notes, see
**[docs/PARITY.md](docs/PARITY.md)**.
