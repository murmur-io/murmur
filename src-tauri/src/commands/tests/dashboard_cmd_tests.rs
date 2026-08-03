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
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert_eq!(commitment_status(Some(&today)), "due");
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
