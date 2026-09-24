use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use fs2::FileExt;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use wait_timeout::ChildExt;

include!("types.rs");
include!("webhook.rs");
include!("poller.rs");
include!("staging.rs");
include!("review.rs");
include!("review_context.rs");
include!("review_validation.rs");
include!("status.rs");

mod mention;
mod review_comments;
mod review_global;
pub use mention::{discover_once, enqueue_cli, mention_status_cli};

mod review_run;
pub use review_run::{render_review_result, review_cli};

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(root: &Path) -> Config {
        Config {
            memory_dir: root.join("memory").to_string_lossy().into_owned(),
            worktree_dir: root.join("worktrees").to_string_lossy().into_owned(),
            ..Config::default()
        }
    }

    fn comment(id: u64, body: &str) -> IssueComment {
        comment_by(id, "alice", body)
    }

    fn comment_by(id: u64, user: &str, body: &str) -> IssueComment {
        IssueComment {
            id,
            body: Some(body.into()),
            user: Some(GhUser { login: user.into() }),
            html_url: Some(format!("https://github.test/comment/{id}")),
            created_at: Some("2026-06-27T00:00:00Z".into()),
        }
    }

    fn pr() -> PullRequest {
        PullRequest {
            number: 7,
            title: "Improve storage".into(),
            url: "https://github.test/repo/pull/7".into(),
            head_ref_oid: "abc123".into(),
            base_ref_name: "main".into(),
            body: Some("Improve storage durability.".into()),
            files: vec![PullRequestFile {
                path: "src/storage.rs".into(),
            }],
            author: Some(GhUser {
                login: "bob".into(),
            }),
        }
    }

    fn trigger(repo: &str, pr_number: u64, comment_id: u64) -> ReviewTrigger {
        let mut pr = pr();
        pr.number = pr_number;
        ReviewTrigger {
            repo: repo.into(),
            pr,
            kind: ReviewTriggerKind::MentionComment(comment(
                comment_id,
                "@whatevertogo review this",
            )),
        }
    }

    fn auto_trigger(repo: &str, pr_number: u64) -> ReviewTrigger {
        let mut pr = pr();
        pr.number = pr_number;
        ReviewTrigger {
            repo: repo.into(),
            pr,
            kind: ReviewTriggerKind::NewPullRequest,
        }
    }

    fn review_finding(priority: &str, title: &str, line: u64) -> ReviewFinding {
        ReviewFinding {
            severity: Some(priority.into()),
            confidence: Some("high".into()),
            category: Some("Correctness".into()),
            path: Some("src/storage.rs".into()),
            side: Some("RIGHT".into()),
            line: Some(line),
            title: Some(title.into()),
            issue: Some("The changed code can return an incorrect value.".into()),
            evidence: Some("Checked the annotated diff and related caller.".into()),
            project_context: Some("Storage changes affect persisted user state.".into()),
            impact: Some("Users can observe stale or lost data.".into()),
            fix: Some("Update the storage path before returning success.".into()),
        }
    }

    fn validated_finding(priority: &str, title: &str, line: u64) -> ValidatedFinding {
        ValidatedFinding {
            priority: priority.into(),
            kind: FindingKind::Confirmed,
            confidence: "high".into(),
            category: "Correctness".into(),
            path: "src/storage.rs".into(),
            side: CommentSide::Right,
            line,
            title: title.into(),
            issue: "The new value is returned before being persisted.".into(),
            evidence: "Checked the annotated diff and related caller.".into(),
            project_context: "Storage changes affect persisted user state.".into(),
            impact: "A crash can lose user data.".into(),
            fix: "Persist first, then return success.".into(),
            original_index: 0,
        }
    }

    fn memory_paths(config: &Config, repo: &str, pr_number: u64) -> PromptMemoryPaths {
        PromptMemoryPaths {
            repo_index: repo_memory_index_path(config, repo).unwrap(),
            pr_memory: pr_memory_path(config, repo, pr_number).unwrap(),
            runs_log: config.memory_dir_path().unwrap().join("runs.jsonl"),
        }
    }

    fn test_review_context() -> ReviewContext {
        let mut commentable_lines = BTreeSet::new();
        commentable_lines.insert(CommentLineKey {
            path: "src/storage.rs".into(),
            side: CommentSide::Right,
            line: 10,
        });
        commentable_lines.insert(CommentLineKey {
            path: "src/storage.rs".into(),
            side: CommentSide::Left,
            line: 8,
        });
        ReviewContext {
            text: "GitHub command audit:\n- `gh pr view 7 --repo whatevertogo/astrcodey --json \
                   ...`: collected\n\nAnnotated diff.\n--- file: src/storage.rs status=modified \
                   +1 -1 changes=2\n@@ -8,1 +10,1 @@\nLEFT 8 -old\nRIGHT 10 +new"
                .into(),
            commentable_lines,
            non_commentable_files: Vec::new(),
            truncated: false,
            files: vec![ReviewFileContext {
                path: "src/storage.rs".into(),
                status: "modified".into(),
                additions: 1,
                deletions: 1,
                changes: 2,
                previous_filename: None,
                annotated_patch: "\n--- file: src/storage.rs status=modified +1 -1 changes=2\n@@ \
                                  -8,1 +10,1 @@\nLEFT 8 -old\nRIGHT 10 +new"
                    .into(),
                kind: ReviewFileKind::Code,
                bytes: 96,
            }],
        }
    }

    #[test]
    fn trigger_comment_requires_mention() {
        let config = Config::default();
        assert_eq!(
            config.repos,
            vec!["VitaDynamics/Vvbot", "whatevertogo/astrcodey"]
        );
        assert_eq!(
            config.trusted_comment_authors,
            vec![
                "whatevertogo",
                "catDforD",
                "letr007",
                "united-pooh",
                "Soulter"
            ]
        );
        assert!(is_trigger_comment(
            &config,
            "VitaDynamics/Vvbot",
            &comment(1, "please @whatevertogo review")
        ));
        assert!(!is_trigger_comment(
            &config,
            "VitaDynamics/Vvbot",
            &comment(2, "please review")
        ));
        assert!(!is_trigger_comment(
            &config,
            "VitaDynamics/Vvbot",
            &comment(3, "<!-- astrcode-auto-review -->\n@whatevertogo")
        ));
        assert!(!is_trigger_comment(
            &config,
            "VitaDynamics/Vvbot",
            &comment(4, "我是 whatevertogo 的替身。\n@whatevertogo")
        ));
        assert!(!is_trigger_comment(
            &config,
            "VitaDynamics/Vvbot",
            &comment(5, "我是 whatevertogo 的自动化审查 agent。\n@whatevertogo")
        ));
        for marker in [
            "<!-- astrcode-review-summary:v2 -->",
            "<!-- astrcode-finding:v2:id -->",
        ] {
            assert!(!is_trigger_comment(
                &config,
                "VitaDynamics/Vvbot",
                &comment(6, &format!("{marker}\n@whatevertogo review"))
            ));
        }
    }

    #[test]
    fn trigger_permission_allows_trusted_authors_in_any_repo() {
        let config = Config::default();

        assert!(is_trigger_comment(
            &config,
            "someone/any-repo",
            &comment_by(1, "Soulter", "@whatevertogo review")
        ));
        assert!(is_trigger_comment(
            &config,
            "someone/any-repo",
            &comment_by(2, "catdford", "@whatevertogo review")
        ));
    }

    #[test]
    fn trigger_permission_allows_untrusted_authors_only_in_allowlisted_repos() {
        let config = Config::default();

        assert!(is_trigger_comment(
            &config,
            "VitaDynamics/Vvbot",
            &comment_by(1, "outside-user", "@whatevertogo review")
        ));
        assert!(!is_trigger_comment(
            &config,
            "someone/any-repo",
            &comment_by(2, "outside-user", "@whatevertogo review")
        ));
    }

    #[test]
    fn processed_key_is_comment_scoped() {
        assert_eq!(
            processed_key("VitaDynamics/Vvbot", 12, 99),
            "VitaDynamics/Vvbot#12:99"
        );
    }

    #[test]
    fn reconciliation_runs_every_poll_cycle() {
        let mut state = State {
            last_reconciliation_at: Some(now_epoch()),
            ..State::default()
        };
        let config = Config {
            poll_fallback_seconds: 900,
            ..Config::default()
        };

        assert!(should_run_reconciliation(&config, &state));
        state.last_reconciliation_at = Some(1);
        assert!(should_run_reconciliation(&config, &state));
    }

    #[test]
    fn pr_memory_path_is_repo_and_pr_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path());
        let path = pr_memory_path(&config, "VitaDynamics/Vvbot", 12).unwrap();
        assert!(path.ends_with("memory/repos/VitaDynamics__Vvbot/pr-12.md"));
        let index = repo_memory_index_path(&config, "VitaDynamics/Vvbot").unwrap();
        assert!(index.ends_with("memory/repos/VitaDynamics__Vvbot/index.md"));
    }

    #[test]
    fn review_prompt_embeds_pr_review_bot_instructions() {
        let tmp = tempfile::tempdir().unwrap();
        let trigger = ReviewTrigger {
            repo: "whatevertogo/astrcodey".into(),
            pr: pr(),
            kind: ReviewTriggerKind::MentionComment(comment(42, "@whatevertogo review this")),
        };
        let config = test_config(tmp.path());
        let paths = memory_paths(&config, &trigger.repo, trigger.pr.number);
        let context = test_review_context();
        let prompt = review_prompt(
            &trigger,
            tmp.path(),
            "old context",
            &paths,
            true,
            Some(&context),
        );
        assert!(prompt.starts_with("我是 whatevertogo 的替身。"));
        assert!(!prompt.starts_with("/reviewnow"));
        assert!(prompt.contains("相关的既有 PR 记忆"));
        assert!(prompt.contains("old context"));
        assert!(prompt.contains("reused existing PR session"));
        assert!(prompt.contains("Improve storage durability."));
        assert!(prompt.contains("src/storage.rs"));
        assert!(prompt.contains("Do exactly what the trigger comment asks for."));
        assert!(prompt.contains("使用简体中文编写简洁 Markdown"));
        assert!(prompt.contains("内置 PR 审查规范"));
        assert!(prompt.contains("示例"));
        assert!(prompt.contains("Correctness"));
        assert!(prompt.contains("Security"));
        assert!(prompt.contains("Reliability/Performance"));
        assert!(prompt.contains("Tests/API Contract"));
        assert!(prompt.contains("插件收集的 GitHub PR 上下文"));
        assert!(prompt.contains("GitHub command audit"));
        assert!(prompt.contains("<finding"));
        assert!(prompt.contains("priority=\"P2\""));
        assert!(prompt.contains("do not run `gh api` to create comments"));
        assert!(prompt.contains("插件会校验标签并发布行内评论"));
        assert!(prompt.contains("The plugin posts all GitHub review comments"));
        assert!(prompt.contains("gh pr view 7 --repo whatevertogo/astrcodey"));
        assert!(prompt.contains("rg -n"));
        assert!(prompt.contains("memory/repos/whatevertogo__astrcodey/pr-7.md"));
        assert!(prompt.contains("memory/repos/whatevertogo__astrcodey/index.md"));
        assert!(!prompt.contains("gh api repos/{repo}/pulls/{pr}/comments"));
        assert!(!prompt.contains("Please use the reviewnow skill"));
    }

    #[test]
    fn review_prompt_does_not_force_reviewnow_for_non_review_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let trigger = ReviewTrigger {
            repo: "whatevertogo/astrcodey".into(),
            pr: pr(),
            kind: ReviewTriggerKind::MentionComment(comment(
                42,
                "@whatevertogo summarize the latest CI state",
            )),
        };
        let config = test_config(tmp.path());
        let paths = memory_paths(&config, &trigger.repo, trigger.pr.number);
        let prompt = review_prompt(&trigger, tmp.path(), "", &paths, true, None);
        assert!(prompt.contains("不要强行进行代码审查"));
        assert!(prompt.contains("本 trigger 不是 review 任务"));
    }

    #[test]
    fn auto_review_prompt_embeds_pr_review_bot() {
        let tmp = tempfile::tempdir().unwrap();
        let trigger = auto_trigger("VitaDynamics/Vvbot", 621);
        let config = test_config(tmp.path());
        let paths = memory_paths(&config, &trigger.repo, trigger.pr.number);
        let context = test_review_context();
        let prompt = review_prompt(&trigger, tmp.path(), "", &paths, false, Some(&context));

        assert!(prompt.starts_with("我是 whatevertogo 的替身。"));
        assert!(prompt.contains("触发类型：new_pull_request"));
        assert!(prompt.contains("这是新 PR 首次发现自动 review"));
        assert!(prompt.contains("返回带标签的 Markdown 发现"));
        assert!(prompt.contains("内置 PR 审查规范"));
        assert!(prompt.contains("示例"));
        assert!(prompt.contains("插件收集的 GitHub PR 上下文"));
        assert!(prompt.contains("<finding"));
        assert!(!prompt.contains("gh api repos/{repo}/pulls/{pr}/comments"));
        assert!(!prompt.contains("Please use the reviewnow skill"));
        assert!(prompt.contains("created new PR session"));
    }

    #[test]
    fn orientation_prompt_injects_repo_memory_and_context() {
        let tmp = tempfile::tempdir().unwrap();
        let trigger = auto_trigger("VitaDynamics/Vvbot", 621);
        let config = test_config(tmp.path());
        let context = test_review_context();
        let prompt = orientation_review_prompt(
            &config,
            &trigger,
            tmp.path(),
            "## Repository PR/Issue relation reminders\n- PR #620 affects storage",
            &context,
            &[],
        );

        assert!(prompt.contains("定向阶段"));
        assert!(prompt.contains("Repository PR/Issue relation reminders"));
        assert!(prompt.contains("PR #620 affects storage"));
        assert!(prompt.contains("插件收集的 PR 上下文"));
        assert!(prompt.contains("src/storage.rs"));
        assert!(!prompt.contains("GitHub command audit"));
        assert!(prompt.contains("标签输出协议"));
    }

    #[test]
    fn annotated_diff_tracks_right_left_and_no_patch_files() {
        let files = vec![
            PullRequestApiFile {
                filename: "src/storage.rs".into(),
                status: Some("modified".into()),
                additions: 2,
                deletions: 1,
                changes: 3,
                previous_filename: None,
                patch: Some(
                    "@@ -8,2 +10,3 @@\n context\n-old value\n+new value\n+extra value\n".into(),
                ),
            },
            PullRequestApiFile {
                filename: "assets/logo.png".into(),
                status: Some("modified".into()),
                additions: 0,
                deletions: 0,
                changes: 1,
                previous_filename: None,
                patch: None,
            },
        ];
        let mut annotated = String::new();
        let mut commentable = BTreeSet::new();
        let mut non_commentable = Vec::new();

        annotate_pull_files(
            &files,
            &mut annotated,
            &mut commentable,
            &mut non_commentable,
        );

        assert!(annotated.contains("RIGHT 10  context"));
        assert!(annotated.contains("LEFT 9 -old value"));
        assert!(annotated.contains("RIGHT 11 +new value"));
        assert!(commentable.contains(&CommentLineKey {
            path: "src/storage.rs".into(),
            side: CommentSide::Right,
            line: 11,
        }));
        assert!(commentable.contains(&CommentLineKey {
            path: "src/storage.rs".into(),
            side: CommentSide::Left,
            line: 9,
        }));
        assert_eq!(
            non_commentable,
            vec!["assets/logo.png (modified; no patch)"]
        );
    }

    #[test]
    fn review_bot_json_parses_from_plain_or_fenced_json_and_ignores_model_verification() {
        let plain = r#"{
            "confirmed_findings": [],
            "advisory_findings": [],
            "observations": ["check related PR #620 before merging"],
            "files_reviewed": [],
            "investigation_log": [],
            "verification": [{"command":"static review","status":"passed","notes":"ok"}],
            "residual_risk": [],
            "summary": "No confirmed issues found."
        }"#;
        let fenced = format!("```json\n{plain}\n```");

        let parsed = parse_review_bot_output(plain).unwrap();
        let fenced_parsed = parse_review_bot_output(&fenced).unwrap();

        assert_eq!(parsed.confirmed_findings.len(), 0);
        assert_eq!(parsed.observations.len(), 1);
        assert_eq!(
            parsed.observations[0].title.as_deref(),
            Some("check related PR #620 before merging")
        );
        assert_eq!(fenced_parsed.verification.len(), 0);
        assert!(parse_review_bot_output("not json").is_err());
    }

    #[test]
    fn review_bot_json_preserves_legacy_observation_objects() {
        let plain = r#"{
            "confirmed_findings": [],
            "advisory_findings": [],
            "observations": [{
                "id": "OBS-HOST-ROUTER-PERMISSION-BROADENING",
                "type": "risky_subsystem",
                "severity": "high",
                "content": "SessionControl can read events without a caller session."
            }],
            "files_reviewed": [],
            "investigation_log": [],
            "residual_risk": [],
            "summary": "Orientation only."
        }"#;

        let parsed = parse_review_bot_output(plain).unwrap();
        let observation = parsed.observations.first().unwrap();

        assert_eq!(observation.confidence.as_deref(), Some("high"));
        assert_eq!(observation.category.as_deref(), Some("risky_subsystem"));
        assert_eq!(
            observation.title.as_deref(),
            Some("OBS-HOST-ROUTER-PERMISSION-BROADENING")
        );
        assert_eq!(
            observation.evidence.as_deref(),
            Some("SessionControl can read events without a caller session.")
        );
    }

    #[test]
    fn review_bot_parses_tagged_markdown_findings() {
        let tagged = r#"
I checked the storage path.

<files_reviewed>
src/storage.rs
</files_reviewed>

<finding kind="confirmed" priority="P2" confidence="high" category="Correctness" path="src/storage.rs" side="RIGHT" line="42" title="Persist before mutating the index">
Issue: The index is mutated before the durable write completes.
Evidence: RIGHT 42 updates the map before persist_note returns.
Project context: Store rebuild relies on disk being the source of truth.
Impact: A crash can expose memory that cannot be reconstructed.
Fix: Write the note first, then update the in-memory index.
</finding>

<observation confidence="medium" category="Repo History" title="Related migration risk">
Evidence: PR #620 touched the same storage migration.
Project context: The memory store has legacy import paths.
Impact: The reviewer should re-check migration ordering.
Next step: Inspect legacy.rs before publishing.
</observation>

<summary>
One concrete finding and one repo-history reminder.
</summary>
"#;

        let parsed = parse_review_bot_output(tagged).unwrap();
        let finding = parsed.confirmed_findings.first().unwrap();

        assert_eq!(parsed.files_reviewed, vec!["src/storage.rs"]);
        assert_eq!(finding.severity.as_deref(), Some("P2"));
        assert_eq!(finding.confidence.as_deref(), Some("high"));
        assert_eq!(finding.category.as_deref(), Some("Correctness"));
        assert_eq!(finding.path.as_deref(), Some("src/storage.rs"));
        assert_eq!(finding.side.as_deref(), Some("RIGHT"));
        assert_eq!(finding.line, Some(42));
        assert_eq!(
            finding.issue.as_deref(),
            Some("The index is mutated before the durable write completes.")
        );
        assert_eq!(parsed.observations.len(), 1);
        assert_eq!(
            parsed.summary.as_deref(),
            Some("One concrete finding and one repo-history reminder.")
        );
    }

    #[test]
    fn staged_review_path_component_is_filesystem_safe() {
        assert_eq!(
            sanitize_path_component("VitaDynamics/Vvbot#624:4816550420"),
            "VitaDynamics_Vvbot_624_4816550420"
        );
    }

    #[test]
    fn file_manifest_classifies_docs_generated_no_patch_and_oversized() {
        let config = Config {
            review_shard_max_bytes: 300,
            ..Config::default()
        };
        let files = vec![
            PullRequestApiFile {
                filename: "docs/guide.md".into(),
                status: Some("modified".into()),
                additions: 1,
                deletions: 0,
                changes: 1,
                previous_filename: None,
                patch: Some("@@ -1,1 +1,1 @@\n+doc\n".into()),
            },
            PullRequestApiFile {
                filename: "Cargo.lock".into(),
                status: Some("modified".into()),
                additions: 1,
                deletions: 0,
                changes: 1,
                previous_filename: None,
                patch: Some("@@ -1,1 +1,1 @@\n+lock\n".into()),
            },
            PullRequestApiFile {
                filename: "assets/logo.png".into(),
                status: Some("modified".into()),
                additions: 0,
                deletions: 0,
                changes: 1,
                previous_filename: None,
                patch: None,
            },
            PullRequestApiFile {
                filename: "src/large.rs".into(),
                status: Some("modified".into()),
                additions: 50,
                deletions: 0,
                changes: 50,
                previous_filename: None,
                patch: Some(format!("@@ -1,1 +1,50 @@\n+{}", "x".repeat(1_000))),
            },
        ];

        let (contexts, _, non_commentable) = build_review_file_contexts(&config, &files);

        assert_eq!(contexts[0].kind, ReviewFileKind::Docs);
        assert_eq!(contexts[1].kind, ReviewFileKind::Generated);
        assert_eq!(contexts[2].kind, ReviewFileKind::NoPatch);
        assert_eq!(contexts[3].kind, ReviewFileKind::Oversized);
        assert_eq!(
            non_commentable,
            vec!["assets/logo.png (modified; no patch)"]
        );
    }

    #[test]
    fn global_review_budget_coverage_and_candidate_accounting() {
        for maximum in [0, 1, 2, 3, 8, 16, 32] {
            let plan = review_global::file_budget(maximum);
            if maximum < 2 {
                assert!(plan.is_err());
                continue;
            }
            let (orientation, files) = plan.unwrap();
            assert!(files >= 1);
            assert_eq!(usize::from(orientation) + files + 1, maximum);
            if maximum == 8 {
                assert_eq!(files, 6);
            }
        }
        let mut context = test_review_context();
        let mut extra = context.files[0].clone();
        extra.path = "src/not-reached.rs".into();
        context.files.push(extra);
        let mut coverage = initial_coverage(&context);
        assert_eq!(coverage.reviewed_count(), 0);
        coverage.mark(
            "src/storage.rs",
            CoverageStatus::Reviewed,
            "successful file stage",
        );
        coverage.mark(
            "src/not-reached.rs",
            CoverageStatus::Failed,
            "budget exhausted",
        );
        assert_eq!(
            coverage.entries["src/not-reached.rs"].status,
            CoverageStatus::Failed
        );
        assert!(coverage_residual_risk(&coverage)
            .iter()
            .any(|r| r.contains("budget exhausted")));

        let prior = ReviewBotOutput {
            confirmed_findings: vec![review_finding("P1", "candidate", 10)],
            observations: vec![ReviewObservation {
                title: Some("possible race".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let candidates = review_global::candidates(&prior);
        let raw = json!({"global_review_complete":true,"investigation_log":["caller.rs:20 guards the path"],
        "observations":[{"title":"possible race","evidence":"writer.rs:10 uses another lock"}],
        "candidate_checks":[
            {"id":"C001","outcome":"rejected","reason":"caller.rs:20 retries the failed operation"},
            {"id":"C002","outcome":"observation","result_index":0,"reason":"writer.rs:10; interleaving remains unverified"}
        ]});
        let output = parse_review_bot_output(&raw.to_string()).unwrap();
        review_global::validate(&output, &candidates).unwrap();
        assert!(output.confirmed_findings.is_empty());
        assert_eq!(output.observations.len(), 1);
        let mut validated = validate_review_output(&Config::default(), &output, &context);
        assert!(validated.global_review_complete);
        assert!(review_global::summary(&validated).contains("保留 0，排除 1，待核实 1"));
        validated.global_review_complete = false;
        assert!(publish_structured_review(
            &Config::default(),
            &auto_trigger("owner/repo", 1),
            "fixture",
            &validated
        )
        .unwrap_err()
        .to_string()
        .contains("mandatory global review"));
        for invalid in [
            json!({"global_review_complete":false,"candidate_checks":raw["candidate_checks"],"observations":raw["observations"]}),
            json!({"global_review_complete":true,"candidate_checks":[raw["candidate_checks"][0]]}),
            json!({"global_review_complete":true,"candidate_checks":[raw["candidate_checks"][0],raw["candidate_checks"][0]]}),
            json!({"global_review_complete":true,"candidate_checks":[{"id":"C003","outcome":"rejected","reason":"unknown"}]}),
            json!({"global_review_complete":true,"candidate_checks":raw["candidate_checks"],"observations":[]}),
        ] {
            let mut invalid = invalid;
            invalid["investigation_log"] = raw["investigation_log"].clone();
            assert!(review_global::validate(
                &parse_review_bot_output(&invalid.to_string()).unwrap(),
                &candidates
            )
            .is_err());
        }
        let tagged = parse_review_bot_output("<global_review_complete>true</global_review_complete><investigation_log>caller.rs:20 guards the path</investigation_log><candidate_check id=\"C001\" outcome=\"rejected\">caller.rs:20 guards the path</candidate_check><candidate_check id=\"C002\" outcome=\"rejected\">writer.rs:10 serializes both writers</candidate_check>").unwrap();
        review_global::validate(&tagged, &candidates).unwrap();
        assert!(review_global::validate(&ReviewBotOutput::default(), &[]).is_err());
        review_global::validate(
            &ReviewBotOutput {
                global_review_complete: true,
                investigation_log: vec!["caller.rs:20 checked against base".into()],
                ..Default::default()
            },
            &[],
        )
        .unwrap();
        let repaired = review_global::repair_receipt(
            &output,
            ReviewBotOutput {
                global_review_complete: true,
                candidate_checks: output.candidate_checks.clone(),
                ..Default::default()
            },
        );
        assert_eq!(repaired.observations.len(), output.observations.len());
        assert_eq!(repaired.investigation_log, output.investigation_log);
        review_global::validate(&repaired, &candidates).unwrap();
        assert!(review_global::validate(
            &ReviewBotOutput {
                global_review_complete: true,
                ..Default::default()
            },
            &[]
        )
        .is_err());
    }

    #[tokio::test]
    async fn stage_cache_requires_identical_input_and_preserves_legacy_records() {
        let tmp = tempfile::tempdir().unwrap();
        RUN_DATA_DIR
            .scope(tmp.path().to_path_buf(), async {
                let trigger = auto_trigger("owner/repo", 1);
                let path = staged_review_path(&trigger).unwrap();
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                let legacy = json!({"label":"global-pass","created_at":1,"output":{}}).to_string();
                fs::write(&path, format!("{legacy}\n")).unwrap();
                let mut run =
                    StagedReviewRun::load(&trigger, "base/head/model/config-A".into()).unwrap();
                assert!(run.output("global-pass", "prompt").is_none());
                run.append("file-pass-001", "prompt", &ReviewBotOutput::default())
                    .unwrap();
                let restored =
                    StagedReviewRun::load(&trigger, "base/head/model/config-A".into()).unwrap();
                assert!(restored.output("file-pass-001", "prompt").is_some());
                assert!(restored.output("file-pass-001", "changed prompt").is_none());
                assert!(restored.output("global-pass", "prompt").is_none());
                let changed =
                    StagedReviewRun::load(&trigger, "base/head/model/config-B".into()).unwrap();
                assert!(changed.output("file-pass-001", "prompt").is_none());
                assert!(fs::read_to_string(path).unwrap().starts_with(&legacy));
            })
            .await;
    }

    #[test]
    fn shard_planner_covers_reviewable_files() {
        let config = Config {
            max_files_per_shard: 1,
            review_shard_max_bytes: 10_000,
            ..Config::default()
        };
        let mut context = test_review_context();
        context.files.push(ReviewFileContext {
            path: "src/other.rs".into(),
            status: "modified".into(),
            additions: 1,
            deletions: 0,
            changes: 1,
            previous_filename: None,
            annotated_patch: "\n--- file: src/other.rs status=modified +1 -0 changes=1\n@@ -0,0 \
                              +1,1 @@\nRIGHT 1 +new"
                .into(),
            kind: ReviewFileKind::Code,
            bytes: 90,
        });
        context.files.push(ReviewFileContext {
            path: "Cargo.lock".into(),
            status: "modified".into(),
            additions: 1,
            deletions: 0,
            changes: 1,
            previous_filename: None,
            annotated_patch: "lock".into(),
            kind: ReviewFileKind::Generated,
            bytes: 4,
        });

        let shards = plan_review_shards(&config, &context);
        let paths = shards
            .iter()
            .flat_map(|shard| shard.files.iter().map(|file| file.path.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(shards.len(), 2);
        assert!(paths.contains(&"src/storage.rs"));
        assert!(paths.contains(&"src/other.rs"));
        assert!(!paths.contains(&"Cargo.lock"));
    }

    #[test]
    fn finding_validation_filters_invalid_duplicate_and_overflow() {
        let config = Config {
            max_inline_comments: 1,
            ..Config::default()
        };
        let context = test_review_context();
        let output = ReviewBotOutput {
            confirmed_findings: vec![
                review_finding("P2", "Valid issue", 10),
                review_finding("P2", "Valid issue", 10),
                review_finding("P1", "Bad line", 999),
                ReviewFinding {
                    severity: Some("P3".into()),
                    confidence: Some("high".into()),
                    category: Some("Tests/API Contract".into()),
                    path: Some("src/storage.rs".into()),
                    side: Some("LEFT".into()),
                    line: Some(8),
                    title: Some("Overflow issue".into()),
                    issue: Some("Old behavior needs test.".into()),
                    evidence: Some("Checked the removed branch.".into()),
                    project_context: Some(
                        "Storage behavior has regression coverage conventions.".into(),
                    ),
                    impact: Some("Regression may slip.".into()),
                    fix: Some("Add a test.".into()),
                },
            ],
            files_reviewed: Vec::new(),
            verification: Vec::new(),
            residual_risk: Vec::new(),
            summary: None,
            ..ReviewBotOutput::default()
        };

        let validated = validate_review_output(&config, &output, &context);

        assert_eq!(validated.inline_findings.len(), 1);
        assert_eq!(validated.inline_findings[0].title, "Valid issue");
        assert_eq!(validated.unplaced_findings.len(), 3);
        assert!(validated
            .unplaced_findings
            .iter()
            .any(|finding| finding.reason.contains("duplicate")));
        assert!(validated
            .unplaced_findings
            .iter()
            .any(|finding| finding.reason.contains("not a commentable")));
        assert!(validated
            .unplaced_findings
            .iter()
            .any(|finding| finding.reason.contains("max_inline_comments")));
    }

    #[test]
    fn review_api_payload_and_summary_include_agent_marker_and_counts() {
        let config = Config::default();
        let trigger = auto_trigger("VitaDynamics/Vvbot", 621);
        let finding = validated_finding("P1", "Persist storage before returning", 10);
        let validated = ValidatedReview {
            global_review_complete: false,
            candidate_checks: Vec::new(),
            inline_findings: vec![finding.clone()],
            summary_findings: Vec::new(),
            unplaced_findings: Vec::new(),
            observations: Vec::new(),
            investigation_log: Vec::new(),
            verification: vec![VerificationItem {
                command: Some("static review of annotated PR diff".into()),
                status: Some("passed".into()),
                notes: Some("one issue found".into()),
            }],
            residual_risk: Vec::new(),
            summary: Some("One issue found.".into()),
            coverage: Some({
                let mut coverage = ReviewCoverage::default();
                coverage.mark(
                    "src/storage.rs",
                    CoverageStatus::Reviewed,
                    "file review pass 1",
                );
                coverage
            }),
            debug_dir: None,
        };
        let summary = StructuredReviewSummary {
            config: &config,
            trigger: &trigger,
            session_id: "s1",
            validated: &validated,
            inline_comments_posted: 1,
            unplaced_count: 0,
            highest_risk: Some("P1 Persist storage before returning"),
            publish_error: None,
        };
        let body = structured_review_summary_body(&summary);
        let payload = pull_review_payload(&config, &trigger, &body, &[finding]);
        let comments = payload
            .get("comments")
            .and_then(Value::as_array)
            .expect("comments array");
        let comment_body = comments[0].get("body").and_then(Value::as_str).unwrap();

        assert_eq!(
            payload.get("event").and_then(Value::as_str),
            Some("COMMENT")
        );
        assert_eq!(
            payload.get("commit_id").and_then(Value::as_str),
            Some(trigger.pr.head_ref_oid.as_str())
        );
        assert_eq!(
            comments[0].get("path").and_then(Value::as_str),
            Some("src/storage.rs")
        );
        assert_eq!(comments[0].get("line").and_then(Value::as_u64), Some(10));
        assert_eq!(
            comments[0].get("side").and_then(Value::as_str),
            Some("RIGHT")
        );
        assert!(comment_body.starts_with("<!-- astrcode-auto-review -->\n**[P1]"));
        assert!(comment_body.contains("**[P1] Persist storage before returning**"));
        assert!(comment_body.contains("**影响**：A crash can lose user data."));
        assert!(comment_body.contains("**建议**：Persist first, then return success."));
        assert!(body.contains("已发布行内评论：1"));
        assert!(body.contains("暂无法定位的发现：0"));
        assert!(body.contains("已审查文件：1/1"));
    }

    #[test]
    fn batched_review_publish_falls_back_to_individual_findings() {
        let config = Config::default();
        let trigger = auto_trigger("VitaDynamics/Vvbot", 621);
        let first = validated_finding("P1", "First issue", 10);
        let second = validated_finding("P2", "Second issue", 11);
        let mut unplaced = Vec::new();
        let mut calls = 0usize;
        let mut batch_sizes = Vec::new();

        let result = publish_inline_findings_with_fallback(
            &config,
            &trigger,
            "s1",
            vec![first, second],
            &mut unplaced,
            Some("P1 First issue"),
            |_body, findings| {
                calls += 1;
                batch_sizes.push(findings.len());
                if calls == 1 {
                    anyhow::bail!("422 Unprocessable Entity");
                }
                if findings[0].title == "Second issue" {
                    anyhow::bail!("individual line rejected");
                }
                Ok(PostedPullReview {
                    url: Some(format!("https://github.test/review/{calls}")),
                    id: Some(calls as u64),
                })
            },
        );

        assert_eq!(batch_sizes, vec![2, 1, 1]);
        assert_eq!(result.inline_comments_posted, 1);
        assert_eq!(result.inline_findings[0].title, "First issue");
        assert_eq!(result.posted_findings.len(), 1);
        assert_eq!(result.individual_review_urls.len(), 1);
        assert_eq!(unplaced.len(), 1);
        assert_eq!(unplaced[0].title, "Second issue");
        assert!(unplaced[0].reason.contains("individual retry failed"));
        assert!(result
            .publish_error
            .as_deref()
            .unwrap()
            .contains("retried 2 finding(s) individually, posted 1, failed 1"));
    }

    #[test]
    fn auto_review_baseline_marks_existing_open_prs_without_queueing() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path());
        let mut state = State::default();
        let existing = vec![("VitaDynamics/Vvbot".into(), pr())];

        let baselined = baseline_auto_review_repos(&config, &mut state, &existing).unwrap();
        let queued =
            enqueue_auto_review_trigger(&config, &mut state, "VitaDynamics/Vvbot", &pr()).unwrap();

        assert_eq!(baselined, config.repos.len());
        assert!(queued.is_none());
        assert!(state.seen_open_prs.contains_key("VitaDynamics/Vvbot#7"));
        assert!(state.auto_pr_reviews.is_empty());
    }

    #[test]
    fn auto_review_queues_new_open_pr_once_after_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path());
        let mut state = State::default();
        baseline_auto_review_repos(&config, &mut state, &[]).unwrap();

        let first =
            enqueue_auto_review_trigger(&config, &mut state, "VitaDynamics/Vvbot", &pr()).unwrap();
        state
            .auto_pr_reviews
            .get_mut("VitaDynamics/Vvbot#7")
            .unwrap()
            .status = STATUS_COMMENTED.into();
        let mut updated = pr();
        updated.head_ref_oid = "def456".into();
        let second =
            enqueue_auto_review_trigger(&config, &mut state, "VitaDynamics/Vvbot", &updated)
                .unwrap();

        assert!(first.is_some());
        assert!(second.is_none());
        assert_eq!(state.auto_pr_reviews.len(), 1);
        assert_eq!(
            state
                .auto_pr_reviews
                .get("VitaDynamics/Vvbot#7")
                .unwrap()
                .status,
            STATUS_COMMENTED
        );
        assert_eq!(
            state
                .seen_open_prs
                .get("VitaDynamics/Vvbot#7")
                .unwrap()
                .head_sha,
            "def456"
        );
    }

    #[test]
    fn pending_auto_review_is_requeued_after_interrupted_process() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path());
        let mut state = State::default();
        baseline_auto_review_repos(&config, &mut state, &[]).unwrap();

        let first =
            enqueue_auto_review_trigger(&config, &mut state, "VitaDynamics/Vvbot", &pr()).unwrap();
        assert!(first.is_some());

        let mut updated = pr();
        updated.head_ref_oid = "def456".into();
        let recovered =
            enqueue_auto_review_trigger(&config, &mut state, "VitaDynamics/Vvbot", &updated)
                .unwrap();

        let recovered = recovered.expect("pending auto review should be requeued");
        assert!(recovered.is_auto_review());
        assert_eq!(recovered.pr.head_ref_oid, "def456");
        assert_eq!(
            state
                .auto_pr_reviews
                .get("VitaDynamics/Vvbot#7")
                .unwrap()
                .head_sha,
            "def456"
        );
    }

    #[test]
    fn mention_triggers_sort_before_auto_reviews() {
        let mut state = State::default();
        let mention = trigger("VitaDynamics/Vvbot", 7, 42);
        let auto = auto_trigger("VitaDynamics/Vvbot", 8);

        assert!(insert_pending_mention_trigger(&mut state, &mention));
        state.auto_pr_reviews.insert(
            auto.state_key(),
            new_auto_pr_review(&auto.repo, &auto.pr, STATUS_PENDING),
        );
        state
            .processed_comments
            .get_mut("VitaDynamics/Vvbot#7:42")
            .unwrap()
            .started_at = 20;
        state
            .auto_pr_reviews
            .get_mut("VitaDynamics/Vvbot#8")
            .unwrap()
            .started_at = 10;

        let mut triggers = vec![auto, mention];
        sort_pending_triggers(&state, &mut triggers);

        assert!(triggers[0].comment().is_some());
        assert!(triggers[1].is_auto_review());
    }

    #[test]
    fn auto_review_comments_include_marker_and_agent_line() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path());
        let trigger = auto_trigger("VitaDynamics/Vvbot", 7);

        let start_body = auto_review_start_comment_body(&config, &trigger);
        assert!(start_body.starts_with("<!-- astrcode-auto-review -->\n我是 whatevertogo 的替身。"));
        assert!(start_body.contains("已启动自动审查"));

        let final_body = review_comment_body(&config, &trigger, "s1", "No issues.");
        assert!(final_body.starts_with("<!-- astrcode-auto-review -->\n我是 whatevertogo 的替身。"));
        assert!(final_body.contains("触发：新 PR 自动审查"));

        let failure_body = auto_review_failure_comment_body(&config, &trigger, "timeout");
        assert!(
            failure_body.starts_with("<!-- astrcode-auto-review -->\n我是 whatevertogo 的替身。")
        );
        assert!(failure_body.contains("PR 审查失败"));
        assert!(failure_body.contains("本次任务已停止"));
        assert!(failure_body.contains("触发：新 PR 自动审查"));
        assert!(failure_body.contains("@whatevertogo review it"));
    }

    #[test]
    fn checkout_retry_recovers_transient_failures_and_reports_exhaustion() {
        let mut recovery_attempts = 0;
        let value = retry_operation("checkout", 3, Duration::ZERO, || {
            recovery_attempts += 1;
            if recovery_attempts < 3 {
                anyhow::bail!("temporary network failure");
            }
            Ok("ready")
        })
        .unwrap();

        assert_eq!(value, "ready");
        assert_eq!(recovery_attempts, 3);

        let mut failed_attempts = 0;
        let error = retry_operation::<()>("checkout", 3, Duration::ZERO, || {
            failed_attempts += 1;
            anyhow::bail!("network unavailable")
        })
        .unwrap_err();

        assert_eq!(failed_attempts, 3);
        assert!(error
            .to_string()
            .contains("checkout failed after 3 attempts"));
    }

    #[test]
    fn cargo_check_scope_targets_changed_crates_and_expands_for_workspace_inputs() {
        let metadata = CargoMetadata {
            workspace_root: "/repo".into(),
            packages: vec![
                CargoMetadataPackage {
                    name: "alpha".into(),
                    manifest_path: "/repo/crates/alpha/Cargo.toml".into(),
                },
                CargoMetadataPackage {
                    name: "beta".into(),
                    manifest_path: "/repo/crates/beta/Cargo.toml".into(),
                },
            ],
        };

        assert_eq!(
            select_cargo_check_scope(
                &metadata,
                &[
                    "crates/beta/tests/behavior.rs".into(),
                    "crates/alpha/src/lib.rs".into(),
                ],
            ),
            CargoCheckScope::Packages(vec!["alpha".into(), "beta".into()])
        );
        assert_eq!(
            select_cargo_check_scope(&metadata, &["crates/alpha/Cargo.toml".into()]),
            CargoCheckScope::Workspace
        );
        assert!(!rust_verification_path("docs/review.md"));
    }

    #[test]
    fn latest_error_from_session_log_extracts_provider_error() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("session-s1.jsonl");
        fs::write(
            &log,
            r#"{"payload":{"type":"error_occurred","message":"first"}}
{"payload":{"type":"turn_completed"}}
{"payload":{"type":"error_occurred","message":"Client error (429): quota reached"}}"#,
        )
        .unwrap();

        assert_eq!(
            latest_error_from_session_log(&log).as_deref(),
            Some("Client error (429): quota reached")
        );
    }

    #[test]
    fn review_comment_body_starts_with_marker_and_agent_line() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path());
        let trigger = ReviewTrigger {
            repo: "whatevertogo/astrcodey".into(),
            pr: pr(),
            kind: ReviewTriggerKind::MentionComment(comment(42, "@whatevertogo review this")),
        };
        let body = review_comment_body(&config, &trigger, "s1", "没有发现需要处理的问题。");
        assert!(body.starts_with("<!-- astrcode-auto-review -->\n我是 whatevertogo 的替身。"));
        assert!(body.contains("审查会话：`s1`"));
    }

    #[test]
    fn inline_comment_body_uses_chinese_labels() {
        let tmp = tempfile::tempdir().unwrap();
        let config = test_config(tmp.path());
        let body =
            inline_review_comment_body(&config, &validated_finding("P2", "持久化顺序错误", 10));

        assert!(body.contains("**[P2] 持久化顺序错误**"));
        assert!(body.contains("**依据**："));
        assert!(body.contains("**影响**："));
        assert!(body.contains("**建议**："));
        assert!(body.find("**依据**").unwrap() < body.find("<details>").unwrap());
        assert!(!body.contains("Category:"));
    }

    #[test]
    fn github_markdown_preserves_blocks_and_escapes_metadata() {
        let mut finding = validated_finding("P2", "状态 **错误**\n# 下一行 <tag>", 10);
        finding.path = "src/a`b.rs".into();
        finding.project_context.clear();
        for evidence in [
            "- 第一项\n- 第二项",
            "1. 第一步\n2. 第二步",
            "```rust\nreturn Err(error);\n```",
            "> 触发条件成立",
            "| 条件 | 结果 |\n|---|---|\n| 重试 | 失败 |",
        ] {
            finding.evidence = evidence.into();
            let body = review_comments::finding(&finding);
            assert!(body.contains(&format!("**依据**\n\n{evidence}\n")));
            assert!(!body.contains("<details>"));
            assert!(!body.contains("\n# 下一行"));
            assert!(body.contains("\\*\\*错误\\*\\*"));
            assert!(body.contains("&lt;tag&gt;"));
        }
        assert!(review_comments::finding_details(&finding).contains("`` src/a`b.rs:10 · RIGHT ``"));
        let long_path = format!("src/{}  name.rs", "a".repeat(510));
        assert_eq!(review_comments::code(&long_path), format!("`{long_path}`"));
        let mut folded = "前一段".to_owned();
        review_comments::fold(&mut folded, "A < B & C", "- 内容");
        assert!(folded.contains("前一段\n\n<details>"));
        assert!(folded.contains("<summary>A &lt; B &amp; C</summary>\n\n- 内容\n\n</details>"));

        let mut raw = json!({
            "schema_version":3,"input_key":"fixture","repo":"example/repo","pr_number":1,
            "base_sha":"aaaa","head_sha":"bbbb","model":{},"pipeline":"quality_first","status":"partial",
            "review": {"inline_findings":[],"summary_findings":[finding, finding],
                "unplaced_findings":[],"observations":[],"investigation_log":[],
                "verification":[],"residual_risk":[],"summary":null,"coverage":null,"debug_dir":null},
            "stages":[]
        });
        for (checks, expected) in [
            (json!([]), "未记录实际执行的检查"),
            (
                json!([{"command":"cargo test","status":"passed"}]),
                "1 项确定性检查通过",
            ),
            (
                json!([{"command":"echo `x`","status":"failed","notes":"第一行\n\n```text\nerror\n```"}]),
                "- `` echo `x` `` · failed\n  \n  第一行",
            ),
        ] {
            raw["review"]["verification"] = checks;
            let body = render_review_result(&raw.to_string()).unwrap();
            assert!(body.contains(expected), "{body}");
            assert!(!body.contains("不要汇总"));
            assert!(!body.contains("模型用量与耗时"));
            assert!(body.contains("其他发现与建议（2）"));
            assert!(body.contains("\n\n---\n\n"));
        }
        raw["stages"] = json!([{
            "label":"file | <x>\nextra","input_key":"fixture","session_id":"fixture",
            "started_at":0,"elapsed_seconds":3,"status":"complete","error":null,
            "usage":{"input_tokens":10,"cached_input_tokens":4,"cache_creation_input_tokens":0,
                "uncached_input_tokens":6,"output_tokens":2,"reasoning_output_tokens":0,
                "requests":1,"tool_calls":0,"estimated_requests":0,"unknown_accounting_requests":0,"missing_usage_requests":0}
        }]);
        let body = render_review_result(&raw.to_string()).unwrap();
        assert!(body.contains("| file \\| &lt;x&gt; extra | complete | 输入 10<br>缓存输入 4<br>输出 2 | 请求 1<br>工具 0<br>3 秒 |"));
    }

    #[test]
    fn comments_preserve_evidence_and_label_uncertainty_without_repeated_metadata() {
        let config = Config::default();
        for (kind, confidence, uncertain) in [
            (FindingKind::Confirmed, "high", false),
            (FindingKind::Advisory, "high", true),
            (FindingKind::Confirmed, "medium", true),
        ] {
            let mut finding = validated_finding("P2", "写入前不能报告成功", 10);
            finding.kind = kind;
            finding.confidence = confidence.into();
            finding.evidence = "`write.rs:10` 在返回前没有执行持久化；调用方没有补写路径。".into();
            let body = inline_review_comment_body(&config, &finding);
            let visible = body.split("<details>").next().unwrap();
            for fact in [
                &finding.issue,
                &finding.impact,
                &finding.evidence,
                &finding.fix,
            ] {
                assert!(visible.contains(fact));
            }
            assert_eq!(visible.contains("待核实建议"), uncertain);
            assert!(body.contains(&finding.project_context));
            assert_eq!(body.matches(&finding.evidence).count(), 1);
            assert!(!body.contains(AGENT_LINE));
            assert!(!is_trigger_comment(
                &config,
                "VitaDynamics/Vvbot",
                &comment_by(1, "whatevertogo", &format!("{body} @whatevertogo"))
            ));
        }
        for response in [
            "<files_reviewed>src/client.py</files_reviewed><finding kind=\"confirmed\" priority=\"P3\" confidence=\"high\" title=\"无标题\"></finding><summary>没有发现缺陷</summary>".to_owned(),
            json!({"files_reviewed":["src/client.py"],"confirmed_findings":[{"title":"无标题","issue":" "}],"summary":"没有发现缺陷"}).to_string(),
        ] {
            let parsed=parse_review_bot_output(&response).unwrap();
            assert!(parsed.confirmed_findings.is_empty());
            assert_eq!(parsed.files_reviewed,["src/client.py"]);
        }
        for finding in [
            json!({"title":"初始化失败后没有重试路径"}),
            json!({"title":"无标题","evidence":"caller.rs:12 has no retry"}),
        ] {
            let response = json!({"advisory_findings":[finding]}).to_string();
            assert_eq!(
                parse_review_bot_output(&response)
                    .unwrap()
                    .advisory_findings
                    .len(),
                1
            );
        }
        let examples = parse_review_bot_output(PR_REVIEW_FEW_SHOTS_PROMPT).unwrap();
        assert_eq!(examples.confirmed_findings.len(), 1);
        assert_eq!(examples.observations.len(), 1);
        assert_eq!(
            examples.confirmed_findings[0].path.as_deref(),
            Some("src/enqueue.rs")
        );
    }

    #[test]
    fn report_fallback_and_publication_receipts_do_not_overstate_success() {
        let config = Config::default();
        let trigger = ReviewTrigger {
            repo: "owner/repo".into(),
            pr: pr(),
            kind: ReviewTriggerKind::NewPullRequest,
        };
        let first = validated_finding("P1", "第一个问题", 10);
        let second = validated_finding("P2", "第二个问题", 11);
        let published = PublishedReview {
            url: None,
            inline_review_url: Some("https://github.test/review/1".into()),
            inline_review_id: Some(1),
            summary_body: String::new(),
            inline_comments_posted: 1,
            unplaced_findings_count: 1,
            highest_risk: Some("P1 第一个问题".into()),
            verification: Vec::new(),
            posted_findings: vec![finding_memory_from_validated(
                &second,
                &trigger.pr.head_ref_oid,
            )],
        };
        let mut review = ValidatedReview {
            global_review_complete: true,
            candidate_checks: Vec::new(),
            inline_findings: vec![first.clone(), second.clone()],
            summary_findings: Vec::new(),
            unplaced_findings: Vec::new(),
            observations: Vec::new(),
            investigation_log: Vec::new(),
            verification: Vec::new(),
            residual_risk: Vec::new(),
            summary: None,
            coverage: None,
            debug_dir: None,
        };
        let prompt = final_comment_report_prompt(&trigger, &review, &published);
        assert!(prompt.contains("仅总结发现：1"));
        let posted = prompt
            .split("实际已发布的行内发现：")
            .nth(1)
            .unwrap()
            .split("仅总结的发现：")
            .next()
            .unwrap();
        assert!(posted.contains(&second.title));
        assert!(!posted.contains(&first.title));
        let unposted = prompt
            .split("仅总结的发现：")
            .nth(1)
            .unwrap()
            .split("无法定位的发现说明：")
            .next()
            .unwrap();
        assert!(unposted.contains(&first.title));
        assert!(unposted.contains(&first.issue));
        for (covered, checks, partial) in [
            (false, vec![], true),
            (true, vec![], true),
            (
                true,
                vec![VerificationItem {
                    command: Some("cargo check".into()),
                    status: Some("failed".into()),
                    notes: Some("linker unavailable".into()),
                }],
                true,
            ),
            (
                true,
                vec![VerificationItem {
                    command: Some("cargo check".into()),
                    status: Some("passed".into()),
                    notes: None,
                }],
                false,
            ),
        ] {
            review.verification = checks;
            review.coverage = covered.then(|| {
                let mut c = ReviewCoverage::default();
                c.mark("src/storage.rs", CoverageStatus::Reviewed, "reviewed");
                c
            });
            let fallback =
                fallback_final_report(&review, &published, Some("report formatter failed"));
            let body = final_review_comment_body(
                &config, &trigger, "session", &review, &published, &fallback,
            );
            assert_eq!(body.contains("**部分完成**"), partial);
            assert!(body.contains(&first.evidence));
            assert!(body.contains(&first.fix));
            assert!(!body.contains("未发现确认的阻塞问题"));
            assert!(!body.contains("## 合并评估"));
            assert!(body.find("第一个问题").unwrap() < body.find("审查会话").unwrap());
        }
        review.inline_findings.clear();
        review
            .unplaced_findings
            .push(unplaced_from_validated(first, "line unavailable".into()));
        let fallback = fallback_final_report(&review, &published, None);
        assert!(fallback.contains("未能定位或完整验证"));
        assert!(!fallback.contains("未发现需要修复"));
    }

    #[test]
    fn summarize_review_extracts_findings() {
        let trigger = ReviewTrigger {
            repo: "whatevertogo/astrcodey".into(),
            pr: pr(),
            kind: ReviewTriggerKind::MentionComment(comment(42, "@whatevertogo review this")),
        };
        let published = PublishedReview {
            url: Some("https://github.test/review".into()),
            inline_review_url: None,
            inline_review_id: None,
            summary_body: "- [P1] Fix the storage race\n- [P2] Add tests".into(),
            inline_comments_posted: 0,
            unplaced_findings_count: 0,
            highest_risk: None,
            verification: Vec::new(),
            posted_findings: Vec::new(),
        };
        let summary = summarize_review(&trigger, "s1", &published);
        assert!(summary.contains("[P1] Fix the storage race"));
        assert!(summary.contains("会话 `s1`"));
    }

    #[test]
    fn enqueue_trigger_records_pending_without_overwriting_existing_state() {
        let mut state = State::default();
        let trigger = trigger("owner/repo", 7, 42);

        assert!(insert_pending_mention_trigger(&mut state, &trigger));
        assert!(!insert_pending_mention_trigger(&mut state, &trigger));

        assert_eq!(state.processed_comments.len(), 1);
        let record = state
            .processed_comments
            .get("owner/repo#7:42")
            .expect("queued trigger should be recorded");
        assert_eq!(record.status, STATUS_PENDING);
        assert_eq!(record.session_id, None);
    }

    #[test]
    fn pending_triggers_are_sorted_by_queue_time() {
        let mut state = State::default();
        let first = trigger("owner/repo", 7, 1);
        let second = trigger("owner/repo", 8, 2);
        state.processed_comments.insert(
            "owner/repo#7:1".into(),
            processed_comment("owner/repo", 7, None, STATUS_PENDING, None),
        );
        state.processed_comments.insert(
            "owner/repo#8:2".into(),
            processed_comment("owner/repo", 8, None, STATUS_PENDING, None),
        );
        state
            .processed_comments
            .get_mut("owner/repo#7:1")
            .expect("first trigger")
            .started_at = 20;
        state
            .processed_comments
            .get_mut("owner/repo#8:2")
            .expect("second trigger")
            .started_at = 10;

        let mut triggers = vec![first, second];
        sort_pending_triggers(&state, &mut triggers);

        assert_eq!(triggers[0].pr.number, 8);
        assert_eq!(triggers[1].pr.number, 7);
    }

    #[test]
    fn recover_stale_running_reviews_frees_the_queue() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            review_timeout_seconds: 1,
            ..test_config(tmp.path())
        };
        let mut state = State::default();
        state.processed_comments.insert(
            "owner/repo#7:42".into(),
            processed_comment("owner/repo", 7, Some("session-1"), STATUS_RUNNING, None),
        );

        let recovered = mark_stale_running_reviews_failed(&config, &mut state);

        assert_eq!(recovered, 1);
        assert!(!has_running_review(&state));
        let record = state
            .processed_comments
            .get("owner/repo#7:42")
            .expect("stale running review should remain tracked");
        assert_eq!(record.status, STATUS_FAILED);
        assert!(record.error.as_deref().unwrap_or("").contains("timeout"));
    }

    fn pr_session(repo: &str, pr_number: u64, session_id: &str, worktree: &Path) -> PrSession {
        PrSession {
            repo: repo.into(),
            pr_number,
            session_id: session_id.into(),
            worktree: worktree.to_string_lossy().into_owned(),
            last_head_sha: "abc123".into(),
            created_at: 1,
            updated_at: 1,
        }
    }

    fn processed_comment(
        repo: &str,
        pr_number: u64,
        session_id: Option<&str>,
        status: &str,
        finished_at: Option<u64>,
    ) -> ProcessedComment {
        ProcessedComment {
            repo: repo.into(),
            pr_number,
            comment_id: 42,
            head_sha: "abc123".into(),
            session_id: session_id.map(str::to_owned),
            status: status.into(),
            started_at: 1,
            finished_at,
            review_comment_url: None,
            error: None,
        }
    }

    #[test]
    fn cleanup_completed_pr_worktrees_removes_old_completed_worktree_but_keeps_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            worktree_cleanup_delay_seconds: 600,
            ..test_config(tmp.path())
        };
        let worktree = tmp
            .path()
            .join("worktrees")
            .join("owner__repo")
            .join("pr-7");
        fs::create_dir_all(worktree.join(".git")).unwrap();
        let memory = pr_memory_path(&config, "owner/repo", 7).unwrap();
        fs::create_dir_all(memory.parent().unwrap()).unwrap();
        fs::write(&memory, "keep me").unwrap();

        let mut state = State::default();
        state.pr_sessions.insert(
            "owner/repo#7".into(),
            pr_session("owner/repo", 7, "session-1", &worktree),
        );
        state.processed_comments.insert(
            "owner/repo#7:42".into(),
            processed_comment("owner/repo", 7, Some("session-1"), "commented", Some(1_000)),
        );

        let cleaned = cleanup_completed_pr_worktrees(&config, &mut state).unwrap();

        assert_eq!(cleaned, 1);
        assert!(!worktree.exists());
        assert!(memory.exists(), "PR memory must survive worktree cleanup");
        assert!(
            state.pr_sessions.contains_key("owner/repo#7"),
            "session memory must survive worktree cleanup"
        );
    }

    #[test]
    fn cleanup_completed_pr_worktrees_keeps_recent_or_running_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            worktree_cleanup_delay_seconds: 600,
            ..test_config(tmp.path())
        };
        let recent = tmp
            .path()
            .join("worktrees")
            .join("owner__repo")
            .join("pr-1");
        let running = tmp
            .path()
            .join("worktrees")
            .join("owner__repo")
            .join("pr-2");
        fs::create_dir_all(&recent).unwrap();
        fs::create_dir_all(&running).unwrap();

        let mut state = State::default();
        state.pr_sessions.insert(
            "owner/repo#1".into(),
            pr_session("owner/repo", 1, "recent-session", &recent),
        );
        state.pr_sessions.insert(
            "owner/repo#2".into(),
            pr_session("owner/repo", 2, "running-session", &running),
        );
        let now = now_epoch();
        state.processed_comments.insert(
            "owner/repo#1:1".into(),
            processed_comment(
                "owner/repo",
                1,
                Some("recent-session"),
                "commented",
                Some(now),
            ),
        );
        state.processed_comments.insert(
            "owner/repo#2:2".into(),
            processed_comment("owner/repo", 2, Some("running-session"), "running", None),
        );

        let cleaned = cleanup_completed_pr_worktrees(&config, &mut state).unwrap();

        assert_eq!(cleaned, 0);
        assert!(recent.exists());
        assert!(running.exists());
        assert!(state.pr_sessions.contains_key("owner/repo#1"));
        assert!(state.pr_sessions.contains_key("owner/repo#2"));
    }

    #[test]
    fn cleanup_completed_pr_sessions_removes_old_session_after_session_delay() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            session_cleanup_delay_seconds: 3 * 24 * 60 * 60,
            ..test_config(tmp.path())
        };
        let worktree = tmp
            .path()
            .join("worktrees")
            .join("owner__repo")
            .join("pr-7");
        fs::create_dir_all(&worktree).unwrap();
        let memory = pr_memory_path(&config, "owner/repo", 7).unwrap();
        fs::create_dir_all(memory.parent().unwrap()).unwrap();
        fs::write(&memory, "keep me").unwrap();

        let mut state = State::default();
        state.pr_sessions.insert(
            "owner/repo#7".into(),
            pr_session("owner/repo", 7, "session-1", &worktree),
        );
        state.processed_comments.insert(
            "owner/repo#7:42".into(),
            processed_comment("owner/repo", 7, Some("session-1"), "commented", Some(1_000)),
        );

        let cleaned = cleanup_completed_pr_sessions_without_server(&config, &mut state).unwrap();

        assert_eq!(cleaned, 1);
        assert!(!worktree.exists());
        assert!(memory.exists(), "PR memory must survive session cleanup");
        assert!(!state.pr_sessions.contains_key("owner/repo#7"));
    }

    #[test]
    fn cleanup_completed_pr_sessions_keeps_tracking_when_session_delete_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            session_cleanup_delay_seconds: 3 * 24 * 60 * 60,
            ..test_config(tmp.path())
        };
        let worktree = tmp
            .path()
            .join("worktrees")
            .join("owner__repo")
            .join("pr-7");
        fs::create_dir_all(&worktree).unwrap();

        let mut state = State::default();
        state.pr_sessions.insert(
            "owner/repo#7".into(),
            pr_session("owner/repo", 7, "session-1", &worktree),
        );
        state.processed_comments.insert(
            "owner/repo#7:42".into(),
            processed_comment("owner/repo", 7, Some("session-1"), "commented", Some(1_000)),
        );

        let cleaned = cleanup_completed_pr_sessions_after_delete_result(
            &config,
            &mut state,
            Err(anyhow::anyhow!("connection refused")),
        )
        .unwrap();

        assert_eq!(cleaned, 0);
        assert!(worktree.exists());
        assert!(state.pr_sessions.contains_key("owner/repo#7"));
    }

    #[test]
    fn webhook_signature_validation_accepts_only_matching_hmac() {
        let secret = "top-secret";
        let body = br#"{"zen":"Keep it logically awesome."}"#;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = format!("sha256={}", to_hex(&mac.finalize().into_bytes()));

        assert!(verify_webhook_signature(secret, body, &signature));
        assert!(!verify_webhook_signature("wrong", body, &signature));
        assert!(!verify_webhook_signature(secret, body, "sha1=bad"));
    }

    #[test]
    fn pull_request_webhook_opened_enqueues_review_event() {
        let config = Config {
            repos: vec!["owner/repo".into()],
            ..Config::default()
        };
        let mut state = State::default();
        state.webhook_deliveries.insert(
            "d1".into(),
            WebhookDelivery {
                delivery_id: "d1".into(),
                event: "pull_request".into(),
                action: Some("opened".into()),
                status: STATUS_PENDING.into(),
                received_at: 1,
                processed_at: None,
                message: None,
            },
        );
        let payload = json!({
            "repository": {"full_name": "owner/repo"},
            "pull_request": {
                "number": 7,
                "title": "Improve storage",
                "html_url": "https://github.test/owner/repo/pull/7",
                "body": "body",
                "head": {"sha": "head123"},
                "base": {"ref": "main"}
            }
        });

        let queued = enqueue_pull_request_webhook(
            &config,
            &mut state,
            "d1",
            "pull_request",
            "opened",
            &payload,
        )
        .unwrap();

        assert!(queued);
        assert_eq!(state.event_queue.len(), 1);
        assert_eq!(state.event_queue[0].repo, "owner/repo");
        assert_eq!(state.webhook_deliveries["d1"].status, STATUS_COMMENTED);
    }

    #[test]
    fn severity_gate_keeps_p3_in_summary_unless_nitpick_requested() {
        let config = Config::default();
        let mut p3 = validated_finding("P3", "Add edge case test", 10);
        p3.category = "Tests/API Contract".into();
        p3.issue = "The changed branch lacks a regression test.".into();
        p3.impact = "A later regression may slip.".into();
        p3.fix = "Add a focused test.".into();
        let mut inline = vec![p3.clone()];
        let mut unplaced = Vec::new();
        apply_severity_gate(
            &config,
            &auto_trigger("VitaDynamics/Vvbot", 7),
            &mut inline,
            &mut unplaced,
        );
        assert!(inline.is_empty());
        assert_eq!(unplaced.len(), 1);

        let mut inline = vec![p3];
        let mut unplaced = Vec::new();
        let nitpick = ReviewTrigger {
            repo: "VitaDynamics/Vvbot".into(),
            pr: pr(),
            kind: ReviewTriggerKind::MentionComment(comment(
                42,
                "@whatevertogo nitpick style review",
            )),
        };
        apply_severity_gate(&config, &nitpick, &mut inline, &mut unplaced);
        assert_eq!(inline.len(), 1);
        assert!(unplaced.is_empty());
    }

    #[test]
    fn instruction_context_loads_repo_and_matching_path_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            instruction_context_max_bytes: 10_000,
            ..test_config(tmp.path())
        };
        fs::create_dir_all(tmp.path().join(".github/instructions")).unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "Prefer maintainable code.").unwrap();
        fs::write(
            tmp.path().join(".github/instructions/rust.instructions.md"),
            "applyTo: **/*.rs\nCheck Rust ownership.",
        )
        .unwrap();
        fs::write(
            tmp.path().join(".github/instructions/docs.instructions.md"),
            "applyTo: docs/**\nCheck docs.",
        )
        .unwrap();

        let text =
            instruction_context_for_paths(&config, tmp.path(), &[String::from("src/lib.rs")])
                .unwrap();

        assert!(text.contains("Prefer maintainable code."));
        assert!(text.contains("Check Rust ownership."));
        assert!(!text.contains("Check docs."));
    }

    #[test]
    fn invalid_inline_line_falls_back_to_nearest_right_hunk() {
        let config = Config::default();
        let context = test_review_context();
        let output = ReviewBotOutput {
            confirmed_findings: vec![review_finding("P1", "Nearby issue", 12)],
            ..ReviewBotOutput::default()
        };

        let validated = validate_review_output(&config, &output, &context);

        assert_eq!(validated.inline_findings.len(), 1);
        assert_eq!(validated.inline_findings[0].line, 10);
        assert!(validated.unplaced_findings.is_empty());
    }

    #[test]
    fn pr_memory_suppresses_repeated_posted_findings() {
        let finding = validated_finding("P1", "Persist storage before returning", 10);
        let mut validated = ValidatedReview {
            global_review_complete: false,
            candidate_checks: Vec::new(),
            inline_findings: vec![finding.clone()],
            summary_findings: Vec::new(),
            unplaced_findings: Vec::new(),
            observations: Vec::new(),
            investigation_log: Vec::new(),
            verification: Vec::new(),
            residual_risk: Vec::new(),
            summary: None,
            coverage: None,
            debug_dir: None,
        };
        let trigger = auto_trigger("VitaDynamics/Vvbot", 7);
        let mut state = State::default();
        let mut memory = PrReviewMemory::default();
        let memory_finding = finding_memory_from_validated(&finding, "abc123");
        memory
            .posted_findings
            .insert(memory_finding.fingerprint.clone(), memory_finding);
        state
            .pr_review_memory
            .insert(pr_key(&trigger.repo, trigger.pr.number), memory);

        suppress_repeated_findings(&state, &trigger, &mut validated);

        assert!(validated.inline_findings.is_empty());
        assert_eq!(validated.unplaced_findings.len(), 1);
        assert!(validated.unplaced_findings[0]
            .reason
            .contains("already posted"));
    }

    #[test]
    fn pr_review_memory_records_summary_findings_and_observations() {
        let trigger = auto_trigger("VitaDynamics/Vvbot", 7);
        let mut validated = ValidatedReview {
            global_review_complete: false,
            candidate_checks: Vec::new(),
            inline_findings: Vec::new(),
            summary_findings: vec![validated_finding("P3", "Add a regression test", 10)],
            unplaced_findings: Vec::new(),
            observations: vec![ReviewObservation {
                confidence: Some("low".into()),
                category: Some("Tests/API Contract".into()),
                path: Some("src/storage.rs".into()),
                line: Some(10),
                title: Some("Related PR reminder".into()),
                evidence: Some("Related PR #620 changed this path.".into()),
                project_context: Some("Storage regressions have appeared before.".into()),
                impact: Some("A repeated regression could slip.".into()),
                next_step: Some("Compare with PR #620 before merging.".into()),
            }],
            investigation_log: Vec::new(),
            verification: Vec::new(),
            residual_risk: Vec::new(),
            summary: None,
            coverage: None,
            debug_dir: None,
        };
        let published = PublishedReview {
            url: Some("https://github.test/review".into()),
            inline_review_url: None,
            inline_review_id: None,
            summary_body: "summary".into(),
            inline_comments_posted: 0,
            unplaced_findings_count: 0,
            highest_risk: None,
            verification: Vec::new(),
            posted_findings: Vec::new(),
        };
        let mut state = State::default();

        update_pr_review_memory(&mut state, &trigger, "s1", Some(&validated), &published);

        let memory = state
            .pr_review_memory
            .get("VitaDynamics/Vvbot#7")
            .expect("memory entry");
        assert_eq!(memory.summary_findings.len(), 1);
        assert_eq!(memory.observations.len(), 1);
        assert_eq!(memory.observations[0].title, "Related PR reminder");
        assert_eq!(
            memory.observations[0].summary,
            "Compare with PR #620 before merging."
        );

        validated.unplaced_findings.push(UnplacedFinding {
            priority: "P3".into(),
            kind: "Advisory".into(),
            confidence: "medium".into(),
            title: "Could not place line".into(),
            path: Some("src/storage.rs".into()),
            side: Some("RIGHT".into()),
            line: Some(10),
            reason: "line was not commentable".into(),
        });
        update_pr_review_memory(&mut state, &trigger, "s1", Some(&validated), &published);
        let memory = state.pr_review_memory.get("VitaDynamics/Vvbot#7").unwrap();
        assert!(memory.summary_findings.len() >= 2);
    }

    #[test]
    fn final_report_sanitizer_keeps_markdown_and_removes_outer_metadata() {
        let report = sanitize_final_report(
            "<!-- astrcode-auto-review -->\n我是 whatevertogo 的替身。\nReview session: \
             `s1`\nHead SHA: `abc`\n# 代码审查\n\n## 发现\n\n- [P1] 有问题\n\n| 声明 | 结论 \
             |\n|---|---|\n| x | ✅ |",
        );

        assert!(report.starts_with("# 代码审查"));
        assert!(report.contains("- [P1] 有问题"));
        assert!(report.contains("| 声明 | 结论 |"));
        assert!(!report.contains("Review session"));
        assert!(!report.contains("Head SHA"));
    }

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
