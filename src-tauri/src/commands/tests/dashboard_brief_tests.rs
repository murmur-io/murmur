//! THE BOARD-BRIEF ORACLE — what a board's DERIVED tiles contribute to its own Ask.
//!
//! The asymmetry this closes: `tools.rs::tool_specs` exposes `get_dashboard` to the
//! agentic loop and the local MCP server, so an external agent sees every tile. But
//! board Ask pins its sources, and `ask_vault` routes any non-empty pinned list to
//! the deterministic floor — skipping the agentic path and therefore that tool. The
//! in-app "Ask this board" saw strictly LESS of the board than Claude Code did.
//!
//! Every test here carries a CONTROL. A leak assertion that would also pass with the
//! gate deleted proves nothing, and this feature has already shipped one such gate:
//! the adversarial review of PR #562 found the answer-cache check was both incomplete
//! AND unfalsifiable — disabling it left all 2732 tests green.
//!
//! ## EVIDENCE MAP — every derived kind, proven at the PACKED PROVIDER PROMPT
//!
//! The negative that matters is not "the brief omits it" but "the prompt that LEAVES
//! the process omits it". Each row below asserts absence (or indistinguishability) on
//! `packed_scoped(..)` / `packed_user(..)` output — the sink itself, not
//! `dashboard_brief_inner`'s return value one layer earlier — and each carries a
//! control that flips it, so none can go vacuous.
//!
//! | kind | packed-sink negative | control that flips it |
//! | --- | --- | --- |
//! | promises | `a_sealed_row_never_reaches_the_packed_prompt` | unlock ⇒ row appears |
//! | reminders | `a_reminder_anchored_only_to_a_sealed_meeting_contributes_nothing` — `!packed.contains("CHASE THE SEALED THING")` | same marker PRESENT once unlocked |
//! | drift | `a_visible_entity_drift_carries_no_row_from_a_sealed_meeting` — `!packed_hidden.contains("SEALED-DATE-JUN-14")` | `packed_visible` DOES contain it |
//! | numbers | `a_visible_entity_carries_no_row_from_a_sealed_meeting` — `!packed_hidden.contains("SEALED-FIGURE-42")` | open-meeting figure still present |
//! | pulse | `a_visible_entity_pulse_counts_no_sealed_mention` — `packed_hidden == packed_baseline` | `packed_visible != packed_hidden` |
//! | person | `a_visible_person_counts_nothing_from_a_sealed_meeting` — `counts(packed_hidden) == counts(packed_baseline)` | `counts(packed_visible) != counts(packed_hidden)` |
//! | drift/numbers/pulse chrome | `a_hidden_entity_tile_leaks_no_timing` — `packed.is_empty()` | visible pulse ⇒ `packed.contains("Atlas")` |
//! | living answer | `a_withheld_living_answer_contributes_nothing` (+ unstamped LEGACY fail-closed) | stamped-and-readable renders |
//!
//! Pulse and person assert INDISTINGUISHABILITY rather than a missing substring on
//! purpose: their content is aggregates, so "the secret string is absent" passes
//! trivially. A count or a recency date discloses without a word of content.
//!
//! ## Honest RED limit
//!
//! The primary contract claim is not executable against the base commit: the board
//! brief sink and its function argument did not exist there, so the new prompt test
//! cannot compile on that tree. Its RED is structural. The lock-model claims remain
//! falsifiable through the paired sealed/unlocked controls above.
//!
use super::*;
use crate::storage::models::{
    EntityKind, Folder, Meeting, MeetingStatus, NoteRecord, ReminderDraft, ReminderOrigin,
    ReminderSourceAnchor,
};
use std::collections::HashSet;

const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"; // MURMUR_DEV_DEK placeholder

fn file_db(label: &str) -> crate::storage::Db {
    crate::storage::Db::open_with_key(
        &crate::storage::db::unique_temp_path(&format!("meetnotes-dash-brief-{label}"), "sqlite"),
        TEST_DEK,
    )
    .unwrap()
}

fn seed_folder(db: &crate::storage::Db, id: &str, locked: bool) {
    db.insert_folder(&Folder {
        id: id.to_string(),
        name: id.to_string(),
        path: id.to_string(),
        parent_id: None,
        locked,
        created_at: "2026-08-01T00:00:00Z".to_string(),
    })
    .unwrap();
}

/// A meeting whose note carries an Action-items section, so `list_open_commitments`
/// (which the `promises` tile reads) has something real to find.
fn seed_meeting_with_commitment(
    db: &crate::storage::Db,
    meeting_id: &str,
    folder_id: &str,
    task: &str,
) {
    db.insert_meeting(&Meeting {
        id: meeting_id.to_string(),
        started_at: "2026-08-01T09:00:00Z".to_string(),
        ended_at: None,
        title: Some("standup".to_string()),
        duration_s: 600,
        audio_path: None,
        status: MeetingStatus::Summarized,
        folder_id: Some(folder_id.to_string()),
    })
    .unwrap();
    db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.to_string(),
        provider_id: "test".to_string(),
        markdown: format!("## Action items\n- [ ] Marcus — {task} (due 2026-07-22)\n"),
        created_at: "2026-08-01T09:00:00Z".to_string(),
        exported_path: None,
        model_requested: None,
        model_served: None,
        gateway_host: None,
    })
    .unwrap();
    db.set_note_folder(meeting_id, Some(folder_id)).unwrap();
}

fn tile(id: &str, kind: &str, ref_id: Option<&str>, position: i64) -> DashboardTile {
    DashboardTile {
        id: id.to_string(),
        dashboard_id: "b1".to_string(),
        kind: kind.to_string(),
        ref_id: ref_id.map(|s| s.to_string()),
        title: None,
        span: 4,
        position,
        config: None,
        created_at: "2026-08-01T09:00:00Z".to_string(),
    }
}

/// RED-BEFORE-GREEN. The board's own suggested question is "Who owes me something on
/// this board?" — and before this, the model could not see the ledger that answers it.
#[test]
fn a_promise_ledger_reaches_the_prompt() {
    let db = file_db("promises");
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m1", "f-open", "send the Acme paperwork");

    let brief = dashboard_brief_inner(
        &db,
        vec![tile("t1", "promises", None, 0)],
        &HashSet::new(),
        MAX_BRIEF_CHARS,
    )
    .unwrap();

    assert!(
        brief.contains("send the Acme paperwork"),
        "the ledger row the user is looking at must reach the prompt: {brief}"
    );
    assert!(
        brief.starts_with(BRIEF_HEADER),
        "the brief must be labelled, or the model re-derives what it was handed"
    );
}

/// THE LEAK ORACLE. A sealed source contributes NOTHING — and the control proves the
/// same assertion catches a leak when one exists.
#[test]
fn a_sealed_source_contributes_nothing_to_the_brief() {
    let db = file_db("sealed");
    seed_folder(&db, "f-sealed", true);
    seed_meeting_with_commitment(&db, "m-sealed", "f-sealed", "the sealed obligation");

    let tiles = vec![tile("t1", "promises", None, 0)];

    let brief =
        dashboard_brief_inner(&db, tiles.clone(), &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    assert!(
        !brief.contains("the sealed obligation"),
        "a sealed folder's commitment must never reach a prompt: {brief}"
    );

    // CONTROL — session-unlock the folder and the SAME assertion finds the row, so the
    // check above is testing the gate rather than a seeding mistake.
    let unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
    let brief = dashboard_brief_inner(&db, tiles, &unlocked, MAX_BRIEF_CHARS).unwrap();
    assert!(
        brief.contains("the sealed obligation"),
        "unlocking must restore it, or the leak test is vacuous: {brief}"
    );
}

/// An entity-anchored tile whose entity stopped being visible degrades to an EMPTY
/// VIEW rather than to `Locked`, so its payload still carries `weekly`/`total`/
/// `quiet_days`. Emitting "quiet for 9d" would leak TIMING about a sealed entity
/// without ever printing its name.
#[test]
fn a_hidden_entity_tile_leaks_no_timing() {
    let db = file_db("hidden-entity");
    let tiles = vec![
        tile("t1", "pulse", Some("e-gone"), 0),
        tile("t2", "drift", Some("e-gone"), 1),
        tile("t3", "numbers", Some("e-gone"), 2),
    ];
    let brief = dashboard_brief_inner(&db, tiles, &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    assert!(
        brief.is_empty(),
        "an invisible entity's tiles contribute nothing at all: {brief}"
    );
    let packed = packed_scoped(&db, &HashSet::new(), Some(&brief), None).unwrap_or_default();
    assert!(
        packed.is_empty(),
        "withheld drift/numbers/pulse chrome must stay absent at the provider sink: {packed}"
    );

    // CONTROL — a VISIBLE entity's pulse tile does render, so "empty" above is the
    // withheld predicate and not simply a brief that never emits entity tiles.
    //
    // The mention is REQUIRED, not decoration: `list_entities_visible` INNER JOINs
    // `entity_mentions`, so an entity nobody has mentioned is invisible by
    // construction and `entity_name` yields the hidden placeholder for it. Without
    // this the control would fail for a reason that has nothing to do with the gate.
    let db2 = file_db("visible-entity");
    seed_folder(&db2, "f-open", false);
    seed_meeting_with_commitment(&db2, "m1", "f-open", "ship it");
    let atlas = db2.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db2.add_mention(&atlas, "m1").unwrap();
    let brief = dashboard_brief_inner(
        &db2,
        vec![tile("t1", "pulse", Some(&atlas), 0)],
        &HashSet::new(),
        MAX_BRIEF_CHARS,
    )
    .unwrap();
    assert!(
        brief.contains("Atlas"),
        "a visible entity's tile does reach the prompt: {brief}"
    );
    let packed = packed_scoped(&db2, &HashSet::new(), Some(&brief), None)
        .expect("a visible pulse must reach the provider sink");
    assert!(
        packed.contains("Atlas"),
        "the control must reach the provider sink, or the packed negative is vacuous: {packed}"
    );
}

/// MATERIAL tiles are already packed as SOURCES with their full text. Repeating a
/// snippet in the brief would spend budget to say something the corpus says better.
#[test]
fn material_tiles_are_not_repeated_in_the_brief() {
    let db = file_db("material");
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m1", "f-open", "ship it");

    let tiles = vec![
        tile("t1", "note", Some("n1"), 0),
        tile("t2", "meeting", Some("m1"), 1),
        tile("t3", "document", Some("d1"), 2),
    ];
    assert!(
        dashboard_brief_inner(&db, tiles, &HashSet::new(), MAX_BRIEF_CHARS)
            .unwrap()
            .is_empty(),
        "note/meeting/document ride the corpus, not the brief"
    );
}

/// An empty board yields an EMPTY string, not a bare header. A header with nothing
/// under it tells the model the user composed views that do not exist.
#[test]
fn a_board_with_no_derived_tiles_yields_nothing() {
    let db = file_db("empty");
    assert!(
        dashboard_brief_inner(&db, vec![], &HashSet::new(), MAX_BRIEF_CHARS)
            .unwrap()
            .is_empty()
    );
}

/// The cap stops at a whole tile. Half a promise row is worse than no row, because
/// the model cannot tell it was truncated.
#[test]
fn the_brief_is_bounded_and_never_cut_mid_row() {
    let db = file_db("cap");
    seed_folder(&db, "f-open", false);
    for i in 0..40 {
        seed_meeting_with_commitment(
            &db,
            &format!("m{i}"),
            "f-open",
            &format!("obligation number {i} with a deliberately long tail of words"),
        );
    }
    let tiles: Vec<_> = (0..40)
        .map(|i| tile(&format!("t{i}"), "promises", None, i))
        .collect();
    let brief = dashboard_brief_inner(&db, tiles, &HashSet::new(), MAX_BRIEF_CHARS).unwrap();

    assert!(
        brief.chars().count() <= MAX_BRIEF_CHARS,
        "the brief must stay inside its cap, got {}",
        brief.chars().count()
    );
    // Truncation happens between tiles, so the last line is a complete rendered row.
    assert!(
        !brief.ends_with('·') && !brief.ends_with(' '),
        "the brief must not end mid-row: {:?}",
        brief.chars().rev().take(40).collect::<String>()
    );
}

// ── THE PACKED-PROMPT LAYER ────────────────────────────────────────────────────
//
// Both reviews landed the same MAJOR on the first cut of this change: every test
// above asserts on `dashboard_brief_inner`'s return value, which is an INTERMEDIATE
// string. None of them drove `build_ask_vault_floor_prompt`, so the whole wiring —
// the scope trigger, the allowance, the tile-boundary truncation, the join with the
// corpus, and the interaction with the empty-corpus early return — was unexercised.
// An invariant proven one layer below the one the model actually reads is not proven.

// `commands/mod.rs` mounts ask.rs as `ask_commands` and re-exports it, so the floor
// builder is reachable from this nested test module the same way `ask_vault_tests`
// reaches it.
use crate::commands::{build_ask_vault_floor_prompt, AskFloorPrompt};
use crate::settings::AppConfig;

fn packed_user(
    db: &crate::storage::Db,
    unlocked: &HashSet<String>,
    brief: &str,
    sources: Option<&[SourceRef]>,
) -> Option<String> {
    packed_scoped(db, unlocked, Some(brief), sources)
}

/// `None` = not asked from a board at all; `Some("")` = a board with nothing readable.
fn packed_scoped(
    db: &crate::storage::Db,
    unlocked: &HashSet<String>,
    brief: Option<&str>,
    sources: Option<&[SourceRef]>,
) -> Option<String> {
    packed_with_provider(db, unlocked, brief, sources, "claude_code")
}

fn packed_with_provider(
    db: &crate::storage::Db,
    unlocked: &HashSet<String>,
    brief: Option<&str>,
    sources: Option<&[SourceRef]>,
    provider_id: &str,
) -> Option<String> {
    let cfg = AppConfig {
        semantic_search_enabled: false,
        provider_id: provider_id.to_string(),
        cloud_egress_consented: true,
        ..AppConfig::default()
    };
    match build_ask_vault_floor_prompt(
        db,
        &cfg,
        unlocked,
        "who owes me something on this board?",
        &[],
        "",
        None,
        sources,
        None,
        brief,
    )
    .unwrap()
    {
        // BOTH halves. `vault_chat::build` packs the corpus into the SYSTEM message and
        // leaves the user turn as the bare question, so asserting on `user` alone would
        // pass vacuously no matter what the brief did.
        AskFloorPrompt::Ready { system, user, .. } => Some(format!(
            "{system}
{user}"
        )),
        AskFloorPrompt::Empty(_) => None,
    }
}

/// The ledger row reaches the PACKED PROMPT, on a board whose tiles are ALL derived —
/// no note, no meeting, no document, therefore no `SourceRef` at all. That board is
/// exactly the one the first cut of this change still failed on.
///
/// ITS RED STATE IS STRUCTURAL, NOT AN EXECUTED FAILING RUN, and that distinction is
/// worth stating rather than letting "RED-before-GREEN" be read as more than it is:
/// this test cannot be compiled against the base commit, because neither
/// `dashboard_brief_inner` nor `build_ask_vault_floor_prompt`'s tenth argument exists
/// there. Pre-patch the sink was absent, so a derived tile could not reach the prompt
/// by construction. The EXECUTED red-before-green in this change is the routing bug
/// the reviews caught mid-flight — a derived-only board silently skipping the floor —
/// which `an_all_withheld_board_never_falls_back_to_the_whole_vault` now pins.
#[test]
fn a_derived_only_board_packs_its_ledger_into_the_prompt() {
    let db = file_db("packed-derived-only");
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m1", "f-open", "send the Acme paperwork");

    let brief = dashboard_brief_inner(
        &db,
        vec![tile("t1", "promises", None, 0)],
        &HashSet::new(),
        MAX_BRIEF_CHARS,
    )
    .unwrap();

    assert!(!brief.is_empty(), "fixture brief must be non-empty");
    let user = packed_user(&db, &HashSet::new(), &brief, None)
        .expect("a board with only derived tiles must still produce a prompt");

    assert!(
        user.contains("send the Acme paperwork"),
        "the ledger row must survive into the packed prompt: {user}"
    );
    assert!(
        user.contains(BRIEF_HEADER),
        "the label must survive truncation, or the model re-derives the ledger: {user}"
    );
}

/// THE PACKED-PROMPT LEAK ORACLE, with a control. The brief is the only thing that
/// can put a derived tile's content into a model call, so the negative belongs here
/// and not only one layer down.
#[test]
fn a_sealed_row_never_reaches_the_packed_prompt() {
    let db = file_db("packed-sealed");
    seed_folder(&db, "f-sealed", true);
    seed_meeting_with_commitment(&db, "m-sealed", "f-sealed", "the sealed obligation");
    let tiles = vec![tile("t1", "promises", None, 0)];

    let brief =
        dashboard_brief_inner(&db, tiles.clone(), &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    let packed = packed_user(&db, &HashSet::new(), &brief, None);
    assert!(
        packed
            .as_deref()
            .map(|u| !u.contains("the sealed obligation"))
            .unwrap_or(true),
        "a sealed folder's commitment must never reach a prompt: {packed:?}"
    );

    // CONTROL — unlocked, the SAME assertion finds it in the packed prompt, so the
    // negative above is the gate and not an empty fixture.
    let unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
    let brief = dashboard_brief_inner(&db, tiles, &unlocked, MAX_BRIEF_CHARS).unwrap();
    let user = packed_user(&db, &unlocked, &brief, None).expect("unlocked board packs a prompt");
    assert!(
        user.contains("the sealed obligation"),
        "unlocking must restore it, or the leak test is vacuous: {user}"
    );
}

/// A caller with no board is byte-identical to before — the equivalence that keeps
/// every other Ask surface (note editor, org viewer, vault-wide) untouched.
#[test]
fn an_empty_brief_leaves_the_packed_prompt_unchanged() {
    let db = file_db("packed-equivalence");
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m1", "f-open", "ship it");

    let sources = vec![SourceRef {
        kind: crate::links::LinkKind::Meeting,
        id: "m1".to_string(),
    }];
    let no_board = packed_scoped(&db, &HashSet::new(), None, Some(&sources));
    let with_empty = packed_user(&db, &HashSet::new(), "", Some(&sources));
    let with_blank = packed_user(&db, &HashSet::new(), "   \n  ", Some(&sources));
    assert_eq!(
        with_empty, with_blank,
        "a blank brief must not perturb the packed prompt"
    );
    // …and neither perturbs it relative to NO BOARD AT ALL. This is the contract's
    // byte-identity clause for the PINNED caller (the note editor), which the shipped
    // floor-equivalence test does not reach — it only compares the whole-vault search
    // path, where every scope pin is `None`.
    assert_eq!(
        no_board, with_empty,
        "with no board, a pinned-sources ask must pack byte-identically to before"
    );
    assert!(
        with_empty
            .as_deref()
            .map(|u| !u.contains(BRIEF_HEADER))
            .unwrap_or(false),
        "no board ⇒ no board header"
    );

    // The ORG-VIEWER caller too, not just the pinned-sources one. Both share the
    // `saturating_sub` and the `trim().is_empty()` join, so this is belt-and-braces —
    // but "shares the code path" is an argument, and the contract asks for equality.
    let org_none = build_ask_vault_floor_prompt(
        db_ref(&db),
        &cfg_for_test(),
        &HashSet::new(),
        "who owes me something on this board?",
        &[],
        "",
        None,
        None,
        Some("org-item-that-does-not-exist"),
        None,
    );
    let org_blank = build_ask_vault_floor_prompt(
        db_ref(&db),
        &cfg_for_test(),
        &HashSet::new(),
        "who owes me something on this board?",
        &[],
        "",
        None,
        None,
        Some("org-item-that-does-not-exist"),
        Some("   \n  "),
    );
    let org_empty = build_ask_vault_floor_prompt(
        db_ref(&db),
        &cfg_for_test(),
        &HashSet::new(),
        "who owes me something on this board?",
        &[],
        "",
        None,
        None,
        Some("org-item-that-does-not-exist"),
        Some(""),
    );
    // Among BOARD-SCOPED calls, a blank brief adds nothing — the claim this sub-case
    // is actually able to make.
    assert_eq!(
        org_blank.map(prompt_text).ok(),
        org_empty.map(prompt_text).ok(),
        "a blank brief must not perturb the org-pinned caller"
    );
    // A no-board call carries no board header. This REPLACED an assertion that
    // `None` and `Some("")` pack IDENTICALLY, which was true only by accident: with no
    // sources and a missing org item nothing packs, so both landed on the same canned
    // empty answer. Scope is carried by the PRESENCE of the board, so those two are
    // different callers — one has a board open and one does not — and they are now
    // distinguishable exactly where that matters: the board-scoped one says the board
    // is unreadable instead of telling the user to go record a meeting. Asserting them
    // equal would pin the misdirecting message in place.
    let org_none_text = org_none
        .map(prompt_text)
        .expect("org-pinned ask must resolve");
    assert!(
        !org_none_text.contains(BRIEF_HEADER),
        "no board ⇒ no board header, for the org-pinned caller too: {org_none_text}"
    );
}

/// THE SCOPE-BREAK REGRESSION.
///
/// A board whose tiles are ALL sealed or hidden renders an EMPTY brief and produces
/// no `SourceRef` either. The first fix inferred board scope from `!brief.is_empty()`,
/// so that board fell straight through to the vault-wide search — and answered a
/// board-scoped question from unrelated readable vault content. Review caught it.
///
/// Scope is carried by the PRESENCE of the board (`Option`), never by the presence of
/// text, which is why `Some("")` and `None` must behave differently here.
#[test]
fn an_all_withheld_board_never_falls_back_to_the_whole_vault() {
    let db = file_db("scope-break");
    // Something readable and completely unrelated to the board.
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m-unrelated", "f-open", "UNRELATED VAULT CONTENT");

    // The board itself: one promises tile whose only rows live in a SEALED folder.
    seed_folder(&db, "f-sealed", true);
    let brief = dashboard_brief_inner(
        &db,
        vec![tile("t1", "pulse", Some("e-invisible"), 0)],
        &HashSet::new(),
        MAX_BRIEF_CHARS,
    )
    .unwrap();
    assert!(brief.is_empty(), "an all-hidden board has nothing to say");

    let packed = packed_scoped(&db, &HashSet::new(), Some(&brief), None);
    let text = packed.unwrap_or_default();
    assert!(
        !text.contains("UNRELATED VAULT CONTENT"),
        "a board-scoped ask must never answer from content outside the board: {text}"
    );

    // CONTROL — with NO board (`None`), the SAME fixture DOES reach the vault-wide
    // corpus. That is what proves the assertion above is the scope guard and not an
    // empty vault.
    let vault_wide = packed_scoped(&db, &HashSet::new(), None, None).unwrap_or_default();
    assert!(
        vault_wide.contains("UNRELATED VAULT CONTENT"),
        "the vault-wide path must still see the vault, or the guard test is vacuous"
    );
}

/// The remaining derived kinds at the SAME sink, each negative WITH its control.
///
/// Review's point was exact: `tile_is_withheld` applies the `ENTITY_HIDDEN` arm to
/// `Drift`/`Numbers`/`Pulse` only, and everything else falls to `_ => false` — so
/// `person` and `reminders` reached the prompt through a catch-all with nothing
/// proving what they contribute when their source is gone. The code turns out to be
/// right (`resolve_tile`'s `person` arm returns `TileData::Missing`, not an empty
/// view carrying residual counts, precisely because an entity visible only through
/// sealed meetings must be indistinguishable from an unknown one) — but "right and
/// unproven at the sink" is the state that ships leaks.
#[test]
fn a_person_tile_whose_entity_went_invisible_contributes_nothing() {
    let db = file_db("person-hidden");
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m1", "f-open", "ship it");

    let hidden = vec![tile("t1", "person", Some("e-not-visible"), 0)];
    let brief = dashboard_brief_inner(&db, hidden, &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    assert!(
        brief.is_empty(),
        "a person tile with no visible entity must contribute nothing: {brief}"
    );
    let packed = packed_scoped(&db, &HashSet::new(), Some(&brief), None).unwrap_or_default();
    assert!(
        packed.is_empty(),
        "an invisible person must contribute nothing at the provider sink: {packed}"
    );

    // CONTROL — a VISIBLE person does reach the brief, so the emptiness above is the
    // withheld predicate and not a brief that simply never emits person tiles.
    let marcus = db.upsert_entity("Marcus", EntityKind::Person).unwrap();
    db.add_mention(&marcus, "m1").unwrap();
    let brief = dashboard_brief_inner(
        &db,
        vec![tile("t1", "person", Some(&marcus), 0)],
        &HashSet::new(),
        MAX_BRIEF_CHARS,
    )
    .unwrap();
    assert!(
        brief.contains("Marcus"),
        "a visible person's tile does reach the prompt: {brief}"
    );
    let packed = packed_scoped(&db, &HashSet::new(), Some(&brief), None)
        .expect("a visible person must reach the provider sink");
    assert!(
        packed.contains("Marcus"),
        "the control must reach the provider sink, or the packed negative is vacuous: {packed}"
    );
}

/// The brief's allowance is a documented fraction of ONE budget, not a second cap.
///
/// It regressed once already: an inline `/ 8` at the packing site quietly reduced a
/// contracted ~4000-char brief to ~500 on a small-budget provider.
#[test]
fn the_brief_allowance_is_a_documented_fraction_of_the_corpus_budget() {
    // A large budget gives the brief its full contracted cap…
    assert_eq!(brief_allowance(200_000), MAX_BRIEF_CHARS);
    // …and a small one keeps room for the sources the answer must stand on.
    assert_eq!(brief_allowance(4_000), 1_000);
    // Never zero: a degenerate budget still admits a bounded brief rather than
    // silently dropping the board's own views.
    assert!(brief_allowance(1) >= 1);
}

/// Truncation happens between WHOLE rendered tiles, and a rendered tile is
/// MULTILINE — that is exactly what a `.lines()`-based cut got wrong, keeping a
/// heading while dropping the ledger rows beneath it.
#[test]
fn truncation_never_keeps_a_tile_heading_without_its_rows() {
    let db = file_db("multiline-cap");
    seed_folder(&db, "f-open", false);
    for i in 0..6 {
        seed_meeting_with_commitment(&db, &format!("m{i}"), "f-open", &format!("obligation {i}"));
    }
    let tiles: Vec<_> = (0..6)
        .map(|i| tile(&format!("t{i}"), "promises", None, i))
        .collect();

    // An allowance that admits the header and roughly one tile.
    let full = dashboard_brief_inner(&db, tiles.clone(), &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    let one_tile_len = full.lines().count();
    let tight = dashboard_brief_inner(&db, tiles, &HashSet::new(), 260).unwrap();

    assert!(tight.chars().count() <= 260);
    if !tight.is_empty() {
        assert!(
            tight.lines().count() > 1,
            "a bare header is the lie an empty board is careful not to tell: {tight:?}"
        );
        // Every kept line is a WHOLE line from the full render — no row cut in half and
        // no heading orphaned from its rows. (Not a literal prefix: an oversized tile is
        // now kept as a counted prefix of its own rows, which appends a "… N more"
        // marker the full render does not have.)
        for line in tight.lines().skip(1) {
            if line.trim_start().starts_with("· …") {
                continue;
            }
            assert!(
                full.contains(line),
                "kept line must be a whole line from the full render: {line:?}"
            );
        }
    }
    let _ = one_tile_len;
}

/// The last derived kind at this sink, negative WITH control.
///
/// A "smart" reminder's TITLE is authored from meeting content, so a reminder whose
/// every anchor points at a source the session cannot read must contribute nothing —
/// `resolve_tile` enforces that with `reminder_provenance_is_readable`, because
/// `list_stored_reminders` is itself UNGATED. That mattered little while the tile
/// only rendered on screen; the brief makes it an Ask-provider egress, so the
/// negative belongs here too and not only in the on-screen tests.
#[test]
fn a_reminder_anchored_only_to_a_sealed_meeting_contributes_nothing() {
    let db = file_db("reminders-sealed");
    seed_folder(&db, "f-sealed", true);
    seed_meeting_with_commitment(&db, "m-sealed", "f-sealed", "irrelevant");

    db.create_reminder(
        "r1",
        &ReminderDraft {
            title: "CHASE THE SEALED THING".to_string(),
            details: None,
            due_at: 1_780_000_000_000,
            repeat_every: None,
            repeat_unit: None,
            sources: vec![ReminderSourceAnchor {
                kind: "meeting".to_string(),
                id: "m-sealed".to_string(),
            }],
        },
        ReminderOrigin::Manual,
        1_780_000_000_000,
    )
    .unwrap();

    let tiles = vec![tile("t1", "reminders", None, 0)];
    let brief =
        dashboard_brief_inner(&db, tiles.clone(), &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    assert!(
        !brief.contains("CHASE THE SEALED THING"),
        "a reminder whose only anchor is sealed must not reach a prompt: {brief}"
    );
    let packed = packed_scoped(&db, &HashSet::new(), Some(&brief), None).unwrap_or_default();
    assert!(
        !packed.contains("CHASE THE SEALED THING"),
        "a sealed reminder must stay absent at the provider sink: {packed}"
    );

    // CONTROL — unlock the folder and the SAME assertion finds it, so the negative is
    // the provenance gate rather than a reminder that was never stored.
    let unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
    let brief = dashboard_brief_inner(&db, tiles, &unlocked, MAX_BRIEF_CHARS).unwrap();
    assert!(
        brief.contains("CHASE THE SEALED THING"),
        "unlocking must restore it, or the leak test is vacuous: {brief}"
    );
    let packed = packed_scoped(&db, &unlocked, Some(&brief), None)
        .expect("an unlocked reminder must reach the provider sink");
    assert!(
        packed.contains("CHASE THE SEALED THING"),
        "the control must reach the provider sink, or the packed negative is vacuous: {packed}"
    );
}

/// GAP 1 — the SEVENTH derived kind. A `livingAnswer` whose cached answer was
/// stamped against a folder that is no longer fully readable resolves with
/// `withheld: true`, and a withheld cached answer is a PARAPHRASE of content the
/// session cannot read, so it must not reach the prompt either.
#[test]
fn a_withheld_living_answer_contributes_nothing() {
    let db = file_db("living-answer-withheld");
    seed_folder(&db, "f-sealed", true);
    seed_meeting_with_commitment(&db, "m-sealed", "f-sealed", "irrelevant");

    // A cached answer stamped against a folder that is sealed and NOT session-unlocked.
    let cfg = serde_json::json!({
        "question": "will it land?",
        "answer": "CACHED PARAPHRASE OF SEALED CONTENT",
        "answeredAt": "2026-08-01T09:00:00Z",
        "answerReadableFolders": ["f-sealed"],
    })
    .to_string();
    let mut t = tile("t1", "living_answer", None, 0);
    t.config = Some(cfg);

    let brief =
        dashboard_brief_inner(&db, vec![t.clone()], &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    assert!(
        !brief.contains("CACHED PARAPHRASE OF SEALED CONTENT"),
        "a withheld cached answer must not reach a prompt: {brief}"
    );
    let packed = packed_user(&db, &HashSet::new(), &brief, None).unwrap_or_default();
    assert!(!packed.contains("CACHED PARAPHRASE OF SEALED CONTENT"));

    // CONTROL — session-unlock the stamped folder and the SAME string reappears, so the
    // negative above is the withheld predicate rather than an unparsed config.
    let unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
    let brief = dashboard_brief_inner(&db, vec![t], &unlocked, MAX_BRIEF_CHARS).unwrap();
    assert!(
        brief.contains("CACHED PARAPHRASE OF SEALED CONTENT"),
        "unlocking must restore it, or the leak test is vacuous: {brief}"
    );
}

/// GAP 2 — the PRODUCTION wiring, not one layer in.
///
/// Every other test here enters at `dashboard_brief_inner` with a hand-built
/// `HashSet`. That proves the rule but not the plumbing: `dashboard_brief_for_ask`
/// reads the board's tiles from the DB and applies `brief_allowance`, and neither
/// was executed. A gate established by READING a diff is not a gate that ran.
#[test]
fn the_production_brief_entry_point_gates_on_the_session_snapshot() {
    let db = file_db("production-wiring");
    seed_folder(&db, "f-sealed", true);
    seed_meeting_with_commitment(&db, "m-sealed", "f-sealed", "the sealed obligation");
    db.insert_dashboard("b-live", "Live", None, None, "2026-08-01T09:00:00Z")
        .unwrap();
    db.insert_dashboard_tile(
        "t1",
        "b-live",
        "promises",
        None,
        None,
        4,
        None,
        "2026-08-01T09:00:00Z",
    )
    .unwrap();

    let brief = dashboard_brief_for_ask(&db, "b-live", &HashSet::new(), 200_000).unwrap();
    assert!(
        !brief.contains("the sealed obligation"),
        "the production entry point must gate on the session snapshot: {brief}"
    );

    // CONTROL — the same call with the folder session-unlocked finds it.
    let unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
    let brief = dashboard_brief_for_ask(&db, "b-live", &unlocked, 200_000).unwrap();
    assert!(
        brief.contains("the sealed obligation"),
        "unlocking must restore it, or the wiring test is vacuous: {brief}"
    );

    // An unknown board id is empty, never an error — a stale id must not fail an Ask.
    assert!(
        dashboard_brief_for_ask(&db, "no-such-board", &unlocked, 200_000)
            .unwrap()
            .is_empty()
    );
}

/// The brief takes its slice OUT OF the corpus budget rather than on top of it.
///
/// The budget must BIND for this to be observable, which is the whole point. With a
/// 200k budget and a small vault there is slack: the corpus packs whole either way and
/// the brief genuinely adds — correctly, since the prompt is still inside budget. An
/// earlier version of this test ran exactly there, and therefore passed with or without
/// `saturating_sub(board_brief.chars().count())` in `ask.rs`. A test that cannot fail is
/// not evidence — the same vacuity the adversarial review of PR #562 caught in this
/// feature's answer-cache gate.
///
/// This one runs against `ollama`'s 4000-char budget with a fixture large enough to
/// saturate it, so the assertion exercises the shared budget rather than slack.
#[test]
fn the_brief_comes_out_of_the_corpus_budget_when_the_budget_binds() {
    let db = file_db("budget-binds");
    seed_folder(&db, "f-open", false);
    for i in 0..40 {
        seed_meeting_with_commitment(
            &db,
            &format!("m{i}"),
            "f-open",
            &format!("obligation {i} with a deliberately long tail of words to spend the budget"),
        );
    }
    let sources: Vec<SourceRef> = (0..40)
        .map(|i| SourceRef {
            kind: crate::links::LinkKind::Meeting,
            id: format!("m{i}"),
        })
        .collect();
    let brief = dashboard_brief_inner(
        &db,
        vec![tile("t1", "promises", None, 0)],
        &HashSet::new(),
        brief_allowance(4_000),
    )
    .unwrap();
    assert!(!brief.is_empty(), "fixture must produce a brief");

    let with_board =
        packed_with_provider(&db, &HashSet::new(), Some(&brief), Some(&sources), "ollama")
            .unwrap_or_default();
    let without = packed_with_provider(&db, &HashSet::new(), None, Some(&sources), "ollama")
        .unwrap_or_default();

    assert!(
        without.chars().count() > 3_000,
        "the no-board corpus must actually saturate the 4000-char budget, or this test proves nothing: {}",
        without.chars().count()
    );
    // Equal to within the two-character `\n\n` that joins brief to corpus. Without
    // the subtraction the delta would be the brief's own length — hundreds of chars at
    // this allowance — so a 4-char tolerance still falsifies decisively.
    let delta = with_board
        .chars()
        .count()
        .saturating_sub(without.chars().count());
    assert!(
        delta <= 4,
        "the brief must displace corpus, not stack on it: board {} vs no-board {} (delta {delta}, brief {})",
        with_board.chars().count(),
        without.chars().count(),
        brief.chars().count()
    );
}

/// THE LOAD-BEARING TRUNCATION CASE.
///
/// One Promise ledger, alone on the board, rendering LONGER than the allowance. The
/// first whole-tile rule dropped it entirely — so on a small provider budget the
/// single case this feature exists for ("who owes me something on this board?")
/// produced an empty brief. Review caught it.
///
/// A prefix of complete rows plus a COUNTED remainder is honest; a partial row is
/// not, and neither is silence.
#[test]
fn one_oversized_ledger_still_reaches_the_prompt_as_a_counted_prefix() {
    let db = file_db("oversized-ledger");
    seed_folder(&db, "f-open", false);
    for i in 0..6 {
        seed_meeting_with_commitment(
            &db,
            &format!("m{i}"),
            "f-open",
            &format!("obligation {i} with a deliberately long tail of words to spend the budget"),
        );
    }

    let tiles = vec![tile("t1", "promises", None, 0)];
    let full = dashboard_brief_inner(&db, tiles.clone(), &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    assert!(
        full.chars().count() > 320,
        "fixture must exceed the tight cap"
    );

    // An allowance far below the whole tile — ollama's real `brief_allowance` shape.
    let tight = dashboard_brief_inner(&db, tiles, &HashSet::new(), 320).unwrap();

    assert!(!tight.is_empty(), "an oversized ledger must not vanish");
    assert!(
        tight.chars().count() <= 320,
        "…and must stay inside the cap"
    );
    assert!(tight.starts_with(BRIEF_HEADER), "the label survives");
    // Which obligation lands first is `list_open_commitments`' ordering, not this
    // test's business — assert that a COMPLETE row survived, not which one.
    assert!(
        tight.contains("obligation "),
        "at least one complete row must reach the prompt: {tight}"
    );
    assert!(
        tight.contains("more"),
        "the remainder must be COUNTED, not silently dropped: {tight}"
    );
    // No row is cut in half: every ledger line kept is one the full render also has.
    for line in tight.lines().skip(1) {
        if line.trim_start().starts_with("· …") {
            continue;
        }
        assert!(
            full.contains(line),
            "kept line must be a whole line from the full render: {line:?}"
        );
    }
}

// ── THE PRODUCTION COMPOSITION ────────────────────────────────────────────────
//
// Everything above tests a PIECE: `dashboard_brief_inner` (the rule),
// `dashboard_brief_for_ask` (the DB read), `build_ask_vault_floor_prompt` (the pack).
// The glue in `ask_vault` — normalize the id, resolve the brief, PRESERVE `Some("")`,
// forward it — was verified by reading, and that is where the routing defect actually
// lived: a derived-only board skipping the floor entirely. `board_scoped_brief` is
// that glue, extracted so these tests drive the shipped code rather than a copy of it.

/// End to end at the production seam: a board id in, a promises row out of the packed
/// provider prompt — with no hand-assembled intermediate.
#[test]
fn the_production_composition_puts_a_ledger_row_in_the_packed_prompt() {
    let db = file_db("composition");
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m1", "f-open", "send the Acme paperwork");
    db.insert_dashboard("b-1", "Board", None, None, "2026-08-01T09:00:00Z")
        .unwrap();
    db.insert_dashboard_tile(
        "t1",
        "b-1",
        "promises",
        None,
        None,
        4,
        None,
        "2026-08-01T09:00:00Z",
    )
    .unwrap();

    let brief = board_scoped_brief(&db, Some("b-1"), &HashSet::new(), 200_000).unwrap();
    let brief = brief.expect("a board id must yield board scope");
    let packed = packed_scoped(&db, &HashSet::new(), Some(&brief), None)
        .expect("a board-scoped ask must produce a prompt");
    assert!(
        packed.contains("send the Acme paperwork"),
        "the production path must carry the ledger row to the provider prompt: {packed}"
    );
}

/// The `Some("")` case at the same seam — the one the first cut collapsed to `None`.
#[test]
fn the_production_composition_keeps_scope_for_an_all_withheld_board() {
    let db = file_db("composition-withheld");
    // Readable and entirely unrelated to the board.
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m-unrelated", "f-open", "UNRELATED VAULT CONTENT");
    // The board: one tile anchored to an entity nobody can see.
    db.insert_dashboard("b-2", "Board", None, None, "2026-08-01T09:00:00Z")
        .unwrap();
    db.insert_dashboard_tile(
        "t1",
        "b-2",
        "pulse",
        Some("e-invisible"),
        None,
        4,
        None,
        "2026-08-01T09:00:00Z",
    )
    .unwrap();

    let brief = board_scoped_brief(&db, Some("b-2"), &HashSet::new(), 200_000).unwrap();
    assert_eq!(
        brief.as_deref(),
        Some(""),
        "a board with nothing readable must still be Some — scope is presence, not text"
    );

    let packed = packed_scoped(&db, &HashSet::new(), brief.as_deref(), None).unwrap_or_default();
    assert!(
        !packed.contains("UNRELATED VAULT CONTENT"),
        "an all-withheld board must not answer from the whole vault: {packed}"
    );

    // CONTROL — no board id at all, and the SAME fixture reaches the vault-wide corpus.
    assert_eq!(
        board_scoped_brief(&db, None, &HashSet::new(), 200_000).unwrap(),
        None,
        "no board ⇒ no scope"
    );
    let vault_wide = packed_scoped(&db, &HashSet::new(), None, None).unwrap_or_default();
    assert!(
        vault_wide.contains("UNRELATED VAULT CONTENT"),
        "the vault-wide path must still see the vault, or the guard test is vacuous"
    );
}

/// A blank or whitespace-only id is NOT a board — the normalization `ask_vault`
/// applies before anything else, and the one that keeps every non-board caller on the
/// byte-identical path.
#[test]
fn a_blank_dashboard_id_is_not_a_board() {
    let db = file_db("composition-blank");
    for id in ["", "   ", "\t\n"] {
        assert_eq!(
            board_scoped_brief(&db, Some(id), &HashSet::new(), 200_000).unwrap(),
            None,
            "a blank id must not scope the ask: {id:?}"
        );
    }
}

/// The LEGACY living-answer config — no `answerReadableFolders` stamp at all.
///
/// A row written before the stamp existed cannot be checked against anything, so
/// `resolve_tile` fails CLOSED (`living_answer_withheld` treats an empty recorded set
/// as unverifiable). That is the correct direction, and it is the one that had no test
/// at this sink: the existing case only covers a config where the stamp IS present.
/// An unstamped cached answer is still a paraphrase of whatever the vault held when it
/// was written, so trusting it would quote possibly-sealed content back into a prompt
/// forever.
#[test]
fn an_unstamped_legacy_living_answer_fails_closed() {
    let db = file_db("living-answer-legacy");
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m1", "f-open", "irrelevant");

    // question/answer/answeredAt, and NO `answerReadableFolders` key.
    let legacy = serde_json::json!({
        "question": "will it land?",
        "answer": "LEGACY CACHED PARAPHRASE",
        "answeredAt": "2026-08-01T09:00:00Z",
    })
    .to_string();
    let mut t = tile("t1", "living_answer", None, 0);
    t.config = Some(legacy);

    let brief =
        dashboard_brief_inner(&db, vec![t.clone()], &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    assert!(
        !brief.contains("LEGACY CACHED PARAPHRASE"),
        "an unverifiable cached answer must be withheld, not trusted: {brief}"
    );
    let packed = packed_scoped(&db, &HashSet::new(), Some(&brief), None).unwrap_or_default();
    assert!(!packed.contains("LEGACY CACHED PARAPHRASE"));

    // CONTROL — the SAME answer with a stamp naming a readable folder DOES render, so
    // the withholding above is the fail-closed rule and not a config that failed to
    // parse (which would make this test pass for the wrong reason).
    let stamped = serde_json::json!({
        "question": "will it land?",
        "answer": "LEGACY CACHED PARAPHRASE",
        "answeredAt": "2026-08-01T09:00:00Z",
        "answerReadableFolders": ["f-open"],
    })
    .to_string();
    let mut t2 = tile("t1", "living_answer", None, 0);
    t2.config = Some(stamped);
    let brief = dashboard_brief_inner(&db, vec![t2], &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    assert!(
        brief.contains("LEGACY CACHED PARAPHRASE"),
        "a stamped, readable answer does render — the control that keeps the negative honest: {brief}"
    );
}

/// A single row LONGER than the whole allowance.
///
/// The oversized-ledger case above uses several moderate rows, so it always had one
/// that fit. When not even the FIRST row fits, `truncate_tile` returns "" — and
/// counting that as a written tile emitted a BARE HEADER, which is precisely the
/// "views that do not exist" lie the empty-board path is careful to avoid. A
/// heading with nothing under it is worse than silence, because the model reasons
/// from it.
#[test]
fn a_single_row_longer_than_the_allowance_yields_nothing_not_a_bare_header() {
    let db = file_db("row-longer-than-cap");
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m1", "f-open", &"x".repeat(600));

    let tiles = vec![tile("t1", "promises", None, 0)];
    // Room for the header, but nowhere near the single 600-char row.
    let cap = BRIEF_HEADER.chars().count() + 40;
    let brief = dashboard_brief_inner(&db, tiles.clone(), &HashSet::new(), cap).unwrap();

    assert!(
        brief.is_empty(),
        "no complete row fits ⇒ contribute nothing, never a bare header: {brief:?}"
    );

    // CONTROL — the SAME tile with room to breathe does render, so the emptiness above
    // is the truncation rule and not a fixture that produced no rows.
    let roomy = dashboard_brief_inner(&db, tiles, &HashSet::new(), MAX_BRIEF_CHARS).unwrap();
    assert!(
        roomy.contains("xxxx"),
        "the row renders when it fits: {roomy}"
    );
}

/// The cap bounds the OUTPUT, including the label itself.
#[test]
fn a_cap_smaller_than_the_label_yields_nothing() {
    let db = file_db("cap-under-label");
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m1", "f-open", "ship it");
    let tiles = vec![tile("t1", "promises", None, 0)];

    for cap in [0usize, 10, BRIEF_HEADER.chars().count() - 1] {
        let brief = dashboard_brief_inner(&db, tiles.clone(), &HashSet::new(), cap).unwrap();
        assert!(
            brief.chars().count() <= cap,
            "a cap of {cap} must bound the output, got {}",
            brief.chars().count()
        );
    }
}

// ── THE GUARDED BLOCK, EXECUTED ───────────────────────────────────────────────
//
// Everything above enters at `board_scoped_brief` or below with a caller-supplied
// `HashSet`, so the ordering `ask_vault` actually performs — acquire the lifecycle
// guard, THEN take the session snapshot, THEN resolve every tile under it — was
// established by reading the diff. `board_scoped_floor_inputs` is that block; these
// drive it with a real `AppState`, which also exercises the fail-closed default (a
// fresh state unlocks nothing) and the Role::Ask budget derivation inside the guard.

/// An `AppState` over a real temp SQLCipher DB — no Keychain, no Tauri. Same shape
/// `commands::lifecycle_tests::build_state` uses; duplicated rather than shared
/// because that helper is private to its own test module.
fn brief_state(tag: &str) -> crate::state::AppState {
    use std::sync::{Arc, Mutex};
    let path = crate::storage::db::unique_temp_path(&format!("murmur-brief-state-{tag}"), "sqlite");
    let _ = std::fs::remove_file(&path);
    let db = Arc::new(crate::storage::Db::open_with_key(&path, TEST_DEK).unwrap());
    crate::state::AppState {
        recorder: Mutex::new(None),
        recording_stop: Mutex::new(None),
        voice_listener: Mutex::new(None),
        voice_listener_lifecycle: Mutex::new(()),
        recording_starting: std::sync::atomic::AtomicBool::new(false),
        voice_command_capture: Mutex::new(None),
        pending_manual_command: Mutex::new(None),
        live_running: std::sync::atomic::AtomicBool::new(false),
        db,
        config: Arc::new(Mutex::new(AppConfig::default())),
        reasoner: crate::reason::ReasonerCell::fixed(Arc::new(crate::reason::StubReasoner)),
        current_meeting: Mutex::new(None),
        focus_meeting: Mutex::new(None),
        live_transcript: Mutex::new(String::new()),
        live_bullets: Mutex::new(String::new()),
        live_bullets_tracker: Mutex::new(crate::transcribe::bullets::BulletsTracker::default()),
        capped_notified: std::sync::atomic::AtomicBool::new(false),
        capture_fault_notified: std::sync::atomic::AtomicBool::new(false),
        reactions_shadow_count: std::sync::atomic::AtomicU64::new(0),
        reactions_emitted: Mutex::new(HashSet::new()),
        in_flight_turns: Mutex::new(std::collections::HashMap::new()),
        user_turn_in_progress: std::sync::atomic::AtomicBool::new(false),
        verify_cache: Mutex::new(std::collections::HashMap::new()),
        unlocked_folders: Arc::new(Mutex::new(HashSet::new())),
        master_kek: Mutex::new(None),
        org_ock_cache: Mutex::new(std::collections::HashMap::new()),
        account_session: Mutex::new(None),
        lifecycle: Mutex::new(()),
        active_salvages: Mutex::new(HashSet::new()),
        share_refresh_lock: tokio::sync::Mutex::new(()),
        seal_epoch: std::sync::atomic::AtomicU64::new(0),
        heavy_inference: Arc::new(tokio::sync::Semaphore::new(1)),
    }
}

/// The block `ask_vault` runs, driven end to end: a sealed row contributes nothing,
/// and the control proves the same call finds it once the session unlocks the folder.
#[test]
fn the_guarded_floor_inputs_gate_on_the_session_unlock_set() {
    let state = brief_state("guard");
    let db = &state.db;
    seed_folder(db, "f-sealed", true);
    seed_meeting_with_commitment(db, "m-sealed", "f-sealed", "the sealed obligation");
    db.insert_dashboard("b-1", "Board", None, None, "2026-08-01T09:00:00Z")
        .unwrap();
    db.insert_dashboard_tile(
        "t1",
        "b-1",
        "promises",
        None,
        None,
        4,
        None,
        "2026-08-01T09:00:00Z",
    )
    .unwrap();
    let cfg = AppConfig::default();

    let (unlocked, brief) =
        crate::commands::board_scoped_floor_inputs(&state, &cfg, Some("b-1")).unwrap();
    assert!(
        unlocked.is_empty(),
        "a fresh session unlocks nothing — fail closed"
    );
    // The board still SCOPES the ask, and its ledger still renders as a view — but
    // with NO ROWS, because every one of them failed the gate. That distinction is the
    // whole point: the model learns a ledger exists and is empty from here, never what
    // is in it.
    let text = brief.clone().expect("a board id must yield board scope");
    assert!(
        !text.contains("the sealed obligation"),
        "a sealed row must not reach the prompt: {text}"
    );
    assert!(
        !text.contains("    · "),
        "…and no ROW entry survives, so even the COUNT of sealed rows is not disclosed \
         by shape: {text}"
    );

    // CONTROL — unlock through the SAME state the helper reads, and the same call now
    // carries the row. Without this the assertion above would pass on any empty board.
    state
        .unlocked_folders
        .lock()
        .unwrap()
        .insert("f-sealed".to_string());
    let (unlocked, brief) =
        crate::commands::board_scoped_floor_inputs(&state, &cfg, Some("b-1")).unwrap();
    assert_eq!(unlocked.len(), 1, "the returned snapshot is the session's");
    assert!(
        brief.unwrap_or_default().contains("the sealed obligation"),
        "unlocking must restore the row, or the negative above is vacuous"
    );

    // No board id ⇒ no scope, from the same production entry point.
    let (_, brief) = crate::commands::board_scoped_floor_inputs(&state, &cfg, None).unwrap();
    assert_eq!(brief, None);
}

/// The production helper must keep the lifecycle mutex for the entire interval from
/// snapshot through the final derived-tile read. This drives that ordering with two
/// real threads: a relock contender cannot mutate the session set while the resolver
/// closure is paused inside the critical section, then proceeds immediately after it
/// returns. The returned snapshot is therefore wholly pre-relock, never a mixture.
#[test]
fn the_guarded_floor_inputs_serialize_resolution_against_relock() {
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    let state = Arc::new(brief_state("guard-race"));
    state
        .unlocked_folders
        .lock()
        .unwrap()
        .insert("f-sealed".to_string());

    let (inside_tx, inside_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let resolver_state = Arc::clone(&state);
    let resolver = std::thread::spawn(move || {
        crate::commands::with_board_scoped_floor_inputs(&resolver_state, |unlocked| {
            let saw_unlocked = unlocked.contains("f-sealed");
            inside_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(saw_unlocked)
        })
        .unwrap()
    });

    inside_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("resolver must enter the lifecycle critical section");

    let (started_tx, started_rx) = mpsc::channel();
    let (relocked_tx, relocked_rx) = mpsc::channel();
    let relock_state = Arc::clone(&state);
    let relocker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let _lifecycle = crate::commands::lifecycle_guard(&relock_state);
        relock_state
            .unlocked_folders
            .lock()
            .unwrap()
            .remove("f-sealed");
        relocked_tx.send(()).unwrap();
    });
    started_rx.recv().unwrap();

    assert!(
        relocked_rx.recv_timeout(Duration::from_millis(75)).is_err(),
        "relock must block while board resolution owns the lifecycle guard"
    );
    release_tx.send(()).unwrap();

    let (snapshot, saw_unlocked) = resolver.join().unwrap();
    assert!(snapshot.contains("f-sealed"));
    assert!(
        saw_unlocked,
        "the resolver sees one coherent pre-relock snapshot"
    );
    relocked_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("relock must proceed after board resolution releases the guard");
    relocker.join().unwrap();
    assert!(state.unlocked_folders.lock().unwrap().is_empty());
}

/// The row-only suppression, pinned per kind — and the aggregate kind that must NOT
/// be caught by it.
///
/// A review read `weekly`/`total` onto `Drift` and `Numbers` and asked for them to
/// survive an empty row list. They carry neither: `Drift { entity, predicate, rows }`
/// and `Numbers { entity, rows }` are rows and a label, so with no rows there is
/// nothing to say and a heading alone is the lie the empty-board path avoids. The
/// kind that DOES carry aggregates is `Pulse { entity, weekly, total, quiet_days }`,
/// and "quiet for 30 days" is real signal — this pins that distinction so the next
/// person to touch the list sees which side each kind falls on.
#[test]
fn a_pulse_survives_an_empty_row_list_because_its_content_is_aggregates() {
    let db = file_db("aggregate-vs-rows");
    seed_folder(&db, "f-open", false);
    seed_meeting_with_commitment(&db, "m1", "f-open", "ship it");
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m1").unwrap();

    // Pulse has no `rows` field at all — it must reach the prompt on its aggregates.
    let pulse = dashboard_brief_inner(
        &db,
        vec![tile("t1", "pulse", Some(&atlas), 0)],
        &HashSet::new(),
        MAX_BRIEF_CHARS,
    )
    .unwrap();
    assert!(
        pulse.contains("Atlas"),
        "an aggregate-bearing tile must not be suppressed for lacking rows: {pulse}"
    );

    // Drift and Numbers over the SAME visible entity have no facts. They still REACH
    // the prompt — an empty view is a statement about the board, and suppressing it
    // here would make this sink show less than the MCP one, which is the asymmetry
    // this whole change exists to close. What they must not do is invent values.
    for kind in ["drift", "numbers"] {
        let brief = dashboard_brief_inner(
            &db,
            vec![tile("t1", kind, Some(&atlas), 0)],
            &HashSet::new(),
            MAX_BRIEF_CHARS,
        )
        .unwrap();
        assert!(
            brief.contains(kind),
            "{kind} renders its view even with no rows: {brief}"
        );
        assert!(
            !brief.contains("    · "),
            "…but an empty view carries no ROW entries — the indented bullet, not the \
             heading's field separator — so nothing is invented: {brief}"
        );
    }
}

/// THE MIXED CASE — a VISIBLE entity carrying rows from a SEALED meeting.
///
/// Every other negative here seals the whole tile: the entity goes invisible, or the
/// only source folder is locked. This is the sharper shape — the tile is legitimately
/// readable, its heading is legitimately shown, and the question is whether a single
/// ROW derived from a meeting the session cannot read rides along inside it.
///
/// `resolve_tile` gates rows through `list_facts_visible`, which JOINs `meetings` and
/// applies `visibility_clause`, so it should not. That is inherited behaviour rather
/// than new code — but the brief is a NEW SINK for it, and inherited-and-unproven at a
/// new sink is the state that ships leaks.
#[test]
fn a_visible_entity_carries_no_row_from_a_sealed_meeting() {
    use crate::facts::{FactOp, NewFact};

    let db = file_db("mixed-visibility");
    seed_folder(&db, "f-open", false);
    seed_folder(&db, "f-sealed", true);
    seed_meeting_with_commitment(&db, "m-open", "f-open", "ship it");
    seed_meeting_with_commitment(&db, "m-sealed", "f-sealed", "irrelevant");

    // ONE entity, mentioned in BOTH — so it stays visible through the open meeting.
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m-open").unwrap();
    db.add_mention(&atlas, "m-sealed").unwrap();

    let fact = |predicate: &str, object: &str, meeting: &str, at: &str| {
        FactOp::Add(NewFact {
            entity_id: atlas.clone(),
            subject: "Atlas".to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: at.to_string(),
            recorded_at: at.to_string(),
            confidence: 1.0,
            meeting_id: Some(meeting.to_string()),
        })
    };
    db.apply_fact_ops(&[
        fact("arr", "OPEN-FIGURE-180K", "m-open", "2026-07-01T09:00:00Z"),
        fact(
            "headcount",
            "SEALED-FIGURE-42",
            "m-sealed",
            "2026-07-02T09:00:00Z",
        ),
    ])
    .unwrap();

    let brief = dashboard_brief_inner(
        &db,
        vec![tile("t1", "numbers", Some(&atlas), 0)],
        &HashSet::new(),
        MAX_BRIEF_CHARS,
    )
    .unwrap();

    assert!(
        !brief.contains("SEALED-FIGURE-42"),
        "a fact from a sealed meeting must not ride a visible entity's tile: {brief}"
    );
    let packed_hidden = packed_scoped(&db, &HashSet::new(), Some(&brief), None)
        .expect("the readable row keeps the numbers tile non-empty");
    assert!(
        !packed_hidden.contains("SEALED-FIGURE-42"),
        "the sealed numbers row must stay absent at the provider sink: {packed_hidden}"
    );
    assert!(
        brief.contains("OPEN-FIGURE-180K"),
        "…while the readable fact on the SAME tile does appear — which is what makes \
         the negative above a gate and not an empty tile: {brief}"
    );

    // CONTROL — session-unlock the sealed folder and the withheld row joins the same
    // tile, so the absence above is the row-level gate rather than a lost fixture.
    let unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
    let brief = dashboard_brief_inner(
        &db,
        vec![tile("t1", "numbers", Some(&atlas), 0)],
        &unlocked,
        MAX_BRIEF_CHARS,
    )
    .unwrap();
    assert!(
        brief.contains("SEALED-FIGURE-42"),
        "unlocking must restore the row, or the leak test is vacuous: {brief}"
    );
    let packed_visible = packed_scoped(&db, &unlocked, Some(&brief), None)
        .expect("the unlocked numbers tile must reach the provider sink");
    assert!(
        packed_visible.contains("SEALED-FIGURE-42"),
        "the control must reach the provider sink, or the packed negative is vacuous: {packed_visible}"
    );
}

/// Helpers for the org-equivalence assertion above.
fn db_ref(db: &crate::storage::Db) -> &crate::storage::Db {
    db
}

fn cfg_for_test() -> AppConfig {
    AppConfig {
        semantic_search_enabled: false,
        provider_id: "claude_code".to_string(),
        cloud_egress_consented: true,
        ..AppConfig::default()
    }
}

fn prompt_text(p: AskFloorPrompt) -> String {
    match p {
        AskFloorPrompt::Ready { system, user, .. } => format!("{system}\n{user}"),
        AskFloorPrompt::Empty(r) => r.answer,
    }
}

/// PULSE AT THE MIXED CASE — the aggregate twin of
/// `a_visible_entity_carries_no_row_from_a_sealed_meeting`.
///
/// That test proves `list_facts_visible` gates the ROWS a visible entity's Numbers
/// tile carries. It does not transfer to Pulse: Pulse has no rows, and its
/// `weekly`/`total`/`quiet_days` come from `entity_mention_pulse_visible`, a
/// different query. Pulse is also the one derived kind deliberately EXEMPT from the
/// rows-only suppression, so it is the one that reaches the prompt on aggregates
/// alone — and volume-and-recency of activity is itself disclosure: "quiet for 30
/// days" versus "12 mentions" says whether a sealed folder is busy.
///
/// The store query does apply `visibility_clause`, and a store-layer test
/// (`dashboard_tests::pulse_excludes_sealed_meetings_until_unlocked`) covers it. This
/// pins it at the NEW SINK, which is the standard this change applied to Numbers:
/// inherited-and-unproven at a new sink is the state that ships leaks.
#[test]
fn a_visible_entity_pulse_counts_no_sealed_mention() {
    let db = file_db("pulse-mixed-visibility");
    seed_folder(&db, "f-open", false);
    seed_folder(&db, "f-sealed", true);
    seed_meeting_with_commitment(&db, "m-open", "f-open", "ship it");
    seed_meeting_with_commitment(&db, "m-sealed-1", "f-sealed", "irrelevant");
    seed_meeting_with_commitment(&db, "m-sealed-2", "f-sealed", "also irrelevant");

    // ONE entity, visible through the open meeting, ALSO mentioned in two sealed ones.
    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    for m in ["m-open", "m-sealed-1", "m-sealed-2"] {
        db.add_mention(&atlas, m).unwrap();
    }

    let brief_for = |unlocked: &HashSet<String>| {
        dashboard_brief_inner(
            &db,
            vec![tile("t1", "pulse", Some(&atlas), 0)],
            unlocked,
            MAX_BRIEF_CHARS,
        )
        .unwrap()
    };

    // The SAME entity with ONLY the open mention — the honest baseline.
    let baseline_db = file_db("pulse-open-only");
    seed_folder(&baseline_db, "f-open", false);
    seed_meeting_with_commitment(&baseline_db, "m-open", "f-open", "ship it");
    let baseline_atlas = baseline_db
        .upsert_entity("Atlas", EntityKind::Project)
        .unwrap();
    baseline_db.add_mention(&baseline_atlas, "m-open").unwrap();
    let baseline = dashboard_brief_inner(
        &baseline_db,
        vec![tile("t1", "pulse", Some(&baseline_atlas), 0)],
        &HashSet::new(),
        MAX_BRIEF_CHARS,
    )
    .unwrap();

    let sealed_hidden = brief_for(&HashSet::new());
    assert_eq!(
        sealed_hidden, baseline,
        "a sealed folder's mentions must not move the pulse: two sealed meetings \
         produced a different aggregate than the open-only fixture"
    );
    let packed_hidden = packed_scoped(&db, &HashSet::new(), Some(&sealed_hidden), None)
        .expect("the open mention keeps the pulse tile non-empty");
    let packed_baseline = packed_scoped(&baseline_db, &HashSet::new(), Some(&baseline), None)
        .expect("the baseline pulse must reach the provider sink");
    assert_eq!(
        packed_hidden, packed_baseline,
        "sealed pulse activity must remain indistinguishable at the provider sink"
    );

    // CONTROL — session-unlock the sealed folder and the aggregate MUST change,
    // otherwise the equality above would hold for a pulse that counts nothing at all.
    let unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
    let sealed_visible = brief_for(&unlocked);
    assert_ne!(
        sealed_visible, sealed_hidden,
        "unlocking must change the aggregate, or the equality above is vacuous"
    );
    let packed_visible = packed_scoped(&db, &unlocked, Some(&sealed_visible), None)
        .expect("the unlocked pulse must reach the provider sink");
    assert_ne!(
        packed_visible, packed_hidden,
        "the control must change the provider prompt, or the packed negative is vacuous"
    );
}

/// The DRIFT twin of `a_visible_entity_carries_no_row_from_a_sealed_meeting`.
///
/// Numbers and Drift both read facts, but they are separate `resolve_tile` arms and
/// only Numbers was proven at this sink. "Same query, so it must be fine" is the kind
/// of inherited assumption this change has already been wrong about once — Pulse's
/// aggregates looked like they would be covered by the Numbers proof and were not.
///
/// Drift is also the sharper of the two to leak through: a superseded value with its
/// date says WHEN something changed inside a folder the session cannot read.
#[test]
fn a_visible_entity_drift_carries_no_row_from_a_sealed_meeting() {
    use crate::facts::{FactOp, NewFact};

    let db = file_db("drift-mixed-visibility");
    seed_folder(&db, "f-open", false);
    seed_folder(&db, "f-sealed", true);
    seed_meeting_with_commitment(&db, "m-open", "f-open", "ship it");
    seed_meeting_with_commitment(&db, "m-sealed", "f-sealed", "irrelevant");

    let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
    db.add_mention(&atlas, "m-open").unwrap();
    db.add_mention(&atlas, "m-sealed").unwrap();

    // TWO values of the SAME predicate, so the tile is a real drift lane: the older
    // one readable, the newer one recorded inside the sealed folder.
    let fact = |object: &str, meeting: &str, at: &str| {
        FactOp::Add(NewFact {
            entity_id: atlas.clone(),
            subject: "Atlas".to_string(),
            predicate: "ga_date".to_string(),
            object: object.to_string(),
            valid_from: at.to_string(),
            recorded_at: at.to_string(),
            confidence: 1.0,
            meeting_id: Some(meeting.to_string()),
        })
    };
    db.apply_fact_ops(&[
        fact("OPEN-DATE-APR-30", "m-open", "2026-07-01T09:00:00Z"),
        fact("SEALED-DATE-JUN-14", "m-sealed", "2026-07-02T09:00:00Z"),
    ])
    .unwrap();

    let brief_for = |unlocked: &HashSet<String>| {
        dashboard_brief_inner(
            &db,
            vec![tile("t1", "drift", Some(&atlas), 0)],
            unlocked,
            MAX_BRIEF_CHARS,
        )
        .unwrap()
    };

    let sealed_hidden = brief_for(&HashSet::new());
    assert!(
        !sealed_hidden.contains("SEALED-DATE-JUN-14"),
        "a value recorded in a sealed meeting must not reach a prompt: {sealed_hidden}"
    );
    let packed_hidden = packed_scoped(&db, &HashSet::new(), Some(&sealed_hidden), None)
        .expect("the readable drift step keeps the tile non-empty");
    assert!(
        !packed_hidden.contains("SEALED-DATE-JUN-14"),
        "the sealed drift step must stay absent at the provider sink: {packed_hidden}"
    );
    assert!(
        sealed_hidden.contains("OPEN-DATE-APR-30"),
        "…while the readable value on the SAME lane does — which is what makes the \
         negative a gate and not an empty tile: {sealed_hidden}"
    );

    // CONTROL — unlock, and the withheld step joins the same lane.
    let unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
    let visible = brief_for(&unlocked);
    assert!(
        visible.contains("SEALED-DATE-JUN-14"),
        "unlocking must restore the step, or the leak test is vacuous: {visible}"
    );
    let packed_visible = packed_scoped(&db, &unlocked, Some(&visible), None)
        .expect("the unlocked drift lane must reach the provider sink");
    assert!(
        packed_visible.contains("SEALED-DATE-JUN-14"),
        "the control must reach the provider sink, or the packed negative is vacuous: {packed_visible}"
    );
}

/// The PERSON twin — the fourth and last of the mixed-visibility set.
///
/// `person` is the one derived kind `tile_is_withheld` routes to `_ => false` while
/// still carrying numbers, so its negative rests entirely on `resolve_tile`'s
/// unchanged internals. Both of those numbers are disclosure even though neither is
/// text: `mentionCount` says how much a person appears in meetings the session cannot
/// read, and `openCommitments` says how much they owe inside them.
///
/// Its counts come from `list_entities_visible` and `list_open_commitments(unlocked)`,
/// which are different readers again from the facts query Numbers/Drift use and the
/// mention query Pulse uses — which is exactly why each kind needed its own proof at
/// this sink rather than one standing for all four.
#[test]
fn a_visible_person_counts_nothing_from_a_sealed_meeting() {
    let db = file_db("person-mixed-visibility");
    seed_folder(&db, "f-open", false);
    seed_folder(&db, "f-sealed", true);
    // The SAME owner commits in both an open and a sealed meeting.
    seed_meeting_with_commitment(&db, "m-open", "f-open", "the readable obligation");
    seed_meeting_with_commitment(&db, "m-sealed", "f-sealed", "the sealed obligation");

    let marcus = db.upsert_entity("Marcus", EntityKind::Person).unwrap();
    db.add_mention(&marcus, "m-open").unwrap();
    db.add_mention(&marcus, "m-sealed").unwrap();

    let brief_for = |unlocked: &HashSet<String>| {
        dashboard_brief_inner(
            &db,
            vec![tile("t1", "person", Some(&marcus), 0)],
            unlocked,
            MAX_BRIEF_CHARS,
        )
        .unwrap()
    };

    // The honest baseline: the same person seen ONLY through the open meeting.
    let base_db = file_db("person-open-only");
    seed_folder(&base_db, "f-open", false);
    seed_meeting_with_commitment(&base_db, "m-open", "f-open", "the readable obligation");
    let base_marcus = base_db.upsert_entity("Marcus", EntityKind::Person).unwrap();
    base_db.add_mention(&base_marcus, "m-open").unwrap();
    let baseline = dashboard_brief_inner(
        &base_db,
        vec![tile("t1", "person", Some(&base_marcus), 0)],
        &HashSet::new(),
        MAX_BRIEF_CHARS,
    )
    .unwrap();

    let sealed_hidden = brief_for(&HashSet::new());
    assert!(
        !sealed_hidden.contains("the sealed obligation"),
        "a sealed commitment must not surface through a person tile: {sealed_hidden}"
    );
    // Compare the COUNTS, not the whole render: the baseline lives in its own temp DB,
    // so its entity UUID differs for reasons that have nothing to do with visibility.
    let counts = |t: &str| -> String {
        t.split(" · ")
            .filter(|f| f.starts_with("visibleMeetings:") || f.starts_with("openCommitments:"))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    assert_eq!(
        counts(&sealed_hidden),
        counts(&baseline),
        "neither COUNT may move: a total that rises with sealed activity says how busy \
         a locked folder is without printing a word of it. sealed-present={sealed_hidden:?} \
         baseline={baseline:?}"
    );
    let packed_hidden = packed_scoped(&db, &HashSet::new(), Some(&sealed_hidden), None)
        .expect("the readable person data keeps the tile non-empty");
    let packed_baseline = packed_scoped(&base_db, &HashSet::new(), Some(&baseline), None)
        .expect("the baseline person tile must reach the provider sink");
    assert_eq!(
        counts(&packed_hidden),
        counts(&packed_baseline),
        "sealed person activity must not change counts at the provider sink"
    );

    // CONTROL — unlocking MUST change the counts, or the equality above would hold for
    // a person tile that counts nothing at all.
    let unlocked: HashSet<String> = ["f-sealed".to_string()].into_iter().collect();
    let visible = brief_for(&unlocked);
    // Same DB here, so the id is stable and the counts are the only thing that can move.
    assert_ne!(
        counts(&visible),
        counts(&sealed_hidden),
        "unlocking must change the counts, or the equality above is vacuous: \
         unlocked={visible:?} sealed={sealed_hidden:?}"
    );
    let packed_visible = packed_scoped(&db, &unlocked, Some(&visible), None)
        .expect("the unlocked person tile must reach the provider sink");
    assert_ne!(
        counts(&packed_visible),
        counts(&packed_hidden),
        "the control must change provider-visible counts, or the packed negative is vacuous"
    );
}

/// A relock racing the brief build cannot produce a HALF-GATED brief.
///
/// `board_scoped_floor_inputs` takes `lifecycle_guard` BEFORE `unlocked_snapshot` and
/// holds it across every `resolve_tile`. Sharing the caller's snapshot alone would not
/// give this: it prevents a second, later snapshot, but only the guard serializes
/// against a relock landing between the snapshot and the last tile. The later
/// `require_current_content_visibility_snapshot` cannot cover the window either — it
/// runs AFTER the provider call, so it suppresses the RESULT, not the disclosure.
///
/// THE ASSERTION IS DELIBERATELY ORDER-FREE, which is what keeps this test honest
/// rather than flaky. It does NOT claim the build lands on a particular side of the
/// relock — that would be a timing assumption, and a timing-sensitive test on a
/// lock-model path is worse than no test. It claims the property that must hold on
/// EITHER side: two tiles whose rows come from the SAME folder are both readable or
/// both withheld. A build that straddled the relock would show one and not the other,
/// and that can only fail when serialization is actually broken.
#[test]
fn a_relock_racing_the_brief_build_cannot_half_gate_it() {
    use std::sync::Arc;

    for round in 0..8 {
        let state = Arc::new(brief_state(&format!("relock-race-{round}")));
        let db = &state.db;
        seed_folder(db, "f-sealed", true);
        seed_meeting_with_commitment(db, "m-sealed", "f-sealed", "the raced obligation");
        // A reminder anchored to the SAME sealed meeting, so both tiles rise and fall
        // together — one folder, two independent gated readers.
        db.create_reminder(
            "r1",
            &ReminderDraft {
                title: "THE RACED REMINDER".to_string(),
                details: None,
                due_at: 1_780_000_000_000,
                repeat_every: None,
                repeat_unit: None,
                sources: vec![ReminderSourceAnchor {
                    kind: "meeting".to_string(),
                    id: "m-sealed".to_string(),
                }],
            },
            ReminderOrigin::Manual,
            1_780_000_000_000,
        )
        .unwrap();
        db.insert_dashboard("b-1", "Board", None, None, "2026-08-01T09:00:00Z")
            .unwrap();
        db.insert_dashboard_tile(
            "t1",
            "b-1",
            "promises",
            None,
            None,
            4,
            None,
            "2026-08-01T09:00:00Z",
        )
        .unwrap();
        db.insert_dashboard_tile(
            "t2",
            "b-1",
            "reminders",
            None,
            None,
            4,
            None,
            "2026-08-01T09:00:00Z",
        )
        .unwrap();
        state
            .unlocked_folders
            .lock()
            .unwrap()
            .insert("f-sealed".to_string());

        // A relock that takes the SAME guard the brief build takes.
        let racer = {
            let s = Arc::clone(&state);
            std::thread::spawn(move || {
                let _guard = crate::commands::lifecycle_guard(&s);
                s.unlocked_folders.lock().unwrap().clear();
            })
        };

        let cfg = AppConfig::default();
        let (_, brief) =
            crate::commands::board_scoped_floor_inputs(&state, &cfg, Some("b-1")).unwrap();
        racer.join().unwrap();

        let brief = brief.unwrap_or_default();
        let has_promise = brief.contains("the raced obligation");
        let has_reminder = brief.contains("THE RACED REMINDER");
        assert_eq!(
            has_promise, has_reminder,
            "round {round}: the guard must serialize the whole build — a brief showing \
             one tile's rows from a folder while hiding the other's is half-gated: {brief}"
        );
    }
}

/// A WHITESPACE-ONLY brief must cost nothing — no text, and no budget.
///
/// The reservation and the join used to read DIFFERENT values: the subtraction took the
/// RAW brief's length, the join took the TRIMMED one. `Some("   ")` therefore shrank the
/// corpus budget while contributing not one character, so the corpus silently lost room
/// to whitespace.
///
/// The lock-security review filed this as INFO and it was right to: `dashboard_brief_inner`
/// returns either `String::new()` or a string starting with `BRIEF_HEADER`, so the state is
/// unreachable in production. It is pinned here anyway because "unreachable" was an accident
/// of the producer, not a property of this function — and `ask.rs` is in the owned set of the
/// next phase. The fix collapses whitespace-only to `""` ONCE, so both sites read one value.
///
/// The 600-space control makes any reservation for invisible text observable.
#[test]
fn a_whitespace_only_brief_costs_no_corpus_budget() {
    let db = file_db("whitespace-brief");
    seed_folder(&db, "f-open", false);
    for i in 0..40 {
        seed_meeting_with_commitment(
            &db,
            &format!("m{i}"),
            "f-open",
            &format!("obligation {i} with a deliberately long tail of words to spend the budget"),
        );
    }
    let sources: Vec<SourceRef> = (0..40)
        .map(|i| SourceRef {
            kind: crate::links::LinkKind::Meeting,
            id: format!("m{i}"),
        })
        .collect();

    // Long enough that spending it would be unmistakable against the 4000-char budget.
    let blank = " ".repeat(600);
    let with_blank =
        packed_with_provider(&db, &HashSet::new(), Some(&blank), Some(&sources), "ollama")
            .unwrap_or_default();
    let without = packed_with_provider(&db, &HashSet::new(), None, Some(&sources), "ollama")
        .unwrap_or_default();

    // Without a binding budget the two would match trivially and this would prove nothing.
    assert!(
        without.chars().count() > 3_000,
        "the corpus must saturate the 4000-char budget or this test is vacuous: {}",
        without.chars().count()
    );
    assert_eq!(
        with_blank, without,
        "a whitespace-only brief must neither add text nor take budget"
    );
}

/// The canned answer for an EMPTY board-scoped ask must not send the user to fix the
/// wrong thing — and must still disclose nothing.
///
/// A board whose every tile is withheld packs no corpus, so the floor returns
/// `AskFloorPrompt::Empty`. That branch used to say "No meeting notes to search yet —
/// record and summarize a meeting first", which is true of an empty VAULT and false of
/// a board full of locked tiles: it tells someone whose content exists and is sealed to
/// go record a meeting. This change is what makes that state reachable, so the wrong
/// message is this change's to fix.
///
/// THE SECOND ASSERTION IS THE LOCK-RELEVANT ONE. An all-sealed board and a board with
/// no tiles at all must produce the SAME words. If the sealed board got a distinguishable
/// message, the message itself would answer "does this board have content?" for a session
/// that is not allowed to know — disclosure by phrasing rather than by payload.
#[test]
fn a_board_scoped_empty_ask_does_not_tell_the_user_to_record() {
    fn empty_answer(db: &crate::storage::Db, brief: Option<&str>) -> String {
        let cfg = AppConfig {
            semantic_search_enabled: false,
            provider_id: "ollama".to_string(),
            cloud_egress_consented: true,
            ..AppConfig::default()
        };
        match build_ask_vault_floor_prompt(
            db,
            &cfg,
            &HashSet::new(),
            "who owes me something on this board?",
            &[],
            "",
            None,
            None,
            None,
            brief,
        )
        .unwrap()
        {
            AskFloorPrompt::Empty(result) => result.answer,
            AskFloorPrompt::Ready { .. } => panic!("fixture must pack nothing"),
        }
    }

    // A board whose every tile is withheld: scoped (`Some`), but with nothing to say.
    let sealed_db = file_db("empty-ask-sealed");
    seed_folder(&sealed_db, "f-sealed", true);
    seed_meeting_with_commitment(&sealed_db, "m-sealed", "f-sealed", "a sealed obligation");
    let sealed = empty_answer(&sealed_db, Some(""));

    assert!(
        !sealed.contains("record and summarize"),
        "a board-scoped ask must not tell the user to record a meeting: {sealed}"
    );
    assert!(
        sealed.contains("board"),
        "the board-scoped answer must name the board, not the vault: {sealed}"
    );

    // NON-DISCLOSURE: a board with no tiles at all must read identically.
    let bare_db = file_db("empty-ask-bare");
    let bare = empty_answer(&bare_db, Some(""));
    assert_eq!(
        sealed, bare,
        "an all-sealed board and an empty board must be indistinguishable from their \
         answer — otherwise the phrasing itself discloses that content exists"
    );

    // CONTROL — without a board, the vault-wide wording is still the right one, so the
    // assertion above is about board scope and not about the string having been deleted.
    let no_board = empty_answer(&bare_db, None);
    assert!(
        no_board.contains("record and summarize"),
        "an unscoped empty ask must keep the vault-wide guidance: {no_board}"
    );
}
