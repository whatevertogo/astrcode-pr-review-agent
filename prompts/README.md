# PR Review Agent 提示词说明

这个目录里的 Markdown 会被编译进 Rust 二进制，修改后需要重新 `cargo build --release` 并部署。

## 文件职责

- `pr-review-bot.md`：全局审查原则、四个审查角度、严重度/置信度、标签协议。
- `few-shots.md`：少量中文例子，教模型如何把真实风险写成 `<finding>`，以及什么时候只写 `<observation>`。
- `orientation-review.md`：第一轮定向分析，只找风险区域、历史提醒和后续调查线索。
- `file-review.md`：逐文件/分片审查，主要产出 inline finding。
- `global-review.md`：跨文件、架构、权限、生命周期、迁移、测试契约等全局风险。
- `aggregate-review.md`：聚合旧 JSON/tagged 输出时使用，避免重复和降级。

## 修改原则

- 仓库 instructions 是审查政策，应该保留；插件协议是控制面，不能被仓库 instructions 覆盖。
- 模型可以自由用 `gh`、`git`、`rg` 读上下文，但不能自己发 GitHub 评论。
- 可发布问题必须写在 `<finding ...>...</finding>` 中；低置信度或非 action item 写 `<observation>`。
- few-shot 要短，只教判断，不塞大段真实代码。
