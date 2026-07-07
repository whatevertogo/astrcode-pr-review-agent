pub const AGENT_LINE: &str = "我是 whatevertogo 的替身。";
pub const DEFAULT_MARKER: &str = "<!-- astrcode-auto-review -->";
const PR_REVIEW_BOT_PROMPT: &str = include_str!("../prompts/pr-review-bot.md");
const PR_REVIEW_FEW_SHOTS_PROMPT: &str = include_str!("../prompts/few-shots.md");
const ORIENTATION_REVIEW_PROMPT: &str = include_str!("../prompts/orientation-review.md");
const FILE_REVIEW_PROMPT: &str = include_str!("../prompts/file-review.md");
const GLOBAL_REVIEW_PROMPT: &str = include_str!("../prompts/global-review.md");
const STATE_VERSION: u32 = 1;
const STATUS_PENDING: &str = "pending";
const STATUS_RUNNING: &str = "running";
const STATUS_COMMENTED: &str = "commented";
const STATUS_FAILED: &str = "failed";
const STATUS_ABORTED: &str = "aborted";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub github_user: String,
    pub repos: Vec<String>,
    #[serde(default = "default_trusted_comment_authors")]
    pub trusted_comment_authors: Vec<String>,
    #[serde(default = "default_mention_search_limit")]
    pub mention_search_limit: usize,
    pub mention: String,
    pub comment_marker: String,
    #[serde(default)]
    pub webhook_enabled: bool,
    #[serde(default = "default_webhook_listen_addr")]
    pub webhook_listen_addr: String,
    #[serde(default = "default_webhook_path")]
    pub webhook_path: String,
    #[serde(default = "default_webhook_secret_env")]
    pub webhook_secret_env: String,
    pub poll_interval_seconds: u64,
    #[serde(default = "default_poll_fallback_seconds")]
    pub poll_fallback_seconds: u64,
    pub review_timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub auto_review_new_prs: bool,
    #[serde(default)]
    pub auto_review_bootstrap_existing_open_prs: bool,
    #[serde(default = "default_true")]
    pub auto_review_start_comment: bool,
    #[serde(default = "default_true")]
    pub auto_review_failure_comment: bool,
    #[serde(default = "default_worktree_cleanup_delay_seconds")]
    pub worktree_cleanup_delay_seconds: u64,
    #[serde(default = "default_session_cleanup_delay_seconds")]
    pub session_cleanup_delay_seconds: u64,
    #[serde(default = "default_max_inline_comments")]
    pub max_inline_comments: usize,
    #[serde(default = "default_max_advisory_inline_comments")]
    pub max_advisory_inline_comments: usize,
    #[serde(default = "default_max_p3_inline_comments")]
    pub max_p3_inline_comments: usize,
    #[serde(default = "default_inline_confidence_min")]
    pub inline_confidence_min: String,
    #[serde(default = "default_true")]
    pub publish_observations_in_summary: bool,
    #[serde(default = "default_review_context_max_bytes")]
    pub review_context_max_bytes: usize,
    #[serde(default = "default_json_repair_attempts")]
    pub json_repair_attempts: usize,
    #[serde(default = "default_review_pipeline")]
    pub review_pipeline: String,
    #[serde(default = "default_review_shard_max_bytes")]
    pub review_shard_max_bytes: usize,
    #[serde(default = "default_max_files_per_shard")]
    pub max_files_per_shard: usize,
    #[serde(default = "default_max_review_passes_per_pr")]
    pub max_review_passes_per_pr: usize,
    #[serde(default = "default_true")]
    pub publish_no_findings_summary: bool,
    #[serde(default = "default_inline_priority_max")]
    pub inline_priority_max: String,
    #[serde(default = "default_nitpick_inline_priority_max")]
    pub nitpick_inline_priority_max: String,
    #[serde(default = "default_max_nitpick_inline_comments")]
    pub max_nitpick_inline_comments: usize,
    #[serde(default = "default_instruction_context_max_bytes")]
    pub instruction_context_max_bytes: usize,
    #[serde(default = "default_true")]
    pub deterministic_checks_enabled: bool,
    #[serde(default = "default_true")]
    pub full_tests_require_trigger_keyword: bool,
    #[serde(default)]
    pub auto_review_on_synchronize: bool,
    pub memory_dir: String,
    pub worktree_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            github_user: "whatevertogo".into(),
            repos: vec!["VitaDynamics/Vvbot".into(), "whatevertogo/astrcodey".into()],
            trusted_comment_authors: default_trusted_comment_authors(),
            mention_search_limit: default_mention_search_limit(),
            mention: "@whatevertogo".into(),
            comment_marker: DEFAULT_MARKER.into(),
            webhook_enabled: false,
            webhook_listen_addr: default_webhook_listen_addr(),
            webhook_path: default_webhook_path(),
            webhook_secret_env: default_webhook_secret_env(),
            poll_interval_seconds: 5,
            poll_fallback_seconds: default_poll_fallback_seconds(),
            review_timeout_seconds: 1800,
            auto_review_new_prs: true,
            auto_review_bootstrap_existing_open_prs: false,
            auto_review_start_comment: true,
            auto_review_failure_comment: true,
            worktree_cleanup_delay_seconds: default_worktree_cleanup_delay_seconds(),
            session_cleanup_delay_seconds: default_session_cleanup_delay_seconds(),
            max_inline_comments: default_max_inline_comments(),
            max_advisory_inline_comments: default_max_advisory_inline_comments(),
            max_p3_inline_comments: default_max_p3_inline_comments(),
            inline_confidence_min: default_inline_confidence_min(),
            publish_observations_in_summary: true,
            review_context_max_bytes: default_review_context_max_bytes(),
            json_repair_attempts: default_json_repair_attempts(),
            review_pipeline: default_review_pipeline(),
            review_shard_max_bytes: default_review_shard_max_bytes(),
            max_files_per_shard: default_max_files_per_shard(),
            max_review_passes_per_pr: default_max_review_passes_per_pr(),
            publish_no_findings_summary: true,
            inline_priority_max: default_inline_priority_max(),
            nitpick_inline_priority_max: default_nitpick_inline_priority_max(),
            max_nitpick_inline_comments: default_max_nitpick_inline_comments(),
            instruction_context_max_bytes: default_instruction_context_max_bytes(),
            deterministic_checks_enabled: true,
            full_tests_require_trigger_keyword: true,
            auto_review_on_synchronize: false,
            memory_dir: "~/.astrcode/pr-review-agent/memory".into(),
            worktree_dir: "~/.astrcode/pr-review-agent/worktrees".into(),
        }
    }
}

fn default_trusted_comment_authors() -> Vec<String> {
    [
        "whatevertogo",
        "catDforD",
        "letr007",
        "united-pooh",
        "Soulter",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn default_mention_search_limit() -> usize {
    100
}

fn default_true() -> bool {
    true
}

fn default_webhook_listen_addr() -> String {
    "127.0.0.1:3978".into()
}

fn default_webhook_path() -> String {
    "/github/webhook".into()
}

fn default_webhook_secret_env() -> String {
    "ASTRCODE_PR_REVIEW_AGENT_WEBHOOK_SECRET".into()
}

fn default_poll_fallback_seconds() -> u64 {
    5
}

fn default_worktree_cleanup_delay_seconds() -> u64 {
    600
}

fn default_session_cleanup_delay_seconds() -> u64 {
    3 * 24 * 60 * 60
}

fn default_max_inline_comments() -> usize {
    12
}

fn default_max_advisory_inline_comments() -> usize {
    5
}

fn default_max_p3_inline_comments() -> usize {
    4
}

fn default_inline_confidence_min() -> String {
    "medium".into()
}

fn default_review_context_max_bytes() -> usize {
    180_000
}

fn default_json_repair_attempts() -> usize {
    1
}

fn default_review_pipeline() -> String {
    "coverage_first".into()
}

fn default_review_shard_max_bytes() -> usize {
    60_000
}

fn default_max_files_per_shard() -> usize {
    4
}

fn default_max_review_passes_per_pr() -> usize {
    8
}

fn default_inline_priority_max() -> String {
    "P2".into()
}

fn default_nitpick_inline_priority_max() -> String {
    "P3".into()
}

fn default_max_nitpick_inline_comments() -> usize {
    4
}

fn default_instruction_context_max_bytes() -> usize {
    24_000
}

impl Config {
    pub fn load_or_create() -> Result<Self> {
        let dir = agent_dir()?;
        fs::create_dir_all(&dir)?;
        let path = dir.join("config.json");
        if !path.exists() {
            let config = Self::default();
            write_json_pretty(&path, &config)?;
            return Ok(config);
        }
        let config = serde_json::from_str(&fs::read_to_string(&path)?)
            .with_context(|| format!("parse {}", path.display()))?;
        Ok(config)
    }

    fn memory_dir_path(&self) -> Result<PathBuf> {
        expand_home(&self.memory_dir)
    }

    fn worktree_dir_path(&self) -> Result<PathBuf> {
        expand_home(&self.worktree_dir)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct State {
    #[serde(default = "default_state_version")]
    version: u32,
    #[serde(default)]
    processed_comments: BTreeMap<String, ProcessedComment>,
    #[serde(default)]
    seen_open_prs: BTreeMap<String, SeenOpenPr>,
    #[serde(default)]
    auto_pr_reviews: BTreeMap<String, AutoPrReview>,
    #[serde(default)]
    auto_review_baselined_repos: BTreeMap<String, u64>,
    #[serde(default)]
    pr_sessions: BTreeMap<String, PrSession>,
    #[serde(default)]
    webhook_deliveries: BTreeMap<String, WebhookDelivery>,
    #[serde(default)]
    event_queue: Vec<QueuedWebhookEvent>,
    #[serde(default)]
    pr_review_memory: BTreeMap<String, PrReviewMemory>,
    #[serde(default)]
    last_reconciliation_at: Option<u64>,
    #[serde(default)]
    last_deterministic_checks: Vec<VerificationItem>,
    #[serde(default)]
    last_run: Option<RunStatus>,
}

fn default_state_version() -> u32 {
    STATE_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PrSession {
    repo: String,
    pr_number: u64,
    session_id: String,
    worktree: String,
    last_head_sha: String,
    created_at: u64,
    updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProcessedComment {
    repo: String,
    pr_number: u64,
    comment_id: u64,
    head_sha: String,
    session_id: Option<String>,
    status: String,
    started_at: u64,
    finished_at: Option<u64>,
    review_comment_url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SeenOpenPr {
    repo: String,
    pr_number: u64,
    head_sha: String,
    first_seen_at: u64,
    last_seen_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AutoPrReview {
    repo: String,
    pr_number: u64,
    head_sha: String,
    session_id: Option<String>,
    status: String,
    started_at: u64,
    finished_at: Option<u64>,
    start_comment_url: Option<String>,
    review_comment_url: Option<String>,
    failure_comment_url: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunStatus {
    started_at: u64,
    finished_at: Option<u64>,
    status: String,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WebhookDelivery {
    delivery_id: String,
    event: String,
    action: Option<String>,
    status: String,
    received_at: u64,
    processed_at: Option<u64>,
    message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueuedWebhookEvent {
    id: String,
    delivery_id: String,
    event: String,
    action: String,
    repo: String,
    pr_number: u64,
    head_sha: String,
    base_ref_name: String,
    pr_title: String,
    pr_url: String,
    pr_body: Option<String>,
    comment_id: Option<u64>,
    comment_body: Option<String>,
    comment_author: Option<String>,
    comment_url: Option<String>,
    status: String,
    queued_at: u64,
    processed_at: Option<u64>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpooledWebhookEvent {
    event: String,
    delivery_id: String,
    payload: Value,
    spooled_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PrReviewMemory {
    #[serde(default)]
    reviewed_ranges: Vec<ReviewedRange>,
    #[serde(default)]
    posted_findings: BTreeMap<String, FindingMemory>,
    #[serde(default)]
    summary_findings: BTreeMap<String, FindingMemory>,
    #[serde(default)]
    observations: Vec<ObservationMemory>,
    #[serde(default)]
    last_head_sha: Option<String>,
    #[serde(default)]
    last_reviewed_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewedRange {
    base_sha: String,
    head_sha: String,
    reviewed_at: u64,
    session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FindingMemory {
    fingerprint: String,
    priority: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    confidence: String,
    title: String,
    path: Option<String>,
    line: Option<u64>,
    head_sha: String,
    status: String,
    posted_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObservationMemory {
    confidence: String,
    category: String,
    title: String,
    path: Option<String>,
    line: Option<u64>,
    summary: String,
    head_sha: String,
    recorded_at: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    number: u64,
    title: String,
    url: String,
    head_ref_oid: String,
    base_ref_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    files: Vec<PullRequestFile>,
    #[serde(default)]
    author: Option<GhUser>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestFile {
    path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestApiFile {
    filename: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
    #[serde(default)]
    changes: u64,
    #[serde(default)]
    patch: Option<String>,
    #[serde(default)]
    previous_filename: Option<String>,
}

#[derive(Debug, Clone)]
struct ReviewContext {
    text: String,
    commentable_lines: BTreeSet<CommentLineKey>,
    non_commentable_files: Vec<String>,
    truncated: bool,
    files: Vec<ReviewFileContext>,
}

#[derive(Debug, Clone)]
struct ReviewFileContext {
    path: String,
    status: String,
    additions: u64,
    deletions: u64,
    changes: u64,
    previous_filename: Option<String>,
    annotated_patch: String,
    kind: ReviewFileKind,
    bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewFileKind {
    Code,
    Docs,
    Generated,
    NoPatch,
    Oversized,
}

impl ReviewFileKind {
    fn coverage_status(self) -> CoverageStatus {
        match self {
            Self::Code | Self::Docs => CoverageStatus::Reviewed,
            Self::Generated => CoverageStatus::SkippedGenerated,
            Self::NoPatch => CoverageStatus::NoPatch,
            Self::Oversized => CoverageStatus::OversizedPartial,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Docs => "docs",
            Self::Generated => "generated",
            Self::NoPatch => "no_patch",
            Self::Oversized => "oversized",
        }
    }
}

#[derive(Debug, Clone)]
struct ReviewShard {
    index: usize,
    files: Vec<ReviewFileContext>,
    bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoverageStatus {
    Reviewed,
    NoPatch,
    SkippedGenerated,
    OversizedPartial,
    Failed,
}

impl CoverageStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reviewed => "reviewed",
            Self::NoPatch => "no_patch",
            Self::SkippedGenerated => "skipped_generated",
            Self::OversizedPartial => "oversized_partial",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
struct CoverageEntry {
    path: String,
    status: CoverageStatus,
    reason: String,
}

#[derive(Debug, Clone, Default)]
struct ReviewCoverage {
    entries: BTreeMap<String, CoverageEntry>,
}

impl ReviewCoverage {
    fn mark(&mut self, path: impl Into<String>, status: CoverageStatus, reason: impl Into<String>) {
        let path = path.into();
        let replace = self
            .entries
            .get(&path)
            .map(|entry| entry.status != CoverageStatus::Reviewed)
            .unwrap_or(true);
        if replace {
            self.entries.insert(
                path.clone(),
                CoverageEntry {
                    path,
                    status,
                    reason: reason.into(),
                },
            );
        }
    }

    fn reviewed_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.status == CoverageStatus::Reviewed)
            .count()
    }

    fn total_count(&self) -> usize {
        self.entries.len()
    }

    fn incomplete_entries(&self) -> Vec<&CoverageEntry> {
        self.entries
            .values()
            .filter(|entry| entry.status != CoverageStatus::Reviewed)
            .collect()
    }

    fn summary_lines(&self) -> String {
        if self.entries.is_empty() {
            return "- No files in manifest".into();
        }
        self.entries
            .values()
            .map(|entry| {
                format!(
                    "- `{}`: {} ({})",
                    entry.path,
                    entry.status.as_str(),
                    entry.reason
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CommentLineKey {
    path: String,
    side: CommentSide,
    line: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CommentSide {
    Right,
    Left,
}

impl CommentSide {
    fn as_github(self) -> &'static str {
        match self {
            Self::Right => "RIGHT",
            Self::Left => "LEFT",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "RIGHT" => Some(Self::Right),
            "LEFT" => Some(Self::Left),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ReviewBotOutput {
    #[serde(default)]
    confirmed_findings: Vec<ReviewFinding>,
    #[serde(default)]
    advisory_findings: Vec<ReviewFinding>,
    #[serde(default, deserialize_with = "deserialize_observations")]
    observations: Vec<ReviewObservation>,
    #[serde(default)]
    files_reviewed: Vec<String>,
    #[serde(default)]
    investigation_log: Vec<String>,
    #[serde(default, skip_deserializing)]
    verification: Vec<VerificationItem>,
    #[serde(default)]
    residual_risk: Vec<String>,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ReviewFinding {
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    side: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    issue: Option<String>,
    #[serde(default)]
    evidence: Option<String>,
    #[serde(default)]
    project_context: Option<String>,
    #[serde(default)]
    impact: Option<String>,
    #[serde(default)]
    fix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ReviewObservation {
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    evidence: Option<String>,
    #[serde(default)]
    project_context: Option<String>,
    #[serde(default)]
    impact: Option<String>,
    #[serde(default)]
    next_step: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FindingKind {
    Confirmed,
    Advisory,
}

impl FindingKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "Confirmed",
            Self::Advisory => "Advisory",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FinalCommentOutput {
    #[serde(default)]
    report: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct VerificationItem {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Clone)]
struct ValidatedFinding {
    priority: String,
    kind: FindingKind,
    confidence: String,
    category: String,
    path: String,
    side: CommentSide,
    line: u64,
    title: String,
    issue: String,
    evidence: String,
    project_context: String,
    impact: String,
    fix: String,
    original_index: usize,
}

#[derive(Debug, Clone)]
struct UnplacedFinding {
    priority: String,
    kind: String,
    confidence: String,
    title: String,
    path: Option<String>,
    side: Option<String>,
    line: Option<u64>,
    reason: String,
}

#[derive(Debug, Clone)]
struct ValidatedReview {
    inline_findings: Vec<ValidatedFinding>,
    summary_findings: Vec<ValidatedFinding>,
    unplaced_findings: Vec<UnplacedFinding>,
    observations: Vec<ReviewObservation>,
    investigation_log: Vec<String>,
    verification: Vec<VerificationItem>,
    residual_risk: Vec<String>,
    summary: Option<String>,
    coverage: Option<ReviewCoverage>,
    debug_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct PublishedReview {
    url: Option<String>,
    inline_review_url: Option<String>,
    inline_review_id: Option<u64>,
    summary_body: String,
    inline_comments_posted: usize,
    unplaced_findings_count: usize,
    highest_risk: Option<String>,
    verification: Vec<VerificationItem>,
    posted_findings: Vec<FindingMemory>,
}

#[derive(Debug, Clone)]
struct PostedPullReview {
    url: Option<String>,
    id: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct GhUser {
    login: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchPullRequest {
    number: u64,
    repository: SearchRepository,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchRepository {
    name_with_owner: String,
}

#[derive(Debug, Clone, Deserialize)]
struct IssueComment {
    id: u64,
    body: Option<String>,
    user: Option<GhUser>,
    html_url: Option<String>,
    created_at: Option<String>,
}

#[derive(Debug, Clone)]
struct ReviewTrigger {
    repo: String,
    pr: PullRequest,
    kind: ReviewTriggerKind,
}

#[derive(Debug, Clone)]
enum ReviewTriggerKind {
    MentionComment(IssueComment),
    NewPullRequest,
}

impl ReviewTrigger {
    fn comment(&self) -> Option<&IssueComment> {
        match &self.kind {
            ReviewTriggerKind::MentionComment(comment) => Some(comment),
            ReviewTriggerKind::NewPullRequest => None,
        }
    }

    fn is_auto_review(&self) -> bool {
        matches!(self.kind, ReviewTriggerKind::NewPullRequest)
    }

    fn state_key(&self) -> String {
        match self.comment() {
            Some(comment) => processed_key(&self.repo, self.pr.number, comment.id),
            None => pr_key(&self.repo, self.pr.number),
        }
    }

    fn trigger_kind_name(&self) -> &'static str {
        match self.kind {
            ReviewTriggerKind::MentionComment(_) => "mention_comment",
            ReviewTriggerKind::NewPullRequest => "new_pull_request",
        }
    }
}

#[derive(Debug, Clone)]
struct PromptMemoryPaths {
    repo_index: PathBuf,
    pr_memory: PathBuf,
    runs_log: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunInfo {
    port: u16,
    auth_token: String,
}
