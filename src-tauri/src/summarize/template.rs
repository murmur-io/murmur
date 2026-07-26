use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::error::{AppError, Result};
use crate::storage::models::{NoteTemplate, NoteTemplateSection};
use crate::summarize::provider::SummarizeRequest;

/// Scripting tokens that are FORBIDDEN in a user-authored note template. A note template is
/// DECLARATIVE DATA rendered into the summarizer system prompt — never code. These are the
/// hallmark openings of the templating/scripting engines an Obsidian user might paste from
/// (Templater `<%`/`tp.`, Node `require(`/`process.`). We refuse them at SAVE (below), so no such
/// text can ever be persisted, let alone egressed as the system prompt.
pub const FORBIDDEN_TEMPLATE_TOKENS: [&str; 4] = ["<%", "tp.", "require(", "process."];

/// Reject a note template whose ANY text field contains a scripting token, OR misuses the `{{}}`
/// DSL. Called by the `save_note_template` command BEFORE persisting. Declarative data only, ever —
/// this is the security boundary for the template layer (the rendered prompt still passes the
/// `RedactingProvider` firewall on egress, unchanged).
///
/// WHICH FIELDS SUPPORT THE `{{}}` DSL (T4b) — the split is deliberate and enforced HERE:
///   * `sections[].heading` / `sections[].instruction` — YES. They are rendered into the body: the
///     `body_scaffold` substitutes them ([`render_body_scaffold`]) and any placeholder the model
///     echoes back from the instruction is resolved in the finished note
///     ([`assemble_note_with_template`]), so a placeholder in a section can never reach the user
///     unresolved.
///   * `extra_frontmatter_keys` — YES (`client: {{entities}}`). This is the deterministic
///     front-matter binding: Murmur resolves + YAML-escapes it itself.
///   * `name` / `tone` — NO. Neither is ever rendered into a note: `name` is a picker label and
///     `tone` is a directive sent verbatim to the model. A `{{}}` there would silently be literal
///     text forever, so it is REFUSED outright rather than accepted-and-ignored.
///
/// FAIL-CLOSED at SAVE: an unknown / malformed `{{ … }}` is REFUSED here rather than silently
/// dropped at render time, so a typo'd variable can never quietly vanish from a user's notes and an
/// ident outside the closed `match` can never reach the renderer at all.
pub fn validate_note_template(name: &str, tone: &str, t: &NoteTemplate) -> Result<()> {
    // Fields that DO render `{{}}` (validated against the allowlist) …
    let mut dsl_fields: Vec<&str> = Vec::new();
    for s in &t.sections {
        dsl_fields.push(&s.heading);
        dsl_fields.push(&s.instruction);
    }
    for k in &t.extra_frontmatter_keys {
        dsl_fields.push(k);
    }
    // … and fields that never do (any `{{` is refused).
    let plain_fields: [&str; 2] = [name, tone];

    for f in plain_fields.iter().copied().chain(dsl_fields.iter().copied()) {
        for tok in FORBIDDEN_TEMPLATE_TOKENS {
            if f.contains(tok) {
                return Err(AppError::InvalidArg(format!(
                    "note template may not contain the scripting token `{tok}` — templates are \
                     declarative data, not code"
                )));
            }
        }
    }
    for f in plain_fields {
        if f.contains("{{") {
            return Err(AppError::InvalidArg(
                "`{{ … }}` variables are only supported in a template's SECTIONS and its \
                 front-matter keys — the template name and tone are never rendered into a note, \
                 so a variable there would stay literal text"
                    .to_string(),
            ));
        }
    }
    for f in dsl_fields {
        validate_template_vars(f)?;
    }
    Ok(())
}

/// Process-global registry of saved note templates, keyed by id. Populated at boot from the DB
/// (`lib.rs` setup → `Db::list_note_templates`) and refreshed on every save/delete
/// (`commands::settings`). It exists because `build_template` is a PURE renderer called from the
/// pipeline with only the style STRING (`pipeline.rs:2266`) — it has no `AppState`/DB handle — so
/// resolving a saved-template id to its data goes through this cache. Same `OnceLock<RwLock<…>>`
/// shape already used elsewhere in `summarize/` (e.g. `claude_code.rs`, `ner_deberta.rs`). Holds
/// CONTENT-FREE metadata only (a note shape), never meeting content.
fn saved_template_registry() -> &'static RwLock<HashMap<String, NoteTemplate>> {
    static REG: OnceLock<RwLock<HashMap<String, NoteTemplate>>> = OnceLock::new();
    REG.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Replace the saved-template registry with `templates` (id → template). Idempotent; called at boot
/// and after each save/delete so `build_template(<saved id>, …)` renders the live data.
pub fn set_saved_templates(templates: Vec<NoteTemplate>) {
    let mut map = match saved_template_registry().write() {
        Ok(m) => m,
        Err(poison) => poison.into_inner(),
    };
    map.clear();
    for t in templates {
        map.insert(t.id.clone(), t);
    }
}

/// Resolve a saved template by id from the registry (a cheap read-lock clone), or `None` for a
/// built-in / unknown id. THE registry seam callers outside this module use, so a consumer (the
/// pipeline) resolves the row ONCE and then passes it EXPLICITLY to the pure renderers — which is
/// what keeps the renderers unit-testable without mutating process-global state.
pub fn saved_template(id: &str) -> Option<NoteTemplate> {
    lookup_saved_template(id)
}

/// Resolve a saved template by id from the registry (a cheap read-lock clone), or `None`.
fn lookup_saved_template(id: &str) -> Option<NoteTemplate> {
    let map = match saved_template_registry().read() {
        Ok(m) => m,
        Err(poison) => poison.into_inner(),
    };
    map.get(id).cloned()
}

/// The canonical Obsidian note-format prompt (front-matter + sections), shared by all providers.
///
/// This is the *system / instruction* portion sent to every provider. It instructs the
/// model to emit a complete Obsidian-ready Markdown note whose very first line is a YAML
/// front-matter fence (`---`). The `ClaudeCodeProvider` validates exactly this invariant
/// (output must start with `---`) per the design lessons, so the wording here is
/// load-bearing across all three providers.
pub fn default_template() -> String {
    r#"You are a meticulous meeting-notes writer for an Obsidian vault.

Produce a SINGLE, complete Markdown note summarizing the meeting transcript that
follows. Output ONLY the note — no preamble, no explanation, no code fences around
the whole note, no commentary before or after.

The note MUST begin, on the very first line, with a YAML front-matter block delimited
by a line containing exactly three dashes (`---`), then the front-matter keys, then a
closing `---` line. Do not emit anything before the opening `---`.

Front-matter (YAML) keys to include:
- title: a concise human-readable meeting title (string)
- date: the meeting date in ISO format (YYYY-MM-DD)
- duration_minutes: integer minutes (rounded)
- tags: a YAML list including at least [meeting]
- participants: a YAML list (may be empty if unknown)

After the closing `---`, write the note body using these sections (omit a section only
if there is genuinely nothing to say):

# <title>

## Summary
A tight 2–5 sentence overview of what the meeting was about and its outcome.

## Key points
- Bulleted, specific, factual points.

## Decisions
- Decisions that were made (or "None recorded").

## Action items
- [ ] Owner — action (due date if mentioned)

## Notes
Any additional context, open questions, or follow-ups.

Linking rules:
- When the meeting clearly references one of the EXISTING NOTE TITLES provided below,
  link to it using Obsidian wikilink syntax: [[Exact Title]]. Only link titles that
  appear in that list; never invent links.

Formatting rules:
- Use plain Markdown. Use real newlines.
- Be concise and faithful to the transcript; do not fabricate participants, decisions,
  or action items that are not supported by the transcript.
"#
    .to_string()
}

/// Pick the note-format template for a style preset. Unknown styles fall back to standard.
/// Every variant preserves the load-bearing invariant: the note's first line is `---`.
pub fn template_for_style(style: &str) -> String {
    match style {
        "brief" => style_variant(
            "Keep it SHORT — a busy reader skims this in 15 seconds.",
            "# <title>\n\n## TL;DR\nMax 2 sentences capturing the outcome.\n\n## Decisions\n- Only the decisions actually made (or omit).\n\n## Action items\n- [ ] Owner — action (due date if mentioned)\n",
        ),
        "detailed" => style_variant(
            "Be THOROUGH — capture nuance, context, and reasoning for a reader who missed the meeting.",
            "# <title>\n\n## Summary\nA full 4–8 sentence overview of context, discussion, and outcome.\n\n## Discussion\nThe main threads of conversation, with the reasoning and trade-offs raised.\n\n## Key points\n- Specific, factual points.\n\n## Decisions\n- Decisions made, with the rationale.\n\n## Action items\n- [ ] Owner — action (due date if mentioned)\n\n## Risks & open questions\n- Anything unresolved or risky.\n\n## Notes\nAdditional context and follow-ups.\n",
        ),
        "action" => style_variant(
            "Be ACTION-FOCUSED — the reader cares most about what happens next and who owns it.",
            "# <title>\n\n## Summary\n1–2 sentences of context.\n\n## Action items\n- [ ] Owner — action (due date if mentioned)\n\n## Decisions\n- Decisions made (or \"None recorded\").\n\n## Follow-ups\n- Open questions / things to revisit.\n",
        ),
        // "standard" and "" render the canonical default template, byte-identical to the legacy
        // `_ => default_template()` arm. Any OTHER id is a saved user template: resolve it from the
        // registry and render it from its data (same `style_variant` shape). An id that is NOT a
        // known saved template falls back to `default_template()` — byte-identical to the legacy
        // behavior for an unknown/hand-edited style value.
        "standard" | "" => default_template(),
        other => lookup_saved_template(other)
            .map(|t| render_saved_template(&t))
            .unwrap_or_else(default_template),
    }
}

/// Render a saved user template into the style-prompt portion (the `style_variant` shape) from its
/// DATA: `tone` becomes the preamble directive, `sections` become the ordered `## heading` body,
/// and `extra_frontmatter_keys` are appended to the fixed front-matter key list. A template with
/// empty tone / no extra keys renders the same shape a built-in style does.
pub fn render_saved_template(t: &NoteTemplate) -> String {
    style_variant_with_keys(
        t.tone.trim(),
        &render_sections(&t.sections),
        &t.extra_frontmatter_keys,
    )
}

/// Build the `# <title>` + ordered `## {heading}\n{instruction}` body from a template's sections,
/// matching the exact spacing the built-in `body_sections` string literals use: a leading
/// `# <title>\n\n`, sections joined by a blank line, and a single trailing newline. An empty
/// section list degrades to just the title line (a template with no sections is still a valid,
/// front-matter-first note).
fn render_sections(sections: &[NoteTemplateSection]) -> String {
    if sections.is_empty() {
        return "# <title>\n".to_string();
    }
    let joined = sections
        .iter()
        .map(|s| format!("## {}\n{}", s.heading.trim(), s.instruction.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("# <title>\n\n{joined}\n")
}

/// Build a style variant: the shared front-matter contract + a style-specific tone line and
/// body section layout. Mirrors `default_template`'s invariants (first line `---`, no fences).
/// Thin wrapper over [`style_variant_with_keys`] with no extra front-matter keys — the built-in
/// styles call this, so their output stays byte-identical.
fn style_variant(tone: &str, body_sections: &str) -> String {
    style_variant_with_keys(tone, body_sections, &[])
}

/// The general form of [`style_variant`], adding `extra_keys` to the front-matter key list. When
/// `tone` is empty the tone clause is omitted (the preamble ends at the period); a non-empty tone
/// renders as `… whole note. {tone}` exactly as the built-in styles do. When `extra_keys` is empty
/// the front-matter block is byte-identical to the legacy `style_variant` output (the built-in
/// styles rely on this for the byte-identity regression). Each extra key is requested as an
/// optional line so a template can add e.g. `project` / `client` front-matter.
fn style_variant_with_keys(tone: &str, body_sections: &str, extra_keys: &[String]) -> String {
    let tone_clause = if tone.is_empty() {
        String::new()
    } else {
        format!(" {tone}")
    };
    // T4b: a key whose entry carries a `{{}}` value is filled DETERMINISTICALLY by Murmur after the
    // provider call — asking the model for it would be wasted prompt AND a second, guessed source of
    // truth, so it is omitted here. A plain key (no `{{}}`) renders the legacy request line
    // byte-identically.
    let extra = extra_keys
        .iter()
        .filter(|k| !k.contains("{{"))
        .map(|k| format!("- {}: (fill in from the meeting, or omit if unknown)\n", k.trim()))
        .collect::<String>();
    format!(
        r#"You are a meticulous meeting-notes writer for an Obsidian vault.

Produce a SINGLE, complete Markdown note summarizing the meeting transcript that
follows. Output ONLY the note — no preamble, no explanation, no code fences around
the whole note.{tone_clause}

The note MUST begin, on the very first line, with a YAML front-matter block delimited
by a line containing exactly three dashes (`---`), then the front-matter keys, then a
closing `---` line. Do not emit anything before the opening `---`.

Front-matter (YAML) keys to include:
- title: a concise human-readable meeting title (string)
- date: the meeting date in ISO format (YYYY-MM-DD)
- duration_minutes: integer minutes (rounded)
- tags: a YAML list including at least [meeting]
- participants: a YAML list (may be empty if unknown)
{extra}
After the closing `---`, write the note body using these sections (omit a section only
if there is genuinely nothing to say):

{body_sections}
Linking rules:
- When the meeting clearly references one of the EXISTING NOTE TITLES provided below,
  link to it using Obsidian wikilink syntax: [[Exact Title]]. Only link titles that
  appear in that list; never invent links.

Formatting rules:
- Use plain Markdown. Use real newlines.
- Be faithful to the transcript; do not fabricate participants, decisions, or action
  items that are not supported by the transcript.
"#
    )
}

/// Map a `note_language` config code to a human-readable language NAME, single-sourcing the
/// code→name table used by every prompt that pins an output language. `"pl"` → `Some("Polish")`;
/// `"auto"` / `""` (whitespace-trimmed, case-insensitive) → `None` (no pin — match the source);
/// an unknown code passes through as `Some(code)` so a valid but untabled ISO code still names a
/// concrete target rather than silently falling back to "auto".
pub fn language_name(note_language: &str) -> Option<String> {
    let lang = note_language.trim();
    if lang.is_empty() || lang.eq_ignore_ascii_case("auto") {
        return None;
    }
    let name = match lang {
        "en" => "English",
        "pl" => "Polish",
        "de" => "German",
        "es" => "Spanish",
        "fr" => "French",
        "it" => "Italian",
        "pt" => "Portuguese",
        "uk" => "Ukrainian",
        "nl" => "Dutch",
        other => other,
    };
    Some(name.to_string())
}

/// An explicit output-language directive appended to the summary system prompt so the WHOLE
/// note (section headings AND content) comes out in one consistent language. The YAML
/// front-matter KEYS stay English so Obsidian keeps parsing them.
pub fn language_directive(note_language: &str) -> String {
    let target = match language_name(note_language) {
        None => "the SAME language as the meeting transcript below (match the speakers)".to_string(),
        Some(name) => name,
    };
    format!(
        "OUTPUT LANGUAGE: Write the section headings AND the body content in {target}. \
KEEP the YAML front-matter KEYS in English exactly as specified (title, date, \
duration_minutes, tags, participants) — translate only their values and the note body, \
never the keys."
    )
}

/// The full summary system prompt: the style template + the output-language directive, and — when
/// the transcript is SPEAKER-LABELED (`labeled`, i.e. the meeting had ≥2 distinct diarized speakers)
/// — the speaker-attribution directive so the model attributes decisions / key points / action-item
/// OWNERS to who actually said them. `labeled == false` (the default solo-`me` meeting) is
/// byte-identical to the pre-Tier-0 prompt.
pub fn build_template(style: &str, note_language: &str, labeled: bool) -> String {
    let mut t = format!(
        "{}\n\n{}",
        template_for_style(style),
        language_directive(note_language)
    );
    if labeled {
        t.push_str("\n\n");
        t.push_str(speaker_attribution_directive());
    }
    t
}

/// Appended to the system prompt ONLY when the transcript is speaker-labeled (`[start-end] (speaker)
/// text`, the same shape the timeline consumes — see [`crate::summarize::timeline`]). Tells the model
/// to attribute content to the speaker who said it (`me` = the person recording; `others` /
/// `others-0` / `others-1` … = the DISTINCT other participants) so action-item OWNERS and decisions
/// are correctly assigned instead of guessed from speaker-blind text. Mirrors `timeline::SYSTEM`.
pub(crate) fn speaker_attribution_directive() -> &'static str {
    "SPEAKER ATTRIBUTION: the transcript below is diarized — each line is `[start-end] (speaker) \
text` (seconds). The `(speaker)` tag is the source of truth for who is talking: `me` is the person \
recording the meeting; `others`, `others-0`, `others-1`, … are the DISTINCT people on the other \
side of the call. Use it to:\n\
- Attribute every DECISION and KEY POINT to the speaker who made it.\n\
- Assign each action item to its real OWNER — write items as `Owner — action`, where Owner is the \
speaker responsible (map `me` to the recording user; use a participant's real NAME when it is \
clearly stated in the conversation, otherwise keep the tag label).\n\
- List the distinct speakers under the `participants` front-matter (real names when clearly stated, \
else the tag labels).\n\
Never attribute to `me` something another participant said or owns, and never invent a speaker the \
tags do not support."
}

/// Render the full prompt text a provider sends (template + meta + vault titles + transcript).
///
/// Providers that take a single combined prompt (Ollama, and the Claude Code stdin path)
/// use this. Providers with a separate system/user channel (Anthropic) use
/// [`default_template`] (or `req.template`) as the system prompt and
/// [`render_user_content`] as the user message.
/// On-device note prompts are bounded to this many transcript chars to protect the local engine's
/// prefill KV cache from an unbounded 1h transcript (P0.2 / mem-2 — the note-generation twin of the
/// `timeline.rs` guard). Matches the `MAX_TRANSCRIPT_CHARS` cap `chat.rs`/`recipes.rs` already apply.
/// Cloud providers call [`render_user_content`] directly and are NOT capped (they handle the full
/// transcript; the RAM-refuse guard in `reason/sidecar.rs` — plus the child's own self-check — is
/// the true OOM backstop).
const LOCAL_NOTE_MAX_CHARS: usize = 40_000;

pub fn render_prompt(req: &SummarizeRequest) -> String {
    // `render_prompt` serves ONLY the on-device combined-prompt providers (local mistralrs +
    // Ollama); cloud providers (Anthropic, Claude Code) render via `render_user_content` directly.
    // So cap the transcript HERE — it bounds the local prefill KV for a long meeting without
    // touching cloud note quality. `..req.clone()` keeps every other field byte-identical; a
    // within-budget transcript borrows the original req unchanged (no clone, byte-identical output).
    let capped;
    let req = if req.transcript.chars().count() > LOCAL_NOTE_MAX_CHARS {
        let head: String = req.transcript.chars().take(LOCAL_NOTE_MAX_CHARS).collect();
        capped = SummarizeRequest {
            transcript: format!("{head}\n[transcript truncated]"),
            ..req.clone()
        };
        &capped
    } else {
        req
    };
    format!(
        "{instructions}\n\n{user}",
        instructions = req.template,
        user = render_user_content(req),
    )
}

/// The user-facing content: meeting metadata, the existing vault titles (link targets),
/// and the transcript itself. Kept separate so providers with a system/user split can
/// send the template as `system` and this as the `user` message.
pub fn render_user_content(req: &SummarizeRequest) -> String {
    let mut out = String::new();

    out.push_str("MEETING METADATA\n");
    out.push_str(&format!("- date: {}\n", req.meta.date_iso));
    if let Some(hint) = &req.meta.title_hint {
        if !hint.trim().is_empty() {
            out.push_str(&format!("- title_hint: {hint}\n"));
        }
    }
    out.push_str(&format!(
        "- duration_minutes: {}\n",
        (req.meta.duration_s as f64 / 60.0).round() as i64
    ));
    if let Some(lang) = &req.meta.language {
        out.push_str(&format!("- language: {lang}\n"));
    }

    out.push_str("\nEXISTING NOTE TITLES (valid [[wikilink]] targets — link only these):\n");
    if req.vault_titles.is_empty() {
        out.push_str("(none)\n");
    } else {
        for title in &req.vault_titles {
            out.push_str(&format!("- {title}\n"));
        }
    }

    // STAGE 1 (two-stage note redesign — Phase 1): the Stage-1 generation prompt carries ONLY this
    // meeting's transcript + the user's typed notes. Cross-meeting `related_context` is NOT rendered
    // here — the pipeline sets it `None` (see `pipeline::summarize_and_export`), and even if a caller
    // sets it, this renderer never folds it into the prompt, so a related note's `## Action items`
    // can never bleed into the Stage-1 note by construction. The `related_context` field is retained
    // on `SummarizeRequest` for Phase 2 (a deferred additive link/context post-pass on the finished
    // note). See docs/research/2026-07-06-note-and-brain-architecture.md (§3 Stage 1, §8 Phase 1).

    // ENHANCE-MY-NOTES: the user's typed notes become the SKELETON of the note. The block is
    // instruction + verbatim notes; absent/blank ⇒ byte-identical output (mirrors
    // related_context above). Each raw line = one user item (the buffer is \n-joined lines).
    if let Some(notes) = &req.user_notes {
        if !notes.trim().is_empty() {
            out.push_str(
                "\n## The user's own in-meeting notes (SKELETON — build the note around these)\n\
                 The user typed these during the meeting, one item per line, in order. They are \
                 the strongest signal of what mattered. Requirements:\n\
                 - Use them as the outline: cover EVERY item, in the user's order, keeping the \
                 user's wording (fix only obvious typos).\n\
                 - Expand each item with concrete detail from the transcript — decisions, owners, \
                 dates, numbers.\n\
                 - After covering every item, add one section headed exactly `## Also discussed` \
                 for significant transcript topics the notes missed; omit it when nothing \
                 significant remains.\n\
                 - Never invent content that is not grounded in the transcript or these notes.\n\
                 - Never output a section titled `My notes`.\n\
                 - Never repeat a section heading; keep every formatting requirement from the \
                 instructions above (front-matter first, section structure, wikilinks).\n\
                 USER NOTES:\n",
            );
            out.push_str(notes.trim());
            out.push('\n');
        }
    }

    // Brain v2 L4 — LIVE NOTES (auto): the running bullets captured during the recording, as a
    // labeled grounding section BEFORE the transcript. The transcript stays authoritative — the
    // bullets are a light-model digest and must never override it. Absent/blank ⇒ byte-identical
    // output (the same contract as `user_notes` above).
    if let Some(bullets) = &req.live_bullets {
        if !bullets.trim().is_empty() {
            out.push_str(
                "\n## Live notes (auto)\n\
                 Running bullets auto-captured during the meeting by the on-device assistant. \
                 Use them as ADDITIONAL grounding for what mattered; the transcript below is \
                 authoritative — never include a bullet the transcript does not support, and \
                 never output a section titled `Live notes`.\n",
            );
            out.push_str(bullets.trim());
            out.push('\n');
        }
    }

    out.push_str("\nTRANSCRIPT\n");
    out.push_str(&req.transcript);
    out.push('\n');

    out
}

// ── SMART-NOTE ENGINE — deterministic front-matter for a generated document note ─────────────────

/// YAML-quote an opaque string value so a `:` / `"` / `#` / newline in a title or source name can
/// never break the front-matter block. Wraps in double quotes and escapes `\` and `"`; newlines
/// collapse to spaces (a front-matter scalar is single-line).
fn yaml_quote(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let escaped = flat.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Build the deterministic YAML front-matter block for a smart note generated from a document — the
/// first line is exactly `---` (the load-bearing Obsidian invariant the whole app depends on), and
/// the closing `---` is the last line. The `title`/`source` values are opaque strings, YAML-quoted;
/// `date` is an ISO `YYYY-MM-DD`; `recipe` is the [`crate::summarize::recipes::NoteRecipe`] token.
/// Deterministic + pure — no clock, no DB.
pub fn smart_note_front_matter(
    title: &str,
    date_iso: &str,
    source_name: &str,
    recipe: &str,
) -> String {
    format!(
        "---\n\
title: {title}\n\
date: {date}\n\
tags: [note, smart-note]\n\
source: {source}\n\
recipe: {recipe}\n\
---\n",
        title = yaml_quote(title),
        date = date_iso,
        source = yaml_quote(source_name),
        recipe = recipe,
    )
}

// ── T4b — the LOGIC-LESS `{{}}` variable DSL + MURMUR-ASSEMBLED front-matter ─────────────────────
//
// WHY. Until now the note's YAML front-matter was GUESSED BY THE MODEL from the transcript and only
// validated to start with `---` (`claude_code.rs`'s output check). That is both a correctness
// problem (a hallucinated `date:`, a dropped `participants:`) and an INJECTION surface: whatever the
// model echoes lands verbatim in a file Obsidian parses — a transcript-derived participant literally
// named `---\nadmin: true` closes the fence and opens a second document, and `<% tp.system %>` is a
// Templater payload the vault would EXECUTE.
//
// So Murmur assembles the front-matter itself: the LLM produces the BODY, Murmur resolves a small
// FIXED set of deterministic variables, strips the fence/scripting tokens out of every resolved
// value, YAML-escapes it, builds the `---` block and PREPENDS it.
//
// GRAMMAR (deliberately not a language): `{{ident}}` or `{{ident:format}}`, `ident` ∈ a closed Rust
// `match` ([`is_known_template_var`]). No expressions, no conditionals, no loops, no nesting, no
// recursion — [`substitute_vars`] is a SINGLE pass whose replacement text is never re-scanned, and
// every resolved value has `{{` stripped so a value can never become a placeholder.
//
// EGRESS — SCOPE, precisely. Resolved values are computed AFTER the provider call, so they cannot
// ride the generation prompt. On top of that, `pipeline::note_split::assemble_and_split` types
// the pre-assembly output as a `ClassificationInput`, and the two CLASSIFICATION-only consumers
// (`resolve_subfolder` → the thematic folder classifier, and the graph-extraction step) take that
// TYPE — so they structurally cannot be handed the assembled note. That is the enforced guarantee
// and it is exactly two call paths wide; it is NOT a claim that the resolved values never leave the
// device by any route. The assembled note is what gets persisted, sealed and exported, so anything
// that later reads the STORED note (Ask grounding, a re-summarize, a share/publish) sees them —
// which is why every value must come from this meeting's own note plus its own
// `visibility_clause`-gated reads, and nothing wider. `meeting_id` is deliberately NOT a variable —
// no user-facing note value, highest linkability cost.

/// The FIXED allowlist of `{{}}` identifiers, in the order the error message lists them. Kept in
/// lockstep with the closed `match` in [`is_known_template_var`] (pinned by a test).
///
/// `title` / `date` reuse the Obsidian/Templater spellings so a template pasted from a vault keeps
/// working for the shared keys.
///
/// What each resolves FROM is the pipeline's job — see `pipeline::resolve_vars_from_note`. Two are
/// worth knowing here: `participants` is the model's own front-matter list (the only source that
/// heard the room), and `entities` is the `visibility_clause`-gated graph list UNION the note's own
/// `[[wikilink]]` targets, because the graph rows for a meeting are written AFTER its note is
/// assembled and so are empty on a first summarize.
pub const TEMPLATE_VARS: [&str; 8] = [
    "title",
    "date",
    "duration_minutes",
    "participants",
    "action_items",
    "entities",
    "tags",
    "language",
];

/// Is `ident` a known template variable? THE allowlist — a closed `match`, no dynamic namespace.
///
/// NOT included, on purpose: `meeting_id` (a UUID with no user-facing note value and the highest
/// linkability cost of anything we hold — dropped by the 2026-07-26 egress review).
pub fn is_known_template_var(ident: &str) -> bool {
    matches!(
        ident,
        "title"
            | "date"
            | "duration_minutes"
            | "participants"
            | "action_items"
            | "entities"
            | "tags"
            | "language"
    )
}

/// The front-matter keys Murmur OWNS: it renders each one deterministically, so a model-emitted line
/// for the same key is dropped during the merge rather than duplicated.
const OWNED_FRONT_MATTER_KEYS: [&str; 5] = [
    "title",
    "date",
    "duration_minutes",
    "tags",
    "participants",
];

/// Bound on how many items a list variable renders (a runaway entity list must not turn the
/// front-matter block into the note) and on how many model-emitted lines survive the merge.
const MAX_LIST_ITEMS: usize = 24;
const MAX_PRESERVED_FRONT_MATTER_LINES: usize = 32;

/// The DETERMINISTIC values behind the `{{}}` variables for ONE meeting. Resolved by the pipeline
/// from the meeting's own row plus the GATED store (`Db::list_entities_for_meeting_visible`, i.e.
/// the same `visibility_clause` predicate as every other graph read), never by the model.
///
/// Plain data + pure rendering, so the whole escaping/stripping contract is unit-testable without a
/// DB or an `AppState`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedVars {
    pub title: String,
    pub date_iso: String,
    pub duration_minutes: i64,
    pub participants: Vec<String>,
    pub action_items: Vec<String>,
    pub entities: Vec<String>,
    pub tags: Vec<String>,
    pub language: Option<String>,
}

/// Tokens a RESOLVED value may never carry into the note: the front-matter fence (`---`), the
/// Templater opening/closing pair, and `{{`/`}}` (so a value can never be re-read as a placeholder).
/// Stripped ANYWHERE in the value, not just leading — `<% tp.system %>` is a live payload wherever
/// it sits in a line Obsidian renders.
const VALUE_STRIP_TOKENS: [&str; 5] = ["<%", "%>", "{{", "}}", "---"];

/// Make an arbitrary resolved value safe to place in a note:
///   1. strip every fence/scripting token, repeating until STABLE so a removal can never SPLICE a
///      new token into existence (`<<%%>>` → `<%>` → …) — bounded, never recursive;
///   2. collapse ALL whitespace to single spaces (a front-matter scalar is single-line by
///      construction — this is what defuses `---\nadmin: true`, and it tidies the gaps step 1 left);
///   3. strip once more, because collapsing whitespace is the only way step 1's output could change.
fn sanitize_value(raw: &str) -> String {
    fn strip_tokens(mut s: String) -> String {
        for _ in 0..4 {
            let before = s.clone();
            for tok in VALUE_STRIP_TOKENS {
                if s.contains(tok) {
                    s = s.replace(tok, "");
                }
            }
            if s == before {
                break;
            }
        }
        s
    }
    let stripped = strip_tokens(raw.to_string());
    let flat = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    strip_tokens(flat).trim().to_string()
}

/// Would YAML re-TYPE this plain scalar as something other than a string? A reserved boolean/null
/// word (YAML 1.1 accepts `y`/`yes`/`on`/`off` too, and Obsidian's parser follows suit) or anything
/// that parses as a number — a participant literally named `No`, or a purely numeric title like
/// `2026`, must come out of the block as a STRING, not `false` / `2026`.
///
/// `f64::from_str` also accepts `inf` / `infinity` / `nan` (case-insensitively), so the numeric test
/// covers those spellings on its own.
fn is_yaml_typed_plain(v: &str) -> bool {
    let lower = v.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "y" | "n"
            | "yes"
            | "no"
            | "true"
            | "false"
            | "on"
            | "off"
            | "null"
            | "~"
            | ".nan"
            | ".inf"
            | "+.inf"
            | "-.inf"
    ) || v.parse::<f64>().is_ok()
}

/// Render one sanitized value as a YAML scalar: bare when it is unambiguously a plain STRING token
/// (`2026-07-26`, `Q3_planning`), double-quoted+escaped otherwise — so a raw `:` / `"` / `#` in a
/// participant name can never break the block, and a reserved word / number
/// ([`is_yaml_typed_plain`]) can never be re-typed away from the string the user meant.
fn yaml_scalar(raw: &str) -> String {
    let v = sanitize_value(raw);
    let bare = !v.is_empty()
        && !v.starts_with('-')
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
        && !is_yaml_typed_plain(&v);
    if bare {
        v
    } else {
        yaml_quote(&v)
    }
}

/// Render a list variable as a YAML flow list of scalars (`[]` when empty), capped at
/// [`MAX_LIST_ITEMS`]. Every item goes through [`yaml_scalar`], so an injected item is quoted, not
/// executed.
fn yaml_flow_list(items: &[String]) -> String {
    let rendered = items
        .iter()
        .map(|s| sanitize_value(s.as_str()))
        .filter(|s| !s.is_empty())
        .take(MAX_LIST_ITEMS)
        .map(|s| yaml_scalar(&s))
        .collect::<Vec<_>>();
    format!("[{}]", rendered.join(", "))
}

/// Format an ISO date with an Obsidian/moment-style pattern (`YYYY-MM-DD`, `DD.MM.YYYY`, `MMMM D`).
/// Unparseable date or empty format ⇒ the sanitized ISO string, unchanged. Single left-to-right
/// pass, longest token first — emitted text is never re-scanned, so a month NAME containing `M`
/// cannot re-expand.
fn format_date(date_iso: &str, format: Option<&str>) -> String {
    let iso = sanitize_value(date_iso);
    let Some(fmt) = format.map(str::trim).filter(|f| !f.is_empty()) else {
        return iso;
    };
    let Some(date) = iso
        .get(..10)
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
    else {
        return iso;
    };
    let tokens: [(&str, String); 9] = [
        ("YYYY", date.format("%Y").to_string()),
        ("YY", date.format("%y").to_string()),
        ("MMMM", date.format("%B").to_string()),
        ("MMM", date.format("%b").to_string()),
        ("MM", date.format("%m").to_string()),
        ("dddd", date.format("%A").to_string()),
        ("ddd", date.format("%a").to_string()),
        ("DD", date.format("%d").to_string()),
        ("D", date.format("%e").to_string().trim().to_string()),
    ];
    let mut out = String::new();
    let mut at = 0usize;
    while at < fmt.len() {
        let rest = &fmt[at..];
        match tokens.iter().find(|(tok, _)| rest.starts_with(tok)) {
            Some((tok, value)) => {
                out.push_str(value);
                at += tok.len();
            }
            None => {
                let ch = rest.chars().next().unwrap_or('\u{0}');
                out.push(ch);
                at += ch.len_utf8();
            }
        }
    }
    sanitize_value(&out)
}

/// The compiled `{{ident}}` / `{{ident:format}}` grammar, shared by the renderer and the save-time
/// validator so the two can never drift. `None` only if the (constant, test-pinned) pattern failed
/// to compile — callers then fail CLOSED rather than `expect()`-ing.
fn var_regex() -> Option<&'static regex::Regex> {
    static RE: OnceLock<Option<regex::Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\{\{\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?::([^{}\r\n]*))?\}\}").ok()
    })
    .as_ref()
}

/// Resolve one variable to its INLINE (body) text form. Lists comma-join; `date` honors the
/// optional format. Returns `None` for an ident outside the allowlist.
fn resolve_var_inline(vars: &ResolvedVars, ident: &str, format: Option<&str>) -> Option<String> {
    let join = |items: &[String]| {
        items
            .iter()
            .map(|s| sanitize_value(s.as_str()))
            .filter(|s| !s.is_empty())
            .take(MAX_LIST_ITEMS)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let value = match ident {
        "title" => sanitize_value(&vars.title),
        "date" => format_date(&vars.date_iso, format),
        "duration_minutes" => vars.duration_minutes.to_string(),
        "participants" => join(&vars.participants),
        "action_items" => join(&vars.action_items),
        "entities" => join(&vars.entities),
        "tags" => join(&vars.tags),
        "language" => sanitize_value(vars.language.as_deref().unwrap_or("")),
        _ => return None,
    };
    Some(value)
}

/// Resolve one variable to its YAML (front-matter) value form: a flow list for the list vars, a
/// bare/quoted scalar otherwise.
fn resolve_var_yaml(vars: &ResolvedVars, ident: &str, format: Option<&str>) -> Option<String> {
    match ident {
        "participants" => Some(yaml_flow_list(&vars.participants)),
        "action_items" => Some(yaml_flow_list(&vars.action_items)),
        "entities" => Some(yaml_flow_list(&vars.entities)),
        "tags" => Some(yaml_flow_list(&vars.tags)),
        "duration_minutes" => Some(vars.duration_minutes.to_string()),
        _ => resolve_var_inline(vars, ident, format).map(|v| yaml_scalar(&v)),
    }
}

/// SINGLE-PASS `{{}}` substitution over arbitrary text. `replace_all` writes each replacement
/// straight to the output (it is never re-scanned) and every resolved value has `{{`/`<%` stripped,
/// so expansion is non-recursive BY CONSTRUCTION. An ident outside the allowlist is left VERBATIM —
/// visible in the note, never silently dropped (and it cannot get this far: `validate_note_template`
/// refuses it at save).
pub fn substitute_vars(input: &str, vars: &ResolvedVars) -> String {
    let Some(re) = var_regex() else {
        return input.to_string();
    };
    re.replace_all(input, |caps: &regex::Captures<'_>| {
        let whole = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
        let ident = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        let format = caps.get(2).map(|m| m.as_str());
        resolve_var_inline(vars, ident, format).unwrap_or_else(|| whole.to_string())
    })
    .into_owned()
}

/// Fail-closed save-time check: EVERY `{{` in `field` must open a well-formed placeholder whose
/// ident is in the allowlist. Anything else — a typo, a `{{meeting_id}}`, an unterminated `{{` — is
/// `AppError::InvalidArg`. Uses the SAME regex the renderer substitutes with, anchored at each `{{`,
/// so validation and rendering cannot disagree.
fn validate_template_vars(field: &str) -> Result<()> {
    if !field.contains("{{") {
        return Ok(());
    }
    let Some(re) = var_regex() else {
        // The grammar is unavailable ⇒ nothing can be rendered safely ⇒ refuse the `{{` outright.
        return Err(AppError::InvalidArg(
            "note template variables are unavailable — remove the `{{ … }}` placeholders".into(),
        ));
    };
    let mut from = 0usize;
    while let Some(rel) = field[from..].find("{{") {
        let at = from + rel;
        let caps = re
            .captures_at(field, at)
            .filter(|c| c.get(0).map(|m| m.start()) == Some(at));
        let Some(caps) = caps else {
            return Err(AppError::InvalidArg(format!(
                "malformed template variable in `{field}` — the only supported forms are \
                 `{{{{name}}}}` and `{{{{name:format}}}}`, with name one of: {}",
                TEMPLATE_VARS.join(", ")
            )));
        };
        let ident = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        if !is_known_template_var(ident) {
            return Err(AppError::InvalidArg(format!(
                "unknown template variable `{{{{{ident}}}}}` — allowed: {}",
                TEMPLATE_VARS.join(", ")
            )));
        }
        from = caps.get(0).map(|m| m.end()).unwrap_or(at + 2);
    }
    Ok(())
}

/// Read one LIST-valued key out of a raw YAML front-matter block: flow (`participants: [A, B]`) or
/// block (`participants:` + `- A` lines) or a single scalar. Hand-rolled, dep-free, best-effort —
/// it parses the MODEL's guess so [`ResolvedVars`] can re-render it safely; it is never trusted to
/// be well-formed.
pub(crate) fn front_matter_list(yaml: &str, key: &str) -> Vec<String> {
    fn push(out: &mut Vec<String>, raw: &str) {
        let s = raw
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim()
            .to_string();
        if !s.is_empty() {
            out.push(s);
        }
    }
    let mut out = Vec::new();
    let mut lines = yaml.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if !k.trim().eq_ignore_ascii_case(key) {
            continue;
        }
        let v = v.trim();
        if let Some(inner) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            for item in inner.split(',') {
                push(&mut out, item);
            }
        } else if !v.is_empty() {
            push(&mut out, v);
        } else {
            while let Some(next) = lines.peek() {
                let t = next.trim();
                match t.strip_prefix('-') {
                    Some(item) => {
                        push(&mut out, item);
                        lines.next();
                    }
                    None => break,
                }
            }
        }
        break;
    }
    out
}

/// The deterministic front-matter LINES (no fences) Murmur owns, in a stable order: the fixed five,
/// then each of the template's `extra_frontmatter_keys` that carries a `{{}}` value.
fn deterministic_front_matter_lines(t: Option<&NoteTemplate>, vars: &ResolvedVars) -> Vec<String> {
    let mut lines = vec![
        format!("title: {}", yaml_scalar(&vars.title)),
        format!("date: {}", yaml_scalar(&vars.date_iso)),
        format!("duration_minutes: {}", vars.duration_minutes),
        format!("tags: {}", yaml_flow_list(&vars.tags)),
        format!("participants: {}", yaml_flow_list(&vars.participants)),
    ];
    if let Some(t) = t {
        for entry in &t.extra_frontmatter_keys {
            if let Some(line) = render_extra_key_line(entry, vars) {
                lines.push(line);
            }
        }
    }
    lines
}

/// Render ONE `extra_frontmatter_keys` entry into a deterministic front-matter line — but only when
/// it actually carries a `{{}}` value. Supported shapes:
///   * `client: {{participants}}` → the variable's YAML form (flow list / scalar), Murmur-quoted.
///   * `slug: {{date:YYYY}}-review` → substituted, then emitted as ONE opaque scalar (quoted unless
///     the result is already a plain token).
///   * `project` (no `{{}}`) → `None`: no deterministic value exists, so the key stays a REQUEST to
///     the model (the prompt still lists it) and the model's own line survives the merge.
///
/// An unsafe/absent key name, or a key Murmur already owns, yields `None`.
fn render_extra_key_line(entry: &str, vars: &ResolvedVars) -> Option<String> {
    let (key, value_tpl) = entry.split_once(':')?;
    let key = key.trim();
    let lower = key.to_ascii_lowercase();
    if !is_safe_yaml_key(key) || OWNED_FRONT_MATTER_KEYS.contains(&lower.as_str()) {
        return None;
    }
    let value_tpl = value_tpl.trim();
    if !value_tpl.contains("{{") {
        return None;
    }
    // The whole value is exactly one placeholder ⇒ render its native YAML form (a list stays a list).
    if let Some(re) = var_regex() {
        if let Some(caps) = re.captures(value_tpl) {
            let whole = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
            if whole == value_tpl {
                let ident = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
                let format = caps.get(2).map(|m| m.as_str());
                if let Some(v) = resolve_var_yaml(vars, ident, format) {
                    return Some(format!("{key}: {v}"));
                }
                return None;
            }
        }
    }
    // Mixed literal + placeholder(s) ⇒ substitute, then quote the result as ONE opaque scalar.
    let rendered = substitute_vars(value_tpl, vars);
    Some(format!("{key}: {}", yaml_scalar(&rendered)))
}

/// A YAML key we are willing to emit: a conservative identifier, so a model-echoed "key" can never
/// smuggle punctuation (or a fence) into the block.
fn is_safe_yaml_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// RENDER a note template against the resolved variables.
///
/// Returns `(frontmatter_yaml, body_scaffold)`:
///   * `frontmatter_yaml` — the COMPLETE `---`-fenced block Murmur prepends. First line is exactly
///     `---`, last is exactly `---`; every value is sanitized + YAML-escaped, so it is always ONE
///     well-formed single-document block.
///   * `body_scaffold` — the template's section skeleton with `{{}}` resolved. The model normally
///     supplies the body; the scaffold is what a body-less generation degrades to, so a note is
///     never just front-matter.
///
/// `template = None` (a built-in style / unknown id) renders the fixed five keys and a bare title
/// scaffold. Pure: no clock, no DB, no I/O.
pub fn render_template(template: Option<&NoteTemplate>, vars: &ResolvedVars) -> (String, String) {
    let lines = deterministic_front_matter_lines(template, vars);
    let front = format!("---\n{}\n---\n", lines.join("\n"));
    (front, render_body_scaffold(template, vars))
}

fn render_body_scaffold(t: Option<&NoteTemplate>, vars: &ResolvedVars) -> String {
    let sections = match t {
        Some(t) if !t.sections.is_empty() => render_sections(&t.sections),
        _ => "# <title>\n".to_string(),
    };
    let title = sanitize_value(&vars.title);
    let sections = sections.replacen("# <title>", &format!("# {title}"), 1);
    substitute_vars(&sections, vars)
}

/// ASSEMBLE the final note: Murmur's deterministic front-matter block PREPENDED to the model's body.
///
/// * The model's own front-matter is NOT trusted — it is split off and re-merged key-by-key: a key
///   Murmur owns is dropped (Murmur's deterministic value wins), a surviving key keeps its line only
///   if it is a safe `key: scalar` (else the value is re-quoted), and list continuations / bare
///   fences are discarded. This is what preserves stamps like `murmur_enhanced: true` and a
///   template's model-filled extra keys while making a fence-injection structurally impossible.
/// * A body-empty generation degrades to the template's `body_scaffold`.
/// * The BODY is `{{}}`-substituted too. A section instruction may legitimately carry a
///   placeholder (`Attendees: {{participants}}`), and that instruction reaches the model VERBATIM
///   in the prompt — so the model can echo the raw `{{participants}}` into its note. Resolving the
///   body here is what guarantees a placeholder never reaches the user unresolved, on the
///   model-body path as well as on the scaffold path. Substitution is the SAME single pass with the
///   SAME sanitized values (`{{`/`<%`/`---` stripped), so it cannot recurse and cannot inject.
///
/// Pure + idempotent-shaped: re-assembling an already-assembled note re-derives the same block.
pub fn assemble_note_with_template(
    template: Option<&NoteTemplate>,
    vars: &ResolvedVars,
    model_markdown: &str,
) -> String {
    let (model_yaml, model_body) = crate::storage::db::split_front_matter(model_markdown);
    let mut lines = deterministic_front_matter_lines(template, vars);
    let owned: std::collections::HashSet<String> = lines
        .iter()
        .filter_map(|l| l.split_once(':').map(|(k, _)| k.trim().to_ascii_lowercase()))
        .collect();
    lines.extend(preserved_front_matter_lines(&model_yaml, &owned));

    let body = if model_body.trim().is_empty() {
        render_body_scaffold(template, vars)
    } else {
        substitute_vars(&model_body, vars)
    };
    format!(
        "---\n{}\n---\n\n{}",
        lines.join("\n"),
        body.trim_start_matches('\n')
    )
}

/// The model-emitted front-matter lines that SURVIVE the merge, re-quoted where needed. Everything
/// structural (blank lines, comments, a bare `---`) and every key Murmur already rendered is
/// dropped.
///
/// A key with NO inline value is a YAML BLOCK list: its `- item` continuation lines are CONSUMED
/// with it, and — when the key is one Murmur does not own — re-emitted as a sanitized flow list
/// (`project: ["Atlas", "Beta"]`) so a model that answers a template's plain
/// `extra_frontmatter_keys` entry in block style keeps its value instead of losing it silently.
/// A block under a key Murmur DOES own (or an unsafe key) is dropped WITH its items, so the block
/// can never end on a dangling key or an orphaned `- item`.
fn preserved_front_matter_lines(
    yaml: &str,
    owned: &std::collections::HashSet<String>,
) -> Vec<String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if out.len() >= MAX_PRESERVED_FRONT_MATTER_LINES {
            break;
        }
        let t = lines[i].trim();
        i += 1;
        // A bare fence / a list item with no owning key / a comment is structural noise.
        if t.is_empty() || t.starts_with('-') || t.starts_with('#') {
            continue;
        }
        let Some((key, value)) = t.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let keep = is_safe_yaml_key(key) && !owned.contains(&key.to_ascii_lowercase());

        if value.is_empty() {
            // BLOCK form — consume this key's `- item` continuation lines either way.
            let mut items = Vec::new();
            while i < lines.len() {
                let c = lines[i].trim();
                // A bare fence terminates the block; it is not an item.
                if c == "---" {
                    break;
                }
                let Some(item) = c.strip_prefix('-') else { break };
                // Unwrap the model's own quoting so the re-emitted flow list quotes ONCE.
                items.push(
                    item.trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .trim()
                        .to_string(),
                );
                i += 1;
            }
            if keep && !items.is_empty() {
                let flow = yaml_flow_list(&items);
                if flow != "[]" {
                    out.push(format!("{key}: {flow}"));
                }
            }
            continue;
        }
        if !keep {
            continue;
        }
        if is_simple_yaml_value(value) {
            out.push(format!("{key}: {value}"));
        } else {
            let unquoted = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            out.push(format!("{key}: {}", yaml_quote(&sanitize_value(unquoted))));
        }
    }
    out
}

/// Is this model-emitted YAML value safe to keep VERBATIM? A bare scalar or a flow list built from
/// plain tokens — anything with a fence, a scripting opening, a quote, or `{{` gets re-quoted.
fn is_simple_yaml_value(value: &str) -> bool {
    if VALUE_STRIP_TOKENS.iter().any(|tok| value.contains(tok)) {
        return false;
    }
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(value);
    !inner.is_empty()
        && inner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-' | ',' | ' '))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summarize::provider::{MeetingMeta, SummarizeRequest};

    fn req(related: Option<String>) -> SummarizeRequest {
        SummarizeRequest {
            transcript: "We shipped v2 and agreed Anna owns the rollout.".to_string(),
            meta: MeetingMeta {
                date_iso: "2026-06-28".to_string(),
                title_hint: None,
                duration_s: 1800,
                language: Some("en".to_string()),
            },
            template: "TEMPLATE".to_string(),
            vault_titles: vec!["Roadmap".to_string()],
            related_context: related,
            user_notes: None,
            live_bullets: None,
        }
    }

    /// No-regression proof: with `related_context = None` (the default + flag-OFF case) the rendered
    /// user content is byte-identical to the pre-Phase-4 rendering — no Related-prior-notes block,
    /// nothing before TRANSCRIPT changed.
    #[test]
    fn render_user_content_none_is_unchanged() {
        let out = render_user_content(&req(None));
        // The transcript section follows the vault-titles section directly, with no extra block.
        assert!(out.contains("\nTRANSCRIPT\nWe shipped v2"));
        assert!(!out.contains("Related prior notes"));
        // Reconstruct the exact expected string to pin byte-identity.
        let expected = "MEETING METADATA\n- date: 2026-06-28\n- duration_minutes: 30\n- language: en\n\nEXISTING NOTE TITLES (valid [[wikilink]] targets — link only these):\n- Roadmap\n\nTRANSCRIPT\nWe shipped v2 and agreed Anna owns the rollout.\n";
        assert_eq!(out, expected);
    }

    /// mem-2 (P0.2): the ON-DEVICE note prompt (`render_prompt`, used only by local mistralrs +
    /// Ollama) caps the transcript so a 1h meeting cannot blow the local engine's prefill KV, while
    /// the CLOUD path (`render_user_content`) stays uncapped. RED before the cap (the full ~100k-char
    /// transcript rendered verbatim, no marker, unbounded length); GREEN after.
    #[test]
    fn on_device_note_prompt_caps_long_transcript() {
        let mut r = req(None);
        r.transcript = "word ".repeat(20_000); // ~100k chars ≈ a full 1h transcript

        let on_device = render_prompt(&r);
        assert!(
            on_device.contains("[transcript truncated]"),
            "render_prompt must truncate a long transcript for the on-device path",
        );
        assert!(
            on_device.chars().count() < LOCAL_NOTE_MAX_CHARS + 2_000,
            "render_prompt output must be bounded near LOCAL_NOTE_MAX_CHARS; got {}",
            on_device.chars().count(),
        );

        // The cloud path is deliberately NOT capped (it handles the full transcript).
        let cloud = render_user_content(&r);
        assert!(
            !cloud.contains("[transcript truncated]"),
            "render_user_content (cloud) must NOT truncate",
        );
        assert!(
            cloud.chars().count() >= LOCAL_NOTE_MAX_CHARS,
            "the cloud path must carry the full transcript",
        );
    }

    /// A within-budget transcript is byte-identical through `render_prompt` (no clone, no marker) —
    /// the cap is inert for normal meetings.
    #[test]
    fn on_device_note_prompt_short_transcript_unchanged() {
        let r = req(None);
        let p = render_prompt(&r);
        assert!(!p.contains("[transcript truncated]"));
        assert!(p.contains("TRANSCRIPT\nWe shipped v2 and agreed Anna owns the rollout."));
    }

    /// STAGE 1 BLEED REGRESSION (Phase 1): a POISONED `related_context` — another meeting's
    /// `## Action items` — is STRUCTURALLY ABSENT from the Stage-1 generation prompt. The renderer no
    /// longer folds `related_context` into the prompt at all, so cross-meeting tasks can never reach
    /// the weak model's one prompt (the confirmed source of the `## Action items` bleed). RED before
    /// Phase 1 (the old code prepended a "## Related prior notes" block, so `Weronika`/`Alcon` and the
    /// heading WOULD appear); GREEN after. The transcript still renders, and `related_context: None`
    /// (what the pipeline actually sets) trivially satisfies this too.
    #[test]
    fn poisoned_related_note_is_absent_from_stage1_prompt() {
        // A hostile prior note whose action items must NEVER bleed into this meeting's note.
        let poison = "\n\n### [[Other Meeting]] · 2026-04-01 · id:m-other\n\
                      ## Action items\n- [ ] Weronika — weryfikować rampę Alcon\n";
        let out = render_user_content(&req(Some(poison.to_string())));

        // The Stage-1 prompt still carries THIS meeting's transcript.
        assert!(
            out.contains("\nTRANSCRIPT\nWe shipped v2"),
            "the transcript must be present in the Stage-1 prompt; got:\n{out}"
        );
        // …and ZERO cross-meeting content: no related-notes block, no foreign task text.
        assert!(
            !out.contains("Related prior notes"),
            "no '## Related prior notes' block may appear in the Stage-1 prompt; got:\n{out}"
        );
        assert!(
            !out.contains("Weronika") && !out.contains("Alcon"),
            "a poisoned related note's action items must be structurally absent; got:\n{out}"
        );
        // Because the field is now inert, ANY related_context renders identically to `None`.
        assert_eq!(
            out,
            render_user_content(&req(None)),
            "related_context must not affect the rendered Stage-1 prompt"
        );
    }

    /// ENHANCE-MY-NOTES: `user_notes: None` (and blank) render a prompt byte-identical to the
    /// pre-field behavior — the same contract `related_context` established.
    #[test]
    fn user_notes_none_or_blank_renders_without_skeleton_block() {
        let base = render_user_content(&req(None));
        assert!(
            !base.contains("SKELETON"),
            "no skeleton block without notes: {base}"
        );
        let mut blank = req(None);
        blank.user_notes = Some("   \n\t ".to_string());
        assert_eq!(
            render_user_content(&blank),
            base,
            "blank notes must be byte-identical to None"
        );
    }

    /// The skeleton block lands BEFORE the transcript, carries the notes verbatim, and instructs the
    /// `## Also discussed` / no-`My notes` contract. (Stage 1, Phase 1: there is no longer a
    /// related-notes block — the user notes are the only pre-transcript content besides metadata.)
    #[test]
    fn user_notes_block_renders_before_transcript() {
        let mut r = req(None);
        r.user_notes = Some("ship Friday\nAnna owns QA".to_string());
        let s = render_user_content(&r);
        let notes_at = s.find("ship Friday\nAnna owns QA").expect("notes verbatim");
        let transcript_at = s.find("\nTRANSCRIPT\n").expect("transcript section");
        assert!(notes_at < transcript_at, "skeleton before transcript");
        assert!(
            !s.contains("Related prior notes"),
            "no related-notes block in the Stage-1 prompt"
        );
        assert!(
            s.contains("## Also discussed"),
            "instructs the Also discussed section"
        );
        assert!(
            s.contains("Never output a section titled"),
            "forbids a My notes section"
        );
    }

    /// Brain v2 L4: `live_bullets: None` (and blank) render byte-identical to the pre-field
    /// prompt — the section only exists when a recording actually produced bullets.
    #[test]
    fn live_bullets_none_or_blank_renders_without_section() {
        let base = render_user_content(&req(None));
        assert!(
            !base.contains("Live notes (auto)"),
            "no section without bullets"
        );
        let mut blank = req(None);
        blank.live_bullets = Some("  \n ".to_string());
        assert_eq!(
            render_user_content(&blank),
            base,
            "blank bullets must be byte-identical to None"
        );
    }

    /// Brain v2 L4: the "Live notes (auto)" section lands BEFORE the transcript, carries the
    /// bullets verbatim, and labels the transcript as authoritative.
    #[test]
    fn live_bullets_section_renders_before_transcript() {
        let mut r = req(None);
        r.live_bullets = Some("- [deal]: pricing agreed\n- [QA]: Anna owns testing".to_string());
        let s = render_user_content(&r);
        let bullets_at = s
            .find("- [deal]: pricing agreed\n- [QA]: Anna owns testing")
            .expect("bullets verbatim");
        let transcript_at = s.find("\nTRANSCRIPT\n").expect("transcript section");
        assert!(bullets_at < transcript_at, "live notes before transcript");
        assert!(s.contains("## Live notes (auto)"), "labeled section");
        assert!(
            s.contains("the transcript below is authoritative"),
            "transcript stays authoritative: {s}"
        );
    }

    /// SMART-NOTE front-matter is `---`-first, `---`-closed, YAML-safe (a `:` in the title/source
    /// can't break parsing), and stamps the deterministic keys. The load-bearing invariant: the very
    /// first line is exactly `---`.
    #[test]
    fn smart_note_front_matter_is_dashes_first_and_yaml_safe() {
        let fm = smart_note_front_matter(
            "Q3: Planning \"board\"",
            "2026-07-25",
            "whiteboard.png",
            "synthesis",
        );
        assert!(fm.starts_with("---\n"), "front-matter must start with ---: {fm}");
        assert!(fm.trim_end().ends_with("---"), "front-matter must close with ---: {fm}");
        // The colon + quotes in the title are quoted+escaped, never bare.
        assert!(fm.contains("title: \"Q3: Planning \\\"board\\\"\""), "{fm}");
        assert!(fm.contains("date: 2026-07-25"));
        assert!(fm.contains("tags: [note, smart-note]"));
        assert!(fm.contains("source: \"whiteboard.png\""));
        assert!(fm.contains("recipe: synthesis"));
    }

    /// TIER 0: the speaker-attribution directive is appended ONLY when `labeled`, and the
    /// `labeled == false` prompt is byte-identical to the legacy `template + language` prompt.
    /// RED on the old 2-arg `build_template` (no `labeled` param, never appends the directive).
    #[test]
    fn build_template_adds_attribution_only_when_labeled() {
        let base = format!(
            "{}\n\n{}",
            template_for_style("standard"),
            language_directive("auto")
        );
        // Unlabeled (the default solo-`me` meeting): byte-identical to the legacy prompt.
        let unlabeled = build_template("standard", "auto", false);
        assert_eq!(
            unlabeled, base,
            "unlabeled must be byte-identical to the pre-Tier-0 prompt"
        );
        assert!(!unlabeled.contains("SPEAKER ATTRIBUTION"));
        // Labeled: the unlabeled prompt PLUS the attribution directive instructing owner/speaker.
        let labeled = build_template("standard", "auto", true);
        assert!(
            labeled.starts_with(&base),
            "labeled prompt extends the base prompt"
        );
        assert!(labeled.contains("SPEAKER ATTRIBUTION"));
        assert!(
            labeled.contains("OWNER"),
            "instructs action-item OWNER attribution"
        );
        assert!(labeled.contains("(speaker)") && labeled.contains("others"));
    }

    fn tpl(id: &str, tone: &str, sections: &[(&str, &str)], extra: &[&str]) -> NoteTemplate {
        NoteTemplate {
            id: id.to_string(),
            name: format!("{id}-name"),
            tone: tone.to_string(),
            sections: sections
                .iter()
                .map(|(h, i)| NoteTemplateSection {
                    heading: h.to_string(),
                    instruction: i.to_string(),
                })
                .collect(),
            extra_frontmatter_keys: extra.iter().map(|k| k.to_string()).collect(),
            created_at: "2026-07-25T00:00:00Z".to_string(),
        }
    }

    /// (i) BYTE-IDENTITY REGRESSION: after the data-driven refactor, the built-in `brief` style must
    /// render EXACTLY the same prompt as before (the full literal is pinned so any spacing/keyword
    /// drift from the `style_variant` → `style_variant_with_keys` change fails here). RED if the
    /// refactor leaks an extra-key line, drops the tone clause, or shifts the front-matter spacing.
    #[test]
    fn builtin_brief_style_is_byte_identical() {
        let expected = r#"You are a meticulous meeting-notes writer for an Obsidian vault.

Produce a SINGLE, complete Markdown note summarizing the meeting transcript that
follows. Output ONLY the note — no preamble, no explanation, no code fences around
the whole note. Keep it SHORT — a busy reader skims this in 15 seconds.

The note MUST begin, on the very first line, with a YAML front-matter block delimited
by a line containing exactly three dashes (`---`), then the front-matter keys, then a
closing `---` line. Do not emit anything before the opening `---`.

Front-matter (YAML) keys to include:
- title: a concise human-readable meeting title (string)
- date: the meeting date in ISO format (YYYY-MM-DD)
- duration_minutes: integer minutes (rounded)
- tags: a YAML list including at least [meeting]
- participants: a YAML list (may be empty if unknown)

After the closing `---`, write the note body using these sections (omit a section only
if there is genuinely nothing to say):

# <title>

## TL;DR
Max 2 sentences capturing the outcome.

## Decisions
- Only the decisions actually made (or omit).

## Action items
- [ ] Owner — action (due date if mentioned)

Linking rules:
- When the meeting clearly references one of the EXISTING NOTE TITLES provided below,
  link to it using Obsidian wikilink syntax: [[Exact Title]]. Only link titles that
  appear in that list; never invent links.

Formatting rules:
- Use plain Markdown. Use real newlines.
- Be faithful to the transcript; do not fabricate participants, decisions, or action
  items that are not supported by the transcript.
"#;
        assert_eq!(template_for_style("brief"), expected);
    }

    /// (i, continued) The other three built-in ids preserve their exact touched-seam bytes: no
    /// leaked extra-key line, the front-matter/`After the closing` spacing unchanged, and the tone
    /// clause intact. `standard`/`""`/unknown all render the canonical default template unchanged.
    #[test]
    fn builtin_styles_have_no_extra_key_leak_and_default_fallback() {
        for style in ["brief", "detailed", "action"] {
            let out = template_for_style(style);
            assert!(
                !out.contains("(fill in from the meeting"),
                "{style}: a built-in style must not render an extra-key line"
            );
            assert!(
                out.contains("may be empty if unknown)\n\nAfter the closing"),
                "{style}: front-matter → body spacing must be byte-identical"
            );
            assert!(
                out.contains("the whole note. "),
                "{style}: the tone clause must render as `whole note. <tone>`"
            );
        }
        // standard, "" and any unknown/unregistered id all resolve to the canonical default.
        assert_eq!(template_for_style("standard"), default_template());
        assert_eq!(template_for_style(""), default_template());
        assert_eq!(
            template_for_style("no-such-template-id-xyz"),
            default_template()
        );
    }

    /// (ii) A saved template renders the expected prompt SHAPE from its DATA: the tone clause, its
    /// ordered `## heading` sections (in order, with their instructions), and each extra
    /// front-matter key as an optional line — all inside the same front-matter-first contract. And
    /// once registered, `build_template(<saved id>, …)` resolves it through the registry (the exact
    /// path the pipeline uses) and wraps it with the language directive.
    #[test]
    fn saved_template_renders_expected_prompt_from_data() {
        let t = tpl(
            "tpl-client-call",
            "Write it for the CLIENT — warm, outcome-first.",
            &[
                ("Outcome", "One line: what we agreed."),
                ("Next steps", "- [ ] Owner — action"),
            ],
            &["client", "project"],
        );

        let body = render_saved_template(&t);
        // Front-matter-first invariant is unchanged (the style portion starts with the shared
        // preamble; the whole rendered prompt still instructs a `---`-first note).
        assert!(body.contains("no code fences around\nthe whole note. Write it for the CLIENT"));
        assert!(body.contains("Do not emit anything before the opening `---`."));
        // Extra front-matter keys are requested, after the fixed 5, before the blank line + body.
        assert!(body.contains(
            "- participants: a YAML list (may be empty if unknown)\n\
             - client: (fill in from the meeting, or omit if unknown)\n\
             - project: (fill in from the meeting, or omit if unknown)\n\n\
             After the closing"
        ));
        // Ordered sections, in order, with instructions.
        let out_at = body.find("## Outcome\nOne line: what we agreed.").expect("s1");
        let next_at = body
            .find("## Next steps\n- [ ] Owner — action")
            .expect("s2");
        assert!(out_at < next_at, "sections render in author order");
        assert!(body.contains("# <title>\n\n## Outcome"), "title heads the body");

        // Registered → build_template resolves it (pipeline's exact call) + appends the language
        // directive; unlabeled adds no speaker directive.
        set_saved_templates(vec![t.clone()]);
        let built = build_template("tpl-client-call", "auto", false);
        assert!(built.starts_with(&body), "build_template renders the saved body");
        assert!(built.contains("OUTPUT LANGUAGE:"), "language directive appended");
        assert!(!built.contains("SPEAKER ATTRIBUTION"), "no attribution when unlabeled");
        // Cleanup so the process-global registry doesn't leak into other tests.
        set_saved_templates(vec![]);
    }

    /// (iii) SECURITY: a template whose ANY field carries a scripting token (`<%`, `tp.`,
    /// `require(`, `process.`) is REJECTED with `AppError::InvalidArg` at save — declarative data
    /// only, never code. RED if a token slips through into a persisted (and thus egress-able)
    /// template.
    #[test]
    fn scripting_tokens_are_rejected() {
        // A clean template validates.
        let ok = tpl("t1", "warm", &[("Summary", "One line.")], &["client"]);
        assert!(validate_note_template(&ok.name, &ok.tone, &ok).is_ok());

        // Each forbidden token, in a DIFFERENT field, must be refused.
        let name_bad = tpl("t2", "warm", &[("Summary", "<% tp.file.title %>")], &[]);
        let tone_bad = tpl("t3", "process.env.SECRET", &[("S", "ok")], &[]);
        let heading_bad = tpl("t4", "warm", &[("require(fs)", "ok")], &[]);
        let key_bad = tpl("t5", "warm", &[("S", "ok")], &["tp.frontmatter"]);
        for t in [&name_bad, &tone_bad, &heading_bad, &key_bad] {
            let err = validate_note_template(&t.name, &t.tone, t);
            assert!(
                matches!(err, Err(AppError::InvalidArg(_))),
                "scripting token must be rejected as InvalidArg; got {err:?}"
            );
        }
    }

    // ── T4b — the `{{}}` DSL + Murmur-assembled front-matter ─────────────────────────────────────

    /// A `ResolvedVars` carrying the three HOSTILE transcript-derived values the whole feature
    /// exists for, plus a benign one.
    fn hostile_vars() -> ResolvedVars {
        ResolvedVars {
            title: "Q3 planning".to_string(),
            date_iso: "2026-07-26".to_string(),
            duration_minutes: 42,
            participants: vec![
                // (a) closes the fence and opens a SECOND YAML document.
                "---\nadmin: true".to_string(),
                // (b) an Obsidian Templater payload the vault would EXECUTE on open.
                "<% tp.system.prompt() %>".to_string(),
                // (c) a raw `:` — breaks an unquoted YAML scalar.
                "Ann: the CFO".to_string(),
                "Bob".to_string(),
            ],
            action_items: vec!["Bob — ship it".to_string()],
            entities: vec!["ACME Corp".to_string()],
            tags: vec!["meeting".to_string()],
            language: Some("en".to_string()),
        }
    }

    /// Count the `---` FENCE lines in a note (a well-formed single-document front-matter block has
    /// exactly two, and they are the first line and the block terminator).
    fn fence_lines(s: &str) -> usize {
        s.lines().filter(|l| l.trim() == "---").count()
    }

    /// RED-before-GREEN (the escaping/stripping core). A transcript-derived participant named
    /// `---\nadmin: true`, one containing `<% tp.system %>`, and one with a raw `:` must ALL come out
    /// of the assembler as inert, quoted, single-line YAML scalars:
    ///   * the assembled note starts with `---` and contains EXACTLY ONE front-matter block,
    ///   * no injected fence, no `<%`/`%>`, no `{{`,
    ///   * `admin: true` never becomes a front-matter KEY of its own.
    ///
    /// Without the sanitize+quote step every one of these assertions fails (the fence closes early,
    /// the Templater token survives, and the bare `:` splits the scalar).
    #[test]
    fn resolved_values_cannot_inject_a_fence_or_a_scripting_token() {
        let vars = hostile_vars();
        let (front, _scaffold) = render_template(None, &vars);

        assert!(front.starts_with("---\n"), "front-matter must open with ---: {front}");
        assert!(front.trim_end().ends_with("---"), "…and close with ---: {front}");
        assert_eq!(fence_lines(&front), 2, "exactly ONE yaml document: {front}");
        assert!(!front.contains("<%") && !front.contains("%>"), "{front}");
        assert!(!front.contains("{{"), "{front}");
        // The fence text is gone from the VALUE, and `admin: true` never becomes its own line.
        assert!(
            !front.lines().any(|l| l.trim_start().starts_with("admin:")),
            "an injected key must never reach the block: {front}"
        );
        // Every hostile participant survives as CONTENT — quoted, escaped, single-line.
        let participants = front
            .lines()
            .find(|l| l.starts_with("participants:"))
            .expect("participants line");
        assert!(participants.contains("\"admin: true\""), "{participants}");
        assert!(participants.contains("\"tp.system.prompt()\""), "{participants}");
        assert!(participants.contains("\"Ann: the CFO\""), "{participants}");
        assert!(participants.contains("Bob"), "{participants}");
        assert!(!participants.contains('\n'), "single-line: {participants}");

        // The same holds for the FULL assembled note (front-matter + the model's body).
        let note = assemble_note_with_template(None, &vars, "---\ntitle: guessed\n---\n\n# Body\n");
        assert!(note.starts_with("---\n"), "{note}");
        assert_eq!(fence_lines(&note), 2, "assembled note is one yaml doc: {note}");
        assert!(!note.contains("<%") && !note.contains("%>"), "{note}");
        assert!(note.contains("# Body"), "the model body survives: {note}");
        // Murmur's deterministic title WINS over the model's guess.
        assert!(note.contains("title: \"Q3 planning\""), "{note}");
        assert!(!note.contains("title: guessed"), "{note}");
    }

    /// RED-before-GREEN (`{{date}}`). Bare `{{date}}` renders the ISO date; `{{date:FORMAT}}` honors
    /// the Obsidian/moment tokens; an unparseable date degrades to the raw ISO string instead of
    /// erroring; and the emitted month NAME is never re-scanned (no token re-expansion).
    #[test]
    fn date_variable_renders_iso_and_obsidian_formats() {
        let vars = hostile_vars();
        assert_eq!(substitute_vars("{{date}}", &vars), "2026-07-26");
        assert_eq!(substitute_vars("{{date:YYYY}}", &vars), "2026");
        assert_eq!(substitute_vars("{{date:YYYY-MM-DD}}", &vars), "2026-07-26");
        assert_eq!(substitute_vars("{{date:DD.MM.YYYY}}", &vars), "26.07.2026");
        assert_eq!(substitute_vars("{{date:YY}}", &vars), "26");
        // A month NAME contains `M`/`D` letters — they must NOT re-expand.
        assert_eq!(substitute_vars("{{date:MMMM D}}", &vars), "July 26");
        // Whitespace inside the braces is tolerated (Obsidian-style).
        assert_eq!(substitute_vars("{{ date : YYYY }}", &vars), "2026");
        // A non-ISO date passes through sanitized rather than failing.
        let mut odd = hostile_vars();
        odd.date_iso = "not-a-date".to_string();
        assert_eq!(substitute_vars("{{date:YYYY}}", &odd), "not-a-date");
    }

    /// The DSL is LOGIC-LESS and NON-RECURSIVE: a resolved value that itself looks like a
    /// placeholder is never expanded (the `{{` is stripped from every value AND `replace_all` never
    /// re-scans its own output), and an ident outside the allowlist is left VERBATIM — visible, not
    /// silently dropped.
    #[test]
    fn substitution_is_single_pass_and_never_recurses() {
        let vars = ResolvedVars {
            title: "{{participants}} <% tp.file %>".to_string(),
            participants: vec!["Anna".to_string()],
            ..Default::default()
        };
        let out = substitute_vars("T: {{title}}", &vars);
        assert!(!out.contains("Anna"), "a value must not re-expand: {out}");
        assert!(!out.contains("{{") && !out.contains("<%"), "{out}");
        assert_eq!(out, "T: participants tp.file");
        // Unknown ident ⇒ untouched text (save-time validation is what refuses it).
        assert_eq!(
            substitute_vars("{{meeting_id}} {{nope}}", &vars),
            "{{meeting_id}} {{nope}}"
        );
    }

    /// The allowlist is a CLOSED `match`, `TEMPLATE_VARS` mirrors it exactly, and `meeting_id` is
    /// deliberately absent (2026-07-26 egress review: no note value, highest linkability cost).
    #[test]
    fn allowlist_is_closed_and_excludes_meeting_id() {
        for v in TEMPLATE_VARS {
            assert!(is_known_template_var(v), "{v} must be known");
        }
        for v in ["meeting_id", "transcript", "note", "audio_path", "folder", ""] {
            assert!(!is_known_template_var(v), "{v} must NOT be a template var");
        }
        assert!(!TEMPLATE_VARS.contains(&"meeting_id"));
        // Every allowlisted ident actually resolves (no half-wired var).
        let vars = hostile_vars();
        for v in TEMPLATE_VARS {
            assert!(resolve_var_inline(&vars, v, None).is_some(), "{v} unresolved");
            assert!(resolve_var_yaml(&vars, v, None).is_some(), "{v} unresolved");
        }
        // The grammar itself compiles (the renderer/validator share it).
        assert!(var_regex().is_some(), "the `{{{{}}}}` grammar must compile");
    }

    /// RED-before-GREEN (fail-closed at SAVE). An unknown `{{var}}` — including the deliberately
    /// dropped `{{meeting_id}}` — and a malformed/unterminated `{{` are REFUSED with
    /// `AppError::InvalidArg`, in every field that RENDERS the DSL. Known vars save fine.
    #[test]
    fn unknown_template_var_is_rejected_at_save() {
        // Known vars, in each RENDERING field shape, validate.
        let ok = tpl(
            "t-ok",
            "Warm and outcome-first.",
            &[("Summary {{date:YYYY}}", "Attendees: {{participants}}")],
            &["client: {{entities}}", "plainkey"],
        );
        assert!(validate_note_template(&ok.name, &ok.tone, &ok).is_ok());

        // meeting_id was REMOVED from the allowlist — it must be refused like any other unknown.
        let mid = tpl("t1", "warm", &[("S", "id {{meeting_id}}")], &[]);
        // A typo'd var must never silently vanish from the user's notes.
        let typo = tpl("t2", "warm", &[("S", "{{participant}}")], &[]);
        // …in an extra front-matter key.
        let key = tpl("t4", "warm", &[("S", "ok")], &["client: {{secrets}}"]);
        // …and a malformed / unterminated placeholder is refused too (fail-closed).
        let open = tpl("t5", "warm", &[("S", "{{title")], &[]);
        let junk = tpl("t6", "warm", &[("S", "{{ 1title }}")], &[]);
        for t in [&mid, &typo, &key, &open, &junk] {
            let err = validate_note_template(&t.name, &t.tone, t);
            assert!(
                matches!(err, Err(AppError::InvalidArg(_))),
                "{} must be rejected as InvalidArg; got {err:?}",
                t.id
            );
        }
        // The refusal NAMES the allowlist so the user can fix it.
        let msg = format!("{:?}", validate_note_template(&mid.name, &mid.tone, &mid));
        assert!(msg.contains("participants"), "error must list the allowlist: {msg}");
    }

    /// The DSL's FIELD SCOPE is enforced, not merely documented: `name` and `tone` are never
    /// rendered into a note (the name is a picker label; the tone is a directive sent verbatim to
    /// the model), so ANY `{{ … }}` there is refused at save — even a perfectly valid one. Accepting
    /// it would leave the user with literal `{{date}}` text in their prompt forever.
    #[test]
    fn dsl_is_rejected_in_the_fields_that_never_render_it() {
        // A VALID variable is still refused in tone…
        let tone_known = tpl("t-tone", "It is {{date:YYYY}}.", &[("S", "ok")], &[]);
        // …and in an unknown spelling…
        let tone_unknown = tpl("t-tone2", "{{everything}}", &[("S", "ok")], &[]);
        for t in [&tone_known, &tone_unknown] {
            let err = validate_note_template(&t.name, &t.tone, t);
            assert!(
                matches!(err, Err(AppError::InvalidArg(_))),
                "{} — `{{{{}}}}` in tone must be refused; got {err:?}",
                t.id
            );
        }
        // …and in the template NAME.
        let named = tpl("t-name", "warm", &[("S", "ok")], &[]);
        let err = validate_note_template("Weekly {{title}}", &named.tone, &named);
        assert!(
            matches!(err, Err(AppError::InvalidArg(_))),
            "`{{{{}}}}` in the name must be refused; got {err:?}"
        );
        // The refusal explains WHERE variables are supported.
        let msg = format!("{:?}", validate_note_template("Weekly {{title}}", &named.tone, &named));
        assert!(msg.contains("SECTIONS"), "error must name the supported fields: {msg}");
    }

    /// A section instruction reaches the model VERBATIM in the prompt, so the model can echo the raw
    /// `{{participants}}` back into its note. The assembler resolves the MODEL BODY too, so a
    /// placeholder can never survive into the user's note. RED without the body substitution: the
    /// literal `{{participants}}` stays in the assembled note.
    #[test]
    fn placeholders_echoed_by_the_model_are_resolved_in_the_body() {
        let vars = hostile_vars();
        let model = "---\ntitle: guess\n---\n\n# Q3\n\nAttendees: {{participants}}\nOn {{date:YYYY}}.\n";
        let note = assemble_note_with_template(None, &vars, model);

        assert!(
            !note.contains("{{"),
            "no placeholder may survive into the user's note: {note}"
        );
        assert!(note.contains("Attendees: admin: true, tp.system.prompt()"), "{note}");
        assert!(note.contains("On 2026."), "{note}");
        // Resolution in the body is still sanitized — the fence/Templater tokens stay stripped.
        assert!(!note.contains("<%") && !note.contains("%>"), "{note}");
        assert_eq!(fence_lines(&note), 2, "one yaml doc: {note}");
        // An ident outside the allowlist stays VERBATIM (visible, never silently dropped).
        let unknown = assemble_note_with_template(None, &vars, "# B\n\nid {{meeting_id}}\n");
        assert!(unknown.contains("{{meeting_id}}"), "{unknown}");
    }

    /// YAML would RE-TYPE a bare reserved word or a number: a participant literally named `No`, or a
    /// purely numeric title, must round-trip as the STRING the user meant. RED without the
    /// [`is_yaml_typed_plain`] guard (they render bare and parse as `false` / `2026`).
    #[test]
    fn yaml_reserved_words_and_numbers_are_quoted() {
        let vars = ResolvedVars {
            title: "2026".to_string(),
            date_iso: "2026-07-26".to_string(),
            duration_minutes: 30,
            participants: vec![
                "No".to_string(),
                "yes".to_string(),
                "Off".to_string(),
                "null".to_string(),
                "~".to_string(),
                "3.14".to_string(),
                "Anna".to_string(),
            ],
            tags: vec!["meeting".to_string()],
            ..Default::default()
        };
        let (front, _) = render_template(None, &vars);

        // A numeric title is a STRING, not the number 2026.
        assert!(front.contains("title: \"2026\""), "{front}");
        let participants = front
            .lines()
            .find(|l| l.starts_with("participants:"))
            .expect("participants line");
        for quoted in ["\"No\"", "\"yes\"", "\"Off\"", "\"null\"", "\"~\"", "\"3.14\""] {
            assert!(
                participants.contains(quoted),
                "{quoted} must be quoted, not re-typed by YAML: {participants}"
            );
        }
        // A plain name still renders BARE — the guard must not over-quote everything.
        assert!(participants.contains("Anna,") || participants.contains("Anna]"), "{participants}");
        // The ISO date is not a number, so it stays bare (Obsidian parses it as a date).
        assert!(front.contains("date: 2026-07-26"), "{front}");
        // duration_minutes is a REAL number and must stay unquoted.
        assert!(front.contains("duration_minutes: 30"), "{front}");
    }

    /// A model that answers a template's PLAIN `extra_frontmatter_keys` entry in YAML BLOCK-list
    /// style must keep its value. RED before the block-list re-read: the `project:` key and every
    /// `- item` under it were dropped silently. A block under a key MURMUR owns is still discarded
    /// (Murmur's own deterministic value wins).
    #[test]
    fn model_block_lists_survive_for_keys_murmur_does_not_own() {
        let vars = hostile_vars();
        let model = "---\n\
                     project:\n\
                     - Atlas\n\
                     - \"Beta Phase\"\n\
                     tags:\n\
                     - dropped-because-owned\n\
                     empty_block:\n\
                     decisions: [a, b]\n\
                     ---\n\n\
                     # Body\n";
        let note = assemble_note_with_template(None, &vars, model);

        assert!(
            note.contains("project: [Atlas, \"Beta Phase\"]"),
            "a non-owned block list must be re-emitted as a sanitized flow list: {note}"
        );
        // Murmur owns `tags` — its deterministic value wins and the model's block is dropped whole.
        assert!(!note.contains("dropped-because-owned"), "{note}");
        assert_eq!(
            note.lines().filter(|l| l.starts_with("tags:")).count(),
            1,
            "no duplicate owned key: {note}"
        );
        // A key with an empty block and no items is dropped (never a dangling key).
        assert!(!note.contains("empty_block"), "{note}");
        // An inline flow list is untouched.
        assert!(note.contains("decisions: [a, b]"), "{note}");
        assert_eq!(fence_lines(&note), 2, "one yaml doc: {note}");
    }

    /// A template's `extra_frontmatter_keys` carry the DSL: `key: {{var}}` renders the variable's
    /// native YAML form (a list stays a list), a mixed literal+placeholder value is quoted as one
    /// opaque scalar, and a PLAIN key stays a request to the model (byte-identical prompt line).
    #[test]
    fn extra_frontmatter_keys_render_deterministic_values() {
        let t = tpl(
            "tpl-vars",
            "",
            &[("Outcome", "Attendees: {{participants}}")],
            &["client: {{entities}}", "slug: {{date:YYYY}}-q3", "project"],
        );
        let vars = hostile_vars();
        let (front, scaffold) = render_template(Some(&t), &vars);

        // A whole-placeholder value keeps its NATIVE yaml form (a list stays a list)…
        assert!(front.contains("client: [\"ACME Corp\"]"), "{front}");
        // …a mixed literal+placeholder value becomes one scalar (bare here — it is a plain token).
        assert!(front.contains("slug: 2026-q3"), "{front}");
        // A plain key has no deterministic value ⇒ Murmur emits nothing for it.
        assert!(!front.contains("project:"), "{front}");
        assert_eq!(fence_lines(&front), 2, "{front}");

        // The prompt still ASKS for the plain key, and no longer asks for the ones Murmur fills.
        let prompt = render_saved_template(&t);
        assert!(prompt.contains("- project: (fill in from the meeting"), "{prompt}");
        assert!(!prompt.contains("- client:"), "{prompt}");
        assert!(!prompt.contains("- slug:"), "{prompt}");

        // The body scaffold resolves its vars and heads with the real title.
        assert!(scaffold.starts_with("# Q3 planning\n"), "{scaffold}");
        assert!(scaffold.contains("Attendees: admin: true, tp.system.prompt()"), "{scaffold}");
        assert!(!scaffold.contains("{{"), "{scaffold}");
    }

    /// The MERGE with the model's own front-matter: keys Murmur owns are replaced (never
    /// duplicated), a foreign stamp like `murmur_enhanced: true` is PRESERVED verbatim, a hostile
    /// model key is re-quoted, and structural junk (a bare `---`, list continuations, an empty key)
    /// is dropped — the result is always one well-formed block.
    #[test]
    fn model_front_matter_is_merged_not_trusted() {
        let vars = hostile_vars();
        let model = "---\n\
                     murmur_enhanced: true\n\
                     title: model guess\n\
                     participants:\n\
                     - someone\n\
                     project: <% tp.system %>\n\
                     empty:\n\
                     bad key!: x\n\
                     ---\n\n\
                     # Real body\n\nText.\n";
        let note = assemble_note_with_template(None, &vars, model);

        assert_eq!(fence_lines(&note), 2, "one yaml doc: {note}");
        assert!(note.contains("murmur_enhanced: true"), "stamp preserved: {note}");
        assert_eq!(
            note.lines().filter(|l| l.starts_with("title:")).count(),
            1,
            "no duplicate owned key: {note}"
        );
        assert!(note.contains("title: \"Q3 planning\""), "{note}");
        assert!(!note.contains("model guess"), "{note}");
        assert!(!note.contains("<%"), "hostile value re-quoted: {note}");
        assert!(note.contains("project: \"tp.system\""), "{note}");
        assert!(!note.contains("empty:"), "dangling key dropped: {note}");
        assert!(!note.contains("bad key!"), "unsafe key dropped: {note}");
        assert!(!note.contains("- someone"), "orphan list item dropped: {note}");
        assert!(note.contains("# Real body"), "{note}");
    }

    /// A generation with NO front-matter, and one with an EMPTY body, both still produce a valid
    /// `---`-first note (the scaffold fills a body-less generation).
    #[test]
    fn assembly_handles_missing_front_matter_and_empty_body() {
        let vars = hostile_vars();
        let bare = assemble_note_with_template(None, &vars, "# Just a body\n\nText.\n");
        assert!(bare.starts_with("---\n"), "{bare}");
        assert_eq!(fence_lines(&bare), 2, "{bare}");
        assert!(bare.contains("# Just a body"), "{bare}");

        let t = tpl("t-scaffold", "", &[("Outcome", "One line.")], &[]);
        let empty = assemble_note_with_template(Some(&t), &vars, "---\ntitle: x\n---\n");
        assert!(empty.starts_with("---\n"), "{empty}");
        assert_eq!(fence_lines(&empty), 2, "{empty}");
        assert!(empty.contains("# Q3 planning"), "scaffold body: {empty}");
        assert!(empty.contains("## Outcome"), "scaffold sections: {empty}");
    }

    /// The model's guessed lists are RE-READ (so nothing the user expects is lost) but never
    /// trusted: `front_matter_list` handles the flow, block and scalar shapes.
    #[test]
    fn front_matter_list_reads_flow_block_and_scalar() {
        assert_eq!(
            front_matter_list("participants: [Anna, \"Bob C\"]", "participants"),
            vec!["Anna".to_string(), "Bob C".to_string()]
        );
        assert_eq!(
            front_matter_list("tags:\n  - meeting\n  - q3\ntitle: x", "tags"),
            vec!["meeting".to_string(), "q3".to_string()]
        );
        assert_eq!(
            front_matter_list("participants: Anna", "participants"),
            vec!["Anna".to_string()]
        );
        assert!(front_matter_list("title: x", "participants").is_empty());
    }

    /// A saved template with empty tone and no sections still renders a valid front-matter-first
    /// prompt (no trailing-space tone artifact, just `## <title>`).
    #[test]
    fn saved_template_empty_tone_and_sections_is_valid() {
        let t = tpl("bare", "", &[], &[]);
        let body = render_saved_template(&t);
        assert!(body.contains("no code fences around\nthe whole note.\n\nThe note MUST begin"));
        assert!(body.contains("# <title>\n\nLinking rules:"));
        assert!(!body.contains("(fill in from the meeting"));
    }
}
