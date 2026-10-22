//! Keep complete diffs together when possible; split large patches without dropping lines.
use super::*;

pub(super) fn plan(
    config: &Config,
    context: &ReviewContext,
    prompt_overhead: usize,
) -> Result<Vec<ReviewShard>> {
    let mut files: Vec<_> = context
        .files
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                ReviewFileKind::Code | ReviewFileKind::Docs | ReviewFileKind::Oversized
            )
        })
        .cloned()
        .collect();
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let bytes = files
        .iter()
        .map(|f| f.annotated_patch.len() + 2)
        .sum::<usize>();
    // UTF-8 bytes are a conservative input budget, not a claimed model token count.
    let full_budget = config
        .review_context_max_bytes
        .saturating_sub(prompt_overhead);
    let is_document = |file: &ReviewFileContext| is_docs_path(&file.path.to_ascii_lowercase());
    // Fitting in context does not ensure attention across distinct consumers.
    // Keep small mixed PRs together; larger ones give operational docs their own pass.
    let separate_consumers = files.len() > config.max_files_per_shard.max(1)
        && files.iter().any(is_document)
        && files.iter().any(|file| !is_document(file));
    if bytes <= full_budget && !separate_consumers {
        return Ok(vec![ReviewShard {
            index: 0,
            files,
            bytes,
        }]);
    }
    let budget = config
        .review_shard_max_bytes
        .min(config.review_context_max_bytes)
        .saturating_sub(prompt_overhead);
    anyhow::ensure!(
        budget > 0,
        "review instructions exhaust the shard input budget"
    );
    if separate_consumers {
        files.sort_by_key(is_document);
    }
    let mut shards: Vec<ReviewShard> = Vec::new();
    for file in files {
        for patch in split_patch(&file.annotated_patch, budget.saturating_sub(2))
            .with_context(|| format!("split review patch for {}", file.path))?
        {
            let bytes = patch.len() + 2;
            // A path may occur in several passes, but never twice in the same pass.
            let fits = shards.last().is_some_and(|s| {
                s.bytes + bytes <= budget
                    && !s.files.iter().any(|f| f.path == file.path)
                    && (!separate_consumers || is_document(&s.files[0]) == is_document(&file))
            });
            if !fits {
                shards.push(ReviewShard {
                    index: shards.len(),
                    files: Vec::new(),
                    bytes: 0,
                });
            }
            let shard = shards.last_mut().expect("a shard was just created");
            let mut part = file.clone();
            part.annotated_patch = patch;
            part.bytes = part.annotated_patch.len();
            shard.files.push(part);
            shard.bytes += bytes;
        }
    }
    Ok(shards)
}

fn split_patch(patch: &str, budget: usize) -> Result<Vec<String>> {
    if patch.len() <= budget {
        return Ok(vec![patch.to_owned()]);
    }
    let mut offset = 0;
    let start = patch
        .split_inclusive('\n')
        .find_map(|line| {
            let start = offset;
            offset += line.len();
            line.starts_with("@@ ").then_some(start)
        })
        .context("oversized patch has no hunk header")?;
    let header = &patch[..start];
    let mut parts = Vec::new();
    let mut current = header.to_owned();
    let mut hunk = "";
    let mut has_lines = false;
    // Prefer hunk boundaries; only a hunk larger than the budget is split by line.
    let hunks: Vec<_> = patch[start..].split_inclusive('\n').collect();
    let mut index = 0;
    while index < hunks.len() {
        let line = hunks[index];
        if line.starts_with("@@ ") {
            let end = hunks[index + 1..]
                .iter()
                .position(|l| l.starts_with("@@ "))
                .map_or(hunks.len(), |offset| index + 1 + offset);
            let hunk_bytes = hunks[index..end].iter().map(|l| l.len()).sum::<usize>();
            if has_lines && current.len() + hunk_bytes > budget {
                parts.push(current);
                current = header.to_owned();
                has_lines = false;
            }
            hunk = line;
        }
        if current.len() + line.len() > budget {
            anyhow::ensure!(
                has_lines,
                "one annotated line exceeds the review input budget"
            );
            parts.push(current);
            current = format!("{header}{hunk}");
            has_lines = false;
        }
        anyhow::ensure!(
            current.len() + line.len() <= budget,
            "one annotated line exceeds the review input budget"
        );
        current.push_str(line);
        has_lines |= !line.starts_with("@@ ");
        index += 1;
    }
    if has_lines {
        parts.push(current);
    }
    Ok(parts)
}

/// Coverage is awarded only after every assigned part of a path succeeds.
pub(super) fn coverage(
    context: &ReviewContext,
    shards: &[ReviewShard],
    completed: &BTreeMap<usize, BTreeSet<String>>,
) -> ReviewCoverage {
    let mut coverage = initial_coverage(context);
    for file in &context.files {
        let assigned: Vec<_> = shards
            .iter()
            .filter(|s| s.files.iter().any(|f| f.path == file.path))
            .collect();
        if assigned.is_empty() {
            continue;
        }
        let passed = assigned
            .iter()
            .filter(|s| {
                completed
                    .get(&s.index)
                    .is_some_and(|paths| paths.contains(&file.path))
            })
            .count();
        let status = if passed == assigned.len() {
            CoverageStatus::Reviewed
        } else if passed > 0 {
            CoverageStatus::OversizedPartial
        } else {
            CoverageStatus::Failed
        };
        let missing: Vec<_> = assigned
            .iter()
            .filter(|s| {
                !completed
                    .get(&s.index)
                    .is_some_and(|paths| paths.contains(&file.path))
            })
            .map(|s| s.index + 1)
            .collect();
        let mut reason = format!("reviewed {passed}/{} assigned patch parts", assigned.len());
        if !missing.is_empty() {
            reason.push_str(&format!("; incomplete file passes: {missing:?}"));
        }
        coverage.mark(&file.path, status, reason);
    }
    coverage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_diff_split_hunks_and_partial_coverage() {
        let mut context = crate::tests::test_review_context();
        let original = context.files[0].clone();
        context.files = (0..6)
            .map(|i| ReviewFileContext {
                path: format!("src/{i}.rs"),
                ..original.clone()
            })
            .collect();
        let mut config = Config::default();
        assert_eq!(plan(&config, &context, 100).unwrap().len(), 1);
        context.files[0].path = "docs/guide.md".into();
        context.files[0].kind = ReviewFileKind::Docs;
        let cohorts = plan(&config, &context, 100).unwrap();
        assert_eq!(cohorts.len(), 2);
        assert_eq!(cohorts.iter().map(|s| s.files.len()).sum::<usize>(), 6);
        assert!(cohorts.iter().all(|s| s
            .files
            .iter()
            .all(|f| { is_docs_path(&f.path) == is_docs_path(&s.files[0].path) })));
        let patch = format!(
            "--- file: large.rs\n@@ -1,20 +1,20 @@\n{}@@ -40,5 +40,5 @@\n{}",
            (1..=20)
                .map(|i| format!("RIGHT {i} +    line_{i}\n"))
                .collect::<String>(),
            (40..45)
                .map(|i| format!("LEFT {i} -    old_{i}\n"))
                .collect::<String>()
        );
        context.files = vec![ReviewFileContext {
            path: "large.rs".into(),
            bytes: patch.len(),
            annotated_patch: patch.clone(),
            kind: ReviewFileKind::Oversized,
            ..original
        }];
        config.review_context_max_bytes = 250;
        config.review_shard_max_bytes = 250;
        let shards = plan(&config, &context, 50).unwrap();
        let source_lines = |s: &str| {
            s.lines()
                .filter(|l| l.starts_with("RIGHT") || l.starts_with("LEFT"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let emitted = shards
            .iter()
            .flat_map(|s| {
                assert!(s.bytes + 50 <= 250);
                source_lines(&s.files[0].annotated_patch)
            })
            .collect::<Vec<_>>();
        assert_eq!(emitted, source_lines(&patch));
        let unusual_name = patch.replacen("--- file: large.rs", "--- file: @@ literal.rs", 1);
        let parts = split_patch(&unusual_name, 198).unwrap();
        assert_eq!(
            parts
                .iter()
                .flat_map(|p| source_lines(p))
                .collect::<Vec<_>>(),
            source_lines(&patch)
        );
        let mut completed = BTreeMap::new();
        for shard in &shards {
            completed.insert(shard.index, BTreeSet::from(["large.rs".into()]));
        }
        assert_eq!(coverage(&context, &shards, &completed).reviewed_count(), 1);
        completed.remove(&shards.last().unwrap().index);
        assert_eq!(coverage(&context, &shards, &completed).reviewed_count(), 0);
        assert!(split_patch("--- file: huge\n@@ -1 +1 @@\nRIGHT 1 +xxxxxxxx", 12).is_err());
    }
}
