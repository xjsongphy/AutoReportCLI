<div align="center">

![title](https://github.com/xjsongphy/AutoReport/blob/master/assets/screenshots/title.png?raw=1)

### 一款 codex 风格的多智能体物理实验报告命令行工具

[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg)](#)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/xjsongphy/AutoreportCLI/blob/master/LICENSE)

[English](README.md) | 中文

</div>

一款**codex 风格的多智能体命令行工具**，用于自动用 LaTeX 或 Typst 撰写物理实验报告。它是
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
- **OSC-8 超链接** — 文件路径与链接渲染为可点击的终端超链接（codex 对齐的渲染后端）
- **输入历史搜索** — `Ctrl+R` / `Ctrl+S` 对历史输入做反向 / 正向增量搜索，codex 风格
- **逐轮指标** — 每个回合分隔条上展示 token 用量与耗时
- **Codex 审批键位** — 命令执行审批使用 codex 键位（`y` / `a` / `p` / `d` / `Esc` / `n` / `c`）
- **持久化会话** — 每个智能体各自维护上下文，下次启动可继续之前的对话
- **`@` 文件提及** — 在输入框中模糊搜索工作区文件并直接注入上下文
- **斜杠命令** — `/model`、`/env`

## 快速开始

**前置依赖：** Rust 1.85+、TeX 发行版，以及至少一个 LLM Provider 的 API Key。

从源码构建：

```bash
git clone <this-repo> AutoReportCLI && cd AutoReportCLI
cargo build --locked --release
```

开发时建议使用 `cargo run -p autoreport-cli` 或
`cargo build -p autoreport-cli`，这样不会反复执行发行级链接优化。正式发行包使用独立的
`dist` profile：

```bash
cargo build --locked --profile dist -p autoreport-cli
```

需要分析各 crate 的编译耗时时，在命令后加入 `--timings`；报告会写入
`target/cargo-timings/cargo-timing.html`。不要在每次编译前执行 `cargo clean`，否则会删除下一次编译本可复用的缓存。

如果你希望在任意目录直接运行 `autoreport`，需要额外安装到 `PATH`：

```bash
cargo install --locked --path autoreport-rs/cli
```

Linux 还需要安装同目录的沙箱辅助程序；缺少它时，受限的 `exec` 会失败关闭，
不会降级为未隔离执行：

```bash
cargo install --locked --path autoreport-rs/linux-sandbox
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

首次启动时，AutoReportCLI 会先完成 API 和模型配置并确认工作区；确认后才创建项目目录结构并打开
TUI。内置模板和同步的预设/skills 保存在全局 AutoReport home 中。

## 配置

Provider 可以通过以下任一方式配置：

- 设置环境变量，例如 `ANTHROPIC_API_KEY`、`OPENAI_API_KEY`、`DEEPSEEK_API_KEY`、`OPENROUTER_API_KEY`、`GEMINI_API_KEY`
- 将 `autoreport.config.example.toml` 复制为全局
  `~/.autoreport/config.toml`（也可通过 `AUTOREPORT_HOME` 修改位置），或直接使用
  `/model` 配置。
- 在首次启动时使用全屏配置页交互式完成设置
- `/model` 配置 API 和 Main/Sub 模型；`/env` 配置全局 Python 环境以及当前项目的 LaTeX/Typst 报告语言。

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
├── Report/             当前 LaTeX/Typst 源文件与编译后的 PDF
├── Outline/         Main 智能体的大纲与规划
└── （不再写入 AutoReport 隐藏目录；程序状态统一保存在 ~/.autoreport/）
```

全局程序状态目录与 Codex 的 home 模型对齐：

```text
~/.autoreport/
├── config.toml                         配置
├── auth.json                           Provider 凭据（支持的平台上权限为 0600）
├── history.jsonl                       追加式对话历史
├── environment.toml                    全局 Python 环境
├── venv/                               AutoReport 管理的 Python 环境（可选）
├── resources/                          按语言隔离的 skills、模板、主题
├── external/providers/                 同步的 Provider 预设
├── agents/                             全局提示词覆盖
└── workspaces/<id>/                    项目 manifest、规则、project.toml 等状态
```

## 开发

Rust 源码采用与 Codex 一致的 workspace 组织，而不是单一的 `src/` 目录：

```text
autoreport-rs/
├── cli/                  可执行程序入口
├── core/                 配置、Provider、Agent、skills 与领域类型
├── runtime/              持久 Agent loop 与编排
├── tui/                  终端 UI、OSC-8 渲染、IDE 上下文
├── tools/                工具定义与本地 handler
├── shell-command/        codex 对齐的 shell/exec 解析
├── protocol/             共享策略与 sandbox 协议类型
├── codex-protocol/       vendored Codex 协议类型（来自 codex-rs）
├── app-server-protocol/  vendored app-server 协议 + schema fixture
├── app-server-transport/ stdio / unix socket / websocket 传输层
├── uds/                  Unix domain socket 传输
├── rollout/              兼容 Codex 的会话持久化
├── sandboxing/           跨平台执行策略（seatbelt / bwrap / landlock）
├── linux-sandbox/        Linux 沙箱启动器（bwrap + seccomp）
├── bwrap/ · windows-sandbox/   平台沙箱辅助
├── network-proxy/        托管网络代理与 MITM 策略
├── execpolicy/           starlark exec-policy 规则引擎
└── utils/                absolute-path、path-uri、home-dir、pty、image 等
```

运行测试：

```bash
cargo test --locked
```

CI（`/.github/workflows/ci.yml`）使用统一的平台矩阵：Pull Request 运行格式检查、
Linux lint/测试以及 macOS/Windows 原生检查。推送到 `main`、版本 tag 或手动触发时，
还会构建并校验 Linux musl、macOS 和 Windows 的 npm payload；release 打包不会进入
Pull Request 流程。

为当前 Rust target 构建包含原生二进制的 npm 包：

```bash
npm run build:npm
(cd autoreport-cli && npm pack --dry-run)
```

实现状态和对等清单见 **[docs/PARITY.md](docs/PARITY.md)**。
