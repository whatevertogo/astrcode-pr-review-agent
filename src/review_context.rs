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
    review_file_context_with_formatter(
        config,
        file,
        commentable_lines,
        non_commentable_files,
        one_line,
    )
}

fn review_file_context_with_formatter(
    config: &Config,
    file: &PullRequestApiFile,
    commentable_lines: &mut BTreeSet<CommentLineKey>,
    non_commentable_files: &mut Vec<String>,
    format_line: fn(&str) -> String,
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
            annotate_patch_with_formatter(
                file,
                patch,
                &mut annotated,
                commentable_lines,
                format_line,
            );
        }
        _ => {
            kind = ReviewFileKind::NoPatch;
            annotated
                .push_str("no patch available; findings in this file cannot be inline-commented\n");
            non_commentable_files.push(format!("{} ({status}; no patch)", file.filename));
        }
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
                annotate_patch_with_formatter(file, patch, annotated, commentable_lines, one_line);
            }
            _ => {
                annotated.push_str(
                    "no patch available; findings in this file cannot be inline-commented\n",
                );
                non_commentable_files.push(format!("{} ({status}; no patch)", file.filename));
            }
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

fn annotate_patch_with_formatter(
    file: &PullRequestApiFile,
    patch: &str,
    annotated: &mut String,
    commentable_lines: &mut BTreeSet<CommentLineKey>,
    format_line: fn(&str) -> String,
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
            annotated.push_str(&format!("RIGHT {new_line} +{}\n", format_line(rest)));
            commentable_lines.insert(CommentLineKey {
                path: file.filename.clone(),
                side: CommentSide::Right,
                line: new_line,
            });
            new_line = new_line.saturating_add(1);
        } else if let Some(rest) = line.strip_prefix('-') {
            annotated.push_str(&format!("LEFT {old_line} -{}\n", format_line(rest)));
            commentable_lines.insert(CommentLineKey {
                path: file.filename.clone(),
                side: CommentSide::Left,
                line: old_line,
            });
            old_line = old_line.saturating_add(1);
        } else if let Some(rest) = line.strip_prefix(' ') {
            annotated.push_str(&format!("RIGHT {new_line}  {}\n", format_line(rest)));
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
