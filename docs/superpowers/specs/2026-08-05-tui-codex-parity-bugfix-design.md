# TUI Codex parity bugfix design

## Goal

修复 AutoReport TUI 与上级 Codex 实现之间已经暴露的渲染、弹层布局和模型配置问题，同时尽量复用 Codex 源码，避免为 AutoReport 重新实现一套表现层逻辑。

本轮重点是截图所示的对话历史记录：恢复 Codex 的工具调用呼吸间距，并消除本地终端写入过程中普通文本被错误弱化的问题。

## 结论与范围

1. `Main` 与 `Sub agents (all 4)` 继续作为两个独立的模型绑定目标。当前“只显示 Main”按渲染/信息展示缺陷处理，不改变已有配置语义。
2. Markdown 使用上级 Codex 的完整表格布局实现：启用 GFM tables、表头/分隔线/单元格宽度计算、换行和主题样式。AutoReport 只保留必要的模块路径适配。
3. Agent 输出中的 `md`/`markdown` 围栏表格采用 Codex 的保守解围栏规则；普通代码围栏不解围栏。这样同时覆盖原始 Agent 输出和落盘后重放。
4. 普通 Agent 正文使用明确的亮色前景，工具输出继续保留 Codex 的弱化层级；标题、代码、链接、表格表头等语义颜色不被覆盖。
5. 模型页只编辑模型标识：Main 与 Sub 两行目标都可见，Provider 作为 API 页的只读上下文，不再进入额外的预览确认页；保存直接完成当前模型流程。
6. 模型保存继续写入 `config.toml`，同时立即替换对应运行时 role 的 provider handle；正在进行的 stream 保持原 provider，下一轮 turn 使用新模型，不要求重启。
7. 菜单、模型页进入/返回时沿用 Codex 的全帧清理与 viewport 重置约束，并增加可复现的缓冲区渲染测试，保证 popup 不侵入 composer/footer。

## 本轮对话历史视觉对齐

### 现状差异

- Codex 的 `ExecCell`/`CompositeHistoryCell` 把每个工具调用当作独立子 cell，并在相邻调用之间插入一行空白；AutoReport 的 `ToolGroup` 包含多个 `ToolEntry`，历史滚屏、当前 viewport 和超链接路径需要共享同一条组合规则。
- Codex 的历史写入器会在 span 之间维护完整的颜色与 modifier 状态，既能清除 DIM，也能保留语义颜色；AutoReport 当前写入器只重置行首并追加 modifier，容易让默认前景或 DIM 状态影响后续文本。

### 设计

保留 AutoReport 的 `Cell`、`ToolGroup`、事件关联和 provider 协议，不引入 Codex 的完整运行时类型。将 Codex 的历史 cell 组合语义直接映射到本地 `ToolGroup`：每个 `ToolEntry` 通过现有的 exec/patch/MCP renderer 生成一个子 cell，子 cell 之间恰好插入一行空白；普通历史、超链接历史和当前 viewport 都调用同一个组合入口，避免三条路径再次分叉。

将上级 Codex `insert_history.rs` 的 `write_history_line`、span 颜色/背景切换和 modifier 增删状态机移植到本地。只适配本地 `HyperlinkLine`、终端 backend 和现有 ANSI 装饰接口。普通 assistant/user/命令标题保持正常前景色；命令语法色、链接色、成功/失败色和 DIM 的工具输出继续由各自 cell 的语义样式控制，不使用全局强制白色覆盖。

### 数据流与边界

`Cell`/`ToolGroup` → 统一的历史 cell 行组合 → 宽度适配与 hyperlink 标注 → Codex 风格 ANSI 写入器。工具输出仍保持 DIM，空白行不携带前一行的 style；终端写入完成后恢复默认颜色与 modifier，避免影响 composer 或下一帧。

### 验证

- 两个相邻工具调用之间恰好出现一行空白；工具输出本身含空行时，下一调用前仍保留边界空白。
- 普通 assistant/user/命令标题不继承 DIM；stdout/stderr 和树形前缀仍为 DIM，语法高亮和状态色仍保持原色。
- 普通历史、hyperlink 历史、viewport 渲染三者产生相同的可见行结构；ANSI 写入器在相邻 span 由 DIM 切换回普通样式时生成正确的属性清除序列。
- 运行相关 `autoreport-tui` 测试、`cargo fmt --check` 与 `cargo clippy`/构建检查。

## 实现方式

- 从 `../codex/codex-rs/tui/src/markdown_render.rs` 搬运表格渲染代码及其直接依赖，只做当前 crate 的模块路径和依赖版本适配。
- 在 Agent Markdown cell 的最终渲染入口调用围栏规范化；不修改 Provider 通信协议或 Agent 的原始输出。
- 在 `model_migration.rs` 删除 `Preview` step，保留现有 `Target -> Model` 编辑路径和 `s` 保存快捷键；补充 Main/Sub 同时可见的渲染回归测试。
- 在 runtime 增加两个可替换 provider handle（Main、共享 Sub），配置保存时先构造完整的新 provider，再原子替换两个句柄。
- 检查并修正 overlay 离开后的 viewport 生命周期；用固定尺寸 `WritableTestBackend` 覆盖 popup/footer 和配置页返回后的首帧。

## 验收标准

- 原始 pipe table 和 ` ```markdown ` table 都显示为 box-drawing 表格，而不是原始管线文本或代码块。
- 普通 Agent 正文不再被渲染成灰色弱化文本；工具输出的弱化颜色仍然存在。
- 模型页同时显示 Main 和 Sub，不能编辑的 Provider 不出现在确认项中，保存不再跳到独立预览页。
- `/model` 保存后配置文件包含两个模型字段；当前运行时下一轮 turn 使用新模型，不需要重启。
- slash 菜单和 model overlay 的 popup/footer 不越界、不覆盖、不留下上一页残影。
- `cargo test -p autoreport-tui --lib --tests`、格式检查和相关渲染测试通过。
