type HmacSha256 = Hmac<Sha256>;

pub fn spawn_webhook_server(config: Config) -> Result<()> {
    if !config.webhook_enabled {
        return Ok(());
    }
    let secret = match std::env::var(&config.webhook_secret_env) {
        Ok(secret) if !secret.trim().is_empty() => secret,
        _ => {
            eprintln!(
                "astrcode-pr-review-agent webhook disabled: {} is not set",
                config.webhook_secret_env
            );
            return Ok(());
        },
    };
    let listener = TcpListener::bind(&config.webhook_listen_addr)
        .with_context(|| format!("bind webhook listener {}", config.webhook_listen_addr))?;
    let config = Arc::new(config);
    let secret = Arc::new(secret);
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            let config = Arc::clone(&config);
            let secret = Arc::clone(&secret);
            match connection {
                Ok(stream) => {
                    std::thread::spawn(move || {
                        if let Err(error) = handle_webhook_connection(stream, &config, &secret) {
                            eprintln!("astrcode-pr-review-agent webhook request failed: {error:#}");
                        }
                    });
                },
                Err(error) => eprintln!("astrcode-pr-review-agent webhook accept failed: {error}"),
            }
        }
    });
    Ok(())
}

fn handle_webhook_connection(mut stream: TcpStream, config: &Config, secret: &str) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = read_http_request(&mut stream)?;
    let response = match enqueue_webhook_request(config, secret, &request) {
        Ok(message) => http_response(202, "Accepted", &message),
        Err(error) => http_response(error.status, error.reason, &error.message),
    };
    stream.write_all(response.as_bytes())?;
    Ok(())
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if header_end.is_none() {
            header_end = find_header_end(&buffer);
        }
        if let Some(end) = header_end {
            let headers = parse_headers(&buffer[..end])?;
            let content_length = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.parse::<usize>().ok())
                .unwrap_or(0);
            if buffer.len() >= end + 4 + content_length {
                let body = buffer[end + 4..end + 4 + content_length].to_vec();
                let first_line = String::from_utf8_lossy(&buffer[..end])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                return Ok(HttpRequest {
                    first_line,
                    headers,
                    body,
                });
            }
        }
        if buffer.len() > 2_000_000 {
            anyhow::bail!("webhook request exceeds 2MB");
        }
    }
    anyhow::bail!("incomplete webhook request")
}

fn enqueue_webhook_request(
    config: &Config,
    secret: &str,
    request: &HttpRequest,
) -> std::result::Result<String, WebhookHttpError> {
    let mut parts = request.first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if method != "POST" || path != config.webhook_path {
        return Err(WebhookHttpError::new(404, "Not Found", "unknown webhook path"));
    }
    let event = request
        .header("x-github-event")
        .ok_or_else(|| WebhookHttpError::new(400, "Bad Request", "missing X-GitHub-Event"))?;
    let delivery_id = request
        .header("x-github-delivery")
        .ok_or_else(|| WebhookHttpError::new(400, "Bad Request", "missing X-GitHub-Delivery"))?;
    let signature = request
        .header("x-hub-signature-256")
        .ok_or_else(|| WebhookHttpError::new(401, "Unauthorized", "missing signature"))?;
    if !verify_webhook_signature(secret, &request.body, signature) {
        return Err(WebhookHttpError::new(401, "Unauthorized", "invalid signature"));
    }
    let payload: Value = serde_json::from_slice(&request.body)
        .map_err(|error| WebhookHttpError::new(400, "Bad Request", format!("{error}")))?;
    enqueue_webhook_payload(config, event, delivery_id, &payload)
        .map_err(|error| WebhookHttpError::new(500, "Internal Server Error", error.to_string()))
}

fn enqueue_webhook_payload(
    config: &Config,
    event: &str,
    delivery_id: &str,
    payload: &Value,
) -> Result<String> {
    ensure_layout(config)?;
    let lock_path = agent_dir()?.join("run.lock");
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open lock {}", lock_path.display()))?;
    if lock.try_lock_exclusive().is_err() {
        append_spooled_webhook_event(event, delivery_id, payload)?;
        return Ok("queued in webhook spool".into());
    }
    let mut state = load_state()?;
    let message = enqueue_webhook_payload_into_state(config, &mut state, event, delivery_id, payload)?;
    trim_webhook_state(&mut state);
    save_state(&state)?;
    lock.unlock()?;
    Ok(message)
}

fn enqueue_webhook_payload_into_state(
    config: &Config,
    state: &mut State,
    event: &str,
    delivery_id: &str,
    payload: &Value,
) -> Result<String> {
    if state.webhook_deliveries.contains_key(delivery_id) {
        return Ok("duplicate delivery ignored".into());
    }
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    state.webhook_deliveries.insert(
        delivery_id.to_owned(),
        WebhookDelivery {
            delivery_id: delivery_id.to_owned(),
            event: event.to_owned(),
            action: if action.is_empty() {
                None
            } else {
                Some(action.clone())
            },
            status: STATUS_PENDING.into(),
            received_at: now_epoch(),
            processed_at: None,
            message: None,
        },
    );

    let queued = match event {
        "pull_request" if matches!(action.as_str(), "opened" | "reopened" | "synchronize") => {
            enqueue_pull_request_webhook(config, state, delivery_id, event, &action, payload)?
        },
        "issue_comment" if action == "created" => {
            enqueue_issue_comment_webhook(config, state, delivery_id, event, &action, payload)?
        },
        "ping" => {
            mark_delivery_done(state, delivery_id, "ping accepted".into());
            false
        },
        _ => {
            mark_delivery_done(
                state,
                delivery_id,
                format!("ignored event={event} action={action}"),
            );
            false
        },
    };
    Ok(if queued {
        "queued".into()
    } else {
        "accepted".into()
    })
}

fn append_spooled_webhook_event(event: &str, delivery_id: &str, payload: &Value) -> Result<()> {
    let path = webhook_spool_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    serde_json::to_writer(
        &mut file,
        &SpooledWebhookEvent {
            event: event.to_owned(),
            delivery_id: delivery_id.to_owned(),
            payload: payload.clone(),
            spooled_at: now_epoch(),
        },
    )?;
    writeln!(file)?;
    Ok(())
}

fn import_spooled_webhook_events(config: &Config, state: &mut State) -> Result<usize> {
    let path = webhook_spool_path()?;
    if !path.exists() {
        return Ok(0);
    }
    let raw = fs::read_to_string(&path)?;
    let backup = path.with_extension(format!("imported-{}.jsonl", now_epoch()));
    fs::rename(&path, &backup)?;
    let mut imported = 0usize;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<SpooledWebhookEvent>(line) {
            Ok(event) => {
                if let Err(error) = enqueue_webhook_payload_into_state(
                    config,
                    state,
                    &event.event,
                    &event.delivery_id,
                    &event.payload,
                ) {
                    eprintln!(
                        "failed to import spooled webhook delivery {}: {error:#}",
                        event.delivery_id
                    );
                } else {
                    imported += 1;
                }
            },
            Err(error) => eprintln!("failed to parse spooled webhook event: {error:#}"),
        }
    }
    Ok(imported)
}

fn enqueue_pull_request_webhook(
    config: &Config,
    state: &mut State,
    delivery_id: &str,
    event: &str,
    action: &str,
    payload: &Value,
) -> Result<bool> {
    let Some(pr) = payload.get("pull_request") else {
        mark_delivery_failed(state, delivery_id, "missing pull_request".into());
        return Ok(false);
    };
    let repo = payload
        .get("repository")
        .and_then(|repo| repo.get("full_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_repo_allowlisted(config, repo) {
        mark_delivery_done(state, delivery_id, format!("repo {repo} is not allowlisted"));
        return Ok(false);
    }
    let Some(pr_number) = pr.get("number").and_then(Value::as_u64) else {
        mark_delivery_failed(state, delivery_id, "missing PR number".into());
        return Ok(false);
    };
    let event = queued_webhook_event_from_pr(delivery_id, event, action, repo, pr_number, pr)?;
    if action == "synchronize" && !config.auto_review_on_synchronize {
        update_synchronize_memory(state, &event);
        mark_delivery_done(
            state,
            delivery_id,
            format!("recorded synchronize for {repo}#{pr_number}"),
        );
        return Ok(false);
    }
    push_webhook_event(state, event);
    Ok(true)
}

fn enqueue_issue_comment_webhook(
    config: &Config,
    state: &mut State,
    delivery_id: &str,
    event: &str,
    action: &str,
    payload: &Value,
) -> Result<bool> {
    if payload
        .get("issue")
        .and_then(|issue| issue.get("pull_request"))
        .is_none()
    {
        mark_delivery_done(state, delivery_id, "issue comment is not on a PR".into());
        return Ok(false);
    }
    let repo = payload
        .get("repository")
        .and_then(|repo| repo.get("full_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_repo_allowlisted(config, repo) {
        mark_delivery_done(state, delivery_id, format!("repo {repo} is not allowlisted"));
        return Ok(false);
    }
    let Some(pr_number) = payload
        .get("issue")
        .and_then(|issue| issue.get("number"))
        .and_then(Value::as_u64)
    else {
        mark_delivery_failed(state, delivery_id, "missing issue number".into());
        return Ok(false);
    };
    let Some(comment) = payload.get("comment") else {
        mark_delivery_failed(state, delivery_id, "missing comment".into());
        return Ok(false);
    };
    let body = comment
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !body.contains(&config.mention) {
        mark_delivery_done(state, delivery_id, "comment does not mention bot".into());
        return Ok(false);
    }
    let pr = pr_details(repo, pr_number).unwrap_or_else(|_| PullRequest {
        number: pr_number,
        title: payload
            .get("issue")
            .and_then(|issue| issue.get("title"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        url: payload
            .get("issue")
            .and_then(|issue| issue.get("html_url"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        head_ref_oid: String::new(),
        base_ref_name: String::new(),
        body: payload
            .get("issue")
            .and_then(|issue| issue.get("body"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        files: Vec::new(),
        author: None,
    });
    let event = QueuedWebhookEvent {
        id: format!("{delivery_id}:issue_comment:{pr_number}"),
        delivery_id: delivery_id.to_owned(),
        event: event.to_owned(),
        action: action.to_owned(),
        repo: repo.to_owned(),
        pr_number,
        head_sha: pr.head_ref_oid,
        base_ref_name: pr.base_ref_name,
        pr_title: pr.title,
        pr_url: pr.url,
        pr_body: pr.body,
        comment_id: comment.get("id").and_then(Value::as_u64),
        comment_body: Some(body.to_owned()),
        comment_author: comment
            .get("user")
            .and_then(|user| user.get("login"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        comment_url: comment
            .get("html_url")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        status: STATUS_PENDING.into(),
        queued_at: now_epoch(),
        processed_at: None,
        error: None,
    };
    push_webhook_event(state, event);
    Ok(true)
}

fn queued_webhook_event_from_pr(
    delivery_id: &str,
    event: &str,
    action: &str,
    repo: &str,
    pr_number: u64,
    pr: &Value,
) -> Result<QueuedWebhookEvent> {
    Ok(QueuedWebhookEvent {
        id: format!("{delivery_id}:pull_request:{pr_number}"),
        delivery_id: delivery_id.to_owned(),
        event: event.to_owned(),
        action: action.to_owned(),
        repo: repo.to_owned(),
        pr_number,
        head_sha: pr
            .get("head")
            .and_then(|head| head.get("sha"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        base_ref_name: pr
            .get("base")
            .and_then(|base| base.get("ref"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        pr_title: pr
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        pr_url: pr
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        pr_body: pr.get("body").and_then(Value::as_str).map(ToOwned::to_owned),
        comment_id: None,
        comment_body: None,
        comment_author: None,
        comment_url: None,
        status: STATUS_PENDING.into(),
        queued_at: now_epoch(),
        processed_at: None,
        error: None,
    })
}

fn push_webhook_event(state: &mut State, event: QueuedWebhookEvent) {
    if state.event_queue.iter().any(|queued| queued.id == event.id) {
        mark_delivery_done(
            state,
            &event.delivery_id,
            format!("event {} already queued", event.id),
        );
        return;
    }
    mark_delivery_done(state, &event.delivery_id, format!("queued {}", event.id));
    state.event_queue.push(event);
}

fn update_synchronize_memory(state: &mut State, event: &QueuedWebhookEvent) {
    let key = pr_key(&event.repo, event.pr_number);
    let memory = state.pr_review_memory.entry(key).or_default();
    memory.last_head_sha = Some(event.head_sha.clone());
}

fn mark_delivery_done(state: &mut State, delivery_id: &str, message: String) {
    if let Some(delivery) = state.webhook_deliveries.get_mut(delivery_id) {
        delivery.status = STATUS_COMMENTED.into();
        delivery.processed_at = Some(now_epoch());
        delivery.message = Some(message);
    }
}

fn mark_delivery_failed(state: &mut State, delivery_id: &str, message: String) {
    if let Some(delivery) = state.webhook_deliveries.get_mut(delivery_id) {
        delivery.status = STATUS_FAILED.into();
        delivery.processed_at = Some(now_epoch());
        delivery.message = Some(message);
    }
}

fn trim_webhook_state(state: &mut State) {
    if state.event_queue.len() > 500 {
        let keep_from = state.event_queue.len().saturating_sub(500);
        state.event_queue.drain(0..keep_from);
    }
    while state.webhook_deliveries.len() > 500 {
        let Some(key) = state.webhook_deliveries.keys().next().cloned() else {
            break;
        };
        state.webhook_deliveries.remove(&key);
    }
}

fn verify_webhook_signature(secret: &str, body: &[u8], signature: &str) -> bool {
    let Some(hex) = signature.trim().strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = decode_hex(hex) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

fn decode_hex(input: &str) -> Result<Vec<u8>> {
    if input.len() % 2 != 0 {
        anyhow::bail!("hex input has odd length");
    }
    let mut output = Vec::with_capacity(input.len() / 2);
    for pair in input.as_bytes().chunks(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        output.push((high << 4) | low);
    }
    Ok(output)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => anyhow::bail!("invalid hex byte"),
    }
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_headers(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    let text = String::from_utf8_lossy(bytes);
    Ok(text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect())
}

fn http_response(status: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

struct HttpRequest {
    first_line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug)]
struct WebhookHttpError {
    status: u16,
    reason: &'static str,
    message: String,
}

impl WebhookHttpError {
    fn new(status: u16, reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            reason,
            message: message.into(),
        }
    }
}
