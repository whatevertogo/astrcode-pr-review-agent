use super::*;

pub(super) const PROMPT_VERSION: &str = include_str!("../../prompts/quality-review.md");
pub(super) const EVALUATION_RULES:&str="评测约束：只审查冻结的 base/head 和工作树。不得读取该 PR 的评论、review、自动审查报告、当前描述或比冻结 head 更新的代码，这些可能泄露后续答案。使用下方冻结描述和已提供的检查记录。其他必要调查限于固定版本的只读代码证据。";

pub(super) async fn analyze(
    options: &Options,
    config: &Config,
    snapshot: &ReviewSnapshot,
    host: &RunInfo,
    model: Value,
    input_key: String,
) -> Result<ReviewRunResult> {
    let worktree = checkout_pr(config, &snapshot.repo, &snapshot.pr)?;
    context::pin_base(&worktree, &snapshot.pr.base_ref_name, &snapshot.base_sha)?;
    let actual = run_command("git", &["rev-parse", "HEAD"], Some(&worktree))?;
    anyhow::ensure!(
        actual.trim() == snapshot.pr.head_ref_oid,
        "checkout is not the frozen head"
    );
    let receipts_path = options.output.join("stages.json");
    let mut stages: Vec<StageReceipt> = if receipts_path.exists() {
        read(&receipts_path)?
    } else {
        Vec::new()
    };
    let mut migration_context = None;
    let previous_result = options.output.join("result.json");
    if previous_result.exists() {
        let previous: ReviewRunResult = read(&previous_result)?;
        usage::restore_run_keys(
            &mut stages,
            &previous.stages,
            &previous.input_key,
            &input_key,
        );
        if previous.input_key == input_key && previous.schema_version < RESULT_SCHEMA_VERSION {
            migration_context = previous
                .stages
                .iter()
                .rev()
                .find(|stage| stage.label == "global" && stage.status == "complete")
                .and_then(|stage| stage.output.clone());
        }
    }
    let mut retryable_failures = false;
    let review = if options.baseline {
        baseline(
            options,
            config,
            snapshot,
            host,
            &worktree,
            &input_key,
            &mut stages,
        )
        .await?
    } else {
        let (shards, mut incomplete) = context::shards(config, &snapshot.context);
        let mut outputs = Vec::new();
        let maximum = config.max_review_passes_per_pr.max(1);
        let file_limit = maximum.saturating_sub(1);
        for shard in shards.iter().take(file_limit) {
            let instructions = shard
                .files
                .iter()
                .filter_map(|file| snapshot.instructions.get(&file.path))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let mut prompt = format!("{}\n\nPR: {}\nDescription:\n{}\nBase: {}\nHead: {}\n工作树: {}\n适用仓库规则:\n{}\n已有验证:\n{}\n先前分片的结构化结论（用于定位和复用背景，最终问题仍须复核）:\n{}\n本轮文件（只检查这些变更，可读取相关消费者）:\n{}",
                PROMPT_VERSION, snapshot.pr.title, snapshot.pr.body.as_deref().unwrap_or(""), snapshot.base_sha,
                snapshot.pr.head_ref_oid, worktree.display(), instructions, format_verification_items(&snapshot.checks),
                context::prior_conclusions(&outputs), shard.files.iter().map(|file| file.annotated_patch.as_str()).collect::<Vec<_>>().join("\n"));
            if !snapshot.request.is_empty() {
                prompt.push_str(&format!("\n本次审查要求：{}", snapshot.request));
            }
            let label = format!("file-{:03}", shard.index);
            // Successful shards retain their original shared context on recovery.
            // New evidence from retried shards is reconciled by the global pass.
            if let Some(frozen) =
                context::frozen_file_prompt(&options.output, &stages, &input_key, &label)
            {
                prompt = frozen;
            }
            match pass(
                options,
                config,
                host,
                &model,
                &worktree,
                &input_key,
                &label,
                &prompt,
                &mut stages,
            )
            .await
            {
                Ok((mut output, receipt_index)) => {
                    let paths: Vec<_> = shard.files.iter().map(|file| file.path.clone()).collect();
                    // A valid JSON response may still decorate path strings with notes.
                    // Repair that declaration once, without repeating code investigation.
                    let original =
                        fs::read_to_string(options.output.join(format!("{label}-response.txt")));
                    if coverage_has_decorated_paths(&paths, &output)
                        && config.json_repair_attempts > 0
                        && original
                            .as_ref()
                            .is_ok_and(|text| parse_review_bot_output(text).is_ok())
                    {
                        let scope: Vec<_> = shard.files.iter().map(|file| json!({
                            "path": file.path,
                            "hunks": file.annotated_patch.lines().filter(|line| line.starts_with("@@ ")).collect::<Vec<_>>()
                        })).collect();
                        let prompt = format!("只修复审查输出的 files_reviewed 路径格式，不重新调查代码、不调用工具、不生成发现。已审范围指本分片提供的全部 diff 变更，不要求阅读整个文件；对照下方 hunk 区间，原声明的已读区间覆盖全部本片变更即可。只检查了部分变更则不能补报完整覆盖。根据原声明，将确实已审的本分片路径原样填入 files_reviewed，路径不能带括注、行号或说明。不确定或明确未审完整的路径不要加入。仅返回 JSON：{{\"files_reviewed\":[...]}}。\n本分片精确路径及变更区间：{}\n原声明：{}\n原结论：{}\n剩余风险：{}",
                            json!(scope), json!(output.files_reviewed), json!(output.summary), json!(output.residual_risk));
                        let mut repair_config = config.clone();
                        repair_config.json_repair_attempts = 0;
                        match pass(
                            options,
                            &repair_config,
                            host,
                            &model,
                            &worktree,
                            &input_key,
                            &format!("{label}-coverage-format"),
                            &prompt,
                            &mut stages,
                        )
                        .await
                        {
                            Ok((repaired, _)) => output.files_reviewed = repaired.files_reviewed,
                            Err(error) if fatal(&error) => return Err(error),
                            Err(_) => {}
                        }
                    }
                    let mut omitted = Vec::new();
                    for file in &shard.files {
                        if !output.files_reviewed.contains(&file.path) {
                            incomplete.insert(file.path.clone());
                            omitted.push(file.path.clone());
                        }
                    }
                    if !omitted.is_empty() {
                        retryable_failures = true;
                        stages[receipt_index].status = "failed".into();
                        stages[receipt_index].error = Some(format!(
                            "files omitted from files_reviewed: {}",
                            omitted.join(", ")
                        ));
                        save(&receipts_path, &stages)?;
                    }
                    outputs.push(output);
                }
                Err(error) => {
                    retryable_failures = true;
                    incomplete.extend(shard.files.iter().map(|file| file.path.clone()));
                    if fatal(&error) {
                        return Err(error);
                    }
                }
            }
        }
        for shard in shards.iter().skip(file_limit) {
            incomplete.extend(shard.files.iter().map(|file| file.path.clone()));
        }
        let mut output = merge_review_outputs(&outputs);
        if let Some(previous) = migration_context {
            // Revalidate prior discoveries from this exact run during migration;
            // do not lose problems found only by the previous global pass.
            output = merge_review_outputs(&[output, previous]);
        }
        let candidates =
            !output.confirmed_findings.is_empty() || !output.advisory_findings.is_empty();
        let mut adjudicated = !candidates;
        if shards.len() > 1 || candidates {
            let instructions = snapshot
                .instructions
                .values()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            // Pass conclusions, not their complete investigation transcripts or patches.
            let candidates = context::adjudication_context(&output, &outputs);
            let mut prompt = format!("{}\n\n这是最终全局复核阶段。逐条验证候选的触发条件、影响、调用方约束、反例及本次 diff 因果关系；删除不成立或重复的问题。检查跨文件契约及调用者，可读取固定版本代码。返回完整的最终发现集合（不是增量）；只有实际复核成立的问题放 confirmed_findings。同时核对观察与剩余风险：删除已被反证、与最终结论冲突或重复的条目，其余保留完整证据和影响。返回最终 observations 与 residual_risk，不会自动追加分片旧观察。无证据的猜测不得升级。\nPR: {}\nDescription:\n{}\nBase: {}\nHead: {}\n工作树: {}\n规则:\n{}\n验证:\n{}\n文件清单:\n{}\n未完整检查:\n{:?}\n候选:\n{}",
                PROMPT_VERSION, snapshot.pr.title, snapshot.pr.body.as_deref().unwrap_or(""), snapshot.base_sha, snapshot.pr.head_ref_oid,
                worktree.display(), instructions, format_verification_items(&snapshot.checks), short_context_for_global_pass(&snapshot.context), incomplete, candidates);
            if !snapshot.request.is_empty() {
                prompt.push_str(&format!("\n本次审查要求：{}", snapshot.request));
            }
            if maximum > 1 {
                match pass(
                    options,
                    config,
                    host,
                    &model,
                    &worktree,
                    &input_key,
                    "global",
                    &prompt,
                    &mut stages,
                )
                .await
                {
                    Ok((final_output, _)) => {
                        output = final_output;
                        adjudicated = true;
                    }
                    Err(error) => {
                        retryable_failures = true;
                        if fatal(&error) {
                            return Err(error);
                        }
                        output
                            .residual_risk
                            .push(format!("全局复核失败：{error:#}"));
                        incomplete.extend(
                            snapshot
                                .context
                                .files
                                .iter()
                                .filter(|f| f.kind != ReviewFileKind::Generated)
                                .map(|f| f.path.clone()),
                        );
                    }
                }
            } else {
                incomplete.extend(snapshot.context.files.iter().map(|f| f.path.clone()));
            }
        }
        if !adjudicated {
            output
                .advisory_findings
                .append(&mut output.confirmed_findings);
        }
        output.verification = snapshot.checks.clone();
        let mut review = validation::validate(config, &output, &snapshot.context);
        let mut coverage = ReviewCoverage::default();
        for file in &snapshot.context.files {
            let (status, reason) = if incomplete.contains(&file.path) {
                (
                    CoverageStatus::Failed,
                    "未完整审查：阶段失败、缺少 patch 或达到预算",
                )
            } else if file.kind == ReviewFileKind::Generated {
                (CoverageStatus::SkippedGenerated, "生成物未逐行审查")
            } else {
                (CoverageStatus::Reviewed, "全部分片已审查")
            };
            coverage.entries.insert(
                file.path.clone(),
                CoverageEntry {
                    path: file.path.clone(),
                    status,
                    reason: reason.into(),
                },
            );
        }
        review
            .residual_risk
            .extend(incomplete.iter().map(|path| format!("未完整审查：{path}")));
        review.coverage = Some(coverage);
        review
    };
    let status = if review.coverage.as_ref().is_some_and(|coverage| {
        coverage.entries.values().any(|entry| {
            !matches!(
                entry.status,
                CoverageStatus::Reviewed | CoverageStatus::SkippedGenerated
            )
        })
    }) || review
        .verification
        .iter()
        .any(|check| check.status.as_deref() != Some("passed"))
    {
        "partial"
    } else {
        "complete"
    };
    save(&receipts_path, &stages)?;
    stages.retain(|stage| stage.run_key == input_key);
    Ok(ReviewRunResult {
        schema_version: RESULT_SCHEMA_VERSION,
        input_key,
        repo: snapshot.repo.clone(),
        pr_number: snapshot.pr.number,
        base_sha: snapshot.base_sha.clone(),
        head_sha: snapshot.pr.head_ref_oid.clone(),
        model,
        pipeline: if options.baseline {
            "baseline"
        } else {
            "quality_first"
        }
        .into(),
        status: status.into(),
        retryable_failures,
        review,
        stages,
        publication: github::PublicationReceipt::default(),
    })
}

fn fatal(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_lowercase();
    text.contains("401")
        || text.contains("403")
        || text.contains("invalid api key")
        || text.contains("model configuration changed")
}

#[allow(clippy::too_many_arguments)]
async fn pass(
    options: &Options,
    config: &Config,
    host: &RunInfo,
    model: &Value,
    worktree: &Path,
    run_key: &str,
    label: &str,
    prompt: &str,
    stages: &mut Vec<StageReceipt>,
) -> Result<(ReviewBotOutput, usize)> {
    anyhow::ensure!(
        &model_identity(host)? == model,
        "model configuration changed during review"
    );
    let key = digest(format!("{run_key}\n{label}\n{prompt}"));
    if let Some((index, previous)) = stages
        .iter_mut()
        .enumerate()
        .rev()
        .find(|(_, stage)| stage.input_key == key && stage.status == "complete")
    {
        if let Some(output) = &previous.output {
            previous.run_key = run_key.to_owned();
            return Ok((output.clone(), index));
        }
    }
    // Only the latest attempt owns the label's saved response. Revalidate its
    // exact original input after a decoder upgrade, retaining all native usage.
    if let Some((index, previous)) = stages
        .iter_mut()
        .enumerate()
        .rev()
        .find(|(_, stage)| stage.label == label)
    {
        if let Some(output) =
            context::recover_format_response(&options.output, previous, run_key, prompt)
        {
            let session = previous.session_id.clone();
            save(&options.output.join("stages.json"), stages)?;
            archive_session(options, host, &session)?;
            return Ok((output, index));
        }
    }
    // A crashed CLI may have left its private stage session running. Retire only
    // sessions recorded by this run before spending tokens on a replacement.
    for previous in stages.iter_mut().filter(|stage| stage.status == "running") {
        curl_json(
            "POST",
            &format!(
                "http://127.0.0.1:{}/api/sessions/{}/abort",
                host.port, previous.session_id
            ),
            Some(&json!({})),
        )?;
        previous.usage = usage::collect(&previous.session_id)?;
        previous.elapsed_seconds = now_epoch().saturating_sub(previous.started_at);
        previous.status = "aborted".into();
        previous.error = Some("previous process stopped before recording stage completion".into());
    }
    save(&options.output.join("stages.json"), stages)?;
    let session = create_session(host, worktree).await?;
    let started_at = now_epoch();
    let mut receipt = StageReceipt {
        label: label.into(),
        run_key: run_key.into(),
        input_key: key,
        session_id: session.clone(),
        started_at,
        elapsed_seconds: 0,
        status: "running".into(),
        error: None,
        recovered_format_error: None,
        usage: usage::Usage::default(),
        output: None,
    };
    stages.push(receipt.clone());
    let index = stages.len() - 1;
    save(&options.output.join("stages.json"), stages)?;
    fs::write(options.output.join(format!("{label}-prompt.md")), prompt)?;
    eprintln!("review stage {label}: session={session}");
    let outcome = async {
        let response = submit_prompt_and_wait(
            host,
            &session,
            prompt,
            Duration::from_secs(config.review_timeout_seconds),
        )
        .await?;
        fs::write(
            options.output.join(format!("{label}-response.txt")),
            &response,
        )?;
        let output = parse_or_repair_review_output(config, host, &session, &response).await?;
        anyhow::ensure!(
            &model_identity(host)? == model,
            "model configuration changed during review"
        );
        Ok::<_, anyhow::Error>(output)
    }
    .await;
    if outcome.is_err() {
        // This session belongs solely to this stage; never abort a resident PR session.
        let _ = curl_json(
            "POST",
            &format!(
                "http://127.0.0.1:{}/api/sessions/{session}/abort",
                host.port
            ),
            Some(&json!({})),
        );
    }
    receipt.elapsed_seconds = now_epoch().saturating_sub(started_at);
    receipt.usage = usage::collect(&session)?;
    match &outcome {
        Ok(output) => {
            receipt.status = "complete".into();
            receipt.output = Some(output.clone());
        }
        Err(error) => {
            receipt.status = "failed".into();
            receipt.error = Some(format!("{error:#}"));
        }
    }
    stages[index] = receipt;
    save(&options.output.join("stages.json"), stages)?;
    archive_session(options, host, &session)?;
    outcome.map(|output| (output, index))
}

fn archive_session(options: &Options, host: &RunInfo, session: &str) -> Result<()> {
    if let Some(path) = usage::session_log(session)? {
        let directory = options.output.join("sessions");
        fs::create_dir_all(&directory)?;
        fs::copy(path, directory.join(format!("{session}.jsonl")))?;
        // The complete audit log is now run-local; release the host's session state.
        delete_session(host, session)?;
    }
    Ok(())
}

async fn baseline(
    options: &Options,
    config: &Config,
    snapshot: &ReviewSnapshot,
    host: &RunInfo,
    worktree: &Path,
    key: &str,
    stages: &mut Vec<StageReceipt>,
) -> Result<ValidatedReview> {
    let session = create_session(host, worktree).await?;
    let started_at = now_epoch();
    stages.push(StageReceipt {
        label: "baseline-all-passes".into(),
        run_key: key.into(),
        input_key: key.into(),
        session_id: session.clone(),
        started_at,
        elapsed_seconds: 0,
        status: "running".into(),
        error: None,
        recovered_format_error: None,
        usage: usage::Usage::default(),
        output: None,
    });
    let index = stages.len() - 1;
    save(&options.output.join("stages.json"), stages)?;
    let mut original = config.clone();
    original.deterministic_checks_enabled = false;
    let trigger = context::trigger(snapshot);
    let paths = PromptMemoryPaths {
        repo_index: options.output.join("memory/index.md"),
        pr_memory: options.output.join("memory/pr.md"),
        runs_log: options.output.join("memory/runs.jsonl"),
    };
    let memory = format!(
        "{EVALUATION_RULES}\n\n冻结描述：{}\n基准 SHA：{}\nHead SHA：{}",
        snapshot.pr.body.as_deref().unwrap_or(""),
        snapshot.base_sha,
        snapshot.pr.head_ref_oid
    );
    let initial_model = model_identity(host)?;
    let outcome = async {
        let mut review = run_coverage_first_review(
            &original,
            host,
            &session,
            &trigger,
            worktree,
            &memory,
            &paths,
            false,
            &snapshot.context,
            Some(&snapshot.checks),
        )
        .await?;
        review.verification = snapshot.checks.clone();
        let published = PublishedReview {
            url: None,
            inline_review_url: None,
            inline_review_id: None,
            summary_body: String::new(),
            inline_comments_posted: 0,
            unplaced_findings_count: review.unplaced_findings.len(),
            highest_risk: None,
            verification: review.verification.clone(),
            posted_findings: Vec::new(),
        };
        let report =
            final_comment_report_pass(config, host, &session, &trigger, &review, &published)
                .await?;
        fs::write(options.output.join("baseline-model-report.md"), report)?;
        anyhow::ensure!(
            model_identity(host)? == initial_model,
            "model configuration changed during review"
        );
        Ok::<_, anyhow::Error>(review)
    }
    .await;
    if outcome.is_err() {
        let _ = curl_json(
            "POST",
            &format!(
                "http://127.0.0.1:{}/api/sessions/{session}/abort",
                host.port
            ),
            Some(&json!({})),
        );
    }
    stages[index].usage = usage::collect(&session)?;
    stages[index].elapsed_seconds = now_epoch().saturating_sub(started_at);
    stages[index].status = if outcome.is_ok() {
        "complete"
    } else {
        "failed"
    }
    .into();
    stages[index].error = outcome.as_ref().err().map(|e| format!("{e:#}"));
    save(&options.output.join("stages.json"), stages)?;
    archive_session(options, host, &session)?;
    outcome
}

pub(super) fn coverage_has_decorated_paths(paths: &[String], output: &ReviewBotOutput) -> bool {
    let missing: Vec<_> = paths
        .iter()
        .filter(|path| !output.files_reviewed.contains(path))
        .collect();
    !missing.is_empty()
        && missing.iter().all(|path| {
            output.files_reviewed.iter().any(|entry| {
                entry.strip_prefix(path.as_str()).is_some_and(|suffix| {
                    (suffix.starts_with('（') && suffix.ends_with('）'))
                        || (suffix.starts_with(" (") && suffix.ends_with(')'))
                })
            })
        })
}
