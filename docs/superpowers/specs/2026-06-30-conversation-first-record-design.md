<!-- Design spec — 2026-06-30. Brainstormed + approved (variant 1 "Rozmowa-first" + agent-decided actions). -->
# Conversation-first record screen + agent-decided actions

**Goal:** Consolidate the in-meeting assistant thread + the "My notes" field + `@brain` into ONE conversation-first surface on the record screen, and give the in-meeting agent WRITE capability so it DECIDES (model-driven, NO hardcoded regex) whether to answer or take an action (save a note, create a follow-up). This makes future actions free — each new action is just a gated tool the agent can choose, not a hardcoded branch.

## The surface (record screen, variant 1)
- **Slim recording bar (top):** orb + timer + level meter + the **live caption** ticker + **Stop**. Recording becomes ambient status, not the hero.
- **The conversation thread (fills the rest):** ONE chronological thread that is BOTH your notes AND the agent conversation. One input pinned at the bottom.

## Interaction model (one input; @brain is the only signal — the agent decides the rest)
- A line **without `@brain`** = **your note** → persisted via the existing `save_manual_notes` (Feature A) → shown as a note bubble → feeds the live brain context + folds into the finalized note (all already wired).
- A line **with `@brain`** = an agent invocation → the **agentic loop (now allow_writes)** → the agent DECIDES:
  - **answer** ("@brain what is this meeting about?") — informational, as today; or
  - **act** ("@brain save that I send the deck to Anna") — the agent CHOOSES a write tool (save-note / create-follow-up) and runs it → an action-confirmation bubble ("✓ saved a follow-up"). **No keyword/regex** decides this — the model does, via tool-use.
- **Voice** funnels into the same thread (the existing voice path → the same loop).

## Backend changes (focused; reuse everything already built)
1. **`run_informational` (transcribe/live.rs):** flip the in-meeting `GatedToolExecutor` to **`allow_writes: true`** so the agent can call write tools in the in-meeting loop.
2. **Remove the hardcoded intent classifier routing** in `run_assistant_query` (transcribe/live.rs): today `resolve_command_intent` → `CreateReminder`/`NoteAside` → `handle_voice_action` (deterministic write). Replace with: **every** assistant request (voice + text + @brain) routes through the agentic loop (with writes); the **agent decides**. `handle_voice_action` is demoted to the **informational FLOOR** only (loop returns `Ok(None)` / `Err` / no cloud consent / local-backend) — it no longer owns write routing. A write the agent can't complete fails gracefully ("couldn't do that"), never a hardcoded guess. (Keep the deterministic floor for the no-consent/local case so there is no regression for read answers.)
3. **Write tools (tools.rs):** the agent's **save-note** tool appends to `manual_notes` (the durable, sealed, folds-into-the-note home from Feature A) — NOT the orphaned `notes_asides`. The **follow-up/reminder** tool stays (the existing `reminder` write tool). Both must be ADVERTISED to the model (in `tool_specs`, `write: true`) AND executed only through the gated executor (write only to an UNLOCKED meeting; a sealed meeting refuses). Confirm `GatedToolExecutor` with `allow_writes` advertises + gates the write tools.
4. **No new command** for note-lines — the FE reuses `save_manual_notes` (Feature A) for non-@brain lines.

## Frontend changes
- **`record.component.ts`:** rebuild to conversation-first. The slim recording bar (existing orb/timer/level/live-caption + Stop) sits on top; the **unified assistant thread** (the merged `AssistantStore` from PR #93) becomes the full-height main surface and now ALSO holds the user's note lines. **Remove** the separate "My notes" textarea (`meeting-notes.component`) and the standalone assistant card chrome — fold both into the one thread.
- **One input:** on submit, if the line contains `@brain` → send to the agent (existing `askAssistantText` / the unified store's `send`); otherwise → `saveManualNotes` (append) + show a note bubble in the thread. The thread renders: your note bubbles, your @brain questions, agent answers, and agent action-confirmations (distinct styling/attribution).
- Keep the existing `@brain` autocomplete affordance (from Feature A) as the way to mark an agent line.

## Reuse (nothing thrown away)
The agentic brain (`agent.rs` + `GatedToolExecutor` + `run_agentic_loop`), the write tools (`reminder` exists; save-note adapts to `manual_notes`), `manual_notes` persistence + seal-and-restore + fold-into-note + live-brain injection (Feature A), the unified thread store (PR #93), and the redaction/consent firewall all stay. This is mostly: UI → conversation-first, and the agent → allowed to write + decide.

## Constraints (binding)
- Backend: `AppError`/`Result`; gate every write (unlocked meeting only) through the executor; redaction + consent unchanged (the loop already routes through them); verify-before-destroy unchanged (manual_notes seal already correct); no PII in logs. `cargo test --lib` loop.
- FE: zoneless — standalone + OnPush + signals; `@if`/`@for track id`; `afterNextRender`; `var(--token)`; ≤16 kB budget; NO new npm deps; opaque overlays (T3); NG0600 guards.

## Testing / DoD
- Backend (cargo test --lib, RED-before-GREEN where it's a behavior change): the in-meeting executor with `allow_writes` ADVERTISES + EXECUTES the write tools (gated — a write to a sealed meeting refused); the save-note tool appends to `manual_notes`; the dispatch routes through the loop (the intent classifier no longer owns write routing — a "save" phrase is NOT hardcoded-matched; verify by removing the classifier and asserting the loop path is taken); the informational FLOOR still answers when the loop can't run (no-consent / local). The actual model DECISION (answer vs act) needs a live/cloud run — assert the plumbing, not the model's choice.
- lock-security-reviewer (write capability in the in-meeting loop → mandatory): every agent write is gated to an unlocked meeting; no ungated write/leak; the save-note → manual_notes path stays sealed-and-restored.
- FE: `ng lint` + `ng build` green; adversarial-verifier live mocked-IPC smoke — a note line saves + shows; an `@brain` line routes to the agent; an agent answer + an action-confirmation render distinctly; the recording bar + thread coexist.
- Full `scripts/ci.sh` before merge. Batch to trunk, no version bump, no release.

## Out of scope
- The `/brain` knowledge-sources page redesign (separate, paused on `feat/brain-view-redesign`).
- Voice questions replaying full prior chat history (the voice path stays one-shot per turn; it still lands in the thread).
- New action tools beyond save-note + follow-up (the architecture now makes adding them trivial — later).
