use super::*;

pub(super) fn passes_inline_threshold(finding: &ValidatedFinding) -> bool {
    finding.kind == FindingKind::Confirmed
        && finding.confidence == "high"
        && priority_rank(&finding.priority) <= 2
}

pub(super) fn validate(
    config: &Config,
    output: &ReviewBotOutput,
    context: &ReviewContext,
) -> ValidatedReview {
    let mut candidates = Vec::new();
    let mut rejected = Vec::new();
    for (index, (kind, finding)) in output
        .confirmed_findings
        .iter()
        .map(|f| (FindingKind::Confirmed, f))
        .chain(
            output
                .advisory_findings
                .iter()
                .map(|f| (FindingKind::Advisory, f)),
        )
        .enumerate()
    {
        match validate_finding_fields(finding, kind, index) {
            Ok(finding) => candidates.push(finding),
            Err(error) => rejected.push((finding.clone(), error.reason)),
        }
    }
    candidates.sort_by_key(|f| {
        (
            !passes_inline_threshold(f),
            priority_rank(&f.priority),
            confidence_rank(&f.confidence),
            f.kind != FindingKind::Confirmed,
            f.original_index,
        )
    });
    let mut review = ValidatedReview {
        global_review_complete: output.global_review_complete,
        candidate_checks: output.candidate_checks.clone(),
        inline_findings: Vec::new(),
        summary_findings: Vec::new(),
        unplaced_findings: Vec::new(),
        observations: output.observations.clone(),
        investigation_log: Vec::new(),
        verification: output.verification.clone(),
        residual_risk: output.residual_risk.clone(),
        summary: output.summary.clone(),
        coverage: None,
        debug_dir: None,
    };
    let mut seen = BTreeSet::new();
    for mut finding in candidates {
        let key = (
            finding.path.clone(),
            finding.side,
            finding.line,
            normalize_fingerprint_parts(&[&finding.title, &finding.issue]),
        );
        if !seen.insert(key) {
            continue;
        }
        let exact = context.commentable_lines.contains(&CommentLineKey {
            path: finding.path.clone(),
            side: finding.side,
            line: finding.line,
        });
        if !exact {
            finding
                .evidence
                .push_str("\n无法准确定位 diff 行，保留原始位置与证据，不挪到附近行。");
        }
        if exact
            && passes_inline_threshold(&finding)
            && review.inline_findings.len() < config.max_inline_comments
        {
            review.inline_findings.push(finding);
        } else {
            review.summary_findings.push(finding);
        }
    }
    for (finding, reason) in rejected {
        let title = match finding.severity.as_deref().and_then(normalize_priority) {
            Some(priority) => Some(format!(
                "[{priority}] {}",
                finding.title.as_deref().unwrap_or("待核查的问题")
            )),
            None => finding.title,
        };
        review.observations.push(ReviewObservation {
            confidence: finding.confidence,
            category: finding.category,
            path: finding.path,
            line: finding.line,
            title,
            evidence: Some(format!(
                "{}\n{}",
                finding.issue.unwrap_or_default(),
                finding.evidence.unwrap_or_default()
            )),
            project_context: finding.project_context,
            impact: finding.impact,
            next_step: Some(format!("{reason}；{}", finding.fix.unwrap_or_default())),
        });
    }
    review
}
