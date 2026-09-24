use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ReviewSnapshot {
    pub repo: String,
    pub pr: PullRequest,
    pub base_sha: String,
    pub context: ReviewContext,
    pub checks: Vec<VerificationItem>,
    pub instructions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub request: String,
}

pub(super) fn assert_current(snapshot: &ReviewSnapshot) -> Result<String> {
    let identity: Value = gh_json(&[
        "pr",
        "view",
        &snapshot.pr.number.to_string(),
        "--repo",
        &snapshot.repo,
        "--json",
        "headRefOid,baseRefOid,state",
    ])?;
    anyhow::ensure!(
        identity_matches(&snapshot.pr.head_ref_oid, &snapshot.base_sha, &identity),
        "PR base/head changed or state unavailable; refusing to publish stale review"
    );
    Ok(identity["state"]
        .as_str()
        .context("missing PR state")?
        .into())
}

pub(super) fn identity_matches(head: &str, base: &str, current: &Value) -> bool {
    current["headRefOid"] == head
        && current["baseRefOid"] == base
        && matches!(
            current["state"].as_str(),
            Some("OPEN" | "MERGED" | "CLOSED")
        )
}

pub(super) fn trigger(snapshot: &ReviewSnapshot) -> ReviewTrigger {
    ReviewTrigger {
        repo: snapshot.repo.clone(),
        pr: snapshot.pr.clone(),
        kind: ReviewTriggerKind::NewPullRequest,
    }
}

pub(super) fn prepare(options: &Options, config: &Config) -> Result<ReviewSnapshot> {
    let existing = options
        .snapshot
        .clone()
        .unwrap_or_else(|| options.output.join("snapshot.json"));
    if existing.exists() {
        let snapshot: ReviewSnapshot = read(&existing)?;
        anyhow::ensure!(
            snapshot.repo == options.repo && snapshot.pr.number == options.pr,
            "snapshot does not match requested PR"
        );
        anyhow::ensure!(
            options
                .head
                .as_ref()
                .is_none_or(|head| head == &snapshot.pr.head_ref_oid),
            "snapshot does not match requested historical head"
        );
        return Ok(snapshot);
    }
    anyhow::ensure!(
        options.snapshot.is_none(),
        "requested snapshot does not exist"
    );
    let mut pr = pr_details(&options.repo, options.pr)?;
    let identity: Value = gh_json(&[
        "pr",
        "view",
        &options.pr.to_string(),
        "--repo",
        &options.repo,
        "--json",
        "headRefOid,baseRefOid",
    ])?;
    anyhow::ensure!(
        identity["headRefOid"] == pr.head_ref_oid,
        "PR changed while collecting metadata"
    );
    let mut base_sha = identity["baseRefOid"]
        .as_str()
        .context("missing base SHA")?
        .to_owned();
    if let Some(head) = &options.head {
        let commits: Value = gh_json(&[
            "pr",
            "view",
            &options.pr.to_string(),
            "--repo",
            &options.repo,
            "--json",
            "commits",
        ])?;
        anyhow::ensure!(
            commits["commits"]
                .as_array()
                .is_some_and(|commits| commits.iter().any(|commit| commit["oid"] == *head)),
            "historical head is not a commit of this PR"
        );
        pr.head_ref_oid = head.clone();
        // Current descriptions may already describe later fixes: do not leak them
        // into a historical replay or pretend they described this old revision.
        pr.body=Some(format!("历史提交 {head} 的审查；当前 PR 描述可能包含后续修复，因此省略。以此版本代码和提交意图为准。"));
    }
    let worktree = checkout_pr(config, &options.repo, &pr)?;
    if options.head.is_some() {
        base_sha = run_command(
            "git",
            &["merge-base", &base_sha, &pr.head_ref_oid],
            Some(&worktree),
        )?
        .trim()
        .to_owned();
    }
    pin_base(&worktree, &pr.base_ref_name, &base_sha)?;
    let mut files: Vec<PullRequestApiFile> = if options.head.is_some() {
        let compare: Value = gh_json(&[
            "api",
            &format!(
                "repos/{}/compare/{base_sha}...{}",
                options.repo, pr.head_ref_oid
            ),
        ])?;
        let files: Vec<PullRequestApiFile> = serde_json::from_value(compare["files"].clone())?;
        let names = run_command(
            "git",
            &[
                "diff",
                "--name-only",
                "-z",
                "--find-renames",
                &format!("{base_sha}...{}", pr.head_ref_oid),
            ],
            Some(&worktree),
        )?;
        let actual: BTreeSet<_> = names.split('\0').filter(|path| !path.is_empty()).collect();
        anyhow::ensure!(
            actual == files.iter().map(|f| f.filename.as_str()).collect(),
            "historical file manifest was truncated by GitHub"
        );
        pr.files = files
            .iter()
            .map(|f| PullRequestFile {
                path: f.filename.clone(),
            })
            .collect();
        files
    } else {
        pull_request_files(&options.repo, options.pr)?
    };
    for file in &mut files {
        // Local immutable revisions also cover GitHub's truncated or absent patches.
        let range = format!("{base_sha}...{}", pr.head_ref_oid);
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            "--unified=3",
            &range,
            "--",
            &file.filename,
        ];
        if let Some(previous) = &file.previous_filename {
            args.push(previous);
        }
        let diff = run_command("git", &args, Some(&worktree))?;
        file.patch = diff.find("@@ ").map(|start| diff[start..].to_owned());
    }
    let mut file_contexts = Vec::new();
    let mut commentable_lines = BTreeSet::new();
    let mut non_commentable_files = Vec::new();
    for file in &files {
        file_contexts.push(review_file_context_with_formatter(
            config,
            file,
            &mut commentable_lines,
            &mut non_commentable_files,
            str::to_owned,
        ));
    }
    let review_trigger = ReviewTrigger {
        repo: options.repo.clone(),
        pr: pr.clone(),
        kind: ReviewTriggerKind::NewPullRequest,
    };
    let checks = if config.deterministic_checks_enabled {
        deterministic_review_verification(config, &review_trigger, &worktree)
    } else {
        Vec::new()
    };
    let mut instructions = BTreeMap::new();
    for file in &file_contexts {
        instructions.insert(
            file.path.clone(),
            path_instructions(config, &worktree, &file.path)?,
        );
    }
    let text = format!(
        "PR: {}\nDescription:\n{}\nBase: {}\nHead: {}\nChecks:\n{}\n\n{}",
        pr.title,
        pr.body.as_deref().unwrap_or(""),
        base_sha,
        pr.head_ref_oid,
        format_verification_items(&checks),
        file_contexts
            .iter()
            .map(|file| file.annotated_patch.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let snapshot = ReviewSnapshot {
        repo: options.repo.clone(),
        pr,
        base_sha,
        checks,
        instructions,
        request: String::new(),
        context: ReviewContext {
            text,
            commentable_lines,
            non_commentable_files,
            truncated: false,
            files: file_contexts,
        },
    };
    if options.head.is_none() {
        assert_current(&snapshot)?;
    }
    Ok(snapshot)
}

pub(super) fn pin_base(worktree: &Path, branch: &str, sha: &str) -> Result<()> {
    if run_command(
        "git",
        &["cat-file", "-e", &format!("{sha}^{{commit}}")],
        Some(worktree),
    )
    .is_err()
    {
        run_checkout_command("git", &["fetch", "origin", sha], Some(worktree))?;
    }
    // These caches are run-local, so freezing this ref cannot move a resident PR's base.
    run_command(
        "git",
        &["update-ref", &format!("refs/remotes/origin/{branch}"), sha],
        Some(worktree),
    )?;
    run_command(
        "git",
        &["update-ref", &format!("refs/heads/{branch}"), sha],
        Some(worktree),
    )?;
    Ok(())
}

fn path_instructions(config: &Config, worktree: &Path, path: &str) -> Result<String> {
    let mut parts = Vec::new();
    for file in [
        "AGENTS.md",
        ".github/astrcode-review.md",
        ".github/copilot-instructions.md",
    ] {
        push_instruction_file(&mut parts, worktree, file)?;
    }
    let mut parents: Vec<_> = Path::new(path)
        .ancestors()
        .skip(1)
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    parents.reverse();
    for parent in parents {
        push_instruction_file(
            &mut parts,
            worktree,
            &parent.join("AGENTS.md").to_string_lossy(),
        )?;
    }
    let directory = worktree.join(".github/instructions");
    if directory.is_dir() {
        let mut files = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
        files.sort_by_key(|entry| entry.path());
        for entry in files {
            if entry
                .file_name()
                .to_string_lossy()
                .ends_with(".instructions.md")
            {
                let text = fs::read_to_string(entry.path())?;
                if instruction_file_matches_paths(&text, &[path.to_owned()]) {
                    parts.push(text);
                }
            }
        }
    }
    let text = parts.join("\n\n");
    // Do not silently discard authoritative instructions to fit a prompt budget.
    anyhow::ensure!(
        text.len() <= config.instruction_context_max_bytes,
        "applicable instructions for {path} exceed instruction budget"
    );
    Ok(text)
}

pub(super) fn frozen_file_prompt(
    output: &Path,
    stages: &[StageReceipt],
    run_key: &str,
    label: &str,
) -> Option<String> {
    let prompt = fs::read_to_string(output.join(format!("{label}-prompt.md"))).ok()?;
    let key = digest(format!("{run_key}\n{label}\n{prompt}"));
    stages
        .iter()
        .any(|stage| {
            (stage.status == "complete" || is_format_failure(stage))
                && stage.run_key == run_key
                && stage.label == label
                && stage.input_key == key
        })
        .then_some(prompt)
}

fn is_format_failure(stage: &StageReceipt) -> bool {
    stage.status == "failed"
        && stage.error.as_deref().is_some_and(|error| {
            error.starts_with("parse assistant response as ReviewBotOutput JSON")
        })
}

pub(super) fn recover_format_response(
    output: &Path,
    stage: &mut StageReceipt,
    run_key: &str,
    prompt: &str,
) -> Option<ReviewBotOutput> {
    if !is_format_failure(stage)
        || stage.run_key != run_key
        || stage.input_key != digest(format!("{run_key}\n{}\n{prompt}", stage.label))
    {
        return None;
    }
    let saved = fs::read_to_string(output.join(format!("{}-prompt.md", stage.label))).ok()?;
    if saved != prompt {
        return None;
    }
    let raw = fs::read_to_string(output.join(format!("{}-response.txt", stage.label))).ok()?;
    let parsed = parse_review_bot_output(&raw).ok()?;
    stage.recovered_format_error = stage.error.take();
    stage.status = "complete".into();
    stage.output = Some(parsed.clone());
    Some(parsed)
}

pub(super) fn adjudication_context(output: &ReviewBotOutput, shards: &[ReviewBotOutput]) -> Value {
    let risks: BTreeSet<_> = output.residual_risk.iter().collect();
    json!({"residual_risk":risks,
        "contracts_to_verify":prior_conclusions(shards)["contracts"]})
}

/// Share whole source-backed notes, not conversation or tool-output histories.
pub(super) fn prior_conclusions(outputs: &[ReviewBotOutput]) -> Value {
    const BUDGET: usize = 8_000;
    let mut contracts = Vec::new();
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut omitted = 0;
    // Reserve space for the JSON envelope, separators and omitted-entry counter.
    let mut bytes = 128;
    let mut include = |entry: &Value| {
        let serialized = entry.to_string();
        let size = serialized.len() + 1;
        if !seen.insert(serialized) {
            return false;
        }
        if bytes + size > BUDGET {
            omitted += 1;
            return false;
        }
        bytes += size;
        true
    };
    for output in outputs {
        for note in output.investigation_log.iter().take(5) {
            let entry = json!(note);
            if !note.trim().is_empty() && include(&entry) {
                contracts.push(entry);
            }
        }
        for (kind, finding) in output
            .confirmed_findings
            .iter()
            .map(|f| ("confirmed", f))
            .chain(output.advisory_findings.iter().map(|f| ("advisory", f)))
        {
            let entry = json!({"kind":kind,"path":finding.path,"line":finding.line,
                "title":finding.title,"issue":finding.issue,"evidence":finding.evidence});
            if include(&entry) {
                candidates.push(entry);
            }
        }
    }
    json!({"contracts":contracts,"candidates":candidates,"omitted_entries":omitted})
}

/// Never split a hunk or silently truncate a large file. A single oversized hunk
/// is retained in the snapshot and explicitly excluded from a bounded model pass.
pub(super) fn shards(
    config: &Config,
    context: &ReviewContext,
) -> (Vec<ReviewShard>, BTreeSet<String>) {
    let mut units = Vec::new();
    let mut unavailable = BTreeSet::new();
    let limit = config.review_shard_max_bytes.max(1);
    for file in &context.files {
        if matches!(file.kind, ReviewFileKind::Generated) {
            continue;
        }
        if matches!(file.kind, ReviewFileKind::NoPatch) {
            unavailable.insert(file.path.clone());
            continue;
        }
        if file.bytes <= limit {
            units.push(file.clone());
            continue;
        }
        let Some(first) = file.annotated_patch.find("@@ ") else {
            unavailable.insert(file.path.clone());
            continue;
        };
        let header = &file.annotated_patch[..first];
        let mut current = String::from(header);
        let mut hunks: Vec<&str> = Vec::new();
        for line in file.annotated_patch[first..].split_inclusive('\n') {
            if line.starts_with("@@ ") && !hunks.is_empty() {
                let hunk: String = hunks.concat();
                append_hunk(
                    file,
                    header,
                    &hunk,
                    limit,
                    &mut current,
                    &mut units,
                    &mut unavailable,
                );
                hunks.clear();
            }
            hunks.push(line);
        }
        append_hunk(
            file,
            header,
            &hunks.concat(),
            limit,
            &mut current,
            &mut units,
            &mut unavailable,
        );
        if current.len() > header.len() {
            units.push(fragment(file, current));
        }
    }
    let mut result: Vec<ReviewShard> = Vec::new();
    for file in units {
        let create = result.last().is_none_or(|shard| {
            shard.files.len() >= config.max_files_per_shard.max(1)
                || shard.bytes.saturating_add(file.bytes) > limit
        });
        if create {
            result.push(ReviewShard {
                index: result.len(),
                files: Vec::new(),
                bytes: 0,
            });
        }
        let last = result.last_mut().expect("shard created");
        last.bytes += file.bytes;
        last.files.push(file);
    }
    (result, unavailable)
}

fn fragment(file: &ReviewFileContext, text: String) -> ReviewFileContext {
    ReviewFileContext {
        bytes: text.len(),
        annotated_patch: text,
        kind: if is_docs_path(&file.path) {
            ReviewFileKind::Docs
        } else {
            ReviewFileKind::Code
        },
        ..file.clone()
    }
}

#[allow(clippy::too_many_arguments)]
fn append_hunk(
    file: &ReviewFileContext,
    header: &str,
    hunk: &str,
    limit: usize,
    current: &mut String,
    units: &mut Vec<ReviewFileContext>,
    unavailable: &mut BTreeSet<String>,
) {
    if header.len() + hunk.len() > limit {
        unavailable.insert(file.path.clone());
        return;
    }
    if current.len() + hunk.len() > limit {
        units.push(fragment(
            file,
            std::mem::replace(current, header.to_owned()),
        ));
    }
    current.push_str(hunk);
}
