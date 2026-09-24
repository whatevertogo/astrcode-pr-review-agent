use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Source {
    Repo(String),
    Activity(String),
    Search,
}
impl Source {
    fn name(&self) -> String {
        match self {
            Self::Repo(r) => format!("repo:{r}"),
            Self::Activity(a) => format!("activity:{a}"),
            Self::Search => "search".into(),
        }
    }
    fn interval(&self) -> u64 {
        if matches!(self, Self::Search) {
            120
        } else {
            60
        }
    }
    fn path(&self) -> Result<PathBuf> {
        let key = format!(
            "{:x}",
            <Sha256 as sha2::Digest>::digest(self.name().to_ascii_lowercase().as_bytes())
        );
        Ok(agent_dir()?
            .join("mention-discovery")
            .join(format!("{key}.json")))
    }
}
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct CachedPage {
    etag: Option<String>,
    body: Value,
    next: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Progress {
    source: String,
    since: u64,
    last_success: Option<u64>,
    next_scan: u64,
    min_interval: u64,
    failures: u32,
    error: Option<String>,
    warnings: Vec<String>,
    pages: BTreeMap<u32, CachedPage>,
    seen: BTreeSet<String>,
}
impl Progress {
    fn new(source: &Source) -> Self {
        Self {
            source: source.name(),
            since: now_epoch().saturating_sub(86400),
            last_success: None,
            next_scan: 0,
            min_interval: source.interval(),
            failures: 0,
            error: None,
            warnings: Vec::new(),
            pages: BTreeMap::new(),
            seen: BTreeSet::new(),
        }
    }
}
trait Transport {
    fn get(&mut self, endpoint: &str, etag: Option<&str>) -> Result<github::Reply>;
    fn receive(&mut self, target: &Target, source: &str) -> Result<()>;
}
struct Live<'a>(&'a Config);
impl Transport for Live<'_> {
    fn get(&mut self, endpoint: &str, etag: Option<&str>) -> Result<github::Reply> {
        github::get(endpoint, etag)
    }
    fn receive(&mut self, target: &Target, source: &str) -> Result<()> {
        // Known IDs need no revalidation during overlap scans; the executor validates
        // pending tasks before running, and the explicit CLI still checks live permission.
        if existing(&read_state()?, target).is_none() && !inbox::contains(target)? {
            receive(self.0, target, source, false)?;
        }
        Ok(())
    }
}
fn sources(config: &Config) -> Vec<Source> {
    let mut sources = mention_repos(config)
        .iter()
        .cloned()
        .map(Source::Repo)
        .collect::<Vec<_>>();
    sources.extend(
        config
            .trusted_comment_authors
            .iter()
            .cloned()
            .map(Source::Activity),
    );
    sources.push(Source::Search);
    sources
}
fn candidate(config: &Config, comment: &Value) -> Option<Target> {
    let target = Target::parse(comment["html_url"].as_str()?).ok()?;
    let parsed: IssueComment = serde_json::from_value(comment.clone()).ok()?;
    is_trigger_comment(config, &target.repo, &parsed).then_some(target)
}
fn scan_comments(
    api: &mut impl Transport,
    config: &Config,
    endpoint: &str,
    source: &str,
) -> Result<()> {
    for page in 1.. {
        let reply = api.get(&format!("{endpoint}&page={page}"), None)?;
        for comment in reply.body.as_array().context("expected comments array")? {
            if let Some(target) = candidate(config, comment) {
                api.receive(&target, source)?;
            }
        }
        if !github::has_next(&reply) {
            return Ok(());
        }
    }
    unreachable!()
}
fn scan(
    api: &mut impl Transport,
    config: &Config,
    source: &Source,
    progress: &mut Progress,
) -> Result<()> {
    progress.warnings.clear();
    match source {
        Source::Repo(repo) => {
            let started = now_epoch();
            let since = github::timestamp(progress.since.saturating_sub(
                if progress.last_success.is_some() {
                    600
                } else {
                    0
                },
            ))?;
            let endpoint=format!("repos/{repo}/issues/comments?sort=updated&direction=asc&since={since}&per_page=100");
            scan_comments(api, config, &endpoint, &source.name())?;
            progress.since = started;
        }
        Source::Activity(author) => {
            progress.warnings.push("GitHub activity may lag 30s–6h; only 30 days/300 events are available; other users' private activity may be invisible".into());
            let mut seen = BTreeSet::new();
            let mut total = 0;
            for page in 1..=3 {
                let cached = progress.pages.get(&page);
                let reply = api.get(
                    &format!("users/{author}/events?per_page=100&page={page}"),
                    cached.and_then(|p| p.etag.as_deref()),
                )?;
                if let Some(min) = reply
                    .headers
                    .get("x-poll-interval")
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    progress.min_interval = progress.min_interval.max(min);
                }
                let data = if reply.status == 304 {
                    cached.context("304 without cached page")?.clone()
                } else {
                    CachedPage {
                        etag: reply.headers.get("etag").cloned(),
                        next: github::has_next(&reply),
                        body: reply.body,
                    }
                };
                for event in data.body.as_array().context("expected events array")? {
                    total += 1;
                    let id = event["id"].as_str().context("event has no ID")?.to_owned();
                    seen.insert(id.clone());
                    if progress.seen.contains(&id) {
                        continue;
                    }
                    let created = github::parse_timestamp(
                        event["created_at"]
                            .as_str()
                            .context("event timestamp missing")?,
                    )?;
                    // GitHub's event order is not chronological; never stop at an old event.
                    if created < progress.since
                        || event["type"] != "IssueCommentEvent"
                        || event["payload"]["issue"]["pull_request"].is_null()
                    {
                        continue;
                    }
                    if let Some(target) = candidate(config, &event["payload"]["comment"]) {
                        api.receive(&target, &source.name())?;
                    }
                }
                let next = data.next;
                progress.pages.insert(page, data);
                if !next {
                    progress.pages.retain(|p, _| *p <= page);
                    break;
                }
            }
            if total >= 300 {
                progress.warnings.push("activity window reached 300 events; older events may be missing; use enqueue --comment-url".into());
            }
            if !progress.seen.is_empty() && !seen.is_empty() && progress.seen.is_disjoint(&seen) {
                progress.warnings.push("activity window no longer overlaps the previous scan; a discovery gap is possible".into());
            }
            progress.seen = seen;
        }
        Source::Search => {
            let login = mention_login(config);
            anyhow::ensure!(
                login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "invalid mention login"
            );
            let since = github::timestamp(progress.since)?;
            for page in 1..=10 {
                let endpoint = format!(
                    "search/issues?q=is%3Apr+is%3Aopen+mentions%3A{login}&per_page=100&page={page}"
                );
                let reply = api.get(&endpoint, None)?;
                if reply.body["incomplete_results"].as_bool() == Some(true) {
                    progress
                        .warnings
                        .push("search returned incomplete_results; other channels continue".into());
                }
                if reply.body["total_count"].as_u64().unwrap_or(0) > 1000 {
                    progress
                        .warnings
                        .push("search exceeds GitHub's 1000-result window".into());
                }
                for pr in reply.body["items"]
                    .as_array()
                    .context("search items missing")?
                {
                    let repo = pr["repository_url"]
                        .as_str()
                        .and_then(|s| s.strip_prefix("https://api.github.com/repos/"))
                        .context("search repository URL missing")?;
                    let number = pr["number"].as_u64().context("search PR number missing")?;
                    scan_comments(
                        api,
                        config,
                        &format!(
                            "repos/{repo}/issues/{number}/comments?since={since}&per_page=100"
                        ),
                        &source.name(),
                    )?;
                }
                if !github::has_next(&reply) {
                    break;
                }
                if page == 10 {
                    progress
                        .warnings
                        .push("search pagination truncated at 1000 results".into());
                }
            }
        }
    }
    Ok(())
}
fn tick(config: &Config, source: &Source) -> Result<()> {
    let path = source.path()?;
    fs::create_dir_all(path.parent().context("source path")?)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }
    let mut progress: Progress = if path.exists() {
        serde_json::from_slice(&fs::read(&path)?)?
    } else {
        Progress::new(source)
    };
    if progress.next_scan > now_epoch() {
        return Ok(());
    }
    let mut next = progress.clone();
    match scan(&mut Live(config), config, source, &mut next) {
        Ok(()) => {
            next.last_success = Some(now_epoch());
            next.failures = 0;
            next.error = None;
            next.next_scan = now_epoch() + next.min_interval;
            progress = next;
        }
        Err(error) => {
            progress.min_interval = progress.min_interval.max(next.min_interval);
            progress.failures += 1;
            progress.error = Some(format!("{error:#}"));
            progress.next_scan = github::retry_at(&error, progress.failures)
                .max(now_epoch() + progress.min_interval);
        }
    }
    write_json_pretty(&path, &progress)
}
/// One bounded worker per configured source. API stalls and backoff on one source cannot
/// block priority-repository receipt or the existing serial model executor.
pub(crate) struct Discovery {
    stop: Arc<AtomicBool>,
}
impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
pub(crate) fn start_discovery(config: &Config) -> Discovery {
    let stop = Arc::new(AtomicBool::new(false));
    for source in sources(config) {
        let config = config.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Err(e) = tick(&config, &source) {
                    eprintln!("discovery {}: {e:#}", source.name());
                }
                for _ in 0..5 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        });
    }
    Discovery { stop }
}
pub fn discover_once(config: &Config) -> Result<()> {
    let mut errors = Vec::new();
    for source in sources(config) {
        if let Err(e) = tick(config, &source) {
            errors.push(format!("{}: {e:#}", source.name()));
        }
    }
    anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
    Ok(())
}
pub(super) fn status() -> Result<String> {
    let mut lines = vec!["mention discovery (timestamps are Unix seconds):".into()];
    for path in inbox::files(&agent_dir()?.join("mention-discovery"))? {
        let p: Progress = serde_json::from_slice(&fs::read(path)?)?;
        lines.push(format!(
            "{}: last_success={:?} next_scan={} failures={} error={:?} warnings={:?}",
            p.source, p.last_success, p.next_scan, p.failures, p.error, p.warnings
        ));
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
