//! Bulk IMPORT of an external knowledge base into Murmur's canonical store.
//!
//! Distinct from `crate::extract` (one file → extracted text for the retrieval corpus) and from
//! `crate::connectors` (a live, consented, ledgered CLOUD query). An importer here is **local and
//! offline**: it reads an export the user already downloaded, and writes ordinary authored notes
//! through the existing gated funnel. Nothing leaves the machine, so no consent surface, no
//! redaction firewall involvement, and no egress-ledger row.
//!
//! The per-source normalizers are pure functions over in-memory bytes, which keeps them provable by
//! `cargo test --lib` — the DB-touching orchestration lives in `crate::commands::import`.

pub(crate) mod notion;
