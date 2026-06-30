use anyhow::Result;
use astrcode_extension_sdk::{
    builder::tool,
    s5r::ErrorPayload,
    worker_prelude::{command_handler, tool_handler, tool_text, HandlerResult, Worker},
};
use astrcode_pr_review_agent::{
    poll_forever, poll_once, spawn_webhook_server, status_text, Config,
};
use serde_json::json;

const EXT_ID: &str = "astrcode-pr-review-agent";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("s5r") | None => run_s5r().await.map_err(|error| {
            anyhow::anyhow!("s5r worker failed: {} ({})", error.message, error.code)
        }),
        Some("poll") => {
            let config = Config::load_or_create()?;
            poll_once(&config).await
        }
        Some("status") => {
            let config = Config::load_or_create()?;
            println!("{}", status_text(&config)?);
            Ok(())
        }
        Some("help") | Some("--help") | Some("-h") => {
            println!("usage: astrcode-pr-review-agent [s5r|poll|status]");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown mode: {other}"),
    }
}

async fn run_s5r() -> std::result::Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXT_ID).version(env!("CARGO_PKG_VERSION"));
    let poll_config = Config::load_or_create().map_err(|error| {
        ErrorPayload::new(
            "config_load_failed",
            format!("load pr review agent config: {error:#}"),
        )
    })?;
    if poll_config.webhook_enabled {
        spawn_webhook_server(poll_config.clone()).map_err(|error| {
            ErrorPayload::new(
                "webhook_start_failed",
                format!("start pr review agent webhook receiver: {error:#}"),
            )
        })?;
    }
    tokio::spawn(async move {
        poll_forever(poll_config).await;
    });

    worker.tool(
        tool("pr_review_agent_status")
            .description("Show GitHub PR review agent status")
            .parameters(json!({ "type": "object", "properties": {} }))
            .build(),
        tool_handler(|_ctx| async move {
            let text = status_text_for_worker();
            Ok(tool_text(text, false))
        }),
    )?;

    worker.command(
        "pr-review-agent",
        "Show GitHub PR review agent status",
        command_handler(|_ctx| async move {
            let text = status_text_for_worker();
            Ok(HandlerResult::effect(
                "ok",
                json!({
                    "kind": "display",
                    "content": text,
                    "is_error": false
                }),
            ))
        }),
    )?;

    worker.run_stdio().await
}

fn status_text_for_worker() -> String {
    match Config::load_or_create().and_then(|config| status_text(&config)) {
        Ok(text) => text,
        Err(error) => format!("astrcode-pr-review-agent status failed: {error:#}"),
    }
}
