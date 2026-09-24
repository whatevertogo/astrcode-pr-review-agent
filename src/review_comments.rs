//! Shared presentation for both review pipelines; publishing policy stays with each caller.
use super::*;

pub(crate) fn finding(finding: &ValidatedFinding) -> String {
    let qualifier = if finding.kind == FindingKind::Advisory || finding.confidence != "high" {
        "待核实建议 · "
    } else {
        ""
    };
    let mut body = format!(
        "**[{}] {qualifier}{}**\n\n{}\n\n**影响**：{}\n\n**依据**：{}\n\n**建议**：{}\n",
        finding.priority,
        finding.title.trim(),
        finding.issue.trim(),
        finding.impact.trim(),
        finding.evidence.trim(),
        finding.fix.trim(),
    );
    let context = finding.project_context.trim();
    if ![
        &finding.issue,
        &finding.evidence,
        &finding.impact,
        &finding.fix,
    ]
    .iter()
    .any(|field| field.trim() == context)
    {
        fold(&mut body, "相关上下文与验证边界", context);
    }
    body
}

pub(crate) fn observation(observation: &ReviewObservation) -> String {
    let location = observation
        .path
        .as_ref()
        .map(|path| {
            let line = observation
                .line
                .map(|line| format!(":{line}"))
                .unwrap_or_default();
            format!(" · `{path}{line}`")
        })
        .unwrap_or_default();
    let mut body = format!(
        "**待核实：{}**{location}\n",
        observation.title.as_deref().unwrap_or("尚未确立的问题")
    );
    let mut seen = BTreeSet::new();
    for (label, value) in [
        ("已知依据", &observation.evidence),
        ("相关背景", &observation.project_context),
        ("可能影响", &observation.impact),
        ("如何确认", &observation.next_step),
    ] {
        if let Some(text) = value
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            if seen.insert(text) {
                body.push_str(&format!("\n**{label}**：{text}\n"));
            }
        }
    }
    body
}

pub(crate) fn fold(body: &mut String, title: &str, content: &str) {
    if !content.trim().is_empty() {
        body.push_str(&format!(
            "\n<details>\n<summary>{title}</summary>\n\n{}\n\n</details>\n",
            content.trim()
        ));
    }
}

pub(crate) fn metadata(trigger: &ReviewTrigger, session_id: &str) -> String {
    let origin = match trigger.comment() {
        Some(comment) => format!("触发评论：`{}`", comment.id),
        None => "触发：新 PR 自动审查".into(),
    };
    format!(
        "审查会话：`{session_id}`\n{origin}\nHead SHA：`{}`",
        trigger.pr.head_ref_oid
    )
}

pub(crate) fn is_partial(review: &ValidatedReview) -> bool {
    review.coverage.as_ref().is_none_or(|coverage| {
        coverage.entries.values().any(|entry| {
            !matches!(
                entry.status,
                CoverageStatus::Reviewed | CoverageStatus::SkippedGenerated
            )
        })
    }) || review.verification.is_empty()
        || review
            .verification
            .iter()
            .any(|check| check.status.as_deref() != Some("passed"))
}

pub(crate) fn conclusion(review: &ValidatedReview) -> String {
    let significant = review
        .inline_findings
        .iter()
        .chain(&review.summary_findings)
        .filter(|finding| {
            finding.kind == FindingKind::Confirmed
                && finding.confidence == "high"
                && priority_rank(&finding.priority) <= 2
        })
        .count();
    if significant > 0 {
        format!("已确认 {significant} 个高置信度 P0–P2 问题，建议先处理这些问题。\n")
    } else if !review.unplaced_findings.is_empty() {
        format!(
            "有 {} 条未能定位或完整验证的问题记录，请查看详情中的依据与限制。\n",
            review.unplaced_findings.len()
        )
    } else if review
        .coverage
        .as_ref()
        .is_none_or(|coverage| coverage.reviewed_count() == 0 && coverage.total_count() > 0)
    {
        "本次尚未完成文件级审查，不能据此得出没有问题的结论。\n".into()
    } else {
        "在已审范围内未发现需要修复的高置信度 P0–P2 问题。\n".into()
    }
}
