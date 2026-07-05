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
        let once = apply_verify_markers(md, &[f.clone()]);
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
        let once = apply_verify_markers(md, &[f.clone()]);
        // Every marker-originated line is strippable: re-applying with no findings removes ALL of it.
        let cleaned = apply_verify_markers(&once, &[]);
        assert_eq!(cleaned, md, "a multiline detail must not leave residue after strip");
        // And idempotency holds.
        let twice = apply_verify_markers(&once, &[f]);
        assert_eq!(once, twice);
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
