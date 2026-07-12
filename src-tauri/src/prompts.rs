//! Brain v2 L3 — SINGLE-SOURCE shared prompt constants.
//!
//! Migration is deliberately INCREMENTAL (spec §L3 / decision #5): the constants that were
//! duplicated or scattered across `transcribe::live` and `voice_action` move here first, with
//! re-exports from the old locations so every existing import/test keeps working, and ZERO
//! behavior change (the rendered prompts stay byte-identical — the existing prompt-pinning tests
//! in `transcribe::live` are the proof).
//!
//! TODO(L3, spec decision #5): migrate `transcribe::live::assistant_system_prompt`'s BODY here in
//! a follow-up — it is NOT a clean cut today (it leans on `live::tail_chars` +
//! `LIVE_TRANSCRIPT_INJECT_CHARS` and its byte-pinned tests live beside it). Constants only for now.

/// The prompt-set version stamp. Bump when any shared prompt constant changes semantically, so
/// eval artifacts / bake-off reports can attribute a metric shift to a prompt change (spec §L3:
/// "eval artifacts stamp the version"). Never PII.
pub const PROMPT_VERSION: &str = "v2026-07-10";

// ── Cascade tier SUFFIXES (moved verbatim from `transcribe::live`) ──────────────────────────────
// Each is appended to the shared `assistant_system_prompt` (which injects the current-meeting live
// buffer / typed notes / memory brief). They give the tier its SCOPE contract + the `__ESCALATE__`
// escalation instruction. The suffix is prose only — the actual tool boundary is STRUCTURAL
// (`AssistantScope` in the executor), so a model that ignores the prose still cannot reach a higher
// tier's tools. `__ESCALATE__` must exactly match `crate::agent::ESCALATE_SENTINEL` (drift-guarded
// by `tier_suffixes_carry_the_exact_escalation_sentinel` below).

/// Tier 1 — answer ONLY from THIS meeting's own content (the injected live transcript / typed notes),
/// never the vault. If it is not answerable from this meeting, escalate.
pub const TIER1_SUFFIX: &str = "SCOPE — CURRENT MEETING ONLY: Answer STRICTLY from THIS meeting's own \
    content shown above (its live transcript and the user's typed notes). Do NOT use any outside \
    knowledge or other saved meetings. You have NO search tools at this step — that is intentional. \
    If — and only if — the question CANNOT be answered from THIS meeting's content, reply with \
    EXACTLY this JSON and nothing else: {\"answer\":\"__ESCALATE__\"}. Otherwise answer normally.";

/// Tier 2 — answer from the user's OWN VAULT (their saved meetings/notes) via the gated search tools;
/// if the vault doesn't cover it, escalate. B1 (Shared Brain): the org/Shared Brain tool itself is
/// NOT reachable at this tier (it is Tier-3/Connectors-class, `tools::AssistantScope::allows`), so the
/// steering here is to escalate rather than give up — the ACTUAL "also try org" instruction lives in
/// [`TIER3_SUFFIX`], the tier that can actually call it.
pub const TIER2_SUFFIX: &str = "SCOPE — YOUR VAULT: Answer from the user's OWN saved meetings and notes, \
    using the gated vault search tools to ground your answer. Cite meetings by their [[Title]] \
    wikilink. If — and only if — the answer is NOT in the user's vault (it needs the web, Jira, \
    Slack, the calendar, or your organization's Shared Brain), reply with EXACTLY this JSON and \
    nothing else: {\"answer\":\"__ESCALATE__\"}. Otherwise answer normally.";

/// Tier 3 — TERMINAL: reach the consent-gated connectors/web. If it still can't be answered, say so
/// honestly — there is NO further tier, so NEVER emit the escalation sentinel here. B1 (Shared Brain):
/// explicitly steers the model to also try `org_brain_search` before concluding it doesn't know — the
/// tool is already advertised at this tier (`AssistantScope::Connectors` allows `org_brain_search`,
/// tools.rs) but a model that never CHOOSES to call it would otherwise silently skip a colleague's
/// answer. No new tier/UI/badge — reuses the existing `[org · author]` citation format.
pub const TIER3_SUFFIX: &str = "SCOPE — CONNECTORS & WEB (last resort): Use the consent-gated connector \
    and web tools (and the vault tools for grounding) to answer. If the current meeting and your own \
    vault don't answer this, ALSO try org_brain_search (your organization's Shared Brain) before \
    concluding you don't know — a colleague may have already answered it. Loud-attribute external \
    facts (\"(via web)\", \"(via Jira)\", \"(via Slack)\") and org facts by their \"[org · author]\" \
    provenance. This is the LAST step — if you still cannot find the answer, say so plainly; do NOT \
    emit any escalation marker.";

// ── RECORDING-AWARENESS phrases (moved verbatim from `voice_action`) ─────────────────────────────
// The THREE load-bearing substrings BOTH the cloud cascade prompt
// (`transcribe::live::assistant_system_prompt`, which interpolates them) AND the deterministic
// floor (`voice_action::rag_answer`) compose into their prose — defined ONCE here so the two
// prompts can never drift. They are BARE substrings composed into larger sentences, so both
// prompts read naturally while sharing the exact wording the cascade tests pin.

/// A meeting is being **recorded RIGHT NOW** (both the empty-buffer and has-content recording
/// prompt branches open with it).
pub const RECORDING_NOW_PHRASE: &str = "recorded RIGHT NOW";
/// The honest empty-buffer answer — the **meeting just started** and little has been captured.
pub const MEETING_JUST_STARTED_PHRASE: &str = "meeting just started";
/// The substitution BAN — **do NOT search the vault for other saved meetings** and describe them
/// as if they were this one.
pub const NO_SUBSTITUTE_OTHER_MEETINGS_PHRASE: &str =
    "do NOT search the vault for other saved meetings";

// ── Brain v2 L4 — INCREMENTAL LIVE BULLETS (the running-notes prompt) ────────────────────────────
// Used by `transcribe::bullets::update_bullets` on the LOCAL light reasoner only (never a cloud
// call): previous bullets + the new transcript delta → at most 3 NEW bullets, or the literal
// `NOTHING`. Prefix-prompted so the (KV-cached) previous bullets stay a stable prefix across calls.

/// The literal token the bullets model must emit when the delta adds nothing noteworthy.
/// `transcribe::bullets::update_bullets` treats it (case-insensitively) as "no new bullets".
pub const LIVE_BULLETS_NOTHING: &str = "NOTHING";

/// System prompt for the incremental live-bullets update (Brain v2 L4). Small-model-friendly:
/// one narrow task, a fixed line format, and an explicit no-op token.
pub const LIVE_BULLETS_SYSTEM: &str = "You maintain RUNNING NOTES for a meeting in progress. You \
    are given the bullets so far and a NEW fragment of the live transcript. Output ONLY new \
    bullets for genuinely NEW information in the fragment (decisions, facts, owners, dates, open \
    questions) — never restate an existing bullet. Format: at most 3 lines, each exactly \
    `- [topic]: point`, in the language of the transcript. If the fragment adds nothing \
    noteworthy (filler, repetition, small talk), reply with EXACTLY the single word NOTHING and \
    nothing else. No preamble, no commentary, no numbering.";

/// The user message for one incremental bullets update: the bullets so far (may be empty) + the
/// new transcript delta. Pure formatting — no I/O.
pub fn live_bullets_user(previous_bullets: &str, delta: &str) -> String {
    let prev = previous_bullets.trim();
    let prev_block = if prev.is_empty() { "(none yet)" } else { prev };
    format!("NOTES SO FAR:\n{prev_block}\n\nNEW TRANSCRIPT FRAGMENT:\n{}", delta.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DRIFT GUARD: the escalating tiers (1 and 2) must carry EXACTLY the shared sentinel the
    /// ladder detects (`crate::agent::ESCALATE_SENTINEL`); the TERMINAL tier 3 must NOT instruct
    /// the sentinel (it may never escalate).
    #[test]
    fn tier_suffixes_carry_the_exact_escalation_sentinel() {
        assert!(TIER1_SUFFIX.contains(crate::agent::ESCALATE_SENTINEL));
        assert!(TIER2_SUFFIX.contains(crate::agent::ESCALATE_SENTINEL));
        assert!(
            !TIER3_SUFFIX.contains(crate::agent::ESCALATE_SENTINEL),
            "the terminal tier must never be told to emit the sentinel"
        );
    }

    /// B1 (Shared Brain, RED-before-GREEN): the TERMINAL Tier 3 suffix — the only tier that can
    /// actually reach `org_brain_search` (`AssistantScope::Connectors`/`Full`) — must explicitly
    /// steer the model to try it before concluding it doesn't know, mirroring the A2 fallback
    /// sentence. Tier 2 must at least MENTION the Shared Brain as a reason to escalate rather than
    /// give up. Pre-fix, neither suffix mentioned org/Shared Brain at all.
    #[test]
    fn tier3_suffix_steers_toward_org_brain_search_before_giving_up() {
        assert!(
            TIER3_SUFFIX.contains("org_brain_search"),
            "the terminal tier must explicitly name org_brain_search as a fallback: {TIER3_SUFFIX}"
        );
        assert!(
            TIER3_SUFFIX.to_lowercase().contains("shared brain")
                || TIER3_SUFFIX.contains("[org ·"),
            "the terminal tier must frame org search as the Shared Brain / attribute its provenance: {TIER3_SUFFIX}"
        );
        assert!(
            TIER2_SUFFIX.to_lowercase().contains("shared brain"),
            "Tier 2 must mention the Shared Brain as a reason to escalate, not just give up: {TIER2_SUFFIX}"
        );
    }

    /// The prompt version is a stable, dated, non-empty stamp (eval artifacts embed it).
    #[test]
    fn prompt_version_is_a_dated_stamp() {
        assert!(PROMPT_VERSION.starts_with('v'));
        assert!(PROMPT_VERSION.len() >= "v2026-01-01".len());
    }

    /// Brain v2 L4 — the bullets prompt carries the exact no-op token `update_bullets` detects,
    /// and the user message renders both blocks (empty previous bullets get the "(none yet)"
    /// placeholder so the prefix shape is stable).
    #[test]
    fn live_bullets_prompt_carries_nothing_token_and_renders_blocks() {
        assert!(LIVE_BULLETS_SYSTEM.contains(LIVE_BULLETS_NOTHING));
        let u = live_bullets_user("- [budget]: capped at 10k", "we also agreed Anna owns QA");
        assert!(u.contains("NOTES SO FAR:\n- [budget]: capped at 10k"));
        assert!(u.contains("NEW TRANSCRIPT FRAGMENT:\nwe also agreed Anna owns QA"));
        assert!(live_bullets_user("", "delta").contains("(none yet)"));
    }

    /// The recording-awareness phrases stay the exact substrings the cascade prompt tests pin —
    /// changing one here must consciously ripple through those byte-pinned tests.
    #[test]
    fn recording_awareness_phrases_are_the_pinned_substrings() {
        assert_eq!(RECORDING_NOW_PHRASE, "recorded RIGHT NOW");
        assert_eq!(MEETING_JUST_STARTED_PHRASE, "meeting just started");
        assert_eq!(
            NO_SUBSTITUTE_OTHER_MEETINGS_PHRASE,
            "do NOT search the vault for other saved meetings"
        );
    }
}
