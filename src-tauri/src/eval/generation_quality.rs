//! Deterministic, synthetic-only generation-quality bake-off for Murmur's real AI surfaces.
//!
//! The scorer and manifest contracts run in normal `cargo test --lib`. Real Qwen/Codex calls are
//! isolated in one ignored, environment-driven test. A green unit test proves the evaluator's
//! oracles and wiring, never model quality; only a committed real-run artifact may make that claim.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::agent::{AgentOutcome, ToolExecutor};
use crate::error::{AppError, Result};
use crate::reason::{GenOptions, LocalReasoner};
use crate::settings::AppConfig;
use crate::storage::models::{NoteAssistRequest, NoteCitation};
use crate::summarize::provider::{MeetingMeta, SummarizeRequest, SummarizerProvider};
use crate::summarize::roles::Role;
use crate::tools::ToolSpec;

const MANIFEST_JSON: &str = include_str!("fixtures/local-cloud-quality.json");
const QWEN4_ID: &str = "qwen3-4b-instruct-2507-q4-k-m";
const QWEN1_ID: &str = "qwen3-1.7b-q4-k-m";
const QWEN4_FILENAME: &str = "Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf";
const QWEN1_FILENAME: &str = "Qwen_Qwen3-1.7B-Q4_K_M.gguf";
const QWEN4_BYTES: u64 = 2_497_280_736;
const QWEN1_BYTES: u64 = 1_282_439_584;
const QWEN4_SHA256: &str = "2fde00ce69dd4899c70d020845e2638353015bba0fdf161b3eb965f2bca4464e";
const QWEN1_SHA256: &str = "72c5c3cb38fa32d5256e2fe30d03e7a64c6c79e668ad84057e3bd66e250b24fb";
const SOL_ID: &str = "gpt-5.6-sol-requested-high";
const CODEX_MODEL: &str = "gpt-5.6-sol";
const CODEX_EFFORT: &str = "high";
const COMMITTED_EVIDENCE_MANIFEST: &str = "eval/results/2026-08-05-qwen-vs-gpt-sol-evidence.json";
const SOURCE_FINGERPRINT_FILES: &[&str] = &[
    "src/eval/generation_quality.rs",
    "src/eval/generation_retrieval.rs",
    "src/eval/fixtures/local-cloud-quality.json",
    "src/eval/fixtures/rag-bakeoff-synthetic.json",
    "src/eval/bakeoff.rs",
    "src/eval/corpus.rs",
    "src/eval/mod.rs",
    "Cargo.toml",
    ".cargo/config.toml",
    "../Cargo.toml",
    "../Cargo.lock",
    "../.cargo/config.toml",
    "../crates/murmur-brain/Cargo.toml",
    "../eval/results/validate_generation_quality_repeats.py",
    "src/summarize/local.rs",
    "src/summarize/claude_code.rs",
    "src/summarize/codex_cli.rs",
    "src/summarize/mod.rs",
    "src/summarize/provider.rs",
    "src/summarize/redact.rs",
    "src/summarize/ner_deberta.rs",
    "src/summarize/egress_log.rs",
    "src/summarize/meta.rs",
    "src/summarize/roles.rs",
    "src/summarize/action_items.rs",
    "src/summarize/temporal.rs",
    "src/summarize/related_context.rs",
    "src/summarize/template.rs",
    "src/summarize/chat.rs",
    "src/summarize/vault_chat.rs",
    "src/commands/enrich.rs",
    "src/commands/mod.rs",
    "src/commands/ask.rs",
    "src/agent.rs",
    "src/pipeline.rs",
    "src/transcribe/live.rs",
    "src/transcribe/bullets.rs",
    "src/transcribe/model.rs",
    "src/transcribe/mod.rs",
    "src/transcribe/types.rs",
    "src/audio/wake.rs",
    "src/voice_action.rs",
    "src/tools.rs",
    "src/facts.rs",
    "src/user_memory.rs",
    "src/commands/facts.rs",
    "src/brain_reactions.rs",
    "src/storage/db.rs",
    "src/storage/egress_store.rs",
    "src/storage/meetings_store.rs",
    "src/storage/notes_store.rs",
    "src/storage/models.rs",
    "src/storage/mod.rs",
    "src/storage/graph_store.rs",
    "src/embed.rs",
    "src/embed/candle_bert.rs",
    "src/perf.rs",
    "src/settings/config.rs",
    "src/settings/mod.rs",
    "src/settings/postures.rs",
    "src/prompts.rs",
    "src/reason.rs",
    "src/reason/sidecar.rs",
    "src/error.rs",
    "src/links.rs",
    "../crates/murmur-brain/src/main.rs",
    "../crates/murmur-brain/src/brain_ipc.rs",
];
const REQUIRED_SOURCE_FINGERPRINT_FILES: &[&str] = &[
    "src/settings/postures.rs",
    "src/summarize/claude_code.rs",
    "src/summarize/action_items.rs",
    "src/summarize/temporal.rs",
    "src/summarize/related_context.rs",
    "src/summarize/meta.rs",
    "src/embed/candle_bert.rs",
    "src/perf.rs",
    "src/storage/models.rs",
    "src/storage/meetings_store.rs",
    "src/storage/notes_store.rs",
    "src/storage/graph_store.rs",
    "src/transcribe/model.rs",
    "src/transcribe/types.rs",
    "src/audio/wake.rs",
    "src/error.rs",
    "src/links.rs",
    "Cargo.toml",
    "../Cargo.toml",
    "../Cargo.lock",
    ".cargo/config.toml",
    "../.cargo/config.toml",
    "../crates/murmur-brain/Cargo.toml",
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct QualityManifest {
    schema_version: u32,
    synthetic_only: bool,
    cases: Vec<QualityCase>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct QualityCase {
    id: String,
    surface: Surface,
    language: Language,
    model_class: ModelClass,
    #[serde(default)]
    holdout: bool,
    #[serde(default)]
    sol_reference_only: bool,
    #[serde(default)]
    transcript: String,
    #[serde(default)]
    question: String,
    #[serde(default)]
    date_iso: String,
    #[serde(default)]
    title_hint: String,
    #[serde(default)]
    vault_titles: Vec<String>,
    #[serde(default)]
    labeled: bool,
    #[serde(default)]
    diarized_others: bool,
    #[serde(default)]
    duration_s: i64,
    #[serde(default)]
    action: String,
    #[serde(default)]
    selection: String,
    #[serde(default)]
    before: String,
    #[serde(default)]
    previous_bullets: String,
    #[serde(default)]
    tool_result: String,
    #[serde(default)]
    search_result: String,
    #[serde(default)]
    search_terms: Vec<String>,
    #[serde(default)]
    floor_corpus: String,
    /// Candidate-input privacy inventory. These names are declared independently of the scorer's
    /// expected output and are replaced identically for every arm before the canonical firewall.
    #[serde(default)]
    synthetic_redaction_entities: Vec<String>,
    expected: ExpectedOutput,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Surface {
    Summary,
    MeetingChat,
    NoteAssist,
    AskVault,
    LiveCurrent,
    LiveBullets,
    /// Historical artifact label retained for schema compatibility. The final fixture binds these
    /// cases to the Fully Local post-call HEAVY route; live extraction remains covered separately by
    /// `LiveCurrent` / `LiveBullets` on the light lane.
    LightExtraction,
}

impl Surface {
    fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::MeetingChat => "meeting_chat",
            Self::NoteAssist => "note_assist",
            Self::AskVault => "ask_vault",
            Self::LiveCurrent => "live_current",
            Self::LiveBullets => "live_bullets",
            Self::LightExtraction => "light_extraction",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Language {
    Pl,
    En,
}

impl Language {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pl => "pl",
            Self::En => "en",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ModelClass {
    Heavy,
    Light,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct ExpectedOutput {
    required_groups: Vec<Vec<String>>,
    #[serde(default)]
    critical_groups: Vec<Vec<String>>,
    #[serde(default)]
    forbidden_terms: Vec<String>,
    #[serde(default)]
    forbidden_relations: Vec<RelationRequirement>,
    #[serde(default)]
    language_markers: Vec<String>,
    #[serde(default)]
    section_requirements: Vec<SectionRequirement>,
    #[serde(default)]
    forbidden_section_requirements: Vec<SectionRequirement>,
    format: OutputFormat,
    #[serde(default)]
    max_words: Option<usize>,
    #[serde(default)]
    max_bullets: Option<usize>,
    #[serde(default)]
    max_tasks: Option<usize>,
    #[serde(default)]
    required_tools: Vec<String>,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    required_provenance: Vec<Vec<String>>,
    #[serde(default)]
    relation_requirements: Vec<RelationRequirement>,
    #[serde(default)]
    conditional_relations: Vec<ConditionalRelationRequirement>,
    #[serde(default)]
    task_owner_requirements: Vec<TaskOwnerRequirement>,
    #[serde(default)]
    allowed_entities: Vec<String>,
    #[serde(default)]
    structured_facts: Vec<StructuredFact>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct StructuredFact {
    entity: String,
    predicate: String,
    object: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SectionRequirement {
    headings: Vec<String>,
    groups: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RelationRequirement {
    groups: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConditionalRelationRequirement {
    #[serde(default)]
    headings: Vec<String>,
    when_groups: Vec<Vec<String>>,
    then_groups: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskOwnerRequirement {
    headings: Vec<String>,
    owners: Vec<String>,
    task_groups: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OutputFormat {
    SummaryMarkdown,
    PlainAnswer,
    FaithfulEdit,
    Shorter,
    TaskList,
    BulletList,
    AskAnswer,
    LiveAnswer,
    LiveBullets,
    StructuredFacts,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DimensionVerdict {
    Pass,
    Fail,
    NotMeasured,
    NotApplicable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QualityDimensions {
    /// Retrieval is deliberately not inferred from fixture-injected results. Ask/live cases use a
    /// controlled corpus, so this lane is `not_measured`; non-retrieval surfaces are `not_applicable`.
    retrieval_quality: DimensionVerdict,
    /// Tool choice, staged-read policy, and branch convergence on actual agent-loop routes only.
    tool_agent_execution: DimensionVerdict,
    /// Postprocessed product output: content/format/language/provenance/closed-world/state contract.
    final_product_output_contract: DimensionVerdict,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct OracleScore {
    diagnostic_score: f64,
    case_pass: bool,
    critical_failure: bool,
    required_groups_hit: usize,
    required_groups_total: usize,
    format_pass: bool,
    section_pass: bool,
    language_pass: bool,
    forbidden_pass: bool,
    constraint_pass: bool,
    provenance_pass: bool,
    tool_policy_pass: bool,
    relation_pass: bool,
    state_application_pass: bool,
    branch_convergence_pass: bool,
    closed_world_pass: bool,
    structured_labels_pass: bool,
    critical_errors: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseResult {
    case_id: String,
    case_payload_sha256: String,
    surface: Surface,
    language: Language,
    model_class: ModelClass,
    holdout: bool,
    route_input_sha256: String,
    generation_profile: String,
    product_route: String,
    comparison_scope: ComparisonScope,
    route_input_chars: usize,
    output_chars: usize,
    output_sha256: String,
    duration_ms: u64,
    output: String,
    surface_output: Option<String>,
    surface_output_sha256: Option<String>,
    raw_model_output: Option<String>,
    raw_model_output_sha256: Option<String>,
    raw_model_format_pass: Option<bool>,
    structured_schema_pass: Option<bool>,
    structured_labels_pass: Option<bool>,
    structured_envelope_pass: Option<bool>,
    error: Option<String>,
    tool_steps: Vec<String>,
    tool_policy_score: Option<f64>,
    tool_policy_pass: Option<bool>,
    state_application_pass: Option<bool>,
    branch_converged: Option<bool>,
    provenance: Vec<String>,
    provenance_sha256: String,
    tool_steps_sha256: String,
    egress_receipt_start_ordinal: Option<u64>,
    egress_receipt_end_ordinal: Option<u64>,
    egress_receipt_count: u64,
    egress_receipt_sha256: String,
    dimensions: QualityDimensions,
    score: OracleScore,
    case_record_sha256: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ComparisonScope {
    ProductPath,
    OfflineReferenceCeiling,
}

/// Whether a local/reference pair isolates the candidate behind the same product interface or
/// intentionally compares the product's backend-specific route. Keeping this explicit prevents a
/// route/orchestration delta from being mislabeled as a pure model-quality delta.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PairComparisonKind {
    RouteSpecificProductSystem,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ArmMetadata {
    arm_id: String,
    model_requested: String,
    effort: Option<String>,
    effort_transport: Option<String>,
    effort_effective_attested: Option<bool>,
    model_class: String,
    model_filename: Option<String>,
    model_bytes: Option<u64>,
    model_sha256: Option<String>,
    runtime_version: Option<String>,
    runtime_sha256: Option<String>,
    sidecar_idle_secs: Option<u64>,
    sidecar_ready_secs: Option<u64>,
    sidecar_hard_cap_secs: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DimensionAggregate {
    observations: usize,
    applicable_observations: usize,
    measured_observations: usize,
    passed_observations: usize,
    failed_observations: usize,
    not_measured_observations: usize,
    not_applicable_observations: usize,
    coverage_rate: Option<f64>,
    pass_rate: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Aggregate {
    cases: usize,
    call_success_rate: f64,
    case_pass_rate: f64,
    critical_failure_cases: usize,
    diagnostic_score_mean: f64,
    tool_policy_mean: Option<f64>,
    mean_duration_ms: u64,
    retrieval_quality: DimensionAggregate,
    tool_agent_execution: DimensionAggregate,
    final_product_output_contract: DimensionAggregate,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ArmReport {
    metadata: ArmMetadata,
    aggregates: BTreeMap<String, Aggregate>,
    cases: Vec<CaseResult>,
}

/// Projection applied after the single common provider call. The projection is deliberately
/// evaluator-owned and identical for both candidates; it must never dispatch another model call.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ModelOnlyProjection {
    SummaryAssembly,
    RawTrimmed,
    LiveBullets,
    StructuredFacts,
}

impl ModelOnlyProjection {
    fn as_str(self) -> &'static str {
        match self {
            Self::SummaryAssembly => "summary_assembly",
            Self::RawTrimmed => "raw_trimmed",
            Self::LiveBullets => "live_bullets",
            Self::StructuredFacts => "structured_facts",
        }
    }
}

struct SameEnvelope {
    system: String,
    user: String,
    projection: ModelOnlyProjection,
    output_contract: &'static str,
    substitutions: Vec<(String, String)>,
    system_sha256: String,
    user_sha256: String,
    envelope_sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelOnlyCaseResult {
    case_id: String,
    case_payload_sha256: String,
    surface: Surface,
    language: Language,
    model_class: ModelClass,
    holdout: bool,
    arm_id: String,
    model_requested: String,
    system_sha256: String,
    user_sha256: String,
    envelope_sha256: String,
    system_bytes: usize,
    user_bytes: usize,
    system_chars: usize,
    user_chars: usize,
    projection: ModelOnlyProjection,
    output_contract: String,
    opaque_substitution_count: usize,
    opaque_substitutions_sha256: String,
    call_count: u64,
    raw_output_chars: usize,
    raw_output_sha256: String,
    output_chars: usize,
    output_sha256: String,
    output: String,
    provenance: Vec<String>,
    provenance_sha256: String,
    state_application_pass: Option<bool>,
    duration_ms: u64,
    error: Option<String>,
    egress_receipt_start_ordinal: Option<u64>,
    egress_receipt_end_ordinal: Option<u64>,
    egress_receipt_count: u64,
    egress_receipt_sha256: String,
    redactions_email: u32,
    redactions_card: u32,
    redactions_phone: u32,
    redactions_name: u32,
    score: OracleScore,
    case_record_sha256: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelOnlyArmReport {
    arm_id: String,
    model_requested: String,
    aggregates: BTreeMap<String, CompositeAggregate>,
    cases: Vec<ModelOnlyCaseResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelOnlyPair {
    case_id: String,
    case_payload_sha256: String,
    surface: Surface,
    holdout: bool,
    local_arm: String,
    reference_arm: String,
    envelope_sha256: String,
    local_case_pass: bool,
    reference_case_pass: bool,
    local_call_success: bool,
    reference_call_success: bool,
    local_critical_failure: bool,
    reference_critical_failure: bool,
    local_diagnostic_score: f64,
    reference_diagnostic_score: f64,
    reference_minus_local: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelOnlyPairedAggregate {
    local_arm: String,
    reference_arm: String,
    cohort: String,
    matched_cases: usize,
    local_case_pass_rate: f64,
    reference_case_pass_rate: f64,
    local_call_success_rate: f64,
    reference_call_success_rate: f64,
    local_surface_macro_pass_rate: f64,
    reference_surface_macro_pass_rate: f64,
    local_critical_failure_cases: usize,
    reference_critical_failure_cases: usize,
    reference_minus_local_mean: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SameEnvelopeModelOnlyReport {
    lane_id: &'static str,
    entrypoint: &'static str,
    equality_boundary: &'static str,
    provider_rendered_prompts_byte_identical: bool,
    effective_model_inputs_attested_identical: bool,
    limitations: [&'static str; 3],
    arms: Vec<ModelOnlyArmReport>,
    pairs: Vec<ModelOnlyPair>,
    aggregates: Vec<ModelOnlyPairedAggregate>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RetrievalLane {
    mode: &'static str,
    oracle: &'static str,
    attribution: &'static str,
}

/// Content-free projection of the exact entry handed to the canonical SQLite egress writer. It is
/// safe to retain in the synthetic result artifact: no prompt, transcript, response, URL, secret,
/// meeting title, or redacted value can be represented by this type.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct BenchmarkEgressRow {
    ordinal: u64,
    provider_id: String,
    destination: String,
    model_requested: String,
    call_kind: String,
    model_served: Option<String>,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    total_tokens: Option<u32>,
    cached_tokens: Option<u32>,
    redactions_email: u32,
    redactions_card: u32,
    redactions_phone: u32,
    redactions_name: u32,
    system_bytes: usize,
    user_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkEgressEvidence {
    required: bool,
    sqlite_persistence_verified: bool,
    temporary_database_cleaned: bool,
    attempted_rows: u64,
    persisted_rows: u64,
    persistence_failures: u64,
    content_free_rows_sha256: String,
    provider_ids: Vec<String>,
    call_kinds: Vec<String>,
    rows: Vec<BenchmarkEgressRow>,
}

struct BenchmarkEgressSink {
    db: Mutex<Option<Arc<crate::storage::Db>>>,
    directory: PathBuf,
    path: PathBuf,
    key_hex: Zeroizing<String>,
    attempted: AtomicU64,
    persistence_failures: AtomicU64,
    rows: Mutex<Vec<BenchmarkEgressRow>>,
}

impl BenchmarkEgressSink {
    fn create() -> Self {
        let mut nonce = [0_u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let directory = std::env::temp_dir().join(format!(
            "murmur-quality-egress-{}-{}",
            std::process::id(),
            hex_digest(&nonce)
        ));
        std::fs::create_dir(&directory).expect("create private benchmark egress directory");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("protect benchmark egress directory");
        let path = directory.join("ledger.sqlite");
        let mut key = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        let key_hex = Zeroizing::new(hex_digest(&key));
        zeroize::Zeroize::zeroize(&mut key);
        let db = Arc::new(
            crate::storage::Db::open_with_key(&path, key_hex.as_str())
                .expect("create fresh SQLCipher benchmark egress ledger"),
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("protect benchmark egress database");
        Self {
            db: Mutex::new(Some(db)),
            directory,
            path,
            key_hex,
            attempted: AtomicU64::new(0),
            persistence_failures: AtomicU64::new(0),
            rows: Mutex::new(Vec::new()),
        }
    }

    fn cursor(&self) -> u64 {
        self.attempted.load(Ordering::SeqCst)
    }

    fn rows_since(&self, cursor: u64) -> Vec<BenchmarkEgressRow> {
        self.rows
            .lock()
            .map(|rows| {
                rows.iter()
                    .filter(|row| row.ordinal > cursor)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn evidence(&self, required: bool) -> BenchmarkEgressEvidence {
        let rows = self
            .rows
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_default();
        let attempted_rows = self.attempted.load(Ordering::SeqCst);
        let persistence_failures = self.persistence_failures.load(Ordering::SeqCst);
        // Release the writer and force the proof to come from a fresh keyed connection. The sink is
        // no longer reachable by any provider when this is called (all ArmRuntime values were
        // consumed), so taking the handle cannot race a dispatch.
        let writer_closed = self
            .db
            .lock()
            .map(|mut db| db.take().is_some())
            .unwrap_or(false);
        let persisted = writer_closed
            .then(|| read_persisted_benchmark_egress_rows(&self.path, self.key_hex.as_str()))
            .flatten();
        let persisted_rows = persisted.as_ref().map_or(0, |stored| stored.len() as u64);
        let serialized = serde_json::to_string(&rows).expect("serialize content-free egress rows");
        let provider_ids = rows
            .iter()
            .map(|row| row.provider_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let call_kinds = rows
            .iter()
            .map(|row| row.call_kind.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let sqlite_persistence_verified = (!required && attempted_rows == 0)
            || (required
                && writer_closed
                && attempted_rows > 0
                && persistence_failures == 0
                && persisted_rows == attempted_rows
                && rows.len() as u64 == attempted_rows
                && persisted.as_ref() == Some(&rows));
        let temporary_database_cleaned = self.cleanup_files();
        BenchmarkEgressEvidence {
            required,
            sqlite_persistence_verified,
            temporary_database_cleaned,
            attempted_rows,
            persisted_rows,
            persistence_failures,
            content_free_rows_sha256: prompt_hash(&[&serialized]),
            provider_ids,
            call_kinds,
            rows,
        }
    }

    fn cleanup_files(&self) -> bool {
        for path in [
            self.path.clone(),
            PathBuf::from(format!("{}-wal", self.path.display())),
            PathBuf::from(format!("{}-shm", self.path.display())),
        ] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return false,
            }
        }
        match std::fs::remove_dir(&self.directory) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        }
    }
}

impl Drop for BenchmarkEgressSink {
    fn drop(&mut self) {
        if let Ok(mut db) = self.db.lock() {
            let _ = db.take();
        }
        let _ = self.cleanup_files();
    }
}

fn read_persisted_benchmark_egress_rows(
    path: &Path,
    key_hex: &str,
) -> Option<Vec<BenchmarkEgressRow>> {
    // Verification intentionally opens a SECOND keyed handle: reading the writer's own in-memory
    // state would not prove that committed SQLite rows are visible after a reopen.
    let connection = rusqlite::Connection::open(path).ok()?;
    let key = Zeroizing::new(format!("x'{key_hex}'"));
    connection.pragma_update(None, "key", key.as_str()).ok()?;
    let mut statement = connection
        .prepare(
            "SELECT provider_id, destination, model_requested, call_kind, model_served,
                    prompt_tokens, completion_tokens, total_tokens, cached_tokens,
                    redactions_email, redactions_card, redactions_phone, redactions_name,
                    system_bytes, user_bytes, meeting_id
               FROM egress_log ORDER BY rowid ASC",
        )
        .ok()?;
    let mapped = statement
        .query_map([], |row| {
            let meeting_id: Option<String> = row.get(15)?;
            if meeting_id.is_some() {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Ok(BenchmarkEgressRow {
                ordinal: 0,
                provider_id: row.get(0)?,
                destination: row.get(1)?,
                model_requested: row.get(2)?,
                call_kind: row.get(3)?,
                model_served: row.get(4)?,
                prompt_tokens: row.get::<_, Option<i64>>(5)?.map(|value| value as u32),
                completion_tokens: row.get::<_, Option<i64>>(6)?.map(|value| value as u32),
                total_tokens: row.get::<_, Option<i64>>(7)?.map(|value| value as u32),
                cached_tokens: row.get::<_, Option<i64>>(8)?.map(|value| value as u32),
                redactions_email: row.get::<_, i64>(9)? as u32,
                redactions_card: row.get::<_, i64>(10)? as u32,
                redactions_phone: row.get::<_, i64>(11)? as u32,
                redactions_name: row.get::<_, i64>(12)? as u32,
                system_bytes: row.get::<_, i64>(13)? as usize,
                user_bytes: row.get::<_, i64>(14)? as usize,
            })
        })
        .ok()?;
    let mut rows = Vec::new();
    for (index, row) in mapped.enumerate() {
        let mut row = row.ok()?;
        row.ordinal = index as u64 + 1;
        rows.push(row);
    }
    Some(rows)
}

impl crate::summarize::egress_log::EgressSink for BenchmarkEgressSink {
    fn record(&self, entry: crate::summarize::egress_log::EgressEntry) {
        let ordinal = self.attempted.fetch_add(1, Ordering::SeqCst) + 1;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        let row = BenchmarkEgressRow {
            ordinal,
            provider_id: entry.provider_id.clone(),
            destination: entry.destination.clone(),
            model_requested: entry.model_requested.clone(),
            call_kind: entry.call_kind.to_string(),
            model_served: entry.meta.model_served.clone(),
            prompt_tokens: entry.meta.prompt_tokens,
            completion_tokens: entry.meta.completion_tokens,
            total_tokens: entry.meta.total_tokens,
            cached_tokens: entry.meta.cached_tokens,
            redactions_email: entry.redactions.email,
            redactions_card: entry.redactions.card,
            redactions_phone: entry.redactions.phone,
            redactions_name: entry.redactions.name,
            system_bytes: entry.system_bytes,
            user_bytes: entry.user_bytes,
        };
        let db = self.db.lock().ok().and_then(|db| db.as_ref().cloned());
        if db.is_none_or(|db| db.insert_egress(ts, &entry).is_err()) {
            self.persistence_failures.fetch_add(1, Ordering::SeqCst);
            return;
        }
        if let Ok(mut rows) = self.rows.lock() {
            rows.push(row);
        } else {
            self.persistence_failures.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentMetadata {
    hardware_model: Option<String>,
    cpu_brand: Option<String>,
    memory_bytes: Option<u64>,
    os_version: Option<String>,
    os_build: Option<String>,
    name_redactor_mode: &'static str,
    tracked_diff_sha256: Option<String>,
    working_tree_dirty: bool,
    arm_order: Vec<String>,
    repetition: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RunSnapshot {
    repository_commit: String,
    source_fingerprint_sha256: String,
    manifest_sha256: String,
    evaluator_file_sha256: String,
    fixture_file_sha256: String,
    repeat_validator_file_sha256: String,
    tracked_diff_sha256: Option<String>,
    working_tree_dirty: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairedCaseComparison {
    case_id: String,
    case_payload_sha256: String,
    surface: Surface,
    comparison_kind: PairComparisonKind,
    local_arm: String,
    reference_arm: String,
    holdout: bool,
    comparison_scope: ComparisonScope,
    local_route_input_sha256: String,
    reference_route_input_sha256: String,
    local_generation_profile: String,
    reference_generation_profile: String,
    local_case_pass: bool,
    reference_case_pass: bool,
    local_call_success: bool,
    reference_call_success: bool,
    local_critical_failure: bool,
    reference_critical_failure: bool,
    local_diagnostic_score: f64,
    reference_diagnostic_score: f64,
    reference_minus_local: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairedAggregate {
    local_arm: String,
    reference_arm: String,
    comparison_kind: PairComparisonKind,
    cohort: String,
    matched_cases: usize,
    local_case_pass_rate: f64,
    reference_case_pass_rate: f64,
    local_call_success_rate: f64,
    reference_call_success_rate: f64,
    local_surface_macro_pass_rate: f64,
    reference_surface_macro_pass_rate: f64,
    local_critical_failure_cases: usize,
    reference_critical_failure_cases: usize,
    reference_minus_local_mean: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairedComparison {
    cases: Vec<PairedCaseComparison>,
    aggregates: Vec<PairedAggregate>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompositeAggregate {
    cases: usize,
    call_success_rate: f64,
    case_pass_rate: f64,
    surface_macro_pass_rate: f64,
    critical_failure_cases: usize,
    diagnostic_score_mean: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalComposite {
    arm_ids: [&'static str; 2],
    definition: &'static str,
    aggregates: BTreeMap<String, CompositeAggregate>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QualityReport {
    schema_version: u32,
    run_label: String,
    generated_at: String,
    repository_commit: String,
    source_fingerprint_sha256: String,
    manifest_sha256: String,
    prompt_version: &'static str,
    synthetic_only: bool,
    holdout_interpretation: &'static str,
    benchmark_design: &'static str,
    evidence_scope: &'static str,
    evidence_limits: [&'static str; 3],
    retrieval_lane: RetrievalLane,
    retrieval_quality: crate::eval::generation_retrieval::RetrievalQualityEvidence,
    snapshot_start: RunSnapshot,
    snapshot_end: RunSnapshot,
    environment: EnvironmentMetadata,
    egress_ledger: BenchmarkEgressEvidence,
    #[serde(rename = "sameCallerEnvelopeModelStack")]
    same_envelope_model_only: SameEnvelopeModelOnlyReport,
    arms: Vec<ArmReport>,
    local_composite: LocalComposite,
    paired_comparison: PairedComparison,
}

struct Execution {
    output: String,
    surface_output: Option<String>,
    raw_model_output: Option<String>,
    raw_model_format_pass: Option<bool>,
    error: Option<String>,
    prompt_sha256: String,
    input_chars: usize,
    product_route: String,
    comparison_scope: ComparisonScope,
    tool_steps: Vec<String>,
    tool_policy_score: Option<f64>,
    tool_policy_pass: Option<bool>,
    state_application_pass: Option<bool>,
    branch_converged: Option<bool>,
    provenance: Vec<String>,
}

struct CapturingStructuredReasoner {
    inner: Arc<dyn LocalReasoner>,
    observation: Mutex<Option<crate::reason::StructuredObservation>>,
}

impl CapturingStructuredReasoner {
    fn new(inner: Arc<dyn LocalReasoner>) -> Self {
        Self {
            inner,
            observation: Mutex::new(None),
        }
    }

    fn take_observation(&self) -> Option<crate::reason::StructuredObservation> {
        self.observation.lock().ok()?.take()
    }

    fn capture(
        &self,
        observation: crate::reason::StructuredObservation,
    ) -> crate::reason::StructuredObservation {
        if let Ok(mut slot) = self.observation.lock() {
            *slot = Some(observation.clone());
        }
        observation
    }
}

impl LocalReasoner for CapturingStructuredReasoner {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn reason(&self, system: &str, user: &str) -> Result<String> {
        self.inner.reason(system, user)
    }

    fn reason_with(&self, system: &str, user: &str, opts: GenOptions) -> Result<String> {
        self.inner.reason_with(system, user, opts)
    }

    fn structured(
        &self,
        system: &str,
        user: &str,
        schema: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        Ok(self
            .capture(self.inner.structured_with_observation(
                system,
                user,
                schema,
                GenOptions::default(),
            )?)
            .value)
    }

    fn structured_with(
        &self,
        system: &str,
        user: &str,
        schema: &serde_json::Value,
        opts: GenOptions,
    ) -> Result<serde_json::Value> {
        Ok(self
            .capture(
                self.inner
                    .structured_with_observation(system, user, schema, opts)?,
            )
            .value)
    }

    fn structured_with_observation(
        &self,
        system: &str,
        user: &str,
        schema: &serde_json::Value,
        opts: GenOptions,
    ) -> Result<crate::reason::StructuredObservation> {
        Ok(self.capture(
            self.inner
                .structured_with_observation(system, user, schema, opts)?,
        ))
    }
}

struct ArmRuntime {
    metadata: ArmMetadata,
    notes_provider: Arc<dyn SummarizerProvider>,
    ask_provider: Arc<dyn SummarizerProvider>,
    model_only_provider: Arc<dyn SummarizerProvider>,
    ask_reasoner: Arc<dyn LocalReasoner>,
    live_reasoner: Arc<dyn LocalReasoner>,
    extraction_reasoner: Arc<dyn LocalReasoner>,
    egress_sink: Option<Arc<BenchmarkEgressSink>>,
    cloud_reference: bool,
}

impl ArmRuntime {
    fn local(arm_id: &str, model_path: PathBuf, class: ModelClass) -> Result<Self> {
        let metadata = std::fs::metadata(&model_path).map_err(|error| {
            AppError::Unavailable(format!("benchmark model unavailable: {error}"))
        })?;
        let digest = sha256_file(&model_path).map_err(|error| {
            AppError::Unavailable(format!("benchmark model hash failed: {error}"))
        })?;
        let runtime_version = brain_runtime_version().ok_or_else(|| {
            AppError::InvalidArg(
                "MURMUR_BRAIN_SIDECAR must name the exact benchmark sidecar binary".to_string(),
            )
        })?;
        let runtime_sha256 = brain_runtime_sha256().ok_or_else(|| {
            AppError::InvalidArg(
                "MURMUR_BRAIN_SIDECAR must exist and be readable for benchmark provenance"
                    .to_string(),
            )
        })?;
        let filename = model_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let (expected_filename, expected_bytes, expected_sha256, expected_class) = match arm_id {
            QWEN4_ID => (QWEN4_FILENAME, QWEN4_BYTES, QWEN4_SHA256, ModelClass::Heavy),
            QWEN1_ID => (QWEN1_FILENAME, QWEN1_BYTES, QWEN1_SHA256, ModelClass::Light),
            other => {
                return Err(AppError::InvalidArg(format!(
                    "unknown local quality arm: {other}"
                )))
            }
        };
        if filename != expected_filename
            || metadata.len() != expected_bytes
            || digest != expected_sha256
            || class != expected_class
        {
            return Err(AppError::InvalidArg(format!(
                "benchmark model identity mismatch for {arm_id}: expected {expected_filename} ({expected_bytes} bytes, sha256 {expected_sha256})"
            )));
        }
        let timeouts = crate::reason::sidecar::SidecarTimeouts::default();
        let reasoner: Arc<dyn LocalReasoner> = Arc::new(
            crate::reason::sidecar::SidecarReasoner::new(model_path.clone(), timeouts)?,
        );
        let heavy = Arc::new(tokio::sync::Semaphore::new(1));
        let provider: Arc<dyn SummarizerProvider> = Arc::new(
            crate::summarize::local::LocalSummarizerProvider::new(Arc::clone(&reasoner), heavy),
        );
        Ok(Self {
            metadata: ArmMetadata {
                arm_id: arm_id.to_string(),
                model_requested: filename.to_string(),
                effort: None,
                effort_transport: None,
                effort_effective_attested: None,
                model_class: match class {
                    ModelClass::Heavy => "heavy",
                    ModelClass::Light => "light",
                }
                .to_string(),
                model_filename: Some(filename.to_string()),
                model_bytes: Some(metadata.len()),
                model_sha256: Some(digest),
                runtime_version: Some(runtime_version),
                runtime_sha256: Some(runtime_sha256),
                sidecar_idle_secs: Some(timeouts.idle_secs),
                sidecar_ready_secs: Some(timeouts.ready_secs),
                sidecar_hard_cap_secs: Some(timeouts.hard_cap_secs),
            },
            notes_provider: Arc::clone(&provider),
            ask_provider: Arc::clone(&provider),
            model_only_provider: provider,
            ask_reasoner: Arc::clone(&reasoner),
            live_reasoner: Arc::clone(&reasoner),
            extraction_reasoner: reasoner,
            egress_sink: None,
            cloud_reference: false,
        })
    }

    fn cloud(sink: Arc<BenchmarkEgressSink>) -> Result<Self> {
        if !cloud_egress_acknowledged(std::env::var("MURMUR_QUALITY_ALLOW_CLOUD").ok().as_deref()) {
            return Err(AppError::Unavailable(
                "set MURMUR_QUALITY_ALLOW_CLOUD=1 to acknowledge synthetic benchmark egress"
                    .to_string(),
            ));
        }
        let runtime_version = codex_runtime_version().ok_or_else(|| {
            AppError::Unavailable("cannot identify /opt/homebrew/bin/codex runtime".to_string())
        })?;
        let runtime_sha256 =
            sha256_file(Path::new("/opt/homebrew/bin/codex")).map_err(|error| {
                AppError::Unavailable(format!("cannot hash Codex runtime: {error}"))
            })?;
        let config = cloud_config();
        let heavy = Arc::new(tokio::sync::Semaphore::new(1));
        let notes_provider = crate::summarize::provider_for_with_egress_sink(
            Role::Notes,
            &config,
            &heavy,
            sink.clone(),
        )?;
        let ask_provider = crate::summarize::provider_for_with_egress_sink(
            Role::Ask,
            &config,
            &heavy,
            sink.clone(),
        )?;
        let shared = Arc::new(Mutex::new(config));
        let ask_reasoner: Arc<dyn LocalReasoner> =
            Arc::new(crate::reason::CloudReasoner::for_role_with_egress_sink(
                Arc::clone(&shared),
                Role::Ask,
                Arc::clone(&heavy),
                sink.clone(),
            ));
        let live_reasoner: Arc<dyn LocalReasoner> =
            Arc::new(crate::reason::CloudReasoner::for_role_with_egress_sink(
                Arc::clone(&shared),
                Role::Live,
                Arc::clone(&heavy),
                sink.clone(),
            ));
        let extraction_reasoner: Arc<dyn LocalReasoner> =
            Arc::new(crate::reason::CloudReasoner::for_role_with_egress_sink(
                shared,
                Role::Notes,
                heavy,
                sink.clone(),
            ));
        Ok(Self {
            metadata: ArmMetadata {
                arm_id: SOL_ID.to_string(),
                model_requested: CODEX_MODEL.to_string(),
                effort: Some(CODEX_EFFORT.to_string()),
                effort_transport: Some(format!(
                    "--config model_reasoning_effort=\"{CODEX_EFFORT}\""
                )),
                // Codex CLI receives the strict config argv, but its response protocol does not
                // independently attest the effort that the remote model actually applied.
                effort_effective_attested: Some(false),
                model_class: "reference".to_string(),
                model_filename: None,
                model_bytes: None,
                model_sha256: None,
                runtime_version: Some(runtime_version),
                runtime_sha256: Some(runtime_sha256),
                sidecar_idle_secs: None,
                sidecar_ready_secs: None,
                sidecar_hard_cap_secs: None,
            },
            notes_provider,
            ask_provider: Arc::clone(&ask_provider),
            model_only_provider: ask_provider,
            ask_reasoner,
            live_reasoner,
            extraction_reasoner,
            egress_sink: Some(sink),
            cloud_reference: true,
        })
    }
}

fn cloud_egress_acknowledged(value: Option<&str>) -> bool {
    value == Some("1")
}

fn cloud_config() -> AppConfig {
    AppConfig {
        provider_id: crate::summarize::PROVIDER_CODEX_CLI.to_string(),
        provider_model: CODEX_MODEL.to_string(),
        provider_effort: CODEX_EFFORT.to_string(),
        role_notes_connection: crate::summarize::PROVIDER_CODEX_CLI.to_string(),
        role_notes_model: CODEX_MODEL.to_string(),
        role_notes_effort: CODEX_EFFORT.to_string(),
        role_ask_connection: crate::summarize::PROVIDER_CODEX_CLI.to_string(),
        role_ask_model: CODEX_MODEL.to_string(),
        role_ask_effort: CODEX_EFFORT.to_string(),
        role_live_connection: crate::summarize::PROVIDER_CODEX_CLI.to_string(),
        role_live_model: CODEX_MODEL.to_string(),
        role_live_effort: CODEX_EFFORT.to_string(),
        // Construction also requires MURMUR_QUALITY_ALLOW_CLOUD=1. The provider still traverses
        // the canonical consent check, redaction firewall, and content-free ledger seam.
        cloud_egress_consented: true,
        ..Default::default()
    }
}

fn manifest() -> QualityManifest {
    serde_json::from_str(MANIFEST_JSON).expect("quality manifest must be valid")
}

fn arm_accepts(arm_id: &str, class: ModelClass) -> bool {
    match arm_id {
        QWEN4_ID => class == ModelClass::Heavy,
        QWEN1_ID => class == ModelClass::Light,
        SOL_ID => true,
        _ => false,
    }
}

fn comparison_scope_for(cloud_reference: bool, case: &QualityCase) -> ComparisonScope {
    if cloud_reference && case.sol_reference_only {
        ComparisonScope::OfflineReferenceCeiling
    } else {
        ComparisonScope::ProductPath
    }
}

fn generation_profile(arm: &ArmRuntime, case: &QualityCase) -> String {
    match case.surface {
        Surface::Summary | Surface::MeetingChat => "provider_default".to_string(),
        Surface::NoteAssist => format!(
            "edit_rewrite(max_tokens={})",
            crate::commands::note_edit_max_tokens(&case.action, case.selection.chars().count(),)
        ),
        Surface::AskVault if arm.cloud_reference => {
            "cloud_agentic_read_after_locate_then_floor(max_tokens=2048,compaction=true,grammar=false)"
                .to_string()
        }
        Surface::AskVault => "local_floor_one_completion".to_string(),
        Surface::LiveCurrent if arm.cloud_reference => {
            "cloud_three_tier_cascade_then_isolated_floor(max_tokens=1024,compaction=true,grammar=false)".to_string()
        }
        Surface::LiveCurrent => "local_isolated_floor(max_tokens=1024,grammar=false)".to_string(),
        Surface::LiveBullets => {
            "update_bullets(max_tokens=200,temperature=0.2,parsed,max_new=3)".to_string()
        }
        Surface::LightExtraction => {
            "fully_local_post_call_fact_extraction_structured(provider_default)".to_string()
        }
    }
}

fn contains_any(output: &str, alternatives: &[String]) -> bool {
    let output = output.to_lowercase();
    alternatives
        .iter()
        .any(|needle| output.contains(&needle.to_lowercase()))
}

fn contains_forbidden_phrase(output: &str, phrase: &str) -> bool {
    let output = output.to_lowercase();
    let phrase = phrase.to_lowercase();
    if phrase.is_empty() {
        return false;
    }
    let starts_with_word = phrase.chars().next().is_some_and(char::is_alphanumeric);
    let ends_with_word = phrase
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric);
    output
        .match_indices(phrase.as_str())
        .any(|(start, matched)| {
            let end = start + matched.len();
            let left_boundary = !starts_with_word
                || output[..start]
                    .chars()
                    .next_back()
                    .is_none_or(|character| !character.is_alphanumeric());
            let right_boundary = !ends_with_word
                || output[end..]
                    .chars()
                    .next()
                    .is_none_or(|character| !character.is_alphanumeric());
            left_boundary && right_boundary
        })
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn parsed_structured_facts(output: &str) -> Option<Vec<StructuredFact>> {
    let serde_json::Value::Object(root) = serde_json::from_str(output.trim()).ok()? else {
        return None;
    };
    if root.len() != 1 {
        return None;
    }
    let serde_json::Value::Array(facts) = root.get("facts")? else {
        return None;
    };
    let mut parsed = Vec::with_capacity(facts.len());
    for fact in facts {
        let serde_json::Value::Object(fields) = fact else {
            return None;
        };
        if fields.len() != 3 {
            return None;
        }
        let field = |key: &str| {
            fields
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        };
        parsed.push(StructuredFact {
            entity: field("entity")?,
            predicate: field("predicate")?,
            object: field("object")?,
        });
    }
    Some(parsed)
}

fn structured_labels_pass(output: &str, expected: &ExpectedOutput) -> bool {
    if expected.structured_facts.is_empty() {
        return true;
    }
    let Some(actual) = parsed_structured_facts(output) else {
        return false;
    };
    actual.len() == expected.structured_facts.len()
        && actual.iter().collect::<BTreeSet<_>>()
            == expected.structured_facts.iter().collect::<BTreeSet<_>>()
}

fn structured_envelope_pass(raw: &str) -> bool {
    let trimmed = raw.trim();
    serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
        && parsed_structured_facts(trimmed).is_some()
        && ![
            "<think",
            "</think",
            "```",
            "analysis:",
            "reasoning:",
            "here is",
            "oto json",
            "rozumowanie",
        ]
        .iter()
        .any(|marker| trimmed.to_lowercase().contains(marker))
}

fn output_format_pass(output: &str, expected: &ExpectedOutput) -> bool {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return false;
    }
    match expected.format {
        OutputFormat::SummaryMarkdown => {
            let (yaml, body) = crate::storage::db::split_front_matter(trimmed);
            !yaml.trim().is_empty()
                && yaml.lines().any(|line| line.starts_with("title:"))
                && yaml.lines().any(|line| line.starts_with("date:"))
                && !body.trim_start().starts_with("---")
                && body.lines().any(|line| line.starts_with("# "))
                && body.lines().any(|line| line.starts_with("## "))
        }
        OutputFormat::PlainAnswer | OutputFormat::LiveAnswer => {
            !trimmed.starts_with('{') && !trimmed.starts_with("```")
        }
        OutputFormat::AskAnswer => {
            !trimmed.starts_with('{')
                && !trimmed.starts_with("```")
                && !trimmed.starts_with("---")
                && trimmed.contains("**")
        }
        OutputFormat::FaithfulEdit => {
            !trimmed.starts_with('#')
                && !trimmed.to_lowercase().starts_with("here")
                && expected
                    .max_words
                    .is_none_or(|ceiling| word_count(trimmed) <= ceiling)
        }
        OutputFormat::Shorter => !trimmed.starts_with('#'),
        OutputFormat::TaskList => {
            let lines = trimmed
                .lines()
                .filter(|line| !line.trim().is_empty())
                .collect::<Vec<_>>();
            !lines.is_empty()
                && lines
                    .iter()
                    .all(|line| line.trim_start().starts_with("- [ ]"))
        }
        OutputFormat::BulletList => trimmed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| line.trim_start().starts_with("- ")),
        OutputFormat::LiveBullets => {
            let lines = trimmed
                .lines()
                .filter(|line| !line.trim().is_empty())
                .collect::<Vec<_>>();
            !lines.is_empty()
                && lines
                    .iter()
                    .all(|line| line.starts_with("- [") && line.contains("]:"))
        }
        OutputFormat::StructuredFacts => {
            parsed_structured_facts(trimmed).is_some_and(|facts| !facts.is_empty())
                && ![
                    "<think",
                    "</think",
                    "```",
                    "here is",
                    "oto json",
                    "analysis:",
                ]
                .iter()
                .any(|marker| trimmed.to_lowercase().contains(marker))
        }
    }
}

fn output_constraint_pass(output: &str, expected: &ExpectedOutput) -> bool {
    let word_limit = expected
        .max_words
        .is_none_or(|ceiling| word_count(output) <= ceiling);
    let bullet_count = output
        .lines()
        .filter(|line| line.trim_start().starts_with("- "))
        .count();
    let bullet_limit = expected
        .max_bullets
        .is_none_or(|ceiling| bullet_count <= ceiling);
    let task_count = output
        .lines()
        .filter(|line| line.trim_start().starts_with("- [ ]"))
        .count();
    let task_limit = expected
        .max_tasks
        .is_none_or(|ceiling| task_count <= ceiling);
    word_limit && bullet_limit && task_limit
}

fn markdown_section<'a>(output: &'a str, headings: &[String]) -> Option<&'a str> {
    headings.iter().find_map(|heading| {
        let marker = format!("## {heading}");
        let start = output.find(&marker)? + marker.len();
        let tail = &output[start..];
        let end = tail.find("\n## ").unwrap_or(tail.len());
        Some(&tail[..end])
    })
}

fn preceding_alpha_word(output: &str, end: usize) -> &str {
    let prefix = &output[..end];
    let start = prefix
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            (!character.is_alphabetic()).then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    &output[start..end]
}

fn relation_units(output: &str) -> Vec<&str> {
    let boundary = regex::Regex::new(
        r"(?:\r?\n)+|[!?;]+|\.\s+|,\s+(?i:and|while|but|whereas|a|ale|natomiast|podczas gdy)\s+",
    )
    .expect("valid relation boundary regex");
    const ABBREVIATIONS: &[&str] = &["tys", "mln", "mld", "np", "tj", "dr", "prof", "godz"];
    let mut units = Vec::new();
    let mut start = 0usize;
    for found in boundary.find_iter(output) {
        let delimiter = &output[found.start()..found.end()];
        if delimiter.starts_with('.') && !delimiter.contains('\r') && !delimiter.contains('\n') {
            let previous = preceding_alpha_word(output, found.start()).to_lowercase();
            if ABBREVIATIONS.contains(&previous.as_str()) {
                continue;
            }
        }
        let unit = &output[start..found.start()];
        if !unit.trim().is_empty() {
            units.push(unit);
        }
        start = found.end();
    }
    let tail = &output[start..];
    if !tail.trim().is_empty() {
        units.push(tail);
    }
    units
}

fn relation_present(units: &[&str], requirement: &RelationRequirement) -> bool {
    units.iter().any(|unit| {
        requirement
            .groups
            .iter()
            .all(|group| contains_any(unit, group))
    })
}

fn relation_requirements_pass(output: &str, requirements: &[RelationRequirement]) -> bool {
    let units = relation_units(output);
    requirements
        .iter()
        .all(|requirement| relation_present(&units, requirement))
}

fn conditional_relation_pass(output: &str, requirement: &ConditionalRelationRequirement) -> bool {
    let scoped_output = if requirement.headings.is_empty() {
        output
    } else {
        markdown_section(output, &requirement.headings).unwrap_or("")
    };
    relation_units(scoped_output).iter().all(|unit| {
        let triggered = requirement
            .when_groups
            .iter()
            .all(|group| contains_any(unit, group));
        !triggered
            || requirement
                .then_groups
                .iter()
                .all(|group| contains_any(unit, group))
    })
}

fn task_owner_requirement_pass(output: &str, requirement: &TaskOwnerRequirement) -> bool {
    let section = markdown_section(output, &requirement.headings).unwrap_or("");
    let mut matching_task_found = false;
    for line in section.lines() {
        let Some(task) = line.trim_start().strip_prefix("- [ ]") else {
            continue;
        };
        if !requirement
            .task_groups
            .iter()
            .all(|group| contains_any(task, group))
        {
            continue;
        }
        matching_task_found = true;

        let task = task.trim();
        let primary_separators = [" — ", " – ", " - "];
        let leading_end = primary_separators
            .iter()
            .filter_map(|separator| task.find(*separator))
            .min();
        let leading = leading_end.map_or_else(
            || {
                task.split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_string()
            },
            |end| task[..end].to_string(),
        );
        let owner_in_slot = |slot: &str| {
            requirement
                .owners
                .iter()
                .any(|owner| contains_forbidden_phrase(slot, owner))
        };
        if owner_in_slot(&leading) {
            continue;
        }

        let trailing_owner = [" — ", " – ", " - ", ": "].iter().any(|separator| {
            task.rsplit_once(*separator)
                .is_some_and(|(_, trailing)| word_count(trailing) <= 4 && owner_in_slot(trailing))
        });
        if !trailing_owner {
            return false;
        }
    }
    matching_task_found
}

fn expected_reference_text(case: &QualityCase) -> String {
    let mut parts = vec![
        case.transcript.clone(),
        case.question.clone(),
        case.selection.clone(),
        case.before.clone(),
        case.previous_bullets.clone(),
        case.tool_result.clone(),
        case.search_result.clone(),
        case.floor_corpus.clone(),
        case.date_iso.clone(),
        case.title_hint.clone(),
        case.duration_s.to_string(),
        ((case.duration_s as f64 / 60.0).round() as i64).to_string(),
    ];
    parts.extend(case.vault_titles.iter().cloned());
    parts.extend(case.search_terms.iter().cloned());
    parts.extend(
        case.expected
            .required_groups
            .iter()
            .chain(&case.expected.critical_groups)
            .chain(&case.expected.required_provenance)
            .flat_map(|group| group.iter().cloned()),
    );
    for requirement in &case.expected.section_requirements {
        parts.extend(requirement.headings.iter().cloned());
        parts.extend(
            requirement
                .groups
                .iter()
                .flat_map(|group| group.iter().cloned()),
        );
    }
    for requirement in &case.expected.relation_requirements {
        parts.extend(
            requirement
                .groups
                .iter()
                .flat_map(|group| group.iter().cloned()),
        );
    }
    parts.join("\n")
}

fn numeric_tokens(text: &str) -> std::collections::BTreeSet<String> {
    let list_prefix = regex::Regex::new(r"(?m)^\s*\d+[.)]\s+").expect("valid list prefix regex");
    let cleaned = list_prefix.replace_all(text, "");
    regex::Regex::new(r"\b\d+\b")
        .expect("valid numeric token regex")
        .find_iter(&cleaned)
        .map(|value| value.as_str().to_string())
        .collect()
}

fn month_tokens(text: &str) -> std::collections::BTreeSet<String> {
    const MONTHS: &[&str] = &[
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
        "stycznia",
        "lutego",
        "marca",
        "kwietnia",
        "maja",
        "czerwca",
        "lipca",
        "sierpnia",
        "września",
        "wrzesnia",
        "października",
        "pazdziernika",
        "listopada",
        "grudnia",
    ];
    let mut months = text
        .split(|c: char| !c.is_alphabetic())
        .map(str::to_lowercase)
        // Lowercase `may` is overwhelmingly the modal verb. Treat ambiguous English May as a
        // calendar token only in an adjacent numeric date below.
        .filter(|word| word != "may" && MONTHS.contains(&word.as_str()))
        .collect::<std::collections::BTreeSet<_>>();
    let may_calendar_context = regex::Regex::new(
        r"(?x)
          (?:
            \bMay\s+\d{1,4}\b
            | \b\d{1,4}\s+May\b
            | \b(?:in|by|during|before|after|until|through|from|on)\s+May\b
            | \b(?:early|late|mid[-\s]?)May\b
            | \b(?:month|date|launch)\s*(?:is|:)?\s+May\b
            | \bMay\s+is\b
          )",
    )
    .expect("valid English May calendar-context regex");
    if may_calendar_context.is_match(text) {
        months.insert("may".to_string());
    }
    months
}

fn suspicious_actor_pass(output: &str, allowed_entities: &[String], reference: &str) -> bool {
    const GENERIC: &[&str] = &[
        "the",
        "team",
        "project",
        "budget",
        "decision",
        "start",
        "pilot",
        "zespół",
        "zespol",
        "projekt",
        "budżet",
        "budzet",
        "decyzja",
        "ustalenia",
        "no",
        "yes",
        "nie",
        "tak",
    ];
    const ACTION_CUES: &[&str] = &[
        " owns ",
        " will ",
        " committed ",
        " approved ",
        " odpowiada ",
        " przygotuje ",
        " dostarczy ",
        " zatwierdził ",
        " zatwierdzila ",
    ];
    let allowed = allowed_entities
        .iter()
        .map(|entity| entity.to_lowercase())
        .collect::<std::collections::BTreeSet<_>>();
    let reference_tokens = reference
        .split(|c: char| !c.is_alphabetic() && c != '-')
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect::<std::collections::BTreeSet<_>>();
    for unit in output.split(['\n', '.', '!', '?']) {
        let trimmed = unit.trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, '#' | '-' | '*' | '[' | ']' | '(' | ')' | ':' | '_')
        });
        let Some(first) = trimmed
            .split(|c: char| !c.is_alphabetic() && c != '-')
            .find(|token| !token.is_empty())
        else {
            continue;
        };
        let lower = format!(" {} ", trimmed.to_lowercase());
        let checklist = unit.trim_start().starts_with("- [ ]");
        let actor_shaped = ACTION_CUES.iter().any(|cue| lower.contains(cue)) || checklist;
        let first_lower = first.to_lowercase();
        let checklist_names_a_known_owner = checklist
            && allowed
                .iter()
                .any(|entity| lower.contains(&format!(" {entity} ")));
        if actor_shaped
            && first.chars().next().is_some_and(char::is_uppercase)
            && !allowed.contains(&first_lower)
            && !reference_tokens.contains(&first_lower)
            && !checklist_names_a_known_owner
            && !GENERIC.contains(&first_lower.as_str())
        {
            return false;
        }
    }
    true
}

fn closed_world_pass(output: &str, case: &QualityCase) -> bool {
    let reference = expected_reference_text(case);
    let allowed_numbers = numeric_tokens(&reference);
    // The shipped NoteAssist route numbers its one synthetic fixture citation as `[1]` before the
    // model sees it. Remove only that exact code-owned marker; `[2]` and a bare novel `1` remain
    // closed-world failures instead of weakening number detection for every surface.
    let output_without_code_owned_citation = (case.surface == Surface::NoteAssist
        && !case.tool_result.is_empty())
    .then(|| output.replace("[1]", ""));
    let output_numbers = numeric_tokens(
        output_without_code_owned_citation
            .as_deref()
            .unwrap_or(output),
    );
    let allowed_months = month_tokens(&reference);
    let output_months = month_tokens(output);
    let allowed_links = extract_wikilinks(&reference)
        .into_iter()
        .map(|link| link.to_lowercase())
        .collect::<std::collections::BTreeSet<_>>();
    let output_links = extract_wikilinks(output)
        .into_iter()
        .map(|link| link.to_lowercase())
        .collect::<std::collections::BTreeSet<_>>();
    output_numbers.is_subset(&allowed_numbers)
        && output_months.is_subset(&allowed_months)
        && output_links.is_subset(&allowed_links)
        && suspicious_actor_pass(output, &case.expected.allowed_entities, &reference)
}

fn score_output(
    output: &str,
    provenance: &[String],
    error: Option<&str>,
    tool_policy_pass: Option<bool>,
    state_application_pass: Option<bool>,
    branch_converged: Option<bool>,
    case: &QualityCase,
) -> OracleScore {
    let expected = &case.expected;
    let required_groups_hit = expected
        .required_groups
        .iter()
        .filter(|group| contains_any(output, group))
        .count();
    let required_ratio = if expected.required_groups.is_empty() {
        1.0
    } else {
        required_groups_hit as f64 / expected.required_groups.len() as f64
    };
    let format_pass = output_format_pass(output, expected);
    let missing_sections = expected
        .section_requirements
        .iter()
        .flat_map(|requirement| {
            let section = markdown_section(output, &requirement.headings).unwrap_or("");
            requirement
                .groups
                .iter()
                .filter(move |group| !contains_any(section, group))
                .map(move |group| {
                    format!(
                        "missing_section:{}:{}",
                        requirement.headings.join("|"),
                        group.first().map(String::as_str).unwrap_or("?")
                    )
                })
        })
        .collect::<Vec<_>>();
    let forbidden_section_hits = expected
        .forbidden_section_requirements
        .iter()
        .filter(|requirement| {
            let section = markdown_section(output, &requirement.headings).unwrap_or("");
            relation_units(section).iter().any(|unit| {
                requirement
                    .groups
                    .iter()
                    .all(|group| contains_any(unit, group))
            })
        })
        .collect::<Vec<_>>();
    let section_pass = missing_sections.is_empty() && forbidden_section_hits.is_empty();
    let language_hits = expected
        .language_markers
        .iter()
        .filter(|marker| contains_any(output, std::slice::from_ref(marker)))
        .count();
    let language_pass = expected.language_markers.is_empty()
        || language_hits >= expected.language_markers.len().min(2);
    let forbidden_hits = expected
        .forbidden_terms
        .iter()
        .filter(|term| contains_forbidden_phrase(output, term))
        .cloned()
        .collect::<Vec<_>>();
    let relation_units = relation_units(output);
    let forbidden_relation_hits = expected
        .forbidden_relations
        .iter()
        .filter(|requirement| relation_present(&relation_units, requirement))
        .collect::<Vec<_>>();
    let forbidden_pass = forbidden_hits.is_empty()
        && forbidden_relation_hits.is_empty()
        && forbidden_section_hits.is_empty();
    let constraint_pass = output_constraint_pass(output, expected);
    let provenance_text = provenance.join("\n");
    let provenance_pass = expected
        .required_provenance
        .iter()
        .all(|group| contains_any(&provenance_text, group));
    let tool_policy_pass = tool_policy_pass.unwrap_or(true);
    let relation_pass = relation_requirements_pass(output, &expected.relation_requirements)
        && expected
            .conditional_relations
            .iter()
            .all(|requirement| conditional_relation_pass(output, requirement))
        && expected
            .task_owner_requirements
            .iter()
            .all(|requirement| task_owner_requirement_pass(output, requirement));
    let state_application_pass = state_application_pass.unwrap_or(true);
    let branch_convergence_pass = branch_converged.unwrap_or(true);
    let closed_world_pass = closed_world_pass(output, case);
    let structured_labels_pass = structured_labels_pass(output, expected);
    let mut critical_errors = expected
        .critical_groups
        .iter()
        .filter(|group| !contains_any(output, group))
        .map(|group| {
            format!(
                "missing:{}",
                group.first().map(String::as_str).unwrap_or("?")
            )
        })
        .collect::<Vec<_>>();
    if !format_pass {
        critical_errors.push("format".to_string());
    }
    if !language_pass {
        critical_errors.push("language".to_string());
    }
    if !constraint_pass {
        critical_errors.push("constraint".to_string());
    }
    if !provenance_pass {
        critical_errors.push("provenance_receipt".to_string());
    }
    if !tool_policy_pass {
        critical_errors.push("tool_policy".to_string());
    }
    if !relation_pass {
        critical_errors.push("relation".to_string());
    }
    if !state_application_pass {
        critical_errors.push("state_application".to_string());
    }
    if !branch_convergence_pass {
        critical_errors.push("branch_non_converged".to_string());
    }
    if !closed_world_pass {
        critical_errors.push("closed_world".to_string());
    }
    if !structured_labels_pass {
        critical_errors.push("structured_labels".to_string());
    }
    critical_errors.extend(
        forbidden_hits
            .iter()
            .map(|term| format!("forbidden:{term}")),
    );
    critical_errors.extend(forbidden_relation_hits.iter().map(|requirement| {
        format!(
            "forbidden_relation:{}",
            requirement
                .groups
                .iter()
                .filter_map(|group| group.first())
                .cloned()
                .collect::<Vec<_>>()
                .join("+")
        )
    }));
    critical_errors.extend(forbidden_section_hits.iter().map(|requirement| {
        format!(
            "forbidden_section:{}:{}",
            requirement.headings.join("|"),
            requirement
                .groups
                .iter()
                .filter_map(|group| group.first())
                .cloned()
                .collect::<Vec<_>>()
                .join("+")
        )
    }));
    critical_errors.extend(missing_sections);
    if let Some(kind) = error {
        critical_errors.push(format!("runtime:{kind}"));
    }
    let raw = required_ratio * 50.0
        + if format_pass { 10.0 } else { 0.0 }
        + if section_pass { 10.0 } else { 0.0 }
        + if language_pass { 10.0 } else { 0.0 }
        + if forbidden_pass { 10.0 } else { 0.0 }
        + if constraint_pass { 5.0 } else { 0.0 }
        + if provenance_pass { 5.0 } else { 0.0 };
    let case_pass = error.is_none()
        && critical_errors.is_empty()
        && required_groups_hit == expected.required_groups.len()
        && format_pass
        && section_pass
        && language_pass
        && forbidden_pass
        && constraint_pass
        && provenance_pass
        && tool_policy_pass
        && relation_pass
        && state_application_pass
        && branch_convergence_pass
        && closed_world_pass
        && structured_labels_pass;
    let uncapped_diagnostic = (raw * 10.0).round() / 10.0;
    OracleScore {
        diagnostic_score: if critical_errors.is_empty() {
            uncapped_diagnostic
        } else {
            uncapped_diagnostic.min(49.0)
        },
        case_pass,
        critical_failure: !critical_errors.is_empty(),
        required_groups_hit,
        required_groups_total: expected.required_groups.len(),
        format_pass,
        section_pass,
        language_pass,
        forbidden_pass,
        constraint_pass,
        provenance_pass,
        tool_policy_pass,
        relation_pass,
        state_application_pass,
        branch_convergence_pass,
        closed_world_pass,
        structured_labels_pass,
        critical_errors,
    }
}

fn prompt_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hex_digest(hasher.finalize().as_slice())
}

fn string_sequence_hash(values: &[String]) -> String {
    let parts = values.iter().map(String::as_str).collect::<Vec<_>>();
    prompt_hash(&parts)
}

fn canonical_json_hash(value: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(value).expect("serialize canonical quality value");
    prompt_hash(&[&canonical])
}

static ARTIFACT_PRIVACY_PATTERNS: LazyLock<Vec<(&'static str, regex::Regex)>> = LazyLock::new(
    || {
        [
            ("email", r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b"),
            ("international_phone", r"\+\d(?:[ .()-]?\d){6,}"),
            ("macos_user_path", r"/Users/[^/\s]+(?:/[^\s]*)?"),
            ("linux_user_path", r"/home/[^/\s]+(?:/[^\s]*)?"),
            ("windows_user_path", r"(?i)[A-Z]:\\Users\\[^\\\s]+"),
            ("external_url", r"(?i)\b(?:https?|file)://[^\s]+"),
            ("pem_private_key", r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"),
            ("bearer_token", r"(?i)\bbearer\s+[A-Za-z0-9._~+/-]{12,}"),
            ("jwt", r"\b[A-Za-z0-9_-]{12,}\.[A-Za-z0-9_-]{12,}\.[A-Za-z0-9_-]{12,}\b"),
            (
                "provider_secret",
                r"(?i)\b(?:sk-[A-Za-z0-9_-]{12,}|gh[pousr]_[A-Za-z0-9]{12,}|xox[baprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16})\b",
            ),
        ]
        .into_iter()
        .map(|(rule, pattern)| (rule, regex::Regex::new(pattern).expect("valid privacy regex")))
        .collect()
    },
);

fn privacy_pointer_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Walk an artifact without ever returning the matched content. Error strings contain only a rule
/// id and JSON pointer, so the audit itself cannot leak the value it rejected into logs.
fn artifact_privacy_violation(value: &serde_json::Value) -> Option<String> {
    fn walk(value: &serde_json::Value, pointer: &str) -> Option<String> {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, child) in fields {
                    let lower = key.to_ascii_lowercase();
                    if [
                        "prompt",
                        "prompttext",
                        "systemprompt",
                        "userprompt",
                        "apikey",
                        "authorization",
                        "credential",
                        "secret",
                    ]
                    .contains(&lower.as_str())
                    {
                        return Some(format!(
                            "forbidden_artifact_field:{}:/{}",
                            lower,
                            privacy_pointer_segment(key)
                        ));
                    }
                    let child_pointer = format!("{pointer}/{}", privacy_pointer_segment(key));
                    if let Some(violation) = walk(child, &child_pointer) {
                        return Some(violation);
                    }
                }
                None
            }
            serde_json::Value::Array(values) => values
                .iter()
                .enumerate()
                .find_map(|(index, child)| walk(child, &format!("{pointer}/{index}"))),
            serde_json::Value::String(text) => ARTIFACT_PRIVACY_PATTERNS
                .iter()
                .find(|(_, pattern)| pattern.is_match(text))
                .map(|(rule, _)| format!("artifact_privacy:{rule}:{pointer}")),
            _ => None,
        }
    }
    walk(value, "")
}

fn case_payload_sha256(case: &QualityCase) -> String {
    canonical_json_hash(&serde_json::json!([
        "murmur-quality-case-payload-v2",
        &case.id,
        case.surface,
        case.language,
        &case.transcript,
        &case.question,
        &case.date_iso,
        &case.title_hint,
        &case.vault_titles,
        case.labeled,
        case.diarized_others,
        case.duration_s,
        &case.action,
        &case.selection,
        &case.before,
        &case.previous_bullets,
        &case.tool_result,
        &case.search_result,
        &case.search_terms,
        &case.floor_corpus,
        &case.synthetic_redaction_entities,
    ]))
}

fn case_record_sha256(case: &CaseResult) -> String {
    let score = &case.score;
    let value = serde_json::json!([
        &case.case_id,
        &case.case_payload_sha256,
        case.surface,
        case.language,
        case.model_class,
        case.holdout,
        &case.route_input_sha256,
        &case.generation_profile,
        &case.product_route,
        case.comparison_scope,
        case.route_input_chars,
        case.output_chars,
        &case.output_sha256,
        case.duration_ms,
        &case.output,
        &case.surface_output,
        &case.surface_output_sha256,
        &case.raw_model_output,
        &case.raw_model_output_sha256,
        case.raw_model_format_pass,
        case.structured_schema_pass,
        case.structured_labels_pass,
        case.structured_envelope_pass,
        &case.error,
        &case.tool_steps,
        case.tool_policy_score,
        case.tool_policy_pass,
        case.state_application_pass,
        case.branch_converged,
        &case.provenance,
        &case.provenance_sha256,
        &case.tool_steps_sha256,
        case.egress_receipt_start_ordinal,
        case.egress_receipt_end_ordinal,
        case.egress_receipt_count,
        &case.egress_receipt_sha256,
        [
            serde_json::json!(case.dimensions.retrieval_quality),
            serde_json::json!(case.dimensions.tool_agent_execution),
            serde_json::json!(case.dimensions.final_product_output_contract),
        ],
        [
            serde_json::json!(score.diagnostic_score),
            serde_json::json!(score.case_pass),
            serde_json::json!(score.critical_failure),
            serde_json::json!(score.required_groups_hit),
            serde_json::json!(score.required_groups_total),
            serde_json::json!(score.format_pass),
            serde_json::json!(score.section_pass),
            serde_json::json!(score.language_pass),
            serde_json::json!(score.forbidden_pass),
            serde_json::json!(score.constraint_pass),
            serde_json::json!(score.provenance_pass),
            serde_json::json!(score.tool_policy_pass),
            serde_json::json!(score.relation_pass),
            serde_json::json!(score.state_application_pass),
            serde_json::json!(score.branch_convergence_pass),
            serde_json::json!(score.closed_world_pass),
            serde_json::json!(score.structured_labels_pass),
            serde_json::json!(&score.critical_errors),
        ]
    ]);
    canonical_json_hash(&value)
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn command_version(binary: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(binary)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn codex_runtime_version() -> Option<String> {
    command_version("/opt/homebrew/bin/codex", &["--version"])
}

fn brain_runtime_version() -> Option<String> {
    std::env::var_os("MURMUR_BRAIN_SIDECAR")
        .filter(|value| !value.is_empty())
        .map(|_| "murmur-brain-workspace-build".to_string())
}

fn brain_runtime_sha256() -> Option<String> {
    std::env::var_os("MURMUR_BRAIN_SIDECAR")
        .filter(|value| !value.is_empty())
        .and_then(|path| sha256_file(Path::new(&path)).ok())
}

fn sysctl_value(key: &str) -> Option<String> {
    command_version("sysctl", &["-n", key])
}

fn tracked_diff_sha256() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["diff", "--binary", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| hex_digest(Sha256::digest(&output.stdout).as_slice()))
}

fn working_tree_dirty() -> bool {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .is_some_and(|output| output.status.success() && !output.stdout.is_empty())
}

fn environment_metadata(
    arm_order: Vec<String>,
    repetition: String,
    snapshot: &RunSnapshot,
) -> EnvironmentMetadata {
    EnvironmentMetadata {
        hardware_model: sysctl_value("hw.model"),
        cpu_brand: sysctl_value("machdep.cpu.brand_string"),
        memory_bytes: sysctl_value("hw.memsize").and_then(|value| value.parse().ok()),
        os_version: command_version("sw_vers", &["-productVersion"]),
        os_build: command_version("sw_vers", &["-buildVersion"]),
        name_redactor_mode: "forced_noop_for_deterministic_synthetic_benchmark",
        tracked_diff_sha256: snapshot.tracked_diff_sha256.clone(),
        working_tree_dirty: snapshot.working_tree_dirty,
        arm_order,
        repetition,
    }
}

fn error_kind(error: &AppError) -> &'static str {
    match error {
        AppError::Audio(_) => "audio",
        AppError::Transcribe(_) => "transcribe",
        AppError::Summarize(_) => "summarize",
        AppError::Export(_) => "export",
        AppError::Storage(_) => "storage",
        AppError::Migration(_) => "migration",
        AppError::Auth(_) => "auth",
        AppError::Locked(_) => "locked",
        AppError::Secrets(_) => "secrets",
        AppError::KeychainDenied(_) => "keychain_denied",
        AppError::BiometricFailed(_) => "biometric_failed",
        AppError::Config(_) => "config",
        AppError::Unavailable(_) => "unavailable",
        AppError::InvalidArg(_) => "invalid_arg",
        AppError::Other(_) => "other",
    }
}

fn repository_commit() -> String {
    command_version("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string())
}

fn source_fingerprint() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut hasher = Sha256::new();
    for relative in SOURCE_FINGERPRINT_FILES {
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        let bytes = std::fs::read(root.join(relative)).unwrap_or_else(|error| {
            panic!("quality source fingerprint dependency {relative} is unreadable: {error}")
        });
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hex_digest(hasher.finalize().as_slice())
}

fn run_snapshot() -> RunSnapshot {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    RunSnapshot {
        repository_commit: repository_commit(),
        source_fingerprint_sha256: source_fingerprint(),
        manifest_sha256: prompt_hash(&[MANIFEST_JSON]),
        evaluator_file_sha256: sha256_file(&root.join("src/eval/generation_quality.rs"))
            .expect("hash evaluator source"),
        fixture_file_sha256: sha256_file(&root.join("src/eval/fixtures/local-cloud-quality.json"))
            .expect("hash quality fixture"),
        repeat_validator_file_sha256: sha256_file(
            &root.join("../eval/results/validate_generation_quality_repeats.py"),
        )
        .expect("hash repeat validator"),
        tracked_diff_sha256: tracked_diff_sha256(),
        working_tree_dirty: working_tree_dirty(),
    }
}

struct ControlledProductExecutor {
    scope: crate::tools::AssistantScope,
    note_drafts: bool,
    search_result: String,
    search_terms: Vec<String>,
    meeting_result: String,
    calls: Mutex<Vec<String>>,
}

impl ControlledProductExecutor {
    fn record_tool_attempt(&self, name: &str, succeeded: bool) -> Result<()> {
        let label = if succeeded {
            name.to_string()
        } else {
            format!("failed:{name}")
        };
        self.calls
            .lock()
            .map_err(|_| AppError::Storage("quality executor mutex poisoned".into()))?
            .push(label);
        Ok(())
    }
}

fn controlled_product_specs(
    scope: crate::tools::AssistantScope,
    note_drafts: bool,
) -> Vec<ToolSpec> {
    crate::tools::tool_specs()
        .into_iter()
        .filter(|spec| scope.allows(&spec.name))
        .filter(|spec| match spec.name.as_str() {
            // Canonical synthetic configuration: no joined org and no dynamic MCP rows.
            "org_brain_search" => false,
            "propose_note" => note_drafts,
            _ if spec.write => false,
            _ => true,
        })
        .collect()
}

fn tool_specs_json(executor: &dyn ToolExecutor) -> String {
    serde_json::Value::Array(
        executor
            .specs()
            .into_iter()
            .map(|spec| {
                serde_json::json!({
                    "name": spec.name,
                    "description": spec.description,
                    "parameters": spec.parameters,
                    "write": spec.write,
                })
            })
            .collect(),
    )
    .to_string()
}

impl ToolExecutor for ControlledProductExecutor {
    fn specs(&self) -> Vec<ToolSpec> {
        controlled_product_specs(self.scope, self.note_drafts)
    }

    fn run(&self, name: &str, args: &serde_json::Value) -> Result<String> {
        if !self.specs().iter().any(|spec| spec.name == name) {
            self.record_tool_attempt(name, false)?;
            return Err(AppError::InvalidArg(format!(
                "quality executor refused unavailable tool {name}"
            )));
        }
        let prior_search = self
            .calls
            .lock()
            .map_err(|_| AppError::Storage("quality executor mutex poisoned".into()))?
            .iter()
            .any(|call| matches!(call.as_str(), "search_meetings" | "search_semantic"));
        let output = match name {
            "search_meetings" | "search_semantic"
                if args
                    .get("query")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|query| {
                        let query = query.trim().to_lowercase();
                        !query.is_empty()
                            && (self.search_terms.is_empty()
                                || self
                                    .search_terms
                                    .iter()
                                    .any(|term| query.contains(&term.to_lowercase())))
                    }) =>
            {
                self.search_result.clone()
            }
            "get_meeting"
                if prior_search
                    && args
                        .get("meetingId")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| {
                            self.search_result.contains(&format!("[meeting:{id}]"))
                        }) =>
            {
                self.meeting_result.clone()
            }
            "propose_note"
                if args
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|content| !content.trim().is_empty()) =>
            {
                "Synthetic note draft captured; no write occurred.".to_string()
            }
            other
                if !matches!(
                    other,
                    "search_meetings" | "search_semantic" | "get_meeting" | "propose_note"
                ) && self.specs().iter().any(|spec| spec.name == other) =>
            {
                "No synthetic results for this benchmark tool.".to_string()
            }
            _ => {
                self.record_tool_attempt(name, false)?;
                return Err(AppError::InvalidArg(format!(
                    "quality executor refused invalid {name} call"
                )));
            }
        };
        self.record_tool_attempt(name, true)?;
        Ok(output)
    }
}

fn tool_policy_score(
    required_tools: &[String],
    allowed_tools: Option<&[String]>,
    executor_calls: &[String],
) -> (Option<f64>, Option<bool>) {
    if required_tools.is_empty() && allowed_tools.is_none() {
        return (None, None);
    }
    let required_hits = required_tools
        .iter()
        .filter(|tool| executor_calls.iter().any(|call| call == *tool))
        .count();
    let allowed_pass = allowed_tools.is_none_or(|allowed| {
        executor_calls
            .iter()
            .all(|call| allowed.iter().any(|tool| tool == call))
    });
    // The controlled Ask executor cannot produce any successful `get_meeting` before a successful
    // locator call. Preserve that prerequisite for every recorded get in both live scoring and
    // artifact replay; one later valid pair cannot legitimize an impossible earlier read.
    let prerequisites_pass = executor_calls.iter().enumerate().all(|(index, call)| {
        call != "get_meeting"
            || executor_calls[..index]
                .iter()
                .any(|prior| matches!(prior.as_str(), "search_meetings" | "search_semantic"))
    });
    let required_ratio = if required_tools.is_empty() {
        1.0
    } else {
        required_hits as f64 / required_tools.len() as f64
    };
    let score = if allowed_pass && prerequisites_pass {
        required_ratio * 100.0
    } else {
        0.0
    };
    (
        Some((score * 10.0).round() / 10.0),
        Some(required_hits == required_tools.len() && allowed_pass && prerequisites_pass),
    )
}

fn outcome_execution(
    result: Result<Option<AgentOutcome>>,
    prompt_sha256: String,
    input_chars: usize,
    required_tools: &[String],
    allowed_tools: Option<&[String]>,
    executor_calls: Vec<String>,
    product_route: &str,
) -> Execution {
    let (tool_policy_score, tool_policy_pass) =
        tool_policy_score(required_tools, allowed_tools, &executor_calls);
    match result {
        Ok(Some(outcome)) => Execution {
            output: outcome.answer,
            surface_output: None,
            raw_model_output: None,
            raw_model_format_pass: None,
            error: None,
            prompt_sha256,
            input_chars,
            product_route: product_route.to_string(),
            comparison_scope: ComparisonScope::ProductPath,
            tool_steps: executor_calls,
            tool_policy_score,
            tool_policy_pass,
            state_application_pass: None,
            branch_converged: Some(true),
            provenance: outcome.citations,
        },
        Ok(None) => Execution {
            output: String::new(),
            surface_output: None,
            raw_model_output: None,
            raw_model_format_pass: None,
            error: Some("non_converged".to_string()),
            prompt_sha256,
            input_chars,
            product_route: product_route.to_string(),
            comparison_scope: ComparisonScope::ProductPath,
            tool_steps: executor_calls,
            tool_policy_score,
            tool_policy_pass,
            state_application_pass: None,
            branch_converged: Some(false),
            provenance: Vec::new(),
        },
        Err(error) => Execution {
            output: String::new(),
            surface_output: None,
            raw_model_output: None,
            raw_model_format_pass: None,
            error: Some(format!("model_call_failed:{}", error_kind(&error))),
            prompt_sha256,
            input_chars,
            product_route: product_route.to_string(),
            comparison_scope: ComparisonScope::ProductPath,
            tool_steps: executor_calls,
            tool_policy_score,
            tool_policy_pass,
            state_application_pass: None,
            branch_converged: Some(false),
            provenance: Vec::new(),
        },
    }
}

fn extract_wikilinks(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("[[") {
        rest = &rest[open..];
        let Some(close) = rest.find("]]") else {
            break;
        };
        let link = rest[..close + 2].to_string();
        if !links.iter().any(|seen| seen == &link) {
            links.push(link);
        }
        rest = &rest[close + 2..];
    }
    links
}

fn summary_product_output(raw: &str, case: &QualityCase) -> String {
    let finalized = crate::pipeline::finalize_note_markdown(raw, "", "append");
    let title = crate::pipeline::derive_title(&finalized, &case.date_iso);
    let vars = crate::pipeline::resolve_vars_from_note(
        Vec::new(),
        &title,
        &case.date_iso,
        case.duration_s,
        Some(case.language.as_str()),
        &finalized,
    );
    crate::summarize::template::assemble_note_with_template(None, &vars, &finalized)
}

fn summary_request(case: &QualityCase) -> SummarizeRequest {
    SummarizeRequest {
        transcript: case.transcript.clone(),
        meta: MeetingMeta {
            date_iso: case.date_iso.clone(),
            title_hint: (!case.title_hint.is_empty()).then(|| case.title_hint.clone()),
            duration_s: case.duration_s,
            language: Some(case.language.as_str().to_string()),
        },
        template: crate::summarize::template::build_template(
            "standard",
            case.language.as_str(),
            case.labeled,
            case.diarized_others,
            "",
        ),
        vault_titles: case.vault_titles.clone(),
        related_context: None,
        user_notes: None,
        live_bullets: None,
        glossary: None,
    }
}

fn note_assist_prompt(case: &QualityCase) -> (String, String) {
    let request = NoteAssistRequest {
        note_id: format!("quality-{}", case.id),
        action: case.action.clone(),
        selection: case.selection.clone(),
        before: (!case.before.is_empty()).then(|| case.before.clone()),
        after: None,
        variant: None,
        instruction: None,
    };
    let citations = if case.tool_result.is_empty() {
        Vec::new()
    } else {
        vec![NoteCitation {
            kind: "meeting".to_string(),
            id: format!("synthetic-source-{}", case.id),
            title: "Synthetic benchmark source".to_string(),
            snippet: case.tool_result.clone(),
        }]
    };
    crate::commands::build_note_assist_prompt(
        &case.action,
        &request,
        &citations,
        &[],
        case.language.as_str(),
    )
}

/// Replace evaluator-declared synthetic entities with stable opaque labels BEFORE either arm sees
/// the envelope. This prevents the cloud name-redactor from changing only one candidate's input;
/// the reverse mapping is applied uniformly before scoring. The fixture remains the source of truth
/// and the product-path lane continues to exercise the real redaction behavior without substitution.
fn model_only_substitutions(case: &QualityCase) -> Vec<(String, String)> {
    let mut originals = case.synthetic_redaction_entities.clone();
    // Names embedded in production instruction examples rather than in fixture content.
    originals.extend(["Anna".to_string(), "Sara".to_string(), "Iga".to_string()]);
    let mut seen = BTreeSet::new();
    originals.retain(|value| seen.insert(value.clone()));
    let mut substitutions = originals
        .into_iter()
        .enumerate()
        .map(|(index, original)| (format!("SYNTH_ENTITY_{:02}", index + 1), original))
        .collect::<Vec<_>>();
    // This static wikilink example otherwise trips the canonical no-NER person-title fallback and
    // becomes the lossy `[[(person)]]`, slightly degrading the instruction for both candidates.
    substitutions.push(("SYNTH_NOTE_TITLE".to_string(), "Exact Title".to_string()));
    // Replace longer source labels first so an entity contained inside another entity name cannot
    // partially consume it. Stable sort preserves the fixture-declared token numbering.
    substitutions.sort_by_key(|(_, original)| std::cmp::Reverse(original.chars().count()));
    substitutions
}

fn apply_model_only_substitutions(text: &str, substitutions: &[(String, String)]) -> String {
    substitutions
        .iter()
        .fold(text.to_string(), |rendered, (opaque, original)| {
            rendered.replace(original, opaque)
        })
}

fn reverse_model_only_substitutions(text: &str, substitutions: &[(String, String)]) -> String {
    substitutions
        .iter()
        .fold(text.to_string(), |rendered, (opaque, original)| {
            rendered.replace(opaque, original)
        })
}

/// Apply the production regex firewall once, candidate-independently, before either model sees the
/// model-only envelope. `complete_with_meta` treats ISO dates inside a free-text prompt as possible
/// phone numbers (unlike `SummarizeRequest::meta.date_iso`, which is structurally exempt), so this
/// canonical pre-scrub is required for byte equality. The token map is reversed before scoring and
/// is sorted before hashing so committed evidence replays without a model or network.
fn apply_model_only_regex_substitutions(
    system: String,
    user: String,
    substitutions: &mut Vec<(String, String)>,
) -> (String, String) {
    const BOUNDARY: &str = "\u{001e}MURMUR_FIELD_BOUNDARY\u{001f}";
    let combined = format!("{system}{BOUNDARY}{user}");
    let (scrubbed, regex_map) = crate::summarize::redact::redact(&combined);
    let mut fields = scrubbed.split(BOUNDARY);
    let mut system = fields.next().unwrap_or_default().to_string();
    let mut user = fields.next().unwrap_or_default().to_string();
    assert!(
        fields.next().is_none(),
        "model-only field boundary must remain unique after canonical regex redaction"
    );
    let mut regex_pairs = regex_map.into_iter().collect::<Vec<_>>();
    regex_pairs.sort_by(|left, right| left.0.cmp(&right.0));
    for (canonical_token, original) in regex_pairs {
        let ordinal = canonical_token
            .trim_end_matches('\u{27eb}')
            .rsplit('_')
            .next()
            .unwrap_or("0");
        let semantic_token = if iso_date_shape(&original) {
            format!("SYNTH_DATE_{ordinal}_{}", original.replace('-', "_"))
        } else if decimal_timespan_shape(&original) {
            format!(
                "SYNTH_TIMESPAN_{ordinal}_{}",
                original.replace('.', "_").replace('-', "_TO_")
            )
        } else {
            let kind = canonical_token
                .trim_start_matches('\u{27ea}')
                .split('_')
                .next()
                .unwrap_or("PII");
            format!("SYNTH_PII_{kind}_{ordinal}")
        };
        system = system.replace(&canonical_token, &semantic_token);
        user = user.replace(&canonical_token, &semantic_token);
        substitutions.push((semantic_token, original));
    }
    (system, user)
}

fn iso_date_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn decimal_timespan_shape(value: &str) -> bool {
    fn point(part: &str) -> bool {
        let mut pieces = part.split('.');
        let whole = pieces.next().unwrap_or_default();
        let fraction = pieces.next();
        !whole.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
            })
            && pieces.next().is_none()
    }
    value
        .split_once('-')
        .is_some_and(|(start, end)| point(start) && point(end))
}

fn model_only_parse_live_bullets(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(crate::prompts::LIVE_BULLETS_NOTHING) {
        return None;
    }
    let lines = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("- ") && line.len() > 2)
        .take(crate::transcribe::bullets::MAX_NEW_BULLETS)
        .collect::<Vec<_>>();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn model_only_fact_extraction_contract(
    title: &str,
    note_markdown: &str,
    entity_names: &[String],
    note_language: &str,
) -> (String, String, serde_json::Value) {
    const EXTRACT_SYSTEM: &str = "You extract durable FACTS about specific entities from a meeting \
note, as entity·predicate·object triples. Output STRICT JSON ONLY (no prose, no code fences): \
{\"facts\":[{\"entity\":\"Exact Entity Name\",\"predicate\":\"short attribute\",\"object\":\"value\"}]}.\n\
- entity MUST be one of the ENTITIES listed (copy the name exactly).\n\
- predicate is a short, stable attribute (e.g. \"status\", \"owner\", \"deadline\", \"role\").\n\
- object is the current value (e.g. \"shipped\", \"Anna\", \"2026-07-01\").\n\
- Only durable state worth tracking across meetings — not one-off remarks. Empty array if none.\n\
Output ONLY the JSON.";
    let system = match crate::summarize::template::language_name(note_language) {
        Some(name) => format!(
            "{EXTRACT_SYSTEM}\n\
LANGUAGE: Write EVERY predicate and object in {name}. Use ONE language for all facts. \
NEVER output the same fact twice in two languages (never emit both a {name} and an English version \
of one attribute). Keep the ENTITY name EXACTLY as listed — do not translate it."
        ),
        None => format!(
            "{EXTRACT_SYSTEM}\n\
LANGUAGE: Write predicates and objects in the SAME language as the NOTE below; use ONE consistent \
language for all facts; never emit the same fact in two languages. Keep the ENTITY name EXACTLY as \
listed — do not translate it."
        ),
    };
    let excerpt = note_markdown.chars().take(8_000).collect::<String>();
    let names = entity_names
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let user = format!("MEETING: {title}\n\nENTITIES: {names}\n\nNOTE:\n{excerpt}");
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "facts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "entity": { "type": "string" },
                        "predicate": { "type": "string" },
                        "object": { "type": "string" }
                    },
                    "required": ["entity", "predicate", "object"]
                }
            }
        },
        "required": ["facts"]
    });
    (system, user, schema)
}

fn build_same_envelope(case: &QualityCase) -> SameEnvelope {
    let (system, user, projection, output_contract) = match case.surface {
        Surface::Summary => {
            let request = summary_request(case);
            (
                request.template.clone(),
                crate::summarize::template::render_user_content(&request),
                ModelOnlyProjection::SummaryAssembly,
                "summary_pipeline_assembly_then_deterministic_oracle_v6",
            )
        }
        Surface::MeetingChat => {
            let (system, user) =
                crate::summarize::chat::build(&case.transcript, &[], &case.question, "");
            (
                system,
                user,
                ModelOnlyProjection::RawTrimmed,
                "deterministic_oracle_v6",
            )
        }
        Surface::NoteAssist => {
            let (system, user) = note_assist_prompt(case);
            (
                system,
                user,
                ModelOnlyProjection::RawTrimmed,
                "single_call_note_assist_deterministic_oracle_v6",
            )
        }
        Surface::AskVault => {
            let corpus = if case.floor_corpus.is_empty() {
                &case.tool_result
            } else {
                &case.floor_corpus
            };
            let (system, user) =
                crate::summarize::vault_chat::build(corpus, &[], &case.question, "");
            (
                system,
                user,
                ModelOnlyProjection::RawTrimmed,
                "single_call_no_tools_vault_answer_deterministic_oracle_v6",
            )
        }
        Surface::LiveCurrent => {
            let (system, user) = crate::voice_action::current_meeting_isolated_prompt(
                &case.question,
                &case.transcript,
            );
            (
                system.to_string(),
                user,
                ModelOnlyProjection::RawTrimmed,
                "single_call_no_cascade_current_meeting_deterministic_oracle_v6",
            )
        }
        Surface::LiveBullets => (
            crate::prompts::LIVE_BULLETS_SYSTEM.to_string(),
            crate::prompts::live_bullets_user(&case.previous_bullets, &case.transcript),
            ModelOnlyProjection::LiveBullets,
            "shared_live_bullet_parser_and_append_contract_v1",
        ),
        Surface::LightExtraction => {
            let (mut system, user, schema) = model_only_fact_extraction_contract(
                &case.title_hint,
                &case.transcript,
                &case.vault_titles,
                case.language.as_str(),
            );
            let schema = serde_json::to_string(&schema).expect("serialize fact extraction schema");
            system.push_str(
                "\n\nCOMMON FREE-TEXT OUTPUT CONTRACT: Return exactly one JSON object matching \
                 this schema. Do not use Markdown fences or prose. JSON_SCHEMA:\n",
            );
            system.push_str(&schema);
            (
                system,
                user,
                ModelOnlyProjection::StructuredFacts,
                "shared_parse_first_json_exact_fact_projection_v1",
            )
        }
    };
    for reserved in [
        "\u{27ea}EMAIL_",
        "\u{27ea}CARD_",
        "\u{27ea}PHONE_",
        "SYNTH_ENTITY_",
        "SYNTH_NOTE_TITLE",
        "SYNTH_DATE_",
        "SYNTH_TIMESPAN_",
        "SYNTH_PII_",
    ] {
        assert!(
            !system.contains(reserved) && !user.contains(reserved),
            "quality case {} contains reserved evaluator token {reserved}",
            case.id
        );
    }
    let mut substitutions = model_only_substitutions(case);
    let system = apply_model_only_substitutions(&system, &substitutions);
    let user = apply_model_only_substitutions(&user, &substitutions);
    let (system, user) = apply_model_only_regex_substitutions(system, user, &mut substitutions);
    // The canonical COMPLETE firewall also has a no-NER, drop-only scrub for person-shaped
    // structural titles. Apply it to both candidates up front as part of the evaluator-owned
    // envelope; fixture entities were already replaced by reversible SYNTH_ENTITY labels above.
    let system = crate::summarize::redact::scrub_person_name_titles(&system);
    let user = crate::summarize::redact::scrub_person_name_titles(&user);
    assert!(
        !system.contains("(person)") && !user.contains("(person)"),
        "model-only canonical title scrub lost instructional or fixture semantics for {}",
        case.id
    );
    for alternatives in &case.expected.required_provenance {
        assert!(
            alternatives
                .iter()
                .any(|value| system.contains(value) || user.contains(value)),
            "model-only envelope lost required provenance source for {}",
            case.id
        );
    }
    let system_sha256 = prompt_hash(&[&system]);
    let user_sha256 = prompt_hash(&[&user]);
    let envelope_sha256 = prompt_hash(&[
        "murmur-same-caller-envelope-v2",
        projection.as_str(),
        output_contract,
        &system,
        &user,
    ]);
    SameEnvelope {
        system,
        user,
        projection,
        output_contract,
        substitutions,
        system_sha256,
        user_sha256,
        envelope_sha256,
    }
}

fn project_model_only_output(
    raw: &str,
    envelope: &SameEnvelope,
    case: &QualityCase,
) -> (String, Vec<String>, Option<bool>, Option<String>) {
    let raw = reverse_model_only_substitutions(raw.trim(), &envelope.substitutions);
    match envelope.projection {
        ModelOnlyProjection::SummaryAssembly => {
            (summary_product_output(&raw, case), Vec::new(), None, None)
        }
        ModelOnlyProjection::RawTrimmed => {
            let provenance = if case.surface == Surface::AskVault {
                let corpus = if case.floor_corpus.is_empty() {
                    &case.tool_result
                } else {
                    &case.floor_corpus
                };
                extract_wikilinks(corpus)
            } else {
                Vec::new()
            };
            (raw, provenance, None, None)
        }
        ModelOnlyProjection::LiveBullets => {
            let Some(accepted) = model_only_parse_live_bullets(&raw) else {
                return (
                    String::new(),
                    Vec::new(),
                    Some(false),
                    Some("model_only_bullet_projection_empty".to_string()),
                );
            };
            let surface =
                crate::transcribe::bullets::append_bullets(&case.previous_bullets, &accepted);
            let state_pass = surface.contains(case.previous_bullets.trim())
                && accepted
                    .lines()
                    .all(|line| line.trim().is_empty() || surface.contains(line.trim()));
            (accepted, Vec::new(), Some(state_pass), None)
        }
        ModelOnlyProjection::StructuredFacts => {
            let value = match crate::reason::parse_first_json::<serde_json::Value>(&raw) {
                Ok(value) => value,
                Err(_) => {
                    return (
                        String::new(),
                        Vec::new(),
                        None,
                        Some("model_only_structured_parse_failed".to_string()),
                    )
                }
            };
            let canonical = serde_json::to_string(&value).expect("serialize parsed fact object");
            let Some(mut facts) = parsed_structured_facts(&canonical) else {
                return (
                    String::new(),
                    Vec::new(),
                    None,
                    Some("model_only_structured_schema_failed".to_string()),
                );
            };
            facts.sort();
            (
                serde_json::to_string(&serde_json::json!({ "facts": facts }))
                    .expect("serialize sorted fact projection"),
                Vec::new(),
                None,
                None,
            )
        }
    }
}

fn plain_execution(
    output: String,
    error: Option<String>,
    prompt_sha256: String,
    input_chars: usize,
    product_route: &str,
) -> Execution {
    Execution {
        output,
        surface_output: None,
        raw_model_output: None,
        raw_model_format_pass: None,
        error,
        prompt_sha256,
        input_chars,
        product_route: product_route.to_string(),
        comparison_scope: ComparisonScope::ProductPath,
        tool_steps: Vec::new(),
        tool_policy_score: None,
        tool_policy_pass: None,
        state_application_pass: None,
        branch_converged: None,
        provenance: Vec::new(),
    }
}

struct ControlledCascadeAttempt {
    execution: Option<Execution>,
    prompt_sha256: String,
    input_chars: usize,
    calls: Vec<String>,
}

fn controlled_cloud_live_cascade(
    arm: &ArmRuntime,
    case: &QualityCase,
    base_system: &str,
) -> ControlledCascadeAttempt {
    let tiers = [
        (
            crate::tools::AssistantScope::CurrentMeeting,
            crate::prompts::TIER1_SUFFIX,
            crate::transcribe::live::TIER1_MAX_STEPS,
            "live_current_cloud_tier1",
            true,
        ),
        (
            crate::tools::AssistantScope::Vault,
            crate::prompts::TIER2_SUFFIX,
            crate::transcribe::live::TIER2_MAX_STEPS,
            "live_current_cloud_tier2_vault",
            true,
        ),
        (
            crate::tools::AssistantScope::Connectors,
            crate::prompts::TIER3_SUFFIX,
            crate::transcribe::live::TIER3_MAX_STEPS,
            "live_current_cloud_tier3_connectors",
            false,
        ),
    ];
    let mut canonical_parts = vec![base_system.to_string(), case.question.clone()];
    for (scope, suffix, _, _, _) in tiers {
        let executor = ControlledProductExecutor {
            scope,
            note_drafts: true,
            search_result: String::new(),
            search_terms: Vec::new(),
            meeting_result: String::new(),
            calls: Mutex::new(Vec::new()),
        };
        canonical_parts.push(suffix.to_string());
        canonical_parts.push(tool_specs_json(&executor));
    }
    let (floor_system, floor_user) =
        crate::voice_action::current_meeting_isolated_prompt(&case.question, &case.transcript);
    canonical_parts.push(floor_system.to_string());
    canonical_parts.push(floor_user);
    canonical_parts.push("No synthetic results for this benchmark tool.".to_string());
    let canonical_refs = canonical_parts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let prompt_sha256 = prompt_hash(&canonical_refs);
    let input_chars = canonical_parts
        .iter()
        .map(|part| part.chars().count())
        .sum();
    let mut all_calls = Vec::new();

    for (scope, suffix, max_steps, route, may_escalate) in tiers {
        let executor = ControlledProductExecutor {
            scope,
            note_drafts: true,
            search_result: String::new(),
            search_terms: Vec::new(),
            meeting_result: String::new(),
            calls: Mutex::new(Vec::new()),
        };
        let system = format!("{base_system}\n\n{suffix}");
        let result = crate::agent::run_agentic_loop(
            arm.live_reasoner.as_ref(),
            &system,
            &case.question,
            &executor,
            max_steps,
            None,
            GenOptions::live_answer()
                .with_transcript_compaction(true)
                .with_grammar_constraint(false),
        );
        let calls = executor.calls.into_inner().unwrap_or_default();
        all_calls.extend(calls);
        match result {
            Ok(Some(outcome)) if crate::agent::is_escalation(&outcome.answer) => {
                if may_escalate {
                    continue;
                }
                break;
            }
            Ok(Some(outcome)) => {
                let mut execution = outcome_execution(
                    Ok(Some(outcome)),
                    prompt_sha256.clone(),
                    input_chars,
                    &case.expected.required_tools,
                    case.expected.allowed_tools.as_deref(),
                    all_calls.clone(),
                    route,
                );
                if !all_calls.is_empty() {
                    execution.tool_steps = all_calls.clone();
                }
                return ControlledCascadeAttempt {
                    execution: Some(execution),
                    prompt_sha256,
                    input_chars,
                    calls: all_calls,
                };
            }
            Ok(None) => continue,
            Err(_) => break,
        }
    }

    ControlledCascadeAttempt {
        execution: None,
        prompt_sha256,
        input_chars,
        calls: all_calls,
    }
}

async fn execute_case(arm: &ArmRuntime, case: &QualityCase) -> Execution {
    match case.surface {
        Surface::Summary => {
            let template = crate::summarize::template::build_template(
                "standard",
                case.language.as_str(),
                case.labeled,
                case.diarized_others,
                "",
            );
            let request = SummarizeRequest {
                transcript: case.transcript.clone(),
                meta: MeetingMeta {
                    date_iso: case.date_iso.clone(),
                    title_hint: (!case.title_hint.is_empty()).then(|| case.title_hint.clone()),
                    duration_s: case.duration_s,
                    language: Some(case.language.as_str().to_string()),
                },
                template: template.clone(),
                vault_titles: case.vault_titles.clone(),
                related_context: None,
                user_notes: None,
                live_bullets: None,
                glossary: None,
            };
            let canonical = serde_json::json!({
                "transcript": &request.transcript,
                "dateIso": &request.meta.date_iso,
                "titleHint": &request.meta.title_hint,
                "durationS": request.meta.duration_s,
                "language": &request.meta.language,
                "template": &template,
                "vaultTitles": &request.vault_titles,
            })
            .to_string();
            let prompt_sha256 = prompt_hash(&[&canonical]);
            let input_chars = canonical.chars().count();
            match arm.notes_provider.summarize(&request).await {
                Ok(raw) => {
                    let output = summary_product_output(&raw, case);
                    let raw_model_format_pass = Some(output_format_pass(&raw, &case.expected));
                    let mut execution = plain_execution(
                        output,
                        None,
                        prompt_sha256,
                        input_chars,
                        "summary_provider_then_pipeline_assembly",
                    );
                    execution.raw_model_output = Some(raw);
                    execution.raw_model_format_pass = raw_model_format_pass;
                    execution
                }
                Err(error) => plain_execution(
                    String::new(),
                    Some(format!("model_call_failed:{}", error_kind(&error))),
                    prompt_sha256,
                    input_chars,
                    "summary_provider_then_pipeline_assembly",
                ),
            }
        }
        Surface::MeetingChat => {
            let (system, user) =
                crate::summarize::chat::build(&case.transcript, &[], &case.question, "");
            let prompt_sha256 = prompt_hash(&[&system, &user]);
            let input_chars = system.chars().count() + user.chars().count();
            match arm.ask_provider.complete(&system, &user).await {
                Ok(output) => plain_execution(
                    output,
                    None,
                    prompt_sha256,
                    input_chars,
                    "meeting_chat_one_completion",
                ),
                Err(error) => plain_execution(
                    String::new(),
                    Some(format!("model_call_failed:{}", error_kind(&error))),
                    prompt_sha256,
                    input_chars,
                    "meeting_chat_one_completion",
                ),
            }
        }
        Surface::NoteAssist => {
            let request = NoteAssistRequest {
                note_id: format!("quality-{}", case.id),
                action: case.action.clone(),
                selection: case.selection.clone(),
                before: (!case.before.is_empty()).then(|| case.before.clone()),
                after: None,
                variant: None,
                instruction: None,
            };
            let citations = if case.tool_result.is_empty() {
                Vec::new()
            } else {
                vec![NoteCitation {
                    kind: "meeting".to_string(),
                    id: format!("synthetic-source-{}", case.id),
                    title: "Synthetic benchmark source".to_string(),
                    snippet: case.tool_result.clone(),
                }]
            };
            let (system, user) = crate::commands::build_note_assist_prompt(
                &case.action,
                &request,
                &citations,
                &[],
                case.language.as_str(),
            );
            let prompt_sha256 = prompt_hash(&[&system, &user]);
            let input_chars = system.chars().count() + user.chars().count();
            let opts = GenOptions::edit_rewrite(crate::commands::note_edit_max_tokens(
                &case.action,
                case.selection.chars().count(),
            ));
            match crate::commands::generate_note_edit(
                arm.notes_provider.as_ref(),
                &case.action,
                &system,
                &user,
                opts,
                word_count(&case.selection),
            )
            .await
            {
                Ok((output, _meta)) => plain_execution(
                    output.trim().to_string(),
                    None,
                    prompt_sha256,
                    input_chars,
                    "note_assist_generate_note_edit",
                ),
                Err(error) => plain_execution(
                    String::new(),
                    Some(format!("model_call_failed:{}", error_kind(&error))),
                    prompt_sha256,
                    input_chars,
                    "note_assist_generate_note_edit",
                ),
            }
        }
        Surface::AskVault => {
            let floor_corpus = if case.floor_corpus.is_empty() {
                &case.tool_result
            } else {
                &case.floor_corpus
            };
            let (floor_system, floor_user) =
                crate::summarize::vault_chat::build(floor_corpus, &[], &case.question, "");
            let floor_provenance = extract_wikilinks(floor_corpus);
            if !arm.cloud_reference {
                let prompt_sha256 = prompt_hash(&[&floor_system, &floor_user]);
                let input_chars = floor_system.chars().count() + floor_user.chars().count();
                let mut execution =
                    match arm.ask_provider.complete(&floor_system, &floor_user).await {
                        Ok(output) => plain_execution(
                            output,
                            None,
                            prompt_sha256,
                            input_chars,
                            "ask_vault_local_deterministic_floor",
                        ),
                        Err(error) => plain_execution(
                            String::new(),
                            Some(format!("model_call_failed:{}", error_kind(&error))),
                            prompt_sha256,
                            input_chars,
                            "ask_vault_local_deterministic_floor",
                        ),
                    };
                execution.tool_policy_pass = None;
                execution.provenance = floor_provenance;
                execution
            } else {
                let system = crate::summarize::vault_chat::agentic_system_jit("", "", false);
                let user = crate::summarize::vault_chat::render_conversation(&[], &case.question);
                let executor = ControlledProductExecutor {
                    scope: crate::tools::AssistantScope::Full,
                    note_drafts: false,
                    search_result: case.search_result.clone(),
                    search_terms: case.search_terms.clone(),
                    meeting_result: case.tool_result.clone(),
                    calls: Mutex::new(Vec::new()),
                };
                let specs = tool_specs_json(&executor);
                let prompt_sha256 = prompt_hash(&[
                    &system,
                    &user,
                    &specs,
                    &case.search_result,
                    &case.tool_result,
                    &floor_system,
                    &floor_user,
                ]);
                let input_chars = system.chars().count()
                    + user.chars().count()
                    + specs.chars().count()
                    + case.search_result.chars().count()
                    + case.tool_result.chars().count()
                    + floor_system.chars().count()
                    + floor_user.chars().count();
                let result = crate::agent::run_agentic_loop_with_policy(
                    arm.ask_reasoner.as_ref(),
                    &system,
                    &user,
                    &executor,
                    6,
                    None,
                    GenOptions::ask_answer()
                        .with_transcript_compaction(true)
                        .with_grammar_constraint(false),
                    crate::agent::AnswerGroundingPolicy::RetryUnknownAfterUnopenedSearchHit,
                );
                let calls = executor.calls.into_inner().unwrap_or_default();
                let branch = outcome_execution(
                    result,
                    prompt_sha256.clone(),
                    input_chars,
                    &case.expected.required_tools,
                    case.expected.allowed_tools.as_deref(),
                    calls,
                    "ask_vault_cloud_agentic",
                );
                if branch.branch_converged == Some(true) {
                    branch
                } else {
                    let mut fallback =
                        match arm.ask_provider.complete(&floor_system, &floor_user).await {
                            Ok(output) => plain_execution(
                                output,
                                None,
                                prompt_sha256,
                                input_chars,
                                "ask_vault_cloud_agentic_then_floor_fallback",
                            ),
                            Err(error) => plain_execution(
                                String::new(),
                                Some(format!("model_call_failed:{}", error_kind(&error))),
                                prompt_sha256,
                                input_chars,
                                "ask_vault_cloud_agentic_then_floor_fallback",
                            ),
                        };
                    fallback.tool_steps = branch.tool_steps;
                    fallback.tool_policy_score = branch.tool_policy_score;
                    // A correct deterministic floor remains visible in `output`, but it must not
                    // green-wash a non-converged staged Ask branch in the quality headline.
                    fallback.tool_policy_pass = Some(false);
                    fallback.branch_converged = Some(false);
                    fallback.provenance = floor_provenance;
                    fallback
                }
            }
        }
        Surface::LiveCurrent => {
            let base =
                crate::transcribe::live::assistant_system_prompt(&case.transcript, "", "", true);
            let (floor_system, floor_user) = crate::voice_action::current_meeting_isolated_prompt(
                &case.question,
                &case.transcript,
            );
            let local_prompt_sha256 = prompt_hash(&[floor_system, &floor_user]);
            let local_input_chars = floor_system.chars().count() + floor_user.chars().count();
            let floor = || {
                crate::voice_action::answer_current_meeting_isolated(
                    "research",
                    &case.question,
                    &case.transcript,
                    false,
                    (!case.title_hint.is_empty()).then_some(case.title_hint.as_str()),
                    arm.live_reasoner.as_ref(),
                )
            };
            if !arm.cloud_reference {
                let result = floor();
                let mut execution = plain_execution(
                    result.summary,
                    (result.status == "error").then(|| "product_floor_error".to_string()),
                    local_prompt_sha256,
                    local_input_chars,
                    "live_current_local_isolated_floor",
                );
                let (tool_score, tool_pass) = tool_policy_score(
                    &case.expected.required_tools,
                    case.expected.allowed_tools.as_deref(),
                    &[],
                );
                execution.tool_policy_score = tool_score;
                execution.tool_policy_pass = tool_pass;
                execution.provenance = result.citations;
                execution
            } else {
                let attempt = controlled_cloud_live_cascade(arm, case, &base);
                if let Some(branch) = attempt.execution {
                    branch
                } else {
                    let result = floor();
                    let mut fallback = plain_execution(
                        result.summary,
                        (result.status == "error").then(|| "product_floor_error".to_string()),
                        attempt.prompt_sha256,
                        attempt.input_chars,
                        "live_current_cloud_cascade_then_isolated_floor",
                    );
                    let (tool_score, tool_pass) = tool_policy_score(
                        &case.expected.required_tools,
                        case.expected.allowed_tools.as_deref(),
                        &attempt.calls,
                    );
                    fallback.branch_converged = Some(false);
                    fallback.tool_steps = attempt.calls;
                    fallback.tool_policy_score = tool_score;
                    fallback.tool_policy_pass = tool_pass;
                    fallback.provenance = result.citations;
                    fallback
                }
            }
        }
        Surface::LiveBullets => {
            let user = crate::prompts::live_bullets_user(&case.previous_bullets, &case.transcript);
            let system = crate::prompts::LIVE_BULLETS_SYSTEM;
            let prompt_sha256 = prompt_hash(&[system, &user]);
            let input_chars = system.chars().count() + user.chars().count();
            let accepted = crate::transcribe::bullets::update_bullets(
                arm.live_reasoner.as_ref(),
                &case.previous_bullets,
                &case.transcript,
            )
            .unwrap_or_default();
            let surface_output =
                crate::transcribe::bullets::append_bullets(&case.previous_bullets, &accepted);
            let mut execution = plain_execution(
                accepted,
                None,
                prompt_sha256,
                input_chars,
                if arm.cloud_reference {
                    "live_bullets_counterfactual_cloud_ceiling"
                } else {
                    "live_bullets_local_update_parse_append"
                },
            );
            if execution.output.trim().is_empty() {
                execution.error = Some("no_accepted_bullets".to_string());
            }
            execution.surface_output = Some(surface_output);
            execution.state_application_pass = execution.surface_output.as_ref().map(|surface| {
                surface.contains(case.previous_bullets.trim())
                    && execution
                        .output
                        .lines()
                        .all(|line| line.trim().is_empty() || surface.contains(line.trim()))
            });
            execution.comparison_scope = comparison_scope_for(arm.cloud_reference, case);
            execution
        }
        Surface::LightExtraction => {
            let canonical = serde_json::json!({
                "title": &case.title_hint,
                "note": &case.transcript,
                "entities": &case.vault_titles,
                "language": case.language.as_str(),
                "profile": "fully_local_post_call_extraction",
            })
            .to_string();
            let reasoner = CapturingStructuredReasoner::new(Arc::clone(&arm.extraction_reasoner));
            let entities = case
                .vault_titles
                .iter()
                .enumerate()
                .map(|(index, name)| (format!("synthetic-entity-{index}"), name.clone()))
                .collect::<Vec<_>>();
            let candidates = crate::facts::extract_fact_candidates(
                &reasoner,
                &case.title_hint,
                &case.transcript,
                &entities,
                case.language.as_str(),
                GenOptions::default(),
            );
            let observation = reasoner.take_observation();
            let mut facts = candidates
                .into_iter()
                .map(|candidate| StructuredFact {
                    entity: candidate.subject,
                    predicate: candidate.predicate,
                    object: candidate.object,
                })
                .collect::<Vec<_>>();
            facts.sort();
            let output = serde_json::to_string(&serde_json::json!({ "facts": facts }))
                .expect("serialize structured fact projection");
            let raw_model_output = observation
                .as_ref()
                .and_then(|observation| observation.raw_text.clone());
            let envelope_pass = raw_model_output
                .as_deref()
                .is_some_and(structured_envelope_pass);
            let error = if observation.is_none() {
                Some("structured_observation_missing".to_string())
            } else if raw_model_output.is_none() {
                Some("structured_raw_envelope_missing".to_string())
            } else if !envelope_pass {
                Some("structured_envelope_leak".to_string())
            } else if parsed_structured_facts(&output).is_none_or(|facts| facts.is_empty()) {
                Some("structured_extraction_empty".to_string())
            } else {
                None
            };
            let mut execution = plain_execution(
                output,
                error,
                prompt_hash(&[&canonical]),
                canonical.chars().count(),
                "fully_local_post_call_fact_extraction_structured_projection",
            );
            execution.raw_model_output = raw_model_output;
            execution.raw_model_format_pass = Some(envelope_pass);
            execution
        }
    }
}

fn quality_dimensions(
    case: &QualityCase,
    execution: &Execution,
    score: &OracleScore,
) -> QualityDimensions {
    let retrieval_quality = if case.surface == Surface::AskVault {
        // The evaluator injects a fixed synthetic corpus/tool result. It measures whether the
        // product stack uses that result correctly, not whether Murmur's retriever found it.
        DimensionVerdict::NotMeasured
    } else {
        DimensionVerdict::NotApplicable
    };
    let tool_agent_execution = match execution.branch_converged {
        Some(true) if execution.tool_policy_pass.unwrap_or(true) => DimensionVerdict::Pass,
        Some(_) => DimensionVerdict::Fail,
        None => DimensionVerdict::NotApplicable,
    };
    let final_output_pass = execution.error.is_none()
        && score.required_groups_hit == score.required_groups_total
        && score.format_pass
        && score.section_pass
        && score.language_pass
        && score.forbidden_pass
        && score.constraint_pass
        && score.provenance_pass
        && score.relation_pass
        && score.state_application_pass
        && score.closed_world_pass
        && score.structured_labels_pass;
    QualityDimensions {
        retrieval_quality,
        tool_agent_execution,
        final_product_output_contract: if final_output_pass {
            DimensionVerdict::Pass
        } else {
            DimensionVerdict::Fail
        },
    }
}

fn dimension_aggregate<'a>(
    verdicts: impl Iterator<Item = &'a DimensionVerdict>,
) -> DimensionAggregate {
    let verdicts = verdicts.copied().collect::<Vec<_>>();
    let measured_observations = verdicts
        .iter()
        .filter(|verdict| matches!(verdict, DimensionVerdict::Pass | DimensionVerdict::Fail))
        .count();
    let passed_observations = verdicts
        .iter()
        .filter(|verdict| **verdict == DimensionVerdict::Pass)
        .count();
    let failed_observations = measured_observations.saturating_sub(passed_observations);
    let not_measured_observations = verdicts
        .iter()
        .filter(|verdict| **verdict == DimensionVerdict::NotMeasured)
        .count();
    let not_applicable_observations = verdicts
        .iter()
        .filter(|verdict| **verdict == DimensionVerdict::NotApplicable)
        .count();
    let applicable_observations = measured_observations + not_measured_observations;
    DimensionAggregate {
        observations: verdicts.len(),
        applicable_observations,
        measured_observations,
        passed_observations,
        failed_observations,
        not_measured_observations,
        not_applicable_observations,
        coverage_rate: (applicable_observations > 0).then(|| {
            (measured_observations as f64 / applicable_observations as f64 * 1000.0).round() / 10.0
        }),
        pass_rate: (measured_observations > 0).then(|| {
            (passed_observations as f64 / measured_observations as f64 * 1000.0).round() / 10.0
        }),
    }
}

fn combine_dimension_aggregates<'a>(
    values: impl Iterator<Item = &'a DimensionAggregate>,
) -> DimensionAggregate {
    let values = values.collect::<Vec<_>>();
    let observations = values.iter().map(|value| value.observations).sum();
    let applicable_observations = values
        .iter()
        .map(|value| value.applicable_observations)
        .sum();
    let measured_observations = values.iter().map(|value| value.measured_observations).sum();
    let passed_observations = values.iter().map(|value| value.passed_observations).sum();
    let failed_observations = values.iter().map(|value| value.failed_observations).sum();
    let not_measured_observations = values
        .iter()
        .map(|value| value.not_measured_observations)
        .sum();
    let not_applicable_observations = values
        .iter()
        .map(|value| value.not_applicable_observations)
        .sum();
    DimensionAggregate {
        observations,
        applicable_observations,
        measured_observations,
        passed_observations,
        failed_observations,
        not_measured_observations,
        not_applicable_observations,
        coverage_rate: (applicable_observations > 0).then(|| {
            (measured_observations as f64 / applicable_observations as f64 * 1000.0).round() / 10.0
        }),
        pass_rate: (measured_observations > 0).then(|| {
            (passed_observations as f64 / measured_observations as f64 * 1000.0).round() / 10.0
        }),
    }
}

fn aggregate(results: &[CaseResult]) -> BTreeMap<String, Aggregate> {
    #[derive(Default)]
    struct Acc {
        cases: usize,
        call_successes: usize,
        case_passes: usize,
        diagnostic: f64,
        critical_failure_cases: usize,
        tool_sum: f64,
        tool_cases: usize,
        duration_ms: u64,
        retrieval_quality: Vec<DimensionVerdict>,
        tool_agent_execution: Vec<DimensionVerdict>,
        final_product_output_contract: Vec<DimensionVerdict>,
    }

    let mut grouped: BTreeMap<String, Acc> = BTreeMap::new();
    for result in results {
        let keys = if result.comparison_scope == ComparisonScope::OfflineReferenceCeiling {
            vec![format!(
                "reference_ceiling:surface:{}",
                result.surface.as_str()
            )]
        } else {
            vec![
                format!("surface:{}", result.surface.as_str()),
                format!("language:{}", result.language.as_str()),
                format!(
                    "cohort:{}",
                    if result.holdout {
                        "holdout"
                    } else {
                        "calibration"
                    }
                ),
                "all_eligible".to_string(),
            ]
        };
        for key in keys {
            let acc = grouped.entry(key).or_default();
            acc.cases += 1;
            acc.call_successes += usize::from(result.error.is_none());
            acc.case_passes += usize::from(result.score.case_pass);
            acc.diagnostic += result.score.diagnostic_score;
            acc.critical_failure_cases += usize::from(result.score.critical_failure);
            acc.duration_ms = acc.duration_ms.saturating_add(result.duration_ms);
            acc.retrieval_quality
                .push(result.dimensions.retrieval_quality);
            acc.tool_agent_execution
                .push(result.dimensions.tool_agent_execution);
            acc.final_product_output_contract
                .push(result.dimensions.final_product_output_contract);
            if let Some(tool) = result.tool_policy_score {
                acc.tool_sum += tool;
                acc.tool_cases += 1;
            }
        }
    }
    let mut aggregates: BTreeMap<String, Aggregate> = grouped
        .into_iter()
        .map(|(key, acc)| {
            (
                key,
                Aggregate {
                    cases: acc.cases,
                    call_success_rate: if acc.cases == 0 {
                        0.0
                    } else {
                        (acc.call_successes as f64 / acc.cases as f64 * 1000.0).round() / 10.0
                    },
                    case_pass_rate: if acc.cases == 0 {
                        0.0
                    } else {
                        (acc.case_passes as f64 / acc.cases as f64 * 1000.0).round() / 10.0
                    },
                    critical_failure_cases: acc.critical_failure_cases,
                    diagnostic_score_mean: if acc.cases == 0 {
                        0.0
                    } else {
                        (acc.diagnostic / acc.cases as f64 * 10.0).round() / 10.0
                    },
                    tool_policy_mean: (acc.tool_cases > 0)
                        .then(|| (acc.tool_sum / acc.tool_cases as f64 * 10.0).round() / 10.0),
                    mean_duration_ms: if acc.cases == 0 {
                        0
                    } else {
                        acc.duration_ms / acc.cases as u64
                    },
                    retrieval_quality: dimension_aggregate(acc.retrieval_quality.iter()),
                    tool_agent_execution: dimension_aggregate(acc.tool_agent_execution.iter()),
                    final_product_output_contract: dimension_aggregate(
                        acc.final_product_output_contract.iter(),
                    ),
                },
            )
        })
        .collect();
    let surfaces = aggregates
        .iter()
        .filter(|(key, _)| key.starts_with("surface:"))
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    if !surfaces.is_empty() {
        let count = surfaces.len();
        let mean = |values: Vec<f64>| {
            (values.into_iter().sum::<f64>() / count as f64 * 10.0).round() / 10.0
        };
        let tool_values = surfaces
            .iter()
            .filter_map(|surface| surface.tool_policy_mean)
            .collect::<Vec<_>>();
        aggregates.insert(
            "macro_surface".to_string(),
            Aggregate {
                cases: count,
                call_success_rate: mean(
                    surfaces
                        .iter()
                        .map(|surface| surface.call_success_rate)
                        .collect(),
                ),
                case_pass_rate: mean(
                    surfaces
                        .iter()
                        .map(|surface| surface.case_pass_rate)
                        .collect(),
                ),
                critical_failure_cases: surfaces
                    .iter()
                    .map(|surface| surface.critical_failure_cases)
                    .sum(),
                diagnostic_score_mean: mean(
                    surfaces
                        .iter()
                        .map(|surface| surface.diagnostic_score_mean)
                        .collect(),
                ),
                tool_policy_mean: (!tool_values.is_empty()).then(|| {
                    (tool_values.iter().sum::<f64>() / tool_values.len() as f64 * 10.0).round()
                        / 10.0
                }),
                mean_duration_ms: surfaces
                    .iter()
                    .map(|surface| surface.mean_duration_ms)
                    .sum::<u64>()
                    / count as u64,
                retrieval_quality: combine_dimension_aggregates(
                    surfaces.iter().map(|surface| &surface.retrieval_quality),
                ),
                tool_agent_execution: combine_dimension_aggregates(
                    surfaces.iter().map(|surface| &surface.tool_agent_execution),
                ),
                final_product_output_contract: combine_dimension_aggregates(
                    surfaces
                        .iter()
                        .map(|surface| &surface.final_product_output_contract),
                ),
            },
        );
    }
    aggregates
}

fn model_only_case_record_sha256(case: &ModelOnlyCaseResult) -> String {
    canonical_json_hash(&serde_json::json!([
        &case.case_id,
        &case.case_payload_sha256,
        case.surface,
        case.language,
        case.model_class,
        case.holdout,
        &case.arm_id,
        &case.model_requested,
        &case.system_sha256,
        &case.user_sha256,
        &case.envelope_sha256,
        case.system_bytes,
        case.user_bytes,
        case.system_chars,
        case.user_chars,
        case.projection,
        case.output_contract,
        case.opaque_substitution_count,
        &case.opaque_substitutions_sha256,
        case.call_count,
        case.raw_output_chars,
        &case.raw_output_sha256,
        case.output_chars,
        &case.output_sha256,
        &case.output,
        &case.provenance,
        &case.provenance_sha256,
        case.state_application_pass,
        case.duration_ms,
        &case.error,
        case.egress_receipt_start_ordinal,
        case.egress_receipt_end_ordinal,
        case.egress_receipt_count,
        &case.egress_receipt_sha256,
        case.redactions_email,
        case.redactions_card,
        case.redactions_phone,
        case.redactions_name,
        serde_json::to_value(&case.score).expect("serialize model-only score"),
    ]))
}

async fn execute_model_only_case(arm: &ArmRuntime, case: &QualityCase) -> ModelOnlyCaseResult {
    let envelope = build_same_envelope(case);
    let egress_cursor = arm.egress_sink.as_ref().map_or(0, |sink| sink.cursor());
    let started = Instant::now();
    // Load-bearing measurement boundary: exactly ONE call through the same provider trait method
    // for every candidate. No summarize adapter, options, retry, tool loop, cascade, or structured
    // provider API is reachable from this lane.
    let call = arm
        .model_only_provider
        .complete_with_meta(&envelope.system, &envelope.user)
        .await;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (raw, call_error) = match call {
        Ok((raw, _meta)) => (raw, None),
        Err(error) => (
            String::new(),
            Some(format!("model_call_failed:{}", error_kind(&error))),
        ),
    };
    let transport_failed = call_error.is_some();
    let (output, provenance, state_application_pass, projection_error) = if call_error.is_none() {
        project_model_only_output(&raw, &envelope, case)
    } else {
        (String::new(), Vec::new(), None, None)
    };
    let error = call_error.or(projection_error);
    let score = score_output(
        &output,
        &provenance,
        error.as_deref(),
        None,
        state_application_pass,
        None,
        case,
    );
    let egress_rows = arm
        .egress_sink
        .as_ref()
        .map(|sink| sink.rows_since(egress_cursor))
        .unwrap_or_default();
    if arm.cloud_reference {
        assert_eq!(
            egress_rows.len(),
            1,
            "same-envelope Sol case must produce exactly one durable complete receipt"
        );
        assert!(egress_rows.iter().all(|row| {
            row.provider_id == crate::summarize::PROVIDER_CODEX_CLI
                && row.model_requested == CODEX_MODEL
                && row.call_kind
                    == if transport_failed {
                        "complete_error"
                    } else {
                        "complete"
                    }
        }));
        assert!(egress_rows.iter().all(|row| {
            row.redactions_email == 0
                && row.redactions_card == 0
                && row.redactions_phone == 0
                && row.redactions_name == 0
        }), "same-envelope equality requires zero cloud redactions after candidate-independent canonical pre-scrub");
    } else {
        assert!(
            egress_rows.is_empty(),
            "same-envelope local cases must not create cloud egress receipts"
        );
    }
    let receipts_json =
        serde_json::to_string(&egress_rows).expect("serialize model-only egress receipts");
    let substitutions_json = serde_json::to_string(&envelope.substitutions)
        .expect("serialize opaque model-only substitutions");
    let redactions_email = egress_rows.iter().map(|row| row.redactions_email).sum();
    let redactions_card = egress_rows.iter().map(|row| row.redactions_card).sum();
    let redactions_phone = egress_rows.iter().map(|row| row.redactions_phone).sum();
    let redactions_name = egress_rows.iter().map(|row| row.redactions_name).sum();
    let mut result = ModelOnlyCaseResult {
        case_id: case.id.clone(),
        case_payload_sha256: case_payload_sha256(case),
        surface: case.surface,
        language: case.language,
        model_class: case.model_class,
        holdout: case.holdout,
        arm_id: arm.metadata.arm_id.clone(),
        model_requested: arm.metadata.model_requested.clone(),
        system_sha256: envelope.system_sha256,
        user_sha256: envelope.user_sha256,
        envelope_sha256: envelope.envelope_sha256,
        system_bytes: envelope.system.len(),
        user_bytes: envelope.user.len(),
        system_chars: envelope.system.chars().count(),
        user_chars: envelope.user.chars().count(),
        projection: envelope.projection,
        output_contract: envelope.output_contract.to_string(),
        opaque_substitution_count: envelope.substitutions.len(),
        opaque_substitutions_sha256: prompt_hash(&[&substitutions_json]),
        call_count: 1,
        raw_output_chars: raw.chars().count(),
        raw_output_sha256: prompt_hash(&[&raw]),
        output_chars: output.chars().count(),
        output_sha256: prompt_hash(&[&output]),
        output,
        provenance_sha256: string_sequence_hash(&provenance),
        provenance,
        state_application_pass,
        duration_ms,
        error,
        egress_receipt_start_ordinal: egress_rows.first().map(|row| row.ordinal),
        egress_receipt_end_ordinal: egress_rows.last().map(|row| row.ordinal),
        egress_receipt_count: egress_rows.len() as u64,
        egress_receipt_sha256: prompt_hash(&[&receipts_json]),
        redactions_email,
        redactions_card,
        redactions_phone,
        redactions_name,
        score,
        case_record_sha256: String::new(),
    };
    result.case_record_sha256 = model_only_case_record_sha256(&result);
    eprintln!(
        "MURMUR_QUALITY_MODEL_ONLY arm={} case={} pass={} diagnostic={:.1} duration_ms={} error={}",
        result.arm_id,
        result.case_id,
        result.score.case_pass,
        result.score.diagnostic_score,
        result.duration_ms,
        result.error.as_deref().unwrap_or("none")
    );
    result
}

fn model_only_aggregates(results: &[ModelOnlyCaseResult]) -> BTreeMap<String, CompositeAggregate> {
    let mut grouped: BTreeMap<String, Vec<&ModelOnlyCaseResult>> = BTreeMap::new();
    for case in results {
        for key in [
            "all_eligible".to_string(),
            format!("surface:{}", case.surface.as_str()),
            format!("language:{}", case.language.as_str()),
            format!(
                "cohort:{}",
                if case.holdout {
                    "holdout"
                } else {
                    "calibration"
                }
            ),
        ] {
            grouped.entry(key).or_default().push(case);
        }
    }
    grouped
        .into_iter()
        .map(|(key, cases)| {
            let count = cases.len();
            let call_successes = cases.iter().filter(|case| case.error.is_none()).count();
            let passes = cases.iter().filter(|case| case.score.case_pass).count();
            let mut by_surface: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
            for case in &cases {
                let entry = by_surface.entry(case.surface.as_str()).or_default();
                entry.1 += 1;
                entry.0 += usize::from(case.score.case_pass);
            }
            let surface_macro_pass_rate = (by_surface
                .values()
                .map(|(surface_passes, total)| *surface_passes as f64 / *total as f64)
                .sum::<f64>()
                / by_surface.len() as f64
                * 1000.0)
                .round()
                / 10.0;
            (
                key,
                CompositeAggregate {
                    cases: count,
                    call_success_rate: (call_successes as f64 / count as f64 * 1000.0).round()
                        / 10.0,
                    case_pass_rate: (passes as f64 / count as f64 * 1000.0).round() / 10.0,
                    surface_macro_pass_rate,
                    critical_failure_cases: cases
                        .iter()
                        .filter(|case| case.score.critical_failure)
                        .count(),
                    diagnostic_score_mean: (cases
                        .iter()
                        .map(|case| case.score.diagnostic_score)
                        .sum::<f64>()
                        / count as f64
                        * 10.0)
                        .round()
                        / 10.0,
                },
            )
        })
        .collect()
}

async fn run_arm(arm: ArmRuntime, manifest: &QualityManifest) -> (ArmReport, ModelOnlyArmReport) {
    let mut results = Vec::new();
    let mut model_only_results = Vec::new();
    for case in manifest
        .cases
        .iter()
        .filter(|case| arm_accepts(&arm.metadata.arm_id, case.model_class))
    {
        model_only_results.push(execute_model_only_case(&arm, case).await);
        let egress_cursor = arm.egress_sink.as_ref().map_or(0, |sink| sink.cursor());
        let started = Instant::now();
        let execution = execute_case(&arm, case).await;
        let score = score_output(
            &execution.output,
            &execution.provenance,
            execution.error.as_deref(),
            execution.tool_policy_pass,
            execution.state_application_pass,
            execution.branch_converged,
            case,
        );
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        eprintln!(
            "MURMUR_QUALITY_CASE arm={} case={} pass={} diagnostic={:.1} duration_ms={} error={}",
            arm.metadata.arm_id,
            case.id,
            score.case_pass,
            score.diagnostic_score,
            duration_ms,
            execution.error.as_deref().unwrap_or("none")
        );
        let raw_model_output_sha256 = execution
            .raw_model_output
            .as_deref()
            .map(|raw| prompt_hash(&[raw]));
        let surface_output_sha256 = execution
            .surface_output
            .as_deref()
            .map(|surface| prompt_hash(&[surface]));
        let provenance_sha256 = string_sequence_hash(&execution.provenance);
        let tool_steps_sha256 = string_sequence_hash(&execution.tool_steps);
        let dimensions = quality_dimensions(case, &execution, &score);
        let structured_case = case.surface == Surface::LightExtraction;
        let structured_schema_pass =
            structured_case.then(|| output_format_pass(&execution.output, &case.expected));
        let structured_labels_receipt = structured_case.then_some(score.structured_labels_pass);
        let structured_envelope_pass = structured_case.then_some(
            execution.raw_model_format_pass == Some(true)
                && execution.raw_model_output.as_deref().is_some(),
        );
        let egress_rows = arm
            .egress_sink
            .as_ref()
            .map(|sink| sink.rows_since(egress_cursor))
            .unwrap_or_default();
        if arm.cloud_reference {
            assert!(
                !egress_rows.is_empty(),
                "every Sol benchmark case must persist at least one content-free egress receipt"
            );
            assert!(egress_rows.iter().all(|row| {
                row.provider_id == crate::summarize::PROVIDER_CODEX_CLI
                    && row.model_requested == CODEX_MODEL
            }));
        } else {
            assert!(
                egress_rows.is_empty(),
                "local benchmark cases must never create cloud egress receipts"
            );
        }
        let egress_receipt_start_ordinal = egress_rows.first().map(|row| row.ordinal);
        let egress_receipt_end_ordinal = egress_rows.last().map(|row| row.ordinal);
        let egress_receipt_count = egress_rows.len() as u64;
        let egress_receipt_json =
            serde_json::to_string(&egress_rows).expect("serialize per-case egress receipts");
        let mut result = CaseResult {
            case_id: case.id.clone(),
            case_payload_sha256: case_payload_sha256(case),
            surface: case.surface,
            language: case.language,
            model_class: case.model_class,
            holdout: case.holdout,
            route_input_sha256: execution.prompt_sha256,
            generation_profile: generation_profile(&arm, case),
            product_route: execution.product_route,
            comparison_scope: execution.comparison_scope,
            route_input_chars: execution.input_chars,
            output_chars: execution.output.chars().count(),
            output_sha256: prompt_hash(&[&execution.output]),
            duration_ms,
            output: execution.output,
            surface_output: execution.surface_output,
            surface_output_sha256,
            raw_model_output: execution.raw_model_output,
            raw_model_output_sha256,
            raw_model_format_pass: execution.raw_model_format_pass,
            structured_schema_pass,
            structured_labels_pass: structured_labels_receipt,
            structured_envelope_pass,
            error: execution.error,
            tool_steps: execution.tool_steps,
            tool_policy_score: execution.tool_policy_score,
            tool_policy_pass: execution.tool_policy_pass,
            state_application_pass: execution.state_application_pass,
            branch_converged: execution.branch_converged,
            provenance: execution.provenance,
            provenance_sha256,
            tool_steps_sha256,
            egress_receipt_start_ordinal,
            egress_receipt_end_ordinal,
            egress_receipt_count,
            egress_receipt_sha256: prompt_hash(&[&egress_receipt_json]),
            dimensions,
            score,
            case_record_sha256: String::new(),
        };
        result.case_record_sha256 = case_record_sha256(&result);
        results.push(result);
    }
    let aggregates = aggregate(&results);
    let model_only_aggregates = model_only_aggregates(&model_only_results);
    (
        ArmReport {
            metadata: arm.metadata.clone(),
            aggregates,
            cases: results,
        },
        ModelOnlyArmReport {
            arm_id: arm.metadata.arm_id,
            model_requested: arm.metadata.model_requested,
            aggregates: model_only_aggregates,
            cases: model_only_results,
        },
    )
}

fn assert_runtime_artifacts_unchanged(arms: &[ArmReport]) {
    for arm in arms {
        let model_path = match arm.metadata.arm_id.as_str() {
            QWEN4_ID => std::env::var_os("MURMUR_QUALITY_QWEN4").map(PathBuf::from),
            QWEN1_ID => std::env::var_os("MURMUR_QUALITY_QWEN1").map(PathBuf::from),
            _ => None,
        };
        if let Some(path) = model_path {
            assert_eq!(
                sha256_file(&path).ok(),
                arm.metadata.model_sha256,
                "quality model changed while the run was active"
            );
            assert_eq!(
                std::fs::metadata(&path).ok().map(|metadata| metadata.len()),
                arm.metadata.model_bytes,
                "quality model size changed while the run was active"
            );
        }
        let runtime_path = if arm.metadata.arm_id == SOL_ID {
            PathBuf::from("/opt/homebrew/bin/codex")
        } else {
            PathBuf::from(
                std::env::var_os("MURMUR_BRAIN_SIDECAR")
                    .filter(|value| !value.is_empty())
                    .expect("MURMUR_BRAIN_SIDECAR disappeared while the run was active"),
            )
        };
        assert_eq!(
            sha256_file(&runtime_path).ok(),
            arm.metadata.runtime_sha256,
            "quality runtime changed while the run was active"
        );
    }
}

fn paired_aggregate(
    local_arm: &str,
    reference_arm: &str,
    comparison_kind: PairComparisonKind,
    cohort: &str,
    cases: &[&PairedCaseComparison],
) -> PairedAggregate {
    let matched = cases.len();
    let local_passes = cases.iter().filter(|case| case.local_case_pass).count();
    let reference_passes = cases.iter().filter(|case| case.reference_case_pass).count();
    let local_calls = cases.iter().filter(|case| case.local_call_success).count();
    let reference_calls = cases
        .iter()
        .filter(|case| case.reference_call_success)
        .count();
    let local_critical = cases
        .iter()
        .filter(|case| case.local_critical_failure)
        .count();
    let reference_critical = cases
        .iter()
        .filter(|case| case.reference_critical_failure)
        .count();
    let pass_rate = |passes: usize| {
        if matched == 0 {
            0.0
        } else {
            (passes as f64 / matched as f64 * 1000.0).round() / 10.0
        }
    };
    let surface_macro = |local: bool| {
        let mut by_surface: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for case in cases {
            let entry = by_surface.entry(case.surface.as_str()).or_default();
            entry.1 += 1;
            entry.0 += usize::from(if local {
                case.local_case_pass
            } else {
                case.reference_case_pass
            });
        }
        if by_surface.is_empty() {
            0.0
        } else {
            (by_surface
                .values()
                .map(|(passes, total)| *passes as f64 / *total as f64)
                .sum::<f64>()
                / by_surface.len() as f64
                * 1000.0)
                .round()
                / 10.0
        }
    };
    PairedAggregate {
        local_arm: local_arm.to_string(),
        reference_arm: reference_arm.to_string(),
        comparison_kind,
        cohort: cohort.to_string(),
        matched_cases: matched,
        local_case_pass_rate: pass_rate(local_passes),
        reference_case_pass_rate: pass_rate(reference_passes),
        local_call_success_rate: pass_rate(local_calls),
        reference_call_success_rate: pass_rate(reference_calls),
        local_surface_macro_pass_rate: surface_macro(true),
        reference_surface_macro_pass_rate: surface_macro(false),
        local_critical_failure_cases: local_critical,
        reference_critical_failure_cases: reference_critical,
        reference_minus_local_mean: if matched == 0 {
            0.0
        } else {
            (cases
                .iter()
                .map(|case| case.reference_minus_local)
                .sum::<f64>()
                / matched as f64
                * 10.0)
                .round()
                / 10.0
        },
    }
}

fn pair_comparison_kind(_surface: Surface) -> PairComparisonKind {
    // Every enumerated product-backend route crosses backend-specific adapters (Codex prompt
    // rendering/redaction vs Qwen chat templates/options); some also differ by retry, structured
    // decode, or agent loop.
    // The narrower model-stack comparison lives in the separate same-caller-envelope lane.
    PairComparisonKind::RouteSpecificProductSystem
}

fn paired_comparison(arms: &[ArmReport]) -> PairedComparison {
    let Some(reference) = arms.iter().find(|arm| arm.metadata.arm_id == SOL_ID) else {
        return PairedComparison {
            cases: Vec::new(),
            aggregates: Vec::new(),
        };
    };
    let mut cases = Vec::new();
    for local_id in [QWEN4_ID, QWEN1_ID] {
        let Some(local) = arms.iter().find(|arm| arm.metadata.arm_id == local_id) else {
            continue;
        };
        for local_case in &local.cases {
            if local_case.comparison_scope != ComparisonScope::ProductPath {
                continue;
            }
            let Some(reference_case) = reference.cases.iter().find(|candidate| {
                candidate.case_id == local_case.case_id
                    && candidate.comparison_scope == ComparisonScope::ProductPath
            }) else {
                continue;
            };
            assert_eq!(
                local_case.case_payload_sha256, reference_case.case_payload_sha256,
                "paired product-system comparison requires the same fixture payload commitment"
            );
            let comparison_kind = pair_comparison_kind(local_case.surface);
            cases.push(PairedCaseComparison {
                case_id: local_case.case_id.clone(),
                case_payload_sha256: local_case.case_payload_sha256.clone(),
                surface: local_case.surface,
                comparison_kind,
                local_arm: local_id.to_string(),
                reference_arm: SOL_ID.to_string(),
                holdout: local_case.holdout,
                comparison_scope: ComparisonScope::ProductPath,
                local_route_input_sha256: local_case.route_input_sha256.clone(),
                reference_route_input_sha256: reference_case.route_input_sha256.clone(),
                local_generation_profile: local_case.generation_profile.clone(),
                reference_generation_profile: reference_case.generation_profile.clone(),
                local_case_pass: local_case.score.case_pass,
                reference_case_pass: reference_case.score.case_pass,
                local_call_success: local_case.error.is_none(),
                reference_call_success: reference_case.error.is_none(),
                local_critical_failure: local_case.score.critical_failure,
                reference_critical_failure: reference_case.score.critical_failure,
                local_diagnostic_score: local_case.score.diagnostic_score,
                reference_diagnostic_score: reference_case.score.diagnostic_score,
                reference_minus_local: ((reference_case.score.diagnostic_score
                    - local_case.score.diagnostic_score)
                    * 10.0)
                    .round()
                    / 10.0,
            });
        }
    }
    let mut aggregates = Vec::new();
    for comparison_kind in [PairComparisonKind::RouteSpecificProductSystem] {
        for local_id in [QWEN4_ID, QWEN1_ID] {
            for cohort in ["all", "calibration", "holdout"] {
                let subset = cases
                    .iter()
                    .filter(|case| {
                        case.local_arm == local_id
                            && case.comparison_kind == comparison_kind
                            && match cohort {
                                "calibration" => !case.holdout,
                                "holdout" => case.holdout,
                                _ => true,
                            }
                    })
                    .collect::<Vec<_>>();
                if !subset.is_empty() {
                    aggregates.push(paired_aggregate(
                        local_id,
                        SOL_ID,
                        comparison_kind,
                        cohort,
                        &subset,
                    ));
                }
            }
        }
        for cohort in ["all", "calibration", "holdout"] {
            let subset = cases
                .iter()
                .filter(|case| {
                    case.comparison_kind == comparison_kind
                        && match cohort {
                            "calibration" => !case.holdout,
                            "holdout" => case.holdout,
                            _ => true,
                        }
                })
                .collect::<Vec<_>>();
            if !subset.is_empty() {
                aggregates.push(paired_aggregate(
                    "qwen-local-composite",
                    SOL_ID,
                    comparison_kind,
                    cohort,
                    &subset,
                ));
            }
        }
    }
    PairedComparison { cases, aggregates }
}

fn model_only_paired_aggregate(
    local_arm: &str,
    reference_arm: &str,
    cohort: &str,
    cases: &[&ModelOnlyPair],
) -> ModelOnlyPairedAggregate {
    let matched = cases.len();
    let rate = |count: usize| {
        if matched == 0 {
            0.0
        } else {
            (count as f64 / matched as f64 * 1000.0).round() / 10.0
        }
    };
    let surface_macro = |local: bool| {
        let mut by_surface: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for case in cases {
            let entry = by_surface.entry(case.surface.as_str()).or_default();
            entry.1 += 1;
            entry.0 += usize::from(if local {
                case.local_case_pass
            } else {
                case.reference_case_pass
            });
        }
        if by_surface.is_empty() {
            0.0
        } else {
            (by_surface
                .values()
                .map(|(passes, total)| *passes as f64 / *total as f64)
                .sum::<f64>()
                / by_surface.len() as f64
                * 1000.0)
                .round()
                / 10.0
        }
    };
    ModelOnlyPairedAggregate {
        local_arm: local_arm.to_string(),
        reference_arm: reference_arm.to_string(),
        cohort: cohort.to_string(),
        matched_cases: matched,
        local_case_pass_rate: rate(cases.iter().filter(|case| case.local_case_pass).count()),
        reference_case_pass_rate: rate(
            cases.iter().filter(|case| case.reference_case_pass).count(),
        ),
        local_call_success_rate: rate(cases.iter().filter(|case| case.local_call_success).count()),
        reference_call_success_rate: rate(
            cases
                .iter()
                .filter(|case| case.reference_call_success)
                .count(),
        ),
        local_surface_macro_pass_rate: surface_macro(true),
        reference_surface_macro_pass_rate: surface_macro(false),
        local_critical_failure_cases: cases
            .iter()
            .filter(|case| case.local_critical_failure)
            .count(),
        reference_critical_failure_cases: cases
            .iter()
            .filter(|case| case.reference_critical_failure)
            .count(),
        reference_minus_local_mean: if matched == 0 {
            0.0
        } else {
            (cases
                .iter()
                .map(|case| case.reference_minus_local)
                .sum::<f64>()
                / matched as f64
                * 10.0)
                .round()
                / 10.0
        },
    }
}

fn same_envelope_model_only(arms: Vec<ModelOnlyArmReport>) -> SameEnvelopeModelOnlyReport {
    let mut pairs = Vec::new();
    if let Some(reference) = arms.iter().find(|arm| arm.arm_id == SOL_ID) {
        for local_id in [QWEN4_ID, QWEN1_ID] {
            let Some(local) = arms.iter().find(|arm| arm.arm_id == local_id) else {
                continue;
            };
            for local_case in &local.cases {
                let Some(reference_case) = reference
                    .cases
                    .iter()
                    .find(|candidate| candidate.case_id == local_case.case_id)
                else {
                    continue;
                };
                assert_eq!(
                    local_case.case_payload_sha256, reference_case.case_payload_sha256,
                    "same-envelope pair requires the same candidate-independent fixture payload"
                );
                assert_eq!(
                    local_case.envelope_sha256, reference_case.envelope_sha256,
                    "same-envelope pair requires an identical evaluator-owned system/user envelope"
                );
                assert_eq!(local_case.system_sha256, reference_case.system_sha256);
                assert_eq!(local_case.user_sha256, reference_case.user_sha256);
                assert_eq!(local_case.system_bytes, reference_case.system_bytes);
                assert_eq!(local_case.user_bytes, reference_case.user_bytes);
                assert_eq!(local_case.projection, reference_case.projection);
                assert_eq!(local_case.output_contract, reference_case.output_contract);
                assert_eq!(local_case.call_count, 1);
                assert_eq!(reference_case.call_count, 1);
                pairs.push(ModelOnlyPair {
                    case_id: local_case.case_id.clone(),
                    case_payload_sha256: local_case.case_payload_sha256.clone(),
                    surface: local_case.surface,
                    holdout: local_case.holdout,
                    local_arm: local_id.to_string(),
                    reference_arm: SOL_ID.to_string(),
                    envelope_sha256: local_case.envelope_sha256.clone(),
                    local_case_pass: local_case.score.case_pass,
                    reference_case_pass: reference_case.score.case_pass,
                    local_call_success: local_case.error.is_none(),
                    reference_call_success: reference_case.error.is_none(),
                    local_critical_failure: local_case.score.critical_failure,
                    reference_critical_failure: reference_case.score.critical_failure,
                    local_diagnostic_score: local_case.score.diagnostic_score,
                    reference_diagnostic_score: reference_case.score.diagnostic_score,
                    reference_minus_local: ((reference_case.score.diagnostic_score
                        - local_case.score.diagnostic_score)
                        * 10.0)
                        .round()
                        / 10.0,
                });
            }
        }
    }
    let mut aggregates = Vec::new();
    for local_id in [QWEN4_ID, QWEN1_ID] {
        for cohort in ["all", "calibration", "holdout"] {
            let subset = pairs
                .iter()
                .filter(|case| {
                    case.local_arm == local_id
                        && match cohort {
                            "calibration" => !case.holdout,
                            "holdout" => case.holdout,
                            _ => true,
                        }
                })
                .collect::<Vec<_>>();
            if !subset.is_empty() {
                aggregates.push(model_only_paired_aggregate(
                    local_id, SOL_ID, cohort, &subset,
                ));
            }
        }
    }
    for cohort in ["all", "calibration", "holdout"] {
        let subset = pairs
            .iter()
            .filter(|case| match cohort {
                "calibration" => !case.holdout,
                "holdout" => case.holdout,
                _ => true,
            })
            .collect::<Vec<_>>();
        if !subset.is_empty() {
            aggregates.push(model_only_paired_aggregate(
                "qwen-local-composite",
                SOL_ID,
                cohort,
                &subset,
            ));
        }
    }
    SameEnvelopeModelOnlyReport {
        lane_id: "same_caller_envelope_model_stack_v3",
        entrypoint: "SummarizerProvider::complete_with_meta",
        equality_boundary: "evaluator_owned_canonical_prescrubbed_system_user_utf8",
        provider_rendered_prompts_byte_identical: false,
        effective_model_inputs_attested_identical: false,
        limitations: [
            "identical evaluator-owned caller values do not imply identical provider-rendered prompts, tokenization, hidden instructions, sampling, or effective reasoning effort",
            "candidate-independent opaque entity substitution plus the canonical regex and structural-title pre-scrub prevent asymmetric firewall changes; reversible values are restored before deterministic scoring, and this lane does not measure redactor recall",
            "the deterministic oracle measures enumerated facts and contracts, not prose style or general intelligence",
        ],
        arms,
        pairs,
        aggregates,
    }
}

fn local_composite(arms: &[ArmReport]) -> LocalComposite {
    let cases = arms
        .iter()
        .filter(|arm| matches!(arm.metadata.arm_id.as_str(), QWEN4_ID | QWEN1_ID))
        .flat_map(|arm| arm.cases.iter())
        .filter(|case| case.comparison_scope == ComparisonScope::ProductPath)
        .collect::<Vec<_>>();
    let mut aggregates = BTreeMap::new();
    for cohort in ["all", "calibration", "holdout"] {
        let subset = cases
            .iter()
            .filter(|case| match cohort {
                "calibration" => !case.holdout,
                "holdout" => case.holdout,
                _ => true,
            })
            .collect::<Vec<_>>();
        if subset.is_empty() {
            continue;
        }
        let count = subset.len();
        let passes = subset.iter().filter(|case| case.score.case_pass).count();
        let call_successes = subset.iter().filter(|case| case.error.is_none()).count();
        let mut by_surface: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for case in &subset {
            let entry = by_surface.entry(case.surface.as_str()).or_default();
            entry.1 += 1;
            entry.0 += usize::from(case.score.case_pass);
        }
        let surface_macro_pass_rate = (by_surface
            .values()
            .map(|(surface_passes, total)| *surface_passes as f64 / *total as f64)
            .sum::<f64>()
            / by_surface.len() as f64
            * 1000.0)
            .round()
            / 10.0;
        aggregates.insert(
            cohort.to_string(),
            CompositeAggregate {
                cases: count,
                call_success_rate: (call_successes as f64 / count as f64 * 1000.0).round() / 10.0,
                case_pass_rate: (passes as f64 / count as f64 * 1000.0).round() / 10.0,
                surface_macro_pass_rate,
                critical_failure_cases: subset
                    .iter()
                    .filter(|case| case.score.critical_failure)
                    .count(),
                diagnostic_score_mean: (subset
                    .iter()
                    .map(|case| case.score.diagnostic_score)
                    .sum::<f64>()
                    / count as f64
                    * 10.0)
                    .round()
                    / 10.0,
            },
        );
    }
    LocalComposite {
        arm_ids: [QWEN4_ID, QWEN1_ID],
        definition: "Qwen4 for enumerated heavy candidate backend routes plus Qwen1.7 for enumerated lightweight candidate backend routes; includes local-only live bullets; not a released-build or full-product composite",
        aggregates,
    }
}

fn requested_arms() -> Vec<String> {
    std::env::var("MURMUR_QUALITY_ARMS")
        .unwrap_or_else(|_| format!("{QWEN4_ID},{QWEN1_ID}"))
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn cloud_run_schedule_valid(arms: &[String], repetition: &str) -> bool {
    if !arms.iter().any(|arm| arm == SOL_ID) {
        return true;
    }
    let expected = match repetition {
        "1" => [QWEN4_ID, QWEN1_ID, SOL_ID],
        "2" => [SOL_ID, QWEN1_ID, QWEN4_ID],
        _ => return false,
    };
    arms == expected.map(ToString::to_string)
}

fn validate_cloud_run_schedule(arms: &[String], repetition: &str) {
    assert!(
        cloud_run_schedule_valid(arms, repetition),
        "cloud quality runs require repetition 1 or 2 and the preregistered pairwise-reversed arm order"
    );
}

fn required_path(variable: &str) -> PathBuf {
    PathBuf::from(std::env::var(variable).unwrap_or_else(|_| {
        panic!("{variable} must point at the installed synthetic benchmark model")
    }))
}

fn empty_egress_evidence() -> BenchmarkEgressEvidence {
    BenchmarkEgressEvidence {
        required: false,
        sqlite_persistence_verified: true,
        temporary_database_cleaned: true,
        attempted_rows: 0,
        persisted_rows: 0,
        persistence_failures: 0,
        content_free_rows_sha256: prompt_hash(&["[]"]),
        provider_ids: Vec::new(),
        call_kinds: Vec::new(),
        rows: Vec::new(),
    }
}

fn install_benchmark_egress_sink(arms: &[String]) -> Option<Arc<BenchmarkEgressSink>> {
    if !arms.iter().any(|arm| arm == SOL_ID) {
        return None;
    }
    Some(Arc::new(BenchmarkEgressSink::create()))
}

/// Exercise the exact consent/redaction/ledger provider construction with an in-memory transport
/// before any paid or local-model inference. This proves that the evaluator-owned, pre-scrubbed
/// system/user values arrive byte-identically at the cloud transport boundary and produce only
/// zero-redaction receipts. The captured synthetic prompts never leave memory or enter an artifact.
async fn assert_same_envelope_cloud_firewall_byte_preserving(cases: &[QualityCase]) {
    let captured = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let inner: Arc<dyn SummarizerProvider> = Arc::new(ModelOnlyPreflightCaptureProvider {
        captured: Arc::clone(&captured),
    });
    let sink = Arc::new(BenchmarkEgressSink::create());
    let config = cloud_config();
    let heavy = Arc::new(tokio::sync::Semaphore::new(1));
    let provider = crate::summarize::provider_for_with_test_egress_sink(
        Role::Ask,
        &config,
        &heavy,
        sink.clone(),
        inner,
    )
    .expect("construct canonical byte-equality preflight provider");

    for case in cases {
        let envelope = build_same_envelope(case);
        let cursor = sink.cursor();
        provider
            .complete_with_meta(&envelope.system, &envelope.user)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "same-envelope cloud firewall preflight failed for {}: {}",
                    case.id,
                    error_kind(&error)
                )
            });
        let actual = captured
            .lock()
            .expect("read preflight transport capture")
            .pop()
            .expect("preflight transport must capture exactly one call");
        assert!(
            actual.0 == envelope.system && actual.1 == envelope.user,
            "same-envelope cloud firewall changed evaluator bytes for {} (system_equal={}, user_equal={}, expected_system_sha256={}, actual_system_sha256={}, expected_user_sha256={}, actual_user_sha256={})",
            case.id,
            actual.0 == envelope.system,
            actual.1 == envelope.user,
            envelope.system_sha256,
            prompt_hash(&[&actual.0]),
            envelope.user_sha256,
            prompt_hash(&[&actual.1]),
        );
        let rows = sink.rows_since(cursor);
        assert_eq!(rows.len(), 1, "preflight receipt count for {}", case.id);
        assert!(
            rows.iter().all(|row| {
                row.redactions_email == 0
                    && row.redactions_card == 0
                    && row.redactions_phone == 0
                    && row.redactions_name == 0
            }),
            "same-envelope preflight requires a zero-redaction receipt for {}",
            case.id
        );
    }
    assert!(
        captured
            .lock()
            .expect("read final preflight capture")
            .is_empty(),
        "preflight transport capture count drifted"
    );
    drop(provider);
    let evidence = sink.evidence(true);
    assert!(evidence.sqlite_persistence_verified);
    assert!(evidence.temporary_database_cleaned);
    assert_eq!(evidence.persisted_rows as usize, cases.len());
}

/// Real Mac bake-off. Explicitly ignored because it loads multi-GB GGUFs and, for the Sol arm,
/// sends the committed synthetic fixture through Murmur's consent/redaction/ledger provider seam.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real Qwen GGUF + consented GPT-Sol quality bake-off"]
async fn run_local_cloud_generation_quality_from_env() {
    let manifest = manifest();
    assert!(
        manifest.synthetic_only,
        "real content is forbidden in this lane"
    );
    let arms = requested_arms();
    let repetition = std::env::var("MURMUR_QUALITY_REPETITION").unwrap_or_else(|_| "1".to_string());
    validate_cloud_run_schedule(&arms, &repetition);
    let arm_order = arms.clone();
    let out = required_path("MURMUR_QUALITY_OUT");
    assert!(
        !out.exists(),
        "quality output already exists; every repetition must write a fresh artifact"
    );
    if arms.iter().any(|arm| arm == SOL_ID) {
        assert_same_envelope_cloud_firewall_byte_preserving(&manifest.cases).await;
    }
    let benchmark_egress_sink = install_benchmark_egress_sink(&arms);
    let snapshot_start = run_snapshot();
    let retrieval_quality = crate::eval::generation_retrieval::build_evidence()
        .expect("build exact replayable retrieval evidence");
    // Construct and provenance-check every requested runtime before the first inference. This is
    // especially important for repetition 2, where the cloud reference intentionally runs first:
    // a missing/wrong later GGUF or sidecar must fail before any paid synthetic egress occurs.
    let runtimes = arms
        .iter()
        .map(|arm_id| {
            match arm_id.as_str() {
                QWEN4_ID => ArmRuntime::local(
                    QWEN4_ID,
                    required_path("MURMUR_QUALITY_QWEN4"),
                    ModelClass::Heavy,
                ),
                QWEN1_ID => ArmRuntime::local(
                    QWEN1_ID,
                    required_path("MURMUR_QUALITY_QWEN1"),
                    ModelClass::Light,
                ),
                SOL_ID => ArmRuntime::cloud(
                    benchmark_egress_sink
                        .as_ref()
                        .expect("Sol arm requires its explicit benchmark egress sink")
                        .clone(),
                ),
                other => panic!("unknown MURMUR_QUALITY_ARMS entry: {other}"),
            }
            .unwrap_or_else(|error| panic!("cannot construct {arm_id}: {}", error_kind(&error)))
        })
        .collect::<Vec<_>>();
    let mut reports = Vec::new();
    let mut model_only_arms = Vec::new();
    for runtime in runtimes {
        let (product_report, model_only_report) = run_arm(runtime, &manifest).await;
        reports.push(product_report);
        model_only_arms.push(model_only_report);
    }

    assert_runtime_artifacts_unchanged(&reports);
    crate::eval::generation_retrieval::assert_model_unchanged(&retrieval_quality)
        .expect("retrieval model unchanged during generation run");
    let local_composite = local_composite(&reports);
    let paired_comparison = paired_comparison(&reports);
    let same_envelope_model_only = same_envelope_model_only(model_only_arms);
    let egress_ledger = benchmark_egress_sink
        .as_ref()
        .map(|sink| sink.evidence(true))
        .unwrap_or_else(empty_egress_evidence);
    assert!(
        egress_ledger.sqlite_persistence_verified && egress_ledger.temporary_database_cleaned,
        "every benchmark cloud dispatch must round-trip through canonical SQLite and clean its private temp database"
    );
    if egress_ledger.required {
        assert_eq!(
            egress_ledger.provider_ids,
            vec![crate::summarize::PROVIDER_CODEX_CLI.to_string()],
            "benchmark ledger must contain only the explicitly requested Codex provider"
        );
        assert!(
            egress_ledger
                .rows
                .iter()
                .all(|row| row.model_requested == CODEX_MODEL),
            "every persisted benchmark row must identify the requested Sol model"
        );
    }
    let snapshot_end = run_snapshot();
    assert_eq!(
        snapshot_end, snapshot_start,
        "quality source/fixture/diff changed while real model calls were running"
    );

    let report = QualityReport {
        schema_version: manifest.schema_version,
        run_label: std::env::var("MURMUR_QUALITY_RUN")
            .unwrap_or_else(|_| "manual".to_string()),
        generated_at: chrono::Utc::now().to_rfc3339(),
        repository_commit: snapshot_start.repository_commit.clone(),
        source_fingerprint_sha256: snapshot_start.source_fingerprint_sha256.clone(),
        manifest_sha256: snapshot_start.manifest_sha256.clone(),
        prompt_version: crate::prompts::PROMPT_VERSION,
        synthetic_only: true,
        holdout_interpretation: "legacy_pre_remediation_tag_not_untouched_generalization",
        benchmark_design: "two strictly separated lanes over one synthetic fixture: (1) each enumerated candidate product-backend route is labeled route_specific_product_system because provider rendering, redaction, options, structured decode, retry, and orchestration can differ; this is source-snapshot evidence, not a released-build or full-UI claim; (2) same_caller_envelope_model_stack performs exactly one SummarizerProvider::complete_with_meta call per candidate over an identical evaluator-owned, canonical-pre-scrubbed system/user UTF-8 envelope and shared projection. An offline oracle proves those bytes survive the canonical cloud firewall unchanged before any paid dispatch. The lane remains a same-caller-envelope model-stack comparison, not an attestation of identical post-adapter effective model prompts or raw weights. Every verdict comes from the deterministic code-owned scorer; no model judges another model",
        evidence_scope: "bounded synthetic diagnostic evidence for the enumerated facts, relations, forbidden claims, output contracts, staged-tool policy, citations, and state-application invariants in this manifest; it does not establish product-wide quality or generalization",
        evidence_limits: [
            "not a general hallucination-rate, prose-style, or state-of-the-art benchmark; route-specific product-system deltas do not isolate the model; stored tool-step labels are content-free session evidence, not authenticated call transcripts",
            "synthetic cases are a small directional product diagnostic; cohort tags preserve the pre-remediation split, but after prompt or oracle repair the former holdout is no longer an untouched generalization test",
            "prose style, naturalness, and general usefulness are not scored because the evaluator contains no model-as-judge layer; a future blinded human panel is required for those claims",
        ],
        retrieval_lane: RetrievalLane {
            mode: "independent synthetic SQLite bake-off through the production visibility-gated FTS, semantic, and hybrid reader implementations in this source snapshot; answer-generation fixtures remain controlled",
            oracle: "recall@5, nDCG@5, and MRR over fixture-bound PL/EN relevance labels; real selected e5 model required",
            attribution: "retrieval is measured once as a shared pre-generation system dimension; per-arm Ask records mark fixture-injected retrieval as not_measured and score tool/agent plus final output separately",
        },
        retrieval_quality,
        snapshot_start: snapshot_start.clone(),
        snapshot_end,
        environment: environment_metadata(arm_order, repetition, &snapshot_start),
        egress_ledger,
        same_envelope_model_only,
        arms: reports,
        local_composite,
        paired_comparison,
    };
    let report_value =
        serde_json::to_value(&report).expect("project quality report for privacy audit");
    assert_eq!(
        artifact_privacy_violation(&report_value),
        None,
        "quality report failed the content-safe artifact policy"
    );
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("create quality result directory");
    }
    let json = serde_json::to_string_pretty(&report).expect("serialize quality report");
    std::fs::write(&out, format!("{json}\n")).expect("write quality report");
    eprintln!("MURMUR_QUALITY_REPORT written");
}

#[test]
fn quality_source_fingerprint_dependencies_cover_the_posture_router() {
    for required in REQUIRED_SOURCE_FINGERPRINT_FILES {
        assert!(
            SOURCE_FINGERPRINT_FILES.contains(required),
            "required source fingerprint dependency is missing: {required}"
        );
    }
    assert_eq!(
        SOURCE_FINGERPRINT_FILES
            .iter()
            .collect::<BTreeSet<_>>()
            .len(),
        SOURCE_FINGERPRINT_FILES.len(),
        "source fingerprint dependency list must not contain duplicates"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in SOURCE_FINGERPRINT_FILES {
        assert!(
            root.join(relative).is_file(),
            "source fingerprint dependency must be an existing file: {relative}"
        );
    }
}

#[test]
fn quality_manifest_is_synthetic_complete_and_unique() {
    let parsed = manifest();
    let manifest_value: serde_json::Value =
        serde_json::from_str(MANIFEST_JSON).expect("parse fixture for privacy audit");
    assert_eq!(
        artifact_privacy_violation(&manifest_value),
        None,
        "quality fixture failed the content-safe artifact policy"
    );
    assert_eq!(parsed.schema_version, 9);
    assert!(parsed.synthetic_only);
    assert_eq!(parsed.cases.len(), 18);
    let mut ids = parsed
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), parsed.cases.len());
    assert_eq!(
        parsed.cases.iter().filter(|case| case.holdout).count(),
        5,
        "the frozen pre-remediation cohort split must remain stratifiable"
    );
    assert!(parsed.cases.iter().any(|case| {
        case.holdout
            && case.surface == Surface::LiveCurrent
            && case.model_class == ModelClass::Light
    }));
    let reference_only = parsed
        .cases
        .iter()
        .filter(|case| case.sol_reference_only)
        .collect::<Vec<_>>();
    assert_eq!(reference_only.len(), 1);
    assert_eq!(reference_only[0].surface, Surface::LiveBullets);
    for surface in [
        Surface::Summary,
        Surface::MeetingChat,
        Surface::NoteAssist,
        Surface::AskVault,
        Surface::LiveCurrent,
        Surface::LiveBullets,
        Surface::LightExtraction,
    ] {
        assert!(
            parsed.cases.iter().any(|case| case.surface == surface),
            "missing surface {}",
            surface.as_str()
        );
    }
    assert!(parsed.cases.iter().all(|case| match case.surface {
        Surface::LightExtraction => case.model_class == ModelClass::Heavy,
        Surface::LiveCurrent | Surface::LiveBullets => case.model_class == ModelClass::Light,
        _ => true,
    }), "post-call fact extraction must measure the Fully Local heavy route while live surfaces stay on the light lane");
    assert!(
        !MANIFEST_JSON.contains('@')
            && !MANIFEST_JSON.contains("+48")
            && !MANIFEST_JSON.contains("MeetNotes/models"),
        "fixture must contain invented task text only, never contact data or machine paths"
    );
    for case in &parsed.cases {
        let candidate_inputs = [
            case.transcript.as_str(),
            case.question.as_str(),
            case.date_iso.as_str(),
            case.title_hint.as_str(),
            case.action.as_str(),
            case.selection.as_str(),
            case.before.as_str(),
            case.previous_bullets.as_str(),
            case.tool_result.as_str(),
            case.search_result.as_str(),
            case.floor_corpus.as_str(),
        ]
        .into_iter()
        .chain(case.vault_titles.iter().map(String::as_str))
        .chain(case.search_terms.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n");
        let mut unique_redaction_entities = case
            .synthetic_redaction_entities
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        unique_redaction_entities.sort_unstable();
        unique_redaction_entities.dedup();
        assert_eq!(
            unique_redaction_entities.len(),
            case.synthetic_redaction_entities.len(),
            "duplicate candidate-input redaction entity in {}",
            case.id
        );
        assert!(
            case.synthetic_redaction_entities
                .iter()
                .all(|entity| !entity.trim().is_empty() && candidate_inputs.contains(entity)),
            "every candidate-input redaction entity must occur in a non-oracle input for {}",
            case.id
        );
        match case.surface {
            Surface::Summary => {
                assert!(!case.date_iso.is_empty());
                assert!(!case.transcript.is_empty());
            }
            Surface::MeetingChat | Surface::LiveBullets => {
                assert!(!case.transcript.is_empty());
            }
            Surface::LiveCurrent => {
                assert!(!case.transcript.is_empty());
                assert!(case
                    .expected
                    .allowed_tools
                    .as_deref()
                    .is_some_and(|tools| tools.is_empty()));
            }
            Surface::NoteAssist => assert!(!case.selection.is_empty()),
            Surface::AskVault => {
                assert!(!case.question.is_empty());
                assert!(!case.search_terms.is_empty());
                assert!(!case.search_result.is_empty());
                assert!(!case.tool_result.is_empty());
                assert!(case.search_result.starts_with("- [meeting:"));
                assert!(case.search_result.contains(" [id:"));
                assert!(case.tool_result.starts_with("TITLE: [["));
                assert!(case.tool_result.contains("\n\nNOTE:\n"));
                assert!(case
                    .tool_result
                    .contains("\n\nTRANSCRIPT (format=structured, channel=merged):\n"));
                assert!(case.floor_corpus.starts_with("\n\n### [["));
                assert_eq!(case.expected.required_tools, ["get_meeting"]);
                assert_eq!(
                    case.expected.allowed_tools.as_deref(),
                    Some(
                        ["search_meetings", "search_semantic", "get_meeting"]
                            .map(str::to_string)
                            .as_slice()
                    )
                );
                assert!(!case.expected.required_provenance.is_empty());
            }
            Surface::LightExtraction => {
                assert!(!case.transcript.is_empty());
                assert!(!case.title_hint.is_empty());
                assert!(!case.vault_titles.is_empty());
                assert!(!case.expected.structured_facts.is_empty());
                assert_eq!(case.expected.format, OutputFormat::StructuredFacts);
            }
        }
    }
}

#[test]
fn canonical_case_commitment_matches_the_cross_language_known_vector() {
    let value = serde_json::json!([
        "Zażółć",
        serde_json::Value::Null,
        ["x", ""],
        1.5,
        true,
        {"escaped": "line\nquote\""}
    ]);
    assert_eq!(
        canonical_json_hash(&value),
        "f46b4ad1428b63de91b91de4897faaaa9d7c426e63236a4d812b42e0a092bb0e"
    );
}

#[test]
fn case_payload_commitment_ignores_evaluator_metadata_and_binds_candidate_inputs() {
    let case = manifest()
        .cases
        .into_iter()
        .find(|case| case.id == "meeting-chat-pl-delta")
        .expect("fixture case");
    let original = case_payload_sha256(&case);
    let mut metadata_only = serde_json::to_value(&case).expect("serialize fixture case");
    metadata_only["modelClass"] = serde_json::json!("light");
    metadata_only["holdout"] = serde_json::json!(!case.holdout);
    metadata_only["solReferenceOnly"] = serde_json::json!(true);
    metadata_only["expected"]["forbiddenTerms"] = serde_json::json!(["changed evaluator label"]);
    let metadata_only: QualityCase =
        serde_json::from_value(metadata_only).expect("deserialize metadata mutation");
    assert_eq!(case_payload_sha256(&metadata_only), original);

    let mut candidate_input = serde_json::to_value(&case).expect("serialize fixture case");
    candidate_input["question"] = serde_json::json!("different candidate-visible question");
    let candidate_input: QualityCase =
        serde_json::from_value(candidate_input).expect("deserialize input mutation");
    assert_ne!(case_payload_sha256(&candidate_input), original);

    let mut redaction_inventory = serde_json::to_value(&case).expect("serialize fixture case");
    redaction_inventory["syntheticRedactionEntities"] = serde_json::json!(["Delta"]);
    let redaction_inventory: QualityCase =
        serde_json::from_value(redaction_inventory).expect("deserialize redaction mutation");
    assert_ne!(case_payload_sha256(&redaction_inventory), original);
}

#[test]
fn route_specific_pair_is_not_labeled_model_only() {
    for surface in [
        Surface::Summary,
        Surface::MeetingChat,
        Surface::NoteAssist,
        Surface::AskVault,
        Surface::LiveCurrent,
        Surface::LiveBullets,
        Surface::LightExtraction,
    ] {
        assert_eq!(
            pair_comparison_kind(surface),
            PairComparisonKind::RouteSpecificProductSystem
        );
    }
}

#[test]
fn quality_dimensions_never_count_fixture_injection_as_retrieval_or_local_tool_agent_work() {
    let case = manifest()
        .cases
        .into_iter()
        .find(|case| case.id == "ask-vault-pl-orchid")
        .expect("Ask fixture case");
    let provenance = vec!["[[Orchid launch]]".to_string()];
    let output = "**pilotaż:** Orchid startuje 12 listopada w Krakowie. **działanie:** Iga odpowiada za incident playbook do 5 listopada. **budżet:** nie został zatwierdzony.";
    let score = score_output(output, &provenance, None, None, None, None, &case);
    let mut execution = plain_execution(
        output.to_string(),
        None,
        "hash".to_string(),
        1,
        "ask_vault_local_deterministic_floor",
    );
    execution.provenance = provenance;
    let dimensions = quality_dimensions(&case, &execution, &score);
    assert_eq!(dimensions.retrieval_quality, DimensionVerdict::NotMeasured);
    assert_eq!(
        dimensions.tool_agent_execution,
        DimensionVerdict::NotApplicable
    );
}

#[test]
fn good_fallback_can_fail_agent_but_pass_final_output() {
    let case = manifest()
        .cases
        .into_iter()
        .find(|case| case.id == "ask-vault-pl-orchid")
        .expect("Ask fixture case");
    let provenance = vec!["[[Orchid launch]]".to_string()];
    let output = "**pilotaż:** Orchid startuje 12 listopada w Krakowie. **działanie:** Iga odpowiada za incident playbook do 5 listopada. **budżet:** nie został zatwierdzony.";
    let score = score_output(
        output,
        &provenance,
        None,
        Some(false),
        None,
        Some(false),
        &case,
    );
    let mut execution = plain_execution(
        output.to_string(),
        None,
        "hash".to_string(),
        1,
        "ask_vault_cloud_agentic_then_floor_fallback",
    );
    execution.provenance = provenance;
    execution.tool_policy_pass = Some(false);
    execution.branch_converged = Some(false);
    let dimensions = quality_dimensions(&case, &execution, &score);
    assert_eq!(dimensions.tool_agent_execution, DimensionVerdict::Fail);
    assert_eq!(
        dimensions.final_product_output_contract,
        DimensionVerdict::Pass
    );
}

#[test]
fn dimension_aggregate_nulls_rates_without_measurements() {
    let verdicts = [
        DimensionVerdict::NotMeasured,
        DimensionVerdict::NotApplicable,
    ];
    let aggregate = dimension_aggregate(verdicts.iter());
    assert_eq!(aggregate.measured_observations, 0);
    assert_eq!(aggregate.pass_rate, None);
    assert_eq!(aggregate.coverage_rate, Some(0.0));
}

#[test]
fn artifact_privacy_audit_rejects_contact_paths_credentials_urls_and_prompt_fields() {
    let canaries = [
        serde_json::json!({"output": "person@example.test"}),
        serde_json::json!({"output": "+48 600 700 800"}),
        serde_json::json!({"output": "/Users/example/Vault/private.md"}),
        serde_json::json!({"output": "https://invalid.example/private"}),
        serde_json::json!({"output": "Bearer abcdefghijklmnop"}),
        serde_json::json!({"output": "ghp_abcdefghijklmnopqrst"}),
        serde_json::json!({"systemPrompt": "synthetic but forbidden to retain"}),
    ];
    for canary in canaries {
        assert!(
            artifact_privacy_violation(&canary).is_some(),
            "privacy canary must be rejected"
        );
    }
    assert_eq!(
        artifact_privacy_violation(&serde_json::json!({
            "promptVersion": "quality-v1",
            "outputSha256": "0123456789abcdef",
            "output": "Invented project Helix is ready on 2026-12-03."
        })),
        None
    );
}

#[test]
fn model_class_eligibility_keeps_heavy_and_light_lanes_separate() {
    assert!(arm_accepts(QWEN4_ID, ModelClass::Heavy));
    assert!(!arm_accepts(QWEN4_ID, ModelClass::Light));
    assert!(arm_accepts(QWEN1_ID, ModelClass::Light));
    assert!(!arm_accepts(QWEN1_ID, ModelClass::Heavy));
    assert!(arm_accepts(SOL_ID, ModelClass::Heavy));
    assert!(arm_accepts(SOL_ID, ModelClass::Light));
}

#[test]
fn sol_bullets_are_reference_only_and_local_bullets_remain_product_path() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "live-bullets-pl-polaris")
        .unwrap();
    assert_eq!(
        comparison_scope_for(false, case),
        ComparisonScope::ProductPath
    );
    assert_eq!(
        comparison_scope_for(true, case),
        ComparisonScope::OfflineReferenceCeiling
    );
}

#[derive(Debug, PartialEq)]
struct ReplayedReceipts {
    provenance: Vec<String>,
    tool_policy_score: Option<f64>,
    tool_policy_pass: Option<bool>,
    state_application_pass: Option<bool>,
    branch_converged: Option<bool>,
}

fn record_string_sequence(record: &serde_json::Value, key: &str) -> Vec<String> {
    record
        .get(key)
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("source case {key} array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("source case {key} string"))
                .to_string()
        })
        .collect()
}

fn floor_provenance(case: &QualityCase) -> Vec<String> {
    let corpus = if case.floor_corpus.is_empty() {
        &case.tool_result
    } else {
        &case.floor_corpus
    };
    extract_wikilinks(corpus)
}

fn current_meeting_provenance(case: &QualityCase) -> Vec<String> {
    let title = case.title_hint.trim();
    if title.is_empty() {
        Vec::new()
    } else {
        vec![format!("[[{title}]]")]
    }
}

/// Reproduce every non-text input to `score_output` from the immutable fixture plus the recorded
/// execution trace. Stored booleans are receipts to CHECK, never authorities to trust. This catches
/// scores/receipts inconsistent with the recorded labels, route, fixture, and output. It does not
/// authenticate those string labels or attest that an external model produced them; a coordinated
/// archive rewrite plus rehash still needs the external exact-diff/Harness provenance boundary.
fn replay_score_receipts(
    record: &serde_json::Value,
    arm_id: &str,
    case: &QualityCase,
) -> ReplayedReceipts {
    assert!(
        arm_accepts(arm_id, case.model_class),
        "source arm {arm_id} cannot execute {}",
        case.id
    );
    let cloud_reference = arm_id == SOL_ID;
    let route = record
        .get("productRoute")
        .and_then(serde_json::Value::as_str)
        .expect("source case product route");
    let route_is_valid = match (case.surface, cloud_reference) {
        (Surface::Summary, _) => route == "summary_provider_then_pipeline_assembly",
        (Surface::MeetingChat, _) => route == "meeting_chat_one_completion",
        (Surface::NoteAssist, _) => route == "note_assist_generate_note_edit",
        (Surface::AskVault, false) => route == "ask_vault_local_deterministic_floor",
        (Surface::AskVault, true) => matches!(
            route,
            "ask_vault_cloud_agentic" | "ask_vault_cloud_agentic_then_floor_fallback"
        ),
        (Surface::LiveCurrent, false) => route == "live_current_local_isolated_floor",
        (Surface::LiveCurrent, true) => matches!(
            route,
            "live_current_cloud_tier1"
                | "live_current_cloud_tier2_vault"
                | "live_current_cloud_tier3_connectors"
                | "live_current_cloud_cascade_then_isolated_floor"
        ),
        (Surface::LiveBullets, false) => route == "live_bullets_local_update_parse_append",
        (Surface::LiveBullets, true) => route == "live_bullets_counterfactual_cloud_ceiling",
        (Surface::LightExtraction, _) => {
            route == "fully_local_post_call_fact_extraction_structured_projection"
        }
    };
    assert!(
        route_is_valid,
        "source arm {arm_id}/{} has impossible product route {route}",
        case.id
    );

    let expected_scope = match comparison_scope_for(cloud_reference, case) {
        ComparisonScope::ProductPath => "product_path",
        ComparisonScope::OfflineReferenceCeiling => "offline_reference_ceiling",
    };
    assert_eq!(
        record
            .get("comparisonScope")
            .and_then(serde_json::Value::as_str),
        Some(expected_scope),
        "source arm {arm_id}/{} has impossible comparison scope",
        case.id
    );

    let tool_steps = record_string_sequence(record, "toolSteps");
    let (mut replayed_tool_policy_score, mut replayed_tool_policy_pass) = tool_policy_score(
        &case.expected.required_tools,
        case.expected.allowed_tools.as_deref(),
        &tool_steps,
    );
    let error = record.get("error").and_then(serde_json::Value::as_str);
    let branch_converged = match route {
        "ask_vault_cloud_agentic"
        | "live_current_cloud_tier1"
        | "live_current_cloud_tier2_vault"
        | "live_current_cloud_tier3_connectors" => Some(error.is_none()),
        "ask_vault_cloud_agentic_then_floor_fallback"
        | "live_current_cloud_cascade_then_isolated_floor" => Some(false),
        _ => None,
    };
    if route == "ask_vault_local_deterministic_floor" {
        replayed_tool_policy_score = None;
        replayed_tool_policy_pass = None;
    } else if route == "ask_vault_cloud_agentic_then_floor_fallback" {
        replayed_tool_policy_pass = Some(false);
    }

    let output = record
        .get("output")
        .and_then(serde_json::Value::as_str)
        .expect("source case output");
    let state_application_pass = if case.surface == Surface::LiveBullets {
        record
            .get("surfaceOutput")
            .and_then(serde_json::Value::as_str)
            .map(|surface| {
                surface.contains(case.previous_bullets.trim())
                    && output
                        .lines()
                        .all(|line| line.trim().is_empty() || surface.contains(line.trim()))
            })
    } else {
        None
    };

    let provenance =
        match route {
            "ask_vault_local_deterministic_floor"
            | "ask_vault_cloud_agentic_then_floor_fallback" => floor_provenance(case),
            "ask_vault_cloud_agentic"
                if branch_converged == Some(true) && replayed_tool_policy_pass == Some(true) =>
            {
                extract_wikilinks(&case.tool_result)
            }
            "live_current_local_isolated_floor"
            | "live_current_cloud_cascade_then_isolated_floor" => current_meeting_provenance(case),
            _ => Vec::new(),
        };

    let replayed = ReplayedReceipts {
        provenance,
        tool_policy_score: replayed_tool_policy_score,
        tool_policy_pass: replayed_tool_policy_pass,
        state_application_pass,
        branch_converged,
    };
    assert_eq!(
        record_string_sequence(record, "provenance"),
        replayed.provenance,
        "source arm {arm_id}/{} provenance differs from fixture plus tool trace",
        case.id
    );
    assert_eq!(
        record
            .get("toolPolicyScore")
            .and_then(serde_json::Value::as_f64),
        replayed.tool_policy_score,
        "source arm {arm_id}/{} tool score differs from tool trace",
        case.id
    );
    assert_eq!(
        record
            .get("toolPolicyPass")
            .and_then(serde_json::Value::as_bool),
        replayed.tool_policy_pass,
        "source arm {arm_id}/{} tool-policy receipt differs from tool trace",
        case.id
    );
    assert_eq!(
        record
            .get("stateApplicationPass")
            .and_then(serde_json::Value::as_bool),
        replayed.state_application_pass,
        "source arm {arm_id}/{} state receipt differs from applied output",
        case.id
    );
    assert_eq!(
        record
            .get("branchConverged")
            .and_then(serde_json::Value::as_bool),
        replayed.branch_converged,
        "source arm {arm_id}/{} branch receipt differs from product route",
        case.id
    );
    replayed
}

/// Re-score a frozen real-run artifact with the current deterministic oracle without calling any
/// model. Text comes from the record; every grounding/state receipt is independently re-derived.
fn rescore_case_record(
    record: &serde_json::Value,
    parsed: &QualityManifest,
    arm_id: &str,
) -> OracleScore {
    let case_id = record
        .get("caseId")
        .and_then(serde_json::Value::as_str)
        .expect("source case id");
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == case_id)
        .unwrap_or_else(|| panic!("source case {case_id} is absent from manifest"));
    let output = record
        .get("output")
        .and_then(serde_json::Value::as_str)
        .expect("source case output");
    let receipts = replay_score_receipts(record, arm_id, case);
    let error = record.get("error").and_then(serde_json::Value::as_str);
    score_output(
        output,
        &receipts.provenance,
        error,
        receipts.tool_policy_pass,
        receipts.state_application_pass,
        receipts.branch_converged,
        case,
    )
}

#[test]
#[ignore = "requires MURMUR_QUALITY_RESCORE_IN and MURMUR_QUALITY_RESCORE_OUT; no model calls"]
fn rescore_generation_quality_artifact_from_env() {
    let input_path = PathBuf::from(
        std::env::var("MURMUR_QUALITY_RESCORE_IN")
            .expect("MURMUR_QUALITY_RESCORE_IN must name a frozen quality artifact"),
    );
    let output_path = PathBuf::from(
        std::env::var("MURMUR_QUALITY_RESCORE_OUT")
            .expect("MURMUR_QUALITY_RESCORE_OUT must name the rescore artifact"),
    );
    let source_bytes = std::fs::read(&input_path).expect("read frozen quality artifact");
    let source: serde_json::Value =
        serde_json::from_slice(&source_bytes).expect("parse frozen quality artifact");
    let parsed = manifest();
    let arms = source
        .get("arms")
        .and_then(serde_json::Value::as_array)
        .expect("source artifact arms array")
        .iter()
        .map(|arm| {
            let arm_id = arm
                .pointer("/metadata/armId")
                .and_then(serde_json::Value::as_str)
                .expect("source arm id");
            let cases = arm
                .get("cases")
                .and_then(serde_json::Value::as_array)
                .expect("source arm cases")
                .iter()
                .map(|record| {
                    let case_id = record
                        .get("caseId")
                        .and_then(serde_json::Value::as_str)
                        .expect("source case id");
                    let case = parsed
                        .cases
                        .iter()
                        .find(|case| case.id == case_id)
                        .unwrap_or_else(|| panic!("source case {case_id} is absent from manifest"));
                    let rescored = rescore_case_record(record, &parsed, arm_id);
                    serde_json::json!({
                        "caseId": case_id,
                        "surface": case.surface,
                        "holdout": case.holdout,
                        "outputSha256": record.get("outputSha256").cloned().unwrap_or(serde_json::Value::Null),
                        "originalScore": record.get("score").cloned().expect("source case score"),
                        "rescoredScore": rescored,
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({"armId": arm_id, "cases": cases})
        })
        .collect::<Vec<_>>();
    let current = run_snapshot();
    let source_sha256 = hex_digest(Sha256::digest(&source_bytes).as_slice());
    let rescored = serde_json::json!({
        "schemaVersion": 1,
        "kind": "deterministic_rescore_no_model_calls",
        "sourceArtifact": input_path.file_name().and_then(|name| name.to_str()),
        "sourceArtifactSha256": source_sha256,
        "sourceRunLabel": source.get("runLabel").cloned().unwrap_or(serde_json::Value::Null),
        "sourceRepositoryCommit": source.get("repositoryCommit").cloned().unwrap_or(serde_json::Value::Null),
        "scorerSnapshot": {
            "repositoryCommit": current.repository_commit,
            "sourceFingerprintSha256": current.source_fingerprint_sha256,
            "manifestSha256": current.manifest_sha256,
            "evaluatorFileSha256": current.evaluator_file_sha256,
            "fixtureFileSha256": current.fixture_file_sha256,
        },
        "arms": arms,
    });
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).expect("create rescore output directory");
    }
    std::fs::write(
        &output_path,
        serde_json::to_vec_pretty(&rescored).expect("serialize rescore artifact"),
    )
    .expect("write rescore artifact");
}

#[test]
fn deterministic_oracle_accepts_good_and_rejects_bad_output() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "meeting-chat-pl-delta")
        .unwrap();
    let good = "Start zaplanowano na 3 października. Nina odpowiada za dokumentację do 25 września. Cena kolejnego etapu nie została uzgodniona.";
    let bad = "Start będzie później. Cena została uzgodniona.";
    let good_score = score_output(good, &[], None, None, None, None, case);
    let bad_score = score_output(bad, &[], None, None, None, None, case);
    assert_eq!(good_score.diagnostic_score, 100.0);
    assert!(good_score.case_pass);
    assert!(good_score.critical_errors.is_empty());
    assert!(bad_score.diagnostic_score < 50.0);
    assert!(!bad_score.case_pass);
    assert!(!bad_score.critical_errors.is_empty());
    assert!(!bad_score.forbidden_pass);
}

struct BenchmarkFixtureCodexProvider {
    fail: bool,
}

#[async_trait::async_trait]
impl SummarizerProvider for BenchmarkFixtureCodexProvider {
    fn id(&self) -> &str {
        crate::summarize::PROVIDER_CODEX_CLI
    }

    async fn availability(&self) -> crate::summarize::provider::Availability {
        crate::summarize::provider::Availability::Available
    }

    async fn summarize(&self, _request: &SummarizeRequest) -> Result<String> {
        if self.fail {
            Err(AppError::Summarize("synthetic transport failure".into()))
        } else {
            Ok("synthetic response".to_string())
        }
    }

    async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
        if self.fail {
            Err(AppError::Summarize("synthetic transport failure".into()))
        } else {
            Ok("synthetic response".to_string())
        }
    }
}

struct CountingModelOnlyProvider {
    complete_calls: Arc<AtomicU64>,
    summarize_calls: Arc<AtomicU64>,
    reply: String,
}

struct ModelOnlyPreflightCaptureProvider {
    captured: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait::async_trait]
impl SummarizerProvider for ModelOnlyPreflightCaptureProvider {
    fn id(&self) -> &str {
        crate::summarize::PROVIDER_CODEX_CLI
    }

    async fn availability(&self) -> crate::summarize::provider::Availability {
        crate::summarize::provider::Availability::Available
    }

    async fn summarize(&self, _request: &SummarizeRequest) -> Result<String> {
        panic!("same-envelope preflight must never call summarize")
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        self.captured
            .lock()
            .expect("capture preflight transport call")
            .push((system.to_string(), user.to_string()));
        Ok("synthetic preflight response".to_string())
    }
}

#[async_trait::async_trait]
impl SummarizerProvider for CountingModelOnlyProvider {
    fn id(&self) -> &str {
        crate::summarize::PROVIDER_CODEX_CLI
    }

    async fn availability(&self) -> crate::summarize::provider::Availability {
        crate::summarize::provider::Availability::Available
    }

    async fn summarize(&self, _request: &SummarizeRequest) -> Result<String> {
        self.summarize_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.reply.clone())
    }

    async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
        self.complete_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.reply.clone())
    }
}

fn model_only_test_arm(
    arm_id: &str,
    provider: Arc<dyn SummarizerProvider>,
    sink: Option<Arc<BenchmarkEgressSink>>,
    cloud_reference: bool,
) -> ArmRuntime {
    let reasoner: Arc<dyn LocalReasoner> = Arc::new(crate::reason::StubReasoner);
    ArmRuntime {
        metadata: ArmMetadata {
            arm_id: arm_id.to_string(),
            model_requested: if cloud_reference {
                CODEX_MODEL.to_string()
            } else {
                "synthetic-local".to_string()
            },
            effort: cloud_reference.then(|| CODEX_EFFORT.to_string()),
            effort_transport: None,
            effort_effective_attested: cloud_reference.then_some(false),
            model_class: "test".to_string(),
            model_filename: None,
            model_bytes: None,
            model_sha256: None,
            runtime_version: None,
            runtime_sha256: None,
            sidecar_idle_secs: None,
            sidecar_ready_secs: None,
            sidecar_hard_cap_secs: None,
        },
        notes_provider: Arc::clone(&provider),
        ask_provider: Arc::clone(&provider),
        model_only_provider: provider,
        ask_reasoner: Arc::clone(&reasoner),
        live_reasoner: Arc::clone(&reasoner),
        extraction_reasoner: reasoner,
        egress_sink: sink,
        cloud_reference,
    }
}

#[test]
fn same_envelope_builder_is_arm_independent_for_every_manifest_case() {
    for case in manifest().cases {
        let first = build_same_envelope(&case);
        let second = build_same_envelope(&case);
        assert_eq!(first.system, second.system, "system drift: {}", case.id);
        assert_eq!(first.user, second.user, "user drift: {}", case.id);
        assert_eq!(
            first.envelope_sha256, second.envelope_sha256,
            "envelope drift: {}",
            case.id
        );
        assert_eq!(
            first.substitutions, second.substitutions,
            "opaque substitution ordering drift: {}",
            case.id
        );
        assert_eq!(first.projection, second.projection);
        assert!(!first.system.is_empty());
        assert!(!first.user.is_empty());
    }
}

#[tokio::test]
#[ignore = "real canonical firewall preflight; the full quality run executes it automatically"]
async fn same_envelope_is_byte_identical_at_canonical_cloud_transport_boundary() {
    assert_same_envelope_cloud_firewall_byte_preserving(&manifest().cases).await;
}

struct ModelOnlyContractCaptureReasoner {
    raw: String,
    structured_call: Mutex<Option<(String, String, serde_json::Value)>>,
}

impl LocalReasoner for ModelOnlyContractCaptureReasoner {
    fn id(&self) -> &str {
        "quality-contract-capture"
    }

    fn reason(&self, _system: &str, _user: &str) -> Result<String> {
        Ok(self.raw.clone())
    }

    fn structured(
        &self,
        system: &str,
        user: &str,
        schema: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        *self.structured_call.lock().expect("capture fact contract") =
            Some((system.to_string(), user.to_string(), schema.clone()));
        Ok(serde_json::json!({"facts": []}))
    }
}

#[test]
fn evaluator_fact_contract_is_byte_identical_to_the_shipped_extractor() {
    let entities = vec![("entity-zuraw".to_string(), "Żuraw".to_string())];
    let entity_names = vec!["Żuraw".to_string()];
    let note = format!("{}TRUNCATED", "ą".repeat(8_000));
    let expected = model_only_fact_extraction_contract("Projekt", &note, &entity_names, "pl");
    let reasoner = ModelOnlyContractCaptureReasoner {
        raw: String::new(),
        structured_call: Mutex::new(None),
    };
    let _ = crate::facts::extract_fact_candidates(
        &reasoner,
        "Projekt",
        &note,
        &entities,
        "pl",
        GenOptions::default(),
    );
    let actual = reasoner
        .structured_call
        .lock()
        .expect("read fact contract")
        .clone()
        .expect("production extractor called structured reasoner");
    assert_eq!(actual, expected);
    assert!(!expected.1.contains("TRUNCATED"));
}

#[test]
fn evaluator_live_bullet_projection_is_byte_identical_to_the_shipped_parser() {
    let raw = concat!(
        "preamble\n",
        "- [decyzja]: start w piątek\n",
        "- [owner]: Anna\n",
        "- [ryzyko]: brak QA\n",
        "- [extra]: odrzucone"
    );
    let reasoner = ModelOnlyContractCaptureReasoner {
        raw: raw.to_string(),
        structured_call: Mutex::new(None),
    };
    let shipped = crate::transcribe::bullets::update_bullets(
        &reasoner,
        "- [start]: kickoff",
        "To jest wystarczająco długi syntetyczny fragment spotkania dla testu parsera.",
    )
    .expect("shipped parser accepts synthetic bullets");
    assert_eq!(model_only_parse_live_bullets(raw), Some(shipped));
}

#[tokio::test]
async fn same_envelope_note_assist_calls_complete_once_without_summarize_or_retry() {
    let complete_calls = Arc::new(AtomicU64::new(0));
    let summarize_calls = Arc::new(AtomicU64::new(0));
    let provider: Arc<dyn SummarizerProvider> = Arc::new(CountingModelOnlyProvider {
        complete_calls: Arc::clone(&complete_calls),
        summarize_calls: Arc::clone(&summarize_calls),
        reply: "Shorter synthetic passage.".to_string(),
    });
    let arm = model_only_test_arm(QWEN4_ID, provider, None, false);
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-shorten-en")
        .expect("shorten case");
    let result = execute_model_only_case(&arm, case).await;
    assert_eq!(result.call_count, 1);
    assert_eq!(complete_calls.load(Ordering::SeqCst), 1);
    assert_eq!(summarize_calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.egress_receipt_count, 0);
    assert_eq!(result.projection, ModelOnlyProjection::RawTrimmed);
}

#[tokio::test]
async fn same_envelope_cloud_call_uses_canonical_firewall_and_one_durable_zero_redaction_receipt() {
    let sink = Arc::new(BenchmarkEgressSink::create());
    let complete_calls = Arc::new(AtomicU64::new(0));
    let summarize_calls = Arc::new(AtomicU64::new(0));
    let inner: Arc<dyn SummarizerProvider> = Arc::new(CountingModelOnlyProvider {
        complete_calls: Arc::clone(&complete_calls),
        summarize_calls: Arc::clone(&summarize_calls),
        reply: "Start zaplanowano na 3 października. SYNTH_ENTITY_01 odpowiada za dokumentację do 25 września. Cena kolejnego etapu nie została uzgodniona.".to_string(),
    });
    let config = cloud_config();
    let heavy = Arc::new(tokio::sync::Semaphore::new(1));
    let provider = crate::summarize::provider_for_with_test_egress_sink(
        Role::Ask,
        &config,
        &heavy,
        sink.clone(),
        inner,
    )
    .expect("construct canonical benchmark provider");
    let arm = model_only_test_arm(SOL_ID, provider, Some(sink.clone()), true);
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "meeting-chat-pl-delta")
        .expect("meeting chat case");
    let result = execute_model_only_case(&arm, case).await;
    assert_eq!(complete_calls.load(Ordering::SeqCst), 1);
    assert_eq!(summarize_calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.egress_receipt_count, 1);
    assert_eq!(
        (
            result.redactions_email,
            result.redactions_card,
            result.redactions_phone,
            result.redactions_name,
        ),
        (0, 0, 0, 0)
    );
    assert!(result.output.contains("Nina"));
    drop(arm);
    let evidence = sink.evidence(true);
    assert!(evidence.sqlite_persistence_verified);
    assert!(evidence.temporary_database_cleaned);
    assert_eq!(evidence.persisted_rows, 1);
}

#[tokio::test]
async fn benchmark_provider_path_persists_content_free_success_and_failure_receipts() {
    let sink = Arc::new(BenchmarkEgressSink::create());
    let config = cloud_config();
    let heavy = Arc::new(tokio::sync::Semaphore::new(1));
    for fail in [false, true] {
        let provider = crate::summarize::provider_for_with_test_egress_sink(
            Role::Ask,
            &config,
            &heavy,
            sink.clone(),
            Arc::new(BenchmarkFixtureCodexProvider { fail }),
        )
        .expect("construct exact benchmark consent/redaction/ledger provider path");
        let result = provider
            .complete("synthetic system", "synthetic user")
            .await;
        assert_eq!(result.is_err(), fail);
    }
    let evidence = sink.evidence(true);
    assert!(evidence.sqlite_persistence_verified);
    assert!(evidence.temporary_database_cleaned);
    assert_eq!(evidence.attempted_rows, 2);
    assert_eq!(evidence.persisted_rows, 2);
    assert_eq!(
        evidence.provider_ids,
        vec![crate::summarize::PROVIDER_CODEX_CLI.to_string()]
    );
    assert!(evidence
        .rows
        .iter()
        .all(|row| row.model_requested == CODEX_MODEL));
    assert_eq!(
        evidence
            .rows
            .iter()
            .map(|row| row.call_kind.as_str())
            .collect::<Vec<_>>(),
        vec!["complete", "complete_error"]
    );
    let serialized = serde_json::to_value(&evidence).expect("serialize benchmark egress evidence");
    assert_eq!(artifact_privacy_violation(&serialized), None);
}

#[test]
fn committed_quality_evidence_replays_without_models_or_network() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a repository parent");
    let validator = repository.join("eval/results/validate_generation_quality_repeats.py");
    let evidence = repository.join(COMMITTED_EVIDENCE_MANIFEST);
    assert_committed_artifact_scores_match_current_oracle(repository, &evidence);
    let output = std::process::Command::new("python3")
        .arg(&validator)
        .arg("--verify-evidence")
        .arg(&evidence)
        .current_dir(repository)
        .output()
        .expect("run committed quality evidence validator");
    assert!(
        output.status.success(),
        "committed quality evidence did not replay:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_model_only_record_matches_current_contract(
    record: &serde_json::Value,
    parsed: &QualityManifest,
    arm_id: &str,
    ledger_rows: &[BenchmarkEgressRow],
    context: &str,
) {
    let typed: ModelOnlyCaseResult =
        serde_json::from_value(record.clone()).expect("parse committed model-only case");
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == typed.case_id)
        .unwrap_or_else(|| panic!("{context}: unknown model-only case {}", typed.case_id));
    assert!(
        arm_accepts(arm_id, case.model_class),
        "{context}: model-only arm/class mismatch"
    );
    assert_eq!(typed.arm_id, arm_id, "{context}: arm id mismatch");
    assert_eq!(
        typed.case_payload_sha256,
        case_payload_sha256(case),
        "{context}: candidate payload commitment differs"
    );

    let envelope = build_same_envelope(case);
    let substitutions_json =
        serde_json::to_string(&envelope.substitutions).expect("serialize replay substitutions");
    assert_eq!(typed.system_sha256, envelope.system_sha256);
    assert_eq!(typed.user_sha256, envelope.user_sha256);
    assert_eq!(typed.envelope_sha256, envelope.envelope_sha256);
    assert_eq!(typed.system_bytes, envelope.system.len());
    assert_eq!(typed.user_bytes, envelope.user.len());
    assert_eq!(typed.system_chars, envelope.system.chars().count());
    assert_eq!(typed.user_chars, envelope.user.chars().count());
    assert_eq!(typed.projection, envelope.projection);
    assert_eq!(typed.output_contract, envelope.output_contract);
    assert_eq!(
        typed.opaque_substitution_count,
        envelope.substitutions.len()
    );
    assert_eq!(
        typed.opaque_substitutions_sha256,
        prompt_hash(&[&substitutions_json])
    );
    assert_eq!(typed.call_count, 1, "{context}: call count differs");
    assert_eq!(typed.output_chars, typed.output.chars().count());
    assert_eq!(typed.output_sha256, prompt_hash(&[&typed.output]));
    assert_eq!(
        typed.provenance_sha256,
        string_sequence_hash(&typed.provenance)
    );

    let rescored = score_output(
        &typed.output,
        &typed.provenance,
        typed.error.as_deref(),
        None,
        typed.state_application_pass,
        None,
        case,
    );
    assert_eq!(
        serde_json::to_value(&typed.score).expect("serialize committed model-only score"),
        serde_json::to_value(&rescored).expect("serialize replayed model-only score"),
        "{context}: model-only score differs from current Rust oracle"
    );

    let selected_rows = match (
        typed.egress_receipt_start_ordinal,
        typed.egress_receipt_end_ordinal,
    ) {
        (Some(start), Some(end)) => ledger_rows
            .iter()
            .filter(|row| row.ordinal >= start && row.ordinal <= end)
            .cloned()
            .collect::<Vec<_>>(),
        (None, None) => Vec::new(),
        _ => panic!("{context}: incomplete egress ordinal range"),
    };
    assert_eq!(selected_rows.len() as u64, typed.egress_receipt_count);
    assert_eq!(
        selected_rows.first().map(|row| row.ordinal),
        typed.egress_receipt_start_ordinal
    );
    assert_eq!(
        selected_rows.last().map(|row| row.ordinal),
        typed.egress_receipt_end_ordinal
    );
    let receipt_json =
        serde_json::to_string(&selected_rows).expect("serialize replayed model-only receipts");
    assert_eq!(typed.egress_receipt_sha256, prompt_hash(&[&receipt_json]));
    let redaction_totals = selected_rows.iter().fold((0, 0, 0, 0), |totals, row| {
        (
            totals.0 + row.redactions_email,
            totals.1 + row.redactions_card,
            totals.2 + row.redactions_phone,
            totals.3 + row.redactions_name,
        )
    });
    assert_eq!(
        redaction_totals,
        (
            typed.redactions_email,
            typed.redactions_card,
            typed.redactions_phone,
            typed.redactions_name,
        )
    );
    if arm_id == SOL_ID {
        assert_eq!(selected_rows.len(), 1);
        assert_eq!(redaction_totals, (0, 0, 0, 0));
        let expected_call_kind = if typed
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("model_call_failed:"))
        {
            "complete_error"
        } else {
            "complete"
        };
        assert_eq!(selected_rows[0].call_kind, expected_call_kind);
    } else {
        assert!(selected_rows.is_empty());
    }
    assert_eq!(
        typed.case_record_sha256,
        model_only_case_record_sha256(&typed),
        "{context}: model-only case record commitment differs"
    );
}

fn assert_committed_artifact_scores_match_current_oracle(repository: &Path, evidence_path: &Path) {
    let evidence: serde_json::Value = serde_json::from_slice(
        &std::fs::read(evidence_path).expect("read committed quality evidence manifest"),
    )
    .expect("parse committed quality evidence manifest");
    assert_eq!(
        artifact_privacy_violation(&evidence),
        None,
        "committed quality evidence manifest failed the privacy policy"
    );
    let parsed = manifest();
    for repetition in ["1", "2"] {
        let archive = evidence
            .pointer(&format!("/repetitions/{repetition}/archivePath"))
            .and_then(serde_json::Value::as_str)
            .expect("evidence repetition archive path");
        let output = std::process::Command::new("gzip")
            .arg("-dc")
            .arg(repository.join(archive))
            .output()
            .expect("decompress committed quality run");
        assert!(
            output.status.success(),
            "cannot decompress committed quality repetition {repetition}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.len() <= 16 * 1024 * 1024,
            "committed quality repetition {repetition} exceeds the 16 MiB replay cap"
        );
        let report: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("parse committed quality run");
        assert_eq!(
            artifact_privacy_violation(&report),
            None,
            "committed quality repetition {repetition} failed the privacy policy"
        );
        let ledger_rows = serde_json::from_value::<Vec<BenchmarkEgressRow>>(
            report
                .pointer("/egressLedger/rows")
                .cloned()
                .expect("committed quality egress rows"),
        )
        .expect("parse committed quality egress rows");
        for arm in report
            .get("arms")
            .and_then(serde_json::Value::as_array)
            .expect("committed quality arms")
        {
            let arm_id = arm
                .pointer("/metadata/armId")
                .and_then(serde_json::Value::as_str)
                .expect("committed quality arm id");
            for record in arm
                .get("cases")
                .and_then(serde_json::Value::as_array)
                .expect("committed quality cases")
            {
                let case_id = record
                    .get("caseId")
                    .and_then(serde_json::Value::as_str)
                    .expect("committed quality case id");
                let rescored = serde_json::to_value(rescore_case_record(record, &parsed, arm_id))
                    .expect("serialize recomputed quality score");
                assert_eq!(
                    record.get("score"),
                    Some(&rescored),
                    "repetition {repetition}/{arm_id}/{case_id}: committed score differs from current Rust oracle"
                );
            }
        }
        for arm in report
            .pointer("/sameCallerEnvelopeModelStack/arms")
            .and_then(serde_json::Value::as_array)
            .expect("committed model-only arms")
        {
            let arm_id = arm
                .get("armId")
                .and_then(serde_json::Value::as_str)
                .expect("committed model-only arm id");
            for record in arm
                .get("cases")
                .and_then(serde_json::Value::as_array)
                .expect("committed model-only cases")
            {
                let case_id = record
                    .get("caseId")
                    .and_then(serde_json::Value::as_str)
                    .expect("committed model-only case id");
                assert_model_only_record_matches_current_contract(
                    record,
                    &parsed,
                    arm_id,
                    &ledger_rows,
                    &format!("repetition {repetition}/{arm_id}/{case_id}"),
                );
            }
        }
    }
}

#[test]
fn committed_evidence_rescore_rejects_a_forged_self_consistent_verdict() {
    let parsed = manifest();
    let record = serde_json::json!({
        "caseId": "meeting-chat-pl-delta",
        "productRoute": "meeting_chat_one_completion",
        "comparisonScope": "product_path",
        "output": "Start zaplanowano na 3 października. Nina odpowiada za dokumentację do 25 września. Cena kolejnego etapu nie została uzgodniona.",
        "surfaceOutput": null,
        "provenance": [],
        "toolSteps": [],
        "toolPolicyScore": null,
        "error": null,
        "toolPolicyPass": null,
        "stateApplicationPass": null,
        "branchConverged": null
    });
    let recomputed = rescore_case_record(&record, &parsed, QWEN4_ID);
    assert!(recomputed.case_pass);
    assert_eq!(recomputed.diagnostic_score, 100.0);
    let mut forged = serde_json::to_value(&recomputed).expect("serialize valid score");
    forged["casePass"] = serde_json::json!(false);
    forged["diagnosticScore"] = serde_json::json!(0.0);
    assert_ne!(
        forged,
        serde_json::to_value(rescore_case_record(&record, &parsed, QWEN4_ID))
            .expect("serialize independently recomputed score")
    );
}

#[test]
fn committed_evidence_rescore_rejects_a_forged_tool_policy_receipt() {
    let parsed = manifest();
    let mut record = serde_json::json!({
        "caseId": "ask-vault-pl-orchid",
        "productRoute": "ask_vault_cloud_agentic",
        "comparisonScope": "product_path",
        "output": "Pilotaż Orchid startuje 12 listopada w Krakowie. Iga odpowiada za incident playbook. Budżet nie został zatwierdzony.",
        "surfaceOutput": null,
        "provenance": ["[[Orchid launch]]"],
        "toolSteps": ["search_meetings", "get_meeting"],
        "toolPolicyScore": 100.0,
        "error": null,
        "toolPolicyPass": true,
        "stateApplicationPass": null,
        "branchConverged": true
    });
    let valid = rescore_case_record(&record, &parsed, SOL_ID);
    assert!(valid.tool_policy_pass);
    assert!(valid.provenance_pass);

    record["toolSteps"] = serde_json::json!([]);
    let rejected = std::panic::catch_unwind(|| rescore_case_record(&record, &parsed, SOL_ID));
    assert!(
        rejected.is_err(),
        "an empty tool trace must not retain a positive retrieval receipt"
    );
}

#[test]
fn committed_evidence_rescore_rejects_get_without_a_search_prerequisite() {
    let parsed = manifest();
    let mut record = serde_json::json!({
        "caseId": "ask-vault-pl-orchid",
        "productRoute": "ask_vault_cloud_agentic",
        "comparisonScope": "product_path",
        "output": "Pilotaż Orchid startuje 12 listopada w Krakowie. Iga odpowiada za incident playbook. Budżet nie został zatwierdzony.",
        "surfaceOutput": null,
        "provenance": ["[[Orchid launch]]"],
        "toolSteps": ["get_meeting"],
        "toolPolicyScore": 100.0,
        "error": null,
        "toolPolicyPass": true,
        "stateApplicationPass": null,
        "branchConverged": true
    });
    let rejected = std::panic::catch_unwind(|| rescore_case_record(&record, &parsed, SOL_ID));
    assert!(
        rejected.is_err(),
        "get_meeting without a prior successful search must not retain positive receipts"
    );

    record["toolSteps"] = serde_json::json!(["get_meeting", "search_meetings", "get_meeting"]);
    let rejected = std::panic::catch_unwind(|| rescore_case_record(&record, &parsed, SOL_ID));
    assert!(
        rejected.is_err(),
        "a later valid get must not hide an impossible earlier successful get"
    );
}

#[test]
fn deterministic_oracle_rejects_negated_required_relationship() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "meeting-chat-pl-delta")
        .unwrap();
    let contradicted = "Start ustalono na 3 października. Nina nie odpowiada za dokumentację do 25 września. Cena kolejnego etapu nie została uzgodniona.";
    let score = score_output(contradicted, &[], None, None, None, None, case);
    assert_eq!(score.required_groups_hit, score.required_groups_total);
    assert!(!score.forbidden_pass);
    assert!(!score.case_pass);
}

#[test]
fn controlled_tool_executor_has_staged_read_only_oracle() {
    let premature = ControlledProductExecutor {
        scope: crate::tools::AssistantScope::Full,
        note_drafts: false,
        search_result:
            "- [meeting:synthetic] Synthetic (2026-08-05T09:00:00Z) [id:synthetic] — fixed"
                .to_string(),
        search_terms: vec!["synthetic".to_string()],
        meeting_result: "TITLE: [[Synthetic]]\n\nNOTE:\nfixed".to_string(),
        calls: Mutex::new(Vec::new()),
    };
    assert!(premature
        .run("get_meeting", &serde_json::json!({"meetingId":"synthetic"}))
        .is_err());
    assert_eq!(
        *premature.calls.lock().unwrap(),
        ["failed:get_meeting".to_string()]
    );

    let executor = ControlledProductExecutor {
        scope: crate::tools::AssistantScope::Full,
        note_drafts: false,
        search_result: "[meeting:synthetic] Synthetic".to_string(),
        search_terms: vec!["synthetic".to_string()],
        meeting_result: "[[Synthetic]] fixed".to_string(),
        calls: Mutex::new(Vec::new()),
    };
    let specs = executor.specs();
    assert!(
        specs.len() > 10,
        "Ask must keep the production Full catalog"
    );
    assert!(specs.iter().any(|spec| spec.name == "search_meetings"));
    assert!(specs.iter().any(|spec| spec.name == "get_meeting"));
    assert!(specs.iter().any(|spec| spec.name == "web_search"));
    assert!(!specs.iter().any(|spec| spec.name == "propose_note"));
    assert!(specs.iter().all(|spec| !spec.write));
    assert!(executor
        .run("search_meetings", &serde_json::json!({"query":"unrelated"}))
        .is_err());
    assert_eq!(
        *executor.calls.lock().unwrap(),
        ["failed:search_meetings".to_string()]
    );
    assert_eq!(
        executor
            .run("search_meetings", &serde_json::json!({"query":"synthetic"}))
            .unwrap(),
        "[meeting:synthetic] Synthetic"
    );
    assert_eq!(
        executor
            .run("get_meeting", &serde_json::json!({"meetingId":"synthetic"}))
            .unwrap(),
        "[[Synthetic]] fixed"
    );
    assert!(executor.run("save_note", &serde_json::json!({})).is_err());
    assert_eq!(
        *executor.calls.lock().unwrap(),
        [
            "failed:search_meetings".to_string(),
            "search_meetings".to_string(),
            "get_meeting".to_string(),
            "failed:save_note".to_string(),
        ]
    );

    let semantic = ControlledProductExecutor {
        scope: crate::tools::AssistantScope::Full,
        note_drafts: false,
        search_result: "[meeting:synthetic] Synthetic".to_string(),
        search_terms: vec!["synthetic".to_string()],
        meeting_result: "[[Synthetic]] fixed".to_string(),
        calls: Mutex::new(Vec::new()),
    };
    assert_eq!(
        semantic
            .run("search_semantic", &serde_json::json!({"query":"synthetic"}),)
            .unwrap(),
        "[meeting:synthetic] Synthetic"
    );
    assert_eq!(
        semantic
            .run("get_meeting", &serde_json::json!({"meetingId":"synthetic"}))
            .unwrap(),
        "[[Synthetic]] fixed"
    );
    assert_eq!(
        *semantic.calls.lock().unwrap(),
        ["search_semantic".to_string(), "get_meeting".to_string()]
    );

    let required = vec!["search_meetings".to_string(), "get_meeting".to_string()];
    let allowed = required.clone();
    assert_eq!(
        tool_policy_score(&required, Some(&allowed), &required),
        (Some(100.0), Some(true))
    );
    let mut with_extra = required.clone();
    with_extra.push("web_search".to_string());
    assert_eq!(
        tool_policy_score(&required, Some(&allowed), &with_extra),
        (Some(0.0), Some(false))
    );
    assert_eq!(
        tool_policy_score(&[], Some(&[]), &["propose_note".to_string()]),
        (Some(0.0), Some(false))
    );
    let staged_required = vec!["get_meeting".to_string()];
    let staged_allowed = ["search_meetings", "search_semantic", "get_meeting"]
        .map(str::to_string)
        .to_vec();
    assert_eq!(
        tool_policy_score(
            &staged_required,
            Some(&staged_allowed),
            &["get_meeting".to_string()]
        ),
        (Some(0.0), Some(false))
    );
    assert_eq!(
        tool_policy_score(
            &staged_required,
            Some(&staged_allowed),
            &["get_meeting".to_string(), "search_meetings".to_string()]
        ),
        (Some(0.0), Some(false))
    );
    assert_eq!(
        tool_policy_score(
            &staged_required,
            Some(&staged_allowed),
            &["search_semantic".to_string(), "get_meeting".to_string()]
        ),
        (Some(100.0), Some(true))
    );
}

#[test]
fn cloud_arm_requires_an_explicit_acknowledgement() {
    assert!(!cloud_egress_acknowledged(None));
    assert!(!cloud_egress_acknowledged(Some("true")));
    assert!(!cloud_egress_acknowledged(Some("0")));
    assert!(cloud_egress_acknowledged(Some("1")));
}

#[test]
fn cloud_schedule_is_pairwise_counterbalanced_and_repetition_bound() {
    let first = [QWEN4_ID, QWEN1_ID, SOL_ID].map(ToString::to_string);
    let second = [SOL_ID, QWEN1_ID, QWEN4_ID].map(ToString::to_string);
    assert!(cloud_run_schedule_valid(&first, "1"));
    assert!(cloud_run_schedule_valid(&second, "2"));
    assert!(!cloud_run_schedule_valid(&first, "2"));
    assert!(!cloud_run_schedule_valid(&first, "3"));
}

#[test]
fn final_summary_product_repairs_unterminated_model_frontmatter_before_scoring() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "summary-en-cedar")
        .unwrap();
    let malformed = "---\ntitle: Cedar\ndate: 2026-08-05\ntags: [meeting]\n# Cedar\n\n## Summary\nText\n\n## Decisions\nNone\n\n## Action items\nNone";
    let repaired = summary_product_output(malformed, case);
    assert!(output_format_pass(&repaired, &case.expected));
    assert_eq!(
        repaired.lines().filter(|line| line.trim() == "---").count(),
        2,
        "the assembled product output must contain exactly one YAML document: {repaired}"
    );

    let valid = "---\ntitle: Cedar\ndate: 2026-08-05\ntags: [meeting]\n---\n# Cedar\n\n## Summary\nText\n\n## Decisions\nNone\n\n## Action items\nNone";
    let already_valid = summary_product_output(valid, case);
    assert!(output_format_pass(&already_valid, &case.expected));
}

#[test]
fn summary_oracle_requires_committed_budget_in_decisions_and_accepts_polish_actions_alias() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "summary-pl-kestrel")
        .unwrap();
    let good = "---\ntitle: Kestrel\ndate: 2026-08-05\n---\n# Kestrel\n\n## Podsumowanie\nPilotaż Kestrel w Gdańsku zaczyna się 14 września.\n\n## Decyzje\n- Limit budżetu dla pilotażu wynosi 750 tys. zł.\n\n## Akcje\n- [ ] Piotr — przygotować plan rollbacku do 10 września.\n- [ ] Marta — sprawdzić typ przełącznika legacy; wynik jest niepewny.";
    let missing_decision = good.replace(
        "- Limit budżetu dla pilotażu wynosi 750 tys. zł.",
        "- None recorded",
    );
    let planned_promoted = good.replace(
        "- Limit budżetu dla pilotażu wynosi 750 tys. zł.",
        "- Limit budżetu dla pilotażu wynosi 750 tys. zł.\n- Pilotaż rozpocznie się 14 września.",
    );
    let scoped_date = good.replace(
        "- Limit budżetu dla pilotażu wynosi 750 tys. zł.",
        "- Limit budżetu 750 tys. zł zatwierdzono wyłącznie dla pilotażu zaplanowanego na 14 września.",
    );
    let broadened_budget = good.replace(
        "- Limit budżetu dla pilotażu wynosi 750 tys. zł.",
        "- Limit budżetu 750 tys. zł obejmuje pełne wdrożenie.",
    );
    let faithful_scope_boundary = good.replace(
        "- Limit budżetu dla pilotażu wynosi 750 tys. zł.",
        "- Limit budżetu 750 tys. zł dotyczy pilotażu i nie obejmuje pełnego wdrożenia.",
    );
    let mixed_scope = good.replace(
        "- Limit budżetu dla pilotażu wynosi 750 tys. zł.",
        "- Limit budżetu 750 tys. zł obejmuje pilotaż i pełne wdrożenie.",
    );
    let combined_promotion = good.replace(
        "- Limit budżetu dla pilotażu wynosi 750 tys. zł.",
        "- Zatwierdzono limit budżetu 750 tys. zł dla pilotażu oraz ustalono, że pilotaż rozpocznie się 14 września.",
    );
    let good_score = score_output(good, &[], None, None, None, None, case);
    let bad_score = score_output(&missing_decision, &[], None, None, None, None, case);
    let promoted_score = score_output(&planned_promoted, &[], None, None, None, None, case);
    let scoped_score = score_output(&scoped_date, &[], None, None, None, None, case);
    let broadened_score = score_output(&broadened_budget, &[], None, None, None, None, case);
    let faithful_scope_score =
        score_output(&faithful_scope_boundary, &[], None, None, None, None, case);
    let mixed_scope_score = score_output(&mixed_scope, &[], None, None, None, None, case);
    let combined_score = score_output(&combined_promotion, &[], None, None, None, None, case);
    assert!(good_score.case_pass);
    assert!(scoped_score.case_pass);
    assert!(faithful_scope_score.case_pass);
    assert!(!bad_score.section_pass);
    assert!(!bad_score.forbidden_pass);
    assert!(!bad_score.case_pass);
    assert!(!promoted_score.section_pass);
    assert!(!promoted_score.forbidden_pass);
    assert!(!promoted_score.relation_pass);
    assert!(promoted_score
        .critical_errors
        .iter()
        .any(|error| error.starts_with("forbidden_section:")));
    assert!(!promoted_score.case_pass);
    assert_eq!(
        broadened_score.required_groups_hit,
        broadened_score.required_groups_total
    );
    assert!(broadened_score.forbidden_pass);
    assert!(!broadened_score.relation_pass);
    assert!(!broadened_score.case_pass);
    assert_eq!(
        mixed_scope_score.required_groups_hit,
        mixed_scope_score.required_groups_total
    );
    assert!(!mixed_scope_score.relation_pass);
    assert!(!mixed_scope_score.case_pass);
    assert_eq!(
        combined_score.required_groups_hit,
        combined_score.required_groups_total
    );
    assert!(!combined_score.section_pass);
    assert!(!combined_score.forbidden_pass);
    assert!(!combined_score.case_pass);
}

#[test]
fn summary_oracle_rejects_a_positive_vendor_switch_after_non_approval() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "summary-en-cedar")
        .unwrap();
    let good = "---\ntitle: Cedar\ndate: 2026-08-05\n---\n# Cedar\n\n## Summary\nProject Cedar uses a phased rollout. The first cohort starts August 20. Payment latency remains an open question.\n\n## Decisions\n- Roll out in cohorts of ten percent.\n\n## Action items\n- [ ] Rowan — finish the rollback checklist by August 18.";
    let wrong = good.replace(
        "Payment latency remains an open question.",
        "Payment latency remains an open question. Proceed with a vendor switch.",
    );
    let good_score = score_output(good, &[], None, None, None, None, case);
    let wrong_score = score_output(&wrong, &[], None, None, None, None, case);
    assert!(good_score.case_pass);
    assert_eq!(
        wrong_score.required_groups_hit,
        wrong_score.required_groups_total
    );
    assert!(wrong_score.closed_world_pass);
    assert!(!wrong_score.relation_pass);
    assert!(!wrong_score.case_pass);
}

#[test]
fn shorten_oracle_uses_token_boundaries_for_preamble_forbiddens() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-shorten-en")
        .unwrap();
    let natural = "There is an Aurora beta launch on November 12; Morgan sends the customer notice by November 8; budget ceiling remains $420,000.";
    let score = score_output(natural, &[], None, None, None, None, case);
    assert!(score.case_pass);
    assert!(!contains_forbidden_phrase(natural, "here is"));
    assert!(contains_forbidden_phrase(
        "Here is the shortened version",
        "here is"
    ));
}

#[test]
fn action_oracle_rejects_paraphrased_meta_tasks_by_structure() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-actions-pl")
        .unwrap();
    let good = "- [ ] Iga — wysłać plan testów do 6 listopada";
    let bad =
        "- [ ] Iga — wysłać plan testów do 6 listopada\n- [ ] Omówić później wybór nowego partnera";
    let good_score = score_output(good, &[], None, None, None, None, case);
    let bad_score = score_output(bad, &[], None, None, None, None, case);
    assert!(good_score.case_pass);
    assert!(!bad_score.case_pass);
    assert!(!bad_score.constraint_pass);

    let prose_tail =
        "- [ ] Iga — wysłać plan testów do 6 listopada\nTo jest dodatkowe wyjaśnienie.";
    let prose_score = score_output(prose_tail, &[], None, None, None, None, case);
    assert!(!prose_score.format_pass);
    assert!(!prose_score.case_pass);
}

#[test]
fn action_oracle_rejects_past_completion_rewritten_as_an_open_task() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-actions-pl")
        .unwrap();
    for past_completion in [
        "- [ ] Iga wysłała plan testów do 6 listopada",
        "- [ ] Iga — ukończyła plan testów do 6 listopada",
    ] {
        let score = score_output(past_completion, &[], None, None, None, None, case);
        assert!(!score.forbidden_pass);
        assert!(!score.case_pass);
    }
}

#[test]
fn action_oracle_rejects_english_past_completion_and_open_proposal_promotion() {
    let parsed = manifest();
    let leah = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-actions-en-holdout")
        .unwrap();
    let completed = "- [ ] Leah — sent the accessibility test matrix by December 2";
    let completed_score = score_output(completed, &[], None, None, None, None, leah);
    assert_eq!(
        completed_score.required_groups_hit,
        completed_score.required_groups_total
    );
    assert!(!completed_score.forbidden_pass);
    assert!(!completed_score.case_pass);

    let harbor = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-decisions-en")
        .unwrap();
    let promoted = "- Release Harbor in cohorts of 10 percent.\n- Approve the mobile app redesign.";
    let promoted_score = score_output(promoted, &[], None, None, None, None, harbor);
    assert_eq!(
        promoted_score.required_groups_hit,
        promoted_score.required_groups_total
    );
    assert!(promoted_score.forbidden_pass);
    assert!(!promoted_score.relation_pass);
    assert!(!promoted_score.case_pass);
}

#[test]
fn fact_check_oracle_preserves_pilot_location_scope() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-fact-check-pl")
        .unwrap();
    let broadened = "Projekt Orchid nie wystartuje 10 listopada; wystartuje 12 listopada. Budżet nie został zatwierdzony, więc wybrany tekst jest sprzeczny ze źródłem.";
    let scoped = "Źródło wskazuje sprzeczność: pilotaż Orchid w Krakowie startuje 12 listopada, nie 10 listopada. Budżet nie został zatwierdzony.";
    let scoped_plus_broadened = "Źródło wskazuje sprzeczność: pilotaż Orchid w Krakowie startuje 12 listopada, nie 10 listopada. Projekt Orchid wystartuje 12 listopada. Budżet nie został zatwierdzony.";
    let broadened_score = score_output(broadened, &[], None, None, None, None, case);
    let scoped_score = score_output(scoped, &[], None, None, None, None, case);
    let mixed_score = score_output(scoped_plus_broadened, &[], None, None, None, None, case);
    assert!(!broadened_score.case_pass);
    assert!(!broadened_score.relation_pass);
    assert!(scoped_score.case_pass);
    assert_eq!(
        mixed_score.required_groups_hit,
        mixed_score.required_groups_total
    );
    assert!(!mixed_score.relation_pass);
    assert!(!mixed_score.case_pass);
}

#[test]
fn ask_oracle_scores_code_owned_provenance_separately_from_inline_text() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "ask-vault-pl-orchid")
        .unwrap();
    let answer = "**Ustalenia:** Start pilotażu w Krakowie 12 listopada. Iga odpowiada za incident playbook do 5 listopada. Budżet nie został zatwierdzony i pozostaje otwarty.";
    let with_receipt = score_output(
        answer,
        &["[[Orchid launch]]".to_string()],
        None,
        Some(true),
        None,
        None,
        case,
    );
    let without_receipt = score_output(answer, &[], None, Some(true), None, None, case);
    assert!(with_receipt.case_pass);
    assert!(!without_receipt.case_pass);
    assert!(!without_receipt.provenance_pass);

    let non_converged = score_output(
        answer,
        &["[[Orchid launch]]".to_string()],
        None,
        Some(true),
        None,
        Some(false),
        case,
    );
    assert!(!non_converged.branch_convergence_pass);
    assert!(non_converged
        .critical_errors
        .iter()
        .any(|error| error == "branch_non_converged"));
    assert!(!non_converged.case_pass);
}

#[test]
fn ask_oracle_accepts_natural_polish_launch_synonym_from_frozen_output() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "ask-vault-pl-orchid")
        .unwrap();
    let answer = "**Pilot Orchid rusza w Krakowie 12 listopada; incident playbook należy do Igi, a budżet pozostaje niezatwierdzony.**\n\n- **Start projektu:** pilotaż w Krakowie 12 listopada.\n- **Incident playbook:** odpowiada Iga; termin przygotowania to 5 listopada.\n- **Budżet:** nie został zatwierdzony — temat pozostaje otwarty.\n\nŹródło: [[Orchid launch]]";
    let score = score_output(
        answer,
        &["[[Orchid launch]]".to_string()],
        None,
        Some(true),
        None,
        Some(true),
        case,
    );

    assert!(score.relation_pass);
    assert!(score.case_pass);
}

#[test]
fn summary_oracle_does_not_treat_modal_may_as_a_month_in_frozen_output() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "summary-en-cedar")
        .unwrap();
    let answer = "---\ntitle: \"Project Cedar Phased Rollout\"\ndate: 2026-08-05\nduration_minutes: 3\ntags: [meeting, project-cedar]\nparticipants: [Rowan]\n---\n\n# Project Cedar Phased Rollout\n\n## Summary\n\nProject Cedar will use a phased rollout, expanding by 10% of accounts per cohort starting August 20. Rowan will complete the rollback checklist before launch. The cause of payment latency remains unconfirmed, and no vendor change was approved.\n\n## Key points\n\n- The first rollout cohort starts on August 20.\n- PCI logging might be related to payment latency, but this has not been confirmed.\n- The cause of payment latency remains an open question.\n- A vendor change was not approved.\n\n## Decisions\n\n- Project Cedar will roll out in phases, covering 10% of accounts at a time.\n- The first cohort will start on August 20.\n- No vendor change was approved.\n\n## Action items\n\n- [ ] Rowan — Complete the rollback checklist by August 18.\n\n## Notes\n\n- Payment latency may be related to PCI logging, but its cause remains unresolved.";
    let score = score_output(answer, &[], None, None, None, None, case);

    assert!(!month_tokens("Payment latency may be related").contains("may"));
    assert!(!month_tokens("May I ask a question?").contains("may"));
    assert!(month_tokens("The launch is May 8").contains("may"));
    assert!(month_tokens("The launch is 8 May").contains("may"));
    assert!(month_tokens("The launch is in May").contains("may"));
    assert!(month_tokens("Finish by May").contains("may"));
    assert!(month_tokens("The launch is May 2027").contains("may"));
    assert!(score.closed_world_pass);
    assert!(score.case_pass);
}

#[test]
fn fact_check_oracle_accepts_code_owned_citation_number_in_frozen_output() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-fact-check-pl")
        .unwrap();
    let answer = "Projekt Orchid wystartuje 10 listopada – CONTRADICTS [1] (pilotaż Orchid startuje 12 listopada w Krakowie).  \nBudżet został zatwierdzony – CONTRADICTS [1] (Budżet NIE został zatwierdzony i pozostaje tematem otwartym).";
    let score = score_output(answer, &[], None, None, None, None, case);

    assert_eq!(score.required_groups_hit, score.required_groups_total);
    assert!(score.closed_world_pass);
    assert!(score.case_pass);

    for unsupported_number in [answer.replace("[1]", "[2]"), answer.replace("[1]", "1")] {
        let unsupported_score =
            score_output(&unsupported_number, &[], None, None, None, None, case);
        assert!(!unsupported_score.closed_world_pass);
        assert!(!unsupported_score.case_pass);
    }
}

#[test]
fn ask_oracle_allows_omitted_unasked_deadlines_but_rejects_wrong_volunteered_ones() {
    let parsed = manifest();
    let orchid = parsed
        .cases
        .iter()
        .find(|case| case.id == "ask-vault-pl-orchid")
        .unwrap();
    let orchid_provenance = ["[[Orchid launch]]".to_string()];
    let orchid_omitted = "**Ustalenia:** Pilotaż w Krakowie startuje 12 listopada. Iga odpowiada za incident playbook. Budżet nie został zatwierdzony.";
    let orchid_wrong = "**Ustalenia:** Pilotaż w Krakowie startuje 12 listopada. Iga odpowiada za incident playbook do 12 listopada. Budżet nie został zatwierdzony.";
    let omitted_score = score_output(
        orchid_omitted,
        &orchid_provenance,
        None,
        Some(true),
        None,
        Some(true),
        orchid,
    );
    let orchid_wrong_score = score_output(
        orchid_wrong,
        &orchid_provenance,
        None,
        Some(true),
        None,
        Some(true),
        orchid,
    );
    assert!(omitted_score.case_pass);
    assert_eq!(
        orchid_wrong_score.required_groups_hit,
        orchid_wrong_score.required_groups_total
    );
    assert!(!orchid_wrong_score.relation_pass);
    assert!(!orchid_wrong_score.case_pass);

    let quartz = parsed
        .cases
        .iter()
        .find(|case| case.id == "ask-vault-en-quartz-holdout")
        .unwrap();
    let quartz_provenance = ["[[Quartz review]]".to_string()];
    let quartz_omitted = "**Answer:** Quartz launches January 14. Theo owns the rollback drill. The security exception was rejected.";
    let quartz_wrong = "**Answer:** Quartz launches January 14. Theo owns the rollback drill due January 14. The security exception was rejected.";
    let quartz_omitted_score = score_output(
        quartz_omitted,
        &quartz_provenance,
        None,
        Some(true),
        None,
        Some(true),
        quartz,
    );
    let quartz_wrong_score = score_output(
        quartz_wrong,
        &quartz_provenance,
        None,
        Some(true),
        None,
        Some(true),
        quartz,
    );
    assert!(quartz_omitted_score.case_pass);
    assert_eq!(
        quartz_wrong_score.required_groups_hit,
        quartz_wrong_score.required_groups_total
    );
    assert!(!quartz_wrong_score.relation_pass);
    assert!(!quartz_wrong_score.case_pass);
}

#[test]
fn relation_oracle_rejects_owner_deadline_swaps() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "meeting-chat-pl-delta")
        .unwrap();
    let swapped = "Start projektu przypada 25 września. Nina odpowiada za dokumentację do 3 października. Cena kolejnego etapu nie została uzgodniona.";
    let score = score_output(swapped, &[], None, None, None, None, case);
    assert_eq!(score.required_groups_hit, score.required_groups_total);
    assert!(!score.relation_pass);
    assert!(!score.case_pass);
}

#[test]
fn relation_oracle_rejects_dates_attached_to_the_wrong_asked_fact() {
    let parsed = manifest();
    let delta = parsed
        .cases
        .iter()
        .find(|case| case.id == "meeting-chat-pl-delta")
        .unwrap();
    let delta_wrong = "Termin startu projektu Delta to 25 września. Nina odpowiada za dokumentację do 25 września. Cena kolejnego etapu nie została uzgodniona; wrócimy do niej 3 października.";
    let delta_score = score_output(delta_wrong, &[], None, None, None, None, delta);
    assert_eq!(
        delta_score.required_groups_hit,
        delta_score.required_groups_total
    );
    assert!(!delta_score.relation_pass);
    assert!(!delta_score.case_pass);

    let fjord = parsed
        .cases
        .iter()
        .find(|case| case.id == "meeting-chat-en-fjord-holdout")
        .unwrap();
    let fjord_wrong = "Fjord enters beta on November 29. Mei owns the support runbook due November 29. The enterprise price is still open; review it on December 4.";
    let fjord_score = score_output(fjord_wrong, &[], None, None, None, None, fjord);
    assert_eq!(
        fjord_score.required_groups_hit,
        fjord_score.required_groups_total
    );
    assert!(!fjord_score.relation_pass);
    assert!(!fjord_score.case_pass);
}

#[test]
fn live_bullets_oracle_binds_the_beta_date_instead_of_counting_a_keyword_bag() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "live-bullets-pl-polaris")
        .unwrap();
    let wrong = "- [decyzja]: Beta Polaris rusza 8 listopada.\n- [zadanie]: Lena przygotuje listę testerów do 8 listopada.\n- [termin]: Przegląd odbędzie się 15 listopada.";
    let score = score_output(wrong, &[], None, None, None, None, case);
    assert_eq!(score.required_groups_hit, score.required_groups_total);
    assert!(score.format_pass);
    assert!(!score.relation_pass);
    assert!(!score.case_pass);
}

#[test]
fn relation_oracle_rejects_shorten_date_swaps_and_cross_semicolon_keyword_bags() {
    let parsed = manifest();
    let shorten = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-shorten-en")
        .unwrap();
    for swapped in [
        "Aurora beta will launch November 8; Morgan will send the customer notice by November 12; budget ceiling remains 420 thousand dollars.",
        "Aurora beta will launch November 8, and Morgan will send the customer notice by November 12, while the budget remains 420 thousand dollars.",
    ] {
        let swapped_score = score_output(swapped, &[], None, None, None, None, shorten);
        assert_eq!(
            swapped_score.required_groups_hit,
            swapped_score.required_groups_total
        );
        assert!(!swapped_score.relation_pass, "date swap passed: {swapped}");
        assert!(!swapped_score.case_pass);
    }

    let fact_check = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-fact-check-pl")
        .unwrap();
    let split_coordinates = "Pilotaż Orchid w Krakowie; data 12 listopada. Budżet nie został zatwierdzony, więc tekst jest sprzeczny ze źródłem.";
    let split_score = score_output(split_coordinates, &[], None, None, None, None, fact_check);
    assert!(!split_score.relation_pass);
    assert!(!split_score.case_pass);
}

#[test]
fn ask_oracle_rejects_negated_rejection_even_when_keywords_are_present() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "ask-vault-en-quartz-holdout")
        .unwrap();
    let good = "**Answer:** Quartz launches January 14. Theo owns the rollback drill. The security exception was rejected.";
    let negated = "**Answer:** Quartz launches January 14. Theo owns the rollback drill. The security exception was not rejected.";
    let launch_negated = "**Answer:** Quartz does not launch January 14. Theo owns the rollback drill. The security exception was rejected.";
    let provenance = ["[[Quartz review]]".to_string()];
    let good_score = score_output(good, &provenance, None, Some(true), None, Some(true), case);
    let bad_score = score_output(
        negated,
        &provenance,
        None,
        Some(true),
        None,
        Some(true),
        case,
    );
    let launch_bad_score = score_output(
        launch_negated,
        &provenance,
        None,
        Some(true),
        None,
        Some(true),
        case,
    );
    assert!(good_score.case_pass);
    assert_eq!(
        bad_score.required_groups_hit,
        bad_score.required_groups_total
    );
    assert!(!bad_score.forbidden_pass);
    assert!(!bad_score.case_pass);
    assert_eq!(
        launch_bad_score.required_groups_hit,
        launch_bad_score.required_groups_total
    );
    assert!(!launch_bad_score.forbidden_pass);
    assert!(!launch_bad_score.case_pass);
}

#[test]
fn ask_oracle_rejects_launch_polarity_reversal_scoped_to_the_pilot() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "ask-vault-pl-orchid")
        .unwrap();
    let correct = "**Ustalenia:** Pilotaż w Krakowie startuje 12 listopada. Iga odpowiada za incident playbook. Budżet nie został zatwierdzony.";
    let reversed = "**Ustalenia:** Pilotaż w Krakowie 12 listopada nie został zatwierdzony. Iga odpowiada za incident playbook. Budżet nie został zatwierdzony.";
    let parenthetical = "**Ustalenia:** Pilotaż w Krakowie startuje 12 listopada (budżet nie został zatwierdzony). Iga odpowiada za incident playbook.";
    let em_dash_reversed = "**Ustalenia:** Nie zdecydowano o starcie pilotażu Orchid w Krakowie 12 listopada — budżet nie został zatwierdzony. Iga odpowiada za incident playbook.";
    let provenance = ["[[Orchid launch]]".to_string()];
    let correct_score = score_output(
        correct,
        &provenance,
        None,
        Some(true),
        None,
        Some(true),
        case,
    );
    let reversed_score = score_output(
        reversed,
        &provenance,
        None,
        Some(true),
        None,
        Some(true),
        case,
    );
    let parenthetical_score = score_output(
        parenthetical,
        &provenance,
        None,
        Some(true),
        None,
        Some(true),
        case,
    );
    let em_dash_score = score_output(
        em_dash_reversed,
        &provenance,
        None,
        Some(true),
        None,
        Some(true),
        case,
    );
    assert!(correct_score.case_pass);
    assert!(parenthetical_score.case_pass);
    assert_eq!(
        reversed_score.required_groups_hit,
        reversed_score.required_groups_total
    );
    assert!(reversed_score.forbidden_pass);
    assert!(!reversed_score.relation_pass);
    assert!(reversed_score
        .critical_errors
        .iter()
        .any(|error| error == "relation"));
    assert!(!reversed_score.case_pass);
    assert_eq!(
        em_dash_score.required_groups_hit,
        em_dash_score.required_groups_total
    );
    assert!(!em_dash_score.relation_pass);
    assert!(!em_dash_score.case_pass);
}

#[test]
fn summary_oracle_rejects_broadening_a_stage_only_budget_to_the_rollout() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "summary-pl-lumen-holdout")
        .unwrap();
    let broadened = "---\ntitle: Lumen\ndate: 2026-08-05\n---\n# Lumen\n\n## Podsumowanie\nPierwszy etap zaczyna się 22 października i obejmuje 15% kont. Limit 310 tys. zł dla całego wdrożenia. DNS to możliwa, niepotwierdzona hipoteza.\n\n## Decyzje\n- Limit 310 tys. zł dla całego wdrożenia.\n\n## Zadania\n- [ ] Sara — dostarczyć checklistę do 18 października.";
    let score = score_output(broadened, &[], None, None, None, None, case);
    assert!(score.required_groups_hit < score.required_groups_total);
    assert!(!score.forbidden_pass);
    assert!(!score.case_pass);
}

#[test]
fn summary_oracle_binds_the_action_owner_to_the_task_slot() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "summary-pl-lumen-holdout")
        .unwrap();
    let valid = "---\ntitle: Lumen\ndate: 2026-08-05\n---\n# Lumen\n\n## Podsumowanie\nPierwszy etap projektu Lumen zaczyna się 22 października i obejmuje 15% kont. Limit 310 tys. zł dotyczy wyłącznie pierwszego etapu. DNS pozostaje niepotwierdzoną hipotezą.\n\n## Decyzje\n- Limit 310 tys. zł zatwierdzono wyłącznie dla pierwszego etapu.\n\n## Zadania\n- [ ] Sara — dostarczyć checklistę migracji do 18 października.";
    let wrong_owner = valid.replace(
        "- [ ] Sara — dostarczyć checklistę migracji do 18 października.",
        "- [ ] others — dopilnować dostarczenia checklisty migracji do 18 października przez osobę określoną jako Sara.",
    );
    let conflicting_owner = valid.replace(
        "- [ ] Sara — dostarczyć checklistę migracji do 18 października.",
        "- [ ] Sara — dostarczyć checklistę migracji do 18 października.\n- [ ] others — dopilnować dostarczenia checklisty migracji do 18 października przez osobę określoną jako Sara.",
    );
    let wrong_owner_without_separator = valid.replace(
        "- [ ] Sara — dostarczyć checklistę migracji do 18 października.",
        "- [ ] others, nie Sara, przygotuje checklistę migracji do 18 października.",
    );
    let valid_score = score_output(valid, &[], None, None, None, None, case);
    let wrong_score = score_output(&wrong_owner, &[], None, None, None, None, case);
    let conflicting_score = score_output(&conflicting_owner, &[], None, None, None, None, case);
    let no_separator_score = score_output(
        &wrong_owner_without_separator,
        &[],
        None,
        None,
        None,
        None,
        case,
    );
    assert!(valid_score.case_pass);
    assert_eq!(
        wrong_score.required_groups_hit,
        wrong_score.required_groups_total
    );
    assert!(relation_requirements_pass(
        &wrong_owner,
        &case.expected.relation_requirements
    ));
    assert!(!wrong_score.relation_pass);
    assert!(!wrong_score.case_pass);
    assert!(!conflicting_score.relation_pass);
    assert!(!conflicting_score.case_pass);
    assert!(relation_requirements_pass(
        &wrong_owner_without_separator,
        &case.expected.relation_requirements
    ));
    assert!(!no_separator_score.relation_pass);
    assert!(!no_separator_score.case_pass);
}

#[test]
fn relation_oracle_keeps_numeric_dates_intact() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "meeting-chat-pl-delta")
        .unwrap();
    let output = "Nina odpowiada za dokumentację do 25.09.2026. Start przypada 3 października, a cena nie została uzgodniona.";
    assert!(relation_requirements_pass(
        output,
        &case.expected.relation_requirements
    ));

    let lumen = parsed
        .cases
        .iter()
        .find(|case| case.id == "summary-pl-lumen-holdout")
        .unwrap();
    assert_eq!(
        relation_units("Limit 310 tys. zł dotyczy wyłącznie pierwszego etapu. Sara dostarczy checklistę do 18 października."),
        [
            "Limit 310 tys. zł dotyczy wyłącznie pierwszego etapu",
            "Sara dostarczy checklistę do 18 października."
        ]
    );
    assert_eq!(
        relation_units("Limit 310 tys.\nSara dostarczy checklistę."),
        ["Limit 310 tys", "Sara dostarczy checklistę."]
    );
    let complete = "Pierwszy etap zaczyna się 22 października. Limit 310 tys. zł dotyczy wyłącznie pierwszego etapu. Sara dostarczy checklistę do 18 października.";
    let units = relation_units(complete);
    assert_eq!(
        units,
        [
            "Pierwszy etap zaczyna się 22 października",
            "Limit 310 tys. zł dotyczy wyłącznie pierwszego etapu",
            "Sara dostarczy checklistę do 18 października."
        ]
    );
    for (index, requirement) in lumen.expected.relation_requirements.iter().enumerate() {
        assert!(
            relation_present(&units, requirement),
            "Lumen relation requirement {index} did not match {units:?}"
        );
    }
}

#[test]
fn closed_world_oracle_rejects_new_actor_date_number_and_link() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "ask-vault-en-quartz-holdout")
        .unwrap();
    let hallucinated = "**Answer:** Quartz launches January 14. Theo owns the rollback drill by January 9. The security exception was rejected. Legal also approved a production freeze for February 2 in [[Freeze plan]].";
    let score = score_output(
        hallucinated,
        &["[[Quartz review]]".to_string()],
        None,
        Some(true),
        None,
        None,
        case,
    );
    assert!(!score.closed_world_pass);
    assert!(score
        .critical_errors
        .iter()
        .any(|error| error == "closed_world"));
    assert!(!score.case_pass);
}

#[test]
fn closed_world_oracle_rejects_actor_when_reference_has_no_people() {
    let parsed = manifest();
    let case = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-fact-check-pl")
        .unwrap();
    let hallucinated = "Legal approved the corrected date 12 listopada. Budżet nie został zatwierdzony; źródło pokazuje sprzeczność.";
    let score = score_output(hallucinated, &[], None, None, None, None, case);
    assert!(!score.closed_world_pass);
    assert!(!score.case_pass);
}

#[test]
fn closed_world_actor_check_accepts_project_subjects_and_verb_first_tasks() {
    let parsed = manifest();
    let shorten = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-shorten-en")
        .unwrap();
    let shorten_reference = expected_reference_text(shorten);
    assert!(suspicious_actor_pass(
        "Aurora beta will launch November 12; Morgan will send the customer notice by November 8.",
        &shorten.expected.allowed_entities,
        &shorten_reference,
    ));

    let actions = parsed
        .cases
        .iter()
        .find(|case| case.id == "note-popup-actions-en-holdout")
        .unwrap();
    let actions_reference = expected_reference_text(actions);
    assert!(suspicious_actor_pass(
        "- [ ] Send the accessibility test matrix by December 2 — Leah",
        &actions.expected.allowed_entities,
        &actions_reference,
    ));

    let fjord = parsed
        .cases
        .iter()
        .find(|case| case.id == "meeting-chat-en-fjord-holdout")
        .unwrap();
    let fjord_reference = expected_reference_text(fjord);
    assert!(suspicious_actor_pass(
        "No, the enterprise price has not been approved.",
        &fjord.expected.allowed_entities,
        &fjord_reference,
    ));
}
