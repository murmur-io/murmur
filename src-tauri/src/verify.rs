//! NOTE VERIFY — deterministic verification of note claims against LIVE connector truth (v1: Jira).
//!
//! Design (docs/research/2026-07-05-connectors-live-vs-rag.md): the LLM is NEVER the judge.
//! `extract_issue_keys` (pure regex-free scanner) finds ticket keys; the caller fetches each
//! issue's CURRENT state live (staleness would INVERT a verification); `judge` (pure) compares;
//! `apply_verify_markers` appends non-destructive `> ` blockquote markers exactly like
//! `summarize/grounding.rs::annotate_unverified` — idempotent (strip old `(via Jira)` markers,
//! re-insert), byte-preserving for every original line.
//!
//! On-demand + consent-gated ONLY (rides the Jira connector gates). NEVER wired into the
//! zero-egress proactive path (`proactive.rs` contract D1).

use serde::{Deserialize, Serialize};

/// The live state of one Jira issue, fetched at verify time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSnapshot {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub due: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Confirmed,
    NotFound,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyFinding {
    /// 1-based line number in the note markdown the claim sits on.
    pub line_no: usize,
    pub key: String,
    pub verdict: Verdict,
    /// Human detail rendered in the marker/panel. Contains ONLY connector-sourced values +
    /// dates already present in the note line — never other note content.
    pub detail: String,
    pub url: String,
}

/// Max unique keys verified per pass (bounds egress + latency).
const MAX_KEYS: usize = 10;

/// A verify marker line we own (and may strip on re-apply).
fn is_verify_marker(line: &str) -> bool {
    let t = line.trim_start();
    (t.starts_with("> ✓") || t.starts_with("> ⚠") || t.starts_with("> ⧗"))
        && t.trim_end().ends_with("(via Jira)")
}

/// Scan for Jira-style issue keys (`ABC-123`): 1 uppercase letter + up to 9 uppercase
/// alphanumerics, a dash, 1–6 digits, on WORD BOUNDARIES. Skips YAML frontmatter and our own
/// marker lines. Returns (1-based line_no, key), first occurrence per unique key, capped.
pub fn extract_issue_keys(note_md: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut in_frontmatter = false;
    for (idx, line) in note_md.lines().enumerate() {
        let line_no = idx + 1;
        if idx == 0 && line.trim() == "---" {
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        if is_verify_marker(line) {
            continue;
        }
        for key in scan_keys(line) {
            if out.len() >= MAX_KEYS {
                return out;
            }
            if seen.insert(key.clone()) {
                out.push((line_no, key));
            }
        }
    }
    out
}

/// Hand-rolled key scanner (no `regex` dependency): uppercase run then '-' then digit run,
/// bounded by non-alphanumerics.
fn scan_keys(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Word boundary: previous char must not be alphanumeric.
        if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        // Project part: uppercase letter then 0..=9 uppercase alphanumerics.
        if !bytes[i].is_ascii_uppercase() {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        while j < bytes.len()
            && j - start < 10
            && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit())
        {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'-' {
            let dash = j;
            let mut k = dash + 1;
            while k < bytes.len() && k - dash <= 6 && bytes[k].is_ascii_digit() {
                k += 1;
            }
            let digits = k - dash - 1;
            let boundary_ok = k >= bytes.len() || !bytes[k].is_ascii_alphanumeric();
            if digits >= 1 && boundary_ok {
                keys.push(line[start..k].to_string());
                i = k;
                continue;
            }
        }
        i = j;
    }
    keys
}

/// Find the first ISO date (`YYYY-MM-DD`) in a line, if any (hand-rolled, no regex).
fn first_iso_date(line: &str) -> Option<String> {
    let b = line.as_bytes();
    for i in 0..b.len().saturating_sub(9) {
        if i > 0 && b[i - 1].is_ascii_digit() {
            continue;
        }
        let w = &b[i..i + 10];
        let shape = w[0].is_ascii_digit()
            && w[1].is_ascii_digit()
            && w[2].is_ascii_digit()
            && w[3].is_ascii_digit()
            && w[4] == b'-'
            && w[5].is_ascii_digit()
            && w[6].is_ascii_digit()
            && w[7] == b'-'
            && w[8].is_ascii_digit()
            && w[9].is_ascii_digit();
        let boundary = i + 10 >= b.len() || !b[i + 10].is_ascii_digit();
        if shape && boundary {
            return Some(line[i..i + 10].to_string());
        }
    }
    None
}

/// PURE deterministic verdict — the load-bearing property (mirrors `facts::reconcile_facts`:
/// the LLM never judges; injected text has no judgment step to hijack).
pub fn judge(line_text: &str, key: &str, snap: Option<&IssueSnapshot>) -> (Verdict, String) {
    match snap {
        None => (Verdict::NotFound, format!("{key} not found in Jira")),
        Some(s) => {
            if let (Some(note_date), Some(due)) = (first_iso_date(line_text), s.due.as_deref()) {
                if note_date != due {
                    return (
                        Verdict::Conflict,
                        format!("note says {note_date}, {key} due {due}"),
                    );
                }
            }
            let mut detail = format!("{key} · Status: {}", s.status);
            if let Some(due) = s.due.as_deref() {
                detail.push_str(&format!(" · due {due}"));
            }
            (Verdict::Confirmed, detail)
        }
    }
}

/// Brain v2 L5 — the HUMAN prefix a verdict renders with in the `> [!verify]-` callout. Kept as a
/// tiny shared helper so [`judge_with_detail`] and [`apply_verify_callout`] can never drift.
fn human_prefix(v: Verdict) -> &'static str {
    match v {
        Verdict::Confirmed => "✓ Confirmed",
        Verdict::NotFound => "⚠ Not found",
        Verdict::Conflict => "⧗ Conflict",
    }
}

/// Brain v2 L5 — [`judge`] extended with a HUMAN detail string (the callout body wording):
/// `✓ Confirmed — PROJ-1 · Status: In Progress · due 2026-07-10` / `⚠ Not found — …` /
/// `⧗ Conflict — note says …, PROJ-1 due …`. PURE and deterministic exactly like `judge` — the LLM
/// is never the judge; the wording carries ONLY connector-sourced values + dates already present in
/// the note line.
pub fn judge_with_detail(
    line_text: &str,
    key: &str,
    snap: Option<&IssueSnapshot>,
) -> (Verdict, String) {
    let (verdict, base) = judge(line_text, key, snap);
    let detail = format!("{} — {base}", human_prefix(verdict));
    (verdict, detail)
}

/// The fence delimiting the managed verify callout (HTML comments render as nothing in Obsidian).
/// A DISTINCT fence from the enrich lanes (`murmur:context` / `murmur:links`) so all three managed
/// blocks are independent: each strips + reapplies ONLY its own fence.
pub(crate) const VERIFY_FENCE_START: &str = "<!-- murmur:verify -->";
pub(crate) const VERIFY_FENCE_END: &str = "<!-- /murmur:verify -->";

/// Brain v2 L5 — append (or idempotently replace) the consolidated, collapsed `> [!verify]-`
/// callout carrying the verification findings, dated `as_of` (caller-supplied so the function is
/// pure). Mirrors `enrich.rs` fence discipline EXACTLY (it reuses the same engine):
/// - **Idempotent** — the old fenced block is stripped first, never stacked;
/// - **Byte-exact undo** — empty `findings` strips the block and returns the note byte-identical;
/// - **Injection-hardened** — every rendered value rides [`crate::enrich::sanitize`], so a
///   connector-sourced detail carrying CR/LF or a forged `<!-- /murmur:verify -->` fence can
///   neither escape the block nor break the strip.
///
/// Callers that EXTRACT keys from a note (or compute line numbers) must strip this callout first
/// (`apply_verify_callout(md, &[], "")`) — the body lines carry issue keys of their own.
pub fn apply_verify_callout(note_md: &str, findings: &[VerifyFinding], as_of: &str) -> String {
    let callout = format!(
        "> [!verify]- Source check (as of {})",
        crate::enrich::sanitize(as_of)
    );
    let body: Vec<String> = findings
        .iter()
        .map(|f| {
            let detail = crate::enrich::sanitize(&f.detail);
            let prefix = human_prefix(f.verdict);
            match f.url.trim() {
                "" => format!("> - {prefix} — {detail} (via Jira)"),
                url => format!(
                    "> - {prefix} — {detail} (via Jira) — {}",
                    crate::enrich::sanitize(url)
                ),
            }
        })
        .collect();
    crate::enrich::apply_fenced_block(note_md, VERIFY_FENCE_START, VERIFY_FENCE_END, &callout, &body)
}

/// Append one non-destructive marker blockquote after each finding's line. IDEMPOTENT: all
/// existing `(via Jira)` marker lines are stripped first, so re-verifying replaces (never stacks).
/// Every ORIGINAL line is preserved byte-identically (the annotate_unverified discipline).
pub fn apply_verify_markers(note_md: &str, findings: &[VerifyFinding]) -> String {
    // 1) Strip our old markers, remembering the original line numbering AFTER the strip.
    let kept: Vec<&str> = note_md.lines().filter(|l| !is_verify_marker(l)).collect();
    // 2) Group findings by line_no (computed against the STRIPPED text — the command recomputes
    //    findings from the stripped note, see verify_note_sources).
    let mut out: Vec<String> = Vec::with_capacity(kept.len() + findings.len());
    for (idx, line) in kept.iter().enumerate() {
        out.push((*line).to_string());
        let line_no = idx + 1;
        for f in findings.iter().filter(|f| f.line_no == line_no) {
            let glyph = match f.verdict {
                Verdict::Confirmed => "✓",
                Verdict::NotFound => "⚠",
                Verdict::Conflict => "⧗",
            };
            // Collapse any CR/LF in the FE-round-tripped detail to a single space (trimmed) so the
            // marker is ALWAYS one line ending in "(via Jira)" — a newline would spawn a second line
            // that `is_verify_marker` can't strip → residue that accumulates on re-apply + injects
            // markdown into the user's own note. Defense at the formatting point covers every caller.
            let detail = f
                .detail
                .replace(['\r', '\n'], " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            out.push(format!("> {glyph} {detail} (via Jira)"));
        }
    }
    let mut s = out.join("\n");
    if note_md.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(key: &str, status: &str, due: Option<&str>) -> IssueSnapshot {
        IssueSnapshot {
            key: key.into(),
            summary: "Fix login".into(),
            status: status.into(),
            due: due.map(String::from),
            url: format!("https://acme.atlassian.net/browse/{key}"),
        }
    }

    #[test]
    fn extracts_unique_keys_with_line_numbers_capped() {
        let md = "---\ntitle: x\n---\n# Notes\n- Ship PROJ-123 by Friday\n- PROJ-123 again\n- also ABC-9\n";
        let keys = extract_issue_keys(md);
        // 1-based line numbers COUNT the frontmatter lines: PROJ-123 sits on line 5, ABC-9 on 7.
        assert_eq!(keys, vec![(5, "PROJ-123".to_string()), (7, "ABC-9".to_string())]);
        // Cap at 10 unique keys.
        let many: String = (1..=15).map(|i| format!("- K{i}A-{i}\n")).collect();
        assert_eq!(extract_issue_keys(&many).len(), 10);
    }

    #[test]
    fn frontmatter_and_existing_markers_are_not_scanned() {
        let md = "---\nref: FM-1\n---\n> ✓ OLD-1 · Status: Done (via Jira)\n- real REAL-2\n";
        let keys = extract_issue_keys(md);
        // FM-1 (frontmatter) and OLD-1 (our own marker line) are skipped; REAL-2 is on line 5.
        assert_eq!(keys, vec![(5, "REAL-2".to_string())]);
    }

    #[test]
    fn judge_not_found_confirmed_and_date_conflict() {
        // Missing issue → NotFound.
        let (v, d) = judge("- Ship PROJ-1 by 2026-07-08", "PROJ-1", None);
        assert!(matches!(v, Verdict::NotFound));
        assert!(d.contains("PROJ-1"));
        // Found, no date in the line → Confirmed with status/due detail.
        let s = snap("PROJ-1", "In Progress", Some("2026-07-10"));
        let (v, d) = judge("- Ship PROJ-1 soon", "PROJ-1", Some(&s));
        assert!(matches!(v, Verdict::Confirmed));
        assert!(d.contains("In Progress") && d.contains("2026-07-10"));
        // ISO date in the line ≠ issue due → Conflict naming both dates.
        let (v, d) = judge("- Ship PROJ-1 by 2026-07-08", "PROJ-1", Some(&s));
        assert!(matches!(v, Verdict::Conflict));
        assert!(d.contains("2026-07-08") && d.contains("2026-07-10"));
        // ISO date matching the due → Confirmed.
        let (v, _) = judge("- Ship PROJ-1 by 2026-07-10", "PROJ-1", Some(&s));
        assert!(matches!(v, Verdict::Confirmed));
    }

    #[test]
    fn markers_are_inserted_after_lines_and_idempotent() {
        let md = "# N\n- Ship PROJ-1 by 2026-07-08\n- other line\n";
        let f = VerifyFinding {
            line_no: 2,
            key: "PROJ-1".into(),
            verdict: Verdict::Conflict,
            detail: "note says 2026-07-08, PROJ-1 due 2026-07-10".into(),
            url: "https://x/browse/PROJ-1".into(),
        };
        let once = apply_verify_markers(md, std::slice::from_ref(&f));
        assert!(once.contains("\n> ⧗ note says 2026-07-08, PROJ-1 due 2026-07-10 (via Jira)\n"));
        // Idempotent: applying again yields byte-identical output (old markers stripped first).
        let twice = apply_verify_markers(&once, &[f]);
        assert_eq!(once, twice);
        // Non-destructive: original lines untouched.
        assert!(twice.contains("- Ship PROJ-1 by 2026-07-08"));
        assert!(twice.contains("- other line"));
    }

    #[test]
    fn empty_findings_strip_stale_markers_only() {
        let md = "# N\n- done PROJ-1\n> ✓ PROJ-1 · Status: Done (via Jira)\n";
        let out = apply_verify_markers(md, &[]);
        assert!(!out.contains("(via Jira)"), "stale markers removed");
        assert!(out.contains("- done PROJ-1"));
    }

    #[test]
    fn multiline_detail_cannot_break_marker_idempotency() {
        let md = "# N\n- Ship PROJ-1\n";
        let f = VerifyFinding {
            line_no: 2,
            key: "PROJ-1".into(),
            verdict: Verdict::Confirmed,
            detail: "PROJ-1 · Status: Done\ninjected line".into(),
            url: String::new(),
        };
        let once = apply_verify_markers(md, std::slice::from_ref(&f));
        // Every marker-originated line is strippable: re-applying with no findings removes ALL of it.
        let cleaned = apply_verify_markers(&once, &[]);
        assert_eq!(cleaned, md, "a multiline detail must not leave residue after strip");
        // And idempotency holds.
        let twice = apply_verify_markers(&once, &[f]);
        assert_eq!(once, twice);
    }

    // ── Brain v2 L5: judge_with_detail + the `> [!verify]-` fenced callout ──────────────────────

    /// The human detail strings render the verdict wording the callout shows: ✓ Confirmed /
    /// ⚠ Not found / ⧗ Conflict, each carrying the connector-sourced status/due detail.
    #[test]
    fn judge_with_detail_formats_human_strings() {
        let s = snap("PROJ-1", "In Progress", Some("2026-07-10"));
        let (v, d) = judge_with_detail("- Ship PROJ-1 soon", "PROJ-1", Some(&s));
        assert!(matches!(v, Verdict::Confirmed));
        assert_eq!(d, "✓ Confirmed — PROJ-1 · Status: In Progress · due 2026-07-10");

        let (v, d) = judge_with_detail("- Ship PROJ-1", "PROJ-1", None);
        assert!(matches!(v, Verdict::NotFound));
        assert_eq!(d, "⚠ Not found — PROJ-1 not found in Jira");

        let (v, d) = judge_with_detail("- Ship PROJ-1 by 2026-07-08", "PROJ-1", Some(&s));
        assert!(matches!(v, Verdict::Conflict));
        assert_eq!(d, "⧗ Conflict — note says 2026-07-08, PROJ-1 due 2026-07-10");
    }

    fn finding(verdict: Verdict, detail: &str, url: &str) -> VerifyFinding {
        VerifyFinding {
            line_no: 2,
            key: "PROJ-1".into(),
            verdict,
            detail: detail.into(),
            url: url.into(),
        }
    }

    /// The callout appends ONE collapsed `> [!verify]-` block (fenced), idempotent — re-applying
    /// with the same findings+timestamp is byte-identical and the block never stacks.
    #[test]
    fn verify_callout_appends_once_and_is_idempotent() {
        let md = "# N\n- Ship PROJ-1 by 2026-07-08\n";
        let fs = vec![finding(
            Verdict::Conflict,
            "note says 2026-07-08, PROJ-1 due 2026-07-10",
            "https://x/browse/PROJ-1",
        )];
        let once = apply_verify_callout(md, &fs, "2026-07-10T12:00:00Z");
        assert!(once.starts_with(md), "original note preserved byte-for-byte");
        assert!(once.contains("> [!verify]- Source check (as of 2026-07-10T12:00:00Z)"));
        assert!(once.contains(
            "> - ⧗ Conflict — note says 2026-07-08, PROJ-1 due 2026-07-10 (via Jira) — https://x/browse/PROJ-1"
        ));
        assert_eq!(once.matches(VERIFY_FENCE_START).count(), 1);
        let twice = apply_verify_callout(&once, &fs, "2026-07-10T12:00:00Z");
        assert_eq!(once, twice, "re-applying replaces, never stacks");
        assert_eq!(twice.matches("[!verify]-").count(), 1);
    }

    /// Empty findings STRIP the callout byte-exact (the undo path), across trailing-newline /
    /// front-matter / empty-note shapes — the enrich.rs invariant, inherited via the shared engine.
    #[test]
    fn verify_callout_empty_findings_strip_byte_exact() {
        let fs = vec![finding(Verdict::Confirmed, "PROJ-1 · Status: Done", "")];
        for md in [
            "# N\n- done PROJ-1\n",
            "# N\n- done PROJ-1",
            "---\nk: v\n---\n# T\nbody\n",
            "",
        ] {
            let with = apply_verify_callout(md, &fs, "2026-07-10T12:00:00Z");
            assert_ne!(with, md, "the callout was actually added");
            let undone = apply_verify_callout(&with, &[], "");
            assert_eq!(undone, md, "empty findings must undo byte-exact: {md:?}");
        }
    }

    /// A hostile finding forging the verify fence (or carrying newlines) can neither escape the
    /// block nor break the strip — the sanitize hardening from enrich.rs applies here too.
    #[test]
    fn verify_callout_survives_fence_forging_and_newlines() {
        let evil = finding(
            Verdict::NotFound,
            "legit <!-- /murmur:verify --> gotcha\n> [!danger] injected",
            "https://x/<!-- /murmur:verify -->",
        );
        let out = apply_verify_callout("# N\n- keep me\n", std::slice::from_ref(&evil), "now");
        assert_eq!(out.matches(VERIFY_FENCE_START).count(), 1, "no forged start fence");
        assert_eq!(out.matches(VERIFY_FENCE_END).count(), 1, "no forged end fence");
        assert_eq!(
            out.lines().filter(|l| l.trim_start().starts_with("> [!danger")).count(),
            0,
            "an injected callout never reaches a line-start position"
        );
        assert_eq!(
            apply_verify_callout(&out, &[], ""),
            "# N\n- keep me\n",
            "byte-exact undo holds despite the hostile value"
        );
        let twice = apply_verify_callout(&out, std::slice::from_ref(&evil), "now");
        assert_eq!(out, twice, "idempotent with the hostile finding");
    }

    /// The verify callout COEXISTS with the inline `> ✓ … (via Jira)` markers and with the enrich
    /// context block: each managed lane strips/reapplies only its own region. Also: stripping the
    /// callout restores the marker-only note byte-exact.
    #[test]
    fn verify_callout_coexists_with_inline_markers_and_context_block() {
        let md = "# N\n- Ship PROJ-1 by 2026-07-08\n";
        let f = finding(Verdict::Conflict, "note says 2026-07-08, PROJ-1 due 2026-07-10", "");
        let marked = apply_verify_markers(md, std::slice::from_ref(&f));
        let both = apply_verify_callout(&marked, std::slice::from_ref(&f), "2026-07-10T12:00:00Z");
        assert!(both.contains("> ⧗ note says"), "inline marker present");
        assert_eq!(both.matches("[!verify]-").count(), 1, "one callout");
        // Add the enrich context block on top — three managed regions coexist.
        let hit = crate::enrich::ContextHit {
            source: "Jira".into(),
            detail: "PROJ-1 · In Progress".into(),
            url: None,
        };
        let all = crate::enrich::apply_context_markers(&both, std::slice::from_ref(&hit), "t0");
        assert_eq!(all.matches("[!verify]-").count(), 1);
        assert_eq!(all.matches("[!context]-").count(), 1);
        // Stripping the verify callout leaves the context block + markers untouched.
        let no_verify = apply_verify_callout(&all, &[], "");
        assert_eq!(no_verify.matches("[!verify]-").count(), 0);
        assert_eq!(no_verify.matches("[!context]-").count(), 1);
        assert!(no_verify.contains("> ⧗ note says"));
        // And stripping the callout from `both` restores the marker-only note byte-exact.
        assert_eq!(apply_verify_callout(&both, &[], ""), marked);
    }

    #[test]
    fn verdict_serde_casing_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&Verdict::NotFound).unwrap(),
            "\"notfound\""
        );
        assert_eq!(
            serde_json::to_string(&Verdict::Confirmed).unwrap(),
            "\"confirmed\""
        );
        assert_eq!(
            serde_json::to_string(&Verdict::Conflict).unwrap(),
            "\"conflict\""
        );
    }
}
