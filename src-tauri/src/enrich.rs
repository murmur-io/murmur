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

/// Brain v3 PR-3 — INVERSE of the links block: parse the `> - {detail} (via {source})[ — {url}]`
/// body lines of the managed `murmur:links` block back into [`ContextHit`]s, so an accepted semantic
/// link can be MERGED into the block without clobbering the auto related-notes rows already there
/// (the block is a full REPLACE — see [`apply_link_markers`]). Only the links fence is read (the
/// connector `murmur:context` block is left untouched). A note with no links block yields `[]`.
///
/// Best-effort parse (the block is our OWN deterministic render, so the shape is stable): a body line
/// that does not match the render shape is skipped rather than mis-parsed. The `callout` header line
/// (`> [!related]-…`) is not a `> - ` bullet, so it is naturally excluded.
pub fn extract_link_hits(note_md: &str) -> Vec<ContextHit> {
    let Some(start) = note_md.rfind(LINKS_FENCE_START) else {
        return Vec::new();
    };
    let Some(end_rel) = note_md[start..].find(LINKS_FENCE_END) else {
        return Vec::new();
    };
    let block = &note_md[start..start + end_rel];
    let mut out: Vec<ContextHit> = Vec::new();
    for line in block.lines() {
        let Some(rest) = line.strip_prefix("> - ") else {
            continue;
        };
        // Split an optional trailing " — {url}" (em-dash separator the renderer used). rsplit once so
        // an em-dash inside the detail itself is not mistaken for the url separator.
        let (head, url) = match rest.rsplit_once(" — ") {
            Some((h, u)) => (h, Some(u.trim().to_string())),
            None => (rest, None),
        };
        // The " (via {source})" suffix carries the source label.
        let (detail, source) = match head.rsplit_once(" (via ") {
            Some((d, s)) => (d.trim().to_string(), s.trim_end_matches(')').trim().to_string()),
            None => (head.trim().to_string(), String::new()),
        };
        if detail.is_empty() {
            continue;
        }
        out.push(ContextHit {
            source,
            detail,
            url,
        });
    }
    out
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

/// The exact header line a REAL machine `murmur:links` block always carries as its first body line
/// (`apply_link_markers` renders `{fence}\n> [!related]- Related notes\n…`, with NO trailing
/// timestamp). Used to distinguish a genuine machine block from a fence a user typed (or pasted) into
/// their own prose.
const RELATED_CALLOUT_HEADER: &str = "> [!related]- Related notes";

/// The STABLE header PREFIX of a machine `murmur:context` block. Unlike `murmur:links`, the connector
/// context callout carries a dated suffix (`… (as of {date})`, see [`apply_context_markers`]), so the
/// gate is a `starts_with` on this timeless prefix — never an exact match on the whole line.
/// `pub(crate)` so the share-egress scrub reuses it (never a hardcoded string).
pub(crate) const CONTEXT_CALLOUT_HEADER_PREFIX: &str = "> [!context]- Live context";

/// How the first-body-line header gate matches: `Exact` (`murmur:links`, no dated suffix) or `Prefix`
/// (`murmur:context` / `murmur:verify`, which append `(as of {date})`).
enum HeaderMatch<'a> {
    Exact(&'a str),
    Prefix(&'a str),
}

/// HEADER-GATED strip of EVERY managed fenced block of one type. Scans ALL `(fence_start, fence_end)`
/// pairs LEFT-TO-RIGHT and removes EACH ONE ONLY when its first non-empty body line matches `header`;
/// a pair whose body is arbitrary user prose (no machine callout header) is copied through
/// BYTE-IDENTICAL and never eaten. Shared by the read-path links strip AND the share-egress
/// context/verify strips so all three use ONE gate discipline.
///
/// Why SCAN-ALL, not `rfind`-the-last (lock-security re-fail 2026-07-20): a user typing a bare
/// `<!-- murmur:context -->x<!-- /murmur:context -->` fence in prose AFTER a REAL machine block made
/// `rfind` anchor on the LAST (forged) pair — the gate failed on its non-header body, the function
/// returned the note UNCHANGED, and the earlier REAL block (Jira key + workspace URL, or a linked
/// title) LEAKED through `clean_note_body` into the share/org envelope. Scanning every pair strips the
/// real one regardless of a trailing decoy. For each STRIPPED pair, the reclaimed leading `\n\n`/`\n`
/// separator matches the write-path append, so a real block round-trips byte-exact via the append/strip
/// inverse; a header-LESS pair (and all surrounding prose/front-matter) is preserved verbatim.
fn strip_managed_block_if_header(
    md: &str,
    fence_start: &str,
    fence_end: &str,
    header: &HeaderMatch<'_>,
) -> String {
    // Fast path: no start fence at all → nothing to consider (avoids an allocation on the common case).
    if !md.contains(fence_start) {
        return md.to_string();
    }
    let mut out = String::with_capacity(md.len());
    // `cursor` = index into `md` of the next unemitted byte. We walk forward pair-by-pair.
    let mut cursor = 0usize;
    while let Some(rel_start) = md[cursor..].find(fence_start) {
        let start = cursor + rel_start;
        // Find this start's matching end fence (the FIRST end after the start — the write-path never
        // nests these HTML-comment fences, so first-after is the correct pairing).
        let Some(rel_end) = md[start + fence_start.len()..].find(fence_end) else {
            // Lone start fence with no following end → NEVER truncate; emit the rest verbatim and stop.
            out.push_str(&md[cursor..]);
            return out;
        };
        let end = start + fence_start.len() + rel_end + fence_end.len();
        let body = &md[start + fence_start.len()..end - fence_end.len()];
        // GATE: the first NON-EMPTY body line must be the machine callout header. A user's forged fence
        // (whose body is arbitrary prose) fails this → the pair is KEPT (copied through).
        let is_machine_block = match body.lines().map(str::trim).find(|l| !l.is_empty()) {
            Some(first_non_empty) => match header {
                HeaderMatch::Exact(h) => first_non_empty == *h,
                HeaderMatch::Prefix(p) => first_non_empty.starts_with(p),
            },
            None => false, // empty body is never a machine block.
        };
        if is_machine_block {
            // STRIP: emit everything up to this pair MINUS the leading `\n\n`/`\n` separator the
            // write-path wrote before the fence (byte-exact inverse of the append), then skip the pair.
            let before = &md[cursor..start];
            let kept = before
                .strip_suffix("\n\n")
                .or_else(|| before.strip_suffix('\n'))
                .unwrap_or(before);
            out.push_str(kept);
            cursor = end;
        } else {
            // KEEP: emit everything up to AND INCLUDING this pair verbatim, then continue after it.
            out.push_str(&md[cursor..end]);
            cursor = end;
        }
    }
    // Emit any trailing bytes after the last fence pair.
    out.push_str(&md[cursor..]);
    out
}

/// READ-PATH strip (retired `murmur:links` block): remove EVERY managed links fence pair whose fenced
/// body is genuinely the MACHINE block, i.e. its first non-empty body line is exactly the
/// [`RELATED_CALLOUT_HEADER`] (`> [!related]- Related notes`) that `apply_link_markers` always emits.
///
/// Why this is NOT `apply_link_markers(md, &[])`: that variant unconditionally `rfind`s the fence
/// pair and cuts everything between — so USER PROSE that happens to contain both `<!-- murmur:links
/// -->` and `<!-- /murmur:links -->` markers (with real text between them) is silently EATEN, and the
/// editor's debounced autosave then PERSISTS that loss to the DB (owned-file data loss). This helper
/// is SURGICAL: a forged/bare fence in prose (no `[!related]` callout header) is left BYTE-IDENTICAL,
/// and it strips a REAL block even when a forged fence trails it (scan-all, not rfind-last).
///
/// SCOPE: this is the READ/DISPLAY strip — `murmur:links` ONLY (the block is fully retired). The
/// `murmur:context` / `murmur:verify` blocks are in-app FEATURES the user opted into and MUST stay
/// visible in the editor; they are stripped ONLY on the share-egress path
/// ([`strip_managed_context_block`] / [`strip_verify_block_for_egress`]), never here.
pub fn strip_managed_links_block(md: &str) -> String {
    strip_managed_block_if_header(
        md,
        LINKS_FENCE_START,
        LINKS_FENCE_END,
        &HeaderMatch::Exact(RELATED_CALLOUT_HEADER),
    )
}

/// SHARE-EGRESS strip of the connector `murmur:context` block (Jira/Slack/web live snippets +
/// workspace URLs). HEADER-GATED on the timeless [`CONTEXT_CALLOUT_HEADER_PREFIX`] (the callout also
/// carries `(as of {date})`), so a user's forged bare `murmur:context` fence in prose is left
/// byte-identical. NOT called on the read/display path — the block stays visible in the editor;
/// this only stops it LEAKING on share. See [`crate::share::envelope::clean_note_body`].
pub fn strip_managed_context_block(md: &str) -> String {
    strip_managed_block_if_header(
        md,
        FENCE_START,
        FENCE_END,
        &HeaderMatch::Prefix(CONTEXT_CALLOUT_HEADER_PREFIX),
    )
}

/// SHARE-EGRESS strip of the `murmur:verify` block (Jira issue keys + verdicts + workspace URLs).
/// A thin re-export delegating to the shared header gate — the fence constants + header prefix live
/// in [`crate::verify`] (their owner), so this never hardcodes the strings. NOT called on the
/// read/display path (the block is an in-app Verify feature the user opted into).
pub fn strip_verify_block_for_egress(md: &str) -> String {
    strip_managed_block_if_header(
        md,
        crate::verify::VERIFY_FENCE_START,
        crate::verify::VERIFY_FENCE_END,
        &HeaderMatch::Prefix(crate::verify::VERIFY_CALLOUT_HEADER_PREFIX),
    )
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

    /// Brain v3 PR-3 — `extract_link_hits` is the INVERSE of the links block: applying hits then
    /// extracting them round-trips the rendered detail/source/url, so an accept can MERGE a new
    /// `[[Title]]` without clobbering the existing related-notes rows.
    #[test]
    fn extract_link_hits_round_trips_the_links_block() {
        let md = "# Notes\n- a line\n";
        let rendered = apply_link_markers(md, &related_hits());
        let extracted = extract_link_hits(&rendered);
        assert_eq!(extracted.len(), 2, "both related rows recovered");
        // The url-bearing hit round-trips detail + source + url.
        let q2 = extracted
            .iter()
            .find(|h| h.detail.contains("Q3 roadmap"))
            .expect("Q2 row present");
        assert_eq!(q2.source, "Murmur");
        assert_eq!(q2.url.as_deref(), Some("[[Q2 Planning]]"));
        // A note WITHOUT a links block yields no hits.
        assert!(extract_link_hits(md).is_empty());
    }

    /// Brain v3 PR-3 — accept materialization MERGES a new `[[Title]]` into the block, PRESERVES the
    /// existing related-notes rows, and is idempotent (a second merge of the same title is a no-op).
    #[test]
    fn merge_new_wikilink_preserves_existing_related_rows() {
        let md = "# Notes\n- a line\n";
        let with_related = apply_link_markers(md, &related_hits());
        // Merge an accepted [[Design Spec]] into the block (mirrors commands::merge_related_hit).
        let mut hits = extract_link_hits(&with_related);
        let new = hit("note", "[[Design Spec]]", None);
        if !hits.iter().any(|h| h.detail == new.detail) {
            hits.push(new.clone());
        }
        let merged = apply_link_markers(&with_related, &hits);
        assert!(merged.contains("[[Q2 Planning]]"), "existing related row preserved");
        assert!(merged.contains("[[Design Spec]]"), "accepted wikilink materialized");
        // Idempotent: re-merging the same accepted title adds nothing.
        let mut hits2 = extract_link_hits(&merged);
        if !hits2.iter().any(|h| h.detail == new.detail) {
            hits2.push(new);
        }
        let merged2 = apply_link_markers(&merged, &hits2);
        assert_eq!(merged, merged2, "re-accepting the same link is a byte-exact no-op");
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

    // ── FIX 1: `strip_managed_links_block` — HEADER-GATED read-path strip ──────────────────────────

    /// A REAL machine block (rendered by `apply_link_markers`, so it carries the exact
    /// `> [!related]- Related notes` header) IS stripped — byte-identical to `apply_link_markers(_, &[])`.
    #[test]
    fn strip_managed_links_block_removes_a_real_machine_block() {
        let md = "---\ntags: [a]\n---\n# Heading\n\nProse.\n";
        let with_block = apply_link_markers(md, &related_hits());
        assert!(with_block.contains(LINKS_FENCE_START), "precondition: real block present");
        let stripped = strip_managed_links_block(&with_block);
        assert_eq!(stripped, md, "a real machine block strips back to the original note byte-exact");
        assert_eq!(
            stripped,
            apply_link_markers(&with_block, &[]),
            "on a REAL block the header-gated strip matches the unconditional strip byte-for-byte"
        );
    }

    /// The load-bearing FIX-1 guarantee: a `murmur:links` fence pair a USER typed in their OWN prose
    /// (real text between the markers, NO `> [!related]- Related notes` header) is left BYTE-IDENTICAL
    /// — the user's text is NEVER eaten. (The old `apply_link_markers(md, &[])` DID eat it.)
    #[test]
    fn strip_managed_links_block_leaves_a_forged_fence_in_prose_untouched() {
        let prose = "Real line A\n<!-- murmur:links -->\nIMPORTANT user text\n<!-- /murmur:links -->\nReal line B\n";
        assert_eq!(
            strip_managed_links_block(prose),
            prose,
            "a forged fence (no [!related] header) is left byte-identical"
        );
        // Contrast: the OLD unconditional strip DID eat the user's text — pin that difference so a
        // regression back to `apply_link_markers(md, &[])` on the read path is caught here.
        assert_ne!(
            apply_link_markers(prose, &[]),
            prose,
            "the unconditional strip WOULD have eaten the forged-fence body (this is what FIX 1 avoids)"
        );
        assert!(
            !apply_link_markers(prose, &[]).contains("IMPORTANT user text"),
            "confirming the unconditional strip ate the user text"
        );
    }

    /// A fenced region whose first non-empty body line is SOME OTHER callout (not `[!related]`) is a
    /// user construct → left untouched (only the genuine Related-notes machine header matches).
    #[test]
    fn strip_managed_links_block_ignores_a_different_callout_header() {
        let prose = "before\n<!-- murmur:links -->\n> [!note]- My own note\n> body\n<!-- /murmur:links -->\nafter\n";
        assert_eq!(strip_managed_links_block(prose), prose, "a non-[!related] callout is not the machine block");
    }

    /// A lone/unterminated fence never truncates real content; a note with no fence is unchanged.
    #[test]
    fn strip_managed_links_block_no_fence_or_lone_fence_is_noop() {
        let none = "# Just prose\n\nno fence at all\n";
        assert_eq!(strip_managed_links_block(none), none);
        let lone = "text\n<!-- murmur:links -->\n> [!related]- Related notes\n(no end fence, EOF)\n";
        assert_eq!(strip_managed_links_block(lone), lone, "a lone start fence is never truncated");
    }

    /// FIX 3 egress helper — `strip_managed_context_block` removes a REAL connector block (the header
    /// carries a DATED suffix, so the gate is a PREFIX match) but leaves a forged bare `murmur:context`
    /// fence in user prose byte-identical. Also proves it round-trips a real `apply_context_markers`.
    #[test]
    fn strip_managed_context_block_gated_on_the_dated_header_prefix() {
        // A REAL block (dated header via apply_context_markers) IS stripped, byte-exact to the original.
        let md = "# N\n\nprose\n";
        let with_ctx = apply_context_markers(md, &jira_slack(), AS_OF);
        assert!(with_ctx.contains("[!context]-"), "precondition: real context block present");
        assert_eq!(
            strip_managed_context_block(&with_ctx),
            md,
            "a real dated context block strips back byte-exact"
        );
        // A forged bare fence in prose (no `> [!context]- Live context` header) is UNTOUCHED.
        let forged = "line A\n<!-- murmur:context -->\nIMPORTANT user text\n<!-- /murmur:context -->\nline B\n";
        assert_eq!(strip_managed_context_block(forged), forged, "forged context fence left byte-identical");
        // The LINKS strip must NOT touch a context block (distinct fence).
        assert_eq!(strip_managed_links_block(&with_ctx), with_ctx, "links strip leaves the context block alone");
    }

    /// FIX 3 egress helper — `strip_verify_block_for_egress` removes a REAL verify block (dated header
    /// → prefix gate) and leaves a forged bare `murmur:verify` fence untouched.
    #[test]
    fn strip_verify_block_for_egress_gated_on_header_prefix() {
        let md = "# N\n\nprose\n";
        let finding = crate::verify::VerifyFinding {
            line_no: 1,
            key: "PROJ-789".into(),
            verdict: crate::verify::Verdict::Confirmed,
            detail: "PROJ-789 matches".into(),
            url: "https://acme.atlassian.net/browse/PROJ-789".into(),
        };
        let with_verify = crate::verify::apply_verify_callout(md, std::slice::from_ref(&finding), AS_OF);
        assert!(with_verify.contains("[!verify]-"), "precondition: real verify block present");
        assert_eq!(
            strip_verify_block_for_egress(&with_verify),
            md,
            "a real dated verify block strips back byte-exact"
        );
        let forged = "line A\n<!-- murmur:verify -->\nIMPORTANT user text\n<!-- /murmur:verify -->\nline B\n";
        assert_eq!(strip_verify_block_for_egress(forged), forged, "forged verify fence left byte-identical");
    }

    // ── SCAN-ALL (lock-security re-fail 2026-07-20): strip a REAL block even behind a trailing forged
    //    fence, while keeping the header-LESS forged fence intact. RED on the old `rfind`-last anchor. ─

    /// LINKS — a real machine block FOLLOWED by a bare forged `murmur:links` fence: the real block's
    /// linked title is STRIPPED (leak closed) and the forged-fence prose SURVIVES. RED on the old
    /// `rfind`-last code (rfind anchored the trailing forged pair → gate failed → real block kept).
    #[test]
    fn strip_managed_links_block_strips_real_block_before_a_trailing_forged_fence() {
        let real = apply_link_markers(
            "# Notes\n\nReal prose.\n",
            &[hit("note", "[[Secret Zwolnienia Q3]]", None)],
        );
        assert!(real.contains("Secret Zwolnienia Q3"), "precondition: real block carries the title");
        // The user then pastes a bare forged fence (no [!related] header) AFTER the real block.
        let md = format!("{real}\n\nmore prose\n<!-- murmur:links -->\nkeepme forged\n<!-- /murmur:links -->\ntail\n");
        let out = strip_managed_links_block(&md);
        assert!(!out.contains("Secret Zwolnienia Q3"), "the REAL block's linked title is stripped: {out}");
        assert!(!out.contains("> [!related]- Related notes"), "the real machine callout is gone: {out}");
        // The forged fence + its content + all prose survive byte-intact.
        assert!(out.contains("keepme forged"), "forged-fence user content survives: {out}");
        assert!(out.contains("<!-- murmur:links -->\nkeepme forged\n<!-- /murmur:links -->"), "forged fence kept verbatim: {out}");
        assert!(out.contains("Real prose.") && out.contains("more prose") && out.contains("tail"), "prose preserved: {out}");
    }

    /// LINKS — multiple REAL blocks of the same type (pathological, reachable via paste): ALL stripped.
    #[test]
    fn strip_managed_links_block_strips_every_real_block() {
        let one = apply_link_markers("# A\n", &[hit("note", "[[Title One]]", None)]);
        let two = apply_link_markers("# B\n", &[hit("note", "[[Title Two]]", None)]);
        let md = format!("{one}\n\nmid prose\n\n{two}");
        let out = strip_managed_links_block(&md);
        assert!(!out.contains("Title One") && !out.contains("Title Two"), "BOTH real blocks stripped: {out}");
        assert_eq!(out.matches(LINKS_FENCE_START).count(), 0, "no links fence survives: {out}");
        assert!(out.contains("mid prose"), "prose between the blocks preserved: {out}");
    }

    /// CONTEXT — a real dated `murmur:context` block before a trailing forged context fence: the Jira
    /// key + workspace URL are STRIPPED (leak closed), the forged content SURVIVES. RED on `rfind`-last.
    #[test]
    fn strip_managed_context_block_strips_real_block_before_a_trailing_forged_fence() {
        let real = apply_context_markers(
            "# Meeting\n\nprose\n",
            &[hit("Jira", "PROJ-999 · In Progress", Some("https://acme.atlassian.net/browse/PROJ-999"))],
            AS_OF,
        );
        assert!(real.contains("PROJ-999"), "precondition: real context block carries the key");
        let md = format!("{real}\n\ntext\n<!-- murmur:context -->\nkeepme\n<!-- /murmur:context -->\n");
        let out = strip_managed_context_block(&md);
        assert!(!out.contains("PROJ-999"), "the Jira key is stripped: {out}");
        assert!(!out.contains("atlassian.net"), "the workspace URL is stripped: {out}");
        assert!(out.contains("keepme") && out.contains("<!-- murmur:context -->\nkeepme\n<!-- /murmur:context -->"), "forged fence kept: {out}");
        assert!(out.contains("prose") && out.contains("text"), "prose preserved: {out}");
    }

    /// VERIFY — a real dated `murmur:verify` block before a trailing forged verify fence: the Jira key
    /// + URL are STRIPPED, the forged content SURVIVES. RED on `rfind`-last.
    #[test]
    fn strip_verify_block_for_egress_strips_real_block_before_a_trailing_forged_fence() {
        let finding = crate::verify::VerifyFinding {
            line_no: 1,
            key: "PROJ-777".into(),
            verdict: crate::verify::Verdict::Confirmed,
            detail: "PROJ-777 matches".into(),
            url: "https://acme.atlassian.net/browse/PROJ-777".into(),
        };
        let real = crate::verify::apply_verify_callout("# N\n\nprose\n", std::slice::from_ref(&finding), AS_OF);
        assert!(real.contains("PROJ-777"), "precondition: real verify block carries the key");
        let md = format!("{real}\n\ntext\n<!-- murmur:verify -->\nkeepme\n<!-- /murmur:verify -->\n");
        let out = strip_verify_block_for_egress(&md);
        assert!(!out.contains("PROJ-777"), "the Jira key is stripped: {out}");
        assert!(!out.contains("atlassian.net"), "the workspace URL is stripped: {out}");
        assert!(out.contains("keepme") && out.contains("<!-- murmur:verify -->\nkeepme\n<!-- /murmur:verify -->"), "forged fence kept: {out}");
        assert!(out.contains("prose") && out.contains("text"), "prose preserved: {out}");
    }
}
