# "Enhance My Notes" Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Granola-style "enhance" mode — the user's typed in-meeting notes become the *skeleton* of the generated summary (kept in their wording/order, expanded from the transcript, plus an "Also discussed" section), instead of today's verbatim `## My notes` append — with a settings toggle, a redaction-firewalled prompt path, and a calm signal-driven animation layer ("Quiet Weave") on the record + detail surfaces.

**Architecture:** Backend: add `user_notes: Option<String>` to `SummarizeRequest`, render a conditional skeleton block in `render_user_content` (mirroring the `related_context` byte-identical-when-absent pattern), extend `RedactingProvider` to scrub the new field, and replace the post-generation fold with a pure `finalize_note_markdown(generated, manual_notes, notes_mode)` that either stamps a deterministic `murmur_enhanced: true` front-matter marker (enhance) or runs today's verbatim fold (append/empty). A new `notes_mode` config key ("enhance" default | "append") clones the `note_style` pattern. Frontend: pure `computed()` states over existing store signals (zero effects, zero timers, PR-#108-safe), one-shot CSS sweep over note lines during summarize, a settled state that keeps the notes surface up through "done", an ✨ badge on detail derived *only* from the front-matter marker (inherently lock-safe), and a dim+sweep overlay during Re-summarize.

**Tech Stack:** Rust (Tauri 2.11, `meetnotes_lib`), Angular 18 zoneless (standalone, signals, inline styles), SQLCipher, CSS keyframes only (no JS animation).

## Global Constraints

- **Byte-identical invariant (backend-owned):** a meeting with an empty/whitespace-only `manual_notes` buffer produces byte-identical note output in BOTH modes; `user_notes: None` renders a byte-identical prompt. Append mode with notes = exactly today's output.
- **NEW CLOUD EGRESS — loud + justified:** enhance mode sends user-typed notes into the provider prompt (today they never egress). They MUST pass `RedactingProvider` like the transcript (Task 3), and the settings help copy states it. **lock-security-reviewer sign-off is REQUIRED before merge.**
- **Lock model:** no new read/export path; the pipeline's ungated `db.get_manual_notes` stays pipeline-internal (resummarize is gated upstream by `meeting_is_unlocked`, `src-tauri/src/commands.rs:2529`). The FE badge derives ONLY from `note.markdown` (null when masked) — never from a DTO field that could survive masking.
- **No PII in logs:** never log notes text or prompt content; ids/lengths/stages only.
- **Rust:** `AppError`/`Result` only; test loop = `( cd src-tauri && cargo test --lib )` — NEVER `cargo clippy --all-targets`; additive config only.
- **Angular zoneless:** signals/`computed()` only (no effects needed anywhere in this plan); no `setTimeout`/rAF in components; `@if`/`@for` with `track n.id`; all motion via `var(--transition)`/`var(--ease-spring)` tokens + explicit `prefers-reduced-motion` guards; style budgets — record.component gets **ZERO** new CSS (~18.4 kB raw, tightest), note-item.component gets **ZERO** new CSS (~13.1 kB raw, past warn); new animation CSS goes in meeting-conversation.component (~5.9 kB raw) and globals.
- **UI copy is English** (also fixes the stray Polish composer placeholder).
- **Git:** feature branch `feat/enhance-my-notes`; commits authored **only** by `QueaT <kgm004a@gmail.com>`, **NO Claude trailers**; merge to `murmur` trunk **via PR only**. The working tree has PRE-EXISTING unrelated modifications (`src-tauri/src/commands.rs`, `src-tauri/src/transcribe/live.rs`, untracked `src-tauri/binaries/`) — `git add` ONLY the files each task names; never `git add -A`.
- **No new dependencies** (npm or crates).
- Cited line numbers were verified 2026-07-02 but may drift slightly — anchor by the quoted code, not the number.

## File Structure

| File | Responsibility in this plan |
|---|---|
| `src-tauri/src/settings/config.rs` | `notes_mode` key: field, default, const, load, save (+ 2 JSON fixtures) |
| `src-tauri/src/commands.rs` | `AppConfigDto.notes_mode` + `config_to_dto`/`dto_to_config` mapping |
| `src-tauri/src/summarize/provider.rs` | `SummarizeRequest.user_notes: Option<String>` |
| `src-tauri/src/summarize/template.rs` | conditional skeleton block in `render_user_content` + tests |
| `src-tauri/src/summarize/redact.rs` | scrub `user_notes` through the shared redaction map + test |
| `src-tauri/src/pipeline.rs` | fetch-before-request wiring, `finalize_note_markdown`, `mark_enhanced` + tests |
| `src/app/core/models.ts` | `AppConfigDto.notesMode` |
| `src/app/features/settings/settings.component.ts` | the enhance/append `<select>` (reactive-forms clone of noteStyle) |
| `src/app/core/meeting-conversation.store.ts` | `hasPersistedNotes` computed |
| `src/app/features/record/record.component.ts` | `enhanceMode`/`enhancingNotes`/`enhanceSettled` computeds, `showAssistant` + `hint()` extensions, input pass-through (NO CSS) |
| `src/app/features/record/meeting-conversation.component.ts` | `enhancing`/`settled`/`enhanceAware` inputs, orb override, hint branches, sweep CSS, Polish-placeholder fix |
| `src/app/features/detail/detail.component.ts` | `ParsedNote.enhanced` + `murmur_enhanced` parse, ✨ badge, resummarize stale-guard + `.is-resummarizing` overlay |
| `src/styles.css` | global `.pill-enhanced` |

Backend tasks 1→4 are sequential (each builds on the previous). FE tasks 5→7 depend on Task 1's DTO field name only (5) and Task 4's marker string only (7); they can run after Task 4 or in parallel with backend by a second worker if coordinated.

---

### Task 1: `notes_mode` config key (Rust, end-to-end)

**Files:**
- Modify: `src-tauri/src/settings/config.rs` (field ~line 108 region, Default ~227, consts ~267, load ~354, save ~452, JSON fixtures ~589 + ~652)
- Modify: `src-tauri/src/commands.rs` (AppConfigDto ~line 96 region, `config_to_dto` ~2332, `dto_to_config` ~2389)

**Interfaces:**
- Consumes: existing `note_style` pattern (imitated verbatim).
- Produces: `AppConfig.notes_mode: String` (`"enhance"` default | `"append"`), `const K_NOTES_MODE: &str = "notes_mode"`, `AppConfigDto.notes_mode: String` with `#[serde(default)]` (camelCase wire name `notesMode`), empty-string→`"enhance"` fallback in `dto_to_config`. Task 4 reads `config.notes_mode`; Task 5 reads/writes `notesMode`.

- [ ] **Step 1: Create the branch**

```bash
git checkout -b feat/enhance-my-notes
```

- [ ] **Step 2: Write the failing test** — in the existing `#[cfg(test)]` module of `src-tauri/src/settings/config.rs`:

```rust
/// ENHANCE-MY-NOTES: the mode defaults to "enhance" for fresh installs AND for existing
/// users (AppConfig::load falls back to Default for a never-written key).
#[test]
fn notes_mode_defaults_to_enhance() {
    assert_eq!(AppConfig::default().notes_mode, "enhance");
}
```

- [ ] **Step 3: Run it to verify it fails**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib notes_mode_defaults_to_enhance )
```
Expected: COMPILE ERROR — `no field notes_mode on type AppConfig`. (Compile-fail is the RED here.)

- [ ] **Step 4: Implement** — clone the `note_style` pattern exactly:

In `config.rs`, inside `pub struct AppConfig` (next to `note_style`, ~line 108):
```rust
    /// ENHANCE-MY-NOTES: how the user's typed in-meeting notes shape the summary.
    /// "enhance" (default) — the notes become the SKELETON of the generated note (they ride
    /// INSIDE the redacted provider prompt — a deliberate, loud, consent-riding egress);
    /// "append" — legacy: transcript-only summary + verbatim `## My notes` section.
    /// Empty/unknown values fall back to "enhance".
    pub notes_mode: String,
```

In `impl Default for AppConfig` (~line 227):
```rust
            notes_mode: "enhance".to_string(),
```

Next to `const K_NOTE_STYLE` (~line 267):
```rust
const K_NOTES_MODE: &str = "notes_mode";
```

In `AppConfig::load` (next to the `K_NOTE_STYLE` block, ~line 354):
```rust
        if let Some(v) = db.get_setting(K_NOTES_MODE)? {
            if !v.is_empty() {
                cfg.notes_mode = v;
            }
        }
```

In `AppConfig::save` (~line 452):
```rust
        db.set_setting(K_NOTES_MODE, &self.notes_mode)?;
```

In `commands.rs`, inside `pub struct AppConfigDto` (next to `note_style`, ~line 96 — the struct is `#[serde(rename_all = "camelCase")]`, so the wire name becomes `notesMode`):
```rust
    /// ENHANCE-MY-NOTES mode: "enhance" | "append" ("" from an older FE ⇒ "enhance").
    #[serde(default)]
    pub notes_mode: String,
```

In `config_to_dto` (~line 2332):
```rust
        notes_mode: c.notes_mode.clone(),
```

In `dto_to_config` (~line 2389, mirror the `note_style` empty-fallback so an older FE payload can never wipe the mode):
```rust
        notes_mode: if d.notes_mode.trim().is_empty() {
            "enhance".to_string()
        } else {
            d.notes_mode
        },
```

- [ ] **Step 5: Run the full lib suite; repair the two JSON fixtures**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib )
```
Expected: `notes_mode_defaults_to_enhance` PASSES. If the config round-trip tests at `config.rs:~589` and `~652` fail on a missing/mismatched field, add `"notesMode": "enhance"` to those raw-JSON DTO fixtures (they construct a full DTO; `#[serde(default)]` covers deserialization, the equality assertions may still need the field). Re-run until green. NOTE: `get_config`/`save_config` are already registered in `lib.rs` `generate_handler!` — a new config FIELD needs **no** lib.rs change.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/settings/config.rs src-tauri/src/commands.rs
git commit -m "feat(settings): notes_mode config key (enhance|append, default enhance)"
```

---

### Task 2: `SummarizeRequest.user_notes` + the skeleton prompt block

**Files:**
- Modify: `src-tauri/src/summarize/provider.rs:16-32` (struct)
- Modify: `src-tauri/src/summarize/template.rs:164-222` (`render_user_content`) + its `mod tests` (fixture `req()` at ~229)
- Modify: every other `SummarizeRequest { … }` literal (find via grep; at minimum the `redact.rs` test fixture `sample_req` ~line 435)

**Interfaces:**
- Consumes: `SummarizeRequest` (5 existing fields), `render_user_content` ordering: METADATA → EXISTING NOTE TITLES → `## Related prior notes` (optional) → TRANSCRIPT.
- Produces: `pub user_notes: Option<String>` on `SummarizeRequest`; the skeleton block renders between the related-notes block and `TRANSCRIPT`, and ONLY when `Some` + non-blank. Prompt contract for downstream (Task 4 + FE): output sections use `## ` headings, never duplicated, extra section is headed exactly `## Also discussed`, a section titled `My notes` is forbidden, front-matter-first is preserved (claude_code validates the leading `---`).

- [ ] **Step 1: Write the failing tests** — in `src-tauri/src/summarize/template.rs` `mod tests` (the `req(related: Option<String>)` fixture exists at ~229; it will gain `user_notes: None` in Step 3):

```rust
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
    let mut r = req(Some("### [[Prior]] · 2026-06-01 · id:x\nprior body".to_string()));
    r.user_notes = Some("ship Friday\nAnna owns QA".to_string());
    let s = render_user_content(&r);
    let related_at = s.find("## Related prior notes").expect("related block present");
    let notes_at = s.find("ship Friday\nAnna owns QA").expect("notes verbatim");
    let transcript_at = s.find("\nTRANSCRIPT\n").expect("transcript section");
    assert!(related_at < notes_at, "skeleton after related notes");
    assert!(notes_at < transcript_at, "skeleton before transcript");
    assert!(s.contains("## Also discussed"), "instructs the Also discussed section");
    assert!(s.contains("Never output a section titled"), "forbids a My notes section");
}
```

- [ ] **Step 2: Run to verify RED**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib user_notes_ )
```
Expected: COMPILE ERROR — `no field user_notes on type SummarizeRequest`.

- [ ] **Step 3: Implement**

In `provider.rs`, append to `pub struct SummarizeRequest` (after `related_context`):
```rust
    /// ENHANCE-MY-NOTES: the user's own typed in-meeting notes (the `manual_notes` buffer —
    /// raw `\n`-joined lines, NOT markdown bullets), present ONLY when `notes_mode == "enhance"`
    /// AND the buffer is non-blank. `None` ⇒ `render_user_content` is byte-identical to before
    /// this field existed (same contract as `related_context`). SECURITY: this string EGRESSES
    /// to the provider in the prompt — `RedactingProvider` MUST scrub it alongside the
    /// transcript (summarize/redact.rs) before egress. Today's append mode never egresses it.
    pub user_notes: Option<String>,
```

In `template.rs` `render_user_content`, insert between the `related_context` block and the `TRANSCRIPT` push (i.e. right before `out.push_str("\nTRANSCRIPT\n");`):
```rust
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
```

Update ALL `SummarizeRequest { … }` literals to carry the new field:
```bash
grep -rn "SummarizeRequest {" src-tauri/src
```
For every **test fixture** hit (`template.rs` `req()` ~229, `redact.rs` `sample_req` ~435, any provider tests): add `user_notes: None,`. Do NOT touch `pipeline.rs`'s construction yet — that is Task 4 (if the grep shows it, add `user_notes: None,` there temporarily so the crate compiles; Task 4 replaces it).

- [ ] **Step 4: Run to verify GREEN**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib )
```
Expected: both new tests PASS, all pre-existing tests PASS (fixtures compile).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/summarize/provider.rs src-tauri/src/summarize/template.rs src-tauri/src/summarize/redact.rs src-tauri/src/pipeline.rs
git commit -m "feat(summarize): user_notes skeleton block in the summary prompt"
```
(Include `redact.rs`/`pipeline.rs` only if the fixture/temp-field edits touched them.)

---

### Task 3: Redact `user_notes` before egress (the firewall extension)

**Files:**
- Modify: `src-tauri/src/summarize/redact.rs:314-342` (`RedactingProvider::summarize`) + its `mod tests`

**Interfaces:**
- Consumes: `SummarizeRequest.user_notes` (Task 2), the existing shared-map redaction of `transcript` + `related_context` (`redact_into(&req.transcript, &mut map, &mut rev)` … `r.transcript = red_transcript; r.related_context = red_related;`).
- Produces: `user_notes` scrubbed through the SAME `map`/`rev` (consistent restore in the reply) + the same name-layer pass. Task 4's pipeline wiring relies on this being automatic for every provider behind `make_provider`.

- [ ] **Step 1: Write the failing test** — in `redact.rs` `mod tests`. The module already has a `sample_req` fixture (~line 435) and tests that drive `RedactingProvider` over a mock inner provider asserting what egresses; **reuse that file's existing mock-inner-provider type**. If (and only if) no existing mock captures the full request, add this one to the test module:

```rust
    /// Captures the exact SummarizeRequest the wrapped (i.e. EGRESSING) provider receives.
    struct CapturingInner(std::sync::Mutex<Option<SummarizeRequest>>);

    #[async_trait::async_trait]
    impl SummarizerProvider for CapturingInner {
        fn id(&self) -> &str {
            "capture"
        }
        async fn availability(&self) -> Availability {
            // Use the same variant the file's other mocks return.
            Availability::Ready
        }
        async fn summarize(&self, req: &SummarizeRequest) -> Result<String> {
            *self.0.lock().unwrap() = Some(req.clone());
            Ok("---\ntitle: T\n---\n# T\n".to_string())
        }
        async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(String::new())
        }
    }
```
(Adapt the `Availability` variant and constructor style to what the file's existing mocks use — read them first; the assertions below are the contract.)

```rust
/// ENHANCE-MY-NOTES: user_notes EGRESSES in enhance mode, so it MUST pass the same
/// redaction firewall as the transcript — emails/phones never leave un-scrubbed.
#[tokio::test]
async fn user_notes_are_redacted_before_egress() {
    let mut req = sample_req();
    req.user_notes = Some("ping bob@corp.com about the deck".to_string());
    let inner = std::sync::Arc::new(CapturingInner(std::sync::Mutex::new(None)));
    let provider = RedactingProvider::new(inner.clone()); // match the file's constructor
    let _ = provider.summarize(&req).await.unwrap();
    let egressed = inner.0.lock().unwrap().clone().expect("inner called");
    let notes = egressed.user_notes.expect("user_notes forwarded");
    assert!(
        !notes.contains("bob@corp.com"),
        "email must not egress un-redacted: {notes}"
    );
    assert!(
        notes.contains("about the deck"),
        "non-PII text passes through: {notes}"
    );
}
```
(Match `RedactingProvider::new`'s real signature — if it takes a names-layer/config argument, copy the construction from the adjacent existing test.)

- [ ] **Step 2: Run to verify RED**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib user_notes_are_redacted )
```
Expected: FAIL — the assertion `email must not egress un-redacted` (the field is forwarded verbatim by `req.clone()`).

- [ ] **Step 3: Implement** — in `RedactingProvider::summarize` (redact.rs:314-342), mirror the `related_context` handling for `user_notes`:

Right after `let red_related = req.related_context…map(|c| redact_into(c, &mut map, &mut rev));`:
```rust
        // ENHANCE-MY-NOTES: the typed notes ride the prompt in enhance mode — scrub them
        // through the SAME shared map as the transcript so a value redacted anywhere
        // restores consistently in the reply.
        let red_notes = req
            .user_notes
            .as_ref()
            .map(|c| redact_into(c, &mut map, &mut rev));
```

Right after the name-layer pass over `red_related`:
```rust
        let red_notes = red_notes.map(|c| {
            let (c2, more) = self.names.redact_names(&c);
            name_pairs.extend(more);
            c2
        });
```

And where the cloned request is assembled (`r.transcript = …; r.related_context = …;`):
```rust
        r.user_notes = red_notes;
```

- [ ] **Step 4: Run to verify GREEN**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib )
```
Expected: all PASS, including the new redaction test.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/summarize/redact.rs
git commit -m "feat(redact): scrub user_notes through the egress firewall"
```

---

### Task 4: Pipeline wiring — `finalize_note_markdown` + `mark_enhanced`

**Files:**
- Modify: `src-tauri/src/pipeline.rs` (`summarize_and_export` ~497-596: move the notes fetch above the request, fill `user_notes`, replace the fold call; new pure fns next to `fold_manual_notes` ~735; tests next to ~1150)

**Interfaces:**
- Consumes: `config.notes_mode` (Task 1), `SummarizeRequest.user_notes` (Task 2), redaction (Task 3), existing `fold_manual_notes` + `db.get_manual_notes`.
- Produces: `fn finalize_note_markdown(generated: &str, manual_notes: &str, notes_mode: &str) -> String` and `fn mark_enhanced(markdown: &str) -> String`. **Marker contract for Task 7:** enhance-with-notes output front-matter contains the exact line `murmur_enhanced: true` (stamped deterministically by the backend, never by the model); append/empty output is unchanged. Resummarize inherits everything (it re-enters `summarize_and_export`, which re-reads the durable buffer).

- [ ] **Step 1: Write the failing tests** — in `pipeline.rs`'s test module, next to `fold_manual_notes_appends_section_and_empty_is_byte_identical` (~1150):

```rust
/// ENHANCE-MY-NOTES: the mode switch. Empty notes ⇒ byte-identical in BOTH modes (the hard
/// invariant); append + notes ⇒ exactly today's verbatim fold; enhance + notes ⇒ the
/// front-matter marker is stamped, NO verbatim `## My notes` section, body preserved;
/// unknown mode ⇒ defensive fall-back to the legacy fold.
#[test]
fn finalize_note_markdown_switches_between_enhance_and_append() {
    let note = "---\ntitle: Sync\n---\n# Sync\n\n- decided X";
    assert_eq!(finalize_note_markdown(note, "", "enhance"), note);
    assert_eq!(finalize_note_markdown(note, "", "append"), note);
    assert_eq!(finalize_note_markdown(note, "   \n\t ", "enhance"), note);
    assert_eq!(
        finalize_note_markdown(note, "ship Friday", "append"),
        fold_manual_notes(note, "ship Friday"),
        "append mode is byte-identical to today's fold"
    );
    let enhanced = finalize_note_markdown(note, "ship Friday", "enhance");
    assert!(enhanced.contains("murmur_enhanced: true"), "marker stamped: {enhanced}");
    assert!(!enhanced.contains("## My notes"), "no verbatim section in enhance mode");
    assert!(enhanced.contains("# Sync"), "generated body preserved");
    assert_eq!(
        finalize_note_markdown(note, "x", "banana"),
        fold_manual_notes(note, "x"),
        "unknown mode falls back to the legacy fold"
    );
}

/// The marker is a deterministic backend stamp: inserted as the first front-matter line,
/// idempotent, and a no-op when the provider output has no front-matter (defensive —
/// ollama output may lack it).
#[test]
fn mark_enhanced_stamps_front_matter_idempotently() {
    let note = "---\ntitle: T\n---\n# T";
    let stamped = mark_enhanced(note);
    assert!(
        stamped.starts_with("---\nmurmur_enhanced: true\ntitle: T\n"),
        "marker is the first front-matter line: {stamped}"
    );
    assert_eq!(mark_enhanced(&stamped), stamped, "idempotent");
    let bare = "# No front matter";
    assert_eq!(mark_enhanced(bare), bare, "no front-matter ⇒ unchanged");
    let unterminated = "---\ntitle: broken";
    assert_eq!(mark_enhanced(unterminated), unterminated, "unterminated fm ⇒ unchanged");
}
```

- [ ] **Step 2: Run to verify RED**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib finalize_note_markdown mark_enhanced )
```
Expected: COMPILE ERROR — the two functions don't exist.

- [ ] **Step 3: Implement the pure functions** — in `pipeline.rs`, directly below `fold_manual_notes` (~746):

```rust
/// ENHANCE-MY-NOTES finalize: decide how the typed notes reach the stored note.
/// - "enhance" + non-blank notes ⇒ the notes were already IN the prompt as the skeleton
///   (Task: user_notes on SummarizeRequest); do NOT append them again — stamp the
///   `murmur_enhanced: true` front-matter marker instead (the FE badge + honest provenance).
/// - anything else (append mode, empty buffer, unknown mode) ⇒ the legacy verbatim fold,
///   whose empty case is byte-identical passthrough. Pure + Db-free (unit-testable).
fn finalize_note_markdown(generated: &str, manual_notes: &str, notes_mode: &str) -> String {
    if notes_mode == "enhance" && !manual_notes.trim().is_empty() {
        mark_enhanced(generated)
    } else {
        fold_manual_notes(generated, manual_notes)
    }
}

/// Insert `murmur_enhanced: true` as the first YAML front-matter line — a DETERMINISTIC
/// backend stamp (never model-generated, so it can't be forgotten or hallucinated).
/// No/unterminated front-matter ⇒ returned unchanged; already stamped ⇒ unchanged.
fn mark_enhanced(markdown: &str) -> String {
    if markdown.contains("murmur_enhanced:") {
        return markdown.to_string();
    }
    match markdown.strip_prefix("---\n") {
        Some(rest) if rest.contains("\n---") => {
            format!("---\nmurmur_enhanced: true\n{rest}")
        }
        _ => markdown.to_string(),
    }
}
```

- [ ] **Step 4: Wire the pipeline** — in `summarize_and_export`:

(a) MOVE the manual-notes fetch from below `provider.summarize` (~582) to ABOVE the `SummarizeRequest` construction (~559), and fill the new field:
```rust
    // ENHANCE-MY-NOTES: fetch the typed-notes buffer BEFORE building the request — in
    // "enhance" mode the notes ride INSIDE the prompt as the skeleton (a NEW, deliberate,
    // REDACTED egress of user-typed content — see summarize/redact.rs); in "append" mode
    // (or with an empty buffer) they stay out of the prompt exactly as before. The buffer
    // read is ungated by design here (the pipeline is the producer of the note plaintext;
    // resummarize is gated upstream by meeting_is_unlocked).
    let manual_notes = state.db.get_manual_notes(meeting_id).unwrap_or_default();

    let request = SummarizeRequest {
        transcript: transcript_text.to_string(),
        meta: MeetingMeta {
            date_iso: date_iso.to_string(),
            title_hint: None,
            duration_s,
            language,
        },
        template: template::build_template(&config.note_style, &config.note_language),
        vault_titles,
        related_context,
        user_notes: if config.notes_mode == "enhance" && !manual_notes.trim().is_empty() {
            Some(manual_notes.clone())
        } else {
            None
        },
    };
```

(b) Replace the old fold call site (`let manual_notes = …; let markdown = fold_manual_notes(&generated, &manual_notes);` at ~582-583) with:
```rust
    let markdown = finalize_note_markdown(&generated, &manual_notes, &config.notes_mode);
```
Keep the existing block comment above it, amending its first line to:
```rust
    // brain2 realtime notes FINALIZE: `finalize_note_markdown` either stamps the enhance
    // marker (notes were in the prompt) or appends `## My notes` verbatim (append mode).
    // The `manual_notes` buffer stays the DURABLE CANONICAL store — never blanked here, so
    // every (re)summarize re-reads it fresh in EITHER mode; empty buffer ⇒ byte-identical.
```

- [ ] **Step 5: Run the full suite to verify GREEN**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib )
```
Expected: all PASS — including the untouched `finalize_folds_manual_notes_durably_and_resummarize_refolds` (append-path durability) and the two new tests.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/pipeline.rs
git commit -m "feat(pipeline): enhance-my-notes mode — skeleton prompt wiring + murmur_enhanced marker"
```

---

### Task 5: FE settings toggle (`notesMode`)

**Files:**
- Modify: `src/app/core/models.ts` (AppConfigDto, next to `noteStyle` ~line 57)
- Modify: `src/app/features/settings/settings.component.ts` (form ~2040, patchValue ~2222, save() ~2425, template below the "Summary style" field ~192-218)

**Interfaces:**
- Consumes: `AppConfigDto.notes_mode` wire field (`notesMode` camelCase, Task 1).
- Produces: `notesMode: string` on the FE `AppConfigDto`; the settings `<select formControlName="notesMode">`. Task 6 reads `config()?.notesMode` in RecordComponent.

- [ ] **Step 1: Add the model field** — in `src/app/core/models.ts`, inside `AppConfigDto` next to `noteStyle`:

```ts
  /** ENHANCE-MY-NOTES: how typed in-meeting notes shape the summary — "enhance" (they
   *  become the skeleton of the note) | "append" (verbatim `## My notes` section). */
  notesMode: string;
```

Then find every FE construction of a full `AppConfigDto` and add the field:
```bash
grep -rn "AppConfigDto" src/
```
(Expected hits: `models.ts`, `ipc.service.ts` signatures (no change), `settings.component.ts` `save()` (next step), possibly onboarding — add `notesMode: "enhance"` to any other full-DTO literal.)

- [ ] **Step 2: Clone the noteStyle pattern in settings.component.ts**

Form group (~2040), next to `noteStyle: "standard",`:
```ts
    notesMode: "enhance",
```

`patchValue` on load (~2222), next to `noteStyle: cfg.noteStyle ?? "standard",`:
```ts
        notesMode: cfg.notesMode ?? "enhance",
```

`save()` DTO (~2425), next to `noteStyle: v.noteStyle,`:
```ts
      notesMode: v.notesMode,
```

Template — insert this `<label class="field">` block DIRECTLY BELOW the "Summary style" field (after its closing `</label>` at ~line 218):
```html
        <label class="field">
          <span class="field-label">Your typed notes</span>
          <select formControlName="notesMode">
            <option value="enhance">
              Enhance — your notes become the outline (recommended)
            </option>
            <option value="append">Append — keep them verbatim below</option>
          </select>
          <span class="field-help text-muted">
            @switch (form.controls.notesMode.value) {
              @case ("append") {
                The summary is written from the transcript alone; your typed
                notes are added verbatim as a "My notes" section at the end.
              }
              @default {
                Your in-meeting bullets become the skeleton of the note — kept
                in your words and order, expanded with detail from the
                transcript, plus an "Also discussed" section for anything you
                didn't jot down. Notes pass the same redaction firewall as the
                transcript before any cloud call.
              }
            }
            Meetings where you typed nothing are identical in both modes.
          </span>
        </label>
```

- [ ] **Step 3: Verify with the FE gates**

```bash
npx ng lint && npx ng build
```
Expected: both clean (no template errors, budgets green).

- [ ] **Step 4: Commit**

```bash
git add src/app/core/models.ts src/app/features/settings/settings.component.ts
git commit -m "feat(settings): 'Your typed notes' enhance/append toggle"
```

---

### Task 6: Record surface — states + the Quiet-Weave hero animation

**Files:**
- Modify: `src/app/core/meeting-conversation.store.ts` (one computed next to `hasNotes` ~line 174)
- Modify: `src/app/features/record/record.component.ts` (3 computeds + `showAssistant` ~1049 + `hint()` ~1152 + template pass-through; **ZERO new CSS here**)
- Modify: `src/app/features/record/meeting-conversation.component.ts` (inputs, orb override, hint branches, `@for` delay binding, ~700 B CSS, Polish-placeholder fix)

**Interfaces:**
- Consumes: `notesMode` on the config DTO (Task 5), `RecorderStore.stage` signal, `MeetingConversationStore.notes()`/`orbState()`, RecordComponent's existing `config` signal (~1033) and injected `assistant` (MeetingConversationStore, ~1003), existing `isProcessing()`/`vaultMissing()` computeds, `<app-ai-orb [state]=…>` with `OrbState = 'idle'|'listening'|'processing'|'answer'`.
- Produces: `MeetingConversationStore.hasPersistedNotes: Signal<boolean>`; RecordComponent `enhanceMode`/`enhancingNotes`/`enhanceSettled` computeds; MeetingConversationComponent inputs `enhancing`/`settled`/`enhanceAware`. All state is pure `computed()` over root-store signals — survives tab-switch/remount (the PR #108 rule); NO effects, NO timers, NO new Stage values, NO new events.

- [ ] **Step 1: Store truth signal** — in `src/app/core/meeting-conversation.store.ts`, one line next to the existing `hasNotes` computed (~174):

```ts
  /** ENHANCE-MY-NOTES: true once at least one REAL persisted note line exists — i.e. what
   *  the summarizer will actually see (un-accepted @brain anchors are persisted:false). */
  readonly hasPersistedNotes = computed(() =>
    this._notes().some((n) => n.persisted && n.text.trim().length > 0),
  );
```
(If the file's notes signal is exposed as `notes` rather than `_notes` at that scope, read whichever the adjacent `hasNotes` computed reads and match it.)

- [ ] **Step 2: RecordComponent states** — in `src/app/features/record/record.component.ts`, next to the existing computeds (`isProcessing` etc.):

```ts
  /** ENHANCE-MY-NOTES: mode from config; missing/empty ⇒ enhance (the backend default). */
  readonly enhanceMode = computed(
    () => (this.config()?.notesMode ?? "enhance") === "enhance",
  );
  /** The hero trigger: summarizing a meeting whose notes will be the skeleton. */
  readonly enhancingNotes = computed(
    () =>
      this.enhanceMode() &&
      this.assistant.hasPersistedNotes() &&
      this.store.stage() === "summarizing",
  );
  /** Settled: done + the notes were enhanced — keeps the surface up through "Saved ✓". */
  readonly enhanceSettled = computed(
    () =>
      this.enhanceMode() &&
      this.assistant.hasPersistedNotes() &&
      this.store.stage() === "done",
  );
```
(`config` signal exists at ~1033; the conversation store is already injected as `assistant` at ~1003 — reuse, don't re-inject.)

Extend `showAssistant` (~1049) with two disjuncts so the notes surface survives Stop→transcribing→summarizing→done (append-mode/empty-notes meetings change NOTHING — the triple conjunction gates it):
```ts
      || (this.enhanceMode() &&
          this.isProcessing() &&
          this.assistant.hasPersistedNotes())
      || this.enhanceSettled()
```

Extend `hint()` (~1152) — two branch changes:
```ts
    if (this.isProcessing())
      return this.enhanceMode() && this.assistant.hasPersistedNotes()
        ? "Transcribing on-device, then enhancing your notes…"
        : "Transcribing on-device, then summarizing…";
```
and inside the existing `stage() === "done"` branch, BEFORE the current return:
```ts
      if (this.enhanceSettled())
        return this.vaultMissing()
          ? "Saved ✓ — your enhanced note is in Murmur."
          : "Saved ✓ — your enhanced note is in the vault.";
```

Template — extend the existing `<app-meeting-conversation …>` element with:
```html
  [enhancing]="enhancingNotes()"
  [settled]="enhanceSettled()"
  [enhanceAware]="enhanceMode()"
```

- [ ] **Step 3: MeetingConversationComponent** — in `src/app/features/record/meeting-conversation.component.ts`:

Class members (signal `input()` API only):
```ts
  /** ENHANCE-MY-NOTES presentation inputs (pure; all state lives in root stores). */
  readonly enhancing = input(false);
  readonly settled = input(false);
  readonly enhanceAware = input(false);

  /** During the enhance pass the orb shows its shipped 'processing' choreography. */
  readonly orbStateView = computed(() =>
    this.enhancing() ? ("processing" as const) : this.store.orbState(),
  );

  /** Stagger for the one-shot sweep — capped so short summarizes still show a full pass. */
  sweepDelay(i: number): number {
    return Math.min(i, 10) * 180;
  }
```

Template changes:
1. The existing `<app-ai-orb [state]="store.orbState()" …>` → `[state]="orbStateView()"`.
2. The surface hint span (the small helper text in the notes-surface header, near the orb — currently the "Type @brain…" line): add `role="status" aria-live="polite"` + the class bindings and branches:
```html
        <span
          class="surface-hint"
          role="status"
          aria-live="polite"
          [class.is-enhancing]="enhancing()"
          [class.is-settled]="settled()"
        >
          @if (enhancing()) {
            Enhancing your notes…
          } @else if (settled()) {
            ✨ Notes enhanced — your bullets became the outline.
          } @else if (enhanceAware() && store.hasPersistedNotes()) {
            ✨ These bullets will shape your summary
          } @else {
            <!-- keep the EXISTING default hint text here verbatim -->
          }
        </span>
```
(If the current hint element has a different class name, keep its name and add the bindings — the CSS below must target the real class.)
3. The `.flow` div (~88): add `[class.is-enhancing]="enhancing()"`.
4. The notes `@for` (~97): add the index + per-host delay (tracking stays `track n.id`):
```html
        @for (n of store.notes(); track n.id; let i = $index) {
          <app-note-item
            [note]="n"
            (followed)="scrollToBottom()"
            [style.animation-delay.ms]="enhancing() ? sweepDelay(i) : null"
          />
        }
```
5. Drive-by copy fix (~136): composer placeholder `'pisz notatkę… (@brain to open a thread)'` → `'write a note… (@brain to open a thread)'`.

Inline styles — append (~700 B; this component is the designated budget home at ~5.9 kB raw; note-item.component gets ZERO changes):
```css
      /* ── ENHANCE-MY-NOTES: one-shot Quiet-Weave sweep over the note lines ── */
      .flow app-note-item {
        display: block;
        transition: box-shadow var(--transition);
      }
      .flow.is-enhancing app-note-item {
        border-radius: var(--radius-sm);
        background-image: linear-gradient(
          100deg,
          transparent 32%,
          color-mix(in srgb, var(--accent) 12%, transparent) 50%,
          transparent 68%
        );
        background-size: 250% 100%;
        background-repeat: no-repeat;
        background-position: 200% 0; /* parked off-canvas after the single pass */
        animation: enhance-sweep 900ms ease-in-out both;
        box-shadow: inset 2px 0 0
          color-mix(in srgb, var(--accent) 35%, transparent);
      }
      @keyframes enhance-sweep {
        from {
          background-position: -150% 0;
        }
        to {
          background-position: 200% 0;
        }
      }
      .surface-hint.is-enhancing,
      .surface-hint.is-settled {
        color: var(--accent);
        animation: rise 200ms var(--transition) both;
      }
      @media (prefers-reduced-motion: reduce) {
        .flow.is-enhancing app-note-item {
          animation: none;
          background-image: none; /* the static inset accent edge remains as the cue */
        }
        .surface-hint.is-enhancing,
        .surface-hint.is-settled {
          animation: none;
        }
      }
```
(`rise` is the global shared keyframe in `src/styles.css`; the indeterminate wait is carried by the EXISTING `.proc-inline` shimmer in record.component + the orb 'processing' state — deliberately NO new looping element here.)

- [ ] **Step 4: Verify with the FE gates**

```bash
npx ng lint && npx ng build
```
Expected: clean; watch the `anyComponentStyle` budget output — meeting-conversation must stay well under 12 kB warn.

- [ ] **Step 5: Live smoke (mocked IPC)** — with the dev server running (`tauri-dev` skill if not), drive Playwright (MCP) against `http://localhost:1420` with a mocked `window.__TAURI_INTERNALS__.invoke`: type 2 note lines on the record screen, emit `meetnotes://status` `{stage:'summarizing'}`, and assert (a) the notes surface stays visible, (b) `.flow.is-enhancing` present, (c) hint reads "Enhancing your notes…"; then emit `{stage:'done'}` and assert the settled hint. Then repeat with ZERO notes and assert NO new classes/hints appear (pixel-parity gate).

- [ ] **Step 6: Commit**

```bash
git add src/app/core/meeting-conversation.store.ts src/app/features/record/record.component.ts src/app/features/record/meeting-conversation.component.ts
git commit -m "feat(record): Quiet-Weave enhance states + one-shot note sweep"
```

---

### Task 7: Detail view — ✨ badge, resummarize overlay + stale-guard

**Files:**
- Modify: `src/app/features/detail/detail.component.ts` (ParsedNote ~88, `parseNote()` ~2955, meta row ~159-184, sections wrapper, `resummarize()` ~2306; ~420 B CSS max)
- Modify: `src/styles.css` (global `.pill-enhanced`)

**Interfaces:**
- Consumes: the `murmur_enhanced: true` front-matter line (Task 4's marker contract), existing `busy` signal (set by `resummarize()`), existing `pill` global primitive, `note` computed (`parseNote(detail().note.markdown)`).
- Produces: `ParsedNote.enhanced: boolean`; the badge renders ONLY from parsed markdown (masked/locked meeting ⇒ `note.markdown` null ⇒ no badge ⇒ no leak — do NOT add any DTO-level field).

- [ ] **Step 1: Parse the marker** — in `detail.component.ts`:

`ParsedNote` interface (~88): add
```ts
  /** ENHANCE-MY-NOTES: true when the backend stamped `murmur_enhanced: true` (the note's
   *  skeleton was the user's typed notes). Derived ONLY from note.markdown — lock-safe. */
  enhanced: boolean;
```

In `parseNote()` (~2955): inside the front-matter branch (where `tags`/`participants` are read from `fm`), add
```ts
    let enhanced = false;
```
before the front-matter `if`, and inside it (next to the `readFrontMatterList` calls):
```ts
        enhanced = fm.some((l) => /^murmur_enhanced:\s*true\s*$/.test(l.trim()));
```
Then add `enhanced` to BOTH return literals of `parseNote` (the sections-empty `{ tags, participants, sections: [], raw: … }` and the final `{ tags, participants, sections, raw: null }`).

- [ ] **Step 2: The badge** — in the header meta row (~159-184), after the folder/lock badge `@if` block (this whole region already sits inside `@if (!locked())`-guarded content; the derivation is nil for masked notes regardless):
```html
              @if (note()?.enhanced) {
                <span class="meta-sep" aria-hidden="true">·</span>
                <span
                  class="pill pill-enhanced"
                  title="Your typed notes were used as the skeleton of this summary"
                >
                  ✨ Enhanced from your notes
                </span>
              }
```

Global pill styling — in `src/styles.css`, next to the other `.pill` variants:
```css
/* ENHANCE-MY-NOTES provenance badge (detail header). */
.pill-enhanced {
  color: var(--accent);
  border-color: color-mix(in srgb, var(--accent) 35%, transparent);
  background: color-mix(in srgb, var(--accent) 10%, transparent);
}
```

- [ ] **Step 3: Resummarize overlay + stale-guard** — in `detail.component.ts`:

Locate the container element that wraps the note-section cards (the parent of the `@if (n.sections.length) { @for (sec of n.sections; …) }` block at ~710). If it has a class, bind on it; if it is a bare `<div>`, name it: `class="note-sections"`. Add:
```html
              [class.is-resummarizing]="busy()"
```

Replace `resummarize()` (~2306) with the drop-late-responses guard (the closure id vs the currently-displayed meeting):
```ts
  async resummarize(id: string): Promise<void> {
    this.busy.set(true);
    this.msg.set("Re-summarizing…");
    try {
      await this.ipc.resummarize(id);
      const fresh = await this.ipc.getMeetingDetail(id);
      // Drop late responses: the user may have navigated (openRelated) mid-flight —
      // never clobber a different meeting's detail with this closure's re-fetch.
      if (this.detail()?.meeting?.id === id) {
        this.detail.set(fresh);
      }
      this.msg.set("Done.");
    } catch (e) {
      this.msg.set("Error: " + String(e));
    } finally {
      this.busy.set(false);
    }
  }
```

Component CSS — append (keep ≤ ~420 B; the detail styles block is the tightest compiled budget in the tree):
```css
      /* ── ENHANCE-MY-NOTES: re-summarize working overlay (dim + sweep) ── */
      .note-sections.is-resummarizing .card.section {
        opacity: 0.55;
        pointer-events: none;
        transition: opacity var(--transition);
        position: relative;
        overflow: hidden;
      }
      .note-sections.is-resummarizing .card.section::after {
        content: "";
        position: absolute;
        inset: 0;
        background: linear-gradient(
          100deg,
          transparent 35%,
          color-mix(in srgb, var(--accent) 8%, transparent) 50%,
          transparent 65%
        );
        background-size: 250% 100%;
        animation: resum-sweep 1.6s ease-in-out infinite;
      }
      @keyframes resum-sweep {
        from {
          background-position: 200% 0;
        }
        to {
          background-position: -150% 0;
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .note-sections.is-resummarizing .card.section::after {
          animation: none;
          background: none; /* the static dim alone signals working */
        }
      }
```
(This is the design's ONLY looping animation — mapped to a genuinely unknown duration. The finished-note reveal needs NO new code: the existing staggered section rise `[style.animation-delay.ms]="120 + i * 60"` carries it, and "Also discussed" rides it as one more card.)

- [ ] **Step 4: Verify with the FE gates**

```bash
npx ng lint && npx ng build
```
Expected: clean; confirm the `anyComponentStyle` output for detail.component stays under the 16 kB error budget (trim comments in the new block first if borderline).

- [ ] **Step 5: Commit**

```bash
git add src/app/features/detail/detail.component.ts src/styles.css
git commit -m "feat(detail): enhanced-note badge + resummarize overlay + stale-result guard"
```

---

### Task 8: Full verification, adversarial + lock-security review, PR

**Files:** none (verification + review + PR).

**Interfaces:**
- Consumes: everything above.
- Produces: the merge-ready PR. The verdict belongs to the **adversarial-verifier** and (REQUIRED — new egress of user-typed content) the **lock-security-reviewer**, NOT the implementer.

- [ ] **Step 1: Full gates**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib )
npx ng lint && npx ng build
bash scripts/ci.sh
```
Expected: all green (`ci.sh` runs clippy `-D warnings` + tests + lint + build + headless E2E — run ONCE here, not in the loop).

- [ ] **Step 2: Dispatch the adversarial-verifier agent** with this hunt list (RED-before-GREEN evidence for each):
  1. **Byte-identical / pixel-parity:** record with ZERO typed notes in BOTH modes → note output byte-identical to pre-branch (`git stash`-swap or golden-file diff), record screen screenshot-diff pixel-identical to today (the `showAssistant` gate must be the triple conjunction — stage-only degradation = FAIL).
  2. **Append-mode parity WITH notes:** output equals today's verbatim fold exactly.
  3. **Enhance egress redaction:** an email/phone in the typed notes never reaches the mock provider un-scrubbed (extend the Task 3 test angle live if possible).
  4. **Resummarize durability in enhance mode:** resummarize twice; notes still shape the note, NO duplicate sections, marker appears exactly once, the `manual_notes` buffer unchanged.
  5. **Fast-summarize interruption:** class drops mid-sweep — the 200 ms box-shadow transition must read soft, not a pop (live Chromium check).
  6. **Reduced-motion:** with reduce enabled, only static cues remain (inset edge, dim) — no sweep, no rise.
  7. **NG0600 / import-cycle / budget:** no console errors on boot, `ng build` budget output inspected.
  8. **T4 class:** the sweep/badge use `color-mix()` — render-check in Playwright **WebKit** (not just Chromium) since `ng serve` proves nothing for the packaged WKWebView.
- [ ] **Step 3: Dispatch the lock-security-reviewer agent** (REQUIRED gate) on: the new prompt egress of `user_notes` (redaction coverage, consent path, PII-in-logs), the marker (`murmur_enhanced` must not leak note EXISTENCE on a masked DTO — it lives inside `note.markdown`, null when masked), no new read/export path, the pipeline's ungated buffer read still shielded by the upstream `meeting_is_unlocked` gates, seal lifecycle untouched.
- [ ] **Step 4: Runtime honesty check** — boot the dev app (`tauri-dev` skill), record a short real meeting with 2-3 typed notes on the `claude_code` provider, confirm: the enhanced note reads as *your bullets expanded* (prompt-quality eyeball), `## Also discussed` present when expected, badge on detail, resummarize round-trips. Note honestly in the PR: enhance-quality on `ollama` + the packaged-WKWebView render need their own pass; Touch-ID/lock behavior needs a signed build.
- [ ] **Step 5: PR to the `murmur` trunk** (never direct-push):

```bash
git push -u origin feat/enhance-my-notes
gh pr create -R murmur-io/murmur --title "feat: enhance-my-notes — typed notes become the summary skeleton" --body "<summary of modes, egress justification, both reviewer verdicts, test evidence>"
```

---

## Copy reference (exact strings, all English)

| Surface | Condition | String |
|---|---|---|
| Notes-surface hint | recording ∧ enhance ∧ first persisted note exists | `✨ These bullets will shape your summary` |
| Notes-surface hint | summarizing ∧ enhance ∧ notes | `Enhancing your notes…` |
| Notes-surface hint | done ∧ enhance ∧ notes | `✨ Notes enhanced — your bullets became the outline.` |
| Rec-strip `hint()` | processing ∧ enhance ∧ notes | `Transcribing on-device, then enhancing your notes…` |
| Rec-strip `hint()` | done ∧ enhance ∧ notes | `Saved ✓ — your enhanced note is in the vault.` / `…in Murmur.` |
| Detail badge | `murmur_enhanced: true` parsed | `✨ Enhanced from your notes` (tooltip: `Your typed notes were used as the skeleton of this summary`) |
| Note section (backend prompt contract) | enhance, topics missed | heading exactly `## Also discussed` |
| Note section (append mode) | notes present | heading exactly `## My notes` (unchanged) |
| Settings label / options / help / footnote | — | see Task 5 template verbatim |
| Composer placeholder (drive-by fix) | — | `write a note… (@brain to open a thread)` |

## Known risks (tracked, accepted)

- **Prompt quality is provider-side** — the skeleton instructions need a manual A/B on 2-3 real (Polish-language) meetings on `claude_code` AND `ollama`; iterate the block wording in `template.rs` only (one place).
- **Unbounded buffer:** `manual_notes` has no length cap; a pathological buffer bloats the prompt (transcript already dominates — no cap added, YAGNI).
- **Accepted @brain drafts** are model-authored text the user endorsed — "the user's wording" in the prompt covers them too (they're indistinguishable in the buffer; by design).
- **FE saves are fire-and-forget** — a silently-failed `save_manual_notes` means the summarizer sees less than the screen showed (pre-existing behavior, unchanged).
- **`color-mix()`** requires WKWebView ≥ Safari 16.2 — fine for targets, but verify in the real engine (Task 8 / T4).
