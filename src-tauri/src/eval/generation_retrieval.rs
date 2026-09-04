//! Content-free, replayable retrieval evidence for the local-vs-cloud generation benchmark.
//!
//! This lane is intentionally independent of either generation arm. It seeds the committed,
//! synthetic RAG corpus into a fresh SQLCipher database, indexes it with the selected REAL
//! persistence embedder, then calls the same visibility-gated meeting readers used by Ask Brain:
//! keyword FTS, semantic KNN, and production hybrid fusion. The serialized artifact contains only
//! domain-separated hashes of query payloads and meeting ids; it cannot represent query text,
//! titles, snippets, database paths, or SQLCipher keys.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::fs::{File, OpenOptions, Permissions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rand::rngs::OsRng;
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{AppError, Result};
use crate::eval::{ndcg_at_k, recall_at_k, reciprocal_rank, LabeledSet};
use crate::storage::Db;

const RETRIEVAL_K: usize = 5;
const PRODUCT_CANDIDATE_LIMIT: i64 = 20;
const MODE_FTS: &str = "fts_product";
const MODE_SEMANTIC: &str = "semantic_product_floor";
const MODE_HYBRID: &str = "hybrid_product";
const MODES: [&str; 3] = [MODE_FTS, MODE_SEMANTIC, MODE_HYBRID];

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetrievalMetricEvidence {
    pub(crate) recall_at_k: f64,
    pub(crate) ndcg_at_k: f64,
    pub(crate) reciprocal_rank: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetrievalAggregateEvidence {
    pub(crate) recall_at_k: f64,
    pub(crate) ndcg_at_k: f64,
    pub(crate) mrr: f64,
    pub(crate) queries: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetrievalCaseEvidence {
    pub(crate) case_id: String,
    pub(crate) language: String,
    pub(crate) query_payload_sha256: String,
    pub(crate) expected_meetings: usize,
    pub(crate) expected_id_hashes: Vec<String>,
    pub(crate) rankings: BTreeMap<String, Vec<String>>,
    pub(crate) metrics: BTreeMap<String, RetrievalMetricEvidence>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetrievalModelFileEvidence {
    pub(crate) filename: String,
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetrievalQualityEvidence {
    pub(crate) required: bool,
    pub(crate) surface: &'static str,
    pub(crate) attribution: &'static str,
    pub(crate) fixture_sha256: String,
    pub(crate) corpus_source_sha256: String,
    pub(crate) embedder_id: String,
    pub(crate) real_embedder: bool,
    pub(crate) model_files: Vec<RetrievalModelFileEvidence>,
    pub(crate) anchor_date: &'static str,
    pub(crate) k: usize,
    pub(crate) candidate_limit: i64,
    pub(crate) cosine_floor: f32,
    pub(crate) cases: Vec<RetrievalCaseEvidence>,
    pub(crate) aggregates: BTreeMap<String, BTreeMap<String, RetrievalAggregateEvidence>>,
    pub(crate) visibility_gate: &'static str,
    pub(crate) temporary_database_cleaned: bool,
}

/// A private, whole-DB-encrypted scratch database. `Drop` is deliberately best-effort and removes
/// SQLite sidecars too, so early `?` returns and unwinding cannot strand synthetic plaintext.
struct PrivateSqlCipherDb {
    db: Option<Db>,
    key_hex: Option<Zeroizing<String>>,
    directory: PathBuf,
    path: PathBuf,
    cleaned: bool,
}

impl PrivateSqlCipherDb {
    fn create() -> Result<Self> {
        let mut nonce = [0_u8; 16];
        OsRng.fill_bytes(&mut nonce);
        let directory = std::env::temp_dir().join(format!(
            "murmur-quality-retrieval-{}-{}",
            std::process::id(),
            hex_digest(&nonce)
        ));
        std::fs::create_dir(&directory).map_err(|error| {
            AppError::Storage(format!("create private retrieval directory: {error}"))
        })?;
        std::fs::set_permissions(&directory, Permissions::from_mode(0o700)).map_err(|error| {
            AppError::Storage(format!("protect private retrieval directory: {error}"))
        })?;

        let path = directory.join("retrieval.sqlite");
        let precreated = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) => {
                let _ = cleanup_sqlite_files(&directory, &path);
                return Err(AppError::Storage(format!(
                    "pre-create private retrieval database: {error}"
                )));
            }
        };
        drop(precreated);
        let mut raw_key = [0_u8; 32];
        OsRng.fill_bytes(&mut raw_key);
        let key_hex = Zeroizing::new(hex_digest(&raw_key));
        raw_key.zeroize();

        let db = match Db::open_with_key(&path, key_hex.as_str()) {
            Ok(db) => db,
            Err(error) => {
                let _ = cleanup_sqlite_files(&directory, &path);
                return Err(error);
            }
        };
        Ok(Self {
            db: Some(db),
            key_hex: Some(key_hex),
            directory,
            path,
            cleaned: false,
        })
    }

    fn db(&self) -> Result<&Db> {
        self.db.as_ref().ok_or_else(|| {
            AppError::Storage("private retrieval database is already closed".to_string())
        })
    }

    fn close(&mut self) {
        self.db.take();
        self.key_hex.take();
    }

    fn finish(mut self) -> Result<bool> {
        self.close();
        cleanup_sqlite_files(&self.directory, &self.path)?;
        self.cleaned = true;
        Ok(true)
    }
}

impl Drop for PrivateSqlCipherDb {
    fn drop(&mut self) {
        self.close();
        if !self.cleaned {
            let _ = cleanup_sqlite_files(&self.directory, &self.path);
        }
    }
}

fn cleanup_sqlite_files(directory: &Path, path: &Path) -> Result<()> {
    let mut first_error = None;
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
        PathBuf::from(format!("{}-journal", path.display())),
    ] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                first_error.get_or_insert_with(|| {
                    AppError::Storage(format!(
                        "remove private retrieval database artifact: {error}"
                    ))
                });
            }
        }
    }
    match std::fs::remove_dir(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            first_error.get_or_insert_with(|| {
                AppError::Storage(format!("remove private retrieval directory: {error}"))
            });
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

struct RealEmbedderCacheRelease;

impl Drop for RealEmbedderCacheRelease {
    fn drop(&mut self) {
        crate::embed::release_real_embedder_cache();
    }
}

struct RealEmbedTestEnv {
    previous: Option<std::ffi::OsString>,
}

impl RealEmbedTestEnv {
    fn enable() -> Self {
        let previous = std::env::var_os("MURMUR_TEST_REAL_EMBED");
        std::env::set_var("MURMUR_TEST_REAL_EMBED", "1");
        Self { previous }
    }
}

impl Drop for RealEmbedTestEnv {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("MURMUR_TEST_REAL_EMBED", value),
            None => std::env::remove_var("MURMUR_TEST_REAL_EMBED"),
        }
    }
}

fn real_embed_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn framed_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hex_digest(hasher.finalize().as_slice())
}

fn meeting_id_hash(id: &str) -> String {
    framed_hash(&["murmur-retrieval-meeting-id-v1", id])
}

fn query_payload_hash(language: &str, query: &str, expected_ids: &[String]) -> String {
    let mut parts = vec!["murmur-retrieval-case-payload-v2", language, query];
    parts.extend(expected_ids.iter().map(String::as_str));
    framed_hash(&parts)
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .map_err(|error| AppError::Storage(format!("open evidence source file: {error}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| AppError::Storage(format!("hash evidence source file: {error}")))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn metric_for(ranked: &[String], expected: &[String]) -> RetrievalMetricEvidence {
    RetrievalMetricEvidence {
        recall_at_k: recall_at_k(ranked, expected, RETRIEVAL_K),
        ndcg_at_k: ndcg_at_k(ranked, expected, RETRIEVAL_K),
        reciprocal_rank: reciprocal_rank(ranked, expected),
    }
}

fn replay_case_metrics(
    case: &RetrievalCaseEvidence,
) -> Result<BTreeMap<String, RetrievalMetricEvidence>> {
    let mut metrics = BTreeMap::new();
    for mode in MODES {
        let ranked = case.rankings.get(mode).ok_or_else(|| {
            AppError::InvalidArg(format!("retrieval case missing ranking mode {mode}"))
        })?;
        metrics.insert(
            mode.to_string(),
            metric_for(ranked, &case.expected_id_hashes),
        );
    }
    if case.rankings.len() != MODES.len() {
        return Err(AppError::InvalidArg(
            "retrieval case contains an unknown ranking mode".to_string(),
        ));
    }
    Ok(metrics)
}

fn validate_case_metrics(case: &RetrievalCaseEvidence) -> Result<()> {
    if case.expected_meetings != case.expected_id_hashes.len() {
        return Err(AppError::InvalidArg(format!(
            "retrieval expected-id count mismatch for {}",
            case.case_id
        )));
    }
    if replay_case_metrics(case)? != case.metrics {
        return Err(AppError::InvalidArg(format!(
            "retrieval metric replay mismatch for {}",
            case.case_id
        )));
    }
    Ok(())
}

/// Deterministic artifact-integrity check: numeric rows must replay from the retained hashed
/// rankings, and every aggregate must replay from those case rows. No model or database is needed.
pub(crate) fn validate_evidence_replay(evidence: &RetrievalQualityEvidence) -> Result<()> {
    if !evidence.required
        || !evidence.real_embedder
        || !evidence.temporary_database_cleaned
        || evidence.surface != "ask_vault_retrieval"
        || evidence.attribution != "independent_synthetic_retrieval_lane_not_generation_quality"
        || evidence.anchor_date != crate::eval::corpus::CORPUS_ANCHOR_DATE
        || evidence.k != RETRIEVAL_K
        || evidence.candidate_limit != PRODUCT_CANDIDATE_LIMIT
        || evidence.cosine_floor != crate::embed::KNN_SEARCH_COSINE_FLOOR
    {
        return Err(AppError::InvalidArg(
            "retrieval metadata does not match the production evidence profile".to_string(),
        ));
    }
    if evidence.cases.is_empty() {
        return Err(AppError::InvalidArg(
            "retrieval evidence contains no cases".to_string(),
        ));
    }
    let mut case_ids = HashSet::new();
    for case in &evidence.cases {
        if !case_ids.insert(case.case_id.as_str()) {
            return Err(AppError::InvalidArg(
                "retrieval evidence contains duplicate case ids".to_string(),
            ));
        }
        validate_case_metrics(case)?;
    }
    if aggregates_from_cases(&evidence.cases)? != evidence.aggregates {
        return Err(AppError::InvalidArg(
            "retrieval aggregate replay mismatch".to_string(),
        ));
    }
    Ok(())
}

fn aggregate_cases(
    cases: &[RetrievalCaseEvidence],
) -> Result<BTreeMap<String, RetrievalAggregateEvidence>> {
    let mut output = BTreeMap::new();
    for mode in MODES {
        if cases.is_empty() {
            output.insert(
                mode.to_string(),
                RetrievalAggregateEvidence {
                    recall_at_k: 0.0,
                    ndcg_at_k: 0.0,
                    mrr: 0.0,
                    queries: 0,
                },
            );
            continue;
        }
        let mut recall = 0.0;
        let mut ndcg = 0.0;
        let mut mrr = 0.0;
        for case in cases {
            let replayed = replay_case_metrics(case)?;
            let metric = replayed.get(mode).ok_or_else(|| {
                AppError::InvalidArg(format!("retrieval metric missing mode {mode}"))
            })?;
            recall += metric.recall_at_k;
            ndcg += metric.ndcg_at_k;
            mrr += metric.reciprocal_rank;
        }
        let count = cases.len() as f64;
        output.insert(
            mode.to_string(),
            RetrievalAggregateEvidence {
                recall_at_k: recall / count,
                ndcg_at_k: ndcg / count,
                mrr: mrr / count,
                queries: cases.len(),
            },
        );
    }
    Ok(output)
}

fn aggregates_from_cases(
    cases: &[RetrievalCaseEvidence],
) -> Result<BTreeMap<String, BTreeMap<String, RetrievalAggregateEvidence>>> {
    let mut aggregates = BTreeMap::new();
    aggregates.insert("all".to_string(), aggregate_cases(cases)?);
    for language in ["pl", "en"] {
        let subset = cases
            .iter()
            .filter(|case| case.language == language)
            .cloned()
            .collect::<Vec<_>>();
        aggregates.insert(format!("language:{language}"), aggregate_cases(&subset)?);
    }
    Ok(aggregates)
}

fn model_file_evidence() -> Result<Vec<RetrievalModelFileEvidence>> {
    let directory = crate::embed::embed_model_dir()?;
    crate::embed::EMBED_MODEL_FILES
        .iter()
        .map(|filename| {
            let path = directory.join(filename);
            let bytes = std::fs::metadata(&path)
                .map_err(|error| AppError::Storage(format!("stat embed model file: {error}")))?
                .len();
            Ok(RetrievalModelFileEvidence {
                filename: (*filename).to_string(),
                bytes,
                sha256: sha256_file(&path)?,
            })
        })
        .collect()
}

/// Re-check the selected model and every model-file digest after a live run. This catches a model
/// switch or on-disk replacement during measurement without retaining the model directory path.
pub(crate) fn assert_model_unchanged(evidence: &RetrievalQualityEvidence) -> Result<()> {
    if crate::embed::selected_embed_model().id != evidence.embedder_id {
        return Err(AppError::InvalidArg(
            "selected retrieval model changed during measurement".to_string(),
        ));
    }
    let current = model_file_evidence()?;
    if current != evidence.model_files {
        return Err(AppError::InvalidArg(
            "retrieval model files changed during measurement".to_string(),
        ));
    }
    Ok(())
}

/// Build exact, replayable retrieval evidence. A real installed embedder is mandatory: the
/// persistence-only constructor refuses the deterministic stub. All aggregate values are computed
/// from the serialized per-case hashed rankings, never by rerunning retrieval or trusting a second
/// numeric source.
pub(crate) fn build_evidence() -> Result<RetrievalQualityEvidence> {
    let _env_serial = real_embed_env_lock()
        .lock()
        .map_err(|_| AppError::Storage("real-embed eval environment lock poisoned".to_string()))?;
    let _env = RealEmbedTestEnv::enable();
    let _release_cache = RealEmbedderCacheRelease;

    let selected_model = crate::embed::selected_embed_model();
    let model_files = model_file_evidence()?;
    let scratch = PrivateSqlCipherDb::create()?;
    let embedder = crate::embed::active_persistence_embedder()?;
    let db = scratch.db()?;
    let seeded_ids = crate::eval::corpus::seed_synthetic_corpus(db, embedder.as_ref())?;
    if seeded_ids.len() != crate::eval::corpus::SYNTHETIC_MEETINGS.len() {
        return Err(AppError::InvalidArg(
            "synthetic retrieval corpus seed count mismatch".to_string(),
        ));
    }

    let fixture = include_str!("fixtures/rag-bakeoff-synthetic.json");
    let labeled = LabeledSet::from_json(fixture)?;
    if labeled.is_empty() {
        return Err(AppError::InvalidArg(
            "synthetic retrieval fixture is empty".to_string(),
        ));
    }
    let anchor =
        chrono::NaiveDate::parse_from_str(crate::eval::corpus::CORPUS_ANCHOR_DATE, "%Y-%m-%d")
            .map_err(|error| AppError::InvalidArg(format!("parse retrieval anchor: {error}")))?;
    let unlocked = HashSet::new();
    let mut cases = Vec::with_capacity(labeled.len());

    for (index, query) in labeled.0.iter().enumerate() {
        let date_filter = crate::summarize::temporal::extract_date_filter(&query.query, anchor);
        let fts = db
            .search_visible_in_range(
                &query.query,
                PRODUCT_CANDIDATE_LIMIT,
                &unlocked,
                date_filter.clone(),
            None,
        )?
            .into_iter()
            .map(|hit| hit.meeting.id)
            .collect::<Vec<_>>();
        let query_vec = embedder
            .embed_query(std::slice::from_ref(&query.query))?
            .into_iter()
            .next()
            .unwrap_or_default();
        let semantic = db
            .search_semantic_visible(
                &query_vec,
                PRODUCT_CANDIDATE_LIMIT,
                crate::embed::KNN_SEARCH_COSINE_FLOOR,
                &unlocked,
        None,
    )?
            .into_iter()
            .map(|hit| hit.meeting.id)
            .collect::<Vec<_>>();
        let hybrid = db
            .search_hybrid_visible(
                &query.query,
                &query_vec,
                PRODUCT_CANDIDATE_LIMIT,
                crate::embed::KNN_SEARCH_COSINE_FLOOR,
                &unlocked,
                date_filter,
        None,
    )?
            .into_iter()
            .map(|hit| hit.meeting.id)
            .collect::<Vec<_>>();

        let expected_id_hashes = query
            .expected_meeting_ids
            .iter()
            .map(|id| meeting_id_hash(id))
            .collect::<Vec<_>>();
        let rankings = [
            (MODE_FTS, fts),
            (MODE_SEMANTIC, semantic),
            (MODE_HYBRID, hybrid),
        ]
        .into_iter()
        .map(|(mode, ids)| {
            (
                mode.to_string(),
                ids.iter().map(|id| meeting_id_hash(id)).collect(),
            )
        })
        .collect::<BTreeMap<_, _>>();
        let mut case = RetrievalCaseEvidence {
            case_id: format!("retrieval-{:02}", index + 1),
            language: query.lang.clone(),
            query_payload_sha256: query_payload_hash(
                &query.lang,
                &query.query,
                &query.expected_meeting_ids,
            ),
            expected_meetings: query.expected_meeting_ids.len(),
            expected_id_hashes,
            rankings,
            metrics: BTreeMap::new(),
        };
        case.metrics = replay_case_metrics(&case)?;
        validate_case_metrics(&case)?;
        cases.push(case);
    }

    let aggregates = aggregates_from_cases(&cases)?;
    drop(embedder);
    let temporary_database_cleaned = scratch.finish()?;
    let corpus_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/eval/corpus.rs");
    let evidence = RetrievalQualityEvidence {
        required: true,
        surface: "ask_vault_retrieval",
        attribution: "independent_synthetic_retrieval_lane_not_generation_quality",
        fixture_sha256: framed_hash(&[fixture]),
        corpus_source_sha256: sha256_file(&corpus_path)?,
        embedder_id: selected_model.id.to_string(),
        real_embedder: true,
        model_files,
        anchor_date: crate::eval::corpus::CORPUS_ANCHOR_DATE,
        k: RETRIEVAL_K,
        candidate_limit: PRODUCT_CANDIDATE_LIMIT,
        cosine_floor: crate::embed::KNN_SEARCH_COSINE_FLOOR,
        cases,
        aggregates,
        visibility_gate: "Db::search_visible_in_range + Db::search_semantic_visible + Db::search_hybrid_visible with empty session-unlock set",
        temporary_database_cleaned,
    };
    validate_evidence_replay(&evidence)?;
    assert_model_unchanged(&evidence)?;
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::{Embedder, StubEmbedder};
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};
    use crate::transcribe::types::Segment;

    fn sample_case() -> RetrievalCaseEvidence {
        let expected = vec![meeting_id_hash("syn-a"), meeting_id_hash("syn-b")];
        let rankings = [
            (
                MODE_FTS.to_string(),
                vec![meeting_id_hash("syn-a"), meeting_id_hash("noise")],
            ),
            (
                MODE_SEMANTIC.to_string(),
                vec![meeting_id_hash("noise"), meeting_id_hash("syn-b")],
            ),
            (
                MODE_HYBRID.to_string(),
                vec![meeting_id_hash("syn-b"), meeting_id_hash("syn-a")],
            ),
        ]
        .into_iter()
        .collect();
        let mut case = RetrievalCaseEvidence {
            case_id: "retrieval-01".to_string(),
            language: "pl".to_string(),
            query_payload_sha256: framed_hash(&["synthetic query payload"]),
            expected_meetings: 2,
            expected_id_hashes: expected,
            rankings,
            metrics: BTreeMap::new(),
        };
        case.metrics = replay_case_metrics(&case).unwrap();
        case
    }

    fn sample_evidence(cases: Vec<RetrievalCaseEvidence>) -> RetrievalQualityEvidence {
        RetrievalQualityEvidence {
            required: true,
            surface: "ask_vault_retrieval",
            attribution: "independent_synthetic_retrieval_lane_not_generation_quality",
            fixture_sha256: framed_hash(&["fixture"]),
            corpus_source_sha256: framed_hash(&["corpus"]),
            embedder_id: "synthetic-embedder".to_string(),
            real_embedder: true,
            model_files: Vec::new(),
            anchor_date: crate::eval::corpus::CORPUS_ANCHOR_DATE,
            k: RETRIEVAL_K,
            candidate_limit: PRODUCT_CANDIDATE_LIMIT,
            cosine_floor: crate::embed::KNN_SEARCH_COSINE_FLOOR,
            aggregates: aggregates_from_cases(&cases).unwrap(),
            cases,
            visibility_gate: "visibility_clause",
            temporary_database_cleaned: true,
        }
    }

    #[test]
    fn hashed_rankings_replay_metrics_and_detect_tampering() {
        let case = sample_case();
        assert!(validate_case_metrics(&case).is_ok());
        let aggregates = aggregates_from_cases(std::slice::from_ref(&case)).unwrap();
        assert_eq!(aggregates["all"][MODE_HYBRID].recall_at_k, 1.0);
        assert_eq!(aggregates["all"][MODE_HYBRID].mrr, 1.0);

        let mut tampered = case;
        tampered.rankings.get_mut(MODE_HYBRID).unwrap().remove(0);
        assert!(validate_case_metrics(&tampered).is_err());

        let mut evidence = sample_evidence(vec![sample_case()]);
        assert!(validate_evidence_replay(&evidence).is_ok());
        evidence
            .aggregates
            .get_mut("all")
            .unwrap()
            .get_mut(MODE_HYBRID)
            .unwrap()
            .mrr = 0.0;
        assert!(validate_evidence_replay(&evidence).is_err());
    }

    #[test]
    fn private_sqlcipher_db_is_encrypted_permissioned_and_cleaned() {
        let mut scratch = PrivateSqlCipherDb::create().unwrap();
        let directory = scratch.directory.clone();
        let path = scratch.path.clone();
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        scratch.close();
        let mut header = [0_u8; 16];
        File::open(&path).unwrap().read_exact(&mut header).unwrap();
        assert_ne!(&header, b"SQLite format 3\0");
        let unkeyed = rusqlite::Connection::open(&path).unwrap();
        assert!(unkeyed
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
                row.get::<_, i64>(0)
            })
            .is_err());
        drop(unkeyed);
        assert!(scratch.finish().unwrap());
        assert!(!directory.exists());
        assert!(!path.exists());
        assert!(!PathBuf::from(format!("{}-wal", path.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", path.display())).exists());
        assert!(!PathBuf::from(format!("{}-journal", path.display())).exists());
    }

    #[test]
    fn private_sqlcipher_db_drop_cleans_during_unwind() {
        let observed_directory = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observed_in_panic = observed_directory.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let scratch = PrivateSqlCipherDb::create().unwrap();
            *observed_in_panic.lock().unwrap() = Some(scratch.directory.clone());
            panic!("exercise panic-safe retrieval cleanup");
        }));
        assert!(result.is_err());
        let directory = observed_directory.lock().unwrap().clone().unwrap();
        assert!(!directory.exists());
    }

    #[test]
    fn serialized_case_has_strict_content_free_allowlist() {
        let case = sample_case();
        let value = serde_json::to_value(&case).unwrap();
        let mut keys = value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                "caseId",
                "expectedIdHashes",
                "expectedMeetings",
                "language",
                "metrics",
                "queryPayloadSha256",
                "rankings",
            ]
        );
        let serialized = serde_json::to_string(&case).unwrap();
        for forbidden in [
            "synthetic query payload",
            "syn-a",
            "syn-b",
            "title",
            "snippet",
            "databasePath",
            "keyHex",
        ] {
            assert!(!serialized.contains(forbidden), "leaked {forbidden}");
        }
    }

    #[test]
    fn serialized_evidence_has_strict_top_level_allowlist() {
        let evidence = sample_evidence(vec![sample_case()]);
        let value = serde_json::to_value(&evidence).unwrap();
        let mut keys = value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                "aggregates",
                "anchorDate",
                "attribution",
                "candidateLimit",
                "cases",
                "corpusSourceSha256",
                "cosineFloor",
                "embedderId",
                "fixtureSha256",
                "k",
                "modelFiles",
                "realEmbedder",
                "required",
                "surface",
                "temporaryDatabaseCleaned",
                "visibilityGate",
            ]
        );
    }

    #[test]
    fn retrieval_readers_hide_locked_and_restore_session_unlocked_meeting() {
        let scratch = PrivateSqlCipherDb::create().unwrap();
        let db = scratch.db().unwrap();
        db.insert_folder(&Folder {
            id: "f-locked".to_string(),
            name: "Synthetic locked".to_string(),
            path: "Synthetic locked".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-01T00:00:00Z".to_string(),
        })
        .unwrap();
        db.insert_meeting(&Meeting {
            id: "locked-meeting".to_string(),
            started_at: "2026-06-24T10:00:00Z".to_string(),
            ended_at: None,
            title: Some("Synthetic status".to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: "locked-meeting".to_string(),
            provider_id: "local".to_string(),
            markdown: "quarterly budget synthetic body".to_string(),
            created_at: "2026-06-24T10:01:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder("locked-meeting", Some("f-locked"))
            .unwrap();
        let segments = vec![Segment {
            idx: 0,
            start_s: 0.0,
            end_s: 10.0,
            text: "quarterly budget synthetic body".to_string(),
            speaker: Some("me".to_string()),
            confidence: None,
        }];
        let stub = StubEmbedder;
        db.insert_segments("locked-meeting", &segments).unwrap();
        db.index_meeting_chunks("locked-meeting", &segments, &stub)
            .unwrap();
        db.set_folder_locked("f-locked", true, None).unwrap();
        let query = "quarterly budget synthetic body".to_string();
        let query_vec = stub
            .embed_query(std::slice::from_ref(&query))
            .unwrap()
            .into_iter()
            .next()
            .unwrap();

        let locked = HashSet::new();
        assert!(db
            .search_visible_in_range(&query, PRODUCT_CANDIDATE_LIMIT, &locked, None, None)
            .unwrap()
            .is_empty());
        assert!(db
            .search_semantic_visible(&query_vec, PRODUCT_CANDIDATE_LIMIT, 0.0, &locked, None)
            .unwrap()
            .is_empty());
        assert!(db
            .search_hybrid_visible(
                &query,
                &query_vec,
                PRODUCT_CANDIDATE_LIMIT,
                crate::embed::KNN_SEARCH_COSINE_FLOOR,
                &locked,
                None,
        None,
    )
            .unwrap()
            .is_empty());

        let mut unlocked = HashSet::new();
        unlocked.insert("f-locked".to_string());
        assert!(db
            .search_visible_in_range(&query, PRODUCT_CANDIDATE_LIMIT, &unlocked, None, None)
            .unwrap()
            .iter()
            .any(|hit| hit.meeting.id == "locked-meeting"));
        assert!(db
            .search_semantic_visible(&query_vec, PRODUCT_CANDIDATE_LIMIT, 0.0, &unlocked, None)
            .unwrap()
            .iter()
            .any(|hit| hit.meeting.id == "locked-meeting"));
        assert!(db
            .search_hybrid_visible(
                &query,
                &query_vec,
                PRODUCT_CANDIDATE_LIMIT,
                crate::embed::KNN_SEARCH_COSINE_FLOOR,
                &unlocked,
                None,
        None,
    )
            .unwrap()
            .iter()
            .any(|hit| hit.meeting.id == "locked-meeting"));
        scratch.finish().unwrap();
    }
}
