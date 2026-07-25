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

/// Reject a note template whose ANY text field contains a scripting token. Called by the
/// `save_note_template` command BEFORE persisting. Declarative data only, ever — this is the
/// security boundary for the template layer (the rendered prompt still passes the
/// `RedactingProvider` firewall on egress, unchanged).
pub fn validate_note_template(name: &str, tone: &str, t: &NoteTemplate) -> Result<()> {
    let mut fields: Vec<&str> = vec![name, tone];
    for s in &t.sections {
        fields.push(&s.heading);
        fields.push(&s.instruction);
    }
    for k in &t.extra_frontmatter_keys {
        fields.push(k);
    }
    for f in fields {
        for tok in FORBIDDEN_TEMPLATE_TOKENS {
            if f.contains(tok) {
                return Err(AppError::InvalidArg(format!(
                    "note template may not contain the scripting token `{tok}` — templates are \
                     declarative data, not code"
                )));
            }
        }
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
    let extra = extra_keys
        .iter()
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

/// An explicit output-language directive appended to the summary system prompt so the WHOLE
/// note (section headings AND content) comes out in one consistent language. The YAML
/// front-matter KEYS stay English so Obsidian keeps parsing them.
pub fn language_directive(note_language: &str) -> String {
    let lang = note_language.trim();
    let target = if lang.is_empty() || lang.eq_ignore_ascii_case("auto") {
        "the SAME language as the meeting transcript below (match the speakers)".to_string()
    } else {
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
        name.to_string()
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
