# 判断与表达示例

以下均为虚构场景，只示范证据组织和输出协议，不是当前 PR 的事实或必找问题。

## 有明确触发路径的问题

已核对的背景：base 先写入任务记录再发送消息；head 调换顺序。消费者收到消息后立即查库，找不到记录就丢弃，且没有重试。

```markdown
<finding kind="confirmed" priority="P2" confidence="high" category="Reliability/Performance" path="src/enqueue.rs" side="RIGHT" line="48" title="先发送消息会让消费者丢弃尚未登记的任务">
Issue: 当消费者在数据库写入完成前收到消息时，会查不到任务并丢弃消息。这里将 `publish(id)` 移到了 `insert_job(id)` 之前，使这个时序变得可达。
Evidence: `enqueue.rs:48–50` 先发布再写库，base 的顺序相反；`worker.rs:70–74` 在查不到任务时直接返回，没有重新入队。
Project context: 结论基于源码中的生产消费者路径，尚未运行并发复现。
Impact: 数据库中会留下未被消费者执行的任务记录，任务无法按预期完成。
Fix: 在发布消息前完成任务写入；用一个在发布时立即消费消息的测试固定这个顺序。
</finding>
```

## 关键前提未确立，不写成确定缺陷

只看到某个生产接口删除，但仓库内调用方均已迁移。尚不知道是否承诺对外兼容：不要直接写“所有外部消费者都会崩溃”，也不要只因为没有兼容测试就给 P1/P2。

```markdown
<observation confidence="low" category="Tests/API Contract" path="src/api.rs" title="需要确认旧接口是否仍在兼容承诺内">
Evidence: 当前变更删除旧入口，仓库内已迁移到新入口；已读材料没有明确的外部兼容承诺。
Project context: 是否需要保留旧入口取决于支持范围。
Impact: 如果仍支持未迁移的外部调用者，他们需要兼容入口或迁移安排。
Next step: 核对该入口对应的版本兼容约定；若已明确允许移除，无需提出缺陷。
</observation>
```

## 有反例就撤回

怀疑初始化失败后无法恢复，但调用方每次请求前都会重试初始化：应撤回“失败后永久失效”的候选，不把它换成“建议增加更多测试”继续发布。
