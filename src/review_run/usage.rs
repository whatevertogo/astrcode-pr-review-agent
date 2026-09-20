use super::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct Usage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub uncached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub requests: u64,
    pub tool_calls: u64,
    pub estimated_requests: u64,
    pub unknown_accounting_requests: u64,
    pub missing_usage_requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StageReceipt {
    pub label: String,
    #[serde(default)]
    pub run_key: String,
    pub input_key: String,
    pub session_id: String,
    pub started_at: u64,
    pub elapsed_seconds: u64,
    pub status: String,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_format_error: Option<String>,
    pub usage: Usage,
    pub output: Option<ReviewBotOutput>,
}

pub(super) fn restore_run_keys(
    stages: &mut [StageReceipt],
    previous: &[StageReceipt],
    previous_key: &str,
    current_key: &str,
) {
    if previous_key != current_key {
        return;
    }
    let identified: BTreeSet<_> = previous
        .iter()
        .map(|stage| (&stage.session_id, &stage.input_key))
        .collect();
    for stage in stages {
        if stage.run_key.is_empty() && identified.contains(&(&stage.session_id, &stage.input_key)) {
            stage.run_key = current_key.to_owned();
        }
    }
}

pub(super) fn session_log(session: &str) -> Result<Option<PathBuf>> {
    Ok(find_file_named(
        &astrcode_dir()?.join("projects"),
        &format!("session-{session}.jsonl"),
        5,
    ))
}

pub(super) fn collect(session: &str) -> Result<Usage> {
    let Some(path) = session_log(session)? else {
        return Ok(Usage {
            missing_usage_requests: 1,
            ..Usage::default()
        });
    };
    let text = fs::read_to_string(path)?;
    Ok(from_events(text.lines().filter_map(|line| {
        serde_json::from_str::<Value>(line).ok()
    })))
}

fn from_events(events: impl Iterator<Item = Value>) -> Usage {
    let mut result = Usage::default();
    let mut completions = 0u64;
    let mut started = false;
    let mut seen = BTreeSet::new();
    for event in events {
        if let Some(seq) = event.get("seq").and_then(Value::as_u64) {
            if !seen.insert(seq) {
                continue;
            }
        }
        let payload = &event["payload"];
        match payload["type"].as_str() {
            Some("turn_started") => started = true,
            Some("tool_call_requested") => result.tool_calls += 1,
            Some("assistant_message_completed") => completions += 1,
            Some("token_usage_recorded") => {
                let usage = &payload["usage"];
                let number = |key| usage[key].as_u64().unwrap_or(0);
                result.requests += 1;
                let input = number("input_tokens");
                let cached = number("cached_input_tokens");
                let created = number("cache_creation_input_tokens");
                result.cached_input_tokens += cached;
                result.cache_creation_input_tokens += created;
                match usage["input_accounting"].as_str() {
                    Some("components") => {
                        result.input_tokens += input + cached + created;
                        result.uncached_input_tokens += input;
                    }
                    Some("inclusive") => {
                        result.input_tokens += input;
                        result.uncached_input_tokens +=
                            input.saturating_sub(cached).saturating_sub(created);
                    }
                    _ => {
                        result.input_tokens += input;
                        result.unknown_accounting_requests += 1;
                    }
                }
                result.output_tokens += number("output_tokens");
                result.reasoning_output_tokens += number("reasoning_output_tokens");
                if usage["source"] != "provider_usage" {
                    result.estimated_requests += 1;
                }
                if usage["input_tokens"].as_u64().is_none()
                    || usage["output_tokens"].as_u64().is_none()
                {
                    result.missing_usage_requests += 1;
                }
            }
            _ => {}
        }
    }
    result.missing_usage_requests += completions.saturating_sub(result.requests);
    if started && result.requests == 0 {
        result.missing_usage_requests += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_usage_migration_requires_the_same_run_and_exact_stage_identity() {
        let receipt = StageReceipt {
            label: "global".into(),
            run_key: String::new(),
            input_key: "old-global-prompt".into(),
            session_id: "session-a".into(),
            started_at: 1,
            elapsed_seconds: 2,
            status: "complete".into(),
            error: None,
            recovered_format_error: None,
            usage: Usage::default(),
            output: None,
        };
        let previous = vec![receipt.clone()];
        let mut stages = vec![receipt.clone(), receipt.clone(), receipt];
        stages[1].session_id = "unrelated-session".into();
        stages[2].input_key = "unrelated-prompt".into();
        restore_run_keys(&mut stages, &previous, "old-run", "new-run");
        assert!(stages.iter().all(|stage| stage.run_key.is_empty()));
        restore_run_keys(&mut stages, &previous, "same-run", "same-run");
        assert_eq!(stages[0].run_key, "same-run");
        assert!(stages[1..].iter().all(|stage| stage.run_key.is_empty()));
    }

    #[test]
    fn counts_inclusive_components_estimates_missing_and_duplicate_events() {
        let events = [
            json!({"seq":1,"payload":{"type":"token_usage_recorded","usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":10,"reasoning_output_tokens":5,"input_accounting":"inclusive","source":"provider_usage"}}}),
            json!({"seq":2,"payload":{"type":"token_usage_recorded","usage":{"input_tokens":30,"cached_input_tokens":10,"cache_creation_input_tokens":5,"output_tokens":7,"input_accounting":"components","source":"provider_usage"}}}),
            json!({"seq":3,"payload":{"type":"token_usage_recorded","usage":{"input_tokens":8,"source":"local_estimate_fallback"}}}),
            json!({"seq":4,"payload":{"type":"tool_call_requested"}}),
            json!({"seq":4,"payload":{"type":"tool_call_requested"}}),
        ];
        let usage = from_events(events.into_iter());
        assert_eq!(
            (
                usage.input_tokens,
                usage.uncached_input_tokens,
                usage.cached_input_tokens,
                usage.output_tokens
            ),
            (153, 110, 30, 17)
        );
        assert_eq!(
            (
                usage.requests,
                usage.tool_calls,
                usage.estimated_requests,
                usage.missing_usage_requests,
                usage.unknown_accounting_requests
            ),
            (3, 1, 1, 1, 1)
        );
    }
}
