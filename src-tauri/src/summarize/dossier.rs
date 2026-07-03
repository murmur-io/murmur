//! Entity DOSSIER (brain2 Phase 5b) — the flagship cross-meeting synthesis artifact: the
//! "state of [[Project Atlas]]" / "what's the story with [[Anna]]" answer. Assembles, from the
//! encrypted DB, everything Murmur knows about ONE entity — its mentioning meetings, its open
//! commitments, and its co-occurring neighbours — into a citation-tagged corpus, then asks a
//! provider to synthesize four cited sections (Overview · 🕑 Timeline · ⏳ Open commitments ·
//! 🧭 Last said / next step), every claim citing its `[[Title]]`.
//!
//! Two consumption shapes over the SAME gated data builder:
//!   * the Tauri `entity_dossier` command → cloud synthesis (firewall + consent gate, like
//!     `ask_vault`),
//!   * the MCP `get_entity_dossier` tool → EGRESS-FREE: returns the gated STRUCTURED DATA for the
//!     client (Claude Desktop) to synthesize locally — the MCP server NEVER makes a cloud call.
//!
//! ANTI-LEAK INVARIANT (lock-model): every read here goes through a visibility-gated Db method
//! against the live `unlocked` session set — `entity_is_visible` / `entity_mentions_visible` /
//! `get_note_if_visible` / `list_open_commitments` / `entity_neighbors_visible`. A
//! sealed-and-not-session-unlocked mentioning meeting contributes NOTHING: not its title, not its
//! note body, not its commitments. Identical to the `vault_context` / digest corpus gate.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::facts::Fact;
use crate::storage::models::{Commitment, EntityNeighbor, GraphEntity, VaultSource};
use crate::storage::Db;
use crate::summarize::template::language_directive;

/// Max co-occurring neighbours surfaced in a dossier.
const DOSSIER_NEIGHBOR_LIMIT: i64 = 12;

/// Char budget for the synthesized corpus, by provider. Mirrors `vault_context`/`digest`: local
/// quantized models (Ollama) have tiny context windows, so cap tight; cloud/Claude models get
/// headroom. Always bounded so a name-dense entity can't blow the prompt.
fn budget_for(provider_id: &str) -> usize {
    if provider_id == "ollama" {
        4_000
    } else {
        80_000
    }
}

/// The gated, deterministic dossier payload for one entity. Built entirely from visibility-gated
/// Db reads — a sealed-not-unlocked meeting contributes nothing. The `corpus` is the
/// citation-tagged note material (each meeting headed `### [[Title]] · date · id:`), ready to feed
/// a provider (cloud command) or hand to an MCP client (egress-free) for synthesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DossierData {
    pub entity: GraphEntity,
    /// Visible meetings mentioning this entity (newest first), as `[[Title]]` citation chips.
    pub meetings: Vec<VaultSource>,
    /// Open commitments tied to this entity: an OPEN item from one of the entity's visible
    /// mentioning meetings, OR an OPEN item whose owner matches the entity name.
    pub commitments: Vec<Commitment>,
    /// Top co-occurring neighbour entities (shared visible meetings).
    pub neighbors: Vec<EntityNeighbor>,
    /// brain2 R2 — the entity's VISIBLE bitemporal facts (open + recently-closed), newest first.
    /// Open facts (`valid_to == None`) are the CURRENT state; closed facts are WHAT CHANGED. Gated
    /// by `list_facts_visible` — a sealed-not-unlocked meeting's facts are absent.
    pub facts: Vec<Fact>,
    /// Citation-tagged note corpus, each meeting headed `### [[Title]] · date · id:`.
    /// `#[serde(skip)]`: an internal synthesis input (the largest field — full visible note bodies)
    /// consumed only by `render_dossier_user`/`format_dossier_client` via direct field access. It
    /// NEVER crosses IPC — the structured FE render (`get_person_dossier`) needs only
    /// entity/meetings/commitments/neighbors/facts. `String: Default` satisfies the skip on deser.
    #[serde(skip)]
    pub corpus: String,
}

/// Resolve an `entity` token (an entity id OR a name) to a VISIBLE entity id, or `None`.
///
/// Gated: an entity mentioned ONLY in sealed-and-not-unlocked meetings is invisible and never
/// resolves (its name lived only in encrypted markdown). First tries `entity` as an id
/// (`entity_is_visible`); failing that, resolves it as a name via the deterministic, gated
/// `entities_matching_query` and picks the visible candidate with the most visible mentions.
pub fn resolve_entity_id(
    db: &Db,
    entity: &str,
    unlocked: &HashSet<String>,
) -> Result<Option<String>> {
    let entity = entity.trim();
    if entity.is_empty() {
        return Ok(None);
    }
    // 1) Treat as an entity id (the FE passes ids straight from `get_graph`). Gate it.
    if db.entity_is_visible(entity, unlocked)? {
        return Ok(Some(entity.to_string()));
    }
    // 2) Treat as a name. `entities_matching_query` is gated by the SAME visibility predicate, so
    //    only visible candidates come back. Among them, pick the one with the most VISIBLE
    //    mentions (the "strongest" match) for determinism.
    let candidates = db.entities_matching_query(entity, unlocked)?;
    if candidates.is_empty() {
        return Ok(None);
    }
    let counts: std::collections::HashMap<String, i64> = db
        .list_entities_visible(unlocked)?
        .into_iter()
        .map(|n| (n.id, n.mention_count))
        .collect();
    let best = candidates
        .into_iter()
        .max_by_key(|id| counts.get(id).copied().unwrap_or(0));
    Ok(best)
}

/// Build the gated dossier payload for `entity_id`. `None` when the entity is unknown OR has ZERO
/// visible mentions (mentioned only in sealed-not-unlocked meetings) — indistinguishable from an
/// unknown id, exactly like [`Db::build_entity_detail`].
///
/// All four data sources are visibility-gated against `unlocked`:
///   * `entity_is_visible` (anti-leak gate FIRST) + `get_entity` (raw row, only after the gate),
///   * `entity_mentions_visible` → the visible mentioning meetings,
///   * `list_open_commitments` (itself double-gated) filtered to this entity,
///   * `entity_neighbors_visible` → co-occurring neighbours,
///   * `get_note_if_visible` (SECOND gate) for each meeting's note body in the corpus.
pub fn build_dossier_data(
    db: &Db,
    entity_id: &str,
    unlocked: &HashSet<String>,
) -> Result<Option<DossierData>> {
    // Anti-leak gate FIRST: a sealed-only entity is indistinguishable from an unknown id.
    if !db.entity_is_visible(entity_id, unlocked)? {
        return Ok(None);
    }
    let entity = match db.get_entity(entity_id)? {
        Some(e) => e,
        None => return Ok(None),
    };

    let meetings = db.entity_mentions_visible(entity_id, unlocked)?;
    let neighbors = db.entity_neighbors_visible(entity_id, unlocked, DOSSIER_NEIGHBOR_LIMIT)?;
    // brain2 R2 — the entity's VISIBLE bitemporal facts. Same `unlocked` gate as every other read
    // here: a sealed-not-unlocked meeting's facts (its source meeting) never surface.
    let facts = db.list_facts_visible(entity_id, unlocked)?;

    // The visible meetings that mention this entity, as an id set for the commitment filter.
    let mention_ids: HashSet<&str> = meetings.iter().map(|m| m.meeting_id.as_str()).collect();
    let name_lc = entity.name.trim().to_lowercase();
    // `list_open_commitments` is itself double-gated (list_meetings_visible + get_note_if_visible),
    // so every candidate already comes from a visible meeting. Keep an item iff it belongs to one
    // of this entity's mentioning meetings OR is owned by this entity (name match, case-insensitive).
    let commitments: Vec<Commitment> = db
        .list_open_commitments(unlocked, None)?
        .into_iter()
        .filter(|c| {
            mention_ids.contains(c.meeting_id.as_str())
                || c.owner
                    .as_deref()
                    .map(|o| o.trim().to_lowercase() == name_lc)
                    .unwrap_or(false)
        })
        .collect();

    // Citation-tagged corpus: each meeting headed `### [[Title]] · date · id:`, body via the SECOND
    // gate `get_note_if_visible` (None → skip; never reads a sealed note's stale plaintext).
    let mut corpus = String::new();
    for m in &meetings {
        let Some(note) = db.get_note_if_visible(&m.meeting_id, unlocked)? else {
            continue;
        };
        let title = if m.title.is_empty() {
            "(untitled)".to_string()
        } else {
            m.title.clone()
        };
        let date = m
            .started_at
            .split(['T', ' '])
            .next()
            .unwrap_or("")
            .to_string();
        corpus.push_str(&format!(
            "\n\n### [[{title}]] · {date} · id:{}\n",
            m.meeting_id
        ));
        corpus.push_str(&note.markdown);
    }

    Ok(Some(DossierData {
        entity,
        meetings,
        commitments,
        neighbors,
        facts,
        corpus,
    }))
}

/// The system prompt: instruct the model to emit the four cited dossier sections. Mirrors
/// `digest::build_digest_prompt` (keep the emoji; cite `[[Title]]` exactly; never invent).
pub fn dossier_system_prompt(note_language: &str) -> String {
    format!(
        "You write ONE Obsidian DOSSIER note answering 'what is the state of this entity?' across \
several meetings. Base everything ONLY on the meeting notes provided (each headed by \
`### [[Title]] · date · id:`) and the structured TIMELINE / OPEN COMMITMENTS / CO-OCCURRING \
sections. Output clean, scannable Markdown with EXACTLY these sections (keep the emoji):\n\
- a 1-2 sentence **Overview** of where things stand with this entity,\n\
- ## 🕑 Timeline of mentions — chronological, each entry citing its [[Title]],\n\
- ## ⏳ Open commitments — who owes what (owner · due date · item), each citing its [[Title]],\n\
- ## 🧭 Last said / next step — the latest state and the concrete next step, citing [[Title]].\n\
Cite EVERY claim with the source [[Title]] exactly as given. Never invent facts, decisions, \
owners, or dates. Do not emit YAML front-matter.\n\n{lang}",
        lang = language_directive(note_language)
    )
}

/// Render the structured (deterministic, gated) dossier sections — timeline, open commitments,
/// co-occurring neighbours — each citing `[[Title]]`. Shared by the cloud user-prompt and the
/// egress-free MCP client payload so the citation format is identical.
fn render_structured(data: &DossierData) -> String {
    let kind = format!("{:?}", data.entity.kind).to_lowercase();
    let mut s = format!("ENTITY: {} ({kind})\n", data.entity.name);

    // brain2 R2 — CURRENT FACTS (open: valid_to == None) and WHAT CHANGED (closed: superseded).
    let current: Vec<&Fact> = data.facts.iter().filter(|f| f.valid_to.is_none()).collect();
    let changed: Vec<&Fact> = data.facts.iter().filter(|f| f.valid_to.is_some()).collect();
    s.push_str("\nCURRENT FACTS (as of the latest meeting):\n");
    if current.is_empty() {
        s.push_str("(none)\n");
    } else {
        for f in &current {
            let date = f.valid_from.split(['T', ' ']).next().unwrap_or("");
            s.push_str(&format!(
                "- {} {}: {} (since {date})\n",
                f.subject.trim(),
                f.predicate.trim(),
                f.object.trim()
            ));
        }
    }
    s.push_str("\nWHAT CHANGED (superseded facts — history):\n");
    if changed.is_empty() {
        s.push_str("(none)\n");
    } else {
        for f in &changed {
            let from = f.valid_from.split(['T', ' ']).next().unwrap_or("");
            let to = f
                .valid_to
                .as_deref()
                .and_then(|t| t.split(['T', ' ']).next())
                .unwrap_or("");
            s.push_str(&format!(
                "- {} {}: was \"{}\" ({from} → {to})\n",
                f.subject.trim(),
                f.predicate.trim(),
                f.object.trim()
            ));
        }
    }

    s.push_str("\nTIMELINE OF MENTIONS (newest first):\n");
    if data.meetings.is_empty() {
        s.push_str("(none)\n");
    } else {
        for m in &data.meetings {
            let title = if m.title.is_empty() {
                "(untitled)"
            } else {
                &m.title
            };
            let date = m.started_at.split(['T', ' ']).next().unwrap_or("");
            s.push_str(&format!("- [[{title}]] · {date} · id:{}\n", m.meeting_id));
        }
    }

    s.push_str("\nOPEN COMMITMENTS:\n");
    if data.commitments.is_empty() {
        s.push_str("(none)\n");
    } else {
        for c in &data.commitments {
            let mut parts: Vec<String> = Vec::new();
            if let Some(o) = c.owner.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
                parts.push(o.to_string());
            }
            if let Some(d) = c.due_date.as_deref().filter(|d| !d.is_empty()) {
                parts.push(format!("due {d}"));
            }
            parts.push(format!("\"{}\"", c.text.trim()));
            parts.push(format!("[[{}]]", c.meeting_title));
            s.push_str(&format!("- {}\n", parts.join(" · ")));
        }
    }

    s.push_str("\nCO-OCCURRING ENTITIES:\n");
    if data.neighbors.is_empty() {
        s.push_str("(none)\n");
    } else {
        for n in &data.neighbors {
            let kind = format!("{:?}", n.kind).to_lowercase();
            s.push_str(&format!(
                "- {} ({kind}) · {} shared meetings\n",
                n.name, n.shared_meetings
            ));
        }
    }
    s
}

/// Char-cap a corpus on a char boundary (never splits a multibyte char), like `vault_context`.
fn cap(corpus: &str, budget: usize) -> String {
    corpus.chars().take(budget).collect()
}

/// Build the (system, user) prompt for the CLOUD `entity_dossier` command. The user message packs
/// the structured sections + the citation-tagged note corpus, capped to the provider budget.
pub fn render_dossier_user(data: &DossierData, provider_id: &str) -> String {
    let budget = budget_for(provider_id);
    let mut out = render_structured(data);
    out.push_str("\nMEETING NOTES (each headed ### [[Title]] · date · id:):\n");
    let remaining = budget.saturating_sub(out.len());
    out.push_str(&cap(&data.corpus, remaining));
    out
}

/// Build the EGRESS-FREE MCP client payload: a one-line overview header + the gated structured
/// sections + the citation-tagged note corpus, for the client (Claude Desktop) to synthesize the
/// four dossier sections locally. NO provider/cloud call is made on this path.
pub fn format_dossier_client(data: &DossierData) -> String {
    // A generous but bounded cap so a huge entity can't produce an unbounded MCP payload.
    let budget = 200_000usize;
    let overview = format!(
        "DOSSIER for [[{}]] — {} mentioning meeting(s), {} open commitment(s), {} related entity(ies).\n",
        data.entity.name,
        data.meetings.len(),
        data.commitments.len(),
        data.neighbors.len()
    );
    let mut out = overview;
    out.push('\n');
    out.push_str(&render_structured(data));
    out.push_str("\nMEETING NOTES (each headed ### [[Title]] · date · id:):\n");
    let remaining = budget.saturating_sub(out.len());
    out.push_str(&cap(&data.corpus, remaining));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp_db() -> Db {
        let p = crate::storage::db::unique_temp_path("meetnotes-dossier-test", "sqlite");
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    fn seed_meeting(db: &Db, id: &str, title: &str, md: &str, folder: Option<&str>) {
        db.insert_meeting(&Meeting {
            id: id.to_string(),
            started_at: "2026-06-26T09:00:00Z".to_string(),
            ended_at: None,
            title: Some(title.to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: id.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: md.to_string(),
            created_at: "2026-06-26T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(id, folder).unwrap();
    }

    fn seed_folder(db: &Db, id: &str) {
        db.insert_folder(&Folder {
            id: id.to_string(),
            name: id.to_string(),
            path: id.to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
    }

    /// build_dossier_data assembles the entity's VISIBLE mentioning meetings (with [[Title]] cites
    /// in the corpus), its open commitments, and its neighbours — and EXCLUDES a sealed-not-unlocked
    /// mentioning meeting, which reappears once the folder is session-unlocked. The owner/entity
    /// commitment filter works.
    #[test]
    fn build_dossier_gates_sealed_and_filters_commitments() {
        let db = temp_db();
        // Atlas (the dossier subject) + Anna co-occur in an OPEN meeting; Atlas is also mentioned
        // in a SEALED meeting. A commitment owned by "Atlas" lives in a third (open) meeting that
        // does NOT mention Atlas as an entity — caught only by the owner filter.
        seed_meeting(
            &db,
            "open1",
            "Kickoff",
            "## Action items\n- [ ] Anna — draft Atlas spec 2026-07-01\n- [x] done thing\n",
            None,
        );
        seed_folder(&db, "f-lock");
        seed_meeting(
            &db,
            "sealedX",
            "Secret Atlas Review",
            "LOCKED Atlas acquisition price\n## Action items\n- [ ] Carol — sign 2026-07-09\n",
            Some("f-lock"),
        );
        seed_meeting(
            &db,
            "ownerm",
            "Owner Meeting",
            "## Action items\n- [ ] Atlas — own the rollout 2026-07-05\n",
            None,
        );

        let atlas = db
            .upsert_entity("Atlas", crate::storage::models::EntityKind::Project)
            .unwrap();
        let anna = db
            .upsert_entity("Anna", crate::storage::models::EntityKind::Person)
            .unwrap();
        // Atlas mentioned in open1 + sealedX; Anna co-occurs in open1.
        db.add_mention(&atlas, "open1").unwrap();
        db.add_mention(&atlas, "sealedX").unwrap();
        db.add_mention(&anna, "open1").unwrap();

        db.set_folder_locked("f-lock", true, None).unwrap();

        let nothing = HashSet::new();
        let data = build_dossier_data(&db, &atlas, &nothing).unwrap().unwrap();

        // Visible meeting present + cited; sealed meeting absent everywhere.
        assert!(data.meetings.iter().any(|m| m.meeting_id == "open1"));
        assert!(
            data.meetings.iter().all(|m| m.meeting_id != "sealedX"),
            "sealed-not-unlocked mentioning meeting leaked into dossier meetings"
        );
        assert!(data.corpus.contains("[[Kickoff]]"), "must cite the visible meeting [[Title]]");
        assert!(
            !data.corpus.contains("LOCKED Atlas acquisition"),
            "sealed note body leaked into the dossier corpus (gate violation)"
        );

        // Commitment filter: 'draft Atlas spec' is in a mentioning meeting (open1);
        // 'own the rollout' is owner==Atlas (owner filter). Carol's sealed item is excluded.
        assert!(data.commitments.iter().any(|c| c.text.contains("draft Atlas spec")));
        assert!(
            data.commitments.iter().any(|c| c.text.contains("own the rollout")),
            "owner==entity-name commitment must be included"
        );
        assert!(
            data.commitments.iter().all(|c| !c.text.contains("sign")),
            "sealed-not-unlocked commitment leaked (gate violation)"
        );
        assert!(
            data.commitments.iter().all(|c| !c.text.contains("done thing")),
            "checked-off item must not be an open commitment"
        );

        // Neighbours: Anna co-occurs with Atlas in the open meeting.
        assert!(data.neighbors.iter().any(|n| n.name == "Anna"));

        // Session-unlock the folder → the sealed meeting + its content reappear.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let data2 = build_dossier_data(&db, &atlas, &unlocked).unwrap().unwrap();
        assert!(data2.meetings.iter().any(|m| m.meeting_id == "sealedX"));
        assert!(data2.corpus.contains("LOCKED Atlas acquisition"), "unlocked content must reappear");
        assert!(data2.commitments.iter().any(|c| c.text.contains("sign")));
    }

    /// brain2 R2 — the dossier surfaces the entity's CURRENT FACTS (open) + WHAT CHANGED (closed),
    /// and they are GATED: a fact whose source meeting is sealed-not-unlocked is absent, reappearing
    /// on session-unlock. Both the structured payload (cloud + MCP) carries the rendered sections.
    #[test]
    fn dossier_surfaces_and_gates_bitemporal_facts() {
        use crate::facts::{FactOp, NewFact};
        let db = temp_db();
        seed_folder(&db, "f-lock");
        seed_meeting(&db, "open1", "Kickoff", "Atlas status", None);
        seed_meeting(&db, "sealedX", "Secret Review", "Atlas secret", Some("f-lock"));
        let atlas = db
            .upsert_entity("Atlas", crate::storage::models::EntityKind::Project)
            .unwrap();
        db.add_mention(&atlas, "open1").unwrap();
        db.add_mention(&atlas, "sealedX").unwrap();

        let mk = |object: &str, from: &str, mid: &str| {
            FactOp::Add(NewFact {
                entity_id: atlas.clone(),
                subject: "Atlas".into(),
                predicate: "status".into(),
                object: object.into(),
                valid_from: from.into(),
                recorded_at: from.into(),
                confidence: 1.0,
                meeting_id: Some(mid.into()),
            })
        };
        // An open fact from the OPEN meeting + a (sealed-meeting) fact that must be gated out.
        db.apply_fact_ops(&[mk("shipped", "2026-06-20T00:00:00Z", "open1")]).unwrap();
        db.apply_fact_ops(&[mk("price-secret", "2026-06-21T00:00:00Z", "sealedX")]).unwrap();
        db.set_folder_locked("f-lock", true, None).unwrap();

        let nothing = HashSet::new();
        let data = build_dossier_data(&db, &atlas, &nothing).unwrap().unwrap();
        // Open meeting's fact present; sealed meeting's fact absent (gate).
        assert!(data.facts.iter().any(|f| f.object == "shipped"));
        assert!(
            data.facts.iter().all(|f| f.object != "price-secret"),
            "sealed-meeting fact leaked into the dossier (gate violation)"
        );
        let rendered = render_structured(&data);
        assert!(rendered.contains("CURRENT FACTS"));
        assert!(rendered.contains("shipped"));
        assert!(!rendered.contains("price-secret"));

        // Session-unlock → the sealed fact reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let data2 = build_dossier_data(&db, &atlas, &unlocked).unwrap().unwrap();
        assert!(data2.facts.iter().any(|f| f.object == "price-secret"));
    }

    /// The prompt builder emits the four cited sections + respects the provider budget cap (mirrors
    /// the digest.rs prompt test).
    #[test]
    fn builds_dossier_prompt_with_sections_and_budget() {
        let system = dossier_system_prompt("auto");
        assert!(system.contains("🕑 Timeline of mentions"));
        assert!(system.contains("⏳ Open commitments"));
        assert!(system.contains("🧭 Last said / next step"));
        assert!(system.contains("Overview"));

        let data = DossierData {
            entity: GraphEntity {
                id: "e1".to_string(),
                name: "Atlas".to_string(),
                kind: crate::storage::models::EntityKind::Project,
                created_at: "2026-06-26T00:00:00Z".to_string(),
            },
            meetings: vec![VaultSource {
                meeting_id: "m1".to_string(),
                title: "Kickoff".to_string(),
                started_at: "2026-07-01T09:00:00Z".to_string(),
            }],
            commitments: vec![],
            neighbors: vec![],
            facts: vec![],
            corpus: "X".repeat(500_000),
        };
        // ollama → tight 4k budget caps the user message hard; the [[Title]] cite still renders.
        let user = render_dossier_user(&data, "ollama");
        assert!(user.contains("[[Kickoff]]"), "must cite [[Title]] in the user prompt");
        assert!(user.len() <= 4_000, "ollama user prompt must respect the tight budget");
        // Cloud provider gets headroom but is still bounded.
        let user_cloud = render_dossier_user(&data, "anthropic");
        assert!(user_cloud.len() <= 80_000);
        assert!(user_cloud.len() > 4_000, "cloud budget must give more headroom than ollama");
    }
}
