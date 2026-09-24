//! Mandatory final adjudication. Candidate accounting prevents silent omissions.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CandidateCheck {
    pub id: String,
    pub outcome: String,
    // Index into the raw global response, before publication filtering/reordering.
    #[serde(default)]
    pub result_index: Option<usize>,
    pub reason: String,
}

pub(crate) fn candidates(output: &ReviewBotOutput) -> Vec<Value> {
    output.confirmed_findings.iter().map(|f| ("confirmed", json!(f)))
        .chain(output.advisory_findings.iter().map(|f| ("advisory", json!(f))))
        .chain(output.observations.iter().map(|o| ("observation", json!(o))))
        .enumerate()
        .map(|(i, (kind, content))| json!({"id": format!("C{:03}", i + 1), "kind": kind, "content": content}))
        .collect()
}

pub(crate) const CONTRACT: &str = r#"
全局复核是发布前必经阶段，返回完整的最终集合，不是增量。
逐条处理下方带 C 编号的候选（包含观察），主动读取必要调用方与失败路径，寻找成立证据和反例。缺少可从仓库查到的信息时先补查，不直接以“缺少证据”结束。跨文件检查还要独立寻找前片遗漏的问题，无候选也必须检查。
对每个 C 编号返回且仅返回一条 candidate_checks：
{"id":"C001","outcome":"confirmed|advisory|observation|rejected","result_index":0,"reason":"具体源码位置、关键事实及保留或排除理由"}
result_index 为对应最终 confirmed_findings/advisory_findings/observations 数组中的零基索引；rejected 时为 null。重复候选可以指向同一最终条目。只有确切反例、重复、既有问题或明确不适用的证据才能 rejected；未解决的可执行疑点保留 observation，说明缺口与下一步，不能静默丢失。
新增问题同样必须有触发条件、因果、证据与影响。最终 observations、residual_risk 去重并去除已反证项；不要复制早期已过时的担忧。
JSON 输出时包含 global_review_complete:true 与 candidate_checks；其余字段保持原 JSON 协议。
若使用标签协议，额外输出 <global_review_complete>true</global_review_complete>，并逐条输出 <candidate_check id="C001" outcome="rejected">具体证据与理由</candidate_check>；保留条目用 result_index="0" 指向对应 kind 的 finding 或 observation 顺序。不遗漏编号，不以“无问题”代替复核。
global_review_complete 只表明本阶段完成，不扩大文件级覆盖；前片未完整审查的文件仍为未审。
"#;

pub(crate) fn validate(output: &ReviewBotOutput, candidates: &[Value]) -> Result<()> {
    anyhow::ensure!(
        output.global_review_complete,
        "global review completion declaration missing"
    );
    let expected: BTreeSet<_> = candidates.iter().filter_map(|c| c["id"].as_str()).collect();
    let mut seen = BTreeSet::new();
    for check in &output.candidate_checks {
        anyhow::ensure!(
            expected.contains(check.id.as_str()) && seen.insert(check.id.as_str()),
            "unknown or duplicate candidate id: {}",
            check.id
        );
        anyhow::ensure!(
            !check.reason.trim().is_empty(),
            "candidate {} missing evidence/reason",
            check.id
        );
        let count = match check.outcome.as_str() {
            "confirmed" => output.confirmed_findings.len(),
            "advisory" => output.advisory_findings.len(),
            "observation" => output.observations.len(),
            "rejected" => {
                anyhow::ensure!(
                    check.result_index.is_none(),
                    "rejected candidate {} must not reference a result",
                    check.id
                );
                continue;
            }
            _ => anyhow::bail!("invalid outcome for candidate {}", check.id),
        };
        anyhow::ensure!(
            check.result_index.is_some_and(|index| index < count),
            "candidate {} references a missing final result",
            check.id
        );
    }
    anyhow::ensure!(
        seen == expected,
        "global review omitted candidates: {:?}",
        expected.difference(&seen).collect::<Vec<_>>()
    );
    Ok(())
}

/// Orientation is optional at small budgets; one file pass and a final pass are not.
pub(crate) fn file_budget(maximum: usize) -> Result<(bool, usize)> {
    anyhow::ensure!(
        maximum >= 2,
        "review budget must allow at least one file pass and mandatory global review (minimum 2)"
    );
    let orientation = maximum >= 3;
    Ok((orientation, maximum - 1 - usize::from(orientation)))
}

pub(crate) fn summary(review: &ValidatedReview) -> String {
    if !review.global_review_complete {
        return "\n未记录成功的全局复核回执。\n\n".into();
    }
    let rejected = review
        .candidate_checks
        .iter()
        .filter(|c| c.outcome == "rejected")
        .count();
    let unresolved = review
        .candidate_checks
        .iter()
        .filter(|c| c.outcome == "observation")
        .count();
    format!("\n全局复核已完成 · 已处置 {} 条候选：保留 {}，排除 {}，待核实 {}。重复候选可能对应同一最终问题。\n\n",
        review.candidate_checks.len(), review.candidate_checks.len() - rejected - unresolved, rejected, unresolved)
}
