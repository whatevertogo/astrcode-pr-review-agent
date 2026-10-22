//! Inject relevant history, leaving the full index available for targeted retrieval.
use super::*;

pub(super) fn relevant(config: &Config, trigger: &ReviewTrigger) -> Result<String> {
    let mut parts = Vec::new();
    let current = pr_memory_path(config, &trigger.repo, trigger.pr.number)?;
    if current.exists() {
        parts.push(format!(
            "## This PR history (verify against the current diff)\n{}",
            recent_lines(&fs::read_to_string(&current)?, 6_000)
        ));
    }
    let runs = config.memory_dir_path()?.join("runs.jsonl");
    if runs.exists() {
        let records = fs::read_to_string(runs)?;
        let mut seen = BTreeSet::new();
        for record in records
            .lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<ReviewRecord>(l).ok())
        {
            if record.repo != trigger.repo || record.pr_number == trigger.pr.number {
                continue;
            }
            // Exact changed paths are stronger evidence of relevance than recency or common words.
            if !trigger
                .pr
                .files
                .iter()
                .any(|f| record.summary.contains(&f.path))
                || !seen.insert(record.pr_number)
            {
                continue;
            }
            let mut summary = record.summary;
            truncate_text(&mut summary, 1_800);
            parts.push(format!(
                "## Related PR #{} at {} (historical evidence, not a current finding)\n{}",
                record.pr_number, record.head_sha, summary
            ));
            if seen.len() == 3 {
                break;
            }
        }
    }
    Ok(parts.join("\n\n"))
}

fn recent_lines(text: &str, budget: usize) -> String {
    let mut bytes = 0;
    let mut lines = Vec::new();
    for line in text.lines().rev() {
        if bytes + line.len() + 1 > budget {
            if lines.is_empty() {
                let mut excerpt = line.to_owned();
                truncate_text(&mut excerpt, budget);
                return excerpt;
            }
            break;
        }
        bytes += line.len() + 1;
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n")
}
