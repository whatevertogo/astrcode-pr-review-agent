use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Receipt {
    target: Target,
    source: String,
    received_at: u64,
}

pub(super) fn atomic_create<T: Serialize>(path: &Path, value: &T) -> Result<bool> {
    let parent = path.parent().context("inbox path needs parent")?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut tmp, value)?;
    tmp.as_file().sync_all()?;
    match tmp.persist_noclobber(path) {
        Ok(_) => {
            fs::File::open(parent)?.sync_all()?;
            Ok(true)
        }
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn path(target: &Target) -> Result<PathBuf> {
    Ok(agent_dir()?
        .join("mention-inbox")
        .join(format!("{}.json", target.file_key())))
}
pub(super) fn contains(target: &Target) -> Result<bool> {
    Ok(path(target)?.exists())
}
pub(super) fn lookup(target: &Target) -> Result<Option<Receipt>> {
    match fs::read(path(target)?) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub(super) fn persist(target: &Target, source: &str) -> Result<bool> {
    atomic_create(
        &path(target)?,
        &Receipt {
            target: target.clone(),
            source: source.into(),
            received_at: now_epoch(),
        },
    )
}
pub(super) fn files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "json"));
    paths.sort();
    Ok(paths)
}

/// Called only under run.lock. Commit the task before removing its ingress receipt.
pub(crate) fn import_mentions(state: &mut State) -> Result<usize> {
    let mut count = 0;
    for path in files(&agent_dir()?.join("mention-inbox"))? {
        let receipt: Receipt = serde_json::from_slice(&fs::read(&path)?)?;
        if existing(state, &receipt.target).is_none() {
            let t = receipt.target;
            state.processed_comments.insert(
                t.key(),
                ProcessedComment {
                    repo: t.repo,
                    pr_number: t.pr,
                    comment_id: t.comment,
                    head_sha: String::new(),
                    session_id: None,
                    status: STATUS_PENDING.into(),
                    started_at: receipt.received_at,
                    finished_at: None,
                    review_comment_url: None,
                    error: None,
                },
            );
            count += 1;
        }
        save_state(state)?;
        fs::remove_file(path)?;
    }
    Ok(count)
}

#[derive(Default, Serialize, Deserialize)]
struct Retry {
    at: u64,
    failures: u32,
    error: Option<String>,
}

/// Recover pending tasks directly from durable state, without rediscovery. Validate only
/// when the executor is free, so the selected request is fresh immediately before execution.
pub(crate) fn next_pending(config: &Config, state: &mut State) -> Result<Option<ReviewTrigger>> {
    next_pending_with(state, |target| validate(config, target))
}
pub(super) fn next_pending_with(
    state: &mut State,
    mut validate: impl FnMut(&Target) -> Result<Validation>,
) -> Result<Option<ReviewTrigger>> {
    let mut pending = state
        .processed_comments
        .iter()
        .filter(|(_, r)| r.status == STATUS_PENDING)
        .map(|(k, r)| (k.clone(), r.clone()))
        .collect::<Vec<_>>();
    pending.sort_by_key(|(_, r)| r.started_at);
    for (key, r) in pending {
        let target = Target {
            repo: r.repo,
            pr: r.pr_number,
            comment: r.comment_id,
        };
        let path = agent_dir()?
            .join("mention-retries")
            .join(format!("{}.json", target.file_key()));
        let mut retry: Retry = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?)?
        } else {
            Retry::default()
        };
        if retry.at > now_epoch() {
            continue;
        }
        match validate(&target) {
            Ok(Validation::Accepted(mut verified)) => {
                // Preserve the historical ledger key even if GitHub canonicalizes repo casing.
                verified.trigger.repo = target.repo;
                return Ok(Some(verified.trigger));
            }
            Ok(Validation::Rejected(reason)) => {
                let record = state
                    .processed_comments
                    .get_mut(&key)
                    .context("pending record disappeared")?;
                record.status = STATUS_ABORTED.into();
                record.finished_at = Some(now_epoch());
                record.error = Some(reason);
                save_state(state)?;
            }
            Err(e) => {
                retry.failures += 1;
                retry.at = github::retry_at(&e, retry.failures);
                retry.error = Some(format!("{e:#}"));
                write_json_pretty(&path, &retry)?;
                state
                    .processed_comments
                    .get_mut(&key)
                    .context("pending record disappeared")?
                    .error = retry.error;
                save_state(state)?;
            }
        }
    }
    Ok(None)
}

pub(crate) fn persist_webhook(event: &str, delivery: &str, payload: &Value) -> Result<()> {
    let key = format!(
        "{:x}",
        <Sha256 as sha2::Digest>::digest(delivery.as_bytes())
    );
    atomic_create(
        &agent_dir()?
            .join("webhook-inbox")
            .join(format!("{key}.json")),
        &SpooledWebhookEvent {
            event: event.into(),
            delivery_id: delivery.into(),
            payload: payload.clone(),
            spooled_at: now_epoch(),
        },
    )?;
    Ok(())
}

/// Old workers are stopped before deployment. Claim legacy files before reading and keep
/// claimed files until every delivery has a durable replacement (including prior crashes).
pub(crate) fn migrate_webhook_spool() -> Result<()> {
    let dir = agent_dir()?;
    if !dir.exists() {
        return Ok(());
    }
    let old = webhook_spool_path()?;
    if old.exists() {
        let claim = tempfile::Builder::new()
            .prefix("webhook-events.imported-")
            .suffix(".jsonl")
            .tempfile_in(&dir)?;
        let (_, claim) = claim.keep()?;
        fs::rename(old, claim)?;
    }
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with("webhook-events.imported-") || !name.ends_with(".jsonl") {
            continue;
        }
        for line in fs::read_to_string(&path)?
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            let record: SpooledWebhookEvent = serde_json::from_str(line)
                .with_context(|| format!("invalid legacy spool {} (retained)", path.display()))?;
            persist_webhook(&record.event, &record.delivery_id, &record.payload)?;
        }
        fs::rename(&path, path.with_extension("migrated"))?;
    }
    Ok(())
}
pub(crate) fn import_webhooks(config: &Config, state: &mut State) -> Result<usize> {
    migrate_webhook_spool()?;
    let mut imported = 0;
    for path in files(&agent_dir()?.join("webhook-inbox"))? {
        let retry_path = agent_dir()?
            .join("webhook-retries")
            .join(path.file_name().context("webhook filename")?);
        let mut retry: Retry = if retry_path.exists() {
            serde_json::from_slice(&fs::read(&retry_path)?)?
        } else {
            Retry::default()
        };
        if retry.at > now_epoch() {
            continue;
        }
        let record: SpooledWebhookEvent = serde_json::from_slice(&fs::read(&path)?)?;
        // Do not persist partially-mutated delivery state on an API failure.
        let mut next = state.clone();
        match enqueue_webhook_payload_into_state(
            config,
            &mut next,
            &record.event,
            &record.delivery_id,
            &record.payload,
        ) {
            Ok(_) => {
                save_state(&next)?;
                *state = next;
                fs::remove_file(path)?;
                if retry_path.exists() {
                    fs::remove_file(&retry_path)?;
                }
                imported += 1;
            }
            Err(e) => {
                retry.failures += 1;
                retry.at = github::retry_at(&e, retry.failures);
                retry.error = Some(format!("{e:#}"));
                write_json_pretty(&retry_path, &retry)?;
                eprintln!("webhook {} retained for retry: {e:#}", record.delivery_id);
            }
        }
    }
    Ok(imported)
}
pub(super) fn status() -> Result<String> {
    let mut oldest = None;
    let paths = files(&agent_dir()?.join("mention-inbox"))?;
    for path in &paths {
        let record: Receipt = serde_json::from_slice(&fs::read(path)?)?;
        oldest = Some(oldest.map_or(record.received_at, |t: u64| t.min(record.received_at)));
    }
    let mut errors = Vec::new();
    for path in files(&agent_dir()?.join("webhook-retries"))? {
        let retry: Retry = serde_json::from_slice(&fs::read(&path)?)?;
        errors.push(format!(
            "webhook retry: at={} failures={} error={:?}",
            retry.at, retry.failures, retry.error
        ));
    }
    Ok(format!(
        "mention inbox: {}; oldest received: {:?}; webhook inbox: {}\n{}",
        paths.len(),
        oldest,
        files(&agent_dir()?.join("webhook-inbox"))?.len(),
        errors.join("\n")
    ))
}
