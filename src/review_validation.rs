fn validate_review_output(
    config: &Config,
    output: &ReviewBotOutput,
    context: &ReviewContext,
) -> ValidatedReview {
    let mut valid = Vec::new();
    let mut summary_findings = Vec::new();
    let mut unplaced = Vec::new();
    let mut seen = BTreeSet::new();

    let candidates = output
        .confirmed_findings
        .iter()
        .map(|finding| (FindingKind::Confirmed, finding))
        .chain(
            output
                .advisory_findings
                .iter()
                .map(|finding| (FindingKind::Advisory, finding)),
        );

    for (index, (kind, finding)) in candidates.enumerate() {
        match validate_finding(finding, kind, index, context) {
            Ok(finding) => {
                let key = format!(
                    "{}:{}:{}:{}:{}",
                    finding.kind.as_str(),
                    finding.path.to_ascii_lowercase(),
                    finding.side.as_github(),
                    finding.line,
                    finding.title.to_ascii_lowercase()
                );
                if seen.insert(key) {
                    if confidence_allows_inline(config, &finding.confidence) {
                        valid.push(finding);
                    } else {
                        summary_findings.push(finding);
                    }
                } else {
                    unplaced.push(unplaced_from_validated(finding, "duplicate finding".into()));
                }
            }
            Err(unplaced_finding) => unplaced.push(*unplaced_finding),
        }
    }

    valid.sort_by_key(|finding| {
        (
            priority_rank(&finding.priority),
            finding.original_index,
            finding.path.clone(),
            finding.line,
        )
    });
    let max_inline = config.max_inline_comments;
    let overflow = if max_inline == 0 || valid.len() <= max_inline {
        Vec::new()
    } else {
        valid.split_off(max_inline)
    };
    for finding in overflow {
        unplaced.push(unplaced_from_validated(
            finding,
            format!("exceeds max_inline_comments={max_inline}"),
        ));
    }

    let mut residual_risk = output.residual_risk.clone();
    if context.truncated {
        residual_risk.push("审查上下文被插件字节上限截断。".into());
    }
    if !context.non_commentable_files.is_empty() {
        residual_risk.push(format!(
            "部分文件没有 GitHub patch，无法发布行内评论：{}",
            context.non_commentable_files.join(", ")
        ));
    }

    ValidatedReview {
        inline_findings: valid,
        summary_findings,
        unplaced_findings: unplaced,
        observations: output.observations.clone(),
        investigation_log: output.investigation_log.clone(),
        verification: output.verification.clone(),
        residual_risk,
        summary: output.summary.clone(),
        coverage: None,
        debug_dir: None,
    }
}

fn validate_finding(
    finding: &ReviewFinding,
    kind: FindingKind,
    index: usize,
    context: &ReviewContext,
) -> FindingValidationResult<ValidatedFinding> {
    let mut validated = validate_finding_fields(finding, kind, index)?;
    let key = CommentLineKey {
        path: validated.path.clone(),
        side: validated.side,
        line: validated.line,
    };
    if !context.commentable_lines.contains(&key) {
        if let Some(fallback) = nearest_commentable_line(context, &validated.path, validated.line) {
            validated.side = fallback.side;
            validated.line = fallback.line;
        } else {
            return Err(Box::new(unplaced_from_raw(
                finding,
                format!(
                    "{} {} is not a commentable PR diff line",
                    validated.side.as_github(),
                    validated.line
                ),
            )));
        }
    }
    Ok(validated)
}

fn validate_finding_fields(
    finding: &ReviewFinding,
    kind: FindingKind,
    index: usize,
) -> FindingValidationResult<ValidatedFinding> {
    let priority = required_field(&finding.severity, "severity", finding)?;
    let priority = normalize_priority(&priority).ok_or_else(|| {
        Box::new(unplaced_from_raw(
            finding,
            format!("invalid severity `{priority}`; expected P0, P1, P2, or P3"),
        ))
    })?;
    let confidence = required_field(&finding.confidence, "confidence", finding)?;
    let confidence = normalize_confidence(&confidence).ok_or_else(|| {
        Box::new(unplaced_from_raw(
            finding,
            format!("invalid confidence `{confidence}`; expected high, medium, or low"),
        ))
    })?;
    let category = required_field(&finding.category, "category", finding)?;
    let path = finding
        .path
        .as_ref()
        .filter(|path| !path.is_empty())
        .cloned()
        .ok_or_else(|| Box::new(unplaced_from_raw(finding, "missing path".into())))?;
    let side_raw = required_field(&finding.side, "side", finding)?;
    let side = CommentSide::parse(&side_raw).ok_or_else(|| {
        Box::new(unplaced_from_raw(
            finding,
            format!("invalid side `{side_raw}`; expected RIGHT or LEFT"),
        ))
    })?;
    let line = finding
        .line
        .filter(|line| *line > 0)
        .ok_or_else(|| Box::new(unplaced_from_raw(finding, "missing or invalid line".into())))?;
    let title = required_field(&finding.title, "title", finding)?;
    let issue = required_field(&finding.issue, "issue", finding)?;
    let evidence = required_field(&finding.evidence, "evidence", finding)?;
    let project_context = required_field(&finding.project_context, "project_context", finding)?;
    let impact = required_field(&finding.impact, "impact", finding)?;
    let fix = required_field(&finding.fix, "fix", finding)?;
    Ok(ValidatedFinding {
        priority,
        kind,
        confidence,
        category,
        path,
        side,
        line,
        title,
        issue,
        evidence,
        project_context,
        impact,
        fix,
        original_index: index,
    })
}

fn nearest_commentable_line(
    context: &ReviewContext,
    path: &str,
    target_line: u64,
) -> Option<CommentLineKey> {
    context
        .commentable_lines
        .iter()
        .filter(|line| line.path == path && line.side == CommentSide::Right)
        .min_by_key(|line| line.line.abs_diff(target_line))
        .filter(|line| line.line.abs_diff(target_line) <= 20)
        .cloned()
}

fn required_field(
    value: &Option<String>,
    name: &str,
    finding: &ReviewFinding,
) -> FindingValidationResult<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| Box::new(unplaced_from_raw(finding, format!("missing {name}"))))
}

fn normalize_priority(value: &str) -> Option<String> {
    match value.trim().to_ascii_uppercase().as_str() {
        "P0" | "[P0]" => Some("P0".into()),
        "P1" | "[P1]" => Some("P1".into()),
        "P2" | "[P2]" => Some("P2".into()),
        "P3" | "[P3]" => Some("P3".into()),
        _ => None,
    }
}

fn normalize_confidence(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "high" | "高" => Some("high".into()),
        "medium" | "med" | "中" => Some("medium".into()),
        "low" | "低" => Some("low".into()),
        _ => None,
    }
}

fn priority_rank(priority: &str) -> u8 {
    match priority {
        "P0" => 0,
        "P1" => 1,
        "P2" => 2,
        "P3" => 3,
        _ => 4,
    }
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        _ => 3,
    }
}

fn confidence_allows_inline(config: &Config, confidence: &str) -> bool {
    let min =
        normalize_confidence(&config.inline_confidence_min).unwrap_or_else(|| "medium".into());
    confidence_rank(confidence) <= confidence_rank(&min)
}

fn unplaced_from_raw(finding: &ReviewFinding, reason: String) -> UnplacedFinding {
    UnplacedFinding {
        priority: finding
            .severity
            .as_deref()
            .and_then(normalize_priority)
            .unwrap_or_else(|| "P3".into()),
        kind: "Unknown".into(),
        confidence: finding
            .confidence
            .as_deref()
            .and_then(normalize_confidence)
            .unwrap_or_else(|| "low".into()),
        title: finding
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or("Untitled finding")
            .to_owned(),
        path: finding.path.clone(),
        side: finding.side.clone(),
        line: finding.line,
        reason,
    }
}

fn unplaced_from_validated(finding: ValidatedFinding, reason: String) -> UnplacedFinding {
    let kind = finding.kind.as_str().to_owned();
    let side = finding.side.as_github().to_owned();
    UnplacedFinding {
        priority: finding.priority,
        kind,
        confidence: finding.confidence,
        title: finding.title,
        path: Some(finding.path),
        side: Some(side),
        line: Some(finding.line),
        reason,
    }
}
fn deserialize_contract_notes<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Note {
        Text(String),
        SourceFact { path: String, fact: String },
    }
    Vec::<Note>::deserialize(deserializer)?
        .into_iter()
        .map(|note| match note {
            Note::Text(text) => Ok(text),
            Note::SourceFact { path, fact }
                if !path.trim().is_empty() && !fact.trim().is_empty() =>
            {
                Ok(format!("{path}: {fact}"))
            }
            Note::SourceFact { .. } => Err(serde::de::Error::custom(
                "source contract requires path and fact",
            )),
        })
        .collect()
}
