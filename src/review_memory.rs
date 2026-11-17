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
            let title = if record.pr_title.trim().is_empty() {
                String::new()
            } else {
                format!(" 「{}」", record.pr_title.trim())
            };
            let url = if record.pr_url.trim().is_empty() {
                String::new()
            } else {
                format!(" {}", record.pr_url.trim())
            };
            parts.push(format!(
                "## Related PR #{}{title}{url} at {} (historical evidence, not a current finding)\n{}",
                record.pr_number, record.head_sha, summary
            ));
            if seen.len() == 3 {
                break;
            }
        }
    }
    Ok(parts.join("\n\n"))
}

/// Recall structured findings already persisted in `State`, including records
/// written before changed paths were appended to review summaries. No session
/// is required, and the persistence format is unchanged.
pub(super) fn structured(trigger: &ReviewTrigger, state: &State) -> String {
    let current_key = pr_key(&trigger.repo, trigger.pr.number);
    let prefix = format!("{}#", trigger.repo);
    let mut related = state
        .pr_review_memory
        .iter()
        .filter(|(key, memory)| {
            key.starts_with(&prefix)
                && (*key == &current_key
                    || memory
                        .posted_findings
                        .values()
                        .chain(memory.summary_findings.values())
                        .any(|finding| {
                            finding.path.as_ref().is_some_and(|path| {
                                trigger.pr.files.iter().any(|file| &file.path == path)
                            })
                        }))
        })
        .collect::<Vec<_>>();
    related.sort_by_key(|(key, memory)| {
        (
            *key != &current_key,
            std::cmp::Reverse(memory.last_reviewed_at),
        )
    });
    let mut parts = Vec::new();
    for (key, memory) in related.into_iter().take(4) {
        let current = key == &current_key;
        let number = key.strip_prefix(&prefix).unwrap_or("");
        let mut seen = BTreeSet::new();
        let mut findings = memory
            .posted_findings
            .iter()
            .chain(memory.summary_findings.iter())
            .filter(|(fingerprint, finding)| {
                seen.insert(fingerprint.as_str())
                    && (current
                        || finding.path.as_ref().is_some_and(|path| {
                            trigger.pr.files.iter().any(|file| &file.path == path)
                        }))
            })
            .map(|(_, finding)| finding)
            .collect::<Vec<_>>();
        findings.sort_by_key(|finding| std::cmp::Reverse(finding.posted_at));
        let entries = findings
            .into_iter()
            .take(12)
            .map(|finding| {
                format!(
                    "- {} {} `{}`:{} (历史提交 {}, 保存状态 {})",
                    finding.priority,
                    finding.title,
                    finding.path.as_deref().unwrap_or("未定位"),
                    finding.line.unwrap_or(0),
                    finding.head_sha,
                    finding.status
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !entries.is_empty() {
            parts.push(format!(
                "## 历史意见 https://github.com/{}/pull/{}\n以下是历史记录，须对照当前代码与讨论核实已修复、仍有效或尚未核实，保存状态不代表当前状态。\n{}",
                trigger.repo, number, entries
            ));
        }
    }
    parts.join("\n\n")
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
