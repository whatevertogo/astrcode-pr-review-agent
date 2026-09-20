use super::*;

pub(super) fn inline(finding: &ValidatedFinding) -> String {
    format!("**[{}] {}**\n\n{}\n\n影响：{}\n\n建议：{}\n\n<details>\n<summary>代码证据</summary>\n\n{}\n\n{}\n\n</details>",
        finding.priority,finding.title,finding.issue,finding.impact,finding.fix,finding.evidence,finding.project_context)
}

pub(super) fn conclusion(review: &ValidatedReview) -> String {
    let significant = review
        .inline_findings
        .iter()
        .chain(&review.summary_findings)
        .filter(|finding| validation::passes_inline_threshold(finding))
        .count();
    if significant > 0 {
        format!(
            "已确认 {significant} 个高置信度 P0–P2 问题；行评 {} 条，其余保留在详情中。\n",
            review.inline_findings.len()
        )
    } else if review
        .coverage
        .as_ref()
        .is_some_and(|coverage| coverage.reviewed_count() == 0 && coverage.total_count() > 0)
    {
        "本次尚未完成文件级审查，不能据此得出没有问题的结论。\n".into()
    } else {
        "在已审范围内未发现需要修复的高置信度 P0–P2 问题。\n".into()
    }
}

pub(super) fn render(result: &ReviewRunResult) -> String {
    let mut body = format!(
        "**{}** · 审查版本 `{}`\n\n",
        if result.status == "complete" {
            "审查完成"
        } else {
            "部分完成"
        },
        &result.head_sha[..result.head_sha.len().min(12)]
    );
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
            body.push_str(&format!(
                "- **[{}] {}** — `{}`:{}\n",
                finding.priority, finding.title, finding.path, finding.line
            ));
        }
    }
    if let Some(url) = &result.publication.review_url {
        body.push_str(&format!("\n[查看行内审查]({url})\n"));
    }
    if result.publication.error.is_some() {
        body.push_str("\n行评发布未完全确认；已保留结果，重试前将核对 GitHub 回执。\n");
    }
    let mut details = String::new();
    for finding in &result.review.summary_findings {
        details.push_str(&format!("{}\n\n", inline(finding)));
    }
    for observation in &result.review.observations {
        details.push_str(&format!(
            "- **{}**{}\n\n{}\n\n{}\n\n{}\n\n{}\n\n",
            observation.title.as_deref().unwrap_or("观察"),
            observation
                .path
                .as_ref()
                .map(|p| format!(" `{p}`"))
                .unwrap_or_default(),
            observation.evidence.as_deref().unwrap_or(""),
            observation.project_context.as_deref().unwrap_or(""),
            observation.impact.as_deref().unwrap_or(""),
            observation.next_step.as_deref().unwrap_or("")
        ));
    }
    for finding in &result.review.unplaced_findings {
        details.push_str(&format!(
            "- [{}] {}：{}\n",
            finding.priority, finding.title, finding.reason
        ));
    }
    fold(&mut body, "建议与无法行内定位的问题", &details);
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
        &format_verification_items(&result.review.verification),
    );
    let mut usage=String::from("| 阶段 | 状态 | 输入 | 缓存输入 | 输出 | 模型请求 | 工具调用 | 秒 |\n|---|---|---:|---:|---:|---:|---:|---:|\n");
    let mut usage_notes = String::new();
    for stage in &result.stages {
        let u = &stage.usage;
        usage.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            stage.label,
            stage.status,
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
                stage.label,
                u.estimated_requests,
                u.missing_usage_requests,
                u.unknown_accounting_requests
            ));
        }
    }
    usage.push_str(&usage_notes);
    usage.push_str("\n缓存输入是分类统计，不能再次加到总输入；推理输出是输出的一部分。费用需要另按实际供应商价格计算。\n");
    fold(&mut body, "模型用量与耗时", &usage);
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

fn fold(body: &mut String, title: &str, content: &str) {
    if !content.trim().is_empty() {
        body.push_str(&format!(
            "\n<details>\n<summary>{title}</summary>\n\n{content}\n\n</details>\n"
        ));
    }
}
