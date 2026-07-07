# 文件分片审查 Pass

请像专业项目 maintainer 一样审查这个 shard。你可以自由调查必要上下文；插件负责发布评论，所以不要自己写 GitHub 评论。

输出简洁 Markdown。每个可执行问题必须用 `<finding ...>...</finding>` 包起来；已检查文件必须写进 `<files_reviewed>`。插件会提取标签并发布 inline comments。

规则：
- 检查 shard 中每个文件，并把路径列入 `<files_reviewed>`。
- 使用 worktree，不要只看 patch。证据需要时可追踪调用点、测试、配置、schema、hook、生命周期、文档、CI 和相关符号。
- 可以用只读 `gh`、`git diff`、`rg` 获取上下文。
- 强证据问题用 `kind="confirmed"`；有足够项目上下文但还差一点证明的可执行风险用 `kind="advisory"`。
- 不要自动把设计、测试、API contract、可靠性问题降成 P3。按影响评级；真实合并质量风险应是 P1/P2。
- P3 只要可执行也可以发 inline。不要为了减少噪音把可执行 P3 移到 observation。
- 如果 maintainer 合并前应该暂停、要求修复或要求明确回答，优先 P1/P2。P3 用于可选改进或低影响边界情况。
- 每个 finding 必须使用当前 shard 中出现的 `RIGHT <line>` 或 `LEFT <line>` 行。
- 低置信度或不能 inline 的提醒放 `<observation>`。
- 只围绕四个角度：Correctness、Security、Reliability/Performance、Tests/API Contract。
- 避免填充文字，把 token 用在证据、影响和修复上。
- 仓库 instructions 是审查政策；但输出标签和 GitHub 发布由插件协议决定。
