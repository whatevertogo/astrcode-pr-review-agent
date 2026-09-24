use super::*;
use std::fmt;

#[derive(Debug)]
pub(super) struct ApiFailure {
    pub status: u16,
    pub retry_at: u64,
    pub message: String,
}
impl fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GitHub HTTP {}: {}", self.status, self.message)
    }
}
impl std::error::Error for ApiFailure {}

pub(super) struct Reply {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

pub(super) fn parse_response(text: &str) -> Result<Reply> {
    let normalized = text.replace("\r\n", "\n");
    let (head, body) = normalized
        .split_once("\n\n")
        .context("missing GitHub response headers")?;
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .context("missing GitHub response status")?
        .parse()?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(Reply {
        status,
        headers,
        body: if body.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(body)?
        },
    })
}

pub(super) fn get(endpoint: &str, etag: Option<&str>) -> Result<Reply> {
    let conditional = etag.map(|value| format!("If-None-Match: {value}"));
    let mut args = vec!["api", "--hostname", "github.com", "--include", endpoint];
    if let Some(header) = &conditional {
        args.extend(["-H", header]);
    }
    let (success, stdout, stderr) =
        command_output_with_timeout("gh", &args, None, Duration::from_secs(20))?;
    let reply = parse_response(&stdout)
        .with_context(|| format!("GitHub request failed: {}", stderr.trim()))?;
    if success && matches!(reply.status, 200..=299 | 304) {
        return Ok(reply);
    }
    let now = now_epoch();
    let retry_at = reply
        .headers
        .get("retry-after")
        .and_then(|s| s.parse::<u64>().ok())
        .map(|s| now.saturating_add(s))
        .or_else(|| {
            (reply
                .headers
                .get("x-ratelimit-remaining")
                .map(String::as_str)
                == Some("0"))
            .then(|| {
                reply
                    .headers
                    .get("x-ratelimit-reset")
                    .and_then(|s| s.parse().ok())
            })
            .flatten()
        })
        .unwrap_or(0);
    Err(ApiFailure {
        status: reply.status,
        retry_at,
        message: reply.body["message"]
            .as_str()
            .unwrap_or(stderr.trim())
            .to_owned(),
    }
    .into())
}

pub(super) fn is_missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ApiFailure>()
        .is_some_and(|e| matches!(e.status, 404 | 410))
}

pub(super) fn retry_at(error: &anyhow::Error, failures: u32) -> u64 {
    let fallback = now_epoch().saturating_add(30u64.saturating_mul(1 << failures.min(6)).min(1800));
    error.downcast_ref::<ApiFailure>().map_or(fallback, |e| {
        fallback.max(e.retry_at).max(if e.status == 401 {
            now_epoch() + 900
        } else {
            0
        })
    })
}

pub(super) fn has_next(reply: &Reply) -> bool {
    reply
        .headers
        .get("link")
        .is_some_and(|value| value.contains("rel=\"next\""))
}

pub(super) fn timestamp(epoch: u64) -> Result<String> {
    let time =
        chrono::DateTime::from_timestamp(i64::try_from(epoch)?, 0).context("invalid timestamp")?;
    Ok(time.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

pub(super) fn parse_timestamp(value: &str) -> Result<u64> {
    Ok(u64::try_from(
        chrono::DateTime::parse_from_rfc3339(value)?.timestamp(),
    )?)
}
