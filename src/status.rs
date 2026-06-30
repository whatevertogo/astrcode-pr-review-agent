pub fn status_text(config: &Config) -> Result<String> {
    ensure_layout(config)?;
    let state = load_state()?;
    let processed = state.processed_comments.len();
    let auto_reviews = state.auto_pr_reviews.len();
    let seen_open_prs = state.seen_open_prs.len();
    let baselined_repos = state.auto_review_baselined_repos.len();
    let queued_events = state
        .event_queue
        .iter()
        .filter(|event| event.status == STATUS_PENDING)
        .count();
    let last_delivery = state
        .webhook_deliveries
        .values()
        .last()
        .map(|delivery| {
            format!(
                "{} {} {:?}: {}",
                delivery.delivery_id,
                delivery.event,
                delivery.action,
                delivery.status
            )
        })
        .unwrap_or_else(|| "none".into());
    let deterministic = if state.last_deterministic_checks.is_empty() {
        "none".into()
    } else {
        verification_summary(&state.last_deterministic_checks)
    };
    let running = state
        .processed_comments
        .values()
        .filter(|entry| entry.status == STATUS_RUNNING)
        .count()
        + state
            .auto_pr_reviews
            .values()
            .filter(|entry| entry.status == STATUS_RUNNING)
            .count();
    let pending = state
        .processed_comments
        .values()
        .filter(|entry| entry.status == STATUS_PENDING)
        .count()
        + state
            .auto_pr_reviews
            .values()
            .filter(|entry| entry.status == STATUS_PENDING)
            .count();
    let failed = state
        .processed_comments
        .values()
        .filter(|entry| entry.status == STATUS_FAILED)
        .count()
        + state
            .auto_pr_reviews
            .values()
            .filter(|entry| entry.status == STATUS_FAILED)
            .count();
    let last = state
        .last_run
        .map(|run| format!("{}: {}", run.status, run.message))
        .unwrap_or_else(|| "no runs yet".into());
    Ok(format!(
        "astrcode-pr-review-agent\nrepos: {}\nmention: {}\nwebhook: {} {}\nwebhook queue: \
         {queued_events}\nlast delivery: {last_delivery}\nprocessed comments: {processed}\nauto \
         PR reviews: {auto_reviews}\nseen open PRs: {seen_open_prs}\nauto baseline repos: \
         {baselined_repos}\ntracked PR sessions: {}\npending: {pending}\nrunning: \
         {running}\nfailed: {failed}\nlast run: {last}\nlast deterministic checks: \
         {deterministic}\nmemory: {}\nworktrees: {}",
        config.repos.join(", "),
        config.mention,
        if config.webhook_enabled {
            "enabled"
        } else {
            "disabled"
        },
        config.webhook_listen_addr,
        state.pr_sessions.len(),
        config.memory_dir_path()?.display(),
        config.worktree_dir_path()?.display(),
    ))
}

fn ensure_layout(config: &Config) -> Result<()> {
    fs::create_dir_all(agent_dir()?)?;
    fs::create_dir_all(config.memory_dir_path()?)?;
    fs::create_dir_all(config.worktree_dir_path()?)?;
    if !state_path()?.exists() {
        save_state(&State {
            version: STATE_VERSION,
            ..State::default()
        })?;
    }
    Ok(())
}

fn load_state() -> Result<State> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(State {
            version: STATE_VERSION,
            ..State::default()
        });
    }
    let raw = fs::read_to_string(&path)?;
    let mut state: State = match serde_json::from_str(&raw) {
        Ok(state) => state,
        Err(error) => {
            let backup = path.with_extension(format!("legacy-{}.json", now_epoch()));
            fs::write(&backup, raw)?;
            eprintln!(
                "ignored incompatible legacy state {}; backed up to {}: {error}",
                path.display(),
                backup.display()
            );
            State {
                version: STATE_VERSION,
                ..State::default()
            }
        },
    };
    if state.version == 0 {
        state.version = STATE_VERSION;
    }
    Ok(state)
}

fn save_state(state: &State) -> Result<()> {
    let path = state_path()?;
    write_json_pretty(&path, state)
}

fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_string_pretty(value)? + "\n")?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn processed_key(repo: &str, pr_number: u64, comment_id: u64) -> String {
    format!("{}:{comment_id}", pr_key(repo, pr_number))
}

fn pr_key(repo: &str, pr_number: u64) -> String {
    format!("{repo}#{pr_number}")
}

fn pr_memory_path(config: &Config, repo: &str, pr_number: u64) -> Result<PathBuf> {
    Ok(config
        .memory_dir_path()?
        .join("repos")
        .join(repo_key(repo))
        .join(format!("pr-{pr_number}.md")))
}

fn repo_memory_index_path(config: &Config, repo: &str) -> Result<PathBuf> {
    Ok(config
        .memory_dir_path()?
        .join("repos")
        .join(repo_key(repo))
        .join("index.md"))
}

fn repo_related_memory_path(config: &Config, repo: &str) -> Result<PathBuf> {
    Ok(config
        .memory_dir_path()?
        .join("repos")
        .join(repo_key(repo))
        .join("related.md"))
}

fn repo_key(repo: &str) -> String {
    repo.replace('/', "__")
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn expand_home(input: &str) -> Result<PathBuf> {
    if let Some(rest) = input.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }
    Ok(PathBuf::from(input))
}

fn agent_dir() -> Result<PathBuf> {
    Ok(astrcode_dir()?.join("pr-review-agent"))
}

fn astrcode_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".astrcode"))
}

fn state_path() -> Result<PathBuf> {
    Ok(agent_dir()?.join("state.json"))
}

fn webhook_spool_path() -> Result<PathBuf> {
    Ok(agent_dir()?.join("webhook-events.jsonl"))
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
