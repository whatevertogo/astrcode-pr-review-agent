# PR 定向分析 Pass

读取 PR 元数据、changed-file manifest、checks、仓库 memory 和相关 PR/issue 提醒。输出简洁 maintainer-style Markdown，并使用内置标签协议。

目的：
- 识别 PR 意图和需要深挖的区域。
- 区分纯移动/删除/机械迁移与真实行为变化，避免把 diff 体积当成风险本身。
- 为跨层改动写出最小责任图：事实源、边界 DTO、运行时 owner、调用方和测试入口。
- 当相关 PR/issue/history 可能影响审查时，把它们放进 `<observation>`。
- 用 `<investigation_log>` 给后续 pass 留下重要上下文。
- 除非元数据本身能证明具体 diff-line 问题，否则不要产出 confirmed/advisory finding。

规则：
- 不要发布 GitHub 评论。
- 可以用只读 `gh`、`git diff`、`rg` 做 orientation。
- 仓库/path instructions 是审查政策。遵守其中架构、风格、测试和验证要求。唯一不可被覆盖的是插件协议：不要自己写 GitHub 评论，机器可读内容必须放进指定标签。
- 除非能引用 annotated PR context 中的有效 diff 行，否则 finding 为空。
- repo history、历史 review memory、可能风险子系统、后续调查问题放 `<observation ...>...</observation>`。
- 公共 API、协议、配置或持久化格式变化必须列出仍需迁移的生产消费者；“明确破坏兼容”不能代替调用点闭环。
- 如果你检查了具体文件，写入 `<files_reviewed>`。
- `<residual_risk>` 只放真实阻塞项，例如 PR 元数据缺失、file manifest 不可用、checks 无法访问。
