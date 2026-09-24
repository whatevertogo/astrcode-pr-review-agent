use super::*;

#[test]
fn contract_notes_decode_both_shapes_and_recover_only_bound_format_failures() {
    let payload = json!({"files_reviewed":["src/a.rs"],"investigation_log":[
        "src/a.rs:1 owns state", {"path":"src/a.rs:2","fact":"joins the writer"}
    ]});
    let parsed: ReviewBotOutput = serde_json::from_value(payload.clone()).unwrap();
    assert_eq!(
        parsed.investigation_log,
        ["src/a.rs:1 owns state", "src/a.rs:2: joins the writer"]
    );
    for bad in [
        json!({"path":"a","fact":false}),
        json!({"path":"","fact":"x"}),
        json!({"other":"x"}),
    ] {
        assert!(
            serde_json::from_value::<ReviewBotOutput>(json!({"investigation_log":[bad]})).is_err()
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let prompt = "frozen input";
    fs::write(directory.path().join("file-000-prompt.md"), prompt).unwrap();
    fs::write(
        directory.path().join("file-000-response.txt"),
        format!("Preface\n{payload}"),
    )
    .unwrap();
    let mut stage = StageReceipt {
        label: "file-000".into(),
        run_key: "run".into(),
        input_key: digest(format!("run\nfile-000\n{prompt}")),
        session_id: "session".into(),
        started_at: 1,
        elapsed_seconds: 2,
        status: "failed".into(),
        error: Some("parse assistant response as ReviewBotOutput JSON: expected string".into()),
        recovered_format_error: None,
        usage: usage::Usage {
            input_tokens: 123,
            ..usage::Usage::default()
        },
        output: None,
    };
    assert!(
        context::frozen_file_prompt(directory.path(), &[stage.clone()], "run", "file-000")
            .is_some()
    );
    assert!(context::recover_format_response(
        directory.path(),
        &mut stage,
        "different-run",
        prompt
    )
    .is_none());
    let mut authentication_failure = stage.clone();
    authentication_failure.error = Some("HTTP 401".into());
    assert!(context::recover_format_response(
        directory.path(),
        &mut authentication_failure,
        "run",
        prompt
    )
    .is_none());
    assert!(
        context::recover_format_response(directory.path(), &mut stage, "run", prompt).is_some()
    );
    assert_eq!(stage.status, "complete");
    assert_eq!(stage.usage.input_tokens, 123);
    assert!(stage.error.is_none() && stage.recovered_format_error.is_some());
    assert_eq!(stage.output.unwrap().files_reviewed, ["src/a.rs"]);
}

#[test]
fn successful_shards_freeze_context_but_reject_mismatched_or_damaged_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let prompt = "original shared context and shard";
    let path = directory.path().join("file-000-prompt.md");
    fs::write(&path, prompt).unwrap();
    let receipt = StageReceipt {
        label: "file-000".into(),
        run_key: "run".into(),
        input_key: digest(format!("run\nfile-000\n{prompt}")),
        session_id: "session".into(),
        started_at: 1,
        elapsed_seconds: 2,
        status: "complete".into(),
        error: None,
        recovered_format_error: None,
        usage: usage::Usage::default(),
        output: None,
    };
    for (status, run, expected) in [
        ("complete", "run", true),
        ("failed", "run", false),
        ("complete", "another-run", false),
    ] {
        let mut stage = receipt.clone();
        stage.status = status.into();
        assert_eq!(
            context::frozen_file_prompt(directory.path(), &[stage], run, "file-000").is_some(),
            expected
        );
    }
    fs::write(&path, "changed or overwritten prompt").unwrap();
    assert!(context::frozen_file_prompt(directory.path(), &[receipt], "run", "file-000").is_none());
}

#[test]
fn shared_conclusions_are_bounded_deduplicated_and_never_cut_an_entry() {
    let candidate = finding("Existing candidate", "RIGHT", 2, "P2", "high");
    let outputs = vec![
        ReviewBotOutput {
            investigation_log: vec!["src/a.rs:12 owns cancellation".into(), "x".repeat(16_000)],
            confirmed_findings: vec![candidate.clone()],
            ..ReviewBotOutput::default()
        },
        ReviewBotOutput {
            investigation_log: vec![
                "src/a.rs:12 owns cancellation".into(),
                "src/b.rs:20 joins the writer".into(),
            ],
            confirmed_findings: vec![candidate],
            ..ReviewBotOutput::default()
        },
    ];
    let context = context::prior_conclusions(&outputs);
    assert_eq!(context["contracts"].as_array().unwrap().len(), 2);
    assert_eq!(context["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(context["omitted_entries"], 1);
    assert!(context.to_string().len() <= 8_000);
    assert_eq!(
        context["candidates"][0]["evidence"],
        json!(outputs[0].confirmed_findings[0].evidence)
    );
    assert_eq!(outputs[0].investigation_log[1].len(), 16_000);
}

#[test]
fn decorated_coverage_requires_repair_and_never_credits_a_path_by_prefix() {
    let paths = vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()];
    for (entries, repair) in [
        (vec!["src/a.rs", "src/b.rs"], false),
        (vec!["src/a.rs（全文）", "src/b.rs (diff)"], true),
        (vec!["src/a.rs（部分变更未审）", "src/b.rs"], true),
        (vec!["src/a.rs", "src/b.rs.bak（全文）"], false),
        (vec!["src/a.rs（全文）"], false),
        (vec!["src/a.rs、src/b.rs"], false),
    ] {
        let output = ReviewBotOutput {
            files_reviewed: entries.iter().map(|path| (*path).to_owned()).collect(),
            ..ReviewBotOutput::default()
        };
        assert_eq!(
            pipeline::coverage_has_decorated_paths(&paths, &output),
            repair
        );
        assert_eq!(output.files_reviewed.len(), entries.len());
    }
}

#[test]
fn global_review_receives_complete_notes_for_reconciliation() {
    let observation: ReviewObservation = serde_json::from_value(json!({
        "title":"Caller behavior remains uncertain", "evidence":"src/a.rs:12 has no caller",
        "impact":"An external caller may use the optional field", "confidence":"low"
    }))
    .unwrap();
    let output = ReviewBotOutput {
        observations: vec![observation.clone(), observation],
        residual_risk: vec!["Build unavailable".into(), "Build unavailable".into()],
        confirmed_findings: vec![finding("Candidate", "RIGHT", 2, "P2", "high")],
        ..ReviewBotOutput::default()
    };
    let payload = context::adjudication_context(&output, &[]);
    assert_eq!(payload["observations"].as_array().unwrap().len(), 1);
    assert_eq!(
        payload["observations"][0]["impact"],
        json!(output.observations[0].impact)
    );
    assert_eq!(
        payload["observations"][0]["evidence"],
        json!(output.observations[0].evidence)
    );
    assert_eq!(payload["residual_risk"].as_array().unwrap().len(), 1);
    assert_eq!(payload["confirmed_findings"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn concurrent_run_scopes_do_not_change_the_resident_data_directory() {
    let original = agent_dir().unwrap();
    let check = |name: &'static str| {
        RUN_DATA_DIR.scope(PathBuf::from(name), async move {
            assert_eq!(agent_dir().unwrap(), PathBuf::from(name));
            tokio::task::yield_now().await;
            assert_eq!(agent_dir().unwrap(), PathBuf::from(name));
        })
    };
    tokio::join!(check("run-a"), check("run-b"));
    assert_eq!(agent_dir().unwrap(), original);
}

pub(super) fn fixture(patch: &str) -> ReviewContext {
    let file = PullRequestApiFile {
        filename: "src/example.rs".into(),
        status: Some("modified".into()),
        additions: 2,
        deletions: 1,
        changes: 3,
        patch: Some(patch.into()),
        previous_filename: None,
    };
    let (files, commentable_lines, non_commentable_files) =
        build_review_file_contexts(&Config::default(), &[file]);
    ReviewContext {
        text: String::new(),
        files,
        commentable_lines,
        non_commentable_files,
        truncated: false,
    }
}

#[test]
fn snapshot_annotation_preserves_indentation_string_spaces_and_long_lines() {
    let source = format!("    print(\"  {}  \")", "x".repeat(700));
    let file = PullRequestApiFile {
        filename: "src/example.py".into(),
        status: Some("modified".into()),
        additions: 1,
        deletions: 1,
        changes: 2,
        patch: Some(format!("@@ -1 +1 @@\n-    old()\n+{source}\n")),
        previous_filename: None,
    };
    let mut lines = BTreeSet::new();
    let mut unavailable = Vec::new();
    let context = review_file_context_with_formatter(
        &Config::default(),
        &file,
        &mut lines,
        &mut unavailable,
        str::to_owned,
    );
    assert!(context
        .annotated_patch
        .contains(&format!("RIGHT 1 +{source}\n")));
    assert!(context.annotated_patch.contains("LEFT 1 -    old()"));
    assert_eq!(lines.len(), 2);
    assert!(unavailable.is_empty());
}

pub(super) fn finding(
    title: &str,
    side: &str,
    line: u64,
    priority: &str,
    confidence: &str,
) -> ReviewFinding {
    ReviewFinding {
        non_blocking: false,
        severity: Some(priority.into()),
        confidence: Some(confidence.into()),
        category: Some("Correctness".into()),
        path: Some("src/example.rs".into()),
        side: Some(side.into()),
        line: Some(line),
        title: Some(title.into()),
        issue: Some("When cancelled, the old writer remains alive.".into()),
        evidence: Some("cancel() returns without joining writer".into()),
        project_context: Some("writer owns the store".into()),
        impact: Some("stale writes after replacement".into()),
        fix: Some("join the writer before replacement".into()),
    }
}

#[test]
fn strict_locations_filter_before_limit_and_preserve_non_inline_evidence() {
    let context = fixture("@@ -10,2 +10,3 @@\n-old\n+new\n+more\n context\n");
    let mut output = ReviewBotOutput {
        confirmed_findings: vec![
            finding("nearby invalid", "RIGHT", 9, "P0", "high"),
            finding("optional", "RIGHT", 10, "P3", "high"),
            finding("uncertain", "RIGHT", 10, "P1", "medium"),
            finding("confirmed", "LEFT", 10, "P1", "high"),
            finding("overflow", "RIGHT", 11, "P2", "high"),
        ],
        advisory_findings: vec![
            finding("advice", "RIGHT", 11, "P0", "high"),
            finding("confirmed", "LEFT", 10, "P1", "medium"),
            finding("confirmed", "LEFT", 10, "P0", "high"),
        ],
        ..ReviewBotOutput::default()
    };
    let mut malformed = finding("incomplete evidence", "RIGHT", 10, "P1", "high");
    malformed.evidence = None;
    output.confirmed_findings.push(malformed);
    let config = Config {
        max_inline_comments: 1,
        ..Config::default()
    };
    let result = validation::validate(&config, &output, &context);
    assert_eq!(result.inline_findings.len(), 1);
    assert_eq!(result.inline_findings[0].title, "confirmed");
    assert_eq!(result.inline_findings[0].side, CommentSide::Left);
    assert_eq!(result.summary_findings.len(), 5);
    assert_eq!(result.observations.len(), 1);
    assert!(result.observations[0]
        .title
        .as_deref()
        .unwrap()
        .starts_with("[P1]"));
    assert!(report::conclusion(&result).contains("已确认 4"));
    assert!(result.observations[0]
        .evidence
        .as_deref()
        .unwrap()
        .contains("cancelled"));
    assert!(result.observations[0]
        .next_step
        .as_deref()
        .unwrap()
        .contains("evidence"));
    let unplaced = result
        .summary_findings
        .iter()
        .find(|f| f.title == "nearby invalid")
        .unwrap();
    assert_eq!(unplaced.priority, "P0");
    assert_eq!(unplaced.line, 9);
    assert!(unplaced.evidence.contains("cancel()") && unplaced.evidence.contains("无法准确定位"));
    let zero = validation::validate(
        &Config {
            max_inline_comments: 0,
            ..config
        },
        &output,
        &context,
    );
    assert!(zero.inline_findings.is_empty());
    assert_eq!(zero.summary_findings.len(), 6);
    assert!(report::conclusion(&zero).contains("已确认 4"));
}

#[test]
fn shard_boundaries_preserve_hunks_and_disclose_unreviewed_content() {
    let mut context = fixture("@@ -1,1 +1,1 @@\n-old\n+new\n@@ -20,1 +20,1 @@\n-old2\n+new2\n");
    let header = context.files[0].annotated_patch.find("@@ ").unwrap();
    let limit = header + 55;
    let config = Config {
        max_files_per_shard: 1,
        review_shard_max_bytes: limit,
        ..Config::default()
    };
    let (shards, missing) = context::shards(&config, &context);
    assert_eq!(shards.len(), 2);
    assert!(missing.is_empty());
    assert!(shards
        .iter()
        .all(|s| s.bytes <= limit && s.files.len() == 1));
    assert_eq!(
        shards
            .iter()
            .flat_map(|s| &s.files)
            .map(|f| f.annotated_patch.matches("@@ ").count())
            .sum::<usize>(),
        2
    );
    context.files[0]
        .annotated_patch
        .push_str(&"x".repeat(limit));
    context.files[0].bytes = context.files[0].annotated_patch.len();
    let (shards, missing) = context::shards(&config, &context);
    assert!(missing.contains("src/example.rs"));
    assert_eq!(shards.len(), 1);
    for kind in [ReviewFileKind::NoPatch, ReviewFileKind::Generated] {
        context.files[0].kind = kind;
        let (shards, missing) = context::shards(&config, &context);
        assert!(shards.is_empty());
        assert_eq!(
            missing.contains("src/example.rs"),
            kind == ReviewFileKind::NoPatch
        );
    }
}

#[test]
fn cli_requires_explicit_publication_and_unambiguous_input() {
    let base = vec!["--repo", "owner/repo", "--pr", "1", "--output-dir", "work"];
    let parse = |args: Vec<&str>| Options::parse(args.into_iter().map(str::to_owned).collect());
    assert!(!parse(base.clone()).unwrap().publish);
    for tail in [
        vec!["--pipeline", "baseline", "--publish"],
        vec!["--prepare-only", "--publish"],
        vec!["--unknown"],
        vec!["--snapshot"],
    ] {
        let mut args = base.clone();
        args.extend(tail);
        assert!(parse(args).is_err());
    }
    assert!(parse(vec![
        "--repo",
        "../bad/repo",
        "--pr",
        "1",
        "--output-dir",
        "work"
    ])
    .is_err());
}

#[test]
fn frozen_identity_rejects_drift_but_allows_exact_revision_after_merge() {
    for (head, base, state, expected) in [
        ("h", "b", "OPEN", true),
        ("new", "b", "OPEN", false),
        ("h", "new", "OPEN", false),
        ("h", "b", "CLOSED", true),
        ("h", "b", "MERGED", true),
        ("new", "b", "MERGED", false),
        ("h", "new", "MERGED", false),
        ("h", "b", "UNKNOWN", false),
    ] {
        assert_eq!(
            context::identity_matches(
                "h",
                "b",
                &json!({"headRefOid":head,"baseRefOid":base,"state":state})
            ),
            expected
        );
    }
}

#[test]
fn artifacts_round_trip_atomically_and_cache_key_covers_every_input() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("nested/result.json");
    save(&path, &json!({"value":1})).unwrap();
    save(&path, &json!({"value":2})).unwrap();
    assert_eq!(read::<Value>(&path).unwrap()["value"], 2);
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
        context: fixture("@@ -1 +1 @@\n-old\n+new\n"),
        checks: Vec::new(),
        instructions: BTreeMap::new(),
        request: String::new(),
    };
    let config = Config::default();
    let model = json!({"model":"same"});
    let key = input_key(&snapshot, &config, &model).unwrap();
    let mut changed = snapshot.clone();
    changed.base_sha.push('x');
    assert_ne!(key, input_key(&changed, &config, &model).unwrap());
    changed.base_sha = snapshot.base_sha.clone();
    changed.pr.head_ref_oid.push('x');
    assert_ne!(key, input_key(&changed, &config, &model).unwrap());
    changed.pr.head_ref_oid = snapshot.pr.head_ref_oid.clone();
    changed
        .instructions
        .insert("AGENTS.md".into(), "changed policy".into());
    assert_ne!(key, input_key(&changed, &config, &model).unwrap());
    assert_ne!(
        key,
        input_key(
            &snapshot,
            &Config {
                max_review_passes_per_pr: 2,
                ..config
            },
            &model
        )
        .unwrap()
    );
    assert_ne!(
        key,
        input_key(&snapshot, &Config::default(), &json!({"model":"other"})).unwrap()
    );
}
