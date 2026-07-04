use crate::summarize::provider::SummarizeRequest;

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
        _ => default_template(),
    }
}

/// Build a style variant: the shared front-matter contract + a style-specific tone line and
/// body section layout. Mirrors `default_template`'s invariants (first line `---`, no fences).
fn style_variant(tone: &str, body_sections: &str) -> String {
    format!(
        r#"You are a meticulous meeting-notes writer for an Obsidian vault.

Produce a SINGLE, complete Markdown note summarizing the meeting transcript that
follows. Output ONLY the note — no preamble, no explanation, no code fences around
the whole note. {tone}

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
pub fn render_prompt(req: &SummarizeRequest) -> String {
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

    // brain2 RAG Phase 4 — RETRIEVAL-AUGMENTED NOTE GENERATION. When `related_context` is present
    // (the flag is ON and the gated retrieval found visible prior notes), prepend it as a clearly
    // labelled, read-only block BEFORE the transcript so the model can reference prior decisions /
    // owed items and cite them as `[[Title]]`. When `None` (the default + flag-OFF case) this whole
    // block is skipped, so the output is BYTE-IDENTICAL to before this field existed — no regression.
    if let Some(ctx) = &req.related_context {
        if !ctx.trim().is_empty() {
            out.push_str(
                "\n## Related prior notes (context only — do not copy, cite as [[Title]])\n",
            );
            out.push_str(ctx);
            out.push('\n');
        }
    }

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

    out.push_str("\nTRANSCRIPT\n");
    out.push_str(&req.transcript);
    out.push('\n');

    out
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

    /// Flag ON: a `Some(context)` prepends the labelled, read-only block BEFORE the transcript.
    #[test]
    fn render_user_content_some_prepends_block() {
        let ctx = "\n\n### [[Q2 Planning]] · 2026-04-01 · id:m-prev\nWe decided to delay launch.";
        let out = render_user_content(&req(Some(ctx.to_string())));
        let block_at = out
            .find("## Related prior notes (context only — do not copy, cite as [[Title]])")
            .expect("related block present");
        let transcript_at = out.find("\nTRANSCRIPT\n").expect("transcript present");
        assert!(
            block_at < transcript_at,
            "related block must precede the transcript"
        );
        assert!(out.contains("Q2 Planning"));
        assert!(out.contains("We decided to delay launch."));
    }

    /// An empty/whitespace context is treated as None (no block, byte-identical to the None path).
    #[test]
    fn render_user_content_empty_context_is_skipped() {
        assert_eq!(
            render_user_content(&req(Some("   \n".to_string()))),
            render_user_content(&req(None))
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

    /// The skeleton block lands AFTER the related-notes block and BEFORE the transcript,
    /// carries the notes verbatim, and instructs the `## Also discussed` / no-`My notes` contract.
    #[test]
    fn user_notes_block_renders_between_related_and_transcript() {
        let mut r = req(Some(
            "### [[Prior]] · 2026-06-01 · id:x\nprior body".to_string(),
        ));
        r.user_notes = Some("ship Friday\nAnna owns QA".to_string());
        let s = render_user_content(&r);
        let related_at = s
            .find("## Related prior notes")
            .expect("related block present");
        let notes_at = s.find("ship Friday\nAnna owns QA").expect("notes verbatim");
        let transcript_at = s.find("\nTRANSCRIPT\n").expect("transcript section");
        assert!(related_at < notes_at, "skeleton after related notes");
        assert!(notes_at < transcript_at, "skeleton before transcript");
        assert!(
            s.contains("## Also discussed"),
            "instructs the Also discussed section"
        );
        assert!(
            s.contains("Never output a section titled"),
            "forbids a My notes section"
        );
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
}
