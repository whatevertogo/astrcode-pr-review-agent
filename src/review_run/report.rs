use super::*;

pub(super) fn inline(finding: &ValidatedFinding) -> String {
    review_comments::finding(finding)
}

pub(super) fn conclusion(review: &ValidatedReview) -> String {
    review_comments::conclusion(review)
}

/// Keep every limit that changes how the conclusion should be read, and
/// distinguish failed checks from checks that never ran.
fn limits(result: &ReviewRunResult) -> Option<String> {
    let review = &result.review;
    let mut reasons = Vec::new();
    if !review.global_review_complete {
        reasons.push("全局复核未完成，以下为已收集结果，不代表最终结论。".to_string());
    }
    match &review.coverage {
        None => reasons.push("未记录文件覆盖范围，尚不能确认全部文件已审完。".into()),
        Some(coverage)
            if coverage.entries.values().any(|entry| {
                !matches!(
                    entry.status,
                    CoverageStatus::Reviewed | CoverageStatus::SkippedGenerated
                )
            }) =>
        {
            reasons.push("仍有文件未完整审查，以下结论仅覆盖已审文件。".into());
        }
        Some(_) => {}
    }
    if review.verification.is_empty() {
        reasons.push("未记录实际执行的检查，不能据此确认已验证。".into());
    } else {
        let failed = review
            .verification
            .iter()
            .filter(|item| item.status.as_deref() == Some("failed"))
            .count();
        let unexecuted = review
            .verification
            .iter()
            .filter(|item| !matches!(item.status.as_deref(), Some("passed" | "failed")))
            .count();
        if failed > 0 {
            reasons.push(format!("有 {failed} 项检查失败，不能将验证视为通过。"));
        }
        if unexecuted > 0 {
            reasons.push(format!(
                "有 {unexecuted} 项验证未执行或状态未知，结论未覆盖这些检查。"
            ));
        }
    }
    if reasons.is_empty() && result.status != "complete" {
        reasons.push("本次审查仍有未完成的部分，结论仅覆盖已核实范围。".into());
    }
    (!reasons.is_empty()).then(|| reasons.join("\n"))
}

use review_comments::fold;

pub(super) fn render(result: &ReviewRunResult) -> String {
    let mut body = format!(
        "**{}** · 审查版本 `{}`\n\n",
        if result.status == "complete" && result.review.global_review_complete {
            "审查完成"
        } else {
            "部分完成"
        },
        &result.head_sha[..result.head_sha.len().min(12)]
    );
    let conclusion = conclusion(&result.review);
    body.push_str(&conclusion);
    if let Some(limit) = limits(result) {
        body.push_str(&limit);
        body.push_str("\n\n");
    }
    if !result.review.inline_findings.is_empty() {
        body.push('\n');
        for finding in &result.review.inline_findings {
            body.push_str(&review_comments::finding_index(finding));
        }
    }
    if let Some(url) = &result.publication.review_url {
        body.push_str(&format!("\n[查看行内审查]({url})\n"));
    }
    if result.publication.error.is_some() {
        body.push_str("\n行评发布未完全确认；已保留结果，重试前将核对 GitHub 回执。\n");
    }
    if let Some(summary) = result
        .review
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty() && *summary != conclusion.trim())
    {
        // Preserve the review stage's own paragraphs and links; the structured
        // conclusion and findings above remain independently visible.
        body.push('\n');
        body.push_str(summary);
        body.push_str("\n\n");
    }
    if !result.review.residual_risk.is_empty() {
        body.push_str("\n仍需确认：\n");
        for risk in &result.review.residual_risk {
            body.push_str(&format!("- {}\n", risk.trim()));
        }
    }
    let details = result
        .review
        .summary_findings
        .iter()
        .map(review_comments::finding_details)
        .collect::<Vec<_>>();
    review_comments::fold_items(&mut body, "其他发现与建议", &details);
    let observations = result
        .review
        .observations
        .iter()
        .map(review_comments::observation)
        .collect::<Vec<_>>();
    review_comments::fold_items(&mut body, "待核实的观察", &observations);
    let unplaced = result
        .review
        .unplaced_findings
        .iter()
        .map(|finding| {
            format!(
                "- [{}] {}：{}",
                finding.priority, finding.title, finding.reason
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    fold(&mut body, "无法行内定位的问题", &unplaced);
    fold(
        &mut body,
        "审查范围",
        &result
            .review
            .coverage
            .as_ref()
            .map(ReviewCoverage::summary_lines)
            .unwrap_or_else(|| "未记录覆盖范围".into()),
    );
    fold(
        &mut body,
        "复核记录",
        review_global::summary(&result.review).trim(),
    );
    let investigation = result
        .review
        .investigation_log
        .iter()
        .map(|item| review_comments::list_item(item))
        .collect::<Vec<_>>()
        .join("\n");
    fold(&mut body, "审查中核对的事实", &investigation);
    fold(
        &mut body,
        "已执行验证",
        &verification_summary(&result.review.verification),
    );
    let mut usage = String::from("| 阶段 | 状态 | Token 用量 | 执行 |\n|---|---|---|---|\n");
    let mut usage_notes = String::new();
    for stage in &result.stages {
        let u = &stage.usage;
        usage.push_str(&format!(
            "| {} | {} | 输入 {}<br>缓存输入 {}<br>输出 {} | 请求 {}<br>工具 {}<br>{} 秒 |\n",
            review_comments::text(&stage.label),
            review_comments::text(&stage.status),
            u.input_tokens,
            u.cached_input_tokens,
            u.output_tokens,
            u.requests,
            u.tool_calls,
            stage.elapsed_seconds
        ));
        if u.missing_usage_requests + u.estimated_requests + u.unknown_accounting_requests > 0 {
            usage_notes.push_str(&format!(
                "\n{}：估算/未知来源 {} 次，缺少用量 {} 次，缓存口径未知 {} 次。\n",
                review_comments::text(&stage.label),
                u.estimated_requests,
                u.missing_usage_requests,
                u.unknown_accounting_requests
            ));
        }
    }
    usage.push_str(&usage_notes);
    usage.push_str("\n缓存输入是分类统计，不能再次加到总输入；推理输出是输出的一部分。费用需要另按实际供应商价格计算。\n");
    if !result.stages.is_empty() {
        fold(&mut body, "模型用量与耗时", &usage);
    }
    body
}
