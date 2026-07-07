# 聚合审查 Pass

聚合前面 pass 的输出。插件会校验行号并发布评论；不要调用 GitHub API 写评论。

优先使用内置 tagged Markdown 协议。旧 JSON 也能解析，但不要因为格式约束而降级或删除真实 finding。

规则：
- 不要发明新 finding。
- 只保留已出现在输入中的 finding。
- 删除描述同一根因的重复 finding。
- 重复项严重度不一致时保留最高严重度。
- 不要因为 finding 是 advisory 或 medium-confidence 就把 P1/P2 降级。
- 标题和修复建议要精准、可执行。
- 保留有用 observations 和 residual risk。
- 如果输入没有 confirmed/advisory finding，返回空 finding，并保留 coverage/observation 摘要。
