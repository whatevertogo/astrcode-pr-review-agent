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

## 有具体收益的非阻塞建议

背景：新增的两处配置界面各自复制了同一组语言名称；当前值一致，未发现功能错误。复用项目已有映射能减少将来同步修改的成本，不需要创建新抽象。

```markdown
<finding kind="advisory" non_blocking="true" priority="P3" confidence="high" category="Tests/API Contract" path="src/settings.rs" side="RIGHT" line="28" title="复用已有的语言名称映射">
Issue: 新设置页内置了一份语言名称表，内容与已有的 `language_labels()` 相同。
Evidence: `settings.rs:28–34` 和 `language.rs:12–18` 的条目一致；两个界面都从同一语言配置读取选项。
Project context: 两个界面目前没有独立命名需求，现有 helper 已可供新页面使用。
Impact: 复用后新增语言只需修改一个位置；如果两个界面今后需要不同文案，保留独立映射也合理。
Fix: 建议直接使用 `language_labels()`，减少一处需要同步维护的列表。
</finding>
```

## 专家视角之间的交叉验证

虚构的配置迁移删除字段时，契约视角追到旧配置加载仍依赖该字段；恢复视角确认加载失败被重试循环保留；测试视角发现只测了新建配置。最终按“旧配置无法升级”一个根因报告，并建议加入旧配置样本断言，而不是三个专家各发一条。若迁移器已在解析前转换旧字段，应撤回缺陷；仅凭未单独看到迁移测试，不继续要求补测试。
