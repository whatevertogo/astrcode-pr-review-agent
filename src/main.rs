use anyhow::Result;
use astrcode_extension_worker::worker_prelude::{
    command, command_handler, tool, tool_handler, tool_planner, tool_text, ErrorPayload,
    ExtensionCommandResult, HandlerEffect, HandlerResult, ToolPlan, WireErrorCode, Worker,
};
use astrcode_pr_review_agent::{
    poll_forever, poll_once, spawn_webhook_server, status_text, Config,
};
use serde_json::json;

const EXT_ID: &str = "astrcode-pr-review-agent";
/// Set to skip starting the GitHub poll loop on activation (conformance probes, tests).
const DISABLE_POLL_ENV: &str = "ASTRCODE_PR_REVIEW_AGENT_DISABLE_POLL";

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
        Some("review") => {
            astrcode_pr_review_agent::review_cli(std::env::args().skip(2).collect()).await
        }
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
            println!("usage: astrcode-pr-review-agent [s5r|poll|status|review --repo OWNER/REPO --pr NUMBER --output-dir PATH [--publish]]");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown mode: {other}"),
    }
}

async fn run_s5r() -> std::result::Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXT_ID, env!("CARGO_PKG_VERSION"));

    worker.on_activate(|_config| async move {
        let poll_config = Config::load_or_create().map_err(|error| {
            ErrorPayload::new(
                WireErrorCode::InvalidInput,
                format!("load pr review agent config: {error:#}"),
            )
        })?;
        if poll_config.webhook_enabled {
            spawn_webhook_server(poll_config.clone()).map_err(|error| {
                ErrorPayload::new(
                    WireErrorCode::BackendUnavailable,
                    format!("start pr review agent webhook receiver: {error:#}"),
                )
            })?;
        }
        if std::env::var_os(DISABLE_POLL_ENV).is_none() {
            tokio::spawn(async move {
                poll_forever(poll_config).await;
            });
        }
        Ok(())
    });

    worker.tool(
        tool("pr_review_agent_status")
            .description("Show GitHub PR review agent status")
            .parameters(json!({ "type": "object", "properties": {} }))
            .build(),
        tool_planner(|_ctx| async { Ok(ToolPlan::default()) }),
        tool_handler(|_ctx| async move {
            let text = status_text_for_worker();
            Ok(tool_text(text, false))
        }),
    )?;

    worker.command(
        command("pr-review-agent")
            .description("Show GitHub PR review agent status")
            .build(),
        command_handler(|_ctx| async move {
            let text = status_text_for_worker();
            let data = serde_json::to_value(ExtensionCommandResult::display(text, false)).map_err(
                |error| {
                    ErrorPayload::new(
                        WireErrorCode::SerializationFailed,
                        format!("serialize pr-review-agent command result: {error}"),
                    )
                },
            )?;
            Ok(HandlerResult::effect(HandlerEffect::Ok, data))
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
