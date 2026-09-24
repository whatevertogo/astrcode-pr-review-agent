//! cargo run --example render_review -- path/to/result.json > preview.md
use anyhow::{Context, Result};

fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .context("usage: render_review RESULT.json")?;
    let raw = std::fs::read_to_string(path)?;
    print!("{}", astrcode_pr_review_agent::render_review_result(&raw)?);
    Ok(())
}
