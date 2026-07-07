#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewRecord {
    repo: String,
    pr_number: u64,
    pr_title: String,
    pr_url: String,
    head_sha: String,
    #[serde(default = "default_trigger_kind")]
    trigger_kind: String,
    #[serde(default)]
    trigger_comment_id: Option<u64>,
    trigger_comment_url: Option<String>,
    trigger_author: String,
    session_id: String,
    review_comment_url: Option<String>,
    #[serde(default)]
    related_context: Option<String>,
    #[serde(default)]
    inline_comments_posted: usize,
    #[serde(default)]
    unplaced_findings: usize,
    #[serde(default)]
    highest_risk: Option<String>,
    summary: String,
    created_at: u64,
}

type FindingValidationResult<T> = std::result::Result<T, Box<UnplacedFinding>>;

fn default_trigger_kind() -> String {
    "mention_comment".into()
}

async fn review_trigger(
    config: &Config,
    state: &mut State,
    run_info: &RunInfo,
    trigger: &ReviewTrigger,
) -> Result<ReviewRecord> {
    let mut trigger = trigger.clone();
    if let Ok(details) = pr_details(&trigger.repo, trigger.pr.number) {
        trigger.pr = details;
    }
    let worktree = checkout_pr(config, &trigger.repo, trigger.pr.number)?;
    let mut memory = relevant_memory(config, &trigger.repo, trigger.pr.number)?;
    let related_context = related_github_context(&trigger)
        .unwrap_or_else(|error| format!("related GitHub context unavailable: {error:#}"));
    if !related_context.trim().is_empty() {
        memory.push_str("\n\n## Related PRs and Issues\n");
        memory.push_str(related_context.trim());
        memory.push('\n');
    }
    let memory_paths = PromptMemoryPaths {
        repo_index: repo_memory_index_path(config, &trigger.repo)?,
        pr_memory: pr_memory_path(config, &trigger.repo, trigger.pr.number)?,
        runs_log: config.memory_dir_path()?.join("runs.jsonl"),
    };
    let is_review_task = trigger_requests_review(&trigger);
    let review_context = if is_review_task {
        Some(collect_review_context(config, &trigger, &worktree)?)
    } else {
        None
    };
    let (mut session_id, mut reused_session) =
        ensure_pr_session(state, run_info, &trigger, &worktree).await?;
    let published = if let Some(context) = review_context.as_ref() {
        let mut result = run_coverage_first_review(
            config,
            run_info,
            &session_id,
            &trigger,
            &worktree,
            &memory,
            &memory_paths,
            reused_session,
            context,
        )
        .await;
        if result.is_err() && reused_session {
            let error = result
                .as_ref()
                .err()
                .map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "unknown error".into());
            eprintln!(
                "coverage review in existing PR session failed; recreating session for {}#{}: \
                 {error}",
                trigger.repo, trigger.pr.number
            );
            session_id = create_or_replace_pr_session(state, run_info, &trigger, &worktree).await?;
            reused_session = false;
            result = run_coverage_first_review(
                config,
                run_info,
                &session_id,
                &trigger,
                &worktree,
                &memory,
                &memory_paths,
                reused_session,
                context,
            )
            .await;
        }
        let mut validated = result?;
        suppress_repeated_findings(state, &trigger, &mut validated);
        let mut published = publish_structured_review(config, &trigger, &session_id, &validated)?;
        post_final_structured_report(
            config,
            run_info,
            &trigger,
            &session_id,
            &validated,
            &mut published,
        )
        .await?;
        update_pr_review_memory(state, &trigger, &session_id, Some(&validated), &published);
        published
    } else {
        let mut prompt = review_prompt(
            &trigger,
            &worktree,
            &memory,
            &memory_paths,
            reused_session,
            None,
        );
        let mut review_result = submit_prompt_and_wait(
            run_info,
            &session_id,
            &prompt,
            Duration::from_secs(config.review_timeout_seconds),
        )
        .await;
        if let Err(error) = review_result.as_ref() {
            if !reused_session {
                return Err(anyhow::anyhow!("{error:#}"));
            }
            eprintln!(
                "submit to existing PR session failed; recreating session for {}#{}: {error:#}",
                trigger.repo, trigger.pr.number
            );
            session_id = create_or_replace_pr_session(state, run_info, &trigger, &worktree).await?;
            reused_session = false;
            prompt = review_prompt(
                &trigger,
                &worktree,
                &memory,
                &memory_paths,
                reused_session,
                None,
            );
            review_result = submit_prompt_and_wait(
                run_info,
                &session_id,
                &prompt,
                Duration::from_secs(config.review_timeout_seconds),
            )
            .await;
        }
        let review = review_result?;
        let review_comment_url = post_review_comment(config, &trigger, &session_id, &review)?;
        PublishedReview {
            url: review_comment_url.clone(),
            inline_review_url: None,
            inline_review_id: None,
            summary_body: review.clone(),
            inline_comments_posted: 0,
            unplaced_findings_count: 0,
            highest_risk: None,
            verification: Vec::new(),
            posted_findings: Vec::new(),
        }
    };
    if !is_review_task {
        update_pr_review_memory(state, &trigger, &session_id, None, &published);
    }
    state.last_deterministic_checks = published.verification.clone();
    let summary = summarize_review(&trigger, &session_id, &published);
    Ok(ReviewRecord {
        repo: trigger.repo.clone(),
        pr_number: trigger.pr.number,
        pr_title: trigger.pr.title.clone(),
        pr_url: trigger.pr.url.clone(),
        head_sha: trigger.pr.head_ref_oid.clone(),
        trigger_kind: trigger.trigger_kind_name().into(),
        trigger_comment_id: trigger.comment().map(|comment| comment.id),
        trigger_comment_url: trigger
            .comment()
            .and_then(|comment| comment.html_url.clone()),
        trigger_author: trigger
            .comment()
            .and_then(|comment| comment.user.as_ref())
            .map(|user| user.login.clone())
            .unwrap_or_else(|| "auto:new-pr".into()),
        session_id,
        review_comment_url: published.url,
        related_context: Some(related_context)
            .filter(|context| !context.trim().is_empty())
            .map(|context| context.trim().to_owned()),
        inline_comments_posted: published.inline_comments_posted,
        unplaced_findings: published.unplaced_findings_count,
        highest_risk: published.highest_risk,
        summary,
        created_at: now_epoch(),
    })
}

async fn ensure_pr_session(
    state: &mut State,
    run_info: &RunInfo,
    trigger: &ReviewTrigger,
    worktree: &Path,
) -> Result<(String, bool)> {
    let key = pr_key(&trigger.repo, trigger.pr.number);
    if let Some(existing) = state.pr_sessions.get_mut(&key) {
        existing.last_head_sha = trigger.pr.head_ref_oid.clone();
        existing.worktree = path_str(worktree)?.to_string();
        existing.updated_at = now_epoch();
        let session_id = existing.session_id.clone();
        save_state(state)?;
        return Ok((session_id, true));
    }
    let session_id = create_or_replace_pr_session(state, run_info, trigger, worktree).await?;
    Ok((session_id, false))
}

fn update_pr_review_memory(
    state: &mut State,
    trigger: &ReviewTrigger,
    session_id: &str,
    validated: Option<&ValidatedReview>,
    published: &PublishedReview,
) {
    let key = pr_key(&trigger.repo, trigger.pr.number);
    let memory = state.pr_review_memory.entry(key).or_default();
    let now = now_epoch();
    memory.last_head_sha = Some(trigger.pr.head_ref_oid.clone());
    memory.last_reviewed_at = Some(now);
    memory.reviewed_ranges.push(ReviewedRange {
        base_sha: trigger.pr.base_ref_name.clone(),
        head_sha: trigger.pr.head_ref_oid.clone(),
        reviewed_at: now,
        session_id: Some(session_id.to_owned()),
    });
    for finding in &published.posted_findings {
        memory
            .posted_findings
            .insert(finding.fingerprint.clone(), finding.clone());
    }
    if let Some(validated) = validated {
        for finding in &validated.summary_findings {
            let memory_finding =
                finding_memory_from_validated_with_status(finding, &trigger.pr.head_ref_oid, "summary_only");
            memory
                .summary_findings
                .insert(memory_finding.fingerprint.clone(), memory_finding);
        }
        for finding in &validated.unplaced_findings {
            let fingerprint = normalize_fingerprint_parts(&[
                &finding.priority,
                &finding.kind,
                &finding.confidence,
                finding.path.as_deref().unwrap_or(""),
                finding.side.as_deref().unwrap_or(""),
                &finding.line.map(|line| line.to_string()).unwrap_or_default(),
                &finding.title,
                &finding.reason,
            ]);
            memory.summary_findings.insert(
                fingerprint.clone(),
                FindingMemory {
                    fingerprint,
                    priority: finding.priority.clone(),
                    kind: finding.kind.clone(),
                    confidence: finding.confidence.clone(),
                    title: finding.title.clone(),
                    path: finding.path.clone(),
                    line: finding.line,
                    head_sha: trigger.pr.head_ref_oid.clone(),
                    status: "unplaced".into(),
                    posted_at: now,
                },
            );
        }
        memory.observations.extend(
            validated
                .observations
                .iter()
                .take(20)
                .map(|observation| ObservationMemory {
                    confidence: observation
                        .confidence
                        .as_deref()
                        .and_then(normalize_confidence)
                        .unwrap_or_else(|| "low".into()),
                    category: observation
                        .category
                        .as_deref()
                        .unwrap_or("Observation")
                        .to_owned(),
                    title: observation
                        .title
                        .as_deref()
                        .unwrap_or("Untitled observation")
                        .to_owned(),
                    path: observation.path.clone(),
                    line: observation.line,
                    summary: one_line(
                        observation
                            .next_step
                            .as_deref()
                            .or(observation.impact.as_deref())
                            .or(observation.evidence.as_deref())
                            .unwrap_or(""),
                    ),
                    head_sha: trigger.pr.head_ref_oid.clone(),
                    recorded_at: now,
                }),
        );
    }
    if memory.reviewed_ranges.len() > 50 {
        let keep_from = memory.reviewed_ranges.len().saturating_sub(50);
        memory.reviewed_ranges.drain(0..keep_from);
    }
    if memory.summary_findings.len() > 100 {
        let drop_count = memory.summary_findings.len().saturating_sub(100);
        let keys = memory
            .summary_findings
            .keys()
            .take(drop_count)
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            memory.summary_findings.remove(&key);
        }
    }
    if memory.observations.len() > 100 {
        let keep_from = memory.observations.len().saturating_sub(100);
        memory.observations.drain(0..keep_from);
    }
}

async fn create_or_replace_pr_session(
    state: &mut State,
    run_info: &RunInfo,
    trigger: &ReviewTrigger,
    worktree: &Path,
) -> Result<String> {
    let session_id = create_session(run_info, worktree).await?;
    let now = now_epoch();
    state.pr_sessions.insert(
        pr_key(&trigger.repo, trigger.pr.number),
        PrSession {
            repo: trigger.repo.clone(),
            pr_number: trigger.pr.number,
            session_id: session_id.clone(),
            worktree: path_str(worktree)?.to_string(),
            last_head_sha: trigger.pr.head_ref_oid.clone(),
            created_at: now,
            updated_at: now,
        },
    );
    save_state(state)?;
    Ok(session_id)
}

#[allow(clippy::too_many_arguments)]
async fn run_coverage_first_review(
    config: &Config,
    run_info: &RunInfo,
    session_id: &str,
    trigger: &ReviewTrigger,
    worktree: &Path,
    memory: &str,
    memory_paths: &PromptMemoryPaths,
    reused_session: bool,
    context: &ReviewContext,
) -> Result<ValidatedReview> {
    let debug_dir = create_debug_run_dir(config, trigger).ok();
    if config.review_pipeline != "coverage_first" {
        let prompt = review_prompt(
            trigger,
            worktree,
            memory,
            memory_paths,
            reused_session,
            Some(context),
        );
        write_debug_artifact(debug_dir.as_deref(), "legacy-review-prompt.md", &prompt);
        let review = submit_prompt_and_wait(
            run_info,
            session_id,
            &prompt,
            Duration::from_secs(config.review_timeout_seconds),
        )
        .await?;
        write_debug_artifact(debug_dir.as_deref(), "legacy-review-response.txt", &review);
        let output = parse_or_repair_review_output(config, run_info, session_id, &review).await?;
        let mut validated = validate_review_output(config, &output, context);
        validated.debug_dir = debug_dir;
        return Ok(validated);
    }

    let shards = plan_review_shards(config, context);
    let mut coverage = initial_coverage(context);
    let mut outputs = Vec::new();
    let deterministic_checks = if config.deterministic_checks_enabled {
        deterministic_review_verification(config, trigger, worktree)
    } else {
        Vec::new()
    };
    write_debug_artifact(debug_dir.as_deref(), "review-context.txt", &context.text);
    write_debug_artifact(
        debug_dir.as_deref(),
        "deterministic-checks.json",
        &serde_json::to_string_pretty(&deterministic_checks).unwrap_or_default(),
    );
    let mut staging = match StagedReviewRun::load(trigger) {
        Ok(run) => {
            write_debug_artifact(
                debug_dir.as_deref(),
                "staged-findings-path.txt",
                &run.path.display().to_string(),
            );
            Some(run)
        },
        Err(error) => {
            eprintln!(
                "failed to initialize staged review outputs for {}#{}: {error:#}",
                trigger.repo, trigger.pr.number
            );
            None
        },
    };
    let max_passes = config.max_review_passes_per_pr.max(1);
    let reserved_passes = 2usize.min(max_passes.saturating_sub(1));
    let max_file_passes = max_passes.saturating_sub(reserved_passes).max(1);

    if max_passes > 1 {
        let prompt = orientation_review_prompt(
            config,
            trigger,
            worktree,
            memory,
            context,
            &deterministic_checks,
        );
        match submit_review_pass(
            config,
            run_info,
            session_id,
            &prompt,
            debug_dir.as_deref(),
            staging.as_mut(),
            "orientation",
        )
        .await
        {
            Ok(output) => outputs.push(output),
            Err(error) => outputs.push(ReviewBotOutput {
                residual_risk: vec![format!("orientation pass failed: {error:#}")],
                summary: Some("Orientation pass failed; file/global passes continued.".into()),
                ..ReviewBotOutput::default()
            }),
        }
    }

    for shard in shards.iter().take(max_file_passes) {
        let prompt_context = PassPromptContext {
            config,
            trigger,
            worktree,
            memory,
            context,
            deterministic_checks: &deterministic_checks,
        };
        let prompt = file_review_prompt(&prompt_context, memory_paths, shard);
        let label = format!("file-pass-{:03}", shard.index + 1);
        match submit_review_pass(
            config,
            run_info,
            session_id,
            &prompt,
            debug_dir.as_deref(),
            staging.as_mut(),
            &label,
        )
        .await
        {
            Ok(output) => {
                let reviewed = output
                    .files_reviewed
                    .iter()
                    .map(|path| path.trim())
                    .collect::<BTreeSet<_>>();
                for file in &shard.files {
                    if matches!(file.kind, ReviewFileKind::Code | ReviewFileKind::Docs) {
                        if reviewed.contains(file.path.as_str()) {
                            coverage.mark(
                                file.path.clone(),
                                CoverageStatus::Reviewed,
                                format!("file review pass {}", shard.index + 1),
                            );
                        } else {
                            coverage.mark(
                                file.path.clone(),
                                CoverageStatus::Failed,
                                format!(
                                    "file review pass {} omitted this file from files_reviewed",
                                    shard.index + 1
                                ),
                            );
                        }
                    }
                }
                outputs.push(output);
            },
            Err(error) => {
                for file in &shard.files {
                    if matches!(file.kind, ReviewFileKind::Code | ReviewFileKind::Docs) {
                        coverage.mark(
                            file.path.clone(),
                            CoverageStatus::Failed,
                            format!("file review pass {} failed: {error:#}", shard.index + 1),
                        );
                    }
                }
            },
        }
    }
    for shard in shards.iter().skip(max_file_passes) {
        for file in &shard.files {
            if matches!(file.kind, ReviewFileKind::Code | ReviewFileKind::Docs) {
                coverage.mark(
                    file.path.clone(),
                    CoverageStatus::Failed,
                    format!(
                        "not reviewed because max_review_passes_per_pr={} was reached",
                        config.max_review_passes_per_pr
                    ),
                );
            }
        }
    }

    if outputs.len() + 1 < max_passes {
        let prompt_context = PassPromptContext {
            config,
            trigger,
            worktree,
            memory,
            context,
            deterministic_checks: &deterministic_checks,
        };
        let prompt = global_review_prompt(&prompt_context, &coverage, &outputs);
        match submit_review_pass(
            config,
            run_info,
            session_id,
            &prompt,
            debug_dir.as_deref(),
            staging.as_mut(),
            "global-pass",
        )
        .await
        {
            Ok(output) => outputs.push(output),
            Err(error) => {
                outputs.push(ReviewBotOutput {
                    residual_risk: vec![format!("global review pass failed: {error:#}")],
                    summary: Some("Global risk pass failed; file pass results were used.".into()),
                    ..ReviewBotOutput::default()
                });
            },
        }
    }

    let mut merged = merge_review_outputs(&outputs);
    merged.verification.extend(deterministic_checks);
    write_debug_artifact(
        debug_dir.as_deref(),
        "merged-output.json",
        &serde_json::to_string_pretty(&merged).unwrap_or_default(),
    );
    let mut validated = validate_review_output(config, &merged, context);
    let mut residual = coverage_residual_risk(&coverage);
    residual.extend(validated.residual_risk);
    validated.residual_risk = residual;
    validated.coverage = Some(coverage);
    validated.debug_dir = debug_dir;
    write_debug_artifact(
        validated.debug_dir.as_deref(),
        "validated-findings.json",
        &serde_json::to_string_pretty(&validated_review_debug_json(&validated))
            .unwrap_or_default(),
    );
    Ok(validated)
}

async fn submit_review_pass(
    config: &Config,
    run_info: &RunInfo,
    session_id: &str,
    prompt: &str,
    debug_dir: Option<&Path>,
    mut staging: Option<&mut StagedReviewRun>,
    label: &str,
) -> Result<ReviewBotOutput> {
    if let Some(staging) = staging.as_deref_mut() {
        if let Some(output) = staging.output(label) {
            write_debug_artifact(
                debug_dir,
                &format!("{label}-response.json"),
                &serde_json::to_string_pretty(&output).unwrap_or_default(),
            );
            return Ok(output);
        }
    }
    write_debug_artifact(debug_dir, &format!("{label}-prompt.md"), prompt);
    let review = submit_prompt_and_wait(
        run_info,
        session_id,
        prompt,
        Duration::from_secs(config.review_timeout_seconds),
    )
    .await?;
    write_debug_artifact(debug_dir, &format!("{label}-response.txt"), &review);
    let output = parse_or_repair_review_output(config, run_info, session_id, &review).await?;
    write_debug_artifact(
        debug_dir,
        &format!("{label}-response.json"),
        &serde_json::to_string_pretty(&output).unwrap_or_default(),
    );
    if let Some(staging) = staging.as_deref_mut() {
        staging.append(label, &output)?;
    }
    Ok(output)
}

fn create_debug_run_dir(config: &Config, trigger: &ReviewTrigger) -> Result<PathBuf> {
    let dir = agent_dir()?
        .join("debug-runs")
        .join(repo_key(&trigger.repo))
        .join(format!("pr-{}", trigger.pr.number))
        .join(format!("{}-{}", now_epoch(), trigger.pr.head_ref_oid));
    fs::create_dir_all(&dir)?;
    let metadata = json!({
        "repo": trigger.repo,
        "pr_number": trigger.pr.number,
        "title": trigger.pr.title,
        "head_sha": trigger.pr.head_ref_oid,
        "trigger_kind": trigger.trigger_kind_name(),
        "comment_id": trigger.comment().map(|comment| comment.id),
        "config": {
            "max_inline_comments": config.max_inline_comments,
            "max_advisory_inline_comments": config.max_advisory_inline_comments,
            "max_p3_inline_comments": config.max_p3_inline_comments,
            "inline_confidence_min": config.inline_confidence_min,
        }
    });
    fs::write(dir.join("run-metadata.json"), serde_json::to_vec_pretty(&metadata)?)?;
    Ok(dir)
}

fn write_debug_artifact(debug_dir: Option<&Path>, name: &str, content: &str) {
    let Some(debug_dir) = debug_dir else {
        return;
    };
    if let Err(error) = fs::write(debug_dir.join(name), content) {
        eprintln!(
            "failed to write review debug artifact {}: {error:#}",
            debug_dir.join(name).display()
        );
    }
}

fn validated_review_debug_json(validated: &ValidatedReview) -> Value {
    json!({
        "inline_findings": validated.inline_findings.iter().map(validated_finding_json).collect::<Vec<_>>(),
        "summary_findings": validated.summary_findings.iter().map(validated_finding_json).collect::<Vec<_>>(),
        "unplaced_findings": validated.unplaced_findings.iter().map(|finding| json!({
            "severity": finding.priority,
            "kind": finding.kind,
            "confidence": finding.confidence,
            "title": finding.title,
            "path": finding.path,
            "side": finding.side,
            "line": finding.line,
            "reason": finding.reason,
        })).collect::<Vec<_>>(),
        "observations": validated.observations,
        "investigation_log": validated.investigation_log,
        "residual_risk": validated.residual_risk,
    })
}

fn validated_finding_json(finding: &ValidatedFinding) -> Value {
    json!({
        "severity": finding.priority,
        "kind": finding.kind.as_str(),
        "confidence": finding.confidence,
        "category": finding.category,
        "path": finding.path,
        "side": finding.side.as_github(),
        "line": finding.line,
        "title": finding.title,
        "issue": finding.issue,
        "evidence": finding.evidence,
        "project_context": finding.project_context,
        "impact": finding.impact,
        "fix": finding.fix,
    })
}

fn initial_coverage(context: &ReviewContext) -> ReviewCoverage {
    let mut coverage = ReviewCoverage::default();
    for file in &context.files {
        coverage.mark(
            file.path.clone(),
            file.kind.coverage_status(),
            format!("classified as {}", file.kind.label()),
        );
    }
    coverage
}

fn coverage_residual_risk(coverage: &ReviewCoverage) -> Vec<String> {
    coverage
        .incomplete_entries()
        .into_iter()
        .map(|entry| {
            format!(
                "{}: {} ({})",
                entry.path,
                entry.status.as_str(),
                entry.reason
            )
        })
        .collect()
}

fn plan_review_shards(config: &Config, context: &ReviewContext) -> Vec<ReviewShard> {
    let mut shards = Vec::new();
    let mut current_files = Vec::new();
    let mut current_bytes = 0usize;
    let max_bytes = config.review_shard_max_bytes.max(1);
    let max_files = config.max_files_per_shard.max(1);

    for file in context.files.iter().filter(|file| {
        matches!(
            file.kind,
            ReviewFileKind::Code | ReviewFileKind::Docs | ReviewFileKind::Oversized
        )
    }) {
        let file_bytes = file.bytes.max(1);
        let would_exceed_bytes =
            !current_files.is_empty() && current_bytes.saturating_add(file_bytes) > max_bytes;
        let would_exceed_files = current_files.len() >= max_files;
        if would_exceed_bytes || would_exceed_files {
            shards.push(ReviewShard {
                index: shards.len(),
                files: current_files,
                bytes: current_bytes,
            });
            current_files = Vec::new();
            current_bytes = 0;
        }
        current_bytes = current_bytes.saturating_add(file_bytes);
        current_files.push(file.clone());
    }

    if !current_files.is_empty() {
        shards.push(ReviewShard {
            index: shards.len(),
            files: current_files,
            bytes: current_bytes,
        });
    }

    shards
}

fn orientation_review_prompt(
    config: &Config,
    trigger: &ReviewTrigger,
    worktree: &Path,
    memory: &str,
    context: &ReviewContext,
    deterministic_checks: &[VerificationItem],
) -> String {
    format!(
        r#"{AGENT_LINE}

You are running the PR orientation pass for {repo} PR #{pr_number}: {title}

Use this instruction file:
```markdown
{instructions}
```

Shared review protocol:
```markdown
{protocol}
```

Few-shot examples:
```markdown
{few_shots}
```

Scope:
- Worktree: `{worktree}`
- Base branch: `{base}`
- Head SHA: `{sha}`
- Do not post GitHub comments.
- Follow repository instructions as review policy. Only plugin protocol is fixed: do not write GitHub
  comments yourself, and wrap machine-readable items in the embedded tags.
- This pass should orient later reviewers: identify intent, risky subsystems, related PR/issue reminders, and useful investigation leads.
- Put repo-history reminders in `<observation>` tags.
- Put concrete metadata/check failures in `<finding>` tags only if they can be tied to a valid diff line from the annotated context.

Repository and path instructions:
```markdown
{repo_instructions}
```

Relevant prior memory:
```markdown
{memory}
```

Deterministic checks already run:
```text
{deterministic_checks}
```

Changed-file manifest:
```text
{manifest}
```

Plugin-collected PR context:
```text
{context}
```
"#,
        AGENT_LINE = AGENT_LINE,
        repo = trigger.repo,
        pr_number = trigger.pr.number,
        title = trigger.pr.title,
        worktree = worktree.display(),
        base = trigger.pr.base_ref_name,
        sha = trigger.pr.head_ref_oid,
        instructions = ORIENTATION_REVIEW_PROMPT.trim(),
        protocol = PR_REVIEW_BOT_PROMPT.trim(),
        few_shots = PR_REVIEW_FEW_SHOTS_PROMPT.trim(),
        repo_instructions = instruction_context_for_paths(
            config,
            worktree,
            &context
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|error| format!("instructions unavailable: {error:#}")),
        memory = if memory.trim().is_empty() {
            "No prior memory for this PR."
        } else {
            memory
        },
        deterministic_checks = format_verification_items(deterministic_checks),
        manifest = short_context_for_global_pass(context),
        context = short_context_for_global_pass(context),
    )
}

struct PassPromptContext<'a> {
    config: &'a Config,
    trigger: &'a ReviewTrigger,
    worktree: &'a Path,
    memory: &'a str,
    context: &'a ReviewContext,
    deterministic_checks: &'a [VerificationItem],
}

fn file_review_prompt(
    prompt: &PassPromptContext<'_>,
    memory_paths: &PromptMemoryPaths,
    shard: &ReviewShard,
) -> String {
    let shard_text = shard
        .files
        .iter()
        .map(|file| {
            let mut patch = file.annotated_patch.clone();
            truncate_text(&mut patch, file.bytes.min(60_000));
            format!(
                "File kind: {}\nStatus: {}\nAdditions: {} Deletions: {} Changes: {}\nPrevious \
                 filename: {}\n{}",
                file.kind.label(),
                file.status,
                file.additions,
                file.deletions,
                file.changes,
                file.previous_filename.as_deref().unwrap_or("none"),
                patch
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        r#"{AGENT_LINE}

You are running file review pass {pass} for {repo} PR #{pr_number}.

Use this instruction file:
```markdown
{instructions}
```

Shared review protocol:
```markdown
{protocol}
```

Few-shot examples:
```markdown
{few_shots}
```

Scope:
- Worktree: `{worktree}`
- Head SHA: `{sha}`
- Shard byte size: {shard_bytes}
- Review only this shard's files.
- Do not post GitHub comments.
- Include every shard file path in `<files_reviewed>` if you inspected it.
- Put actionable issues in `<finding>` tags; do not create `verification` items or "passed" review notes.
- Grade severity by impact, not by bucket. P1/P2 are valid for API contract, reliability, and test risks when they affect merge quality.
- Follow repository instructions as review policy. Only plugin protocol is fixed: do not write GitHub
  comments yourself, and wrap machine-readable items in the embedded tags.

Reviewer profile:
```markdown
{reviewer_profile}
```

Repository and path instructions:
```markdown
{repo_instructions}
```

Deterministic checks already run:
```text
{deterministic_checks}
```
Use failed checks only as evidence for concrete findings. Do not summarize passed checks.

Relevant prior memory:
```markdown
{memory}
```

Memory paths:
- PR memory: `{pr_memory_path}`
- Repo memory index: `{repo_index_path}`

Shard {pass} of coverage-first review:
```text
{shard_text}
```

Known non-inline-commentable files:
```text
{non_commentable}
```
        "#,
        AGENT_LINE = AGENT_LINE,
        pass = shard.index + 1,
        repo = prompt.trigger.repo,
        pr_number = prompt.trigger.pr.number,
        instructions = FILE_REVIEW_PROMPT.trim(),
        protocol = PR_REVIEW_BOT_PROMPT.trim(),
        few_shots = PR_REVIEW_FEW_SHOTS_PROMPT.trim(),
        reviewer_profile = reviewer_profile_for_shard(shard),
        repo_instructions = instruction_context_for_shard(prompt.config, prompt.worktree, shard)
            .unwrap_or_else(|error| format!("instructions unavailable: {error:#}")),
        deterministic_checks = format_verification_items(prompt.deterministic_checks),
        worktree = prompt.worktree.display(),
        sha = prompt.trigger.pr.head_ref_oid,
        shard_bytes = shard.bytes,
        memory = if prompt.memory.trim().is_empty() {
            "No prior memory for this PR."
        } else {
            prompt.memory
        },
        pr_memory_path = memory_paths.pr_memory.display(),
        repo_index_path = memory_paths.repo_index.display(),
        shard_text = shard_text,
        non_commentable = if prompt.context.non_commentable_files.is_empty() {
            "None".into()
        } else {
            prompt.context.non_commentable_files.join("\n")
        },
    )
}

fn global_review_prompt(
    prompt: &PassPromptContext<'_>,
    coverage: &ReviewCoverage,
    outputs: &[ReviewBotOutput],
) -> String {
    format!(
        r#"{AGENT_LINE}

You are running the global risk pass for {repo} PR #{pr_number}.

Use this instruction file:
```markdown
{instructions}
```

Shared review protocol:
```markdown
{protocol}
```

Few-shot examples:
```markdown
{few_shots}
```

Scope:
- Worktree: `{worktree}`
- Head SHA: `{sha}`
- Look for cross-file correctness, security, reliability/performance, and Tests/API Contract issues.
- Do not repeat findings already present in file pass outputs.
- Do not post GitHub comments.
- Put actionable issues in `<finding>` tags; do not create `verification` items or "passed" review notes.
- Grade severity by impact, not by bucket. P1/P2 are valid for API contract, reliability, and test risks when they affect merge quality.
- Follow repository instructions as review policy. Only plugin protocol is fixed: do not write GitHub
  comments yourself, and wrap machine-readable items in the embedded tags.

Repository-level instructions:
```markdown
{repo_instructions}
```

Deterministic checks already run:
```text
{deterministic_checks}
```
Use failed checks only as evidence for concrete findings. Do not summarize passed checks.

Coverage:
```text
{coverage}
```

File pass outputs:
```json
{outputs}
```

Prior memory:
```markdown
{memory}
```

Full PR context summary:
```text
{context}
```
        "#,
        AGENT_LINE = AGENT_LINE,
        repo = prompt.trigger.repo,
        pr_number = prompt.trigger.pr.number,
        instructions = GLOBAL_REVIEW_PROMPT.trim(),
        protocol = PR_REVIEW_BOT_PROMPT.trim(),
        few_shots = PR_REVIEW_FEW_SHOTS_PROMPT.trim(),
        repo_instructions = instruction_context_for_paths(
            prompt.config,
            prompt.worktree,
            &prompt
                .context
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|error| format!("instructions unavailable: {error:#}")),
        deterministic_checks = format_verification_items(prompt.deterministic_checks),
        worktree = prompt.worktree.display(),
        sha = prompt.trigger.pr.head_ref_oid,
        coverage = coverage.summary_lines(),
        outputs = serde_json::to_string_pretty(outputs).unwrap_or_else(|_| "[]".into()),
        memory = if prompt.memory.trim().is_empty() {
            "No prior memory for this PR."
        } else {
            prompt.memory
        },
        context = short_context_for_global_pass(prompt.context),
    )
}

fn short_context_for_global_pass(context: &ReviewContext) -> String {
    context
        .files
        .iter()
        .map(|file| {
            format!(
                "- `{}`: kind={} status={} +{} -{} changes={}",
                file.path,
                file.kind.label(),
                file.status,
                file.additions,
                file.deletions,
                file.changes
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn reviewer_profile_for_shard(shard: &ReviewShard) -> String {
    let paths = shard
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    let has_rust = paths.iter().any(|path| path.ends_with(".rs"));
    let has_ts = paths.iter().any(|path| {
        path.ends_with(".ts")
            || path.ends_with(".tsx")
            || path.ends_with(".js")
            || path.ends_with(".jsx")
    });
    let has_infra = paths.iter().any(|path| {
        path.contains("Dockerfile")
            || path.ends_with(".yml")
            || path.ends_with(".yaml")
            || path.ends_with(".toml")
            || path.ends_with(".json")
            || path.starts_with(".github/")
    });
    let has_tests = paths.iter().any(|path| {
        path.contains("test")
            || path.contains("spec")
            || path.contains("__tests__")
            || path.ends_with(".snap")
    });
    let has_docs = shard.files.iter().any(|file| file.kind == ReviewFileKind::Docs);
    let mut parts = Vec::new();
    if has_rust {
        parts.push(
            "Rust reviewer: focus on ownership/lifetimes, async cancellation, error propagation, \
             serde/backward compatibility, unsafe boundaries, and observable behavior changes.",
        );
    }
    if has_ts {
        parts.push(
            "TypeScript/React reviewer: focus on API contracts, state/effect dependencies, \
             rendering edge cases, accessibility, async races, and type soundness.",
        );
    }
    if has_infra {
        parts.push(
            "Infra/config reviewer: focus on secrets, permissions, deployment rollback, CI \
             breakage, path globs, and environment compatibility.",
        );
    }
    if has_tests {
        parts.push(
            "Tests/API contract reviewer: focus on missing assertions, brittle fixtures, contract \
             drift, and whether changed behavior is covered.",
        );
    }
    if has_docs {
        parts.push(
            "Docs reviewer: focus only on documentation that changes user-visible behavior, setup, \
             or operational safety; avoid style-only comments by default.",
        );
    }
    if parts.is_empty() {
        parts.push(
            "General reviewer: focus on correctness, security, reliability/performance, and \
             Tests/API Contract issues proven by this PR diff.",
        );
    }
    parts.join("\n")
}

fn instruction_context_for_shard(
    config: &Config,
    worktree: &Path,
    shard: &ReviewShard,
) -> Result<String> {
    let paths = shard
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    instruction_context_for_paths(config, worktree, &paths)
}

fn instruction_context_for_paths(
    config: &Config,
    worktree: &Path,
    changed_paths: &[String],
) -> Result<String> {
    let mut parts = Vec::new();
    for relative in [
        ".github/astrcode-review.md",
        ".github/copilot-instructions.md",
        "AGENTS.md",
        "README.md",
    ] {
        push_instruction_file(&mut parts, worktree, relative)?;
    }
    let docs = worktree.join("docs");
    if docs.is_dir() {
        for entry in fs::read_dir(&docs)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with("architecture") && name.ends_with(".md") {
                let relative = format!("docs/{name}");
                push_instruction_file(&mut parts, worktree, &relative)?;
            }
        }
    }
    let instructions_dir = worktree.join(".github/instructions");
    if instructions_dir.is_dir() {
        for entry in fs::read_dir(&instructions_dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.ends_with(".instructions.md") {
                continue;
            }
            let relative = format!(".github/instructions/{name}");
            let text = fs::read_to_string(&path)?;
            if instruction_file_matches_paths(&text, changed_paths) {
                parts.push(format!("## `{relative}`\n{}", text.trim()));
            }
        }
    }
    let mut text = if parts.is_empty() {
        "No repository instructions found.".to_owned()
    } else {
        parts.join("\n\n")
    };
    truncate_text(&mut text, config.instruction_context_max_bytes.max(1));
    Ok(text)
}

fn push_instruction_file(parts: &mut Vec<String>, worktree: &Path, relative: &str) -> Result<()> {
    let path = worktree.join(relative);
    if path.is_file() {
        let text = fs::read_to_string(&path)?;
        if !text.trim().is_empty() {
            parts.push(format!("## `{relative}`\n{}", text.trim()));
        }
    }
    Ok(())
}

fn instruction_file_matches_paths(text: &str, changed_paths: &[String]) -> bool {
    let Some(apply_to) = text
        .lines()
        .find(|line| line.trim_start().starts_with("applyTo:"))
        .and_then(|line| line.split_once(':').map(|(_, value)| value.trim()))
    else {
        return true;
    };
    let patterns = apply_to
        .split(',')
        .map(|pattern| pattern.trim().trim_matches('"').trim_matches('\''))
        .filter(|pattern| !pattern.is_empty())
        .collect::<Vec<_>>();
    patterns.is_empty()
        || changed_paths
            .iter()
            .any(|path| patterns.iter().any(|pattern| simple_glob_match(pattern, path)))
}

fn simple_glob_match(pattern: &str, path: &str) -> bool {
    if pattern == "*" || pattern == "**" {
        return true;
    }
    if let Some(ext) = pattern.strip_prefix("**/*.") {
        return path.rsplit_once('.').map(|(_, suffix)| suffix == ext).unwrap_or(false);
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path.starts_with(prefix);
    }
    if pattern.contains('*') {
        let parts = pattern.split('*').collect::<Vec<_>>();
        let mut rest = path;
        for part in parts {
            if part.is_empty() {
                continue;
            }
            let Some(index) = rest.find(part) else {
                return false;
            };
            rest = &rest[index + part.len()..];
        }
        return true;
    }
    path == pattern || path.starts_with(pattern)
}

fn format_verification_items(items: &[VerificationItem]) -> String {
    let noteworthy = items
        .iter()
        .filter(|item| item.status.as_deref() != Some("passed"))
        .map(|item| {
            format!(
                "- `{}`: {} ({})",
                item.command.as_deref().unwrap_or("unspecified"),
                item.status.as_deref().unwrap_or("unknown"),
                one_line(item.notes.as_deref().unwrap_or("no notes"))
            )
        })
        .collect::<Vec<_>>()
        ;
    if noteworthy.is_empty() {
        "No deterministic check failures. Do not summarize passed checks.".into()
    } else {
        noteworthy.join("\n")
    }
}

fn merge_review_outputs(outputs: &[ReviewBotOutput]) -> ReviewBotOutput {
    let mut merged = ReviewBotOutput::default();
    let mut reviewed = BTreeSet::new();
    for output in outputs {
        merged
            .confirmed_findings
            .extend(output.confirmed_findings.clone());
        merged
            .advisory_findings
            .extend(output.advisory_findings.clone());
        merged.observations.extend(output.observations.clone());
        for file in &output.files_reviewed {
            if reviewed.insert(file.clone()) {
                merged.files_reviewed.push(file.clone());
            }
        }
        merged
            .investigation_log
            .extend(output.investigation_log.clone());
        merged.residual_risk.extend(output.residual_risk.clone());
    }
    merged.summary = Some(merged_review_summary(
        outputs,
        merged.confirmed_findings.len() + merged.advisory_findings.len(),
    ));
    merged
}

fn merged_review_summary(outputs: &[ReviewBotOutput], finding_count: usize) -> String {
    if finding_count == 0 {
        return format!(
            "本次自动审查完成，覆盖了 {} 个审查 pass，未发现可由当前 diff 证明的可 inline 问题。",
            outputs.len()
        );
    }
    let sample = outputs
        .iter()
        .filter_map(|output| output.summary.as_deref())
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");
    if sample.is_empty() {
        format!("本次自动审查完成，发现 {finding_count} 个候选问题。")
    } else {
        format!("本次自动审查完成，发现 {finding_count} 个候选问题。{sample}")
    }
}

fn deterministic_review_verification(
    config: &Config,
    trigger: &ReviewTrigger,
    worktree: &Path,
) -> Vec<VerificationItem> {
    let base = format!("origin/{}...HEAD", trigger.pr.base_ref_name);
    let mut checks = vec![run_verification_command(
        "git diff --check",
        "git",
        &["diff", "--check", &base],
        worktree,
        "no whitespace or conflict-marker errors",
    )];
    if worktree.join("Cargo.toml").exists() {
        checks.push(run_verification_command(
            "cargo check --workspace --all-targets",
            "cargo",
            &["check", "--workspace", "--all-targets"],
            worktree,
            "cargo check passed",
        ));
        if should_run_full_tests(trigger, config) {
            checks.push(run_verification_command(
                "cargo test --workspace",
                "cargo",
                &["test", "--workspace"],
                worktree,
                "cargo test passed",
            ));
        }
    }
    if worktree.join("package.json").exists() {
        checks.push(run_verification_command(
            "npm run -s typecheck --if-present",
            "npm",
            &["run", "-s", "typecheck", "--if-present"],
            worktree,
            "typecheck passed or script absent",
        ));
        checks.push(run_verification_command(
            "npm run -s lint --if-present",
            "npm",
            &["run", "-s", "lint", "--if-present"],
            worktree,
            "lint passed or script absent",
        ));
        if should_run_full_tests(trigger, config) {
            checks.push(run_verification_command(
                "npm test --if-present",
                "npm",
                &["test", "--if-present"],
                worktree,
                "npm test passed or script absent",
            ));
        }
    }
    checks
}

fn run_verification_command(
    label: &str,
    program: &str,
    args: &[&str],
    worktree: &Path,
    success_notes: &str,
) -> VerificationItem {
    match run_command(program, args, Some(worktree)) {
        Ok(output) if output.trim().is_empty() => VerificationItem {
            command: Some(label.into()),
            status: Some("passed".into()),
            notes: Some(success_notes.into()),
        },
        Ok(output) => VerificationItem {
            command: Some(label.into()),
            status: Some("passed".into()),
            notes: Some(one_line(&output)),
        },
        Err(error) => {
            let text = format!("{error:#}");
            VerificationItem {
                command: Some(label.into()),
                status: Some(
                    if is_environment_tooling_failure(&text) {
                        "skipped"
                    } else {
                        "failed"
                    }
                    .into(),
                ),
                notes: Some(one_line(&text)),
            }
        },
    }
}

fn is_environment_tooling_failure(error: &str) -> bool {
    error.contains("linker `cc` not found")
        || error.contains("command not found")
        || error.contains("No such file or directory")
        || error.contains("could not execute process")
}

fn should_run_full_tests(trigger: &ReviewTrigger, config: &Config) -> bool {
    if !config.full_tests_require_trigger_keyword {
        return true;
    }
    let instruction = trigger_instruction(trigger).to_ascii_lowercase();
    instruction.contains("full test") || instruction.contains("跑测试")
}

fn review_prompt(
    trigger: &ReviewTrigger,
    worktree: &Path,
    memory: &str,
    memory_paths: &PromptMemoryPaths,
    reused_session: bool,
    review_context: Option<&ReviewContext>,
) -> String {
    let author = trigger
        .pr
        .author
        .as_ref()
        .map(|user| user.login.as_str())
        .unwrap_or("unknown");
    let trigger_author = trigger
        .comment()
        .and_then(|comment| comment.user.as_ref())
        .map(|user| user.login.as_str())
        .unwrap_or("auto:new-pr");
    let trigger_body = trigger_instruction(trigger);
    let files = changed_file_summary(&trigger.pr);
    let pr_body = trigger
        .pr
        .body
        .as_deref()
        .filter(|body| !body.trim().is_empty())
        .unwrap_or("No PR body was available from GitHub metadata.");
    let is_review_task = review_context.is_some();
    let review_hint = if trigger.is_auto_review() {
        "This is an automatic review for a newly discovered PR. Use the embedded PR review bot \
         instructions and return tagged Markdown findings. Do not wait for another user prompt."
    } else if is_review_task {
        "The trigger asks for review. Use the embedded PR review bot instructions and return \
         tagged Markdown findings."
    } else {
        "The trigger does not explicitly ask for review. Do not force a code review; follow the \
         trigger comment directly."
    };
    let review_bot_section = if is_review_task {
        format!(
            r#"内置 PR 审查规范:
```markdown
{}
```

Few-shot examples:
```markdown
{}
```"#,
            PR_REVIEW_BOT_PROMPT.trim(),
            PR_REVIEW_FEW_SHOTS_PROMPT.trim()
        )
    } else {
        "内置 PR 审查规范：本 trigger 不是 review 任务，因此不加载。"
            .into()
    };
    let review_context_section = review_context
        .map(|context| {
            format!(
                r#"Plugin-collected GitHub PR context:
```text
{}
```"#,
                context.text.trim()
            )
        })
        .unwrap_or_else(|| {
            "Plugin-collected GitHub PR context: not loaded because this trigger is not a review \
             task."
                .into()
        });
    let response_instruction = if is_review_task {
        "Write concise Markdown, but wrap every actionable issue in <finding> tags and list \
         inspected files in <files_reviewed>. Do not post GitHub comments yourself; the plugin \
         validates the tags and publishes inline comments."
    } else {
        "Return GitHub-ready Markdown only."
    };
    format!(
        r##"{AGENT_LINE}

You are handling GitHub PR #{pr_number} in {repo}. Follow the trigger instruction as the primary instruction.

PR title: {title}
PR URL: {url}
Author: {author}
Base branch: {base}
Head SHA: {sha}
Worktree: {worktree}
Trigger type: {trigger_kind}
Trigger comment id: {comment_id}
Trigger author: {trigger_author}
Trigger created at: {created_at}

PR body:
```markdown
{pr_body}
```

Changed files from GitHub metadata:
```text
{files}
```

Trigger instruction:
```text
{trigger_body}
```

Relevant prior PR memory:
```markdown
{memory}
```

Context lookup:
- Work from the checked-out PR worktree: `{worktree}`.
- Treat `{repo}` PR #{pr_number} at head `{sha}` as the canonical scope unless the trigger explicitly says otherwise.
- The plugin has already collected live GitHub PR metadata, PR files, checks, and annotated diff lines for review tasks.
- Use targeted `git diff origin/{base}...HEAD -- <path>` and `rg` for extra local context when needed.
- Prefer `rg` over broad grep/find when inspecting callers, tests, config, and related symbols.

Memory lookup:
- Repo memory index: `{repo_index_path}`
- This PR memory: `{pr_memory_path}`
- Run audit log: `{runs_log_path}`
- Search memory deliberately, for example:
  - `rg -n "#{pr_number}|{sha}|{comment_id}|{repo_search}" "{repo_index_path}" "{pr_memory_path}" "{runs_log_path}"`
  - `rg -n "keyword from trigger|changed symbol|related subsystem" "{repo_index_path}" "{pr_memory_path}"`
- Use repo-level memory to discover recurring patterns across PRs, but use PR-level memory as the stronger signal for this PR.
- Use "Related PRs and Issues" as reminder context. If a related PR/issue materially affects the review, cite it in observations or the final report with the reason.
- Never repeat an old finding from memory unless the current diff still proves it.

Execution instructions:
- This PR has one persistent Astrcode session. Treat this turn as the latest review pass for the same PR.
- Session mode: {session_mode}.
- {review_hint}
- Do exactly what the trigger comment asks for.
- For review tasks, follow the embedded PR review bot instructions exactly. They are part of this plugin binary and do not depend on any external skill.
- For review tasks, do not run `gh api` to create comments. The plugin owns GitHub comment publishing after validating your finding tags.
- For review tasks, read-only `gh pr view`, `gh pr diff`, `gh pr checks`, `gh issue list`, `gh pr list`, and `gh api` GET calls are allowed when useful.
- The plugin posts all GitHub review comments after you respond. Do not post the final summary yourself.
- For non-review tasks, still stay scoped to this PR/repository unless the user clearly asks otherwise.
- Run the narrowest useful local verification commands if feasible.
- Do not run broad workspace-wide builds such as `cargo check --workspace` unless the trigger explicitly asks for full verification.
- Prefer targeted checks for changed crates, files, or tests.
- Do not use `sudo`, `sudo -S`, privilege escalation, or system package managers (`apt`, `apt-get`, `dnf`, `yum`, `pacman`, `apk`, `brew`, etc.) in the review environment.
- If build tools or system dependencies are missing, do not try to install them. Report local verification as blocked with the exact missing tool or error, then continue with static review, CI logs, or narrower checks where useful.
- When using shell pipelines for verification, enable pipeline failure visibility (`set -o pipefail` in POSIX shells) or split the command into separate steps. Do not hide failures behind `tail`, `grep`, or other final pipeline commands.
- Do not repeatedly poll or re-check a completed background shell command. Once a command has completed, use its output and move on.
- Use memory as context and as a search index; verify everything against live files, git diff, or GitHub metadata before responding.
{review_context_section}
{review_bot_section}

{response_instruction}
"##,
        pr_number = trigger.pr.number,
        repo = trigger.repo,
        title = trigger.pr.title.as_str(),
        url = trigger.pr.url,
        author = author,
        base = trigger.pr.base_ref_name,
        sha = trigger.pr.head_ref_oid,
        worktree = worktree.display(),
        repo_index_path = memory_paths.repo_index.display(),
        pr_memory_path = memory_paths.pr_memory.display(),
        runs_log_path = memory_paths.runs_log.display(),
        trigger_kind = trigger.trigger_kind_name(),
        comment_id = trigger
            .comment()
            .map(|comment| comment.id.to_string())
            .unwrap_or_else(|| "none; automatic new PR review".into()),
        trigger_author = trigger_author,
        created_at = trigger
            .comment()
            .and_then(|comment| comment.created_at.as_deref())
            .unwrap_or("automatic trigger"),
        pr_body = pr_body,
        files = files,
        trigger_body = trigger_body,
        memory = if memory.trim().is_empty() {
            "No prior memory for this PR."
        } else {
            memory
        },
        repo_search = trigger.repo.replace('/', "__"),
        session_mode = if reused_session {
            "reused existing PR session"
        } else {
            "created new PR session"
        },
        review_hint = review_hint,
        review_bot_section = review_bot_section,
        review_context_section = review_context_section,
        response_instruction = response_instruction,
        AGENT_LINE = AGENT_LINE,
    )
}

fn trigger_requests_review(trigger: &ReviewTrigger) -> bool {
    trigger.is_auto_review() || asks_for_review(&trigger_instruction(trigger))
}

fn asks_for_review(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("review")
        || lower.contains("cr")
        || lower.contains("code review")
        || text.contains("审查")
        || text.contains("检查")
        || text.contains("看看")
}

fn trigger_instruction(trigger: &ReviewTrigger) -> String {
    match trigger.comment() {
        Some(comment) => comment.body.as_deref().unwrap_or("").to_owned(),
        None => "这是新 PR 首次发现自动 review，请按插件内置 PR review bot \
             规范做一次代码审查。请聚焦当前 PR 相对 base branch 的 \
             diff，并输出插件可校验和发布的结构化 findings。"
            .to_string(),
    }
}

fn changed_file_summary(pr: &PullRequest) -> String {
    if pr.files.is_empty() {
        return "No changed file list was available from GitHub metadata; use gh pr view/files or \
                git diff in the worktree."
            .into();
    }
    pr.files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect_review_context(
    config: &Config,
    trigger: &ReviewTrigger,
    worktree: &Path,
) -> Result<ReviewContext> {
    let pr_number = trigger.pr.number.to_string();
    let pr_view = run_command(
        "gh",
        &[
            "pr",
            "view",
            &pr_number,
            "--repo",
            &trigger.repo,
            "--json",
            "title,body,baseRefName,headRefOid,files,commits,comments,reviews,reviewDecision,\
             mergeStateStatus",
        ],
        None,
    )
    .with_context(|| {
        format!(
            "collect gh pr view for {}#{}",
            trigger.repo, trigger.pr.number
        )
    })?;
    let name_only = run_command(
        "gh",
        &[
            "pr",
            "diff",
            &pr_number,
            "--repo",
            &trigger.repo,
            "--name-only",
        ],
        None,
    )
    .with_context(|| {
        format!(
            "collect gh pr diff --name-only for {}#{}",
            trigger.repo, trigger.pr.number
        )
    })?;
    let checks = run_command(
        "gh",
        &["pr", "checks", &pr_number, "--repo", &trigger.repo],
        None,
    )
    .unwrap_or_else(|error| format!("gh pr checks unavailable: {error:#}"));
    let diff_stat = run_command(
        "git",
        &[
            "diff",
            "--stat",
            &format!("origin/{}...HEAD", trigger.pr.base_ref_name),
        ],
        Some(worktree),
    )
    .unwrap_or_else(|error| format!("git diff --stat unavailable: {error:#}"));
    let files = pull_request_files(&trigger.repo, trigger.pr.number)?;
    let (file_contexts, commentable_lines, non_commentable_files) =
        build_review_file_contexts(config, &files);
    let annotated = file_contexts
        .iter()
        .map(|file| file.annotated_patch.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let changed_files_text = if name_only.trim().is_empty() {
        "No files returned.".to_string()
    } else {
        name_only.clone()
    };
    let annotated_text = if annotated.trim().is_empty() {
        "No annotated patch lines were available.".to_string()
    } else {
        annotated.clone()
    };
    let non_commentable_text = if non_commentable_files.is_empty() {
        "None".to_string()
    } else {
        non_commentable_files.join("\n")
    };

    let mut text = format!(
        r#"GitHub command audit:
- `gh pr view {pr_number} --repo {repo} --json ...`: collected
- `gh pr diff {pr_number} --repo {repo} --name-only`: collected
- `gh api --paginate --slurp repos/{repo}/pulls/{pr_number}/files?per_page=100`: collected {file_count} file(s)
- `gh pr checks {pr_number} --repo {repo}`: collected or recorded as unavailable
- `git diff --stat origin/{base}...HEAD`: collected or recorded as unavailable

PR metadata JSON:
{pr_view}

Changed files from `gh pr diff --name-only`:
{changed_files_text}

Checks:
{checks}

Diff stat:
{diff_stat}

Annotated diff.
Use only `RIGHT <line>` or `LEFT <line>` locations that appear here for findings:
{annotated_text}

Non-inline-commentable files:
{non_commentable_text}
"#,
        pr_number = trigger.pr.number,
        repo = trigger.repo,
        file_count = files.len(),
        base = trigger.pr.base_ref_name,
        pr_view = pr_view,
        changed_files_text = changed_files_text,
        checks = checks,
        diff_stat = diff_stat,
        annotated_text = annotated_text,
        non_commentable_text = non_commentable_text,
    );
    let truncated = truncate_text(&mut text, config.review_context_max_bytes);
    Ok(ReviewContext {
        text,
        commentable_lines,
        non_commentable_files,
        truncated,
        files: file_contexts,
    })
}

fn pull_request_files(repo: &str, pr_number: u64) -> Result<Vec<PullRequestApiFile>> {
    let endpoint = format!("repos/{repo}/pulls/{pr_number}/files?per_page=100");
    let output = run_command("gh", &["api", "--paginate", "--slurp", &endpoint], None)?;
    let pages: Vec<Vec<PullRequestApiFile>> = serde_json::from_str(&output)
        .with_context(|| format!("parse gh paginated pull files for {repo}#{pr_number}"))?;
    Ok(pages.into_iter().flatten().collect())
}

fn build_review_file_contexts(
    config: &Config,
    files: &[PullRequestApiFile],
) -> (
    Vec<ReviewFileContext>,
    BTreeSet<CommentLineKey>,
    Vec<String>,
) {
    let mut contexts = Vec::new();
    let mut commentable_lines = BTreeSet::new();
    let mut non_commentable_files = Vec::new();
    for file in files {
        contexts.push(review_file_context(
            config,
            file,
            &mut commentable_lines,
            &mut non_commentable_files,
        ));
    }
    (contexts, commentable_lines, non_commentable_files)
}

fn review_file_context(
    config: &Config,
    file: &PullRequestApiFile,
    commentable_lines: &mut BTreeSet<CommentLineKey>,
    non_commentable_files: &mut Vec<String>,
) -> ReviewFileContext {
    let mut annotated = String::new();
    let status = file.status.as_deref().unwrap_or("modified");
    annotated.push_str(&format!(
        "\n--- file: {} status={} +{} -{} changes={}\n",
        file.filename, status, file.additions, file.deletions, file.changes
    ));
    if let Some(previous) = file.previous_filename.as_deref() {
        annotated.push_str(&format!("previous_filename: {previous}\n"));
    }
    let mut kind = classify_review_file(file);
    match file.patch.as_deref() {
        Some(patch) if !patch.trim().is_empty() => {
            annotate_patch(file, patch, &mut annotated, commentable_lines);
        },
        _ => {
            kind = ReviewFileKind::NoPatch;
            annotated
                .push_str("no patch available; findings in this file cannot be inline-commented\n");
            non_commentable_files.push(format!("{} ({status}; no patch)", file.filename));
        },
    }
    let bytes = annotated.len();
    if matches!(kind, ReviewFileKind::Code | ReviewFileKind::Docs)
        && bytes > config.review_shard_max_bytes
    {
        kind = ReviewFileKind::Oversized;
    }
    ReviewFileContext {
        path: file.filename.clone(),
        status: status.into(),
        additions: file.additions,
        deletions: file.deletions,
        changes: file.changes,
        previous_filename: file.previous_filename.clone(),
        annotated_patch: annotated,
        kind,
        bytes,
    }
}

#[cfg(test)]
fn annotate_pull_files(
    files: &[PullRequestApiFile],
    annotated: &mut String,
    commentable_lines: &mut BTreeSet<CommentLineKey>,
    non_commentable_files: &mut Vec<String>,
) {
    for file in files {
        let status = file.status.as_deref().unwrap_or("modified");
        annotated.push_str(&format!(
            "\n--- file: {} status={} +{} -{} changes={}\n",
            file.filename, status, file.additions, file.deletions, file.changes
        ));
        if let Some(previous) = file.previous_filename.as_deref() {
            annotated.push_str(&format!("previous_filename: {previous}\n"));
        }
        match file.patch.as_deref() {
            Some(patch) if !patch.trim().is_empty() => {
                annotate_patch(file, patch, annotated, commentable_lines);
            },
            _ => {
                annotated.push_str(
                    "no patch available; findings in this file cannot be inline-commented\n",
                );
                non_commentable_files.push(format!("{} ({status}; no patch)", file.filename));
            },
        }
    }
}

fn classify_review_file(file: &PullRequestApiFile) -> ReviewFileKind {
    let path = file.filename.to_ascii_lowercase();
    if is_generated_path(&path) {
        ReviewFileKind::Generated
    } else if is_docs_path(&path) {
        ReviewFileKind::Docs
    } else {
        ReviewFileKind::Code
    }
}

fn is_docs_path(path: &str) -> bool {
    path.starts_with("docs/")
        || path.ends_with(".md")
        || path.ends_with(".mdx")
        || path.ends_with(".txt")
        || path.ends_with(".rst")
}

fn is_generated_path(path: &str) -> bool {
    path.contains("/generated/")
        || path.ends_with(".lock")
        || path.ends_with("package-lock.json")
        || path.ends_with("pnpm-lock.yaml")
        || path.ends_with("yarn.lock")
        || path.ends_with("cargo.lock")
        || path.ends_with(".min.js")
        || path.ends_with(".snap")
}

fn annotate_patch(
    file: &PullRequestApiFile,
    patch: &str,
    annotated: &mut String,
    commentable_lines: &mut BTreeSet<CommentLineKey>,
) {
    let mut old_line = 0u64;
    let mut new_line = 0u64;
    for line in patch.lines() {
        if let Some((old_start, new_start)) = parse_hunk_header(line) {
            old_line = old_start;
            new_line = new_start;
            annotated.push_str(line);
            annotated.push('\n');
            continue;
        }
        if line.starts_with("\\ No newline at end of file") {
            annotated.push_str(line);
            annotated.push('\n');
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            annotated.push_str(&format!("RIGHT {new_line} +{}\n", one_line(rest)));
            commentable_lines.insert(CommentLineKey {
                path: file.filename.clone(),
                side: CommentSide::Right,
                line: new_line,
            });
            new_line = new_line.saturating_add(1);
        } else if let Some(rest) = line.strip_prefix('-') {
            annotated.push_str(&format!("LEFT {old_line} -{}\n", one_line(rest)));
            commentable_lines.insert(CommentLineKey {
                path: file.filename.clone(),
                side: CommentSide::Left,
                line: old_line,
            });
            old_line = old_line.saturating_add(1);
        } else if let Some(rest) = line.strip_prefix(' ') {
            annotated.push_str(&format!("RIGHT {new_line}  {}\n", one_line(rest)));
            commentable_lines.insert(CommentLineKey {
                path: file.filename.clone(),
                side: CommentSide::Right,
                line: new_line,
            });
            old_line = old_line.saturating_add(1);
            new_line = new_line.saturating_add(1);
        }
    }
}

fn parse_hunk_header(line: &str) -> Option<(u64, u64)> {
    if !line.starts_with("@@") {
        return None;
    }
    let mut parts = line.split_whitespace();
    parts.next()?;
    let old_range = parts.next()?;
    let new_range = parts.next()?;
    Some((
        parse_range_start(old_range.trim_start_matches('-'))?,
        parse_range_start(new_range.trim_start_matches('+'))?,
    ))
}

fn parse_range_start(range: &str) -> Option<u64> {
    range.split(',').next()?.parse().ok()
}

fn one_line(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = collapsed.chars().take(500).collect::<String>();
    if collapsed.chars().count() > 500 {
        out.push_str("...");
    }
    out
}

fn truncate_text(text: &mut String, max_bytes: usize) -> bool {
    if max_bytes == 0 || text.len() <= max_bytes {
        return false;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    text.push_str("\n\n[truncated by astrcode-pr-review-agent]\n");
    true
}

fn checkout_pr(config: &Config, repo: &str, pr_number: u64) -> Result<PathBuf> {
    let root = config.worktree_dir_path()?;
    fs::create_dir_all(&root)?;
    let target = root.join(repo_key(repo)).join(format!("pr-{pr_number}"));
    if !target.join(".git").exists() {
        if target.exists() {
            anyhow::bail!(
                "worktree target exists but is not a git repo: {}",
                target.display()
            );
        }
        run_command("gh", &["repo", "clone", repo, path_str(&target)?], None)?;
    }
    run_command("git", &["fetch", "--all", "--prune"], Some(&target))?;
    run_command(
        "gh",
        &["pr", "checkout", &pr_number.to_string(), "--repo", repo],
        Some(&target),
    )?;
    Ok(target)
}

fn configured_open_prs(config: &Config) -> Result<Vec<(String, PullRequest)>> {
    let mut candidates = BTreeMap::new();
    for repo in &config.repos {
        for pr in open_prs(repo)? {
            candidates.insert(pr_key(repo, pr.number), (repo.clone(), pr));
        }
    }

    Ok(candidates.into_values().collect())
}

fn mentioned_open_prs(config: &Config) -> Result<Vec<(String, PullRequest)>> {
    let mention = mention_login(config);
    let limit = config.mention_search_limit.max(1).to_string();
    let search_results: Vec<SearchPullRequest> = gh_json_with_timeout(&[
        "search",
        "prs",
        "--mentions",
        &mention,
        "--state",
        "open",
        "--limit",
        &limit,
        "--json",
        "number,repository",
    ], poll_command_timeout())?;

    let mut prs = Vec::new();
    for result in search_results {
        let repo = result.repository.name_with_owner;
        match pr_details_quick(&repo, result.number) {
            Ok(pr) => prs.push((repo, pr)),
            Err(error) => eprintln!(
                "failed to read globally mentioned PR {repo}#{} details: {error:#}",
                result.number
            ),
        }
    }
    Ok(prs)
}

fn mention_login(config: &Config) -> String {
    config.mention.trim().trim_start_matches('@').to_owned()
}

fn open_prs(repo: &str) -> Result<Vec<PullRequest>> {
    gh_json_with_timeout(&[
        "pr",
        "list",
        "--repo",
        repo,
        "--state",
        "open",
        "--json",
        "number,title,url,headRefOid,baseRefName,author",
    ], poll_command_timeout())
}

fn pr_details(repo: &str, pr_number: u64) -> Result<PullRequest> {
    gh_json(&[
        "pr",
        "view",
        &pr_number.to_string(),
        "--repo",
        repo,
        "--json",
        "number,title,url,body,headRefOid,baseRefName,files,author",
    ])
}

fn pr_details_quick(repo: &str, pr_number: u64) -> Result<PullRequest> {
    gh_json_with_timeout(&[
        "pr",
        "view",
        &pr_number.to_string(),
        "--repo",
        repo,
        "--json",
        "number,title,url,body,headRefOid,baseRefName,files,author",
    ], poll_command_timeout())
}

fn issue_comments_quick(repo: &str, pr_number: u64) -> Result<Vec<IssueComment>> {
    let endpoint = format!("repos/{repo}/issues/{pr_number}/comments?per_page=100");
    let output = run_command_with_timeout(
        "gh",
        &["api", "--paginate", "--slurp", &endpoint],
        None,
        poll_command_timeout(),
    )?;
    let pages: Vec<Vec<IssueComment>> = serde_json::from_str(&output)
        .with_context(|| format!("parse gh paginated comments for {repo}#{pr_number}"))?;
    Ok(pages.into_iter().flatten().collect())
}

fn add_comment_reaction(repo: &str, comment_id: u64, reaction: &str) -> Result<()> {
    let result = run_command(
        "gh",
        &[
            "api",
            "--method",
            "POST",
            "-H",
            "Accept: application/vnd.github+json",
            &format!("repos/{repo}/issues/comments/{comment_id}/reactions"),
            "-f",
            &format!("content={reaction}"),
        ],
        None,
    );
    match result {
        Ok(_) => Ok(()),
        Err(error)
            if error.to_string().contains("already_exists")
                || error.to_string().contains("already exists") =>
        {
            Ok(())
        },
        Err(error) => {
            Err(error).with_context(|| format!("add reaction to {repo} comment {comment_id}"))
        },
    }
}

fn post_review_comment(
    config: &Config,
    trigger: &ReviewTrigger,
    session_id: &str,
    review: &str,
) -> Result<Option<String>> {
    let body = review_comment_body(config, trigger, session_id, review);
    let payload = json!({ "body": body });
    let mut file = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(file.as_file_mut(), &payload)?;
    file.as_file_mut().flush()?;
    let out: Value = gh_json_with_input(
        &[
            "api",
            "--method",
            "POST",
            &format!(
                "repos/{}/issues/{}/comments",
                trigger.repo, trigger.pr.number
            ),
        ],
        file.path(),
    )?;
    Ok(out
        .get("html_url")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

fn review_comment_body(
    config: &Config,
    trigger: &ReviewTrigger,
    session_id: &str,
    review: &str,
) -> String {
    let trigger_line = match trigger.comment() {
        Some(comment) => format!("Trigger comment: `{}`", comment.id),
        None => "Trigger: new PR auto review".into(),
    };
    format!(
        r#"{marker}
{AGENT_LINE}

Review session: `{session_id}`
{trigger_line}
Head SHA: `{sha}`

{review}
"#,
        marker = config.comment_marker,
        AGENT_LINE = AGENT_LINE,
        session_id = session_id,
        trigger_line = trigger_line,
        sha = trigger.pr.head_ref_oid,
        review = review.trim(),
    )
}

fn post_auto_review_start_comment(
    config: &Config,
    trigger: &ReviewTrigger,
) -> Result<Option<String>> {
    let body = auto_review_start_comment_body(config, trigger);
    post_issue_comment(&trigger.repo, trigger.pr.number, &body)
}

fn auto_review_start_comment_body(config: &Config, trigger: &ReviewTrigger) -> String {
    format!(
        r#"{marker}
{AGENT_LINE}

检测到新的 PR，已启动自动 review。

Head SHA: `{sha}`
"#,
        marker = config.comment_marker,
        AGENT_LINE = AGENT_LINE,
        sha = trigger.pr.head_ref_oid,
    )
}

fn post_auto_review_failure_comment(
    config: &Config,
    trigger: &ReviewTrigger,
    error: &str,
) -> Result<Option<String>> {
    let body = auto_review_failure_comment_body(config, trigger, error);
    post_issue_comment(&trigger.repo, trigger.pr.number, &body)
}

fn auto_review_failure_comment_body(
    config: &Config,
    trigger: &ReviewTrigger,
    error: &str,
) -> String {
    format!(
        r#"{marker}
{AGENT_LINE}

自动 review 失败，未继续重试。

Head SHA: `{sha}`
Error: `{error}`

可以在本 PR 评论 `{mention} review it` 手动重新触发。
"#,
        marker = config.comment_marker,
        AGENT_LINE = AGENT_LINE,
        sha = trigger.pr.head_ref_oid,
        error = error.replace('`', "'"),
        mention = config.mention,
    )
}

fn post_issue_comment(repo: &str, pr_number: u64, body: &str) -> Result<Option<String>> {
    let payload = json!({ "body": body });
    let mut file = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(file.as_file_mut(), &payload)?;
    file.as_file_mut().flush()?;
    let out: Value = gh_json_with_input(
        &[
            "api",
            "--method",
            "POST",
            &format!("repos/{repo}/issues/{pr_number}/comments"),
        ],
        file.path(),
    )?;
    Ok(out
        .get("html_url")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

async fn parse_or_repair_review_output(
    config: &Config,
    run_info: &RunInfo,
    session_id: &str,
    initial: &str,
) -> Result<ReviewBotOutput> {
    let mut latest = initial.to_owned();
    let mut last_error = None;
    for attempt in 0..=config.json_repair_attempts {
        match parse_review_bot_output(&latest) {
            Ok(output) => return Ok(output),
            Err(error) => {
                last_error = Some(error);
                if attempt == config.json_repair_attempts {
                    break;
                }
                let prompt = json_repair_prompt(&latest, last_error.as_ref().unwrap());
                latest = submit_prompt_and_wait(
                    run_info,
                    session_id,
                    &prompt,
                    Duration::from_secs(config.review_timeout_seconds),
                )
                .await?;
            },
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("review bot output was empty")))
}

fn parse_review_bot_output(text: &str) -> Result<ReviewBotOutput> {
    let json_error = match extract_json_object(text) {
        Some(candidate) => match serde_json::from_str(candidate) {
            Ok(output) => return Ok(output),
            Err(error) => Some(error),
        },
        None => None,
    };
    match parse_tagged_review_output(text) {
        Ok(output) => Ok(output),
        Err(tag_error) => match json_error {
            Some(error) => Err(error).with_context(|| {
                format!(
                    "parse assistant response as ReviewBotOutput JSON; tagged parse also failed: \
                     {tag_error:#}"
                )
            }),
            None => Err(tag_error),
        },
    }
}

#[derive(Debug)]
struct TaggedBlock {
    attrs: BTreeMap<String, String>,
    body: String,
}

fn parse_tagged_review_output(text: &str) -> Result<ReviewBotOutput> {
    let mut output = ReviewBotOutput::default();
    let mut recognized = false;

    let files_reviewed = extract_tag_blocks(text, "files_reviewed")
        .into_iter()
        .flat_map(|block| parse_tagged_list(&block.body))
        .collect::<Vec<_>>();
    if !files_reviewed.is_empty() {
        recognized = true;
        output.files_reviewed = files_reviewed;
    }

    for block in extract_tag_blocks(text, "finding") {
        recognized = true;
        let kind = block
            .attrs
            .get("kind")
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_else(|| "confirmed".into());
        let finding = tagged_finding(&block);
        if kind == "advisory" {
            output.advisory_findings.push(finding);
        } else {
            output.confirmed_findings.push(finding);
        }
    }

    for block in extract_tag_blocks(text, "observation") {
        recognized = true;
        output.observations.push(tagged_observation(&block));
    }

    let investigation_log = extract_tag_blocks(text, "investigation_log")
        .into_iter()
        .flat_map(|block| parse_tagged_list(&block.body))
        .collect::<Vec<_>>();
    if !investigation_log.is_empty() {
        recognized = true;
        output.investigation_log = investigation_log;
    }

    let residual_risk = extract_tag_blocks(text, "residual_risk")
        .into_iter()
        .flat_map(|block| parse_tagged_list(&block.body))
        .collect::<Vec<_>>();
    if !residual_risk.is_empty() {
        recognized = true;
        output.residual_risk = residual_risk;
    }

    if let Some(summary) = extract_tag_blocks(text, "summary")
        .into_iter()
        .map(|block| block.body.trim().to_owned())
        .find(|summary| !summary.is_empty())
    {
        recognized = true;
        output.summary = Some(summary);
    }

    if recognized {
        Ok(output)
    } else {
        anyhow::bail!("assistant response contained neither review JSON nor tagged review blocks")
    }
}

fn tagged_finding(block: &TaggedBlock) -> ReviewFinding {
    let sections = tagged_body_sections(&block.body);
    ReviewFinding {
        severity: tagged_value(block, &sections, &["priority", "severity"]),
        confidence: tagged_value(block, &sections, &["confidence"]),
        category: tagged_value(block, &sections, &["category"]),
        path: tagged_value(block, &sections, &["path", "file"]),
        side: tagged_value(block, &sections, &["side"]).or_else(|| Some("RIGHT".into())),
        line: tagged_u64(block, &sections, &["line"]),
        title: tagged_value(block, &sections, &["title"])
            .or_else(|| first_meaningful_line(&block.body)),
        issue: tagged_value(block, &sections, &["issue", "problem"])
            .or_else(|| first_meaningful_line(&block.body)),
        evidence: tagged_value(block, &sections, &["evidence"]),
        project_context: tagged_value(block, &sections, &["project_context", "project context"]),
        impact: tagged_value(block, &sections, &["impact"]),
        fix: tagged_value(block, &sections, &["fix", "next_step", "next step"]),
    }
}

fn tagged_observation(block: &TaggedBlock) -> ReviewObservation {
    let sections = tagged_body_sections(&block.body);
    ReviewObservation {
        confidence: tagged_value(block, &sections, &["confidence"]).or_else(|| Some("low".into())),
        category: tagged_value(block, &sections, &["category"]).or_else(|| Some("Observation".into())),
        path: tagged_value(block, &sections, &["path", "file"]),
        line: tagged_u64(block, &sections, &["line"]),
        title: tagged_value(block, &sections, &["title"])
            .or_else(|| first_meaningful_line(&block.body)),
        evidence: tagged_value(block, &sections, &["evidence"])
            .or_else(|| first_meaningful_line(&block.body)),
        project_context: tagged_value(block, &sections, &["project_context", "project context"]),
        impact: tagged_value(block, &sections, &["impact"]),
        next_step: tagged_value(block, &sections, &["next_step", "next step", "fix"]),
    }
}

fn tagged_value(
    block: &TaggedBlock,
    sections: &BTreeMap<String, String>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| {
            let normalized = normalize_tag_key(key);
            block
                .attrs
                .get(&normalized)
                .or_else(|| sections.get(&normalized))
                .map(|value| value.trim().to_owned())
        })
        .filter(|value| !value.is_empty())
}

fn tagged_u64(
    block: &TaggedBlock,
    sections: &BTreeMap<String, String>,
    keys: &[&str],
) -> Option<u64> {
    tagged_value(block, sections, keys).and_then(|value| value.parse::<u64>().ok())
}

fn extract_tag_blocks(text: &str, tag: &str) -> Vec<TaggedBlock> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut cursor = 0usize;
    let mut blocks = Vec::new();
    while let Some(start_rel) = text[cursor..].find(&open) {
        let start = cursor + start_rel;
        let attrs_start = start + open.len();
        let Some(open_end_rel) = text[attrs_start..].find('>') else {
            break;
        };
        let open_end = attrs_start + open_end_rel;
        let body_start = open_end + 1;
        let Some(close_rel) = text[body_start..].find(&close) else {
            break;
        };
        let body_end = body_start + close_rel;
        blocks.push(TaggedBlock {
            attrs: parse_tag_attrs(&text[attrs_start..open_end]),
            body: text[body_start..body_end].trim().to_owned(),
        });
        cursor = body_end + close.len();
    }
    blocks
}

fn parse_tag_attrs(input: &str) -> BTreeMap<String, String> {
    let chars = input.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut attrs = BTreeMap::new();
    while index < chars.len() {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        let key_start = index;
        while index < chars.len()
            && !chars[index].is_whitespace()
            && chars[index] != '='
            && chars[index] != '/'
        {
            index += 1;
        }
        if key_start == index {
            index += 1;
            continue;
        }
        let key = normalize_tag_key(&chars[key_start..index].iter().collect::<String>());
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index >= chars.len() || chars[index] != '=' {
            attrs.insert(key, "true".into());
            continue;
        }
        index += 1;
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        let value = if chars[index] == '"' || chars[index] == '\'' {
            let quote = chars[index];
            index += 1;
            let value_start = index;
            while index < chars.len() && chars[index] != quote {
                index += 1;
            }
            let value = chars[value_start..index].iter().collect::<String>();
            if index < chars.len() {
                index += 1;
            }
            value
        } else {
            let value_start = index;
            while index < chars.len() && !chars[index].is_whitespace() {
                index += 1;
            }
            chars[value_start..index].iter().collect::<String>()
        };
        attrs.insert(key, value);
    }
    attrs
}

fn tagged_body_sections(body: &str) -> BTreeMap<String, String> {
    let mut sections = BTreeMap::new();
    let mut current_key: Option<String> = None;
    let mut current_value = String::new();
    for line in body.lines() {
        if let Some((key, value)) = parse_tagged_section_header(line) {
            if let Some(key) = current_key.take() {
                sections.insert(key, current_value.trim().to_owned());
                current_value.clear();
            }
            current_key = Some(key);
            current_value.push_str(value.trim());
        } else if current_key.is_some() {
            if !current_value.is_empty() {
                current_value.push('\n');
            }
            current_value.push_str(line.trim());
        }
    }
    if let Some(key) = current_key {
        sections.insert(key, current_value.trim().to_owned());
    }
    sections
}

fn parse_tagged_section_header(line: &str) -> Option<(String, &str)> {
    let trimmed = line
        .trim()
        .trim_start_matches('-')
        .trim_start()
        .trim_start_matches('*')
        .trim();
    let (raw_key, value) = trimmed.split_once(':')?;
    let key = raw_key
        .trim()
        .trim_matches('*')
        .trim_matches('`')
        .trim();
    let normalized = normalize_tag_key(key);
    matches!(
        normalized.as_str(),
        "issue"
            | "problem"
            | "evidence"
            | "project_context"
            | "impact"
            | "fix"
            | "next_step"
            | "priority"
            | "severity"
            | "confidence"
            | "category"
            | "path"
            | "file"
            | "side"
            | "line"
            | "title"
    )
    .then_some((normalized, value))
}

fn parse_tagged_list(body: &str) -> Vec<String> {
    body.lines()
        .flat_map(|line| line.split(','))
        .map(|item| {
            item.trim()
                .trim_start_matches('-')
                .trim_start()
                .trim_matches('`')
                .trim()
                .to_owned()
        })
        .filter(|item| !item.is_empty() && item != "None" && item != "无")
        .collect()
}

fn normalize_tag_key(key: &str) -> String {
    key.trim()
        .to_ascii_lowercase()
        .replace([' ', '-'], "_")
}

fn first_meaningful_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('<'))
        .map(one_line)
}

fn deserialize_observations<'de, D>(deserializer: D) -> std::result::Result<Vec<ReviewObservation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<Value>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|value| match value {
            Value::String(text) => Ok(ReviewObservation {
                confidence: Some("low".into()),
                category: Some("Observation".into()),
                path: None,
                line: None,
                title: Some(one_line(&text)),
                evidence: Some(text),
                project_context: None,
                impact: None,
                next_step: None,
            }),
            Value::Object(object) => {
                if object.contains_key("content")
                    || object.contains_key("severity")
                    || object.contains_key("type")
                {
                    Ok(legacy_observation_object(&object))
                } else {
                    serde_json::from_value(Value::Object(object)).map_err(serde::de::Error::custom)
                }
            },
            other => Err(serde::de::Error::custom(format!(
                "observation must be object or string, got {other}"
            ))),
        })
        .collect()
}

fn legacy_observation_object(object: &serde_json::Map<String, Value>) -> ReviewObservation {
    let text_field = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let severity = text_field("severity");
    let confidence = severity.as_deref().map(|value| match value {
        "high" => "high",
        "medium" => "medium",
        _ => "low",
    });
    let content = text_field("content").or_else(|| text_field("message"));
    ReviewObservation {
        confidence: confidence.map(str::to_owned).or_else(|| Some("low".into())),
        category: text_field("category")
            .or_else(|| text_field("type"))
            .or_else(|| Some("Observation".into())),
        path: text_field("path"),
        line: object.get("line").and_then(Value::as_u64),
        title: text_field("title")
            .or_else(|| text_field("id"))
            .or_else(|| content.as_ref().map(|text| one_line(text))),
        evidence: content,
        project_context: text_field("project_context"),
        impact: text_field("impact"),
        next_step: text_field("next_step"),
    }
}

fn finding_fingerprint(finding: &ValidatedFinding) -> String {
    normalize_fingerprint_parts(&[
        &finding.priority,
        finding.kind.as_str(),
        &finding.confidence,
        &finding.path,
        finding.side.as_github(),
        &finding.line.to_string(),
        &finding.title,
        &finding.issue,
    ])
}

fn normalize_fingerprint_parts(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|part| {
            part.to_ascii_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn extract_json_object(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let without_fence = if trimmed.starts_with("```") {
        let after_first_line = trimmed.split_once('\n')?.1;
        after_first_line
            .rsplit_once("```")
            .map(|(body, _)| body.trim())
            .unwrap_or(after_first_line.trim())
    } else {
        trimmed
    };
    let start = without_fence.find('{')?;
    let end = without_fence.rfind('}')?;
    (start <= end).then_some(&without_fence[start..=end])
}

fn json_repair_prompt(previous: &str, error: &anyhow::Error) -> String {
    format!(
        r#"{AGENT_LINE}

Your previous response could not be parsed as the required PR review JSON.

Parse error:
```text
{error:#}
```

Return only a valid JSON object with this shape:
```json
{{
  "files_reviewed": ["path/from/diff.rs"],
  "confirmed_findings": [
    {{
      "severity": "P1",
      "confidence": "high",
      "category": "Correctness",
      "path": "path/from/diff.rs",
      "side": "RIGHT",
      "line": 123,
      "title": "Short title",
      "issue": "Concrete issue proven by the PR diff.",
      "evidence": "Diff line and project context inspected.",
      "project_context": "Why this matters in this repository.",
      "impact": "Specific impact.",
      "fix": "Concrete fix."
    }}
  ],
  "advisory_findings": [],
  "observations": [],
  "investigation_log": [],
  "residual_risk": []
}}
```

Previous response:
```text
{previous}
```
"#,
        AGENT_LINE = AGENT_LINE,
        error = error,
        previous = previous.trim(),
    )
}

async fn post_final_structured_report(
    config: &Config,
    run_info: &RunInfo,
    trigger: &ReviewTrigger,
    session_id: &str,
    validated: &ValidatedReview,
    published: &mut PublishedReview,
) -> Result<()> {
    let report =
        match final_comment_report_pass(config, run_info, session_id, trigger, validated, published)
            .await
        {
            Ok(report) => report,
            Err(error) => fallback_final_report(validated, published, Some(&format!("{error:#}"))),
        };
    let body = final_review_comment_body(config, trigger, session_id, published, &report);
    write_debug_artifact(
        validated.debug_dir.as_deref(),
        "final-comment-body.md",
        &body,
    );
    let url = post_issue_comment(&trigger.repo, trigger.pr.number, &body)?;
    published.url = url.or_else(|| published.inline_review_url.clone());
    published.summary_body = body;
    Ok(())
}

async fn final_comment_report_pass(
    config: &Config,
    run_info: &RunInfo,
    session_id: &str,
    trigger: &ReviewTrigger,
    validated: &ValidatedReview,
    published: &PublishedReview,
) -> Result<String> {
    let prompt = final_comment_report_prompt(trigger, validated, published);
    write_debug_artifact(
        validated.debug_dir.as_deref(),
        "final-report-prompt.md",
        &prompt,
    );
    let response = submit_prompt_and_wait(
        run_info,
        session_id,
        &prompt,
        Duration::from_secs(config.review_timeout_seconds),
    )
    .await?;
    write_debug_artifact(
        validated.debug_dir.as_deref(),
        "final-report-response.txt",
        &response,
    );
    let candidate = extract_json_object(&response).context("final report response missing JSON")?;
    let parsed: FinalCommentOutput =
        serde_json::from_str(candidate).context("parse final report JSON")?;
    let report = sanitize_final_report(&parsed.report);
    if report.is_empty() {
        anyhow::bail!("final report was empty");
    }
    Ok(report)
}

fn final_comment_report_prompt(
    trigger: &ReviewTrigger,
    validated: &ValidatedReview,
    published: &PublishedReview,
) -> String {
    let inline = validated
        .inline_findings
        .iter()
        .take(published.inline_comments_posted)
        .map(|finding| {
            format!(
                "- [{}][{}][{}] {} `{}`:{} — {}\n  Evidence: {}\n  Issue: {}\n  Project context: {}\n  Impact: {}\n  Fix: {}",
                finding.priority,
                finding.kind.as_str(),
                finding.confidence,
                finding.category,
                finding.path,
                finding.line,
                finding.title,
                one_line(&finding.evidence),
                one_line(&finding.issue),
                one_line(&finding.project_context),
                one_line(&finding.impact),
                one_line(&finding.fix),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let summary_only = validated
        .summary_findings
        .iter()
        .take(12)
        .map(|finding| {
            format!(
                "- [{}][{}][{}] {} `{}`:{} — {}\n  Evidence: {}\n  Project context: {}\n  Impact: {}\n  Fix: {}",
                finding.priority,
                finding.kind.as_str(),
                finding.confidence,
                finding.category,
                finding.path,
                finding.line,
                finding.title,
                one_line(&finding.evidence),
                one_line(&finding.project_context),
                one_line(&finding.impact),
                one_line(&finding.fix),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let unplaced = validated
        .unplaced_findings
        .iter()
        .take(8)
        .map(|finding| {
            format!(
                "- [{}][{}][{}] {}: {}",
                finding.priority, finding.kind, finding.confidence, finding.title, finding.reason
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let observations = validated
        .observations
        .iter()
        .take(10)
        .map(|observation| {
            format!(
                "- [{}][{}] {}{}{}\n  Evidence: {}\n  Project context: {}\n  Impact: {}\n  Next step: {}",
                observation
                    .confidence
                    .as_deref()
                    .and_then(normalize_confidence)
                    .unwrap_or_else(|| "low".into()),
                observation.category.as_deref().unwrap_or("Observation"),
                observation.title.as_deref().unwrap_or("Untitled observation"),
                observation
                    .path
                    .as_deref()
                    .map(|path| format!(" `{path}`"))
                    .unwrap_or_default(),
                observation
                    .line
                    .map(|line| format!(":{line}"))
                    .unwrap_or_default(),
                one_line(observation.evidence.as_deref().unwrap_or("")),
                one_line(observation.project_context.as_deref().unwrap_or("")),
                one_line(observation.impact.as_deref().unwrap_or("")),
                one_line(observation.next_step.as_deref().unwrap_or("")),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let investigation = if validated.investigation_log.is_empty() {
        "None".into()
    } else {
        validated
            .investigation_log
            .iter()
            .take(12)
            .map(|item| format!("- {}", one_line(item)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let coverage = validated
        .coverage
        .as_ref()
        .map(ReviewCoverage::summary_lines)
        .unwrap_or_else(|| "coverage unavailable".into());
    let verification = verification_summary(&validated.verification);
    let residual = if validated.residual_risk.is_empty() {
        "None".into()
    } else {
        validated
            .residual_risk
            .iter()
            .map(|risk| format!("- {risk}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let changed_files = changed_file_summary(&trigger.pr);
    let inline_reference = match (&published.inline_review_id, &published.inline_review_url) {
        (Some(id), Some(url)) => format!("GitHub review ID {id}, URL {url}"),
        (Some(id), None) => format!("GitHub review ID {id}"),
        (None, Some(url)) => format!("GitHub review URL {url}"),
        (None, None) => "No inline review object was created".into(),
    };
    format!(
        r#"{AGENT_LINE}

Write the final GitHub PR review report for {repo} PR #{pr_number}: {title}

Rules:
- Return strict JSON only: {{"report":"...markdown..."}}
- Write a complete Chinese Markdown report in the old reviewnow style: concrete scope, verification, findings, merge assessment, low-confidence observations, what was done, next steps, and residual risk.
- Do not include the HTML marker, agent line, review session, trigger, or head SHA; the plugin adds those.
- Use the four unchanged review angles only: Correctness, Security, Reliability/Performance, Tests/API Contract.
- Be concrete and grounded in the data below. Do not invent code facts, commands, line numbers, or risks.
- If inline findings exist, include a `## 发现` section and summarize each issue with priority, file/line, angle, issue, impact, and fix.
- If there are summary-only P1/P2 findings, include them under `## 发现` too, clearly marking why they were not inline.
- If there are P3 summary-only findings or observations, include them under `## 设计提醒` or `## 低置信度观察` instead of pretending nothing was found.
- If there are no inline findings, explain whether the run found summary-only advisory/observations or truly found no useful risk.
- Keep `## 验证` useful: mention failed/skipped checks or meaningful validation, not a long list of "passed" boilerplate.
- Include `## 合并评估`, `## 做了什么`, `## 下一步建议`, and `## 剩余风险`.
- Low-confidence observations are allowed only when clearly labelled and useful.
- Do not tell the user to run GitHub API commands. Inline comments have already been handled by the plugin.

Review result:
- Coverage: {coverage}
- Inline comments posted: {inline_count}
- Inline review reference: {inline_reference}
- Summary-only findings: {summary_only_count}
- Observations: {observation_count}
- Unplaced findings: {unplaced_count}
- Highest risk: {highest_risk}

Changed files:
```text
{changed_files}
```

Inline findings that were actually posted:
```text
{inline}
```

Summary-only findings:
```text
{summary_only}
```

Unplaced finding notes:
```text
{unplaced}
```

Observations:
```text
{observations}
```

Investigation log:
```text
{investigation}
```

Noteworthy deterministic checks:
```text
{verification}
```

Residual risk:
```text
{residual}
```
"#,
        AGENT_LINE = AGENT_LINE,
        repo = trigger.repo,
        pr_number = trigger.pr.number,
        title = trigger.pr.title,
        coverage = coverage,
        inline_count = published.inline_comments_posted,
        inline_reference = inline_reference,
        summary_only_count = validated.summary_findings.len(),
        observation_count = validated.observations.len(),
        unplaced_count = published.unplaced_findings_count,
        highest_risk = published.highest_risk.as_deref().unwrap_or("None"),
        changed_files = changed_files,
        inline = if inline.trim().is_empty() { "None" } else { &inline },
        summary_only = if summary_only.trim().is_empty() {
            "None"
        } else {
            &summary_only
        },
        unplaced = if unplaced.trim().is_empty() {
            "None"
        } else {
            &unplaced
        },
        observations = if observations.trim().is_empty() {
            "None"
        } else {
            &observations
        },
        investigation = investigation,
        verification = verification,
        residual = residual,
    )
}

fn sanitize_final_report(report: &str) -> String {
    let mut text = report.trim().to_owned();
    let outer_fence = text
        .strip_prefix("```markdown")
        .or_else(|| text.strip_prefix("```md"))
        .or_else(|| text.strip_prefix("```"))
        .map(str::to_owned);
    if let Some(stripped) = outer_fence
    {
        text = stripped.trim().to_owned();
        if let Some(stripped) = text.strip_suffix("```") {
            text = stripped.trim().to_owned();
        }
    }
    let filtered = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("<!-- astrcode-auto-review")
                && trimmed != AGENT_LINE
                && !trimmed.starts_with("Review session:")
                && !trimmed.starts_with("Trigger:")
                && !trimmed.starts_with("Trigger comment:")
                && !trimmed.starts_with("Head SHA:")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    let mut text = filtered;
    if text.chars().count() > 14_000 {
        text = text.chars().take(14_000).collect::<String>();
        text.push_str("\n\n...（最终报告过长，已截断）");
    }
    text
}

fn final_review_comment_body(
    config: &Config,
    trigger: &ReviewTrigger,
    session_id: &str,
    published: &PublishedReview,
    report: &str,
) -> String {
    let trigger_line = match trigger.comment() {
        Some(comment) => format!("Trigger comment: `{}`", comment.id),
        None => "Trigger: new PR auto review".into(),
    };
    let inline_line = if published.inline_comments_posted > 0 {
        match (published.inline_review_id, published.inline_review_url.as_deref()) {
            (Some(id), Some(url)) => {
                format!("内联评论已发布 (ID {id}，[查看 review]({url}))。以下是我的总结。")
            },
            (Some(id), None) => format!("内联评论已发布 (ID {id})。以下是我的总结。"),
            (None, Some(url)) => format!("内联评论已发布（[查看 review]({url})）。以下是我的总结。"),
            (None, None) => "内联评论已发布。以下是我的总结。".into(),
        }
    } else if published.unplaced_findings_count > 0 {
        "没有成功发布 inline 评论；相关发现已放入总结。以下是我的总结。".into()
    } else {
        "未发现需要发布的 inline 评论。以下是我的总结。".into()
    };
    format!(
        r#"{marker}
{AGENT_LINE}

Review session: `{session_id}`
{trigger_line}
Head SHA: `{sha}`

{inline_line}

---

{report}
"#,
        marker = config.comment_marker,
        AGENT_LINE = AGENT_LINE,
        session_id = session_id,
        trigger_line = trigger_line,
        sha = trigger.pr.head_ref_oid,
        inline_line = inline_line,
        report = report.trim(),
    )
}

fn fallback_final_report(
    validated: &ValidatedReview,
    published: &PublishedReview,
    generation_error: Option<&str>,
) -> String {
    let posted_findings = validated
        .inline_findings
        .iter()
        .take(published.inline_comments_posted)
        .map(|finding| {
            format!(
                "### [{}][{}][{}] {}\n- **文件:** `{}`:{}\n- **视角:** {}\n- **问题:** {}\n- **证据:** {}\n- **项目上下文:** {}\n- **影响:** {}\n- **修复:** {}",
                finding.priority,
                finding.kind.as_str(),
                finding.confidence,
                finding.title,
                finding.path,
                finding.line,
                finding.category,
                one_line(&finding.issue),
                one_line(&finding.evidence),
                one_line(&finding.project_context),
                one_line(&finding.impact),
                one_line(&finding.fix),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let summary_findings = validated
        .summary_findings
        .iter()
        .take(10)
        .map(|finding| {
            format!(
                "- [{}][{}][{}] `{}`:{} {}：{} 建议：{}",
                finding.priority,
                finding.kind.as_str(),
                finding.confidence,
                finding.path,
                finding.line,
                finding.title,
                one_line(&finding.issue),
                one_line(&finding.fix),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let unplaced = validated
        .unplaced_findings
        .iter()
        .take(10)
        .map(|finding| {
            let location = match (&finding.path, &finding.side, finding.line) {
                (Some(path), Some(side), Some(line)) => format!(" `{path}` {side} {line}"),
                (Some(path), _, _) => format!(" `{path}`"),
                _ => String::new(),
            };
            format!(
                "- [{}][{}][{}]{} {}：{}",
                finding.priority,
                finding.kind,
                finding.confidence,
                location,
                finding.title,
                finding.reason
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let observations = validated
        .observations
        .iter()
        .take(10)
        .map(|observation| {
            format!(
                "- [{}][{}] {}{}{}：{} 下一步：{}",
                observation.confidence.as_deref().unwrap_or("low"),
                observation.category.as_deref().unwrap_or("Observation"),
                observation.title.as_deref().unwrap_or("未命名观察"),
                observation
                    .path
                    .as_deref()
                    .map(|path| format!(" `{path}`"))
                    .unwrap_or_default(),
                observation
                    .line
                    .map(|line| format!(":{line}"))
                    .unwrap_or_default(),
                one_line(observation.impact.as_deref().unwrap_or("")),
                one_line(observation.next_step.as_deref().unwrap_or("")),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let findings = if posted_findings.trim().is_empty() {
        "未发布 inline 确认问题。".into()
    } else {
        posted_findings
    };
    let design_reminders = [summary_findings.as_str(), unplaced.as_str()]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let design_reminders = if design_reminders.trim().is_empty() {
        "- 无 summary-only 或无法定位的发现。".into()
    } else {
        design_reminders
    };
    let observations = if observations.trim().is_empty() {
        "- 无低置信度观察。".into()
    } else {
        observations
    };
    let coverage = coverage_summary_for_comment(validated.coverage.as_ref());
    let verification = verification_summary(&validated.verification);
    let residual = if validated.residual_risk.is_empty() {
        "- 无额外剩余风险。".into()
    } else {
        validated
            .residual_risk
            .iter()
            .map(|risk| format!("- {risk}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let generation_note = generation_error
        .map(|error| format!("\n\n> 最终报告生成降级：{}", one_line(error)))
        .unwrap_or_default();
    format!(
        r#"# 代码审查

## 范围

本次 review 覆盖了当前 PR diff，并按 Correctness、Security、Reliability/Performance、Tests/API Contract 四个角度检查。

## 验证

{verification}

## 发现

{findings}

## 设计提醒

{design_reminders}

## 低置信度观察

{observations}

## 合并评估

总体：{assessment}

## 做了什么

完成 coverage-first 审查并发布 {inline_count} 条 inline comment；未能 inline 的发现数为 {unplaced_count}。

## 下一步建议

1. 优先处理 inline comment 中的 P0/P1/P2 问题。
2. 如果需要风格或 nitpick 级别 review，可以评论 `@whatevertogo nitpick review` 重新触发。

## 剩余风险

{residual}

## Coverage

{coverage}{generation_note}"#,
        verification = verification,
        findings = findings,
        design_reminders = design_reminders,
        observations = observations,
        assessment = if published.inline_comments_posted == 0 {
            "未发现确认的阻塞问题"
        } else {
            "需要处理已发布的 inline findings"
        },
        inline_count = published.inline_comments_posted,
        unplaced_count = published.unplaced_findings_count,
        residual = residual,
        coverage = coverage,
        generation_note = generation_note,
    )
}

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
            },
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
        residual_risk.push("Review context was truncated by the plugin byte cap.".into());
    }
    if !context.non_commentable_files.is_empty() {
        residual_risk.push(format!(
            "Some files had no GitHub patch and cannot receive inline comments: {}",
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
    let path = required_field(&finding.path, "path", finding)?;
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
    let key = CommentLineKey {
        path: path.clone(),
        side,
        line,
    };
    let (side, line) = if context.commentable_lines.contains(&key) {
        (side, line)
    } else if let Some(fallback) = nearest_commentable_line(context, &path, line) {
        (fallback.side, fallback.line)
    } else {
        return Err(Box::new(unplaced_from_raw(
            finding,
            format!(
                "{} {} is not a commentable PR diff line",
                side.as_github(),
                line
            ),
        )));
    };
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

fn publish_structured_review(
    config: &Config,
    trigger: &ReviewTrigger,
    session_id: &str,
    validated: &ValidatedReview,
) -> Result<PublishedReview> {
    let mut fallback_unplaced = validated.unplaced_findings.clone();
    let mut inline_findings = validated.inline_findings.clone();
    apply_severity_gate(config, trigger, &mut inline_findings, &mut fallback_unplaced);
    let mut inline_comments_posted = inline_findings.len();
    let mut posted_findings = Vec::new();
    let mut inline_review_url = None;
    let mut inline_review_id = None;
    let mut publish_error = None;
    let highest_risk = inline_findings
        .first()
        .map(|finding| format!("{} {}", finding.priority, finding.title))
        .or_else(|| {
            fallback_unplaced
                .first()
                .map(|finding| format!("{} {}", finding.priority, finding.title))
        });

    if !inline_findings.is_empty() {
        let review_body = inline_review_batch_body(
            config,
            trigger,
            session_id,
            inline_comments_posted,
            fallback_unplaced.len(),
            highest_risk.as_deref(),
        );
        match post_pull_review(config, trigger, &review_body, &inline_findings) {
            Ok(review) => {
                inline_review_url = review.url;
                inline_review_id = review.id;
                posted_findings = inline_findings
                    .iter()
                    .map(|finding| finding_memory_from_validated(finding, &trigger.pr.head_ref_oid))
                    .collect();
            },
            Err(error) => {
                let error_text = format!("GitHub rejected inline review payload: {error:#}");
                publish_error = Some(error_text.clone());
                fallback_unplaced.extend(inline_findings.drain(..).map(|finding| {
                    unplaced_from_validated(finding, error_text.clone())
                }));
                inline_comments_posted = 0;
            },
        }
    }

    let summary_validated = ValidatedReview {
        inline_findings: inline_findings.clone(),
        summary_findings: validated.summary_findings.clone(),
        unplaced_findings: fallback_unplaced.clone(),
        observations: validated.observations.clone(),
        investigation_log: validated.investigation_log.clone(),
        verification: validated.verification.clone(),
        residual_risk: validated.residual_risk.clone(),
        summary: validated.summary.clone(),
        coverage: validated.coverage.clone(),
        debug_dir: validated.debug_dir.clone(),
    };
    let summary_context = StructuredReviewSummary {
        config,
        trigger,
        session_id,
        validated: &summary_validated,
        inline_comments_posted,
        unplaced_count: fallback_unplaced.len(),
        highest_risk: highest_risk.as_deref(),
        publish_error: publish_error.as_deref(),
    };
    let summary_body = structured_review_summary_body(&summary_context);
    write_debug_artifact(
        validated.debug_dir.as_deref(),
        "publish-result.json",
        &serde_json::to_string_pretty(&json!({
            "inline_review_url": inline_review_url.as_deref(),
            "inline_review_id": inline_review_id,
            "inline_comments_posted": inline_comments_posted,
            "unplaced_findings_count": fallback_unplaced.len(),
            "highest_risk": highest_risk.as_deref(),
            "publish_error": publish_error.as_deref(),
            "posted_findings": &posted_findings,
        }))
        .unwrap_or_default(),
    );

    Ok(PublishedReview {
        url: inline_review_url.clone(),
        inline_review_url,
        inline_review_id,
        summary_body,
        inline_comments_posted,
        unplaced_findings_count: fallback_unplaced.len(),
        highest_risk,
        verification: validated.verification.clone(),
        posted_findings,
    })
}

fn suppress_repeated_findings(
    state: &State,
    trigger: &ReviewTrigger,
    validated: &mut ValidatedReview,
) {
    let key = pr_key(&trigger.repo, trigger.pr.number);
    let Some(memory) = state.pr_review_memory.get(&key) else {
        return;
    };
    let mut kept = Vec::new();
    for finding in validated.inline_findings.drain(..) {
        let fingerprint = finding_fingerprint(&finding);
        match memory.posted_findings.get(&fingerprint) {
            Some(previous) if previous.status != "superseded" && previous.status != "resolved" => {
                validated.unplaced_findings.push(unplaced_from_validated(
                    finding,
                    "already posted for this PR in a previous review run".into(),
                ));
            },
            _ => kept.push(finding),
        }
    }
    validated.inline_findings = kept;
}

fn finding_memory_from_validated(finding: &ValidatedFinding, head_sha: &str) -> FindingMemory {
    finding_memory_from_validated_with_status(finding, head_sha, "posted")
}

fn finding_memory_from_validated_with_status(
    finding: &ValidatedFinding,
    head_sha: &str,
    status: &str,
) -> FindingMemory {
    FindingMemory {
        fingerprint: finding_fingerprint(finding),
        priority: finding.priority.clone(),
        kind: finding.kind.as_str().into(),
        confidence: finding.confidence.clone(),
        title: finding.title.clone(),
        path: Some(finding.path.clone()),
        line: Some(finding.line),
        head_sha: head_sha.to_owned(),
        status: status.into(),
        posted_at: now_epoch(),
    }
}

fn apply_severity_gate(
    config: &Config,
    trigger: &ReviewTrigger,
    inline_findings: &mut Vec<ValidatedFinding>,
    fallback_unplaced: &mut Vec<UnplacedFinding>,
) {
    let default_max = normalize_priority(&config.inline_priority_max).unwrap_or_else(|| "P2".into());
    let nitpick_requested = trigger_instruction(trigger)
        .to_ascii_lowercase()
        .contains("nitpick")
        || trigger_instruction(trigger).contains("细节")
        || trigger_instruction(trigger)
            .to_ascii_lowercase()
            .contains("style review");
    let nitpick_max =
        normalize_priority(&config.nitpick_inline_priority_max).unwrap_or_else(|| "P3".into());
    let max_priority = if nitpick_requested {
        nitpick_max
    } else {
        default_max
    };
    let max_rank = priority_rank(&max_priority);
    let mut kept = Vec::new();
    let mut p3_inlined = 0usize;
    let mut advisory_inlined = 0usize;
    for finding in inline_findings.drain(..) {
        let rank = priority_rank(&finding.priority);
        let is_p3 = rank >= priority_rank("P3");
        let p3_limit = if nitpick_requested {
            config.max_nitpick_inline_comments
        } else {
            config.max_p3_inline_comments
        };
        let priority_allowed = match finding.kind {
            FindingKind::Confirmed => rank <= max_rank || (nitpick_requested && rank <= priority_rank("P3")),
            FindingKind::Advisory => rank <= priority_rank("P3"),
        };
        let advisory_allowed = finding.kind != FindingKind::Advisory
            || advisory_inlined < config.max_advisory_inline_comments;
        let p3_allowed = !is_p3 || p3_inlined < p3_limit;
        if priority_allowed && advisory_allowed && p3_allowed {
            if is_p3 {
                p3_inlined += 1;
            }
            if finding.kind == FindingKind::Advisory {
                advisory_inlined += 1;
            }
            kept.push(finding);
        } else {
            let reason = if !priority_allowed {
                format!("severity gate kept confirmed inline comments at {max_priority} or higher")
            } else if !advisory_allowed {
                format!(
                    "advisory inline limit max_advisory_inline_comments={} was reached",
                    config.max_advisory_inline_comments
                )
            } else {
                format!("P3 inline limit {p3_limit} was reached")
            };
            fallback_unplaced.push(unplaced_from_validated(finding, reason));
        }
    }
    *inline_findings = kept;
}

fn inline_review_batch_body(
    config: &Config,
    trigger: &ReviewTrigger,
    session_id: &str,
    inline_comments_posted: usize,
    unplaced_count: usize,
    highest_risk: Option<&str>,
) -> String {
    let trigger_line = match trigger.comment() {
        Some(comment) => format!("Trigger comment: `{}`", comment.id),
        None => "Trigger: new PR auto review".into(),
    };
    format!(
        r#"{marker}
{AGENT_LINE}

Review session: `{session_id}`
{trigger_line}
Head SHA: `{sha}`

已发布 {inline_comments_posted} 条 inline review comment。最终总结将作为单独评论发布。

- Highest risk: {highest_risk}
- Unplaced findings so far: {unplaced_count}
"#,
        marker = config.comment_marker,
        AGENT_LINE = AGENT_LINE,
        session_id = session_id,
        trigger_line = trigger_line,
        sha = trigger.pr.head_ref_oid,
        inline_comments_posted = inline_comments_posted,
        highest_risk = highest_risk.unwrap_or("None"),
        unplaced_count = unplaced_count,
    )
}

fn post_pull_review(
    config: &Config,
    trigger: &ReviewTrigger,
    body: &str,
    findings: &[ValidatedFinding],
) -> Result<PostedPullReview> {
    let payload = pull_review_payload(config, trigger, body, findings);
    let mut file = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(file.as_file_mut(), &payload)?;
    file.as_file_mut().flush()?;
    let out: Value = gh_json_with_input(
        &[
            "api",
            "--method",
            "POST",
            &format!("repos/{}/pulls/{}/reviews", trigger.repo, trigger.pr.number),
        ],
        file.path(),
    )?;
    Ok(PostedPullReview {
        url: out
            .get("html_url")
            .and_then(Value::as_str)
            .or_else(|| {
                out.get("_links")?
                    .get("html")?
                    .get("href")
                    .and_then(Value::as_str)
            })
            .map(ToOwned::to_owned),
        id: out.get("id").and_then(Value::as_u64),
    })
}

fn pull_review_payload(
    config: &Config,
    trigger: &ReviewTrigger,
    body: &str,
    findings: &[ValidatedFinding],
) -> Value {
    let comments = findings
        .iter()
        .map(|finding| {
            json!({
                "path": finding.path,
                "line": finding.line,
                "side": finding.side.as_github(),
                "body": inline_review_comment_body(config, finding),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "commit_id": trigger.pr.head_ref_oid,
        "event": "COMMENT",
        "body": body,
        "comments": comments,
    })
}

fn inline_review_comment_body(config: &Config, finding: &ValidatedFinding) -> String {
    format!(
        r#"{marker}
{AGENT_LINE}

[{priority}][{kind}][{confidence} confidence] {title}

Category: {category}
Evidence: {evidence}
Issue: {issue}
Project context: {project_context}
Impact: {impact}
Fix: {fix}
"#,
        marker = config.comment_marker,
        AGENT_LINE = AGENT_LINE,
        priority = finding.priority.as_str(),
        kind = finding.kind.as_str(),
        confidence = finding.confidence.as_str(),
        title = finding.title.as_str(),
        category = finding.category.as_str(),
        evidence = finding.evidence.as_str(),
        issue = finding.issue.as_str(),
        project_context = finding.project_context.as_str(),
        impact = finding.impact.as_str(),
        fix = finding.fix.as_str(),
    )
}

struct StructuredReviewSummary<'a> {
    config: &'a Config,
    trigger: &'a ReviewTrigger,
    session_id: &'a str,
    validated: &'a ValidatedReview,
    inline_comments_posted: usize,
    unplaced_count: usize,
    highest_risk: Option<&'a str>,
    publish_error: Option<&'a str>,
}

fn structured_review_summary_body(summary: &StructuredReviewSummary<'_>) -> String {
    let trigger_line = match summary.trigger.comment() {
        Some(comment) => format!("Trigger comment: `{}`", comment.id),
        None => "Trigger: new PR auto review".into(),
    };
    let unplaced = if summary.validated.unplaced_findings.is_empty() {
        "None".into()
    } else {
        summary
            .validated
            .unplaced_findings
            .iter()
            .take(8)
            .map(|finding| {
                let location = match (&finding.path, &finding.side, finding.line) {
                    (Some(path), Some(side), Some(line)) => format!(" `{path}` {side} {line}"),
                    (Some(path), _, _) => format!(" `{path}`"),
                    _ => String::new(),
                };
                format!(
                    "- [{}]{} {}: {}",
                    finding.priority, location, finding.title, finding.reason
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let verification = verification_summary(&summary.validated.verification);
    let mut residual = summary
        .validated
        .residual_risk
        .iter()
        .map(|risk| format!("- {risk}"))
        .collect::<Vec<_>>();
    if let Some(error) = summary.publish_error {
        residual.push(format!("- {error}"));
    }
    let residual = if residual.is_empty() {
        "- None".into()
    } else {
        residual.join("\n")
    };
    let body_summary = summary
        .validated
        .summary
        .as_deref()
        .filter(|body_summary| !body_summary.trim().is_empty())
        .unwrap_or("Structured PR review completed.");
    let coverage_summary = coverage_summary_for_comment(summary.validated.coverage.as_ref());
    format!(
        r#"{marker}
{AGENT_LINE}

Review session: `{session_id}`
{trigger_line}
Head SHA: `{sha}`

{summary}

## Findings
- Inline comments posted: {inline_comments_posted}
- Highest risk: {highest_risk}
- Unplaced findings: {unplaced_count}

## Coverage
{coverage_summary}

## Unplaced Findings
{unplaced}

## Verification
{verification}

## Residual Risk
{residual}
"#,
        marker = summary.config.comment_marker,
        AGENT_LINE = AGENT_LINE,
        session_id = summary.session_id,
        trigger_line = trigger_line,
        sha = summary.trigger.pr.head_ref_oid,
        summary = body_summary,
        inline_comments_posted = summary.inline_comments_posted,
        highest_risk = summary.highest_risk.unwrap_or("None"),
        unplaced_count = summary.unplaced_count,
        coverage_summary = coverage_summary,
        unplaced = unplaced,
        verification = verification,
        residual = residual,
    )
}

fn verification_summary(items: &[VerificationItem]) -> String {
    if items.is_empty() {
        return "- Plugin-collected GitHub PR context: passed".into();
    }
    let passed = items
        .iter()
        .filter(|item| item.status.as_deref() == Some("passed"))
        .count();
    let noteworthy = items
        .iter()
        .filter(|item| item.status.as_deref() != Some("passed"))
        .map(|item| {
            format!(
                "- `{}`: {} ({})",
                item.command.as_deref().unwrap_or("unspecified"),
                item.status.as_deref().unwrap_or("unknown"),
                item.notes.as_deref().unwrap_or("no notes")
            )
        })
        .collect::<Vec<_>>();
    if noteworthy.is_empty() {
        format!("- {passed} deterministic check(s) passed")
    } else if passed == 0 {
        noteworthy.join("\n")
    } else {
        format!(
            "- {passed} deterministic check(s) passed\n{}",
            noteworthy.join("\n")
        )
    }
}

fn coverage_summary_for_comment(coverage: Option<&ReviewCoverage>) -> String {
    let Some(coverage) = coverage else {
        return "- Coverage details unavailable".into();
    };
    let incomplete = coverage.incomplete_entries();
    if incomplete.is_empty() {
        return format!(
            "- Files reviewed: {}/{}",
            coverage.reviewed_count(),
            coverage.total_count()
        );
    }
    let details = incomplete
        .into_iter()
        .take(12)
        .map(|entry| {
            format!(
                "- `{}`: {} ({})",
                entry.path,
                entry.status.as_str(),
                entry.reason
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "- Files reviewed: {}/{}\n{}",
        coverage.reviewed_count(),
        coverage.total_count(),
        details
    )
}

async fn create_session(run_info: &RunInfo, working_dir: &Path) -> Result<String> {
    let url = format!("http://127.0.0.1:{}/api/sessions", run_info.port);
    let response = curl_json(
        "POST",
        &url,
        &run_info.auth_token,
        Some(&json!({ "workingDir": path_str(working_dir)? })),
    )?;
    response
        .get("sessionId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .context("create session response missing sessionId")
}

fn delete_session(run_info: &RunInfo, session_id: &str) -> Result<()> {
    let url = format!(
        "http://127.0.0.1:{}/api/sessions/{session_id}",
        run_info.port
    );
    let _ = curl_json("DELETE", &url, &run_info.auth_token, None)?;
    Ok(())
}

async fn submit_prompt(run_info: &RunInfo, session_id: &str, prompt: &str) -> Result<()> {
    let url = format!(
        "http://127.0.0.1:{}/api/sessions/{session_id}/prompt",
        run_info.port
    );
    let _ = curl_json(
        "POST",
        &url,
        &run_info.auth_token,
        Some(&json!({ "text": prompt, "attachments": [] })),
    )?;
    Ok(())
}

async fn submit_prompt_and_wait(
    run_info: &RunInfo,
    session_id: &str,
    prompt: &str,
    timeout: Duration,
) -> Result<String> {
    let before = conversation_snapshot(run_info, session_id)
        .await
        .map(|snapshot| assistant_text_count(&snapshot))
        .unwrap_or(0);
    submit_prompt(run_info, session_id, prompt).await?;
    wait_for_review_after_count(run_info, session_id, timeout, before).await
}

async fn wait_for_review_after_count(
    run_info: &RunInfo,
    session_id: &str,
    timeout: Duration,
    previous_assistant_count: usize,
) -> Result<String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_phase = "unknown".to_string();
    let mut idle_without_assistant_since: Option<std::time::Instant> = None;
    while std::time::Instant::now() < deadline {
        let snapshot = conversation_snapshot(run_info, session_id).await?;
        last_phase = snapshot
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        match last_phase.to_ascii_lowercase().as_str() {
            "idle" => {
                if assistant_text_count(&snapshot) > previous_assistant_count {
                    let text = latest_assistant_text(&snapshot)
                        .context("review session completed without assistant text")?;
                    return Ok(text);
                }
                let idle_since =
                    idle_without_assistant_since.get_or_insert_with(std::time::Instant::now);
                if idle_since.elapsed() >= Duration::from_secs(30) {
                    anyhow::bail!(
                        "review session became idle without producing a new assistant response"
                    );
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            },
            "error" => anyhow::bail!("review session entered error phase"),
            _ => {
                idle_without_assistant_since = None;
                tokio::time::sleep(Duration::from_secs(5)).await;
            },
        }
    }
    anyhow::bail!("review session timed out; last phase={last_phase}")
}

async fn conversation_snapshot(run_info: &RunInfo, session_id: &str) -> Result<Value> {
    let url = format!(
        "http://127.0.0.1:{}/api/sessions/{session_id}/conversation",
        run_info.port
    );
    curl_json("GET", &url, &run_info.auth_token, None)
}

fn curl_json(method: &str, url: &str, token: &str, payload: Option<&Value>) -> Result<Value> {
    let mut payload_file = None;
    let mut args = vec![
        "-fsS".to_string(),
        "-X".to_string(),
        method.to_string(),
        "-H".to_string(),
        format!("Authorization: Bearer {token}"),
    ];
    if let Some(payload) = payload {
        let mut file = tempfile::NamedTempFile::new()?;
        serde_json::to_writer(file.as_file_mut(), payload)?;
        file.as_file_mut().flush()?;
        args.push("-H".into());
        args.push("Content-Type: application/json".into());
        args.push("--data-binary".into());
        args.push(format!("@{}", file.path().display()));
        payload_file = Some(file);
    }
    args.push(url.to_string());
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run_command("curl", &refs, None)?;
    drop(payload_file);
    if output.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&output).with_context(|| format!("parse curl JSON response from {url}"))
}

fn latest_assistant_text(snapshot: &Value) -> Option<String> {
    snapshot
        .get("blocks")?
        .as_array()?
        .iter()
        .filter(|block| block.get("kind").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .rfind(|text| !text.trim().is_empty())
        .map(|text| text.trim().to_owned())
}

fn assistant_text_count(snapshot: &Value) -> usize {
    snapshot
        .get("blocks")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("kind").and_then(Value::as_str) == Some("assistant"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .filter(|text| !text.trim().is_empty())
                .count()
        })
        .unwrap_or(0)
}

fn ensure_astrcode_run_info() -> Result<RunInfo> {
    let path = astrcode_dir()?.join("run.json");
    if !path.exists() {
        let _ = run_command("systemctl", &["--user", "start", "astrcode.service"], None);
        std::thread::sleep(Duration::from_secs(2));
    }
    let data = fs::read_to_string(&path)
        .with_context(|| format!("read {}; is astrcode.service running?", path.display()))?;
    serde_json::from_str(&data).with_context(|| format!("parse {}", path.display()))
}

fn gh_json<T: for<'de> Deserialize<'de>>(args: &[&str]) -> Result<T> {
    let output = run_command("gh", args, None)?;
    serde_json::from_str(&output)
        .with_context(|| format!("parse gh output for gh {}", args.join(" ")))
}

fn ensure_gh_authenticated() -> Result<()> {
    run_command_with_timeout("gh", &["auth", "token"], None, poll_command_timeout())
        .map(|_| ())
        .context("gh is not authenticated or not available in PATH")
}

fn gh_json_with_timeout<T: for<'de> Deserialize<'de>>(args: &[&str], timeout: Duration) -> Result<T> {
    let output = run_command_with_timeout("gh", args, None, timeout)?;
    serde_json::from_str(&output)
        .with_context(|| format!("parse gh output for gh {}", args.join(" ")))
}

fn gh_json_with_input<T: for<'de> Deserialize<'de>>(args: &[&str], input: &Path) -> Result<T> {
    let mut full_args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    full_args.push("--input".into());
    full_args.push(path_str(input)?.to_string());
    let refs = full_args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run_command("gh", &refs, None)?;
    serde_json::from_str(&output)
        .with_context(|| format!("parse gh output for gh {}", refs.join(" ")))
}

fn run_command(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<String> {
    run_command_with_timeout(program, args, cwd, command_timeout())
}

fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<String> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    command.env("PATH", format!("{home}/bin:{home}/.local/bin:{path}"));

    let stdout_file = tempfile::NamedTempFile::new()?;
    let stderr_file = tempfile::NamedTempFile::new()?;
    command.stdout(Stdio::from(stdout_file.reopen()?));
    command.stderr(Stdio::from(stderr_file.reopen()?));

    let mut child = command
        .spawn()
        .with_context(|| format!("run {program} {}", args.join(" ")))?;

    let status = match child
        .wait_timeout(timeout)
        .with_context(|| format!("wait for {program} {}", args.join(" ")))?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            anyhow::bail!(
                "{} {} timed out after {}s",
                program,
                args.join(" "),
                timeout.as_secs()
            );
        }
    };
    let stdout = fs::read_to_string(stdout_file.path()).unwrap_or_default();
    let stderr = fs::read_to_string(stderr_file.path()).unwrap_or_default();

    if !status.success() {
        anyhow::bail!(
            "{} {} failed: {}",
            program,
            args.join(" "),
            stderr.trim()
        );
    }
    Ok(stdout.trim().to_owned())
}

fn poll_command_timeout() -> Duration {
    std::env::var("ASTRCODE_PR_REVIEW_AGENT_POLL_COMMAND_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds >= 5)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(12))
}

fn command_timeout() -> Duration {
    std::env::var("ASTRCODE_PR_REVIEW_AGENT_COMMAND_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds >= 10)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(60))
}

fn related_github_context(trigger: &ReviewTrigger) -> Result<String> {
    let terms = related_search_terms(trigger);
    if terms.is_empty() {
        return Ok("No useful search terms for related PR/issue lookup.".into());
    }
    let mut seen = BTreeSet::new();
    let mut lines = Vec::new();
    for term in terms.iter().take(6) {
        for (kind, item) in related_items_for_term(&trigger.repo, term).unwrap_or_default() {
            let url = item
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if url.is_empty() || !seen.insert(url.clone()) {
                continue;
            }
            let number = item.get("number").and_then(Value::as_u64).unwrap_or_default();
            if kind == "PR" && number == trigger.pr.number {
                continue;
            }
            let title = item.get("title").and_then(Value::as_str).unwrap_or("untitled");
            let state = item.get("state").and_then(Value::as_str).unwrap_or("unknown");
            let updated = item
                .get("updatedAt")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            lines.push(format!(
                "- {kind} #{number} `{title}` ({state}, updated {updated}) matched `{term}`: {url}"
            ));
            if lines.len() >= 12 {
                break;
            }
        }
        if lines.len() >= 12 {
            break;
        }
    }
    if lines.is_empty() {
        Ok(format!(
            "No related PRs/issues found for search terms: {}",
            terms.join(", ")
        ))
    } else {
        Ok(lines.join("\n"))
    }
}

fn related_items_for_term(repo: &str, term: &str) -> Result<Vec<(&'static str, Value)>> {
    let mut items = Vec::new();
    let limit = "5";
    let prs: Vec<Value> = gh_json(&[
        "pr",
        "list",
        "--repo",
        repo,
        "--state",
        "all",
        "--search",
        term,
        "--limit",
        limit,
        "--json",
        "number,title,url,state,updatedAt",
    ])
    .unwrap_or_default();
    items.extend(prs.into_iter().map(|item| ("PR", item)));

    let issues: Vec<Value> = gh_json(&[
        "issue",
        "list",
        "--repo",
        repo,
        "--state",
        "all",
        "--search",
        term,
        "--limit",
        limit,
        "--json",
        "number,title,url,state,updatedAt",
    ])
    .unwrap_or_default();
    items.extend(issues.into_iter().map(|item| ("Issue", item)));
    Ok(items)
}

fn related_search_terms(trigger: &ReviewTrigger) -> Vec<String> {
    let mut terms = BTreeSet::new();
    for token in tokenize_search_text(&trigger.pr.title) {
        terms.insert(token);
    }
    if let Some(body) = &trigger.pr.body {
        for token in tokenize_search_text(body).into_iter().take(6) {
            terms.insert(token);
        }
    }
    for file in &trigger.pr.files {
        for part in file
            .path
            .split(['/', '.', '-', '_'])
            .filter(|part| part.len() >= 4)
        {
            if !is_low_value_search_term(part) {
                terms.insert(part.to_owned());
            }
        }
    }
    terms.into_iter().take(12).collect()
}

fn tokenize_search_text(text: &str) -> Vec<String> {
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
        .map(str::trim)
        .filter(|token| token.len() >= 4)
        .filter(|token| !is_low_value_search_term(token))
        .map(ToOwned::to_owned)
        .collect()
}

fn is_low_value_search_term(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "feat"
            | "fix"
            | "docs"
            | "test"
            | "tests"
            | "refactor"
            | "chore"
            | "style"
            | "main"
            | "src"
            | "crates"
            | "package"
            | "readme"
            | "index"
            | "mod"
    )
}

fn relevant_memory(config: &Config, repo: &str, pr_number: u64) -> Result<String> {
    let mut parts = Vec::new();
    let repo_index = repo_memory_index_path(config, repo)?;
    if repo_index.exists() {
        let index = fs::read_to_string(&repo_index)?;
        let recent = index
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        if !recent.trim().is_empty() {
            parts.push(format!("## Recent repository memory\n{recent}"));
        }
    }
    let related_path = repo_related_memory_path(config, repo)?;
    if related_path.exists() {
        let related = fs::read_to_string(&related_path)?;
        let recent = related
            .lines()
            .rev()
            .take(80)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        if !recent.trim().is_empty() {
            parts.push(format!(
                "## Repository PR/Issue relation reminders\n{recent}"
            ));
        }
    }
    let pr_path = pr_memory_path(config, repo, pr_number)?;
    if pr_path.exists() {
        parts.push(format!(
            "## This PR memory\n{}",
            fs::read_to_string(&pr_path)?
        ));
    }
    let runs_path = config.memory_dir_path()?.join("runs.jsonl");
    if runs_path.exists() {
        let lines = fs::read_to_string(&runs_path)?;
        let recent = lines
            .lines()
            .rev()
            .filter_map(|line| serde_json::from_str::<ReviewRecord>(line).ok())
            .filter(|record| record.repo == repo && record.pr_number != pr_number)
            .take(5)
            .map(|record| {
                format!(
                    "- {}#{} `{}`: {}",
                    record.repo, record.pr_number, record.head_sha, record.summary
                )
            })
            .collect::<Vec<_>>();
        if !recent.is_empty() {
            parts.push(format!(
                "## Recent related repo reviews\n{}",
                recent.join("\n")
            ));
        }
    }
    Ok(parts.join("\n\n").trim().to_owned())
}

fn append_memory(config: &Config, record: &ReviewRecord) -> Result<()> {
    let path = pr_memory_path(config, &record.repo, record.pr_number)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(
        file,
        "## {} session `{}` sha `{}`\n\n{}\n",
        record.created_at, record.session_id, record.head_sha, record.summary
    )?;
    append_repo_memory_index(config, record)?;
    append_repo_related_memory(config, record)?;
    Ok(())
}

fn append_repo_memory_index(config: &Config, record: &ReviewRecord) -> Result<()> {
    let path = repo_memory_index_path(config, &record.repo)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(
        file,
        "- {} PR #{} `{}` session `{}`: {}",
        record.created_at, record.pr_number, record.head_sha, record.session_id, record.summary
    )?;
    Ok(())
}

fn append_repo_related_memory(config: &Config, record: &ReviewRecord) -> Result<()> {
    let Some(related_context) = record.related_context.as_deref() else {
        return Ok(());
    };
    if related_context.trim().is_empty() {
        return Ok(());
    }
    let path = repo_related_memory_path(config, &record.repo)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(
        file,
        "## {} PR #{} `{}` session `{}`\n\nRelated PR/Issue reminders:\n{}\n\nReview memory summary:\n{}\n",
        record.created_at,
        record.pr_number,
        record.head_sha,
        record.session_id,
        related_context.trim(),
        record.summary
    )?;
    Ok(())
}

fn append_run_log(config: &Config, record: &ReviewRecord) -> Result<()> {
    let path = config.memory_dir_path()?.join("runs.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    serde_json::to_writer(&mut file, record)?;
    writeln!(file)?;
    Ok(())
}

fn summarize_review(
    trigger: &ReviewTrigger,
    session_id: &str,
    published: &PublishedReview,
) -> String {
    let finding_summary = if published.inline_comments_posted > 0
        || published.unplaced_findings_count > 0
        || published.highest_risk.is_some()
    {
        format!(
            "Inline comments posted: {}; unplaced findings: {}; highest risk: {}",
            published.inline_comments_posted,
            published.unplaced_findings_count,
            published.highest_risk.as_deref().unwrap_or("None")
        )
    } else {
        finding_summary(&published.summary_body)
    };
    let comment_url = published.url.as_deref().unwrap_or("unknown comment URL");
    let trigger_description = match trigger.comment() {
        Some(comment) => format!(
            "{} 的评论 {}",
            comment
                .user
                .as_ref()
                .map(|user| user.login.as_str())
                .unwrap_or("unknown"),
            comment.html_url.as_deref().unwrap_or("unknown comment URL")
        ),
        None => "新 PR 首次发现".into(),
    };
    format!(
        "本次审查由 {} 触发，针对 {}#{} 的 head `{}` 创建了 Astrcode session \
         `{}`。审查结论：{}。自动化已发布 GitHub review 评论：{}。后续同 PR \
         审查应优先确认这些发现是否已被修复，并避免重复报告已关闭的问题。",
        trigger_description,
        trigger.repo,
        trigger.pr.number,
        trigger.pr.head_ref_oid,
        session_id,
        finding_summary,
        comment_url,
    )
}

fn finding_summary(review: &str) -> String {
    let lower = review.to_ascii_lowercase();
    if lower.contains("no issues") || review.contains("未发现") || review.contains("没有发现")
    {
        return "未发现明确阻断问题".into();
    }
    let candidates = review
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            line.contains("[P")
                || line.starts_with("- ")
                || line.starts_with("* ")
                || line.starts_with("1.")
                || line.starts_with("发现")
        })
        .take(3)
        .map(|line| line.trim_start_matches(['-', '*', ' ']).to_owned())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        review
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("review 已完成，未能自动提取具体发现")
            .chars()
            .take(240)
            .collect()
    } else {
        candidates.join("；")
    }
}
