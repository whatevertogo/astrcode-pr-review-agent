pub async fn poll_once(config: &Config) -> Result<()> {
    ensure_layout(config)?;
    let lock_path = agent_dir()?.join("run.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open lock {}", lock_path.display()))?;
    if lock.try_lock_exclusive().is_err() {
        eprintln!("another astrcode-pr-review-agent poll is active");
        return Ok(());
    }

    let mut state = load_state()?;
    import_spooled_webhook_events(config, &mut state)?;
    state.last_run = Some(RunStatus {
        started_at: now_epoch(),
        finished_at: None,
        status: "running".into(),
        message: "poll started".into(),
    });
    save_state(&state)?;

    let result = poll_inner(config, &mut state).await;
    state.last_run = Some(match &result {
        Ok(message) => RunStatus {
            started_at: state
                .last_run
                .as_ref()
                .map(|run| run.started_at)
                .unwrap_or_else(now_epoch),
            finished_at: Some(now_epoch()),
            status: "ok".into(),
            message: message.clone(),
        },
        Err(error) => RunStatus {
            started_at: state
                .last_run
                .as_ref()
                .map(|run| run.started_at)
                .unwrap_or_else(now_epoch),
            finished_at: Some(now_epoch()),
            status: "failed".into(),
            message: error.to_string(),
        },
    });
    save_state(&state)?;
    lock.unlock()?;
    result.map(|_| ())
}

pub async fn poll_forever(config: Config) {
    loop {
        if let Err(error) = poll_once(&config).await {
            eprintln!("astrcode-pr-review-agent poll failed: {error:#}");
        }
        tokio::time::sleep(Duration::from_secs(config.poll_interval_seconds.max(5))).await;
    }
}

async fn poll_inner(config: &Config, state: &mut State) -> Result<String> {
    ensure_gh_authenticated()?;
    let mut handled = 0usize;
    let mut queued = 0usize;
    let mut auto_queued = 0usize;
    let mut skipped = 0usize;
    let run_info = ensure_astrcode_run_info()?;
    let mut cleanup = cleanup_completed_pr_resources(config, state, Some(&run_info))?;
    let recovered = recover_stale_running_reviews(config, state)?;
    let mut pending_triggers = Vec::new();
    let webhook_queued = drain_webhook_events(config, state, &mut pending_triggers)?;
    queued += webhook_queued.mentions;
    auto_queued += webhook_queued.auto_reviews;
    skipped += webhook_queued.skipped;

    let mut baselined = 0usize;
    if should_run_reconciliation(config, state) {
        let mentioned_prs = mentioned_open_prs(config).unwrap_or_else(|error| {
            eprintln!(
                "failed to search globally mentioned PRs; skipping mention reconciliation this \
                 cycle: {error:#}"
            );
            Vec::new()
        });
        let open_prs = configured_open_prs(config)?;
        baselined = baseline_auto_review_repos(config, state, &open_prs)?;

        for (repo, pr) in mentioned_prs {
            let comments = match issue_comments_quick(&repo, pr.number) {
                Ok(comments) => comments,
                Err(error) => {
                    eprintln!(
                        "failed to read comments for {repo}#{}: {error:#}",
                        pr.number
                    );
                    skipped += 1;
                    continue;
                },
            };
            for comment in comments {
                if !is_trigger_comment(config, &repo, &comment) {
                    skipped += 1;
                    continue;
                }
                let key = processed_key(&repo, pr.number, comment.id);
                let trigger = ReviewTrigger {
                    repo: repo.clone(),
                    pr: pr.clone(),
                    kind: ReviewTriggerKind::MentionComment(comment),
                };
                match state
                    .processed_comments
                    .get(&key)
                    .map(|record| record.status.as_str())
                {
                    None => {
                        acknowledge_trigger(config, &trigger);
                        enqueue_mention_trigger(state, &trigger)?;
                        pending_triggers.push(trigger);
                        queued += 1;
                    },
                    Some(STATUS_PENDING) => pending_triggers.push(trigger),
                    Some(_) => {
                        skipped += 1;
                    },
                }
            }
        }

        for (repo, pr) in open_prs {
            if let Some(trigger) = enqueue_auto_review_trigger(config, state, &repo, &pr)? {
                pending_triggers.push(trigger);
                auto_queued += 1;
            }
        }
        state.last_reconciliation_at = Some(now_epoch());
    }

    sort_pending_triggers(state, &mut pending_triggers);
    if has_running_review(state) {
        cleanup += cleanup_completed_pr_resources(config, state, Some(&run_info))?;
        return Ok(format!(
            "handled 0 trigger(s), queued {queued} mention trigger(s), queued {auto_queued} auto \
             trigger(s), skipped {skipped} comment(s), baselined {baselined} repo(s), pending {} \
             trigger(s), running 1 trigger(s), recovered {recovered} stale review(s), cleaned {} \
             worktree(s), cleaned {} session(s)",
            pending_triggers.len(),
            cleanup.worktrees,
            cleanup.sessions
        ));
    }

    if let Some(trigger) = pending_triggers.into_iter().next() {
        process_trigger(config, state, &run_info, trigger).await?;
        handled += 1;
    }

    cleanup += cleanup_completed_pr_resources(config, state, Some(&run_info))?;

    Ok(format!(
        "handled {handled} trigger(s), queued {queued} mention trigger(s), queued {auto_queued} \
         auto trigger(s), skipped {skipped} comment(s), baselined {baselined} repo(s), recovered \
         {recovered} stale review(s), cleaned {} worktree(s), cleaned {} session(s)",
        cleanup.worktrees, cleanup.sessions
    ))
}

#[derive(Default)]
struct WebhookQueueDrain {
    mentions: usize,
    auto_reviews: usize,
    skipped: usize,
}

fn should_run_reconciliation(config: &Config, state: &State) -> bool {
    let _ = (config, state);
    true
}

fn drain_webhook_events(
    config: &Config,
    state: &mut State,
    pending_triggers: &mut Vec<ReviewTrigger>,
) -> Result<WebhookQueueDrain> {
    let mut counts = WebhookQueueDrain::default();
    let pending_events = state
        .event_queue
        .iter()
        .filter(|event| event.status == STATUS_PENDING)
        .cloned()
        .collect::<Vec<_>>();

    for event in pending_events {
        match queue_event_to_trigger(config, state, &event) {
            Ok(Some(trigger)) => {
                if trigger.is_auto_review() {
                    counts.auto_reviews += 1;
                } else {
                    counts.mentions += 1;
                }
                pending_triggers.push(trigger);
                mark_queue_event_done(state, &event.id, "queued review trigger".into());
            },
            Ok(None) => {
                counts.skipped += 1;
                mark_queue_event_done(state, &event.id, "event did not create review".into());
            },
            Err(error) => {
                counts.skipped += 1;
                mark_queue_event_failed(state, &event.id, format!("{error:#}"));
            },
        }
    }
    Ok(counts)
}

fn queue_event_to_trigger(
    config: &Config,
    state: &mut State,
    event: &QueuedWebhookEvent,
) -> Result<Option<ReviewTrigger>> {
    if !is_repo_allowlisted(config, &event.repo) {
        return Ok(None);
    }
    let pr = pr_details(&event.repo, event.pr_number).unwrap_or_else(|_| PullRequest {
        number: event.pr_number,
        title: event.pr_title.clone(),
        url: event.pr_url.clone(),
        head_ref_oid: event.head_sha.clone(),
        base_ref_name: event.base_ref_name.clone(),
        body: event.pr_body.clone(),
        files: Vec::new(),
        author: None,
    });

    if event.event == "issue_comment" {
        let Some(comment_id) = event.comment_id else {
            return Ok(None);
        };
        let comment = IssueComment {
            id: comment_id,
            body: event.comment_body.clone(),
            user: event
                .comment_author
                .as_ref()
                .map(|login| GhUser { login: login.clone() }),
            html_url: event.comment_url.clone(),
            created_at: None,
        };
        if !is_trigger_comment(config, &event.repo, &comment) {
            return Ok(None);
        }
        let trigger = ReviewTrigger {
            repo: event.repo.clone(),
            pr,
            kind: ReviewTriggerKind::MentionComment(comment),
        };
        let key = trigger.state_key();
        match state
            .processed_comments
            .get(&key)
            .map(|record| record.status.as_str())
        {
            None => {
                acknowledge_trigger(config, &trigger);
                enqueue_mention_trigger(state, &trigger)?;
                Ok(Some(trigger))
            },
            Some(STATUS_PENDING) => Ok(Some(trigger)),
            Some(_) => Ok(None),
        }
    } else if event.event == "pull_request" {
        let now = now_epoch();
        mark_seen_open_pr(state, &event.repo, &pr, now);
        let key = pr_key(&event.repo, event.pr_number);
        if state.auto_pr_reviews.contains_key(&key) {
            return Ok(None);
        }
        state
            .auto_pr_reviews
            .insert(key, new_auto_pr_review(&event.repo, &pr, STATUS_PENDING));
        Ok(Some(ReviewTrigger {
            repo: event.repo.clone(),
            pr,
            kind: ReviewTriggerKind::NewPullRequest,
        }))
    } else {
        Ok(None)
    }
}

fn mark_queue_event_done(state: &mut State, event_id: &str, message: String) {
    if let Some(event) = state.event_queue.iter_mut().find(|event| event.id == event_id) {
        event.status = STATUS_COMMENTED.into();
        event.processed_at = Some(now_epoch());
        event.error = None;
        if let Some(delivery) = state.webhook_deliveries.get_mut(&event.delivery_id) {
            delivery.status = STATUS_COMMENTED.into();
            delivery.processed_at = Some(now_epoch());
            delivery.message = Some(message);
        }
    }
}

fn mark_queue_event_failed(state: &mut State, event_id: &str, error: String) {
    if let Some(event) = state.event_queue.iter_mut().find(|event| event.id == event_id) {
        event.status = STATUS_FAILED.into();
        event.processed_at = Some(now_epoch());
        event.error = Some(error.clone());
        if let Some(delivery) = state.webhook_deliveries.get_mut(&event.delivery_id) {
            delivery.status = STATUS_FAILED.into();
            delivery.processed_at = Some(now_epoch());
            delivery.message = Some(error);
        }
    }
}

fn baseline_auto_review_repos(
    config: &Config,
    state: &mut State,
    open_prs: &[(String, PullRequest)],
) -> Result<usize> {
    if !config.auto_review_new_prs || config.auto_review_bootstrap_existing_open_prs {
        return Ok(0);
    }

    let now = now_epoch();
    let mut baselined = 0usize;
    for repo in &config.repos {
        if state.auto_review_baselined_repos.contains_key(repo) {
            continue;
        }
        for (candidate_repo, pr) in open_prs
            .iter()
            .filter(|(candidate_repo, _)| candidate_repo.eq_ignore_ascii_case(repo))
        {
            let _ = mark_seen_open_pr(state, candidate_repo, pr, now);
        }
        state.auto_review_baselined_repos.insert(repo.clone(), now);
        baselined += 1;
    }
    Ok(baselined)
}

fn enqueue_auto_review_trigger(
    config: &Config,
    state: &mut State,
    repo: &str,
    pr: &PullRequest,
) -> Result<Option<ReviewTrigger>> {
    if !config.auto_review_new_prs || !is_repo_allowlisted(config, repo) {
        return Ok(None);
    }

    if config.auto_review_bootstrap_existing_open_prs {
        let now = now_epoch();
        state
            .auto_review_baselined_repos
            .entry(repo.to_owned())
            .or_insert(now);
    } else if !state.auto_review_baselined_repos.contains_key(repo) {
        return Ok(None);
    }

    let now = now_epoch();
    let is_new = mark_seen_open_pr(state, repo, pr, now);
    let key = pr_key(repo, pr.number);
    if !is_new {
        if let Some(record) = state.auto_pr_reviews.get_mut(&key) {
            if record.status == STATUS_PENDING {
                record.head_sha = pr.head_ref_oid.clone();
                return Ok(Some(ReviewTrigger {
                    repo: repo.to_owned(),
                    pr: pr.clone(),
                    kind: ReviewTriggerKind::NewPullRequest,
                }));
            }
        }
        return Ok(None);
    }

    if state.auto_pr_reviews.contains_key(&key) {
        return Ok(None);
    }

    state
        .auto_pr_reviews
        .insert(key, new_auto_pr_review(repo, pr, STATUS_PENDING));
    Ok(Some(ReviewTrigger {
        repo: repo.to_owned(),
        pr: pr.clone(),
        kind: ReviewTriggerKind::NewPullRequest,
    }))
}

fn is_trigger_comment(config: &Config, repo: &str, comment: &IssueComment) -> bool {
    let Some(body) = comment.body.as_deref() else {
        return false;
    };
    if body.contains(&config.comment_marker)
        || body.contains(AGENT_LINE)
        || body.contains("我是 whatevertogo 的自动化审查 agent。")
    {
        return false;
    }
    if !body.contains(&config.mention) {
        return false;
    }
    let author = comment.user.as_ref().map(|user| user.login.as_str());
    author.is_some_and(|author| is_trusted_comment_author(config, author))
        || is_repo_allowlisted(config, repo)
}

fn is_trusted_comment_author(config: &Config, author: &str) -> bool {
    config
        .trusted_comment_authors
        .iter()
        .any(|trusted| trusted.eq_ignore_ascii_case(author))
}

fn is_repo_allowlisted(config: &Config, repo: &str) -> bool {
    config
        .repos
        .iter()
        .any(|allowed_repo| allowed_repo.eq_ignore_ascii_case(repo))
}

fn acknowledge_trigger(config: &Config, trigger: &ReviewTrigger) {
    let Some(comment) = trigger.comment() else {
        return;
    };
    if let Err(error) = add_comment_reaction(&trigger.repo, comment.id, "eyes") {
        eprintln!(
            "failed to add {} reaction to {} comment {}: {error:#}",
            config.github_user, trigger.repo, comment.id
        );
    }
}

async fn process_trigger(
    config: &Config,
    state: &mut State,
    run_info: &RunInfo,
    trigger: ReviewTrigger,
) -> Result<()> {
    let key = trigger.state_key();
    mark_trigger_running(state, &trigger);
    save_state(state)?;
    if trigger.is_auto_review() && config.auto_review_start_comment {
        match post_auto_review_start_comment(config, &trigger) {
            Ok(Some(url)) => {
                update_auto_review_start_comment(state, &key, url);
                save_state(state)?;
            },
            Ok(None) => {},
            Err(error) => eprintln!(
                "failed to post auto review start comment for {}; continuing review: {error:#}",
                key
            ),
        }
    }

    let result = review_trigger(config, state, run_info, &trigger).await;
    match result {
        Ok(record) => {
            mark_trigger_commented(state, &key, &record);
            append_memory(config, &record)?;
            append_run_log(config, &record)?;
        },
        Err(error) => {
            let error_text = error.to_string();
            mark_trigger_failed(state, &key, &trigger, error_text.clone());
            if trigger.is_auto_review() && config.auto_review_failure_comment {
                match post_auto_review_failure_comment(config, &trigger, &error_text) {
                    Ok(Some(url)) => update_auto_review_failure_comment(state, &key, url),
                    Ok(None) => {},
                    Err(comment_error) => eprintln!(
                        "failed to post auto review failure comment for {}: {comment_error:#}",
                        key
                    ),
                }
            }
            save_state(state)?;
            return Err(error);
        },
    }
    save_state(state)?;
    Ok(())
}

fn enqueue_mention_trigger(state: &mut State, trigger: &ReviewTrigger) -> Result<()> {
    if insert_pending_mention_trigger(state, trigger) {
        save_state(state)?;
    }
    Ok(())
}

fn insert_pending_mention_trigger(state: &mut State, trigger: &ReviewTrigger) -> bool {
    let Some(comment) = trigger.comment() else {
        return false;
    };
    let key = processed_key(&trigger.repo, trigger.pr.number, comment.id);
    if state.processed_comments.contains_key(&key) {
        return false;
    }
    state
        .processed_comments
        .insert(key, new_processed_comment(trigger, comment, STATUS_PENDING));
    true
}

fn mark_trigger_running(state: &mut State, trigger: &ReviewTrigger) {
    let key = trigger.state_key();
    let now = now_epoch();
    match trigger.comment() {
        Some(comment) => {
            let entry = state
                .processed_comments
                .entry(key)
                .or_insert_with(|| new_processed_comment(trigger, comment, STATUS_PENDING));
            entry.status = STATUS_RUNNING.into();
            entry.started_at = now;
            entry.finished_at = None;
            entry.error = None;
        },
        None => {
            let entry = state
                .auto_pr_reviews
                .entry(key)
                .or_insert_with(|| new_auto_pr_review(&trigger.repo, &trigger.pr, STATUS_PENDING));
            entry.status = STATUS_RUNNING.into();
            entry.started_at = now;
            entry.finished_at = None;
            entry.error = None;
        },
    }
}

fn new_processed_comment(
    trigger: &ReviewTrigger,
    comment: &IssueComment,
    status: &str,
) -> ProcessedComment {
    ProcessedComment {
        repo: trigger.repo.clone(),
        pr_number: trigger.pr.number,
        comment_id: comment.id,
        head_sha: trigger.pr.head_ref_oid.clone(),
        session_id: None,
        status: status.into(),
        started_at: now_epoch(),
        finished_at: None,
        review_comment_url: None,
        error: None,
    }
}

fn new_auto_pr_review(repo: &str, pr: &PullRequest, status: &str) -> AutoPrReview {
    AutoPrReview {
        repo: repo.to_owned(),
        pr_number: pr.number,
        head_sha: pr.head_ref_oid.clone(),
        session_id: None,
        status: status.into(),
        started_at: now_epoch(),
        finished_at: None,
        start_comment_url: None,
        review_comment_url: None,
        failure_comment_url: None,
        error: None,
    }
}

fn mark_seen_open_pr(state: &mut State, repo: &str, pr: &PullRequest, now: u64) -> bool {
    let key = pr_key(repo, pr.number);
    match state.seen_open_prs.get_mut(&key) {
        Some(seen) => {
            seen.head_sha = pr.head_ref_oid.clone();
            seen.last_seen_at = now;
            false
        },
        None => {
            state.seen_open_prs.insert(
                key,
                SeenOpenPr {
                    repo: repo.to_owned(),
                    pr_number: pr.number,
                    head_sha: pr.head_ref_oid.clone(),
                    first_seen_at: now,
                    last_seen_at: now,
                },
            );
            true
        },
    }
}

fn mark_trigger_commented(state: &mut State, key: &str, record: &ReviewRecord) {
    if let Some(entry) = state.processed_comments.get_mut(key) {
        entry.session_id = Some(record.session_id.clone());
        entry.status = STATUS_COMMENTED.into();
        entry.finished_at = Some(now_epoch());
        entry.review_comment_url = record.review_comment_url.clone();
        return;
    }
    if let Some(entry) = state.auto_pr_reviews.get_mut(key) {
        entry.session_id = Some(record.session_id.clone());
        entry.status = STATUS_COMMENTED.into();
        entry.finished_at = Some(now_epoch());
        entry.review_comment_url = record.review_comment_url.clone();
    }
}

fn mark_trigger_failed(state: &mut State, key: &str, trigger: &ReviewTrigger, error: String) {
    if let Some(entry) = state.processed_comments.get_mut(key) {
        entry.status = STATUS_FAILED.into();
        entry.finished_at = Some(now_epoch());
        entry.error = Some(error);
        return;
    }
    let entry = state
        .auto_pr_reviews
        .entry(key.to_owned())
        .or_insert_with(|| new_auto_pr_review(&trigger.repo, &trigger.pr, STATUS_PENDING));
    entry.status = STATUS_FAILED.into();
    entry.finished_at = Some(now_epoch());
    entry.error = Some(error);
}

fn update_auto_review_start_comment(state: &mut State, key: &str, url: String) {
    if let Some(entry) = state.auto_pr_reviews.get_mut(key) {
        entry.start_comment_url = Some(url);
    }
}

fn update_auto_review_failure_comment(state: &mut State, key: &str, url: String) {
    if let Some(entry) = state.auto_pr_reviews.get_mut(key) {
        entry.failure_comment_url = Some(url);
    }
}

fn sort_pending_triggers(state: &State, triggers: &mut [ReviewTrigger]) {
    triggers.sort_by(|left, right| {
        pending_trigger_order_key(state, left).cmp(&pending_trigger_order_key(state, right))
    });
}

fn pending_trigger_order_key(state: &State, trigger: &ReviewTrigger) -> (u8, u64, u64) {
    match trigger.comment() {
        Some(comment) => {
            let key = processed_key(&trigger.repo, trigger.pr.number, comment.id);
            let queued_at = state
                .processed_comments
                .get(&key)
                .map(|record| record.started_at)
                .unwrap_or(u64::MAX);
            (0, queued_at, comment.id)
        },
        None => {
            let key = pr_key(&trigger.repo, trigger.pr.number);
            let queued_at = state
                .auto_pr_reviews
                .get(&key)
                .map(|record| record.started_at)
                .unwrap_or(u64::MAX);
            (1, queued_at, trigger.pr.number)
        },
    }
}

fn has_running_review(state: &State) -> bool {
    state
        .processed_comments
        .values()
        .any(|record| record.status == STATUS_RUNNING)
        || state
            .auto_pr_reviews
            .values()
            .any(|record| record.status == STATUS_RUNNING)
}

fn recover_stale_running_reviews(config: &Config, state: &mut State) -> Result<usize> {
    let recovered = mark_stale_running_reviews_failed(config, state);
    if recovered > 0 {
        save_state(state)?;
    }
    Ok(recovered)
}

fn mark_stale_running_reviews_failed(config: &Config, state: &mut State) -> usize {
    let now = now_epoch();
    let timeout = config.review_timeout_seconds.saturating_add(60);
    let mut recovered = 0usize;
    for record in state.processed_comments.values_mut() {
        if record.status != STATUS_RUNNING {
            continue;
        }
        if now.saturating_sub(record.started_at) <= timeout {
            continue;
        }
        record.status = STATUS_FAILED.into();
        record.finished_at = Some(now);
        record.error = Some(format!(
            "review was marked failed after exceeding queue recovery timeout of {timeout}s"
        ));
        recovered += 1;
    }
    for record in state.auto_pr_reviews.values_mut() {
        if record.status != STATUS_RUNNING {
            continue;
        }
        if now.saturating_sub(record.started_at) <= timeout {
            continue;
        }
        record.status = STATUS_FAILED.into();
        record.finished_at = Some(now);
        record.error = Some(format!(
            "auto review was marked failed after exceeding queue recovery timeout of {timeout}s"
        ));
        recovered += 1;
    }
    recovered
}

#[derive(Debug, Default, Clone, Copy)]
struct CleanupStats {
    worktrees: usize,
    sessions: usize,
}

impl std::ops::AddAssign for CleanupStats {
    fn add_assign(&mut self, rhs: Self) {
        self.worktrees += rhs.worktrees;
        self.sessions += rhs.sessions;
    }
}

fn cleanup_completed_pr_resources(
    config: &Config,
    state: &mut State,
    run_info: Option<&RunInfo>,
) -> Result<CleanupStats> {
    Ok(CleanupStats {
        worktrees: cleanup_completed_pr_worktrees(config, state)?,
        sessions: cleanup_completed_pr_sessions(config, state, run_info)?,
    })
}

fn cleanup_completed_pr_worktrees(config: &Config, state: &mut State) -> Result<usize> {
    let now = now_epoch();
    let delay = config.worktree_cleanup_delay_seconds;
    let candidates = cleanup_candidates(state, now, delay);

    let mut cleaned = 0usize;
    for (key, session, finished_at) in candidates {
        let worktree = PathBuf::from(&session.worktree);
        if !worktree.exists() {
            continue;
        }

        fs::remove_dir_all(&worktree)
            .with_context(|| format!("remove worktree {}", worktree.display()))?;
        let _ = remove_empty_parent(&worktree)?;

        cleaned += 1;
        eprintln!(
            "cleaned PR worktree for {} after {}s: session={}, worktree={}",
            key,
            now.saturating_sub(finished_at),
            session.session_id,
            session.worktree
        );
    }

    Ok(cleaned)
}

fn cleanup_completed_pr_sessions(
    config: &Config,
    state: &mut State,
    run_info: Option<&RunInfo>,
) -> Result<usize> {
    let now = now_epoch();
    let delay = config.session_cleanup_delay_seconds;
    let candidates = cleanup_candidates(state, now, delay);

    let mut cleaned = 0usize;
    for (key, session, finished_at) in candidates {
        if let Some(run_info) = run_info {
            if let Err(error) = delete_session(run_info, &session.session_id) {
                if session_delete_error_is_not_found(&error) {
                    eprintln!(
                        "astrcode session {} for {} is already gone; removing stale tracking",
                        session.session_id, key
                    );
                } else {
                    eprintln!(
                        "failed to delete astrcode session {} for {} during session cleanup; \
                         keeping tracking for retry: {error:#}",
                        session.session_id, key
                    );
                    continue;
                }
            }
        }

        let worktree = PathBuf::from(&session.worktree);
        if worktree.exists() {
            fs::remove_dir_all(&worktree)
                .with_context(|| format!("remove worktree {}", worktree.display()))?;
        }

        let _ = remove_empty_parent(&worktree)?;

        if state.pr_sessions.remove(&key).is_none() {
            continue;
        }
        cleaned += 1;
        eprintln!(
            "cleaned PR session for {} after {}s: session={}, worktree={}",
            key,
            now.saturating_sub(finished_at),
            session.session_id,
            session.worktree
        );
    }

    if cleaned > 0 {
        save_state(state)?;
    }
    Ok(cleaned)
}

fn session_delete_error_is_not_found(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("404") || message.to_ascii_lowercase().contains("not found")
}

#[cfg(test)]
fn cleanup_completed_pr_sessions_after_delete_result(
    config: &Config,
    state: &mut State,
    delete_session_result: Result<()>,
) -> Result<usize> {
    let now = now_epoch();
    let delay = config.session_cleanup_delay_seconds;
    let candidates = cleanup_candidates(state, now, delay);

    let mut cleaned = 0usize;
    for (key, session, finished_at) in candidates {
        if let Err(error) = &delete_session_result {
            if !session_delete_error_is_not_found(error) {
                eprintln!(
                    "failed to delete astrcode session {} for {} during session cleanup; keeping \
                     tracking for retry: {error:#}",
                    session.session_id, key
                );
                continue;
            }
        }

        let worktree = PathBuf::from(&session.worktree);
        if worktree.exists() {
            fs::remove_dir_all(&worktree)
                .with_context(|| format!("remove worktree {}", worktree.display()))?;
        }

        let _ = remove_empty_parent(&worktree)?;

        if state.pr_sessions.remove(&key).is_none() {
            continue;
        }
        cleaned += 1;
        eprintln!(
            "cleaned PR session for {} after {}s: session={}, worktree={}",
            key,
            now.saturating_sub(finished_at),
            session.session_id,
            session.worktree
        );
    }

    Ok(cleaned)
}

#[cfg(test)]
fn cleanup_completed_pr_sessions_without_server(
    config: &Config,
    state: &mut State,
) -> Result<usize> {
    cleanup_completed_pr_sessions_after_delete_result(config, state, Ok(()))
}

fn cleanup_candidates(state: &State, now: u64, delay: u64) -> Vec<(String, PrSession, u64)> {
    state
        .pr_sessions
        .iter()
        .filter_map(|(key, session)| {
            cleanup_candidate_for_pr(state, session, now, delay)
                .map(|finished_at| (key.clone(), session.clone(), finished_at))
        })
        .collect()
}

fn cleanup_candidate_for_pr(
    state: &State,
    session: &PrSession,
    now: u64,
    delay: u64,
) -> Option<u64> {
    let mut latest_finished: Option<u64> = None;
    let mut saw_comment = false;
    for record in state.processed_comments.values().filter(|record| {
        record.repo == session.repo
            && record.pr_number == session.pr_number
            && record.session_id.as_deref() == Some(session.session_id.as_str())
    }) {
        saw_comment = true;
        if record.status == STATUS_RUNNING {
            return None;
        }
        if matches!(
            record.status.as_str(),
            STATUS_COMMENTED | STATUS_FAILED | STATUS_ABORTED
        ) {
            if let Some(finished_at) = record.finished_at {
                latest_finished =
                    Some(latest_finished.map_or(finished_at, |latest| latest.max(finished_at)));
            }
        }
    }
    for record in state.auto_pr_reviews.values().filter(|record| {
        record.repo == session.repo
            && record.pr_number == session.pr_number
            && record.session_id.as_deref() == Some(session.session_id.as_str())
    }) {
        saw_comment = true;
        if record.status == STATUS_RUNNING {
            return None;
        }
        if matches!(
            record.status.as_str(),
            STATUS_COMMENTED | STATUS_FAILED | STATUS_ABORTED
        ) {
            if let Some(finished_at) = record.finished_at {
                latest_finished =
                    Some(latest_finished.map_or(finished_at, |latest| latest.max(finished_at)));
            }
        }
    }

    if !saw_comment {
        return None;
    }

    let finished_at = latest_finished?;
    if now.saturating_sub(finished_at) < delay {
        return None;
    }

    Some(finished_at)
}

fn remove_empty_parent(path: &Path) -> Result<bool> {
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    match fs::remove_dir(parent) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(false),
        Err(error) => Err(error).with_context(|| format!("remove empty {}", parent.display())),
    }
}
