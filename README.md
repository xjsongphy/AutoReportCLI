<div align="center">

![title](https://github.com/xjsongphy/AutoReport/blob/master/assets/screenshots/title.png?raw=1)

### A Codex-Style Multi-Agent CLI for Automated Physics Experiment Report Writing

[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg)](#)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/xjsongphy/AutoreportCLI/blob/master/LICENSE)

English | [中文](README_zh.md)

</div>

A **codex-style, multi-agent command-line tool** for automatically writing
physics-experiment reports in LaTeX or Typst. It is a Rust rewrite of the
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
- **OSC-8 Hyperlinks** — file paths and links render as clickable terminal hyperlinks (codex-aligned render backend)
- **Composer History Search** — `Ctrl+R` / `Ctrl+S` reverse- and forward-i-search through prior inputs, codex-style
- **Per-Turn Metrics** — token usage and timing are shown on each turn separator
- **Codex Approval Prompts** — command-execution approvals use the codex keymap (`y` / `a` / `p` / `d` / `Esc` / `n` / `c`)
- **Persistent Agent Sessions** — each agent keeps its own conversation history and resumes on the next launch
- **`@` File Mentions** — fuzzy-search workspace files and inject them into prompts directly from the input box
- **Slash Commands** — `/model`, `/env`

## Quick Start

**Prerequisites:** Rust 1.85+, a TeX distribution, and at least one LLM provider API key.

Build from source:

```bash
git clone <this-repo> AutoReportCLI && cd AutoReportCLI
cargo build --locked --release
```

For the fastest development loop, use `cargo run -p autoreport-cli` or
`cargo build -p autoreport-cli`. Formal release payloads use the separate
`dist` profile:

```bash
cargo build --locked --profile dist -p autoreport-cli
```

To inspect compilation time by crate, add `--timings`; Cargo writes the report
to `target/cargo-timings/cargo-timing.html`. Avoid running `cargo clean` before
every build, because that removes the cache the next build would reuse.

Install globally if you want `autoreport` available from any directory:

```bash
cargo install --locked --path autoreport-rs/cli
```

On Linux, install the companion sandbox launcher into the same Cargo bin
directory as well. Restricted `exec` commands fail closed if it is absent:

```bash
cargo install --locked --path autoreport-rs/linux-sandbox
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

On first launch, AutoReportCLI configures the API and models, selects the global
Python environment and the project's report language, asks you to confirm the
workspace, then creates the workspace folders and opens the TUI.
Built-in templates and synced presets/skills live in the global AutoReport home.

## Configuration

Configure a provider in any of these ways:

- Set an API key environment variable such as `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`, or `GEMINI_API_KEY`
- Copy `autoreport.config.example.toml` to `$AUTOREPORT_HOME/config.toml` (default:
  `~/.autoreport/config.toml`), or use `/model`. `AUTOREPORT_HOME` can point to
  another global AutoReport home.
- Use `/model` to configure API entries and bind main/sub-agent model names. Use `/env` to configure Python and the current project's LaTeX/Typst language.
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
├── Report/             active LaTeX/Typst sources and compiled PDF
├── Outline/         main agent planning output
└── (no AutoReport metadata files; program state lives in ~/.autoreport/)
```

Global program state follows Codex's home-directory model:

```text
~/.autoreport/
├── config.toml
├── auth.json         provider credentials (mode 0600 where supported)
├── history.jsonl                       append-only conversation history
├── environment.toml                    global Python environment
├── venv/                               AutoReport-managed Python environment (optional)
├── resources/       language-specific bundled and synced skills/templates/themes
├── external/providers/  synced provider presets
├── agents/          global prompt overrides
└── workspaces/<id>/                    manifests, rules, project.toml, metadata
```

## Development

The Rust source follows Codex's workspace layout rather than a monolithic
`src/` tree:

```text
autoreport-rs/
├── cli/                  executable entry point
├── core/                 configuration, providers, agents, skills, domain types
├── runtime/              persistent agent loops and orchestration
├── tui/                  terminal UI, OSC-8 rendering, IDE context
├── tools/                tool definitions and local handlers
├── shell-command/        codex-aligned shell/exec parsing
├── protocol/             shared policy and sandbox protocol types
├── codex-protocol/       vendored Codex protocol types (from codex-rs)
├── app-server-protocol/  vendored app-server protocol + schema fixtures
├── app-server-transport/ stdio / unix-socket / websocket transport
├── uds/                  Unix domain socket transport
├── rollout/              Codex-compatible session persistence
├── sandboxing/           cross-platform execution policy (seatbelt / bwrap / landlock)
├── linux-sandbox/        Linux sandbox launcher (bwrap + seccomp)
├── bwrap/ · windows-sandbox/   platform sandbox helpers
├── network-proxy/        managed network proxy and MITM policy
├── execpolicy/           starlark exec-policy rule engine
└── utils/                absolute-path, path-uri, home-dir, pty, image, …
```

Run tests with:

```bash
cargo test --locked
```

CI (`/.github/workflows/ci.yml`) uses one platform matrix: pull requests run
formatting, Linux lint/tests, and native macOS/Windows checks. Pushes to `main`,
version tags, and manual runs additionally build and verify the Linux musl,
macOS, and Windows npm payloads. Release packaging is therefore not part of
the pull-request path.

Build an npm package with a native binary for the current Rust target:

```bash
npm run build:npm
(cd autoreport-cli && npm pack --dry-run)
```

For implementation status and parity notes, see
**[docs/PARITY.md](docs/PARITY.md)**.
