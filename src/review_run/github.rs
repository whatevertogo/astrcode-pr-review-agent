use super::*;

const SUMMARY_MARKER: &str = "<!-- astrcode-review-summary:v2 -->";

fn publication_lock(snapshot: &ReviewSnapshot, kind: &str) -> Result<fs::File> {
    let directory = astrcode_dir()?.join("review-publication-locks");
    fs::create_dir_all(&directory)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join(format!(
            "{}-{}-{kind}.lock",
            repo_key(&snapshot.repo),
            snapshot.pr.number
        )))?;
    file.try_lock_exclusive()
        .context("another publisher is updating this PR; retry after it finishes")?;
    Ok(file)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct PublicationReceipt {
    pub summary_id: Option<u64>,
    pub summary_url: Option<String>,
    pub review_url: Option<String>,
    pub comments: BTreeMap<String, Value>,
    pub error: Option<String>,
}

fn request(method: &str, endpoint: &str, payload: &Value) -> Result<Value> {
    let mut file = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(file.as_file_mut(), payload)?;
    file.as_file_mut().flush()?;
    gh_json_with_input(&["api", "--method", method, endpoint], file.path())
}

fn pages(endpoint: &str) -> Result<Vec<Value>> {
    let raw = run_command("gh", &["api", "--paginate", "--slurp", endpoint], None)?;
    let pages: Vec<Vec<Value>> = serde_json::from_str(&raw)?;
    Ok(pages.into_iter().flatten().collect())
}

fn viewer() -> Result<u64> {
    let user: Value = gh_json(&["api", "user"])?;
    user["id"].as_u64().context("GitHub viewer missing id")
}

fn owned_with_marker<'a>(comments: &'a [Value], owner: u64, marker: &str) -> Option<&'a Value> {
    comments.iter().rev().find(|comment| {
        comment["user"]["id"].as_u64() == Some(owner)
            && comment["body"]
                .as_str()
                .is_some_and(|body| body.starts_with(marker))
    })
}

pub(super) fn update_summary(
    snapshot: &ReviewSnapshot,
    status: &str,
    content: &str,
    output: &Path,
) -> Result<PublicationReceipt> {
    let _lock = publication_lock(snapshot, "summary")?;
    let state = context::assert_current(snapshot)?;
    let lifecycle_note = match state.as_str() {
        "MERGED" => "该 PR 已合并；以下为合并后的固定版本审查记录。\n\n",
        "CLOSED" => "该 PR 已关闭；以下为固定版本审查记录。\n\n",
        _ => "",
    };
    let owner = viewer()?;
    let endpoint = format!(
        "repos/{}/issues/{}/comments",
        snapshot.repo, snapshot.pr.number
    );
    let body = format!(
        "{SUMMARY_MARKER}\n{DEFAULT_MARKER}\n\n**{status}** · `{}`\n\n{lifecycle_note}{content}",
        snapshot.pr.head_ref_oid
    );
    // Bound GitHub payload size without cutting through Markdown/HTML structures.
    anyhow::ensure!(
        body.len() <= 60_000,
        "summary exceeds GitHub size budget; full result retained locally"
    );
    let comments = pages(&format!("{endpoint}?per_page=100"))?;
    let reply = if let Some(existing) = owned_with_marker(&comments, owner, SUMMARY_MARKER) {
        let id = existing["id"].as_u64().context("summary missing id")?;
        if existing["body"] == body {
            existing.clone()
        } else {
            request(
                "PATCH",
                &format!("repos/{}/issues/comments/{id}", snapshot.repo),
                &json!({"body":body}),
            )?
        }
    } else {
        match request("POST", &endpoint, &json!({"body":body})) {
            Ok(reply) => reply,
            Err(error) => {
                let reconciled = pages(&format!("{endpoint}?per_page=100"))?;
                owned_with_marker(&reconciled, owner, SUMMARY_MARKER)
                    .cloned()
                    .ok_or(error)?
            }
        }
    };
    let receipt = PublicationReceipt {
        summary_id: reply["id"].as_u64(),
        summary_url: reply["html_url"].as_str().map(str::to_owned),
        ..PublicationReceipt::default()
    };
    save(&output.join("summary-receipt.json"), &receipt)?;
    Ok(receipt)
}

fn finding_id(result: &ReviewRunResult, finding: &ValidatedFinding) -> String {
    digest(
        json!([
            result.repo,
            result.pr_number,
            result.head_sha,
            finding.path,
            finding_fingerprint(finding)
        ])
        .to_string(),
    )
}

fn apply_remote_comments(
    snapshot: &ReviewSnapshot,
    result: &mut ReviewRunResult,
    owner: u64,
    comments: &[Value],
) {
    for finding in &result.review.inline_findings {
        let id = finding_id(result, finding);
        let marker = format!("<!-- astrcode-finding:v2:{id} -->");
        if let Some(comment) = owned_with_marker(comments, owner, &marker) {
            result.publication.comments.insert(id,json!({"id":comment["id"],"url":comment["html_url"],"review_id":comment["pull_request_review_id"]}));
            if let Some(id) = comment["pull_request_review_id"].as_u64() {
                result.publication.review_url =
                    Some(format!("{}#pullrequestreview-{id}", snapshot.pr.url));
            }
        }
    }
}

fn publish_inline(
    snapshot: &ReviewSnapshot,
    result: &mut ReviewRunResult,
    owner: u64,
    mut list: impl FnMut() -> Result<Vec<Value>>,
    mut post: impl FnMut(&Value) -> Result<Value>,
) -> Result<()> {
    apply_remote_comments(snapshot, result, owner, &list()?);
    let pending:Vec<_>=result.review.inline_findings.iter()
        .filter(|finding|!result.publication.comments.contains_key(&finding_id(result,finding)))
        .map(|finding|json!({"path":finding.path,"line":finding.line,"side":finding.side.as_github(),
            "body":format!("<!-- astrcode-finding:v2:{} -->\n{DEFAULT_MARKER}\n{}",finding_id(result,finding),report::inline(finding))}))
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    let payload = json!({"commit_id":result.head_sha,"event":"COMMENT",
        "body":format!("固定版本审查：{}。详情见本 PR 的审查总览。",result.head_sha),"comments":pending});
    let response = post(&payload);
    if let Ok(reply) = &response {
        result.publication.review_url = reply["html_url"].as_str().map(str::to_owned);
    }
    // A timed-out POST may have succeeded. Reconcile once, never fan out blind retries.
    apply_remote_comments(snapshot, result, owner, &list()?);
    if result.publication.comments.len() < result.review.inline_findings.len() {
        result.publication.error = Some(
            response
                .err()
                .map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "GitHub has not confirmed all inline comments".into()),
        );
        anyhow::bail!("inline publication not fully confirmed; reconcile on next explicit retry");
    }
    Ok(())
}

pub(super) fn publish(
    snapshot: &ReviewSnapshot,
    result: &mut ReviewRunResult,
    output: &Path,
) -> Result<()> {
    let _lock = publication_lock(snapshot, "inline")?;
    context::assert_current(snapshot)?;
    let owner = viewer()?;
    if !result.review.inline_findings.is_empty() {
        // Git gives complete analysis context; GitHub owns inline placement.
        let files = pull_request_files(&snapshot.repo, snapshot.pr.number)?;
        let (_, commentable, _) = build_review_file_contexts(&Config::default(), &files);
        let mut placed = Vec::new();
        for mut finding in result.review.inline_findings.drain(..) {
            let key = CommentLineKey {
                path: finding.path.clone(),
                side: finding.side,
                line: finding.line,
            };
            if commentable.contains(&key) {
                placed.push(finding);
            } else {
                finding
                    .evidence
                    .push_str("\nGitHub 未提供可用的精确行内位置，保留在摘要中。");
                result.review.summary_findings.push(finding);
            }
        }
        result.review.inline_findings = placed;
    }
    let outcome = publish_inline(
        snapshot,
        result,
        owner,
        || {
            pages(&format!(
                "repos/{}/pulls/{}/comments?per_page=100",
                snapshot.repo, snapshot.pr.number
            ))
        },
        |payload| {
            save(&output.join("publish-payload.json"), payload)?;
            context::assert_current(snapshot)?;
            request(
                "POST",
                &format!(
                    "repos/{}/pulls/{}/reviews",
                    snapshot.repo, snapshot.pr.number
                ),
                payload,
            )
        },
    );
    save(&output.join("result.json"), result)?;
    outcome?;
    result.publication.error = None;
    context::assert_current(snapshot)?;
    let rendered = report::render(result);
    let content = rendered
        .split_once("\n\n")
        .map(|(_, body)| body)
        .unwrap_or(&rendered);
    let summary = update_summary(
        snapshot,
        if result.status == "complete" {
            "审查完成"
        } else {
            "部分完成"
        },
        content,
        output,
    )?;
    result.publication.summary_id = summary.summary_id;
    result.publication.summary_url = summary.summary_url;
    save(&output.join("publication.json"), &result.publication)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn ambiguous_post_reconciles_full_partial_and_rejected_batches_before_replay() {
        use super::super::tests::{finding, fixture};
        for initially_posted in [0, 1, 2] {
            let context = fixture("@@ -10,2 +10,3 @@\n-old\n+new\n+more\n context\n");
            let output = ReviewBotOutput {
                confirmed_findings: vec![
                    finding("one", "RIGHT", 10, "P1", "high"),
                    finding("two", "RIGHT", 11, "P2", "high"),
                ],
                ..ReviewBotOutput::default()
            };
            let snapshot = ReviewSnapshot {
                repo: "owner/repo".into(),
                pr: PullRequest {
                    number: 1,
                    title: "test".into(),
                    url: "https://github.test/owner/repo/pull/1".into(),
                    head_ref_oid: "head".into(),
                    base_ref_name: "main".into(),
                    body: None,
                    files: Vec::new(),
                    author: None,
                },
                base_sha: "base".into(),
                context: context.clone(),
                checks: Vec::new(),
                instructions: BTreeMap::new(),
                request: String::new(),
            };
            let mut result = ReviewRunResult {
                schema_version: 1,
                input_key: "key".into(),
                repo: snapshot.repo.clone(),
                pr_number: 1,
                base_sha: "base".into(),
                head_sha: "head".into(),
                model: json!({}),
                pipeline: "quality_first".into(),
                status: "complete".into(),
                retryable_failures: false,
                review: validation::validate(&Config::default(), &output, &context),
                stages: Vec::new(),
                publication: PublicationReceipt::default(),
            };
            let mut path_context = context.clone();
            let mut path_output = ReviewBotOutput::default();
            for path in [
                "src/A.rs",
                "src/a.rs",
                "src/a b.rs",
                "src/a  b.rs",
                " src/a.rs",
                "src/a.rs ",
            ] {
                let mut candidate = output.confirmed_findings[0].clone();
                candidate.path = Some(path.into());
                path_output.confirmed_findings.push(candidate);
                let mut file = context.files[0].clone();
                file.path = path.into();
                file.annotated_patch = file.annotated_patch.replace("src/example.rs", path);
                path_context.files.push(file);
                path_context.commentable_lines.insert(CommentLineKey {
                    path: path.into(),
                    side: CommentSide::Right,
                    line: 10,
                });
            }
            let path_review = validation::validate(&Config::default(), &path_output, &path_context);
            assert_eq!(path_review.inline_findings.len(), 6);
            let ids: BTreeSet<_> = path_review
                .inline_findings
                .iter()
                .map(|finding| finding_id(&result, finding))
                .collect();
            assert_eq!(
                ids.len(),
                6,
                "Git paths preserve case and whitespace in publication identities"
            );
            let remote = RefCell::new(Vec::new());
            let posts = Cell::new(0);
            let mut post = |payload: &Value| {
                let count = posts.get();
                posts.set(count + 1);
                assert_eq!(payload["event"], "COMMENT");
                assert_eq!(payload["commit_id"], "head");
                let comments = payload["comments"].as_array().unwrap();
                if count == 1 {
                    assert_eq!(comments.len(), 2 - initially_posted);
                }
                for comment in comments.iter().take(if count == 0 {
                    initially_posted
                } else {
                    comments.len()
                }) {
                    let id = remote.borrow().len() + 1;
                    remote.borrow_mut().push(json!({"id":id,"user":{"id":1},"body":comment["body"],"html_url":"https://github.test/comment","pull_request_review_id":7}));
                }
                if count == 0 {
                    anyhow::bail!("connection closed after sending request");
                }
                Ok(json!({"html_url":"https://github.test/review"}))
            };
            let first = publish_inline(
                &snapshot,
                &mut result,
                1,
                || Ok(remote.borrow().clone()),
                &mut post,
            );
            assert_eq!(first.is_ok(), initially_posted == 2);
            assert_eq!(result.publication.comments.len(), initially_posted);
            publish_inline(
                &snapshot,
                &mut result,
                1,
                || Ok(remote.borrow().clone()),
                &mut post,
            )
            .unwrap();
            publish_inline(
                &snapshot,
                &mut result,
                1,
                || Ok(remote.borrow().clone()),
                &mut post,
            )
            .unwrap();
            assert_eq!(remote.borrow().len(), 2);
            assert_eq!(posts.get(), if initially_posted == 2 { 1 } else { 2 });
        }
    }
    #[test]
    fn reconciliation_requires_both_marker_and_actual_author() {
        let comments = [
            json!({"user":{"id":2},"body":SUMMARY_MARKER,"id":1}),
            json!({"user":{"id":1},"body":"human comment","id":2}),
            json!({"user":{"id":1},"body":SUMMARY_MARKER,"id":3}),
        ];
        assert_eq!(
            owned_with_marker(&comments, 1, SUMMARY_MARKER).unwrap()["id"],
            3
        );
        assert!(owned_with_marker(&comments[..2], 1, SUMMARY_MARKER).is_none());
    }
}
