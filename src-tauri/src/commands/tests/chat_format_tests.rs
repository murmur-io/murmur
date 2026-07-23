use super::*;

fn msg(role: &str, text: &str) -> ChatMsg {
    ChatMsg {
        role: role.into(),
        text: text.into(),
    }
}

#[test]
fn format_chat_extracts_latest_and_renders_history() {
    let msgs = vec![
        msg("user", "what did we decide on pricing?"),
        msg("assistant", "You agreed on tiered pricing."),
        msg("user", "and the timeline?"),
    ];
    let (latest, convo) = format_chat(&msgs).unwrap();
    // `latest` (drives intent + the floor) is the newest USER message.
    assert_eq!(latest, "and the timeline?");
    // The conversation context carries the prior turns labelled by role → multi-turn memory.
    assert!(convo.contains("User: what did we decide on pricing?"));
    assert!(convo.contains("Assistant: You agreed on tiered pricing."));
    assert!(convo.contains("User: and the timeline?"));
    assert!(convo.contains("LATEST"));
}

#[test]
fn format_chat_rejects_empty_or_non_user_last() {
    assert!(format_chat(&[]).is_err(), "empty chat is rejected");
    assert!(
        format_chat(&[msg("user", "   ")]).is_err(),
        "blank last message is rejected"
    );
    assert!(
        format_chat(&[msg("user", "hi"), msg("assistant", "hello")]).is_err(),
        "the last message must be from the user"
    );
}

/// Brain v2 L3 — the CHAR-budget boundary: exactly-at-budget keeps everything; one char over
/// drops the OLDEST message; the newest message is ALWAYS kept even when it alone busts the
/// budget; an empty slice stays empty.
#[test]
fn trim_history_to_budget_boundary() {
    let m = |text: &str| msg("user", text);

    // 3 × 10 chars against a 30-char budget: EXACTLY at budget ⇒ all kept.
    let msgs = vec![m(&"a".repeat(10)), m(&"b".repeat(10)), m(&"c".repeat(10))];
    assert_eq!(trim_history_to_budget(&msgs, 30).len(), 3);
    // One char under ⇒ the oldest is dropped, newest two kept.
    let kept = trim_history_to_budget(&msgs, 29);
    assert_eq!(kept.len(), 2);
    assert!(kept[0].text.starts_with('b'), "oldest-first trim");
    // Budget below even the newest ⇒ the newest alone is still kept (never dropped).
    let kept_one = trim_history_to_budget(&msgs, 5);
    assert_eq!(kept_one.len(), 1);
    assert!(kept_one[0].text.starts_with('c'));
    // Empty in, empty out.
    assert!(trim_history_to_budget(&[], 100).is_empty());
}

/// Brain v2 L3 — format_chat applies the char budget on top of the turn cap: a chat whose 12
/// recent turns carry ~7k chars each renders only the newest turns that fit 64k, oldest
/// dropped; and small chats are untouched (the turn-cap test above stays green).
#[test]
fn format_chat_trims_oversized_history_to_char_budget() {
    // 12 turns × 7_000 chars = 84k > 64k ⇒ the oldest ~3 turns fall out.
    let mut msgs: Vec<ChatMsg> = (0..11)
        .map(|i| msg("user", &format!("turn-{i}-{}", "x".repeat(7_000))))
        .collect();
    msgs.push(msg("user", "the final question"));
    let (latest, convo) = format_chat(&msgs).unwrap();
    assert_eq!(latest, "the final question");
    assert!(
        convo.contains("the final question"),
        "the newest turn always renders"
    );
    assert!(
        !convo.contains("turn-0-"),
        "an over-budget oldest turn is dropped"
    );
    assert!(
        convo.contains("turn-10-"),
        "the newest big turn still renders"
    );
    assert!(
        convo.chars().count() < CHAT_HISTORY_CHAR_BUDGET + 1_000,
        "the rendered conversation stays near the budget, got {}",
        convo.chars().count()
    );
}

#[test]
fn format_chat_caps_history_to_recent_turns() {
    // A long chat: only the last CHAT_CONTEXT_TURNS are rendered (bounds tokens + cloud egress).
    let mut msgs: Vec<ChatMsg> = (0..39)
        .map(|i| msg("user", &format!("turn-{i}-text")))
        .collect();
    msgs.push(msg("user", "the final question"));
    let (latest, convo) = format_chat(&msgs).unwrap();
    assert_eq!(latest, "the final question");
    assert!(convo.contains("the final question"));
    assert!(
        !convo.contains("turn-0-text"),
        "turns beyond the cap are dropped"
    );
}
