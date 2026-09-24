struct StagedReviewRun {
    path: PathBuf,
    outputs: BTreeMap<(String, String), ReviewBotOutput>,
    identity: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StagedReviewRecord {
    label: String,
    created_at: u64,
    #[serde(default)]
    input_key: String,
    output: ReviewBotOutput,
}

impl StagedReviewRun {
    fn load(trigger: &ReviewTrigger, identity: String) -> Result<Self> {
        let path = staged_review_path(trigger)?;
        let mut outputs = BTreeMap::new();
        if path.exists() {
            let data = fs::read_to_string(&path)
                .with_context(|| format!("read staged review outputs {}", path.display()))?;
            for (line_index, line) in data.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<StagedReviewRecord>(line) {
                    Ok(record) => {
                        if !record.input_key.is_empty() {
                            outputs.insert((record.label, record.input_key), record.output);
                        }
                    },
                    Err(error) => eprintln!(
                        "failed to parse staged review output {} line {}: {error:#}",
                        path.display(),
                        line_index + 1
                    ),
                }
            }
        }
        Ok(Self { path, outputs, identity })
    }

    fn output(&self, label: &str, prompt: &str) -> Option<ReviewBotOutput> {
        self.outputs.get(&(label.to_owned(), self.input_key(prompt))).cloned()
    }

    fn append(&mut self, label: &str, prompt: &str, output: &ReviewBotOutput) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create staged review dir {}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("open staged review output {}", self.path.display()))?;
        let record = StagedReviewRecord {
            label: label.to_owned(),
            created_at: now_epoch(),
            input_key: self.input_key(prompt),
            output: output.clone(),
        };
        let line = serde_json::to_string(&record)?;
        writeln!(file, "{line}")
            .with_context(|| format!("write staged review output {}", self.path.display()))?;
        file.sync_data()?;
        self.outputs.insert((label.to_owned(), record.input_key), output.clone());
        Ok(())
    }

    fn input_key(&self, prompt: &str) -> String {
        use sha2::Digest;
        format!("{:x}", Sha256::digest(format!("{}\n{prompt}", self.identity)))
    }
}

fn staged_review_path(trigger: &ReviewTrigger) -> Result<PathBuf> {
    Ok(agent_dir()?
        .join("staged-runs")
        .join(repo_key(&trigger.repo))
        .join(format!("pr-{}", trigger.pr.number))
        .join(&trigger.pr.head_ref_oid)
        .join(format!("{}.jsonl", sanitize_path_component(&trigger.state_key()))))
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
