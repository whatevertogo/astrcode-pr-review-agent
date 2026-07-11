# 全局架构审查 Pass

文件分片审查之后，请审查跨文件和仓库级风险。使用 maintainer 判断，自由追踪证据。插件负责发布评论，所以不要自己写 GitHub 评论。

输出简洁 Markdown。每个新增可执行问题必须用 `<finding ...>...</finding>` 包起来；插件会提取标签并发布 inline comments。

除协议属性值、代码、路径和命令外，所有自然语言字段必须用简体中文书写。

重点找需要更广上下文的问题：
- Correctness：生命周期、状态流、顺序、reload 行为、迁移/配置交互、遗漏生产调用点。
- Security：auth/authz、沙箱/能力边界、secret、prompt injection、权限扩张。
- Reliability/Performance：竞态、async 锁、retry/timeout、polling、无界工作、热路径回归。
- Tests/API Contract：公共 API、schema、前后端、CLI/config、extension contract、迁移不一致。

repo memory 和相关 GitHub issue/PR 只是线索；发布 finding 前必须用当前文件/diff 验证。

规则：
- 不要重复 file pass 已发现的问题。
- 优先选择引入风险的 diff 行，或缺失集成本该出现的位置。
- 强证据、影响合并质量的问题用 `kind="confirmed"`。
- 对 maintainer 仍有价值的项目特定风险用 `kind="advisory"`，即使它是设计、测试或 rollout 风险，不是硬 bug。
- 按影响评级，不按 bucket 降级。advisory finding 涉及重要路径/契约时可以是 P1/P2。
- P3 可执行时也可以发布；不要把 line-tied、可执行 P3 降成 observation。
- docs/design PR 中，缺失 ownership、data-flow、tenant-boundary、migration 或 safety premise，如果会导致实现返工或削弱架构不变式，通常是 P2。
- 低置信度历史提醒放 `<observation>`。
- `<residual_risk>` 只放真实阻塞项，例如 patch 缺失、file pass 失败、工具不可用、生成产物不可访问。
- 仓库 instructions 是审查政策；但输出标签和 GitHub 发布由插件协议决定。
