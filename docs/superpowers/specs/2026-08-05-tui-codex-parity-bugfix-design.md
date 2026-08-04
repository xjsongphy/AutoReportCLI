# TUI Codex parity bugfix design

## Goal

修复 AutoReport TUI 与上级 Codex 实现之间已经暴露的渲染、弹层布局和模型配置问题，同时尽量复用 Codex 源码，避免为 AutoReport 重新实现一套表现层逻辑。

## 结论与范围

1. `Main` 与 `Sub agents (all 4)` 继续作为两个独立的模型绑定目标。当前“只显示 Main”按渲染/信息展示缺陷处理，不改变已有配置语义。
2. Markdown 使用上级 Codex 的完整表格布局实现：启用 GFM tables、表头/分隔线/单元格宽度计算、换行和主题样式。AutoReport 只保留必要的模块路径适配。
3. Agent 输出中的 `md`/`markdown` 围栏表格采用 Codex 的保守解围栏规则；普通代码围栏不解围栏。这样同时覆盖原始 Agent 输出和落盘后重放。
4. 普通 Agent 正文使用明确的亮色前景，工具输出继续保留 Codex 的弱化层级；标题、代码、链接、表格表头等语义颜色不被覆盖。
5. 模型页只确认可编辑的模型标识：两行目标都可见，Provider 作为 API 页的只读上下文，不再进入单独的 YAML 预览确认页；保存直接完成当前模型流程。
6. 模型保存继续写入 `config.toml`。当前运行中的 Provider/Agent loop 若由启动参数构造，则提示重启后生效，避免状态栏把“已保存配置”误报成“当前运行模型”。
7. 菜单、模型页进入/返回时沿用 Codex 的全帧清理与 viewport 重置约束，并增加可复现的缓冲区渲染测试，保证 popup 不侵入 composer/footer。

## 实现方式

- 从 `../codex/codex-rs/tui/src/markdown_render.rs` 搬运表格渲染代码及其直接依赖，只做当前 crate 的模块路径和依赖版本适配。
- 在 Agent Markdown cell 的最终渲染入口调用围栏规范化；不修改 Provider 通信协议或 Agent 的原始输出。
- 在 `model_migration.rs` 删除 `Preview` step，保留现有 `Target -> Model` 编辑路径和 `s` 保存快捷键；补充 Main/Sub 同时可见的渲染回归测试。
- 检查并修正 overlay 离开后的 viewport 生命周期；用固定尺寸 `WritableTestBackend` 覆盖 popup/footer 和配置页返回后的首帧。

## 验收标准

- 原始 pipe table 和 ` ```markdown ` table 都显示为 box-drawing 表格，而不是原始管线文本或代码块。
- 普通 Agent 正文不再被渲染成灰色弱化文本；工具输出的弱化颜色仍然存在。
- 模型页同时显示 Main 和 Sub，不能编辑的 Provider 不出现在确认项中，保存不再跳到独立预览页。
- `/model` 保存后配置文件包含两个模型字段；UI 明确说明重启生效（若当前运行时未重建 loop）。
- slash 菜单和 model overlay 的 popup/footer 不越界、不覆盖、不留下上一页残影。
- `cargo test -p autoreport-tui --lib --tests`、格式检查和相关渲染测试通过。
