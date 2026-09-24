//! Comment discovery is a producer; only the existing executor writes review state.
use super::*;

mod discovery;
mod github;
mod inbox;
#[cfg(test)]
mod tests;
pub use discovery::discover_once;
pub(crate) use discovery::start_discovery;
pub(crate) use inbox::{import_mentions, import_webhooks, next_pending, persist_webhook};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Target {
    pub repo: String,
    pub pr: u64,
    pub comment: u64,
}
impl Target {
    pub fn parse(url: &str) -> Result<Self> {
        let tail = url
            .strip_prefix("https://github.com/")
            .context("expected a github.com PR issuecomment URL")?;
        let (path, comment) = tail
            .split_once("#issuecomment-")
            .context("expected a PR discussion comment, not an inline review")?;
        let parts: Vec<_> = path.split('/').collect();
        anyhow::ensure!(
            parts.len() == 4 && parts[2] == "pull",
            "expected /OWNER/REPO/pull/NUMBER#issuecomment-ID"
        );
        anyhow::ensure!(
            parts[..2].iter().all(|s| !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))),
            "invalid repository"
        );
        let result = Self {
            repo: format!("{}/{}", parts[0], parts[1]),
            pr: parts[3].parse()?,
            comment: comment.parse()?,
        };
        anyhow::ensure!(
            result.pr > 0 && result.comment > 0,
            "invalid PR/comment number"
        );
        Ok(result)
    }
    pub fn key(&self) -> String {
        processed_key(&self.repo, self.pr, self.comment)
    }
    fn file_key(&self) -> String {
        format!(
            "{:x}",
            <Sha256 as sha2::Digest>::digest(self.key().to_ascii_lowercase().as_bytes())
        )
    }
}

pub(crate) fn mention_repos(config: &Config) -> &[String] {
    config.mention_repos.as_deref().unwrap_or(&config.repos)
}

pub(crate) fn matches_mention(body: &str, mention: &str) -> bool {
    let body = body.to_ascii_lowercase();
    let mention = mention.to_ascii_lowercase();
    if !mention.starts_with('@') || mention.len() < 2 {
        return false;
    }
    let username_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '@');
    body.match_indices(&mention).any(|(at, _)| {
        body[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !username_char(c))
            && body[at + mention.len()..]
                .chars()
                .next()
                .is_none_or(|c| !username_char(c))
    })
}

pub(crate) fn rejection(
    config: &Config,
    repo: &str,
    comment: &IssueComment,
) -> Option<&'static str> {
    let body = comment.body.as_deref().unwrap_or("");
    if [
        config.comment_marker.as_str(),
        "<!-- astrcode-review-summary:v2 -->",
        "<!-- astrcode-finding:v2:",
        AGENT_LINE,
        "我是 whatevertogo 的自动化审查 agent。",
    ]
    .iter()
    .filter(|m| !m.is_empty())
    .any(|m| body.contains(m))
    {
        return Some("agent_comment");
    }
    if !matches_mention(body, &config.mention) {
        return Some("mention_missing");
    }
    if comment.user.is_none() {
        return Some("comment_author_missing");
    }
    if !comment
        .user
        .as_ref()
        .is_some_and(|u| is_trusted_comment_author(config, &u.login))
        && !mention_repos(config)
            .iter()
            .any(|r| r.eq_ignore_ascii_case(repo))
    {
        return Some("author_not_allowed");
    }
    None
}

pub(crate) struct Verified {
    pub target: Target,
    pub trigger: ReviewTrigger,
}

pub(crate) enum Validation {
    Accepted(Box<Verified>),
    Rejected(String),
}

pub(crate) fn validate(config: &Config, target: &Target) -> Result<Validation> {
    validate_with(config, target, github::get)
}
fn validate_with(
    config: &Config,
    target: &Target,
    mut get: impl FnMut(&str, Option<&str>) -> Result<github::Reply>,
) -> Result<Validation> {
    let response = match get(
        &format!("repos/{}/issues/comments/{}", target.repo, target.comment),
        None,
    ) {
        Ok(reply) => reply.body,
        Err(error) if github::is_missing(&error) => {
            return Ok(Validation::Rejected(
                "comment_deleted_or_inaccessible".into(),
            ))
        }
        Err(error) => return Err(error),
    };
    let url = response["html_url"]
        .as_str()
        .context("comment has no canonical URL")?;
    let actual = match Target::parse(url) {
        Ok(target) => target,
        Err(_) => return Ok(Validation::Rejected("not_pr_discussion_comment".into())),
    };
    anyhow::ensure!(
        actual.repo.eq_ignore_ascii_case(&target.repo)
            && actual.pr == target.pr
            && actual.comment == target.comment,
        "comment URL does not belong to requested PR"
    );
    let comment: IssueComment = serde_json::from_value(response)?;
    if let Some(reason) = rejection(config, &actual.repo, &comment) {
        return Ok(Validation::Rejected(reason.into()));
    }
    let value = match get(&format!("repos/{}/pulls/{}", actual.repo, actual.pr), None) {
        Ok(reply) => reply.body,
        Err(e) if github::is_missing(&e) => {
            return Ok(Validation::Rejected("pr_deleted_or_inaccessible".into()))
        }
        Err(e) => return Err(e),
    };
    if value["state"] != "open" {
        return Ok(Validation::Rejected("pr_closed".into()));
    }
    let pr = PullRequest {
        number: actual.pr,
        title: value["title"].as_str().context("missing PR title")?.into(),
        url: value["html_url"].as_str().context("missing PR URL")?.into(),
        head_ref_oid: value["head"]["sha"]
            .as_str()
            .context("missing PR head")?
            .into(),
        base_ref_name: value["base"]["ref"]
            .as_str()
            .context("missing PR base")?
            .into(),
        body: value["body"].as_str().map(str::to_owned),
        files: Vec::new(),
        author: serde_json::from_value(value["user"].clone()).ok(),
    };
    Ok(Validation::Accepted(Box::new(Verified {
        target: actual.clone(),
        trigger: ReviewTrigger {
            repo: actual.repo,
            pr,
            kind: ReviewTriggerKind::MentionComment(comment),
        },
    })))
}

fn read_state() -> Result<State> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(State::default());
    }
    // Discovery must never turn a malformed state into an empty replay ledger.
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub(crate) fn existing<'a>(state: &'a State, target: &Target) -> Option<&'a ProcessedComment> {
    state.processed_comments.values().find(|r| {
        r.comment_id == target.comment
            && r.pr_number == target.pr
            && r.repo.eq_ignore_ascii_case(&target.repo)
    })
}

fn disposition(record: &ProcessedComment) -> &'static str {
    match record.status.as_str() {
        STATUS_PENDING => "already_pending",
        STATUS_RUNNING => "running",
        STATUS_FAILED => "failed",
        STATUS_ABORTED => "rejected",
        _ => "already_processed",
    }
}

pub(crate) fn receive(
    config: &Config,
    target: &Target,
    source: &str,
    dry_run: bool,
) -> Result<Value> {
    // Validate even manual requests for already-known records; CLI is never an authorization bypass.
    let verified = match validate(config, target)? {
        Validation::Accepted(value) => value,
        Validation::Rejected(reason) => return Ok(json!({"status":"rejected","reason":reason})),
    };
    if let Some(record) = existing(&read_state()?, &verified.target) {
        return Ok(
            json!({"status":disposition(record),"error":record.error,"reply":record.review_comment_url}),
        );
    }
    if inbox::contains(&verified.target)? {
        return Ok(json!({"status":"already_pending","stage":"received"}));
    }
    if dry_run {
        return Ok(json!({"status":"eligible","target":verified.target}));
    }
    let fresh = inbox::persist(&verified.target, source)?;
    if fresh {
        acknowledge_trigger(config, &verified.trigger);
    }
    Ok(
        json!({"status":if fresh {"queued"} else {"already_pending"},"stage":"received","target":verified.target}),
    )
}

fn arguments(args: Vec<String>, allow_dry_run: bool) -> Result<(Target, bool)> {
    let mut iter = args.into_iter();
    let mut url = None;
    let mut dry = false;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--comment-url" if url.is_none() => {
                url = Some(iter.next().context("--comment-url needs a URL")?)
            }
            "--dry-run" if allow_dry_run => dry = true,
            _ => anyhow::bail!("unknown/duplicate argument {arg}"),
        }
    }
    Ok((
        Target::parse(&url.context("--comment-url is required")?)?,
        dry,
    ))
}

pub fn enqueue_cli(args: Vec<String>) -> Result<()> {
    let (target, dry) = arguments(args, true)?;
    // Loading configuration must not create files during --dry-run.
    let path = agent_dir()?.join("config.json");
    let config = if path.exists() {
        serde_json::from_slice(&fs::read(path)?)?
    } else {
        Config::default()
    };
    println!("{}", receive(&config, &target, "cli", dry)?);
    Ok(())
}

pub fn mention_status_cli(args: Vec<String>) -> Result<()> {
    let (target, _) = arguments(args, false)?;
    let state = read_state()?;
    let response = if let Some(r) = existing(&state, &target) {
        json!({"status":r.status,"reply":r.review_comment_url,"error":r.error,"started_at":r.started_at,"finished_at":r.finished_at})
    } else if let Some(record) = inbox::lookup(&target)? {
        json!({"status":"received","record":record})
    } else {
        json!({"status":"not_discovered","target":target})
    };
    println!("{response}");
    Ok(())
}

pub(crate) fn status() -> Result<String> {
    Ok(format!("{}\n{}", inbox::status()?, discovery::status()?))
}
