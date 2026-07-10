//! Connector-agnostic note ENRICHMENT primitive.
//!
//! The write-half of "fold live connector context INTO the note" (see
//! `docs/research/2026-07-05-connector-note-enrichment.md`). This is the pure, deterministic,
//! headless-testable core — no network, no connectors, no clock, no LLM. Given the note markdown +
//! a set of already-fetched [`ContextHit`]s (each from ANY connector — Jira, Slack, web, Linear,
//! …, loud-attributed by its own `source`), it appends ONE consolidated, collapsed Obsidian
//! `> [!context]-` callout, dated with a caller-supplied `as_of`.
//!
//! Design (mirrors the shipped `verify::apply_verify_markers` discipline, generalized to any source):
//! - **Append-only + byte-preserving.** Every ORIGINAL line of the note is preserved byte-identically;
//!   we only add (or replace) our own managed block at the end. Front-matter and prose are untouched.
//! - **Idempotent.** The block lives inside a stable HTML-comment fence (invisible in rendered
//!   Obsidian). Re-enriching STRIPS the old fenced block first, then re-appends — never stacks.
//! - **Byte-exact undo.** `apply_context_markers(md, &[], _)` strips the block and returns the note
//!   exactly as it was before enrichment. `apply(apply(md, h, t), h, t) == apply(md, h, t)`.
//! - **Pure.** `as_of` is an INPUT (the caller passes the enrichment timestamp), so the function is
//!   deterministic and unit-testable without a clock; production passes `chrono::Utc::now()`.
//!
//! WHERE it must be persisted (NOT decided here — the command layer's job): into the CANONICAL DB
//! note markdown via `upsert_note` (so it SEALS with the note under the folder lock), exactly like
//! `apply_note_verify_markers`. NOT the vault-file-only path Re-Truth uses (`overwrite_note`), which
//! is dropped on seal. See the research brief §F2.

use serde::{Deserialize, Serialize};

/// The fence that delimits our managed enrichment block. HTML comments render as nothing in
/// Obsidian, and are our own marker so the strip is unambiguous and can never collide with prose.
const FENCE_START: &str = "<!-- murmur:context -->";
const FENCE_END: &str = "<!-- /murmur:context -->";

/// Lane A (Stage 2) — the CROSS-MEETING local LINKS managed block. A DISTINCT fence from the
/// connector `murmur:context` block above so the two lanes are INDEPENDENT, self-managed blocks:
/// each strips + reapplies ONLY its own fence, so a note can carry BOTH at once and neither disturbs
/// the other. Lane A is deterministic + ZERO-EGRESS (local `[[Title]]` links + task-free gists).
const LINKS_FENCE_START: &str = "<!-- murmur:links -->";
const LINKS_FENCE_END: &str = "<!-- /murmur:links -->";

/// Defense cap on hits rendered in one block (the CALLER should pre-filter for relevance; this only
/// bounds a runaway from turning a note into a wall). Kept generous — real callers pass a handful.
const MAX_CONTEXT_HITS: usize = 12;

/// One already-fetched, already-redaction-safe piece of connector context to fold into the note.
/// Connector-AGNOSTIC: `source` is the truthful connector label (matching the connector's
/// `egress_attribution` / `source_label` — e.g. "Jira", "Slack", "Linear", "web") so every line is
/// loudly attributed `(via <source>)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextHit {
    /// Truthful connector label, e.g. "Jira" / "Slack" / "Linear". Rendered as `(via {source})`.
    pub source: String,
    /// One-line human summary containing ONLY connector-sourced values (status/assignee/snippet).
    /// Any CR/LF is collapsed to a single space so a hit can never spawn a second, un-strippable line.
    pub detail: String,
    /// Optional deep link back to the source item (issue / message permalink).
    pub url: Option<String>,
}

/// Append (or idempotently replace) the managed `> [!context]-` block carrying `hits` (Lane B,
/// connector context), dated `as_of`. `hits` empty ⇒ the block is STRIPPED and the original note
/// returned byte-identical. See the module docs for the invariants.
pub fn apply_context_markers(note_md: &str, hits: &[ContextHit], as_of: &str) -> String {
    let callout = format!("> [!context]- Live context (as of {})", sanitize(as_of));
    let body: Vec<String> = hits.iter().take(MAX_CONTEXT_HITS).map(render_hit_line).collect();
    apply_fenced_block(note_md, FENCE_START, FENCE_END, &callout, &body)
}

/// Lane A (Stage 2) — append (or idempotently replace) the managed `> [!related]- Related notes`
/// block carrying cross-meeting `hits` (each a task-free gist + a `[[Title]]` wikilink), under the
/// DISTINCT `murmur:links` fence. `hits` empty ⇒ the block is STRIPPED and the note returned
/// byte-identical (so re-running self-heals the link graph). INDEPENDENT of the connector
/// `> [!context]-` block: the two coexist and each strips only its OWN fence. Same idempotency /
/// byte-exact-undo / sanitize-hardening / seal-safety as [`apply_context_markers`].
///
/// UNLIKE Lane B, this block carries NO `as_of` timestamp: cross-meeting links point at OWNED notes
/// (timeless) not a live external snapshot, so the rendered block is a pure function of `(note, hits)`.
/// That determinism is load-bearing — it lets the caller's `linked == existing` short-circuit skip a
/// rewrite when the link set is unchanged, so the deferred auto-pass never churns the note / vault
/// `.md` on every re-summarize (Phase-2 finding #3).
pub fn apply_link_markers(note_md: &str, hits: &[ContextHit]) -> String {
    let callout = "> [!related]- Related notes";
    let body: Vec<String> = hits.iter().take(MAX_CONTEXT_HITS).map(render_hit_line).collect();
    apply_fenced_block(note_md, LINKS_FENCE_START, LINKS_FENCE_END, callout, &body)
}

/// Render ONE hit as a collapsed, sanitized, loudly-attributed callout body line (no trailing
/// newline — the block writer adds the break). Shared by BOTH lanes (context + links) so the line
/// shape + injection-hardening are identical everywhere.
fn render_hit_line(h: &ContextHit) -> String {
    let detail = sanitize(&h.detail);
    let via = format!("(via {})", sanitize(&h.source));
    match h.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => format!("> - {detail} {via} — {}", sanitize(u)),
        None => format!("> - {detail} {via}"),
    }
}

/// Shared strip+append engine for ONE managed fenced block. Strips this note's EXISTING block for
/// `(fence_start, fence_end)` via [`strip_fenced_block`], then — if `body_lines` is non-empty —
/// appends a fresh block `\n\n{fence_start}\n{callout_line}\n{body…}\n{fence_end}`. An EMPTY
/// `body_lines` returns the stripped note byte-for-byte (byte-exact undo). Callers MUST pre-
/// [`sanitize`] every value flowing into `callout_line` / `body_lines`.
///
/// `pub(crate)` so sibling managed-block lanes (the verify `> [!verify]-` callout in
/// [`crate::verify::apply_verify_callout`]) reuse EXACTLY this engine — one fence discipline,
/// one set of idempotency / byte-exact-undo / anti-forging invariants, never a re-implementation.
pub(crate) fn apply_fenced_block(
    note_md: &str,
    fence_start: &str,
    fence_end: &str,
    callout_line: &str,
    body_lines: &[String],
) -> String {
    let base = strip_fenced_block(note_md, fence_start, fence_end);
    if body_lines.is_empty() {
        return base;
    }
    let mut block = String::new();
    block.push_str(fence_start);
    block.push('\n');
    block.push_str(callout_line);
    block.push('\n');
    for line in body_lines {
        block.push_str(line);
        block.push('\n');
    }
    block.push_str(fence_end);
    // Append at the very end, separated from the note body by a blank line. `strip_fenced_block`
    // removes exactly this leading separator + fenced region, so append/strip are byte-exact inverses.
    format!("{base}\n\n{block}")
}

/// Remove the managed fenced block for `(fence_start, fence_end)` (and the `\n\n` separator inserted
/// before it), restoring the note byte-for-byte to its pre-block state for THAT fence. A no-op when
/// the note carries no such block. Only this fence's block is touched — a sibling lane's block (a
/// different fence) is left intact, which is what lets the links + context blocks coexist.
fn strip_fenced_block(md: &str, fence_start: &str, fence_end: &str) -> String {
    let Some(start) = md.rfind(fence_start) else {
        return md.to_string();
    };
    // The end fence must follow the start fence; if a note somehow carries a lone start fence, leave
    // it untouched rather than truncate real content.
    let Some(end_rel) = md[start..].find(fence_end) else {
        return md.to_string();
    };
    let end = start + end_rel + fence_end.len();
    // Reclaim the separator we wrote before the fence (`\n\n`), or a lone `\n`, if present.
    let before = &md[..start];
    let cut_start = if before.ends_with("\n\n") {
        start - 2
    } else if before.ends_with('\n') {
        start - 1
    } else {
        start
    };
    let mut out = String::with_capacity(md.len());
    out.push_str(&md[..cut_start]);
    out.push_str(&md[end..]);
    out
}

/// Make a connector-supplied value safe to embed in the callout:
/// - collapse CR/LF + whitespace runs to single spaces so it can never spawn a second line that
///   escapes the block / injects a line-start callout (mirrors `verify::apply_verify_markers`);
/// - NEUTRALIZE HTML-comment delimiters (`<!--` / `-->`) so a hostile or attacker-influenced hit
///   (any web snippet / Slack message the search returns) can never contain — or FORGE — EITHER
///   managed-block fence (`murmur:context` or `murmur:links`). Without this, a value carrying
///   `<!-- /murmur:context -->` would make the next `strip_fenced_block` match that embedded marker
///   first and cut mid-block, permanently breaking the byte-exact undo / idempotency invariant
///   (2026-07-05 lock-security finding). The broken tokens render as harmless literal text.
///
/// `pub(crate)` so the verify-callout lane ([`crate::verify::apply_verify_callout`]) applies the
/// SAME hardening to its connector-sourced values.
pub(crate) fn sanitize(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
        .replace("<!--", "<! --")
        .replace("-->", "-- >")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const AS_OF: &str = "2026-07-05T14:32:00Z";

    fn hit(source: &str, detail: &str, url: Option<&str>) -> ContextHit {
        ContextHit {
            source: source.into(),
            detail: detail.into(),
            url: url.map(String::from),
        }
    }

    fn jira_slack() -> Vec<ContextHit> {
        vec![
            hit("Jira", "PROJ-123 · In Progress · due 2026-07-10", Some("https://acme.atlassian.net/browse/PROJ-123")),
            hit("Slack", "\"ship it Friday\" in #eng", Some("https://acme.slack.com/archives/C1/p123")),
        ]
    }

    /// Lane A links: task-free gists + `[[Title]]` wikilinks, source "Murmur".
    fn related_hits() -> Vec<ContextHit> {
        vec![
            hit("Murmur", "We agreed the Q3 roadmap and the budget runway.", Some("[[Q2 Planning]]")),
            hit("Murmur", "The bed comfort trial went well.", Some("[[Bed Comfort]]")),
        ]
    }

    #[test]
    fn appends_one_collapsed_callout_attributed_per_source() {
        let md = "---\ntitle: Sync\n---\n# Notes\n- decided to ship\n";
        let out = apply_context_markers(md, &jira_slack(), AS_OF);
        // Original body preserved verbatim (prefix unchanged).
        assert!(out.starts_with(md), "prose + front-matter preserved byte-for-byte");
        // One foldable callout, dated, each line loud-attributed to the RIGHT source.
        assert!(out.contains("> [!context]- Live context (as of 2026-07-05T14:32:00Z)"));
        assert!(out.contains("> - PROJ-123 · In Progress · due 2026-07-10 (via Jira) — https://acme.atlassian.net/browse/PROJ-123"));
        assert!(out.contains("> - \"ship it Friday\" in #eng (via Slack) — https://acme.slack.com/archives/C1/p123"));
        assert_eq!(out.matches("[!context]-").count(), 1, "exactly one consolidated block");
        assert_eq!(out.matches(FENCE_START).count(), 1);
    }

    #[test]
    fn is_idempotent_reenrich_replaces_never_stacks() {
        let md = "# Notes\n- a line\n";
        let once = apply_context_markers(md, &jira_slack(), AS_OF);
        let twice = apply_context_markers(&once, &jira_slack(), AS_OF);
        assert_eq!(once, twice, "re-enriching with the same hits+timestamp is a no-op");
        assert_eq!(twice.matches(FENCE_START).count(), 1, "the block never stacks");
    }

    #[test]
    fn empty_hits_strips_the_block_byte_exact_undo() {
        for md in [
            "# Notes\n- a\n- b\n",          // trailing newline
            "# Notes\n- a\n- b",            // no trailing newline
            "---\nk: v\n---\n# T\nbody\n",  // front-matter
            "",                             // empty note
        ] {
            let enriched = apply_context_markers(md, &jira_slack(), AS_OF);
            assert_ne!(enriched, md, "enrichment actually added the block");
            let undone = apply_context_markers(&enriched, &[], AS_OF);
            assert_eq!(undone, md, "empty-hits strip restores the note byte-for-byte: {md:?}");
        }
    }

    #[test]
    fn reenrich_with_new_values_replaces_old_block() {
        let md = "# Notes\n";
        let stale = apply_context_markers(md, &[hit("Jira", "PROJ-1 · In Progress", None)], "2026-07-01T00:00:00Z");
        let fresh = apply_context_markers(&stale, &[hit("Jira", "PROJ-1 · Done", None)], AS_OF);
        assert!(fresh.contains("PROJ-1 · Done (via Jira)"));
        assert!(!fresh.contains("In Progress"), "the stale snapshot is replaced, not stacked");
        assert!(fresh.contains("as of 2026-07-05T14:32:00Z"), "the timestamp is refreshed");
        assert_eq!(fresh.matches(FENCE_START).count(), 1);
    }

    #[test]
    fn a_multiline_detail_cannot_break_the_block_or_inject_markdown() {
        // A hostile/rich connector value with newlines + markdown must collapse to ONE line inside
        // the callout — never escape the fence (which would leave un-strippable residue).
        let nasty = hit("Slack", "line one\n> [!danger] injected\nline two", None);
        let out = apply_context_markers("# N\n", &[nasty], AS_OF);
        assert_eq!(out.matches("[!context]-").count(), 1);
        // The collapse keeps the value on ONE `> - ` body line, so the injected `[!danger]` lands
        // mid-line as inert text — it never becomes a line-START callout Obsidian would render, and
        // never spawns an extra line that would escape the fence.
        assert_eq!(
            out.lines().filter(|l| l.trim_start().starts_with("> [!danger")).count(),
            0,
            "the injected callout never reaches a line-start position"
        );
        assert_eq!(
            out.lines().filter(|l| l.trim_start().starts_with("> [!")).count(),
            1,
            "only our own context callout is a real callout"
        );
        // And it still round-trips: strip restores the original byte-for-byte.
        assert_eq!(apply_context_markers(&out, &[], AS_OF), "# N\n");
    }

    /// A hit that tries to FORGE our managed-block fence (an attacker-influenced web/Slack result
    /// carrying `<!-- /murmur:context -->` in its text/url) must NOT break the strip: the delimiters
    /// are neutralized, so only the real fence pair exists and the block still round-trips byte-exact.
    /// (2026-07-05 lock-security finding — the fence-injection case.)
    #[test]
    fn a_hit_forging_the_fence_cannot_break_strip() {
        let evil = hit(
            "web",
            "legit <!-- /murmur:context --> gotcha <!-- murmur:context -->",
            Some("https://x/<!-- /murmur:context -->"),
        );
        let out = apply_context_markers("# N\n- keep me\n", std::slice::from_ref(&evil), AS_OF);
        // Exactly ONE real fence pair — the value's forged fences were broken by sanitize.
        assert_eq!(out.matches(FENCE_START).count(), 1, "no forged start fence");
        assert_eq!(out.matches(FENCE_END).count(), 1, "no forged end fence");
        // Byte-exact undo still holds despite the hostile value.
        assert_eq!(apply_context_markers(&out, &[], AS_OF), "# N\n- keep me\n");
        // And idempotent (re-enrich with the same evil hit replaces, never stacks).
        let twice = apply_context_markers(&out, std::slice::from_ref(&evil), AS_OF);
        assert_eq!(out, twice);
    }

    /// SEAL-SAFETY (the point of persisting via the DB note markdown, not the vault-only path): an
    /// enriched note round-trips through the folder-lock seal byte-identical. This is what Re-Truth's
    /// `overwrite_note`-only path would FAIL — the enrichment must live inside `notes.markdown` so it
    /// seals into `content_blob` and restores intact. (Crypto round-trip stands in for the DB seal;
    /// `seal_note` encrypts exactly this markdown — see commands.rs `lock_folder_inner`.)
    #[test]
    fn enriched_note_seals_and_restores_byte_identical() {
        let md = "---\ntitle: Board\n---\n# Notes\n- ship Friday\n";
        let enriched = apply_context_markers(md, &jira_slack(), AS_OF);
        let key = crate::crypto::random_key().unwrap();
        let aad = b"murmur:content:v1|folder=f|meeting=m|provider=claude_code|type=note";
        let blob = crate::crypto::encrypt(&key, enriched.as_bytes(), aad).unwrap();
        let restored = crate::crypto::decrypt(&key, &blob, aad).unwrap();
        assert_eq!(
            restored,
            enriched.as_bytes(),
            "the enriched note (context block included) must seal + restore byte-identical"
        );
    }

    // ── Lane A (`apply_link_markers`) — same invariants, distinct fence ───────────────────────────

    /// Idempotent: re-linking with the same hits+timestamp is a no-op and the block never stacks.
    #[test]
    fn apply_link_markers_is_idempotent() {
        let md = "# Notes\n- a line\n";
        let once = apply_link_markers(md, &related_hits());
        let twice = apply_link_markers(&once, &related_hits());
        assert_eq!(once, twice, "re-linking with the same hits is a no-op");
        assert_eq!(twice.matches(LINKS_FENCE_START).count(), 1, "the links block never stacks");
        assert!(once.contains("> [!related]- Related notes"), "the Related-notes callout header");
        assert!(once.contains("[[Q2 Planning]]"), "the [[Title]] wikilink is rendered");
        assert!(once.contains("(via Murmur)"), "loud Murmur attribution");
    }

    /// Empty hits STRIP the links block byte-exact (self-healing / byte-exact undo).
    #[test]
    fn apply_link_markers_empty_strips_byte_exact() {
        for md in [
            "# N\n- a\n- b\n",
            "# N\n- a\n- b",
            "---\nk: v\n---\n# T\nbody\n",
            "",
        ] {
            let linked = apply_link_markers(md, &related_hits());
            assert_ne!(linked, md, "linking actually added the block");
            let undone = apply_link_markers(&linked, &[]);
            assert_eq!(undone, md, "empty-hits strip restores the note byte-for-byte: {md:?}");
        }
    }

    /// A links block + a context block COEXIST as independent self-managed blocks: each strips +
    /// reapplies ONLY its own fence, so neither disturbs the other. This is the Lane A / Lane B
    /// coexistence contract.
    #[test]
    fn links_and_context_blocks_coexist_independently() {
        let md = "# Notes\n- decided to ship\n";
        let with_links = apply_link_markers(md, &related_hits());
        let both = apply_context_markers(&with_links, &jira_slack(), AS_OF);
        // Both managed blocks present, each exactly once, original body preserved.
        assert!(both.starts_with(md), "prose preserved byte-for-byte");
        assert_eq!(both.matches("[!related]-").count(), 1);
        assert_eq!(both.matches("[!context]-").count(), 1);
        assert_eq!(both.matches(LINKS_FENCE_START).count(), 1);
        assert_eq!(both.matches(FENCE_START).count(), 1);
        // Stripping the CONTEXT block leaves the LINKS block byte-exact (== the links-only note).
        let links_only = apply_context_markers(&both, &[], AS_OF);
        assert_eq!(links_only, with_links, "stripping context restores the links-only note byte-exact");
        assert_eq!(links_only.matches("[!related]-").count(), 1);
        // Stripping the LINKS block leaves the CONTEXT block byte-exact (== the context-only note).
        let context_only = apply_link_markers(&both, &[]);
        assert_eq!(
            context_only,
            apply_context_markers(md, &jira_slack(), AS_OF),
            "stripping links restores the context-only note byte-exact"
        );
        assert_eq!(context_only.matches("[!related]-").count(), 0);
        assert_eq!(context_only.matches("[!context]-").count(), 1);
    }

    /// A hostile Lane-A hit forging the links fence in its detail/url must NOT break the strip: the
    /// delimiters are neutralized so only the real fence pair exists and byte-exact undo still holds.
    #[test]
    fn apply_link_markers_forging_the_fence_is_sanitized() {
        let evil = hit(
            "Murmur",
            "legit <!-- /murmur:links --> gotcha <!-- murmur:links -->",
            Some("[[Note <!-- /murmur:links -->]]"),
        );
        let out = apply_link_markers("# N\n- keep me\n", std::slice::from_ref(&evil));
        assert_eq!(out.matches(LINKS_FENCE_START).count(), 1, "no forged start fence");
        assert_eq!(out.matches(LINKS_FENCE_END).count(), 1, "no forged end fence");
        assert_eq!(
            apply_link_markers(&out, &[]),
            "# N\n- keep me\n",
            "byte-exact undo holds despite the hostile value"
        );
        let twice = apply_link_markers(&out, std::slice::from_ref(&evil));
        assert_eq!(out, twice, "idempotent with the hostile hit");
    }
}
