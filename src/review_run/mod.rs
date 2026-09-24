//! A replayable review run. Analysis does not publish to GitHub; publication is explicit.
use super::*;
use sha2::Digest;

mod context;
mod github;
mod pipeline;
mod report;
mod usage;
mod validation;

#[cfg(test)]
mod tests;

use context::ReviewSnapshot;
use usage::StageReceipt;
const RESULT_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRunResult {
    schema_version: u32,
    input_key: String,
    repo: String,
    pr_number: u64,
    base_sha: String,
    head_sha: String,
    model: Value,
    pipeline: String,
    status: String,
    #[serde(default)]
    retryable_failures: bool,
    review: ValidatedReview,
    stages: Vec<StageReceipt>,
    #[serde(default)]
    publication: github::PublicationReceipt,
}

/// Render saved analysis with the current presentation, without model or GitHub calls.
pub fn render_review_result(json: &str) -> Result<String> {
    let result: ReviewRunResult =
        serde_json::from_str(json).context("parse saved review result")?;
    Ok(report::render(&result))
}

#[derive(Default)]
struct Options {
    repo: String,
    pr: u64,
    output: PathBuf,
    snapshot: Option<PathBuf>,
    head: Option<String>,
    baseline: bool,
    publish: bool,
    prepare_only: bool,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--publish" => options.publish = true,
                "--prepare-only" => options.prepare_only = true,
                "--repo" => options.repo = args.next().context("--repo requires OWNER/REPO")?,
                "--pr" => options.pr = args.next().context("--pr requires a number")?.parse()?,
                "--head" => {
                    options.head = Some(args.next().context("--head requires a full commit SHA")?)
                }
                "--output-dir" => {
                    options.output = args.next().context("--output-dir requires a path")?.into()
                }
                "--snapshot" => {
                    options.snapshot =
                        Some(args.next().context("--snapshot requires a path")?.into())
                }
                "--pipeline" => {
                    options.baseline = match args.next().as_deref() {
                        Some("baseline") => true,
                        Some("quality_first") => false,
                        _ => anyhow::bail!("--pipeline must be baseline or quality_first"),
                    };
                }
                _ => anyhow::bail!("unknown review argument: {arg}"),
            }
        }
        let parts: Vec<_> = options.repo.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || !part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            })
            || options.pr == 0
            || options.output.as_os_str().is_empty()
        {
            anyhow::bail!("review requires --repo OWNER/REPO --pr NUMBER --output-dir PATH");
        }
        anyhow::ensure!(
            !(options.baseline && options.publish),
            "baseline evaluation cannot publish"
        );
        anyhow::ensure!(
            !(options.prepare_only && options.publish),
            "snapshot preparation cannot publish"
        );
        if let Some(head) = &options.head {
            anyhow::ensure!(
                head.len() == 40 && head.chars().all(|c| c.is_ascii_hexdigit()),
                "--head requires a full 40-character commit SHA"
            );
        }
        Ok(options)
    }
}

fn digest(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("artifact path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    serde_json::from_reader(fs::File::open(path)?)
        .with_context(|| format!("read {}", path.display()))
}

fn model_identity(run_info: &RunInfo) -> Result<Value> {
    let current = curl_json(
        "GET",
        &format!("http://127.0.0.1:{}/api/models/current", run_info.port),
        None,
    )?;
    // Hash configuration to detect changed settings without persisting credentials.
    let config = fs::read(astrcode_dir()?.join("config.toml"))?;
    let instructions = fs::read(astrcode_dir()?.join("AGENTS.md")).unwrap_or_default();
    Ok(
        json!({"selection":current,"config_digest":digest(config),"host_instructions_digest":digest(instructions)}),
    )
}

fn input_key(snapshot: &ReviewSnapshot, config: &Config, model: &Value) -> Result<String> {
    let mut input = json!({
        "version":1,"snapshot":snapshot,"config":config,"model":model,
        "prompts":pipeline::PROMPT_VERSION,"protocol":PR_REVIEW_BOT_PROMPT,
        "file":FILE_REVIEW_PROMPT,"global":GLOBAL_REVIEW_PROMPT,
        "orientation":ORIENTATION_REVIEW_PROMPT,"few_shots":PR_REVIEW_FEW_SHOTS_PROMPT,
    });
    if config.review_pipeline == "coverage_first" {
        input["evaluation_rules"] = json!(pipeline::EVALUATION_RULES);
    }
    Ok(digest(serde_json::to_vec(&input)?))
}

pub async fn review_cli(args: Vec<String>) -> Result<()> {
    let mut options = Options::parse(args)?;
    fs::create_dir_all(&options.output)?;
    options.output = fs::canonicalize(&options.output)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(options.output.join("run.lock"))?;
    lock.try_lock_exclusive()
        .context("this output directory already has a running review")?;
    // Read the installed policy, then isolate ALL mutable review-agent state.
    let config_path = agent_dir()?.join("config.json");
    let mut config: Config = if config_path.exists() {
        read(&config_path)?
    } else {
        Config::default()
    };
    config.memory_dir = options.output.join("memory").to_string_lossy().into_owned();
    config.worktree_dir = options
        .output
        .join("worktrees")
        .to_string_lossy()
        .into_owned();
    if !options.baseline {
        config.review_pipeline = "quality_first".into();
        config.max_review_passes_per_pr = 16;
        config.max_files_per_shard = 4;
        config.review_shard_max_bytes = 60_000;
        config.max_inline_comments = 12;
        config.inline_priority_max = "P2".into();
        config.inline_confidence_min = "high".into();
        config.max_advisory_inline_comments = 0;
        config.max_p3_inline_comments = 0;
        config.json_repair_attempts = config.json_repair_attempts.min(1);
    } else {
        config.review_pipeline = "coverage_first".into();
    }
    let snapshot = context::prepare(&options, &config)?;
    save(&options.output.join("snapshot.json"), &snapshot)?;
    if options.prepare_only {
        return Ok(());
    }
    // Unlike the poller, an experiment never starts or restarts a service.
    let run_info: RunInfo = read(&astrcode_dir()?.join("run.json"))?;
    let model = model_identity(&run_info)?;
    let key = input_key(&snapshot, &config, &model)?;
    // Legacy stage labels alone are insufficient cache keys. Its evaluation state
    // also lives beneath the full input digest, never the resident agent's directory.
    let run_data_dir = options.output.join("agent-data").join(&key);
    let result_path = options.output.join("result.json");
    let cached: Option<ReviewRunResult> = if result_path.exists() {
        Some(read(&result_path)?)
    } else {
        None
    };
    if options.publish {
        context::assert_current(&snapshot)?;
        github::update_summary(
            &snapshot,
            "审查进行中",
            "正在核对固定版本的审查结果。",
            &options.output,
        )?;
    }
    let outcome = async {
        let mut result = match cached {
            Some(result) if result.schema_version==RESULT_SCHEMA_VERSION && result.input_key == key && !result.retryable_failures => result,
            _ => RUN_DATA_DIR.scope(run_data_dir,pipeline::analyze(&options, &config, &snapshot, &run_info, model, key)).await?,
        };
        save(&result_path, &result)?;
        fs::write(options.output.join("report.md"), report::render(&result))?;
        if options.publish {
            context::assert_current(&snapshot)?;
            github::publish(&snapshot, &mut result, &options.output)?;
            save(&result_path, &result)?;
            fs::write(options.output.join("report.md"), report::render(&result))?;
        }
        println!("{}", serde_json::to_string(&json!({"status":result.status,"result":result_path,"summary_url":result.publication.summary_url}))?);
        Ok::<_, anyhow::Error>(())
    }.await;
    if let Err(error) = &outcome {
        save(
            &options.output.join("failure.json"),
            &json!({"error":format!("{error:#}"),"at":now_epoch()}),
        )?;
        if options.publish {
            let _ = github::update_summary(
                &snapshot,
                "审查未完成",
                "本次审查或发布未完成；已有结果已保留，请查看运行记录。",
                &options.output,
            );
        }
    }
    outcome
}

/// Opt-in poller entry. Its run-local cache and worktree never alter the legacy
/// persistent session or staged files for the same PR.
pub(super) async fn poll_review(
    config: &Config,
    state: &mut State,
    host: &RunInfo,
    trigger: &ReviewTrigger,
) -> Result<ReviewRecord> {
    let identity: Value = gh_json(&[
        "pr",
        "view",
        &trigger.pr.number.to_string(),
        "--repo",
        &trigger.repo,
        "--json",
        "baseRefOid,headRefOid",
    ])?;
    let base = identity["baseRefOid"]
        .as_str()
        .context("missing base SHA")?;
    let head = identity["headRefOid"]
        .as_str()
        .context("missing head SHA")?;
    let output = config
        .memory_dir_path()?
        .join("quality-runs")
        .join(repo_key(&trigger.repo))
        .join(format!("pr-{}-{head}-{base}", trigger.pr.number));
    fs::create_dir_all(&output)?;
    let options = Options {
        repo: trigger.repo.clone(),
        pr: trigger.pr.number,
        output: output.clone(),
        ..Options::default()
    };
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(output.join("run.lock"))?;
    lock.try_lock_exclusive()
        .context("review already running for this PR revision")?;
    let mut isolated = config.clone();
    isolated.worktree_dir = output.join("worktrees").to_string_lossy().into_owned();
    isolated.json_repair_attempts = isolated.json_repair_attempts.min(1);
    let mut snapshot = context::prepare(&options, &isolated)?;
    snapshot.request = trigger_instruction(trigger);
    save(&output.join("snapshot.json"), &snapshot)?;
    let model = model_identity(host)?;
    let key = input_key(&snapshot, &isolated, &model)?;
    let path = output.join("result.json");
    let cached: Option<ReviewRunResult> = if path.exists() {
        Some(read(&path)?)
    } else {
        None
    };
    let mut result = match cached {
        Some(result)
            if result.schema_version == RESULT_SCHEMA_VERSION
                && result.input_key == key
                && !result.retryable_failures =>
        {
            result
        }
        _ => pipeline::analyze(&options, &isolated, &snapshot, host, model, key).await?,
    };
    save(&path, &result)?;
    github::publish(&snapshot, &mut result, &output)?;
    save(&path, &result)?;
    let published = PublishedReview {
        url: result.publication.summary_url.clone(),
        inline_review_url: result.publication.review_url.clone(),
        inline_review_id: None,
        summary_body: report::render(&result),
        inline_comments_posted: result.publication.comments.len(),
        unplaced_findings_count: result.review.observations.len()
            + result.review.summary_findings.len(),
        highest_risk: result
            .review
            .inline_findings
            .first()
            .map(|f| f.priority.clone()),
        verification: result.review.verification.clone(),
        posted_findings: result
            .review
            .inline_findings
            .iter()
            .map(|f| finding_memory_from_validated(f, head))
            .collect(),
    };
    let session = result
        .stages
        .last()
        .map(|s| s.session_id.clone())
        .unwrap_or_default();
    let mut current_trigger = trigger.clone();
    current_trigger.pr = snapshot.pr;
    update_pr_review_memory(
        state,
        &current_trigger,
        &session,
        Some(&result.review),
        &published,
    );
    state.last_deterministic_checks = published.verification.clone();
    Ok(ReviewRecord {
        repo: trigger.repo.clone(),
        pr_number: trigger.pr.number,
        pr_title: current_trigger.pr.title.clone(),
        pr_url: current_trigger.pr.url.clone(),
        head_sha: head.into(),
        trigger_kind: trigger.trigger_kind_name().into(),
        trigger_comment_id: trigger.comment().map(|c| c.id),
        trigger_comment_url: trigger.comment().and_then(|c| c.html_url.clone()),
        trigger_author: trigger
            .comment()
            .and_then(|c| c.user.as_ref())
            .map(|u| u.login.clone())
            .unwrap_or_else(|| "auto:new-pr".into()),
        session_id: session,
        review_comment_url: published.url,
        related_context: None,
        inline_comments_posted: published.inline_comments_posted,
        unplaced_findings: published.unplaced_findings_count,
        highest_risk: published.highest_risk,
        summary: published.summary_body,
        created_at: now_epoch(),
    })
}

pub(super) fn status_comment(
    config: &Config,
    trigger: &ReviewTrigger,
    failed: bool,
) -> Result<Option<String>> {
    let snapshot = ReviewSnapshot {
        repo: trigger.repo.clone(),
        pr: trigger.pr.clone(),
        base_sha: String::new(),
        context: ReviewContext {
            text: String::new(),
            files: Vec::new(),
            commentable_lines: BTreeSet::new(),
            non_commentable_files: Vec::new(),
            truncated: false,
        },
        checks: Vec::new(),
        instructions: BTreeMap::new(),
        request: String::new(),
    };
    let output = config
        .memory_dir_path()?
        .join("quality-runs")
        .join(repo_key(&trigger.repo))
        .join(format!("pr-{}-status", trigger.pr.number));
    let receipt = github::update_summary(
        &snapshot,
        if failed {
            "审查未完成"
        } else {
            "审查进行中"
        },
        if failed {
            "本轮未完成，已有结果已保留。"
        } else {
            "正在审查固定版本的代码。"
        },
        &output,
    )?;
    Ok(receipt.summary_url)
}
