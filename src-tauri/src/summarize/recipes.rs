//! "Recipes": run a builtin or saved prompt template over ONE meeting's transcript via the
//! configured provider's complete() — grounded recap emails, decision logs, work tickets,
//! and per-meeting-type recaps (1:1 / standup / sales / interview). Mirrors chat.rs/timeline.rs.
//!
//! SMART-NOTE ENGINE (2026-07-25): the lower half of this module turns an already-extracted
//! `documents.text` into a readable Obsidian note in TWO selectable recipe shapes — `synthesis`
//! (a flagship anti-slop free-form summary → outline → action items, the whiteboard-photo case)
//! and `structure-mirror` (a deterministic Rust JSON→markdown transpile of a form/table, EVERY
//! value an OPAQUE STRING — §10: no money/arithmetic, an invoice's line items are copied verbatim,
//! never summed). Both the prompt builders and the deterministic renderer are PURE functions of
//! their inputs (no DB, no clock, no egress) so they are unit-testable in isolation; the wiring
//! (provider call + gated note write) lives in `commands/documents.rs`.

use crate::summarize::template::language_directive;

const MAX_TRANSCRIPT_CHARS: usize = 40_000;
/// Char cap on the extracted document text fed to a smart-note recipe prompt — the note-generation
/// twin of [`MAX_TRANSCRIPT_CHARS`] (bounds the local prefill KV / cloud prompt for a huge doc).
const MAX_DOC_CHARS: usize = 40_000;

/// Built-in recipes shown as quick chips: (id, label, instruction prompt).
pub const BUILTIN_RECIPES: &[(&str, &str, &str)] = &[
    (
        "grounded-email",
        "Follow-up email",
        "Write a concise, ready-to-send follow-up email recapping this meeting: one line of \
context, the key decisions, and a per-attendee list of action items with owners and due \
dates where stated. Flag anything uncertain as '(to confirm)' — never invent commitments. \
Clear, professional tone.",
    ),
    (
        "decision-log",
        "Decision log",
        "Extract ONLY the decisions made in this meeting as a clean list. For each: the \
decision, who made/owns it, and the rationale if stated. If none, say 'No decisions recorded.'",
    ),
    (
        "ticket",
        "Work ticket",
        "Turn the most important action item or problem discussed into a ready-to-paste work \
ticket: Title, Description, Acceptance criteria (bullets), Owner if mentioned.",
    ),
    (
        "1on1",
        "1:1 recap",
        "Summarize this 1:1: wins/progress, blockers/concerns, feedback exchanged, and agreed \
next steps with owners. Keep it warm and personal.",
    ),
    (
        "standup",
        "Standup notes",
        "Summarize as standup notes: per-person Done / Doing / Blockers, then a short list of \
team-level follow-ups.",
    ),
    (
        "sales",
        "Sales recap",
        "Summarize as a sales call recap: prospect context, pain points, objections, buying \
signals, next steps + owner, and a deal-risk note.",
    ),
    (
        "interview",
        "Interview notes",
        "Summarize as structured interview notes: candidate strengths, concerns, signal per \
competency discussed, and a hire / no-hire lean with reasoning grounded only in what was said.",
    ),
];

/// Build the (system, user) prompt pair for running `recipe_prompt` over `transcript`.
/// Grounded: the model must stick to the transcript. Output language follows `note_language`.
pub fn build_recipe_prompt(
    transcript: &str,
    recipe_prompt: &str,
    note_language: &str,
) -> (String, String) {
    let t = if transcript.chars().count() > MAX_TRANSCRIPT_CHARS {
        let head: String = transcript.chars().take(MAX_TRANSCRIPT_CHARS).collect();
        format!("{head}\n[transcript truncated]")
    } else {
        transcript.to_string()
    };
    let system = format!(
        "You produce a specific artifact from ONE meeting transcript. Base everything STRICTLY \
on the transcript — never invent facts, names, decisions, or commitments; if something isn't \
in the transcript, omit it or mark it uncertain. Be concise and well-formatted in Markdown.\n\n\
TASK: {recipe_prompt}\n\n{lang}",
        lang = language_directive(note_language)
    );
    let user = format!("TRANSCRIPT:\n{t}");
    (system, user)
}

// ── SMART-NOTE ENGINE — turn an extracted document into a readable Obsidian note ─────────────────

/// The two selectable smart-note recipe shapes.
///
/// * [`Synthesis`](NoteRecipe::Synthesis) — the flagship free-form path for a whiteboard photo /
///   screenshot / slide deck with NO inherent schema: `provider.complete()` over one fixed
///   anti-slop template (summary → outline → action items) → readable markdown.
/// * [`StructureMirror`](NoteRecipe::StructureMirror) — for forms/tables: `provider.complete_json()`
///   into a generic `{fields, tables, sections}` schema of OPAQUE STRINGS, then a DETERMINISTIC
///   Rust JSON→markdown renderer. Never computes or sums anything (§10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteRecipe {
    Synthesis,
    StructureMirror,
}

impl NoteRecipe {
    /// Parse the IPC recipe token; `None` for anything unknown (the command rejects it as
    /// `InvalidArg` — never silently coerces).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "synthesis" => Some(Self::Synthesis),
            "structure" | "structure-mirror" | "structure_mirror" => Some(Self::StructureMirror),
            _ => None,
        }
    }

    /// Stable lowercase token (stamped into the note's front-matter `recipe:` key).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Synthesis => "synthesis",
            Self::StructureMirror => "structure-mirror",
        }
    }
}

/// Cap the extracted text on a char boundary (never splits a multibyte char), appending a marker
/// when truncated so the model knows the doc was clipped. Mirrors `build_recipe_prompt`.
fn cap_doc(text: &str) -> String {
    if text.chars().count() > MAX_DOC_CHARS {
        let head: String = text.chars().take(MAX_DOC_CHARS).collect();
        format!("{head}\n[document truncated]")
    } else {
        text.to_string()
    }
}

/// SYNTHESIS recipe — build the (system, user) prompt pair. The model emits the note BODY ONLY (no
/// YAML front-matter — the command prepends deterministic front-matter): a tight anti-slop
/// `## Summary` → `## Outline` → `## Action items` (owners + dates when stated). Grounded: it must
/// stick to the document, never invent. Output language follows `note_language`.
pub fn build_synthesis_prompt(source_name: &str, text: &str, note_language: &str) -> (String, String) {
    let system = format!(
        "You turn ONE already-extracted document (a whiteboard photo, screenshot, slide deck, or \
pasted notes) into a clean, scannable Obsidian note. Output ONLY the note BODY in Markdown — NO \
YAML front-matter, NO surrounding code fences, NO preamble or commentary before or after.\n\n\
Write EXACTLY these sections (omit a section only if there is genuinely nothing to say):\n\
## Summary\n\
A tight 2–4 sentence overview of what this document is and what it's about.\n\
## Outline\n\
- The key points / structure of the document as specific, factual bullets (nest sub-bullets where \
the source is hierarchical).\n\
## Action items\n\
- [ ] Owner — action (due date if stated)\n\n\
Anti-slop rules — be faithful, be specific, add NOTHING:\n\
- Base everything STRICTLY on the document below. Never invent facts, owners, dates, decisions, or \
action items that are not supported by the text.\n\
- No filler, no hedging, no 'In conclusion', no restating the instructions. Cut empty phrases.\n\
- Copy names, figures, and quoted strings VERBATIM — never re-compute, re-total, or re-interpret a \
number.\n\
- If the document has no action items, write '- None' under Action items rather than inventing one.\n\n\
{lang}",
        lang = language_directive(note_language)
    );
    let user = format!("DOCUMENT: {source_name}\n\nCONTENT:\n{}", cap_doc(text));
    (system, user)
}

/// The generic STRUCTURE-MIRROR schema: `{fields, tables, sections}` where EVERY leaf value is an
/// OPAQUE STRING. Passed to the provider for native constrained decoding (the default
/// `complete_json` impl only stringifies it into the system prompt). No numeric/amount types exist
/// in the schema by design (§10) — an invoice's totals ride through as plain strings.
pub fn structure_mirror_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "fields": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "key":   {"type": "string"},
                        "value": {"type": "string"}
                    },
                    "required": ["key", "value"],
                    "additionalProperties": false
                }
            },
            "tables": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"},
                        "rows":  {"type": "array", "items": {"type": "array", "items": {"type": "string"}}}
                    },
                    "required": ["title", "rows"],
                    "additionalProperties": false
                }
            },
            "sections": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "heading": {"type": "string"},
                        "body":    {"type": "string"}
                    },
                    "required": ["heading", "body"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["fields", "tables", "sections"],
        "additionalProperties": false
    })
}

/// STRUCTURE-MIRROR recipe — build the (system, user) prompt pair for the constrained-JSON
/// transcription. The model TRANSCRIBES the document's structure verbatim into `{fields, tables,
/// sections}`; it computes nothing. §10 is load-bearing here: every value is an opaque string, so
/// an invoice/receipt/form is mirrored, never totalled.
pub fn build_structure_prompt(source_name: &str, text: &str, note_language: &str) -> (String, String) {
    let system = format!(
        "You TRANSCRIBE the structure of ONE already-extracted document (a form, invoice, receipt, \
spreadsheet, or table) into JSON. You are a faithful transcriber, NOT an analyst.\n\
- `fields`: labelled key/value pairs from the document header/body (e.g. \"Invoice #\": \"1042\", \
\"Date\": \"2026-07-01\"). Copy both the label and the value VERBATIM.\n\
- `tables`: each repeating/tabular region as a `title` plus `rows` (the FIRST row is the header). \
Copy every cell verbatim as a string.\n\
- `sections`: any free-text regions (notes, terms) as `heading` + `body`.\n\
CRITICAL RULES:\n\
- EVERY value is an OPAQUE STRING copied exactly as written — keep currency symbols, units, and \
formatting inside the string.\n\
- NEVER compute, sum, total, average, or re-derive ANY number. Do not add a 'Total' the document \
does not itself contain. If the document shows a total, copy that shown string verbatim.\n\
- Never invent a field, row, or value that is not in the document. Omit what isn't there (empty \
arrays are fine).\n\n\
{lang}",
        lang = language_directive(note_language)
    );
    let user = format!("DOCUMENT: {source_name}\n\nCONTENT:\n{}", cap_doc(text));
    (system, user)
}

/// One labelled key/value pair transcribed from a document (opaque strings).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct StructuredField {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub value: String,
}

/// One tabular region: a title + rows of opaque string cells (the first row is the header).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct StructuredTable {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub rows: Vec<Vec<String>>,
}

/// One free-text region: a heading + body (opaque strings).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct StructuredSection {
    #[serde(default)]
    pub heading: String,
    #[serde(default)]
    pub body: String,
}

/// The parsed structure-mirror payload (mirrors [`structure_mirror_schema`]). All values are opaque
/// strings; the renderer never interprets them.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct StructuredDoc {
    #[serde(default)]
    pub fields: Vec<StructuredField>,
    #[serde(default)]
    pub tables: Vec<StructuredTable>,
    #[serde(default)]
    pub sections: Vec<StructuredSection>,
}

/// Collapse a value to a single inline-safe line: newlines → spaces, trimmed. Keeps the markdown
/// tidy without interpreting the (opaque) content.
fn inline(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Make an opaque cell safe inside a markdown table: escape the `|` delimiter and flatten newlines,
/// so a value containing a pipe can never break the table structure. Content is otherwise verbatim.
fn table_cell(s: Option<&String>) -> String {
    match s {
        Some(v) => inline(v).replace('|', "\\|"),
        None => String::new(),
    }
}

/// DETERMINISTIC JSON→markdown BODY renderer for the structure-mirror recipe (no front-matter — the
/// command prepends deterministic front-matter). Pure + deterministic: the SAME [`StructuredDoc`]
/// always renders byte-identical markdown. §10: it copies opaque strings only — it never parses,
/// sums, or re-derives a number. Empty payload → a clear placeholder (never an empty note).
pub fn render_structure_markdown(doc: &StructuredDoc) -> String {
    let mut out = String::new();

    // Fields → a "## Details" definition list (skip a blank key).
    let fields: Vec<&StructuredField> = doc
        .fields
        .iter()
        .filter(|f| !f.key.trim().is_empty())
        .collect();
    if !fields.is_empty() {
        out.push_str("## Details\n\n");
        for f in fields {
            out.push_str(&format!("- **{}**: {}\n", inline(&f.key), inline(&f.value)));
        }
        out.push('\n');
    }

    // Tables → a `## <title>` heading + a valid markdown table (first row = header).
    for t in &doc.tables {
        let rows: Vec<&Vec<String>> = t.rows.iter().filter(|r| !r.is_empty()).collect();
        if rows.is_empty() {
            continue;
        }
        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if cols == 0 {
            continue;
        }
        let title = inline(&t.title);
        if !title.is_empty() {
            out.push_str(&format!("## {title}\n\n"));
        }
        // Header row (first), then the `---` separator, then the body rows. Every cell is an opaque
        // string with `|` escaped so the value can never break the table structure.
        for (r, row) in rows.iter().enumerate() {
            out.push('|');
            for c in 0..cols {
                out.push_str(&format!(" {} |", table_cell(row.get(c))));
            }
            out.push('\n');
            if r == 0 {
                out.push('|');
                for _ in 0..cols {
                    out.push_str(" --- |");
                }
                out.push('\n');
            }
        }
        out.push('\n');
    }

    // Sections → `## <heading>` + verbatim body.
    for s in &doc.sections {
        let heading = inline(&s.heading);
        if !heading.is_empty() {
            out.push_str(&format!("## {heading}\n\n"));
        }
        let body = s.body.trim();
        if !body.is_empty() {
            out.push_str(body);
            out.push_str("\n\n");
        }
    }

    let trimmed = out.trim_end();
    if trimmed.is_empty() {
        "## Details\n\n_No structured content could be transcribed from this document._".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_prompt_with_task_and_transcript() {
        let (s, u) = build_recipe_prompt("Alice: hi", "Write an email.", "auto");
        assert!(s.contains("Write an email."));
        assert!(u.contains("Alice: hi"));
    }

    #[test]
    fn truncates_long_transcript() {
        let long = "x".repeat(MAX_TRANSCRIPT_CHARS + 1000);
        let (_s, u) = build_recipe_prompt(&long, "t", "auto");
        assert!(u.contains("[transcript truncated]"));
    }

    #[test]
    fn has_builtin_recipes() {
        assert!(BUILTIN_RECIPES
            .iter()
            .any(|(id, _, _)| *id == "grounded-email"));
        assert!(BUILTIN_RECIPES.len() >= 5);
    }

    // ── Smart-note engine ────────────────────────────────────────────────────────────────────────

    #[test]
    fn note_recipe_parses_both_shapes_and_rejects_unknown() {
        assert_eq!(NoteRecipe::parse("synthesis"), Some(NoteRecipe::Synthesis));
        assert_eq!(
            NoteRecipe::parse("structure-mirror"),
            Some(NoteRecipe::StructureMirror)
        );
        assert_eq!(
            NoteRecipe::parse("STRUCTURE"),
            Some(NoteRecipe::StructureMirror)
        );
        assert_eq!(NoteRecipe::parse("nonsense"), None);
        assert_eq!(NoteRecipe::Synthesis.as_str(), "synthesis");
        assert_eq!(NoteRecipe::StructureMirror.as_str(), "structure-mirror");
    }

    /// SYNTHESIS: the prompt carries the document + instructs the fixed anti-slop section skeleton,
    /// and forbids the model from emitting its own front-matter (the command prepends deterministic
    /// front-matter). This is the "synthesis produces the section skeleton" leg.
    #[test]
    fn synthesis_prompt_requests_the_section_skeleton_and_grounds_on_the_doc() {
        let (system, user) =
            build_synthesis_prompt("whiteboard.png", "Q3 goals: ship v2. Anna owns QA.", "auto");
        assert!(system.contains("## Summary"));
        assert!(system.contains("## Outline"));
        assert!(system.contains("## Action items"));
        assert!(
            system.contains("NO \nYAML front-matter") || system.contains("NO YAML front-matter"),
            "synthesis must forbid model-emitted front-matter: {system}"
        );
        assert!(system.contains("Never invent"));
        assert!(user.contains("Q3 goals: ship v2. Anna owns QA."));
        assert!(user.contains("whiteboard.png"));
    }

    #[test]
    fn synthesis_prompt_caps_a_huge_document() {
        let huge = "word ".repeat(20_000); // ~100k chars
        let (_s, user) = build_synthesis_prompt("big.txt", &huge, "auto");
        assert!(user.contains("[document truncated]"));
        assert!(user.chars().count() < MAX_DOC_CHARS + 2_000);
    }

    /// STRUCTURE-MIRROR: the schema is `{fields, tables, sections}` with EVERY value a string — no
    /// numeric/amount type exists (§10 — an invoice is transpiled, never summed).
    #[test]
    fn structure_schema_has_only_opaque_string_values() {
        let schema = structure_mirror_schema();
        let s = serde_json::to_string(&schema).unwrap();
        assert!(s.contains("fields") && s.contains("tables") && s.contains("sections"));
        // No number/integer JSON-schema type anywhere — every leaf is a string.
        assert!(
            !s.contains("\"number\"") && !s.contains("\"integer\""),
            "the structure schema must contain NO numeric types (opaque strings only): {s}"
        );
        assert!(build_structure_prompt("invoice.pdf", "Total: $50", "auto")
            .0
            .contains("NEVER compute"));
    }

    /// STRUCTURE-MIRROR renders DETERMINISTICALLY: the same payload → byte-identical markdown, with
    /// a valid table (header + separator), fields, and a section. §10: an invoice's amounts ride
    /// through verbatim as opaque strings; the renderer sums nothing.
    #[test]
    fn structure_renders_deterministically_and_never_computes() {
        let doc = StructuredDoc {
            fields: vec![
                StructuredField {
                    key: "Invoice #".into(),
                    value: "1042".into(),
                },
                StructuredField {
                    key: "  ".into(), // blank key is dropped
                    value: "ignored".into(),
                },
            ],
            tables: vec![StructuredTable {
                title: "Line items".into(),
                rows: vec![
                    vec!["Item".into(), "Amount".into()],
                    vec!["Widget | A".into(), "$30.00".into()],
                    vec!["Gadget".into(), "$20.00".into()],
                ],
            }],
            sections: vec![StructuredSection {
                heading: "Terms".into(),
                body: "Net 30.".into(),
            }],
        };
        let md1 = render_structure_markdown(&doc);
        let md2 = render_structure_markdown(&doc);
        assert_eq!(md1, md2, "structure-mirror must render deterministically");
        assert!(md1.contains("## Details"));
        assert!(md1.contains("- **Invoice #**: 1042"));
        assert!(!md1.contains("ignored"), "blank-key field must be dropped");
        assert!(md1.contains("## Line items"));
        assert!(md1.contains("| Item | Amount |"));
        assert!(md1.contains("| --- | --- |"));
        assert!(
            md1.contains("Widget \\| A"),
            "a pipe in an opaque cell must be escaped so the table stays valid: {md1}"
        );
        // Amounts are copied verbatim — never a computed total row.
        assert!(md1.contains("$30.00") && md1.contains("$20.00"));
        assert!(!md1.contains("$50.00"), "the renderer must NEVER sum amounts");
        assert!(md1.contains("## Terms") && md1.contains("Net 30."));
    }

    #[test]
    fn structure_render_empty_payload_is_a_placeholder_never_blank() {
        let md = render_structure_markdown(&StructuredDoc::default());
        assert!(!md.trim().is_empty());
        assert!(md.contains("## Details"));
    }
}
