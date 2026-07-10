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
/// if the vault doesn't cover it, escalate.
pub const TIER2_SUFFIX: &str = "SCOPE — YOUR VAULT: Answer from the user's OWN saved meetings and notes, \
    using the gated vault search tools to ground your answer. Cite meetings by their [[Title]] \
    wikilink. If — and only if — the answer is NOT in the user's vault (it needs the web, Jira, \
    Slack, or the calendar), reply with EXACTLY this JSON and nothing else: \
    {\"answer\":\"__ESCALATE__\"}. Otherwise answer normally.";

/// Tier 3 — TERMINAL: reach the consent-gated connectors/web. If it still can't be answered, say so
/// honestly — there is NO further tier, so NEVER emit the escalation sentinel here.
pub const TIER3_SUFFIX: &str = "SCOPE — CONNECTORS & WEB (last resort): Use the consent-gated connector \
    and web tools (and the vault tools for grounding) to answer. Loud-attribute external facts \
    (\"(via web)\", \"(via Jira)\", \"(via Slack)\"). This is the LAST step — if you still cannot \
    find the answer, say so plainly; do NOT emit any escalation marker.";

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

    /// The prompt version is a stable, dated, non-empty stamp (eval artifacts embed it).
    #[test]
    fn prompt_version_is_a_dated_stamp() {
        assert!(PROMPT_VERSION.starts_with('v'));
        assert!(PROMPT_VERSION.len() >= "v2026-01-01".len());
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
