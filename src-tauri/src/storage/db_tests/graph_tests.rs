//! File-backed (tempfile via `open_with_key` + a FIXED test key) tests for the in-app
//! self-assembling graph (entities + mentions). These NEVER touch the real Keychain — both
//! the SQLCipher DEK and the per-folder lock KEK are explicit literals. They exercise the
//! Sink-A DB helpers + the visibility predicate (the single highest-stakes anti-leak line),
//! and the Sink-B `locked`-gate (disk-truth, not session-unlock) for the vault stub mirror.

use super::*;
use crate::storage::models::{
    EntityKind, Folder, FullGraphEdgeKind, FullGraphNodeKind, FullGraphOpts, GraphEdge, Meeting,
    MeetingStatus, NoteRecord,
};

const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn temp_db_path(label: &str) -> std::path::PathBuf {
    super::unique_temp_path(&format!("meetnotes-graph-test-{label}"), "sqlite")
}

fn file_db(label: &str) -> Db {
    Db::open_with_key(&temp_db_path(label), TEST_DEK).unwrap()
}

/// A scratch vault directory for Sink-B `.md` stub assertions (unique per test).
fn temp_vault(label: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "meetnotes-graph-vault-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn seed_folder(db: &Db, id: &str, name: &str) {
    db.insert_folder(&Folder {
        id: id.to_string(),
        name: name.to_string(),
        path: name.to_string(),
        parent_id: None,
        locked: false,
        created_at: "2026-06-26T00:00:00Z".to_string(),
    })
    .unwrap();
}

/// Seed a meeting + one note row, optionally filed into `folder_id`.
fn seed_note(db: &Db, meeting_id: &str, markdown: &str, folder_id: Option<&str>) {
    db.insert_meeting(&Meeting {
        id: meeting_id.to_string(),
        started_at: format!("2026-06-26T09:00:00Z+{meeting_id}"),
        ended_at: None,
        title: Some(format!("title-{meeting_id}")),
        duration_s: 60,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.to_string(),
        provider_id: "claude_code".to_string(),
        markdown: markdown.to_string(),
        created_at: "2026-06-26T09:05:00Z".to_string(),
        exported_path: Some(format!("/vault/{meeting_id}.md")),
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.set_note_folder(meeting_id, folder_id).unwrap();
}

/// Mirror of `lock_folder`: encrypt each note into a content blob, blank markdown, seal.
fn seal_folder(db: &Db, folder_id: &str, kek: &[u8; 32]) {
    let ck = crate::crypto::random_key().unwrap();
    let wrapped = crate::crypto::encrypt(kek, &ck, b"").unwrap();
    let notes = db.notes_in_folder(folder_id).unwrap();
    let mut blobs = Vec::new();
    for n in &notes {
        let blob = crate::crypto::encrypt(&ck, n.markdown.as_bytes(), b"").unwrap();
        blobs.push((n.meeting_id.clone(), n.provider_id.clone(), blob));
    }
    db.set_folder_locked(folder_id, true, Some(&wrapped))
        .unwrap();
    for (mid, pid, blob) in &blobs {
        db.seal_note(mid, pid, blob).unwrap();
    }
}

fn unlocked_set(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn entity_dedup() {
    // Same name, different casing, same kind → ONE entity (case-insensitive on name_ci),
    // FIRST-SEEN casing kept. Mentions across distinct meetings accumulate; a repeat mention
    // is idempotent (PK). A same-name DIFFERENT kind is a distinct row.
    let db = file_db("dedup");
    seed_note(&db, "m1", "# note", None);
    seed_note(&db, "m2", "# note", None);

    let id1 = db
        .upsert_entity("Anna Kowalska", EntityKind::Person)
        .unwrap();
    let id2 = db
        .upsert_entity("anna kowalska", EntityKind::Person)
        .unwrap();
    assert_eq!(id1, id2, "case-insensitive dedup → same entity id");

    // First-seen casing is preserved (the lowercase re-insert must NOT overwrite it).
    let ent = db.get_entity(&id1).unwrap().unwrap();
    assert_eq!(ent.name, "Anna Kowalska", "first-seen casing kept");

    // Same name, DIFFERENT kind → a distinct entity row (the (name_ci, kind) unique index).
    let proj = db
        .upsert_entity("Anna Kowalska", EntityKind::Project)
        .unwrap();
    assert_ne!(proj, id1, "same name + different kind = distinct entity");

    // Mentions accumulate across meetings; a duplicate mention is idempotent.
    db.add_mention(&id1, "m1").unwrap();
    db.add_mention(&id1, "m1").unwrap(); // idempotent — no double count
    db.add_mention(&id1, "m2").unwrap();

    let empty = HashSet::new();
    let nodes = db.list_entities_visible(&empty).unwrap();
    let anna = nodes
        .iter()
        .find(|n| n.id == id1)
        .expect("Anna present in visible nodes");
    assert_eq!(
        anna.mention_count, 2,
        "two distinct meetings, idempotent repeat"
    );
    assert_eq!(anna.name, "Anna Kowalska");
}

#[test]
fn graph_visibility_filter() {
    // The core anti-leak test. An entity mentioned ONLY in a SEALED folder's meeting is ABSENT
    // from get_graph/list_entities_visible while the folder is locked + not in the unlocked
    // set; it reappears when the folder id IS in the unlocked set. An entity also mentioned in
    // an OPEN meeting keeps only its VISIBLE count (never the true count).
    let db = file_db("visibility");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "secret", "Secret");
    seed_note(&db, "open1", "# open", None); // root → always visible
    seed_note(&db, "sealed1", "# sealed", Some("secret"));

    // "Secret Person" mentioned ONLY in the sealed meeting.
    let secret_p = db
        .upsert_entity("Secret Person", EntityKind::Person)
        .unwrap();
    db.add_mention(&secret_p, "sealed1").unwrap();
    // "Shared Project" mentioned in BOTH the open and the sealed meeting.
    let shared = db
        .upsert_entity("Shared Project", EntityKind::Project)
        .unwrap();
    db.add_mention(&shared, "open1").unwrap();
    db.add_mention(&shared, "sealed1").unwrap();

    let empty: HashSet<String> = HashSet::new();

    // BEFORE sealing: both entities present; Shared has count 2.
    let before = db.list_entities_visible(&empty).unwrap();
    assert!(before.iter().any(|n| n.id == secret_p));
    assert_eq!(
        before
            .iter()
            .find(|n| n.id == shared)
            .unwrap()
            .mention_count,
        2
    );
    // An edge exists between the two (they co-occur in sealed1) — pre-seal.
    let (lo, hi) = if secret_p < shared {
        (secret_p.clone(), shared.clone())
    } else {
        (shared.clone(), secret_p.clone())
    };
    assert!(
        db.graph_edges_visible(&empty)
            .unwrap()
            .iter()
            .any(|e| e.source == lo && e.target == hi),
        "co-occurring entities have an edge before sealing"
    );

    // SEAL the folder, session NOT unlocked.
    seal_folder(&db, "secret", &kek);
    let nodes = db.list_entities_visible(&empty).unwrap();
    assert!(
        !nodes.iter().any(|n| n.id == secret_p),
        "entity only in a sealed-not-unlocked meeting must be ABSENT"
    );
    // Shared survives but with VISIBLE count 1 (only the open meeting), never the true 2.
    let shared_node = nodes
        .iter()
        .find(|n| n.id == shared)
        .expect("shared entity still visible via its open meeting");
    assert_eq!(
        shared_node.mention_count, 1,
        "visible count only — sealed mention drops out, never leaks count 2"
    );
    // No edge when the only co-occurrence is sealed.
    assert!(
        db.graph_edges_visible(&empty).unwrap().is_empty(),
        "co-occurrence in a sealed meeting yields no edge"
    );
    // build_graph reflects the same + flags hidden folders.
    let graph = db.build_graph(&empty).unwrap();
    assert!(
        graph.has_hidden,
        "a sealed-not-unlocked folder sets has_hidden"
    );
    assert!(!graph.nodes.iter().any(|n| n.id == secret_p));
    // entity_mentions_visible: the secret entity has zero visible backlinks while sealed.
    assert!(db
        .entity_mentions_visible(&secret_p, &empty)
        .unwrap()
        .is_empty());

    // SESSION-UNLOCK the folder id → the sealed contribution reappears.
    let unlocked = unlocked_set(&["secret"]);
    let nodes_u = db.list_entities_visible(&unlocked).unwrap();
    assert!(
        nodes_u.iter().any(|n| n.id == secret_p),
        "entity reappears once its folder id is in the unlocked set"
    );
    assert_eq!(
        nodes_u
            .iter()
            .find(|n| n.id == shared)
            .unwrap()
            .mention_count,
        2,
        "both mentions visible again when unlocked"
    );
    // Edge with weight 1 returns (one shared visible meeting).
    let edges_u = db.graph_edges_visible(&unlocked).unwrap();
    assert_eq!(edges_u.len(), 1, "exactly one deduped edge per pair");
    assert_eq!(edges_u[0].weight, 1);
    // build_graph no longer flags hidden (the only locked folder is now unlocked).
    assert!(!db.build_graph(&unlocked).unwrap().has_hidden);
    // The secret entity's visible backlinks return when unlocked.
    assert_eq!(
        db.entity_mentions_visible(&secret_p, &unlocked)
            .unwrap()
            .len(),
        1
    );
}

// ── Brain v3 PR-4 — full-brain graph gating + honesty tests ────────────────────────────────

/// Seed one `links` row directly (test helper; mirrors the raw-insert pattern the PR-3 link
/// tests use). Bypasses the write-time indexers so a test can pin `build_full_graph`'s edge
/// gating in isolation.
fn seed_link(
    db: &Db,
    src: (&str, &str),
    dst: (&str, &str),
    edge_type: &str,
    status: &str,
    score: f64,
) {
    db.lock()
            .execute(
                "INSERT INTO links
                   (src_kind, src_id, dst_kind, dst_id, edge_type, score, created_by, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'auto', ?7, 1)",
                rusqlite::params![src.0, src.1, dst.0, dst.1, edge_type, score, status],
            )
            .unwrap();
}

/// GATE (the load-bearing PR-4 test): sealing a folder removes its meeting/note/document NODES
/// from the full-brain graph AND every edge that touches them (both endpoints gated) — while the
/// open folder's nodes/edges survive. Session-unlocking the folder id restores everything.
/// RED-before-GREEN: drop the visible-node-set both-endpoint check (or read `documents`/meetings
/// ungated) and the sealed meeting/note/document — or an edge into them — leaks.
#[test]
fn build_full_graph_excludes_sealed_nodes_and_their_edges() {
    let db = file_db("fullgraph-gate");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "open", "Open");
    seed_folder(&db, "secret", "Secret");

    // OPEN folder: a meeting (via seed_note), an authored note, and a document.
    seed_note(&db, "m-open", "# open meeting", Some("open"));
    db.insert_note("n-open", "open", "open-note", "Open Note", "body", 1_000)
        .unwrap();
    db.insert_document("d-open", "open", "open.pdf", "doc body", "document", 1_000)
        .unwrap();
    // SECRET folder: a meeting, a note, a document — all sealed below.
    seed_note(&db, "m-secret", "# secret meeting", Some("secret"));
    db.insert_note(
        "n-secret",
        "secret",
        "secret-note",
        "Secret Note",
        "body",
        1_000,
    )
    .unwrap();
    db.insert_document(
        "d-secret",
        "secret",
        "secret.pdf",
        "hush",
        "document",
        1_000,
    )
    .unwrap();

    // An entity mentioned in BOTH meetings (co-occurrence needs ≥2 in a meeting, but a single
    // mention still makes a `mention` edge). Add a second entity so both meetings co-occur them.
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    let nova = db.upsert_entity("Nova", EntityKind::Person).unwrap();
    db.add_mention(&atlas, "m-open").unwrap();
    db.add_mention(&nova, "m-open").unwrap();
    db.add_mention(&atlas, "m-secret").unwrap();
    db.add_mention(&nova, "m-secret").unwrap();

    // Links spanning the boundary:
    //  - a wikilink open-note → secret-note (must vanish when secret seals: sealed endpoint);
    //  - a companion secret-note → secret-meeting (both sealed → vanishes);
    //  - a wikilink open-note → open-meeting (both open → survives).
    seed_link(
        &db,
        ("note", "n-open"),
        ("note", "n-secret"),
        "wikilink",
        "active",
        1.0,
    );
    seed_link(
        &db,
        ("note", "n-secret"),
        ("meeting", "m-secret"),
        "companion",
        "active",
        1.0,
    );
    seed_link(
        &db,
        ("note", "n-open"),
        ("meeting", "m-open"),
        "wikilink",
        "active",
        1.0,
    );

    let empty: HashSet<String> = HashSet::new();
    let opts = FullGraphOpts::default();

    // BEFORE sealing: every node + edge present.
    let g0 = db.build_full_graph(&empty, opts).unwrap();
    for id in [
        "m-open", "m-secret", "n-open", "n-secret", "d-open", "d-secret",
    ] {
        assert!(
            g0.nodes.iter().any(|n| n.id == id),
            "node {id} present pre-seal"
        );
    }
    assert!(
        g0.edges
            .iter()
            .any(|e| e.src == "n-open" && e.dst == "n-secret"),
        "cross-folder wikilink present pre-seal"
    );
    assert!(!g0.has_hidden, "no locked folder pre-seal");

    // SEAL the secret folder.
    seal_folder(&db, "secret", &kek);
    db.set_folder_locked("secret", true, None).unwrap();

    let g1 = db.build_full_graph(&empty, opts).unwrap();
    // The secret folder's nodes are GONE; the open ones survive.
    for id in ["m-secret", "n-secret", "d-secret"] {
        assert!(
            !g1.nodes.iter().any(|n| n.id == id),
            "sealed node {id} must not appear (leak)"
        );
    }
    for id in ["m-open", "n-open", "d-open"] {
        assert!(
            g1.nodes.iter().any(|n| n.id == id),
            "open node {id} survives"
        );
    }
    // EVERY edge touching a sealed node is dropped — both directions.
    assert!(
        !g1.edges
            .iter()
            .any(|e| e.src.contains("secret") || e.dst.contains("secret")),
        "no edge may touch a sealed node (both-endpoint gate)"
    );
    // The all-open wikilink survives.
    assert!(
        g1.edges
            .iter()
            .any(|e| e.src == "n-open" && e.dst == "m-open"),
        "all-open wikilink survives the seal"
    );
    // The mention edges into the sealed meeting are gone; the open one survives.
    assert!(
        g1.edges
            .iter()
            .any(|e| e.kind == FullGraphEdgeKind::Mention && e.dst == "m-open"),
        "mention into the open meeting survives"
    );
    assert!(
        !g1.edges
            .iter()
            .any(|e| e.kind == FullGraphEdgeKind::Mention && e.dst == "m-secret"),
        "mention into the sealed meeting is dropped"
    );
    assert!(
        g1.has_hidden,
        "a sealed-not-unlocked folder sets has_hidden"
    );

    // SESSION-UNLOCK the folder id → everything reappears.
    let unlocked = unlocked_set(&["secret"]);
    let g2 = db.build_full_graph(&unlocked, opts).unwrap();
    for id in ["m-secret", "n-secret", "d-secret"] {
        assert!(
            g2.nodes.iter().any(|n| n.id == id),
            "sealed node {id} reappears once the folder id is unlocked"
        );
    }
    assert!(
        g2.edges
            .iter()
            .any(|e| e.src == "n-open" && e.dst == "n-secret"),
        "cross-folder wikilink returns when unlocked"
    );
    assert!(
        !g2.has_hidden,
        "no hidden folder once the only lock is unlocked"
    );
}

/// A `status='suggested'` semantic link is HIDDEN unless `include_suggested` is on; an `active`
/// link is always present. RED-before-GREEN: read all non-dismissed rows regardless of the flag
/// and the suggested edge leaks into the default graph.
#[test]
fn build_full_graph_suggested_semantic_behind_opts_flag() {
    let db = file_db("fullgraph-suggested");
    seed_folder(&db, "f", "Notes");
    db.insert_note("n1", "f", "n1", "Note One", "body", 1_000)
        .unwrap();
    db.insert_note("n2", "f", "n2", "Note Two", "body", 1_000)
        .unwrap();
    // An ACTIVE wikilink (always shown) + a SUGGESTED semantic (flag-gated), both n1↔n2.
    seed_link(
        &db,
        ("note", "n1"),
        ("note", "n2"),
        "wikilink",
        "active",
        1.0,
    );
    seed_link(
        &db,
        ("note", "n1"),
        ("note", "n2"),
        "semantic",
        "suggested",
        0.9,
    );

    let empty: HashSet<String> = HashSet::new();
    // DEFAULT (flag off): only the active wikilink edge.
    let off = db
        .build_full_graph(&empty, FullGraphOpts::default())
        .unwrap();
    assert!(
        off.edges
            .iter()
            .any(|e| e.kind == FullGraphEdgeKind::Wikilink),
        "the active wikilink is always present"
    );
    assert!(
        !off.edges
            .iter()
            .any(|e| e.kind == FullGraphEdgeKind::Semantic),
        "a suggested semantic edge is hidden with the flag off"
    );
    // Flag ON: the suggested semantic edge appears with status='suggested'.
    let on = db
        .build_full_graph(
            &empty,
            FullGraphOpts {
                include_suggested: true,
            },
        )
        .unwrap();
    let sem = on
        .edges
        .iter()
        .find(|e| e.kind == FullGraphEdgeKind::Semantic)
        .expect("suggested semantic edge present with the flag on");
    assert_eq!(sem.status, "suggested");
    assert!((sem.score - 0.9).abs() < 1e-9, "semantic score = cosine");
}

/// A USER-created `manual` link (the note↔document "Related" chip, written by `upsert_manual_link`)
/// MUST appear as an ACTIVE edge in the full-brain graph and bump both endpoints' degree.
/// REGRESSION (2026-07-20): `full_graph_edge_kind_from_type` mapped only wikilink/companion/semantic,
/// so every `manual` edge fell to `_ => None` and was `continue`-skipped — a linked document showed
/// "0 connections". RED-before-GREEN: drop the `"manual"` arm and this test fails (no edge, degree 0).
#[test]
fn build_full_graph_includes_manual_link_edge() {
    let db = file_db("fullgraph-manual");
    seed_folder(&db, "f", "Notes");
    db.insert_note("n1", "f", "n1", "Note One", "body", 1_000)
        .unwrap();
    db.insert_document("d1", "f", "cv.pdf", "doc body", "document", 1_000)
        .unwrap();
    // A user-created manual link note → document (exactly what `upsert_manual_link` writes).
    seed_link(
        &db,
        ("note", "n1"),
        ("document", "d1"),
        "manual",
        "active",
        1.0,
    );

    let empty: HashSet<String> = HashSet::new();
    let g = db
        .build_full_graph(&empty, FullGraphOpts::default())
        .unwrap();

    // The manual edge is present, active, and correctly typed at both endpoints.
    let edge = g
        .edges
        .iter()
        .find(|e| e.src == "n1" && e.dst == "d1")
        .expect("the manual note→document link must be a graph edge (regression: it was dropped)");
    assert_eq!(
        edge.kind,
        FullGraphEdgeKind::Manual,
        "typed as a manual edge"
    );
    assert_eq!(edge.src_kind, FullGraphNodeKind::Note);
    assert_eq!(edge.dst_kind, FullGraphNodeKind::Document);
    assert_eq!(edge.status, "active", "a manual link is an active edge");
    // The document is no longer a 0-connection orphan.
    let doc = g
        .nodes
        .iter()
        .find(|n| n.id == "d1" && n.kind == FullGraphNodeKind::Document)
        .expect("document node present");
    assert_eq!(
        doc.degree, 1,
        "the linked document has one connection, not zero"
    );
}

/// HONEST CAPS: the per-kind node cap trims `nodes` while `total_visible_nodes` reports the TRUE
/// pre-cap count, and `has_hidden` reflects LOCKED folders independently. RED-before-GREEN: set
/// `total_visible_nodes = nodes.len()` and the silent cap-drop is invisible to the FE.
#[test]
fn build_full_graph_cap_and_has_hidden_are_honest() {
    let db = file_db("fullgraph-cap");
    seed_folder(&db, "f", "Notes");
    // Seed one more note than the per-kind cap so the note leg is trimmed.
    let over = MAX_FULL_GRAPH_PER_KIND + 3;
    for i in 0..over {
        db.insert_note(
            &format!("n{i:04}"),
            "f",
            &format!("n{i}"),
            &format!("Note {i}"),
            "body",
            1_000,
        )
        .unwrap();
    }
    let empty: HashSet<String> = HashSet::new();
    let g = db
        .build_full_graph(&empty, FullGraphOpts::default())
        .unwrap();
    let note_nodes = g
        .nodes
        .iter()
        .filter(|n| n.kind == FullGraphNodeKind::Note)
        .count();
    assert_eq!(
        note_nodes, MAX_FULL_GRAPH_PER_KIND,
        "the note leg is capped at MAX_FULL_GRAPH_PER_KIND"
    );
    assert_eq!(
        g.nodes.len(),
        MAX_FULL_GRAPH_PER_KIND,
        "the emitted node list is trimmed by the cap (only notes here)"
    );
    assert_eq!(
            g.total_visible_nodes, over as i64,
            "total_visible_nodes reports the TRUE (UNCAPPED) count — > nodes.len() when the cap trimmed"
        );
    assert!(
        g.total_visible_nodes > g.nodes.len() as i64,
        "the honest disclosure exposes the silent cap-drop"
    );
    // has_hidden is false with no locked folder — even though the cap trimmed rows (the two
    // signals are distinct: cap-drop is disclosed via total_visible_nodes, not has_hidden).
    assert!(
        !g.has_hidden,
        "cap-drop must NOT masquerade as a locked-folder hide"
    );

    // Lock a folder → has_hidden flips true, independent of the cap.
    db.set_folder_locked("f", true, None).unwrap();
    let g2 = db
        .build_full_graph(&empty, FullGraphOpts::default())
        .unwrap();
    assert!(g2.has_hidden, "a locked folder sets has_hidden");
}

/// PR-9 F3 (co-occurrence determinism): `graph_edges_visible`'s `LIMIT 600` had no tiebreak, so
/// the surviving edge subset could vary between opens (contradicting the "Deterministic" claim).
/// Two calls on the SAME data must now return the byte-identical edge set (weight DESC, pair ASC).
/// RED-before-GREEN: drop the `a.entity_id ASC, b.entity_id ASC` tiebreak and, on a dataset whose
/// weights tie at the cap boundary, the two vectors can differ.
#[test]
fn graph_edges_visible_is_deterministic_across_calls() {
    let db = file_db("cooc-determinism");
    seed_folder(&db, "f", "Notes");
    // Many entities co-occurring in ONE meeting → every pair has weight 1 (a full tie). With no
    // tiebreak the `LIMIT` picks an engine-arbitrary 600 of the pairs; the tiebreak pins them.
    seed_note(&db, "m", "# meeting", Some("f"));
    let mut ids = Vec::new();
    for i in 0..40 {
        let e = db
            .upsert_entity(&format!("Ent {i:02}"), EntityKind::Person)
            .unwrap();
        db.add_mention(&e, "m").unwrap();
        ids.push(e);
    }
    let empty: HashSet<String> = HashSet::new();
    let a = db.graph_edges_visible(&empty).unwrap();
    let b = db.graph_edges_visible(&empty).unwrap();
    assert!(!a.is_empty(), "co-occurrence edges exist");
    let key = |v: &[GraphEdge]| {
        v.iter()
            .map(|e| (e.source.clone(), e.target.clone(), e.weight))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        key(&a),
        key(&b),
        "two build passes on identical data must return the identical edge set"
    );
    // And the order itself is the declared one: weight DESC then (source, target) ASC.
    let mut sorted = a.clone();
    sorted.sort_by(|x, y| {
        y.weight
            .cmp(&x.weight)
            .then_with(|| x.source.cmp(&y.source))
            .then_with(|| x.target.cmp(&y.target))
    });
    assert_eq!(
        key(&a),
        key(&sorted),
        "edges are ordered weight DESC, then pair id ASC (deterministic)"
    );
}

/// PR-9 F2 (mention-edge recency ordering + honest edge-cap): the mention leg truncated at
/// `MAX_MENTION_EDGES` in ARBITRARY entity-UUID order, so an entity late in UUID order lost ALL
/// its edges with no signal. Now it orders by meeting RECENCY, so a mention in the FRESHEST
/// meeting survives the cap regardless of the entity's UUID, and `edges_truncated` discloses the
/// trim. RED-before-GREEN: restore `ORDER BY em.entity_id` and the most-recent-meeting mention
/// from a high-UUID entity is dropped while a stale one survives; drop the flag and the trim is
/// invisible.
#[test]
fn mention_edges_prefer_recency_and_flag_truncation() {
    let db = file_db("mention-cap");
    seed_folder(&db, "f", "Notes");
    // One OLD meeting carries the cap's worth of mentions (2000), one NEW meeting carries one
    // extra mention from a fresh entity. Recency-ordering must keep the NEW mention and shed an
    // OLD one; `edges_truncated` must be true (2001 visible > 2000 cap).
    db.insert_meeting(&Meeting {
        id: "m-old".to_string(),
        started_at: "2020-01-01T00:00:00Z".to_string(),
        ended_at: None,
        title: Some("Old".to_string()),
        duration_s: 60,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    db.insert_meeting(&Meeting {
        id: "m-new".to_string(),
        started_at: "2026-12-31T00:00:00Z".to_string(),
        ended_at: None,
        title: Some("New".to_string()),
        duration_s: 60,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: None,
    })
    .unwrap();
    // Seed entity rows + mentions with CONTROLLED ids (not random UUIDs) so the recency-vs-UUID
    // ordering is deterministic: the fresh entity's id "z-fresh" sorts AFTER every "e00000..".
    // Under the OLD `ORDER BY entity_id ASC LIMIT 2000` the highest-id row (the fresh one) is the
    // one dropped; under the NEW recency order it survives — that contrast is the F2 fix.
    let seed_entity = |id: &str, name: &str| {
        db.lock()
            .execute(
                "INSERT INTO entities (id, name, name_ci, kind, created_at)
                     VALUES (?1, ?2, ?3, 'person', '2020-01-01T00:00:00Z')",
                rusqlite::params![id, name, name.to_lowercase()],
            )
            .unwrap();
    };
    // 2000 distinct entities mentioned in the OLD meeting, ids "e00000".."e01999" (all < "z-fresh").
    for i in 0..2000 {
        let id = format!("e{i:05}");
        seed_entity(&id, &format!("Old {i:04}"));
        db.add_mention(&id, "m-old").unwrap();
    }
    // ONE fresh entity (highest id) mentioned in the NEW meeting.
    let fresh = "z-fresh".to_string();
    seed_entity(&fresh, "Fresh Face");
    db.add_mention(&fresh, "m-new").unwrap();

    let empty: HashSet<String> = HashSet::new();
    let (rows, truncated) = db.entity_meeting_mentions_visible(&empty).unwrap();
    assert_eq!(
        rows.len(),
        2000,
        "the cap bounds the returned mention edges"
    );
    assert!(
        truncated,
        "2001 visible mentions > the 2000 cap sets edges_truncated"
    );
    // The freshest-meeting mention survived the cap (recency-ordered) — the whole point of F2.
    // Under the old `entity_id ASC` order this highest-id entity would be the one dropped.
    assert!(
        rows.iter().any(|(e, m, _)| e == &fresh && m == "m-new"),
        "a mention in the most-RECENT meeting survives the cap regardless of entity UUID"
    );
    // And the truncation surfaces through the full graph payload.
    let g = db
        .build_full_graph(&empty, FullGraphOpts::default())
        .unwrap();
    assert!(
        g.edges_truncated,
        "build_full_graph propagates the mention-leg truncation to the FE"
    );
}

/// PR-9 F2 (links leg bounded + flagged): `full_graph_links` read the WHOLE `links` table
/// unbounded — the fastest-growing edge leg had no cap while the node legs each did. It now caps
/// at `MAX_FULL_GRAPH_LINK_EDGES` (score DESC) and reports truncation. RED-before-GREEN: remove
/// the LIMIT and the returned count is unbounded (> cap); drop the flag and the trim is silent.
#[test]
fn full_graph_links_are_bounded_and_flag_truncation() {
    let db = file_db("links-cap");
    seed_folder(&db, "f", "Notes");
    // Seed MAX_FULL_GRAPH_LINK_EDGES + 1 active note↔note wikilinks so the cap must trim exactly
    // one. (Endpoints need not exist as nodes — `full_graph_links` is PRE-gating, read raw.)
    let over = MAX_FULL_GRAPH_LINK_EDGES + 1;
    for i in 0..over {
        seed_link(
            &db,
            ("note", &format!("s{i:05}")),
            ("note", &format!("d{i:05}")),
            "wikilink",
            "active",
            1.0,
        );
    }
    let (rows, truncated) = db.full_graph_links(false).unwrap();
    assert_eq!(
        rows.len(),
        MAX_FULL_GRAPH_LINK_EDGES,
        "the links leg is bounded by the cap"
    );
    assert!(truncated, "one past the cap sets the truncated flag");
}

/// Metadata-only Related rows are filtered BEFORE the graph edge cap. Otherwise a large set of
/// high-score container relations can consume `LIMIT + 1`, falsely set truncation, and starve the
/// material meeting/note/document edge that the graph is actually able to render.
#[test]
fn full_graph_link_cap_ignores_container_rows_before_limit_and_count() {
    let db = file_db("links-container-before-cap");
    for i in 0..=MAX_FULL_GRAPH_LINK_EDGES {
        seed_link(
            &db,
            ("meeting", &format!("m{i:05}")),
            ("container", &format!("c{i:05}")),
            "manual",
            "active",
            1.0,
        );
    }
    seed_link(
        &db,
        ("meeting", "material-source"),
        ("note", "material-target"),
        "manual",
        "active",
        0.1,
    );

    let (rows, truncated) = db.full_graph_links(false).unwrap();
    assert_eq!(rows.len(), 1, "only the material graph edge belongs in this leg");
    assert_eq!(rows[0].0, "meeting");
    assert_eq!(rows[0].2, "note");
    assert!(
        !truncated,
        "container metadata must not consume the graph cap or inflate truncation"
    );
}

/// PR-9 F4 (edges carry endpoint kinds): a `FullGraphEdge` now names its `src_kind`/`dst_kind`
/// (the endpoint node kinds the backend gated on) so the FE can match endpoints by `(kind, id)`,
/// safe against a cross-kind id collision. A mention edge is entity→meeting; a companion link is
/// note→meeting. RED-before-GREEN: without the fields the FE can only match on bare id.
#[test]
fn full_graph_edges_carry_endpoint_kinds() {
    let db = file_db("edge-kinds");
    seed_folder(&db, "f", "Notes");
    seed_note(&db, "m1", "# meeting", Some("f"));
    db.insert_note("n1", "f", "n1", "Note One", "body", 1_000)
        .unwrap();
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();
    seed_link(
        &db,
        ("note", "n1"),
        ("meeting", "m1"),
        "companion",
        "active",
        1.0,
    );

    let empty: HashSet<String> = HashSet::new();
    let g = db
        .build_full_graph(&empty, FullGraphOpts::default())
        .unwrap();
    let mention = g
        .edges
        .iter()
        .find(|e| e.kind == FullGraphEdgeKind::Mention)
        .expect("mention edge present");
    assert_eq!(mention.src_kind, FullGraphNodeKind::Entity);
    assert_eq!(mention.dst_kind, FullGraphNodeKind::Meeting);
    let companion = g
        .edges
        .iter()
        .find(|e| e.kind == FullGraphEdgeKind::Companion)
        .expect("companion link present");
    assert_eq!(companion.src_kind, FullGraphNodeKind::Note);
    assert_eq!(companion.dst_kind, FullGraphNodeKind::Meeting);
}

/// PR-9 F4 (unified node date): a note/document node's `date` is an RFC3339 ISO string (was the
/// raw epoch-ms string), matching the meeting node's ISO `started_at`. RED-before-GREEN: emit
/// `ts.to_string()` and the field is a bare integer string, inconsistent with meetings.
#[test]
fn full_graph_content_node_date_is_iso() {
    let db = file_db("node-date-iso");
    seed_folder(&db, "f", "Notes");
    // created_at is epoch-MS; 1_700_000_000_000 ms = 2023-11-14T22:13:20Z.
    db.insert_note("n1", "f", "n1", "Note One", "body", 1_700_000_000_000)
        .unwrap();
    let empty: HashSet<String> = HashSet::new();
    let g = db
        .build_full_graph(&empty, FullGraphOpts::default())
        .unwrap();
    let note = g
        .nodes
        .iter()
        .find(|n| n.id == "n1")
        .expect("note node present");
    let date = note.date.as_deref().expect("note carries a date");
    assert!(
        date.starts_with("2023-11-14T"),
        "the note date is RFC3339 ISO (got {date:?}), not raw epoch-ms"
    );
    // A meeting node keeps its ISO started_at — both kinds now share the format.
    seed_note(&db, "m1", "# meeting", Some("f"));
    let g2 = db
        .build_full_graph(&empty, FullGraphOpts::default())
        .unwrap();
    let meeting = g2
        .nodes
        .iter()
        .find(|n| n.id == "m1")
        .expect("meeting node present");
    assert!(
        meeting.date.as_deref().is_some_and(|d| d.contains("T")),
        "the meeting date is ISO too"
    );
}

/// `/people` CRM GATE: a Person mentioned ONLY in a sealed-and-not-session-unlocked meeting is
/// ABSENT from `list_people`, and every count on a visible Person reflects VISIBLE sources only.
/// A Person seen in BOTH an open and a sealed meeting keeps only the open-source counts while the
/// folder is sealed, and the sealed source's meeting/fact/commitment all reappear once the folder
/// id is session-unlocked. RED-before-GREEN: drop the `list_entities_visible` filter (or the
/// per-count gated readers) and the secret person / sealed counts leak.
#[test]
fn list_people_excludes_sealed_person_and_counts_visible_only() {
    use crate::facts::{FactOp, NewFact};
    let db = file_db("people-gate");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "secret", "Secret");

    // OPEN meeting: mentions Bob, has an open commitment Bob owns, no facts here.
    seed_note(
        &db,
        "open1",
        "## Action items\n- [ ] Bob — ship the deck 2026-07-01\n",
        None,
    );
    // SEALED meeting: mentions Bob AGAIN + a Secret-Person-only mention, plus Bob's sealed
    // commitment and a sealed fact about Bob.
    seed_note(
        &db,
        "sealed1",
        "## Action items\n- [ ] Bob — secret task 2026-07-05\n",
        Some("secret"),
    );

    let bob = db.upsert_entity("Bob", EntityKind::Person).unwrap();
    db.add_mention(&bob, "open1").unwrap();
    db.add_mention(&bob, "sealed1").unwrap();
    let secret_p = db
        .upsert_entity("Secret Person", EntityKind::Person)
        .unwrap();
    db.add_mention(&secret_p, "sealed1").unwrap();

    // One VISIBLE-source fact about Bob (open meeting) + one SEALED-source fact (sealed meeting).
    let add = |predicate: &str, object: &str, meeting_id: &str| {
        FactOp::Add(NewFact {
            entity_id: bob.clone(),
            subject: "Bob".to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: "2026-06-01T00:00:00Z".to_string(),
            recorded_at: "2026-06-01T00:00:00Z".to_string(),
            confidence: 1.0,
            meeting_id: Some(meeting_id.to_string()),
        })
    };
    db.apply_fact_ops(&[add("role", "PM", "open1"), add("team", "Growth", "sealed1")])
        .unwrap();

    // SEAL the folder; session NOT unlocked.
    seal_folder(&db, "secret", &kek);

    let empty: HashSet<String> = HashSet::new();
    let sealed_view = db.list_people(&empty).unwrap().people;

    // (1) A person known only through the sealed meeting must NOT surface.
    assert!(
        !sealed_view.iter().any(|p| p.id == secret_p),
        "a person mentioned only in a sealed-not-unlocked meeting leaked into /people"
    );

    // (2) Bob surfaces via his OPEN meeting, but every count reflects VISIBLE sources only.
    let bob_card = sealed_view
        .iter()
        .find(|p| p.id == bob)
        .expect("Bob is visible via his open meeting");
    assert_eq!(bob_card.name, "Bob");
    assert_eq!(
        bob_card.meeting_count, 1,
        "only the open meeting counts while sealed"
    );
    assert_eq!(
        bob_card.last_talked.as_deref(),
        Some("2026-06-26T09:00:00Z+open1"),
        "last_talked = the open meeting's start (the sealed one is invisible)"
    );
    assert_eq!(
        bob_card.open_commitment_count, 1,
        "only the open-meeting commitment counts; the sealed task is hidden"
    );
    assert_eq!(
        bob_card.current_fact_count, 1,
        "only the open-source fact counts; the sealed fact is hidden"
    );

    // (3) Session-unlock the folder → the sealed contributions reappear. A real `unlock_folder`
    // decrypts the sealed note markdown back into the plaintext column for the session; the
    // `seal_folder` test helper only seals, so mirror that restore here (the CK->markdown decrypt
    // is exercised by the lock round-trip tests) before asserting the note-derived commitment
    // count. Mentions + facts need no restore — their rows persist through sealing.
    db.restore_note_markdown(
        "sealed1",
        "claude_code",
        "## Action items\n- [ ] Bob — secret task 2026-07-05\n",
    )
    .unwrap();
    let unlocked = unlocked_set(&["secret"]);
    let unlocked_view = db.list_people(&unlocked).unwrap().people;
    assert!(
        unlocked_view.iter().any(|p| p.id == secret_p),
        "the secret person reappears once its folder id is in the unlocked set"
    );
    let bob_u = unlocked_view.iter().find(|p| p.id == bob).unwrap();
    assert_eq!(
        bob_u.meeting_count, 2,
        "both meetings visible when unlocked"
    );
    assert_eq!(
        bob_u.last_talked.as_deref(),
        Some("2026-06-26T09:00:00Z+sealed1"),
        "last_talked advances to the now-visible sealed meeting (later suffix sorts last)"
    );
    assert_eq!(
        bob_u.open_commitment_count, 2,
        "both commitments visible when unlocked"
    );
    assert_eq!(
        bob_u.current_fact_count, 2,
        "both facts visible when unlocked"
    );
}

/// The People card's `open_commitment_count` badge MUST agree with the count backing the same
/// person's dossier "who owes what" section one click away
/// (`summarize::dossier::build_dossier_data`). B4 fix (2026-07-25): the shared predicate is now
/// OWNER-ONLY — an open item is this person's iff its owner name-matches. This asserts, via BOTH
/// `list_people`'s badge AND `build_dossier_data`'s commitments, that badge == dossier count for
/// two cases: (a) a person who OWNS an open commitment → 1 in both (they AGREE); (b) a person
/// merely MENTIONED in a meeting whose open commitment is owned by SOMEONE ELSE → 0 in both (they
/// AGREE). The badge==dossier invariant is the point; only the predicate (now owner-only) changed.
#[test]
fn list_people_open_commitment_count_matches_dossier_owner_filter() {
    let db = file_db("people-dossier-parity");
    // ONE meeting mentions BOTH people; its single open commitment is OWNED BY Priya. Sam is a
    // co-participant (mentioned) but owns nothing — under owner-only, Sam's item from this shared
    // meeting must NOT be attributed to Sam.
    seed_note(
        &db,
        "m1",
        "## Action items\n- [ ] Priya — ship the pricing page 2026-08-01\n",
        None,
    );
    let priya = db.upsert_entity("Priya", EntityKind::Person).unwrap();
    let sam = db.upsert_entity("Sam", EntityKind::Person).unwrap();
    db.add_mention(&priya, "m1").unwrap();
    db.add_mention(&sam, "m1").unwrap();

    let empty: HashSet<String> = HashSet::new();
    let cards = db.list_people(&empty).unwrap().people;

    // (a) Priya OWNS the commitment → badge 1, dossier 1, they agree.
    let priya_card = cards
        .iter()
        .find(|p| p.id == priya)
        .expect("Priya is visible via her mention");
    assert_eq!(
        priya_card.open_commitment_count, 1,
        "the card badge must count the open commitment OWNED BY Priya"
    );
    let priya_dossier = crate::summarize::dossier::build_dossier_data(&db, &priya, &empty)
        .unwrap()
        .expect("Priya has a visible dossier");
    assert_eq!(
        priya_dossier.commitments.len() as i64,
        priya_card.open_commitment_count,
        "the People card badge and the dossier 'who owes what' count must never disagree \
             for the owning person"
    );

    // (b) Sam is only MENTIONED; the commitment is owned by someone else → badge 0, dossier 0.
    let sam_card = cards
        .iter()
        .find(|p| p.id == sam)
        .expect("Sam is visible via his mention");
    assert_eq!(
        sam_card.open_commitment_count, 0,
        "a co-participant's commitment from a shared mentioning meeting must NOT be attributed \
             to Sam — owner-only"
    );
    let sam_dossier = crate::summarize::dossier::build_dossier_data(&db, &sam, &empty)
        .unwrap()
        .expect("Sam has a visible dossier");
    assert_eq!(
        sam_dossier.commitments.len() as i64,
        sam_card.open_commitment_count,
        "the People card badge and the dossier 'who owes what' count must never disagree \
             for the merely-mentioned person"
    );
}

/// 2026-07-13 UX audit fix: on a vault with MORE than `MAX_VISIBLE_ENTITIES` (500) visible
/// entities and ZERO locked folders, `has_hidden` (which only reflects LOCKED folders) stays
/// false while `list_entities_visible`'s `LIMIT 500` silently trims the roster — so the OLD
/// `GraphData`/`Vec<PersonCard>` shapes gave the FE no way to tell `total()`/"Show all N" was
/// understating completeness. RED-before-GREEN: before `total_visible_entities` /
/// `total_visible_people` existed, there was no field to assert on at all (a compile-time
/// absence); post-fix, both must exceed the capped roster length with `has_hidden` still false.
#[test]
fn graph_and_people_disclose_the_500_cap_even_with_no_locked_folders() {
    let db = file_db("cap-disclosure");
    seed_note(&db, "m1", "one shared meeting", None);

    // 520 distinct Person entities, all mentioned in the SAME open (never-locked) meeting —
    // every one has exactly one VISIBLE mention, so none is trimmed by the `HAVING cnt > 0`
    // gate; only the trailing `LIMIT 500` in `list_entities_visible` caps the roster.
    const SEEDED: usize = 520;
    for i in 0..SEEDED {
        let id = db
            .upsert_entity(&format!("Person {i:04}"), EntityKind::Person)
            .unwrap();
        db.add_mention(&id, "m1").unwrap();
    }

    let empty: HashSet<String> = HashSet::new();

    // No folder was ever locked, so the OLD disclosure signal stays false...
    assert!(
        !db.has_hidden_folders(&empty).unwrap(),
        "no folder is locked, so the locked-folder disclosure has nothing to report"
    );

    // ...yet the graph payload is truncated below the true 520, and now says so explicitly.
    let graph = db.build_graph(&empty).unwrap();
    assert!(!graph.has_hidden, "still no locked folder");
    assert_eq!(graph.nodes.len(), 500, "the render cap trims to 500 nodes");
    assert_eq!(
        graph.total_visible_entities, SEEDED as i64,
        "the TRUE visible-entity count is reported even though the roster is capped"
    );
    assert!(
        graph.total_visible_entities > graph.nodes.len() as i64,
        "total must exceed the capped roster so the FE can detect + disclose truncation"
    );

    // Same shape on `/people`: the roster is capped, but `total_visible_people` is the truth.
    let people = db.list_people(&empty).unwrap();
    assert_eq!(
        people.people.len(),
        500,
        "people roster is also capped at 500"
    );
    assert_eq!(
        people.total_visible_people, SEEDED as i64,
        "the TRUE visible-person count, independent of the render cap"
    );
    assert!(
        people.total_visible_people > people.people.len() as i64,
        "\"Show all N people\" must be able to disclose N > the capped roster length"
    );
}

#[test]
fn dual_sink_skips_vault_when_locked() {
    // Sink B gates on the meeting's folder `locked` flag (DISK truth, NOT session-unlock):
    // a meeting in a locked folder → DB rows still written (Sink A), but ZERO vault `.md`
    // stubs. An OPEN folder → both DB rows AND vault stubs. This mirrors the gate in
    // `commands::build_and_persist_entities` without invoking the LLM provider.
    let db = file_db("dualsink");
    let kek = crate::crypto::random_key().unwrap();
    let vault = temp_vault("dualsink");

    seed_folder(&db, "locked_f", "Locked");
    seed_folder(&db, "open_f", "Open");
    seed_note(&db, "m_locked", "# locked note", Some("locked_f"));
    seed_note(&db, "m_open", "# open note", Some("open_f"));
    seal_folder(&db, "locked_f", &kek); // locked_f now locked=true on disk

    // The dual-sink gate, replicated: Sink A always; Sink B only when folder NOT locked.
    let sink = |meeting_id: &str, person: &str| {
        // Sink A — always persist to the DB.
        let id = db.upsert_entity(person, EntityKind::Person).unwrap();
        db.add_mention(&id, meeting_id).unwrap();
        // Sink B — vault stub only if the meeting's folder is unsealed on disk.
        let folder_locked = match db
            .get_meeting(meeting_id)
            .unwrap()
            .and_then(|m| m.folder_id)
        {
            Some(fid) => db
                .folder_by_id(&fid)
                .unwrap()
                .map(|f| f.locked)
                .unwrap_or(false),
            None => false,
        };
        if !folder_locked {
            crate::export::entity_stub::ensure_entity_backlink(
                &vault,
                "People",
                person,
                &format!("title-{meeting_id}"),
            )
            .unwrap();
        }
    };

    sink("m_locked", "Locked Person");
    sink("m_open", "Open Person");

    // Sink A: BOTH entities are in the DB regardless of lock state.
    // (Read with the locked folder session-unlocked so both meetings are visible for the
    //  assertion — Sink A wrote rows for both either way.)
    let unlocked = unlocked_set(&["locked_f"]);
    let nodes = db.list_entities_visible(&unlocked).unwrap();
    assert!(
        nodes.iter().any(|n| n.name == "Locked Person"),
        "Sink A: DB row written even for a locked-folder meeting"
    );
    assert!(nodes.iter().any(|n| n.name == "Open Person"));

    // Sink B: the OPEN folder's entity has a vault stub; the LOCKED folder's does NOT.
    let open_stub = vault.join("People").join("Open Person.md");
    let locked_stub = vault.join("People").join("Locked Person.md");
    assert!(open_stub.exists(), "open folder → vault stub written");
    assert!(
        !locked_stub.exists(),
        "locked folder → NO vault stub (no plaintext leak to disk)"
    );
}

#[test]
fn no_vault_configured_db_sink_still_works() {
    // Sink A must work with NO vault: no error, DB rows written, no stubs (no vault dir).
    let db = file_db("novault");
    seed_note(&db, "m1", "# note", None);
    let id = db.upsert_entity("Some Person", EntityKind::Person).unwrap();
    db.add_mention(&id, "m1").unwrap();
    let nodes = db.list_entities_visible(&HashSet::new()).unwrap();
    assert!(nodes.iter().any(|n| n.id == id));
}

#[test]
fn cascade_prunes_mentions_and_entity_drops_out() {
    // delete_meeting cascades to entity_mentions (FK ON DELETE CASCADE); an entity with zero
    // remaining mentions disappears from list_entities_visible (HAVING count > 0).
    let db = file_db("cascade");
    seed_note(&db, "m1", "# note", None);
    let id = db.upsert_entity("Solo Person", EntityKind::Person).unwrap();
    db.add_mention(&id, "m1").unwrap();
    assert!(db
        .list_entities_visible(&HashSet::new())
        .unwrap()
        .iter()
        .any(|n| n.id == id));

    db.delete_meeting("m1").unwrap();
    assert!(
        !db.list_entities_visible(&HashSet::new())
            .unwrap()
            .iter()
            .any(|n| n.id == id),
        "entity with no remaining mentions drops out"
    );
    // The entity row itself remains (orphan), but contributes nothing — harmless.
    assert!(db.get_entity(&id).unwrap().is_some());
}

#[test]
fn entity_detail_neighbors_and_backlinks() {
    // build_entity_detail returns the entity, its visible backlinked meetings, and its top
    // co-occurring neighbors ranked by shared visible meetings.
    let db = file_db("detail");
    seed_note(&db, "m1", "# note", None);
    seed_note(&db, "m2", "# note", None);
    let anna = db.upsert_entity("Anna", EntityKind::Person).unwrap();
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    let bob = db.upsert_entity("Bob", EntityKind::Person).unwrap();
    // Anna+Atlas co-occur in m1 AND m2 (weight 2); Anna+Bob only in m1 (weight 1).
    for (e, m) in [
        (&anna, "m1"),
        (&anna, "m2"),
        (&atlas, "m1"),
        (&atlas, "m2"),
        (&bob, "m1"),
    ] {
        db.add_mention(e, m).unwrap();
    }
    let detail = db
        .build_entity_detail(&anna, &HashSet::new(), 12)
        .unwrap()
        .unwrap();
    assert_eq!(detail.entity.name, "Anna");
    assert_eq!(detail.meetings.len(), 2, "Anna backlinks m1 + m2");
    assert_eq!(
        detail.neighbors.first().unwrap().id,
        atlas,
        "Atlas is the top neighbor (shared 2)"
    );
    assert_eq!(detail.neighbors.first().unwrap().shared_meetings, 2);
    assert!(detail.neighbors.iter().any(|n| n.id == bob));
    // Unknown id → None.
    assert!(db
        .build_entity_detail("nope", &HashSet::new(), 12)
        .unwrap()
        .is_none());
}

#[test]
fn entity_detail_hidden_when_only_sealed() {
    // PRIME-DIRECTIVE anti-leak: an entity mentioned ONLY in a sealed-not-unlocked meeting
    // must NEVER surface via get_entity_detail. The leak this guards: the FE held the entity
    // id from a PRIOR open-folder get_graph; the folder is then sealed (or auto-relocked on
    // screen-share); a subsequent get_entity_detail(id) must NOT return the entity — its
    // `name` lived only in the sealed meeting's encrypted markdown. The detail returns None
    // while sealed, and reappears only once the folder id is in the session `unlocked` set.
    let db = file_db("detail-sealed");
    let kek = crate::crypto::random_key().unwrap();
    seed_folder(&db, "secret", "Secret");
    seed_note(&db, "sealed1", "# sealed", Some("secret"));

    let secret_p = db
        .upsert_entity("Secret Person", EntityKind::Person)
        .unwrap();
    db.add_mention(&secret_p, "sealed1").unwrap();

    let empty: HashSet<String> = HashSet::new();

    // While OPEN: detail is available (sanity — proves the test wires a real, resolvable id).
    let open_detail = db.build_entity_detail(&secret_p, &empty, 12).unwrap();
    assert!(open_detail.is_some(), "open folder → detail resolves");

    // SEAL, session NOT unlocked.
    seal_folder(&db, "secret", &kek);
    assert!(
            db.build_entity_detail(&secret_p, &empty, 12).unwrap().is_none(),
            "entity only in a sealed-not-unlocked meeting must NOT surface via get_entity_detail \
             (its name lived only in the sealed meeting) — must be None, not an empty-backlink shell"
        );

    // SESSION-UNLOCK the folder id → the entity (and its visible backlinks) reappear.
    let unlocked = unlocked_set(&["secret"]);
    let detail = db
        .build_entity_detail(&secret_p, &unlocked, 12)
        .unwrap()
        .expect("entity detail reappears once its folder id is in the unlocked set");
    assert_eq!(detail.entity.name, "Secret Person");
    assert_eq!(
        detail.meetings.len(),
        1,
        "the (now visible) sealed meeting backlinks"
    );
}
