<div align="center">

![title](https://github.com/xjsongphy/AutoReport/blob/master/assets/screenshots/title.png?raw=1)

### 一款 codex 风格的多智能体物理实验报告命令行工具

[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg)](#)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/xjsongphy/AutoreportCLI/blob/master/LICENSE)

[English](README.md) | 中文

</div>

一款**codex 风格的多智能体命令行工具**，用于自动用 LaTeX 撰写物理实验报告。它是
[AutoReport](../AutoReport) 桌面应用的 Rust 重写版本 —— 没有 GUI，没有 MCP，没有图像识别。
终端即是界面，工作目录即是项目。

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

## 概述

AutoReportCLI 保留了 [AutoReport](../AutoReport) 的多智能体协作流程，但把整套体验重建为
Rust 编写的终端工具。你在一个实验目录中工作，放入原始数据和参考资料，然后通过一个
codex 风格的 TUI 协调五个智能体完成整份报告。

## 功能特性

### 核心能力
- **多智能体协作** — Main、Theory、Data Analysis、Plotting、Report 五个智能体分工协作
- **面向项目目录** — 每次运行都以当前工作目录为实验项目，并自动初始化标准目录结构
- **灵活的 Provider 配置** — 支持 Anthropic 与 OpenAI 兼容接口，可通过配置文件、环境变量或首次启动向导配置
- **内置默认资源** — 自带报告模板和默认 skills，新项目开箱即用

### TUI 体验
- **Codex 风格界面** — 全屏终端 UI，支持流式输出、Markdown 渲染和键盘优先操作
- **持久化会话** — 每个智能体各自维护上下文，下次启动可继续之前的对话
- **`@` 文件提及** — 在输入框中模糊搜索工作区文件并直接注入上下文
- **斜杠命令** — `/agents`、`/switch`、`/config`、`/models`、`/clear`、`/compact`、`/new`、`/manifest`、`/index`、`/help`

## 快速开始

**前置依赖：** Rust 1.85+、TeX 发行版，以及至少一个 LLM Provider 的 API Key。

从源码构建：

```bash
git clone <this-repo> AutoReportCLI && cd AutoReportCLI
cargo build --release
```

如果你希望在任意目录直接运行 `autoreport`，需要额外安装到 `PATH`：

```bash
cargo install --path autoreport-rs/cli
```

Linux 还需要安装同目录的沙箱辅助程序；缺少它时，受限的 `exec` 会失败关闭，
不会降级为未隔离执行：

```bash
cargo install --path autoreport-rs/linux-sandbox
```

或者直接运行构建产物：

```bash
./target/release/autoreport
```

创建项目目录并启动：

```bash
mkdir ~/my-experiment && cd ~/my-experiment
autoreport
```

如果 `autoreport` 不在 `PATH` 中，就使用二进制路径：

```bash
/path/to/AutoReportCLI/target/release/autoreport
```

首次启动时，AutoReportCLI 会自动创建项目目录结构、写入内置模板、同步外部预设与
skills，并打开 TUI。

## 配置

Provider 可以通过以下任一方式配置：

- 设置环境变量，例如 `ANTHROPIC_API_KEY`、`OPENAI_API_KEY`、`DEEPSEEK_API_KEY`、`OPENROUTER_API_KEY`、`GEMINI_API_KEY`
- 参考 `autoreport.config.example.yaml` 创建 `autoreport.config.yaml`
- 在首次启动时使用全屏配置页交互式完成设置
- `/config` 只配置 API；再用 `/models` 先选择 API、后填写模型名，分别绑定主 agent 与四个 sub agent。首次启动会按 API、模型的顺序打开缺失的配置页。

常用 CLI 参数：

- `--workspace <dir>` 指定工作目录
- `--no-sync` 跳过启动同步，仅使用本地缓存
- `--sync-presets` 强制刷新预设后退出
- `-v` 输出详细日志

## 工作区结构

```text
.
├── Data/            原始数据与处理结果
├── References/      论文、图片、模板、自定义 skills
├── Theory/          Theory 智能体输出
├── Plots/           绘图图表（Plots/Fig）与脚本（Plots/Scripts）
├── Tex/             LaTeX 源文件与编译后的 PDF
├── Outline/         Main 智能体的大纲与规划
└── .autoreport/     会话、同步资源、内部元数据
```

## 开发

Rust 源码采用与 Codex 一致的 workspace 组织，而不是单一的 `src/` 目录：

```text
autoreport-rs/
├── cli/          可执行程序入口
├── core/         配置、Provider、Agent、skills 与领域类型
├── protocol/     共享策略与 sandbox 协议类型
├── rollout/      兼容 Codex 的会话持久化
├── runtime/      持久 Agent loop 与编排
├── sandboxing/   跨平台执行策略
├── tools/        工具定义与本地 handler
├── tui/          终端 UI、渲染与 IDE 上下文
└── utils/        absolute-path 与 path-URI crate
```

运行测试：

```bash
cargo test
```

为当前 Rust target 构建包含原生二进制的 npm 包：

```bash
npm run build:npm
(cd autoreport-cli && npm pack --dry-run)
```

实现状态和对等清单见 **[docs/PARITY.md](docs/PARITY.md)**。
