//! Orientation proposes investigation boundaries; deterministic planning owns coverage.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReviewChain {
    pub behavior: String,
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub related_paths: Vec<String>,
    #[serde(default)]
    pub checks: Vec<String>,
}

// Planning hints are optional. A malformed hint must not invalidate actual findings.
pub(crate) fn deserialize_chains<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<ReviewChain>, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| serde_json::from_value(item.clone()).ok())
        .collect())
}

#[derive(Debug, Serialize)]
pub(super) struct BehaviorPlan {
    pub shards: Vec<ReviewShard>,
    pub chains: Vec<ReviewChain>,
    pub notes: Vec<String>,
}

pub(super) fn plan(
    config: &Config,
    context: &ReviewContext,
    overhead: usize,
    hints: &[ReviewChain],
    max_passes: usize,
) -> Result<BehaviorPlan> {
    let baseline = review_planning::plan(config, context, overhead)?;
    let eligible: BTreeSet<_> = baseline
        .iter()
        .flat_map(|s| s.files.iter().map(|f| f.path.clone()))
        .collect();
    let mut chains = Vec::new();
    let mut notes = Vec::new();
    let mut used = 0;
    let allowance = config.review_context_max_bytes / 8;
    for hint in hints {
        let size = serde_json::to_string(hint)?.len();
        if hint.behavior.trim().is_empty()
            || hint.changed_paths.is_empty()
            || !hint.changed_paths.iter().all(|p| eligible.contains(p))
            || !hint.related_paths.iter().all(|p| safe_relative_path(p))
            || used + size > allowance
        {
            notes.push(
                "Ignored invalid or over-budget chain hint; its files remain in the manifest."
                    .into(),
            );
            continue;
        }
        let mut hint = hint.clone();
        hint.changed_paths.sort();
        hint.changed_paths.dedup();
        hint.related_paths.sort();
        hint.related_paths.dedup();
        used += size;
        chains.push(hint);
    }
    if chains.is_empty() {
        return Ok(BehaviorPlan {
            shards: baseline,
            chains,
            notes,
        });
    }

    // Overlapping chains form one work unit, so a shared file is not reviewed twice.
    let mut groups: Vec<BTreeSet<String>> = Vec::new();
    for chain in &chains {
        let mut group: BTreeSet<_> = chain.changed_paths.iter().cloned().collect();
        let mut index = 0;
        while index < groups.len() {
            if !group.is_disjoint(&groups[index]) {
                group.extend(groups.remove(index));
                index = 0;
            } else {
                index += 1;
            }
        }
        groups.push(group);
    }
    let assigned: BTreeSet<_> = groups.iter().flatten().cloned().collect();
    let remaining: BTreeSet<_> = eligible.difference(&assigned).cloned().collect();
    let mut shards = Vec::new();
    for group in groups {
        let focus = render(
            &chains
                .iter()
                .filter(|c| c.changed_paths.iter().any(|p| group.contains(p)))
                .cloned()
                .collect::<Vec<_>>(),
        );
        let mut subset = context.clone();
        subset.files.retain(|f| group.contains(&f.path));
        // Preserve the proven docs-consumer pass for large mixed groups. Every
        // resulting shard still receives the same cross-file investigation chain.
        match review_planning::plan(config, &subset, overhead + focus.len()) {
            Ok(parts) => shards.extend(parts.into_iter().map(|mut s| {
                s.focus = focus.clone();
                s
            })),
            Err(_) => {
                notes.push("Chain context exceeded the request budget; retained the ordinary complete-patch plan.".into());
                return Ok(BehaviorPlan {
                    shards: baseline,
                    chains,
                    notes,
                });
            }
        }
    }
    let mut subset = context.clone();
    subset.files.retain(|f| remaining.contains(&f.path));
    shards.extend(review_planning::plan(config, &subset, overhead)?);
    if shards.len() > max_passes && shards.len() > baseline.len() {
        notes.push("Chain grouping would reduce file coverage under the pass budget; used the ordinary plan.".into());
        shards = baseline;
    }
    for (index, shard) in shards.iter_mut().enumerate() {
        shard.index = index;
    }
    Ok(BehaviorPlan {
        shards,
        chains,
        notes,
    })
}

fn safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\n', '\r'])
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

pub(super) fn render(chains: &[ReviewChain]) -> String {
    if chains.is_empty() {
        return String::new();
    }
    format!("行为链调查线索（不是既定结论，也不是阅读边界）：\n```json\n{}\n```\n沿行为变化追入口、消费者、失败路径与断言；按需要读取相关未修改代码。先核实线索和反例，不把计划列出的风险直接当作 finding。\n",
        serde_json::to_string(chains).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_group_related_changes_without_claiming_or_losing_coverage() {
        let mut context = crate::tests::test_review_context();
        let original = context.files[0].clone();
        context.files = [
            "src/cancel.rs",
            "src/task.rs",
            "tests/cancel.rs",
            "docs/cancel.md",
            "src/unrelated.rs",
        ]
        .iter()
        .map(|path| ReviewFileContext {
            path: (*path).into(),
            ..original.clone()
        })
        .collect();
        let chain = |paths: &[&str]| ReviewChain {
            behavior: "cancel lifecycle".into(),
            changed_paths: paths.iter().map(|p| (*p).into()).collect(),
            related_paths: vec!["src/unchanged_caller.rs".into()],
            checks: vec!["does cancellation reach the consumer?".into()],
        };
        let config = Config::default();
        let hints = [
            chain(&["src/cancel.rs", "src/task.rs"]),
            chain(&["src/task.rs", "tests/cancel.rs", "docs/cancel.md"]),
            chain(&["not-in-diff.rs"]),
        ];
        let plan = plan(&config, &context, 100, &hints, 20).unwrap();
        assert_eq!(plan.chains.len(), 2);
        assert_eq!(plan.notes.len(), 1);
        assert_eq!(plan.shards.len(), 2);
        assert_eq!(plan.shards[0].files.len(), 4);
        assert!(plan.shards[0].focus.contains("src/unchanged_caller.rs"));
        assert_eq!(plan.shards[1].files[0].path, "src/unrelated.rs");
        let paths: Vec<_> = plan
            .shards
            .iter()
            .flat_map(|s| s.files.iter().map(|f| f.path.as_str()))
            .collect();
        assert_eq!(paths.len(), 5);
        assert_eq!(paths.iter().collect::<BTreeSet<_>>().len(), 5);
        let mut completed = BTreeMap::new();
        for shard in &plan.shards {
            assert!(shard.bytes + shard.focus.len() + 100 <= config.review_context_max_bytes);
            completed.insert(
                shard.index,
                shard.files.iter().map(|f| f.path.clone()).collect(),
            );
        }
        assert_eq!(
            review_planning::coverage(&context, &plan.shards, &completed).reviewed_count(),
            5
        );
        completed.remove(&0);
        assert_eq!(
            review_planning::coverage(&context, &plan.shards, &completed).reviewed_count(),
            1
        );

        let excessive: Vec<_> = context.files.iter().map(|f| chain(&[&f.path])).collect();
        let limited = super::plan(&config, &context, 100, &excessive, 2).unwrap();
        assert!(limited.notes.iter().any(|n| n.contains("pass budget")));
        assert!(limited.shards.len() <= 2);
        let mut invalid = chain(&["src/cancel.rs"]);
        invalid.related_paths = vec!["../outside.rs".into()];
        let fallback = super::plan(&config, &context, 100, &[invalid], 20).unwrap();
        assert!(fallback.chains.is_empty());
        let mut oversized = chain(&["src/cancel.rs"]);
        oversized.behavior = "x".repeat(config.review_context_max_bytes);
        assert!(super::plan(&config, &context, 100, &[oversized], 20)
            .unwrap()
            .chains
            .is_empty());

        let json = json!({"review_chains":[null,{"malformed":true},hints[0]], "investigation_log":["src/task.rs:1 checked"]});
        let parsed = parse_review_bot_output(&json.to_string()).unwrap();
        assert_eq!(parsed.review_chains.len(), 1);
        assert_eq!(parsed.investigation_log.len(), 1);
        let tagged = format!(
            "<review_chain>{}</review_chain><summary>no findings</summary>",
            serde_json::to_string(&hints[0]).unwrap()
        );
        assert_eq!(
            parse_review_bot_output(&tagged)
                .unwrap()
                .review_chains
                .len(),
            1
        );
        assert!(parse_review_bot_output(
            "<review_chain>not json</review_chain><summary>checked</summary>"
        )
        .unwrap()
        .review_chains
        .is_empty());
    }
}
