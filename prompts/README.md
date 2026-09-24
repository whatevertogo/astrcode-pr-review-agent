# PR Review Agent 提示词说明

Markdown 会编译进二进制，修改后需构建并部署。`coverage_first` 与 `quality_first` 都组合共享的审查方法、证据要求和评论风格；本轮保留模型与必经全局复核，不增加按专家各调用一次模型的流程。

## 文件职责

- `review-method.md`：六个专家视角的触发条件、调查职责和输出分类。按真实变更选择适用视角，共享查证事实，按根因去重。
- `finding-evidence.md`：缺陷、非阻塞改进、待确认前提的区别，以及反证、归因和证据要求。
- `comment-style.md`：合作式中文表达、字段分工和 GitHub Markdown 约束。
- `pr-review-bot.md` / `quality-review.md`：标签 / JSON 协议与权限边界。
- `few-shots.md`：虚构的缺陷、非阻塞建议、待确认问题与跨视角去重示例。
- `orientation-review.md`：定向阶段，仅准备风险线索。
- `file-review.md`：完整检查分片及必要消费者，保留可复用事实与未决证据。
- `global-review.md`：先查分片可能遗漏的跨模块路径，再逐条核实候选；零候选也执行。
- `aggregate-review.md`：聚合兼容输出，保留成立且不重复的结论。

## 表达与兼容性

- 已证实缺陷使用 `confirmed`；有具体收益的可选改进使用 `advisory` 且 `non_blocking=true`、P3、high；缺失关键前提使用 `observation`。
- 旧记录缺省 `non_blocking=false`，原 advisory 继续显示“需要确认”，不会被重新解释为已证实的改进。只有高置信度的明确非阻塞改进由校验层固定为 P3；中低置信度仍保留原优先级并提示需要确认，不能借非阻塞标记降低风险等级。
- `non_blocking` 不改变行评数量或发布门槛。质量优先流程仍只将已复核的高置信度 P0–P2 缺陷放进行评，其他发现保留总览索引和完整详情。
- 行评使用短标题与正文，证据直接可见；总览先列行动项，再折叠覆盖、复核与验证。两种流程的统计来自程序回执，不再让最终报告模型重写。
- 这些视角是在已有阶段中的调查指令，并不代表六个独立模型或独立专家已完成验证。检查仍受上下文、工具和模型能力限制。

## 开源参考与取舍

以下来源用于研究方法，提示词根据本插件协议重新组织，不安装或执行外部 skill。查阅日期：2026-09-24。

- [Anthropic PR review toolkit](https://github.com/anthropics/claude-code/blob/main/plugins/pr-review-toolkit/commands/review-pr.md)：参考按变更选择错误处理、测试、类型等职责；本插件复用已有分片，不复制它的多代理调度。
- [Anthropic code review](https://github.com/anthropics/claude-code/blob/main/plugins/code-review/commands/code-review.md)：参考先发现、再查证候选以及一个根因一条评论。
- [Sentry security review](https://github.com/getsentry/skills/blob/main/skills/security-review/SKILL.md)：参考输入来源、可达性与已有防护核查；不采用“已认证路径不报告”的宽泛排除，仍检查对象级越权。
- [Addy Osmani code review and quality](https://github.com/addyosmani/agent-skills/blob/main/skills/code-review-and-quality/SKILL.md)：参考明确的结构改进方向、测试断言与可选建议；不采用按文件长度自动要求拆分的门槛。
- [Google review comments](https://google.github.io/eng-practices/review/reviewer/comments.html)：参考讨论代码、解释原因和明确评论意图。

不复制绝对化的风险评级、问题配额或强制重构；严重度仍由本次改动的真实影响决定。
