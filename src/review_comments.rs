//! Shared presentation for both review pipelines; publishing policy stays with each caller.
use super::*;

pub(crate) fn finding(finding: &ValidatedFinding) -> String {
    let qualifier = if finding.kind == FindingKind::Advisory || finding.confidence != "high" {
        "待核实建议 · "
    } else {
        ""
    };
    let mut body = format!(
        "**[{}] {qualifier}{}**\n\n{}\n",
        text(&finding.priority),
        text(&finding.title),
        finding.issue.trim(),
    );
    for (label, value) in [
        ("影响", &finding.impact),
        ("依据", &finding.evidence),
        ("建议", &finding.fix),
    ] {
        field(&mut body, label, value);
    }
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
            format!(" · {}", code(&format!("{path}{line}")))
        })
        .unwrap_or_default();
    let mut body = format!(
        "**待核实：{}**{location}\n",
        text(observation.title.as_deref().unwrap_or("尚未确立的问题"))
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
                field(&mut body, label, text);
            }
        }
    }
    body
}

pub(crate) fn fold(body: &mut String, title: &str, content: &str) {
    if !content.trim().is_empty() {
        body.push_str(&format!(
            "\n\n<details>\n<summary>{}</summary>\n\n{}\n\n</details>\n",
            html(&single_line(title)),
            content.trim()
        ));
    }
}

/// Keep block Markdown at the start of a line, separate from its field label.
fn field(body: &mut String, label: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let block = value.contains('\n')
        || value.starts_with(['-', '+', '*', '>', '#', '`', '~', '|', '<'])
        || value.chars().next().is_some_and(|c| c.is_ascii_digit());
    let separator = if block { "\n\n" } else { "：" };
    body.push_str(&format!("\n**{label}**{separator}{value}\n"));
}

fn html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn single_line(value: &str) -> String {
    value
        .trim()
        .replace("\r\n", "\n")
        .replace(['\r', '\n'], " ")
}

/// Plain metadata must not turn into headings, links, or table columns.
pub(crate) fn text(value: &str) -> String {
    let mut escaped = String::new();
    for c in html(&single_line(value)).chars() {
        if "\\`*_{}[]()#+-.!|".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

pub(crate) fn code(value: &str) -> String {
    let value = single_line(value);
    if !value.contains('`') {
        return format!("`{value}`");
    }
    let width = value.split(|c| c != '`').map(str::len).max().unwrap_or(0) + 1;
    let fence = "`".repeat(width);
    format!("{fence} {value} {fence}")
}

pub(crate) fn finding_index(finding: &ValidatedFinding) -> String {
    format!(
        "- **[{}] {}** · {}\n",
        text(&finding.priority),
        text(&finding.title),
        code(&format!("{}:{}", finding.path, finding.line)),
    )
}

pub(crate) fn list_item(content: &str) -> String {
    format!("- {}", content.trim().replace('\n', "\n  "))
}

pub(crate) fn finding_details(finding: &ValidatedFinding) -> String {
    format!(
        "{}\n\n{}",
        code(&format!(
            "{}:{} · {}",
            finding.path,
            finding.line,
            finding.side.as_github()
        )),
        self::finding(finding)
    )
}

pub(crate) fn fold_items(body: &mut String, title: &str, items: &[String]) {
    if !items.is_empty() {
        fold(
            body,
            &format!("{title}（{}）", items.len()),
            &items.join("\n\n---\n\n"),
        );
    }
}

pub(crate) fn metadata(trigger: &ReviewTrigger, session_id: &str) -> String {
    let origin = match trigger.comment() {
        Some(comment) => format!("触发评论：`{}`", comment.id),
        None => "触发：新 PR 自动审查".into(),
    };
    format!(
        "- 审查会话：{}\n- {origin}\n- Head SHA：{}",
        code(session_id),
        code(&trigger.pr.head_ref_oid)
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
