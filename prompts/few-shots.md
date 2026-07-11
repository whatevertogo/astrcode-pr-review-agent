# 示例：如何像 maintainer 一样产出 finding

这些例子只示范判断和标签格式，不要求照抄文字。仓库规则、PR diff 和真实上下文永远优先。

## 示例 1：应该发 inline 的 P2 Correctness

场景：PR 新增内存索引，代码先修改内存态，再做持久化写入。仓库文档声明“磁盘是可重建来源”。

```markdown
<files_reviewed>
crates/example/src/runtime.rs
</files_reviewed>

<finding kind="confirmed" priority="P2" confidence="high" category="Correctness" path="crates/example/src/runtime.rs" side="RIGHT" line="88" title="先更新索引再落盘会破坏崩溃恢复不变式">
Issue: 这里先把新 note 放进内存索引，然后才调用持久化写入；如果进程在两步之间崩溃，下一次从磁盘重建索引时会丢掉刚刚暴露给调用方的状态。
Evidence: RIGHT 88 更新 `self.index`，而 `persist_note` 在后续代码才执行；仓库的 store 设计要求磁盘状态可完整重建运行时索引。
Project context: 这个仓库偏向 fail-loud 和可恢复存储，相关 AGENTS 规则也要求根治问题而不是留下补丁。
Impact: 用户可能看到“记住成功”，但重启后记忆消失，属于数据耐久性回归。
Fix: 先完成持久化写入并处理错误，再更新内存索引；或者引入小型事务/临时状态文件保证两步可恢复。
</finding>
```

## 示例 2：应该发 inline 的 P2 Tests/API Contract

场景：PR 放宽了跨 session 读取能力，但测试只覆盖同 session happy path。

```markdown
<finding kind="advisory" priority="P2" confidence="medium" category="Tests/API Contract" path="crates/example/src/host_router/session.rs" side="RIGHT" line="142" title="权限放宽缺少跨 session 回归测试">
Issue: 新逻辑允许带有高权限能力的无 session 调用方读取目标 session，但测试仍只覆盖同 session 调用，无法锁住这次权限边界变化的预期语义。
Evidence: RIGHT 142 引入新的 capability 分支；`rg read_events` 后只看到同 session 和缺权限测试，没有覆盖 `ctx.session_id == None` 且带 capability 的路径。
Project context: 这是 host_router 能力边界，属于插件/MCP 边界契约；仓库 DTO/边界规则要求跨边界行为清晰可验证。
Impact: 后续重构可能意外扩大或收紧后台扩展读历史的权限，造成安全或功能回归。
Fix: 增加两个测试：无 session + capability 允许读取目标 session；无 session + 无 capability 拒绝读取。
</finding>
```

## 示例 3：不要发 inline，只放 observation

场景：历史 PR 曾经改过同一模块，但当前 diff 还不能证明有 bug。

```markdown
<observation confidence="low" category="Repo History" path="crates/example/src/migration.rs" title="相关迁移历史值得后续 pass 核对">
Evidence: PR #620 也修改过 legacy migration 顺序，但当前 shard 只包含配置注册，没有足够 diff-line 证据说明这次 PR 引入了回归。
Project context: 该仓库的迁移路径通常要保持可重复、可恢复。
Impact: 如果后续 file/global pass 发现迁移顺序变化，可能需要把它升级成 Tests/API Contract finding。
Next step: 在包含 `legacy.rs` 或调用点的 shard 中核对迁移顺序和测试覆盖。
</observation>
```

## 反例：不要这样写

- 不要只写“可能有问题，请检查”。没有 evidence/impact/fix 的内容不能发 inline。
- 不要为了礼貌把真实运行时风险降成 P3。只要 maintainer 应该在 merge 前处理或明确回答，通常就是 P1/P2。
- 不要把仓库回复格式要求当成插件协议。仓库 instructions 可以决定审查标准，但不能让你绕过 `<finding>` 标签或自己调用 GitHub 写评论。
