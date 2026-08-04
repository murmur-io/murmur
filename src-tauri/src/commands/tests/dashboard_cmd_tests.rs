//! Unit tests for the DASHBOARDS command layer — the pure, `State`-free helpers: input
//! validation (title/emoji/tint), the tile `config` contract, and the display formatters.
//!
//! The GATED resolution itself is covered where the gate lives (`storage/db_tests/
//! dashboard_tests.rs` for the pulse reader + the existence probe) and by the wire-shape
//! assertions below, which pin the one property the FE depends on for safety: a `Locked` tile
//! serializes with NO payload fields at all.

use super::*;

#[test]
fn titles_are_trimmed_bounded_and_non_empty() {
    assert_eq!(clean_title("  Atlas GA  ", "title").unwrap(), "Atlas GA");
    assert!(clean_title("   ", "title").is_err(), "whitespace-only is empty");
    assert!(clean_title("", "title").is_err());
    let long = "x".repeat(MAX_TITLE_LEN + 1);
    assert!(clean_title(&long, "title").is_err(), "over-long titles are refused");
    let ok = "x".repeat(MAX_TITLE_LEN);
    assert!(clean_title(&ok, "title").is_ok(), "exactly at the cap is allowed");
    // Multi-byte titles are counted in CHARACTERS, not bytes.
    let pl = "ż".repeat(MAX_TITLE_LEN);
    assert!(clean_title(&pl, "title").is_ok());
}

#[test]
fn emoji_is_cosmetic_and_bounded() {
    assert_eq!(clean_emoji(Some("🚀".into())).as_deref(), Some("🚀"));
    assert_eq!(clean_emoji(Some("  🤝 ".into())).as_deref(), Some("🤝"));
    assert_eq!(clean_emoji(None), None);
    assert_eq!(clean_emoji(Some("".into())), None);
    assert_eq!(
        clean_emoji(Some("not an emoji at all".into())),
        None,
        "a long string is not a leading glyph"
    );
    assert_eq!(
        clean_emoji(Some("a\u{0007}".into())),
        None,
        "control characters are refused outright"
    );
}

#[test]
fn tint_is_a_token_name_never_a_raw_colour() {
    assert_eq!(clean_tint(Some("Indigo".into())).as_deref(), Some("indigo"));
    assert_eq!(
        clean_tint(Some("#ff0000".into())),
        None,
        "a raw colour must not reach the stylesheet"
    );
    assert_eq!(clean_tint(Some("url(javascript:alert(1))".into())), None);
    assert_eq!(clean_tint(None), None);
}

#[test]
fn tile_config_round_trips_and_tolerates_absent_fields() {
    let cfg = TileConfig {
        question: Some("Are we going to make Jun 14?".into()),
        answer: Some("Probably not.".into()),
        answered_at: Some("2026-08-03T10:00:00Z".into()),
        ..Default::default()
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back = parse_config(Some(&json));
    assert_eq!(back.question.as_deref(), Some("Are we going to make Jun 14?"));
    assert_eq!(back.owner, None);

    // An empty object, a legacy row, and outright garbage all degrade to defaults rather than
    // failing a whole board render.
    assert_eq!(parse_config(Some("{}")).question, None);
    assert_eq!(parse_config(None).question, None);
    assert_eq!(parse_config(Some("not json")).question, None);
    assert_eq!(
        parse_config(Some(r#"{"predicate":"status","unknownField":42}"#))
            .predicate
            .as_deref(),
        Some("status"),
        "an unknown field must not break deserialization"
    );
}

/// THE WIRE-SHAPE ORACLE: a locked tile must serialize to a discriminant and NOTHING else. If a
/// payload field ever leaks into this arm, this fails — which is the whole point, because the FE
/// renders whatever it is given.
#[test]
fn locked_tile_serializes_with_no_payload() {
    let json = serde_json::to_string(&TileData::Locked).unwrap();
    assert_eq!(json, r#"{"kind":"locked"}"#);
    let json = serde_json::to_string(&TileData::Missing).unwrap();
    assert_eq!(json, r#"{"kind":"missing"}"#);
    let json = serde_json::to_string(&TileData::Unconfigured).unwrap();
    assert_eq!(json, r#"{"kind":"unconfigured"}"#);
}

#[test]
fn resolved_tile_wire_shape_is_flat_for_the_fe() {
    let tile = crate::storage::models::DashboardTile {
        id: "t1".into(),
        dashboard_id: "b1".into(),
        kind: "note".into(),
        ref_id: Some("n1".into()),
        title: None,
        span: 4,
        position: 0,
        config: None,
        created_at: "2026-08-03T10:00:00Z".into(),
    };
    let dto = ResolvedTileDto {
        tile,
        data: TileData::Locked,
    };
    let v: serde_json::Value = serde_json::to_value(&dto).unwrap();
    assert_eq!(v["id"], "t1", "the tile's own fields are flattened");
    assert_eq!(v["data"]["kind"], "locked");
    assert!(v["data"].get("title").is_none());
}

/// THE LEAK ORACLE for the flattened DTO (found by an independent review, 2026-08-03).
///
/// `ResolvedTileDto` flattens the whole `DashboardTile`, and the palette seeds `title` from the
/// chosen source — so a locked tile that kept its title would ship the sealed source's name across
/// IPC even though the UI declines to draw it. `config` carries the Living-answer paraphrase and
/// must go the same way.
#[test]
fn locked_tile_sheds_its_user_authored_chrome() {
    let tile = crate::storage::models::DashboardTile {
        id: "t1".into(),
        dashboard_id: "b1".into(),
        kind: "meeting".into(),
        ref_id: Some("m-sealed".into()),
        title: Some("Acme — termination terms".into()),
        span: 5,
        position: 2,
        config: Some(r#"{"answer":"they will not renew"}"#.into()),
        created_at: "2026-08-03T10:00:00Z".into(),
    };

    let redacted = redact_tile_chrome(tile.clone(), &TileData::Locked);
    assert_eq!(redacted.title, None, "a sealed tile must not ship its title");
    assert_eq!(redacted.config, None, "nor the cached answer in its config");
    assert_eq!(redacted.span, 5, "layout is the user's own, and stays");
    assert_eq!(redacted.position, 2);

    // The serialized wire form is where it actually matters.
    let json = serde_json::to_string(&ResolvedTileDto {
        tile: redacted,
        data: TileData::Locked,
    })
    .unwrap();
    assert!(
        !json.contains("termination"),
        "the sealed title must not appear anywhere on the wire: {json}"
    );
    assert!(!json.contains("will not renew"));

    // CONTROL: a VISIBLE tile keeps its chrome, so the redaction is targeted, not blanket.
    let visible = redact_tile_chrome(
        tile,
        &TileData::Meeting {
            id: "m-1".into(),
            title: "Atlas weekly".into(),
            started_at: "2026-06-03T09:00:00Z".into(),
            duration_s: 60,
            has_audio: false,
        },
    );
    assert_eq!(visible.title.as_deref(), Some("Acme — termination terms"));
}

/// The SECOND half of that leak, and the sharper one: entity-anchored tiles deliberately degrade
/// to an EMPTY view rather than to `Locked`, so a legacy row's copied entity name ("Dana Reyes")
/// would keep rendering as the heading after the folder was sealed. Withholding is keyed on the
/// entity placeholder, so those tiles shed their chrome too.
#[test]
fn entity_tiles_shed_chrome_when_the_entity_is_not_visible() {
    let tile = crate::storage::models::DashboardTile {
        id: "t1".into(),
        dashboard_id: "b1".into(),
        kind: "drift".into(),
        ref_id: Some("e-1".into()),
        title: Some("Dana Reyes".into()),
        span: 4,
        position: 0,
        config: None,
        created_at: "2026-08-03T10:00:00Z".into(),
    };

    let hidden = redact_tile_chrome(
        tile.clone(),
        &TileData::Drift {
            entity: ENTITY_HIDDEN.to_string(),
            predicate: String::new(),
            rows: vec![],
        },
    );
    assert_eq!(
        hidden.title, None,
        "an entity that is no longer visible must not keep its name as the tile heading"
    );

    // Person degrades to Missing — same treatment.
    assert_eq!(redact_tile_chrome(tile.clone(), &TileData::Missing).title, None);

    // CONTROL: a VISIBLE entity keeps the tile's chrome.
    let visible = redact_tile_chrome(
        tile,
        &TileData::Drift {
            entity: "Project Atlas".to_string(),
            predicate: "ga_date".to_string(),
            rows: vec![],
        },
    );
    assert_eq!(visible.title.as_deref(), Some("Dana Reyes"));
}

/// A withheld Living answer must also shed the `config` it is stored in — otherwise the gate on
/// the payload is pointless, because the raw answer rides along in the flattened tile.
#[test]
fn withheld_living_answer_sheds_its_config() {
    let tile = crate::storage::models::DashboardTile {
        id: "t1".into(),
        dashboard_id: "b1".into(),
        kind: "living_answer".into(),
        ref_id: None,
        title: Some("Will they renew?".into()),
        span: 4,
        position: 0,
        config: Some(r#"{"question":"Will they renew?","answer":"No — they are churning"}"#.into()),
        created_at: "2026-08-03T10:00:00Z".into(),
    };
    let out = redact_tile_chrome(
        tile,
        &TileData::LivingAnswer {
            question: "Will they renew?".into(),
            answer: None,
            answered_at: None,
            withheld: true,
        },
    );
    assert_eq!(out.config, None, "the cached answer must not ride along in config");
    let json = serde_json::to_string(&ResolvedTileDto {
        tile: out,
        data: TileData::LivingAnswer {
            question: "Will they renew?".into(),
            answer: None,
            answered_at: None,
            withheld: true,
        },
    })
    .unwrap();
    assert!(!json.contains("churning"), "answer text on the wire: {json}");
}

/// THE ORACLE the previous gate never had. An independent verifier proved that switching the old
/// Living-answer gate off left all 2732 tests green — it was unfalsifiable. This tests the gate
/// itself as a pure function, so neutering it fails here immediately.
///
/// The gate is a READABLE-FOLDER snapshot rather than a source list because `ask_vault`'s pinned
/// path expands into linked NEIGHBOURS (`vault_context.rs`, `LINK_CONTEXT_CAP`) that no caller
/// records — so only a bound on what was readable at the time can cover what the answer saw.
#[test]
fn living_answer_gate_withholds_when_a_folder_stopped_being_readable() {
    let readable = |ids: &[&str]| -> std::collections::HashSet<String> {
        ids.iter().map(|s| (*s).to_string()).collect()
    };
    let recorded = vec!["f-open".to_string(), "f-secret".to_string()];

    // Everything still readable ⇒ the answer shows.
    assert!(!living_answer_withheld(
        true,
        &recorded,
        &readable(&["f-open", "f-secret", "f-new"]),
    ));

    // `f-secret` got sealed ⇒ WITHHELD, even though it was never a tile source: the answer could
    // have paraphrased it via link expansion.
    assert!(living_answer_withheld(
        true,
        &recorded,
        &readable(&["f-open"]),
    ));

    // A folder that vanished entirely is also "not readable" ⇒ withheld.
    assert!(living_answer_withheld(true, &recorded, &readable(&[])));

    // FAIL-CLOSED: a legacy row with no recorded snapshot cannot be checked ⇒ withheld.
    assert!(living_answer_withheld(true, &[], &readable(&["f-open"])));

    // No answer at all is not "withheld" — the tile just has nothing yet.
    assert!(!living_answer_withheld(false, &[], &readable(&[])));
}

#[test]
fn living_answer_config_round_trips_its_readable_snapshot() {
    let cfg = TileConfig {
        question: Some("Will we make Jun 14?".into()),
        answer: Some("Probably not.".into()),
        answer_readable_folders: Some(vec!["f-open".into()]),
        ..Default::default()
    };
    let back = parse_config(Some(&serde_json::to_string(&cfg).unwrap()));
    assert_eq!(back.answer_readable_folders.as_ref().map(Vec::len), Some(1));
    // A legacy row (an answer, no recorded sources) parses — and the resolver treats that as
    // un-gateable, i.e. withheld. See `resolve_tile`'s living_answer arm.
    let legacy = parse_config(Some(r#"{"question":"q","answer":"a"}"#));
    assert_eq!(legacy.answer_readable_folders, None);
    assert!(legacy.answer.is_some());
}

#[test]
fn snippets_are_bounded_and_ellipsized() {
    assert_eq!(snippet_of("  short  ", 10), "short");
    let s = snippet_of(&"a".repeat(50), 10);
    assert_eq!(s.chars().count(), 11, "10 chars + the ellipsis");
    assert!(s.ends_with('…'));
    // Multi-byte input must not panic on a char boundary.
    let pl = snippet_of(&"żółw ".repeat(40), 12);
    assert!(pl.ends_with('…'));
}

#[test]
fn numeric_detection_picks_figures_not_prose() {
    assert!(looks_numeric("180000"));
    assert!(looks_numeric("$180k"));
    assert!(looks_numeric("2026-06-14"));
    assert!(looks_numeric("+3 weeks"));
    assert!(!looks_numeric("in progress"));
    assert!(!looks_numeric(""));
    assert!(!looks_numeric("shipped"));
}

#[test]
fn commitment_status_reads_the_due_date() {
    assert_eq!(commitment_status(None), "open");
    assert_eq!(commitment_status(Some("1999-01-01")), "late");
    assert_eq!(commitment_status(Some("2999-01-01")), "open");
    // LOCAL, not UTC: a due date is a calendar day a human wrote down. Asserting against the UTC
    // day is what made this test disagree with the fix east of Greenwich around local midnight.
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert_eq!(commitment_status(Some(&today)), "due");
    let yesterday = (chrono::Local::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    assert_eq!(commitment_status(Some(&yesterday)), "late");
    // A malformed date is never "late" — an unparsable string must not nag the user.
    assert_eq!(commitment_status(Some("soon")), "open");
}

#[test]
fn epoch_formatting_tolerates_seconds_and_millis() {
    // 2026-06-14T00:00:00Z
    let secs = 1_781_308_800i64;
    assert_eq!(format_epoch_day(secs), format_epoch_day(secs * 1000));
    assert!(!format_epoch_day(secs).is_empty());
}

#[test]
fn iso_parsing_accepts_both_stored_shapes() {
    assert!(iso_to_epoch_secs("2026-08-01T09:00:00Z").is_some());
    assert!(iso_to_epoch_secs("2026-08-01T09:00:00+02:00").is_some());
    assert!(iso_to_epoch_secs("2026-08-01T09:00:00").is_some());
    assert!(iso_to_epoch_secs("nonsense").is_none());
}

/// The tile-kind allowlist and the FE's renderer must not drift apart silently: every kind the
/// store accepts has an arm in the resolver's `match` (checked by exhaustiveness here — a new
/// kind added to `TILE_KINDS` without a resolver arm fails this test).
#[test]
fn every_tile_kind_is_resolvable() {
    let handled = [
        "note",
        "meeting",
        "document",
        "person",
        "reminders",
        "drift",
        "numbers",
        "pulse",
        "promises",
        "living_answer",
    ];
    for kind in TILE_KINDS {
        assert!(
            handled.contains(kind),
            "tile kind `{kind}` is storable but has no resolver arm"
        );
    }
    assert_eq!(handled.len(), TILE_KINDS.len(), "no stale handled kind");
}
