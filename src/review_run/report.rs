use super::*;

pub(super) fn inline(finding: &ValidatedFinding) -> String {
    review_comments::finding(finding)
}

pub(super) fn conclusion(review: &ValidatedReview) -> String {
    review_comments::conclusion(review)
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
    body.push_str(&review_global::summary(&result.review));
    if let Some(coverage) = &result.review.coverage {
        body.push_str(&format!(
            "文件审查：{} / {}。验证状态：{}。\n\n",
            coverage.reviewed_count(),
            coverage.total_count(),
            if result
                .review
                .verification
                .iter()
                .all(|item| item.status.as_deref() == Some("passed"))
                && !result.review.verification.is_empty()
            {
                "已执行的检查通过"
            } else {
                "尚未全部完成，请展开验证详情"
            }
        ));
    }
    body.push_str(&conclusion(&result.review));
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
        "覆盖范围",
        &result
            .review
            .coverage
            .as_ref()
            .map(ReviewCoverage::summary_lines)
            .unwrap_or_else(|| "未记录覆盖范围".into()),
    );
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
    fold(
        &mut body,
        "剩余风险",
        &result
            .review
            .residual_risk
            .iter()
            .map(|risk| format!("- {risk}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    body
}
