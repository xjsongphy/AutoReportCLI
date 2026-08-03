# AutoReportCLI 编译过程优化设计

## 背景与问题

AutoReportCLI 是一个包含大量 workspace crate、TUI 依赖和内嵌资源的 Rust 应用。当前根 `Cargo.toml` 将 release profile 配置为 `lto = "thin"`，但 `.cargo/config.toml` 又对所有 release 构建强制设置了 Fat LTO、`codegen-units = 1` 和关闭增量编译。实际的本地 `cargo install --path autoreport-rs/cli` 因此接近发行包构建，而不是适合开发循环的构建。

当前工作区的构建缓存约为 116GB，其中 debug 增量缓存约 74GB；这表明缓存和构建 profile 的边界也需要明确。仓库还忽略 `Cargo.lock`，而 README、CI 和 npm 打包脚本都把 `--release` 当作唯一的优化构建入口。

`autoreport-core` 直接通过 `include_str!` 嵌入模板、agent prompt 和 skills。开发时修改一份 Markdown、Typst 或 LaTeX 文件会使 core 失效，并传递到 runtime、TUI 和 CLI。发行包仍需要自包含资源，因此不能简单地删除内嵌资源。

## 目标

1. 日常 `cargo run -p autoreport-cli` 和普通 release 构建不再执行 Fat LTO + 单 codegen unit。
2. 正式发行构建仍可使用 Fat LTO、单 codegen unit 和 strip，保持现有发行质量与自包含资源行为。
3. 开发构建修改模板、skills 或 agent prompt 时，不再因为 `include_str!` 触发 core 的资源重编译；开发运行时读取仓库源文件，发行构建继续读取内嵌副本。
4. CI 和 npm 打包使用固定依赖和明确的发行 profile，构建结果可复现。
5. 通过显式 Tokio feature 保留当前 workspace 使用到的 API，同时避免 `features = ["full"]` 带来的无差别启用。
6. 提供清晰的开发、普通 release、发行构建和构建耗时分析命令。

## 非目标

- 本次不删除语法高亮，也不把 `syntect`/`two-face` 改为默认关闭的可选功能。是否值得这样做由 profile 调整后的 timings 数据决定。
- 不在编译前自动执行 `cargo clean`，也不删除现有 `target`、Cargo registry 或用户生成文件。
- 不改变资源覆盖优先级、workspace 目录布局、TUI 功能或发行包的文件布局。
- 不重构与编译耗时无关的现有用户改动。

## 方案与选择

考虑过三种方案：

1. 只修改 release profile。风险低、收益直接，但不能处理模板资源造成的开发期失效。
2. 分层优化：拆分普通 release 与发行 profile，固定 lockfile，收窄 Tokio feature，调整 CI/npm/README，并为开发构建增加源码资源路径。该方案能同时覆盖主要编译瓶颈和高频的资源失效问题，且不改变发行行为。
3. 进一步把语法高亮做成可选 feature，开发构建关闭全部语法数据库。该方案可能继续降低编译量，但会改变开发版运行能力，且需要较大范围的 TUI 条件编译。

本设计采用方案 2；方案 3 留待 timings 证明必要后单独设计。

## 设计

### 1. Cargo profile 分层

根 `Cargo.toml` 保留优化级别 3，但把普通 release 设为快速链接配置，并新增发行 profile：

```toml
[profile.release]
opt-level = 3
lto = false
codegen-units = 16
strip = "symbols"

[profile.dist]
inherits = "release"
lto = "fat"
codegen-units = 1
strip = "symbols"
```

`.cargo/config.toml` 只保留通用增量构建和较轻的 debug 信息：

```toml
[build]
incremental = true

[profile.dev]
debug = 1
```

这样普通 `release` 不再被配置文件覆盖；`dist` 是唯一明确承担 Fat LTO 和单 codegen unit 成本的 profile。release profile 的 `incremental = false` 由 Cargo 默认值承担，避免为发行构建保存增量缓存。

命令约定如下：

| 场景 | 命令 | 说明 |
|---|---|---|
| 日常运行 | `cargo run -p autoreport-cli` | dev profile，最快的编辑-运行循环 |
| 日常检查 | `cargo check -p autoreport-cli` | 不链接最终二进制 |
| 本地优化运行 | `cargo build --release -p autoreport-cli` | 普通 release，不使用 Fat LTO |
| 正式发行 | `cargo build --profile dist -p autoreport-cli` | 完整优化、strip、自包含资源 |
| 构建诊断 | `cargo build -p autoreport-cli --timings` | 输出 `target/cargo-timings/cargo-timing.html` |

`cargo install --path autoreport-rs/cli` 继续作为全局安装命令，但它使用普通 release；开发阶段文档优先引导 `cargo run` 或直接运行 `target/debug/autoreport`。

### 2. 可复现依赖与 CI/npm 入口

移除根 `.gitignore` 对 `Cargo.lock` 的忽略，并把现有 workspace lockfile 纳入版本控制。不要重新解析依赖版本，除非 lockfile 确实与 manifest 不一致。所有需要解析依赖的 CI 构建、检查和测试命令增加 `--locked`。

CI 的 package job 以及 `autoreport-cli/scripts/build_npm_package.py` 改用 `--profile dist`，并从 `target/<target>/dist/` 读取二进制。这样本地普通 release 的加速不会降低正式 npm payload 的优化质量。debug 检查和测试继续使用 dev profile，不强行使用 dist。

README 和脚本说明要明确区分：

- 开发运行使用 `cargo run -p autoreport-cli`；
- 本地需要优化但不打包时使用 `cargo build --release`；
- 发行包和跨平台 npm payload 使用 `--profile dist`；
- 只有需要安装到 PATH 时才使用 `cargo install`；
- 需要排查慢点时使用 `--timings`，而不是先清理整个 workspace。

### 3. Tokio feature 收窄

将 workspace dependency 中的 Tokio 从 `features = ["full"]` 改为当前代码和测试实际使用的集合：

```toml
tokio = { version = "1", features = [
  "fs",
  "io-std",
  "io-util",
  "macros",
  "net",
  "process",
  "rt-multi-thread",
  "signal",
  "sync",
  "test-util",
  "time",
  "tracing",
] }
```

`test-util` 保留是因为 workspace/vendor 测试使用 `#[tokio::test(start_paused = true)]` 等测试能力。修改后用 `cargo check --workspace --all-targets` 和现有测试覆盖所有 API，若编译器发现遗漏 feature，只补充对应 feature，不恢复 `full`。

### 4. 开发期源码资源与发行期嵌入资源分离

新增 core 内部资源加载层，统一服务 `bundled.rs`、`prompts` 和 `sync.rs`。每个资源条目显式包含两个路径：源码树中的 `templates/...` 路径，以及写入 AutoReport home 时使用的 `resources/...` 输出路径；prompt 和 sync 使用源码路径，materialize 使用这两个字段完成读取与写入：

- `cfg(debug_assertions)` 下，根据 `CARGO_MANIFEST_DIR` 定位仓库根目录，读取 `templates/...` 源文件；读取失败时返回明确的路径错误或保留当前调用点的 warning/fallback 语义。
- `cfg(not(debug_assertions))` 下，使用当前 `include_str!` 内容，确保 release/dist/安装包不依赖源码目录。
- 资源输出路径仍使用当前 `resources/...` 布局，默认资源只在目标文件不存在时 materialize，不覆盖用户修改。
- prompt 覆盖优先级保持不变：项目覆盖 > 全局覆盖 > 默认资源。
- 发行构建中的内嵌资源清单和内容不减少；开发构建只改变默认资源的来源，不改变外部行为。

为避免 debug 分支仍被 `include_str!` 跟踪，所有 `include_str!` 必须位于 `#[cfg(not(debug_assertions))]` 的模块或函数中，不能仅在运行时分支中包裹。资源加载层应提供统一的 `Cow<'static, str>` 或等价接口，让 release 使用静态借用、debug 使用文件读取结果。

### 5. 错误处理与兼容性

- 发行构建继续保证新环境首次启动能够 materialize 内嵌默认资源。
- debug 构建找不到源码资源时，错误必须包含实际尝试的绝对路径和资源相对路径，便于从错误的工作树运行时定位问题。
- 资源读取失败不能静默写入空文件；materialize 的现有 warning 行为可以保留，但必须跳过失败项。
- 不覆盖已经存在的用户资源，也不改变外部 `$AUTOREPORT_HOME` 目录结构。

## 验证策略

### 静态验证

1. `cargo metadata --locked --no-deps --format-version 1` 成功。
2. `rg` 确认 `.cargo/config.toml` 不再有 `[profile.release]` 覆盖项，根 `Cargo.toml` 同时包含 `release` 和 `dist` profile。
3. `rg` 确认 CI、npm 打包脚本和 README 使用了正确的 profile；package job 不再从 `release/` 读取发行 payload。
4. `cargo tree -e features` 确认 Tokio 不再通过 workspace 直接启用 `full`。
5. `git check-ignore Cargo.lock` 不再返回根 `.gitignore` 规则，且 lockfile 被 Git 跟踪。

### 构建和测试

```bash
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked -p autoreport-core
cargo test --locked -p autoreport-tui
cargo build --locked --release -p autoreport-cli
cargo build --locked --profile dist -p autoreport-cli
```

在 debug 构建成功后修改一份模板或 skill，重复 `cargo check -p autoreport-cli`，确认资源修改不会因为 `include_str!` 使 core 重新编译；运行一次 debug CLI 验证新内容被读取。再用 release/dist 二进制在临时 `AUTOREPORT_HOME` 下启动，确认默认资源仍可 materialize。

### 性能证据

在清洁度相同且不执行 `cargo clean` 的前提下，对比一次普通 release 与旧配置的 timings；重点记录最终链接阶段、`autoreport-core`、`autoreport-tui` 和包含大型语法数据库的依赖耗时。若 profile 调整后语法高亮仍是主要瓶颈，再为它单独提出 feature-split 设计。

## 变更边界

预计修改：根 `Cargo.toml`、`.cargo/config.toml`、`.gitignore`、`Cargo.lock`、README 中英文本、CI workflow、npm 构建脚本，以及 `autoreport-rs/core` 的资源加载相关文件。所有现有未提交业务和 TUI 改动都不属于本设计，实施时只对构建入口和资源加载边界做最小必要修改。
