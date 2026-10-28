//! Freeze the fetched revision once; never resolve an ambiguous local branch for review.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReviewRevision {
    pub base_sha: String,
    pub head_sha: String,
    pub merge_base_sha: String,
}

impl ReviewRevision {
    pub fn resolve(worktree: &Path, pr: &PullRequest, base_sha: Option<&str>) -> Result<Self> {
        let resolve = |reference: &str| -> Result<String> {
            Ok(run_command(
                "git",
                &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
                Some(worktree),
            )?
            .trim()
            .to_owned())
        };
        let head_sha = resolve("HEAD")?;
        anyhow::ensure!(
            head_sha == pr.head_ref_oid,
            "review worktree HEAD differs from PR head"
        );
        let base_ref = format!("refs/remotes/origin/{}", pr.base_ref_name);
        let base_sha = match base_sha {
            Some(sha) => {
                anyhow::ensure!(
                    sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid PR base SHA"
                );
                if resolve(sha).is_err() {
                    run_checkout_command(
                        "git",
                        &["fetch", "--no-tags", "origin", sha],
                        Some(worktree),
                    )?;
                }
                resolve(sha)?
            }
            None => {
                resolve(&base_ref).with_context(|| format!("resolve fetched PR base {base_ref}"))?
            }
        };
        let merge_base_sha =
            run_command("git", &["merge-base", &head_sha, &base_sha], Some(worktree))?
                .trim()
                .to_owned();
        Ok(Self {
            base_sha,
            head_sha,
            merge_base_sha,
        })
    }

    pub fn range(&self) -> String {
        format!("{}..{}", self.merge_base_sha, self.head_sha)
    }

    pub fn prompt(&self) -> String {
        format!("Frozen review revisions: base={} head={} merge_base={}\nUse `git diff {} -- <path>` and `git show <sha>:<path>` for related code. Do not use bare local branch names: shared-cache branches can be stale.\n",
            self.base_sha, self.head_sha, self.merge_base_sha, self.range())
    }

    pub fn matches_metadata(&self, pr: &PullRequest, metadata: &Value) -> bool {
        metadata["headRefOid"] == self.head_sha
            && metadata["baseRefOid"] == self.base_sha
            && metadata["baseRefName"] == pr.base_ref_name
    }
}

pub(super) fn validate_manifest(
    worktree: &Path,
    revision: &ReviewRevision,
    count: u64,
    files: &[PullRequestApiFile],
) -> Result<()> {
    let paths = run_command(
        "git",
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            &revision.range(),
            "--",
        ],
        Some(worktree),
    )?;
    let local: BTreeSet<_> = paths.split('\0').filter(|s| !s.is_empty()).collect();
    let names: BTreeSet<_> = files.iter().map(|f| f.filename.as_str()).collect();
    // Compare touched paths independent of GitHub/local rename similarity thresholds.
    let remote: BTreeSet<_> = files.iter().flat_map(|file| {
        std::iter::once(file.filename.as_str()).chain(
            file.previous_filename.as_deref().filter(|_| file.status.as_deref() == Some("renamed")))
    }).collect();
    anyhow::ensure!(files.len() == count as usize && names.len() == files.len() && remote == local,
        "PR file manifest incomplete or inconsistent: GitHub declared {count}, returned {}, local diff has {} files; missing={:?}; unexpected={:?}",
        files.len(), local.len(), local.difference(&remote).take(8).collect::<Vec<_>>(), remote.difference(&local).take(8).collect::<Vec<_>>());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacked_refs_stale_branches_and_complete_manifests() {
        let root = tempfile::tempdir().unwrap();
        let cwd = Some(root.path());
        let git = |args: &[&str]| run_command("git", args, cwd).unwrap().trim().to_owned();
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Review Fixture"]);
        git(&["config", "user.email", "review@example.invalid"]);
        fs::write(root.path().join("source.rs"), "before\n").unwrap();
        fs::write(root.path().join("old name.rs"), "unchanged rename\n").unwrap();
        git(&["add", "."]);
        git(&["-c", "commit.gpgsign=false", "commit", "-m", "ancestor"]);
        let ancestor = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-b", "topic"]);
        fs::write(root.path().join("source.rs"), "after\n").unwrap();
        git(&["mv", "old name.rs", "new name.rs"]);
        git(&["add", "."]);
        git(&["-c", "commit.gpgsign=false", "commit", "-m", "PR"]);
        let head = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "main"]);
        fs::write(root.path().join("base-only.rs"), "not part of the PR\n").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "base advanced",
        ]);
        let base = git(&["rev-parse", "HEAD"]);
        git(&[
            "update-ref",
            "refs/remotes/origin/split/opik-event-contract",
            &base,
        ]);
        git(&["checkout", "--detach", &head]);
        let mut pr = crate::tests::pr();
        pr.base_ref_name = "split/opik-event-contract".into();
        pr.head_ref_oid = head.clone();
        for stale in [false, true] {
            if stale {
                git(&[
                    "update-ref",
                    "refs/heads/split/opik-event-contract",
                    &ancestor,
                ]);
            }
            let revision = ReviewRevision::resolve(root.path(), &pr, None).unwrap();
            assert_eq!(revision.base_sha, base);
            assert_eq!(revision.merge_base_sha, ancestor);
            assert_eq!(revision.head_sha, head);
            let file = |name: &str| PullRequestApiFile {
                filename: name.into(),
                status: None,
                additions: 0,
                deletions: 0,
                changes: 0,
                patch: None,
                previous_filename: None,
            };
            let mut renamed = file("new name.rs");
            renamed.status = Some("renamed".into());
            renamed.previous_filename = Some("old name.rs".into());
            let files = vec![file("source.rs"), renamed];
            validate_manifest(root.path(), &revision, 2, &files).unwrap();
            assert!(validate_manifest(root.path(), &revision, 3, &files).is_err());
            assert!(validate_manifest(root.path(), &revision, 1, &files[..1]).is_err());
            assert!(validate_manifest(
                root.path(),
                &revision,
                2,
                &[file("source.rs"), file("source.rs")]
            )
            .is_err());
            let mut metadata =
                json!({"baseRefName": pr.base_ref_name, "baseRefOid": base, "headRefOid": head});
            assert!(revision.matches_metadata(&pr, &metadata));
            metadata["baseRefName"] = json!("main");
            assert!(!revision.matches_metadata(&pr, &metadata));
            metadata["baseRefName"] = json!(pr.base_ref_name);
            metadata["headRefOid"] = json!(ancestor);
            assert!(!revision.matches_metadata(&pr, &metadata));
        }
        pr.head_ref_oid = ancestor;
        assert!(ReviewRevision::resolve(root.path(), &pr, None).is_err());
    }

    #[test]
    #[ignore = "requires remote incident worktrees, session logs and read-only gh access"]
    fn incident_preflight_from_real_artifacts() {
        let root =
            PathBuf::from(std::env::var("ASTRCODE_REVIEW_INCIDENT_ROOT").expect("incident root"));
        let config = Config::default();
        let mut pr = pr_details("VitaDynamics/Vvbot", 1300).unwrap();
        pr.head_ref_oid = "6e8b335318767b2840310241c553a6b973f56dcc".into();
        pr.base_ref_name = "split/opik-event-contract".into();
        let revision = ReviewRevision::resolve(
            &root.join("worktrees/VitaDynamics__Vvbot/pr-1300"),
            &pr,
            None,
        )
        .unwrap();
        assert_eq!(
            revision.base_sha,
            "026980517e7fd11b0aca92fe973b05c74594ab55"
        );
        let pr = pr_details("VitaDynamics/Vvbot", 1306).unwrap();
        let trigger = ReviewTrigger {
            repo: "VitaDynamics/Vvbot".into(),
            pr,
            kind: ReviewTriggerKind::NewPullRequest,
        };
        let large_pr_worktree = PathBuf::from(
            std::env::var("ASTRCODE_REVIEW_LARGE_PR_WORKTREE")
                .expect("isolated current #1306 worktree"),
        );
        let context = collect_review_context(&config, &trigger, &large_pr_worktree).unwrap();
        assert!(context.files.len() > 4);
        assert!(!context.text.split("PR metadata JSON:").next().unwrap().contains("gh pr diff"));
        println!(
            "#1300 stacked base resolved; #1306 validated {} changed files",
            context.files.len()
        );
        let response = fs::read_to_string(
            std::env::var("ASTRCODE_REVIEW_INCIDENT_RESPONSE").expect("response path"),
        )
        .unwrap();
        let output = parse_review_bot_output(&response).unwrap();
        review_global::validate(&output, &[]).unwrap();
        assert!(!output.investigation_log.is_empty());
        println!(
            "#1308 original global result recovered with {} evidence entries",
            output.investigation_log.len()
        );
    }
}
