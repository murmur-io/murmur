<!-- Design spec — 2026-06-30. Brainstormed + approved (shape + inline layout). -->
# Unified in-meeting assistant — merge the chat panel into the card

**Goal:** Collapse the two overlapping in-meeting AI surfaces (the quick **Assistant card** and the slide-out **Chat panel**) into ONE surface: a single conversation thread, fed by both voice and text, with multi-turn memory, that clears when a new recording starts.

**Why:** During a live meeting the user sees two things that both say "ask the AI about this meeting + your vault" — decision fatigue + visual clutter. The panel's only real edge over the card is multi-turn memory + a readable thread; the card's edge is the voice orb + quick one-shot. Merge keeps both edges in one place.

## Current state (the duplication)

- `src/app/core/assistant.store.ts` — `AssistantStore`: voice orb (wake / listening / processing / `orbState`) + one-shot text (`askText` → `ask_assistant_text`). Holds a **newest-first, capped (12) `interactions` list, NO conversation memory** (each turn is independent; the backend `run_assistant_query` is one-shot). Exports the shared types `AssistantCitation`, `ToolTraceStep`, `parseCitations`.
- `src/app/core/meeting-chat.store.ts` — `MeetingChatStore`: a **chronological multi-turn `messages` thread WITH memory** (each `send` ships the full history to `ask_assistant_chat`), text-only, plus the slide-out `open/close`.
- `src/app/features/record/assistant-actions.component.ts` — the card UI (orb + composer + the newest-first interaction list + trace chips).
- `src/app/features/record/meeting-chat-panel.component.ts` — the slide-out panel UI + a FAB to open it.
- `src/app/features/record/record.component.ts` — renders BOTH (`@if (showAssistant())` card at ~280, `@if (showChat())` panel at ~367; wiring at 1182-1243), `chat.init()` + `assistant.init()` in `ngOnInit`.

## Target architecture

### One conversation store (`AssistantStore` absorbs the thread; `MeetingChatStore` is deleted)
A single store owns BOTH the unified conversation thread AND the voice input-state:
- **Conversation (from the chat store):** a chronological `messages: signal<ChatMessage[]>` thread (oldest → newest), `hasMessages`, and a `send(text)` that ships the **full clean history** to `ask_assistant_chat` (multi-turn memory) and resolves the in-flight assistant bubble. (Lift `MeetingChatStore.send` / `onTool` / the `ChatMessage` shape verbatim.)
- **Voice input (kept from the assistant store):** `askNow()` / `endAsk()` (begin/end voice command), `listening` / `processing` / `manualAskInFlight` / `orbState`, and the `onWake` / `onListening` / `onProcessing` / `onResult` listeners. **Change:** `onWake` appends a `user` bubble (the heard command, `status:"pending"`-paired with an assistant bubble) to the SAME `messages` thread; `onResult` resolves that assistant bubble (summary + citations) — voice turns now live in the one thread, not a separate list.
- **Tool trace:** the in-flight (last pending) assistant bubble accretes chips from the tool-trace stream. The unified store subscribes to **both** existing events (`EVENT_ASSISTANT_TOOL` from the voice path, `EVENT_CHAT_TOOL` from `ask_assistant_chat`) and routes every chip to the last pending bubble — **no backend event change** (both fire into the one thread). The shared types `AssistantCitation` / `ToolTraceStep` / `parseCitations` move into this store (or `core/models.ts`).
- `clear()` empties `messages`.

### One surface, inline (the approved layout)
`assistant-actions.component.ts` becomes the single surface in the card's home (recording screen, bottom region):
- A **scrollable thread** (oldest at top, newest at bottom) of user/assistant bubbles with trace chips + citation chips, reusing the chat panel's bubble rendering. `afterNextRender` auto-scroll to the newest bubble (the existing pattern in the panel).
- **Input pinned at the bottom:** the voice orb / mic button (`askNow`/`endAsk`) + the text composer (`send`) side by side.
- **Deleted:** `meeting-chat-panel.component.ts`, its FAB, and the `showChat()` / panel block + `MeetingChatStore` wiring in `record.component.ts`. The card is shown whenever the assistant is enabled while recording (the existing `showAssistant()` condition, minus the "has any interaction" gate — the thread is always the home now).

### Both inputs → one thread, with memory
- **Text:** composer → `send(text)` → `ask_assistant_chat(fullHistory)` → full multi-turn memory. (already works)
- **Voice:** the mic/wake path answers one-shot via the existing voice backend (now WITH the 0.6.2 live-transcript context), and its turn (heard command + answer + trace + citations) is appended to the thread. Because every text `send` ships the whole thread, a typed follow-up remembers what was asked by voice.
- **Accepted trade-off (explicit):** a VOICE turn itself stays one-shot — it does not replay prior chat turns to the brain (the voice backend path is single-shot). Deliberate multi-turn follow-ups are text; the live-transcript already gives voice the meeting context. (Full voice-with-history memory = out of scope, a later option requiring routing voice through `ask_assistant_chat`.)

### Clear on new recording
On recording **start** (the `RecorderStore` stage transition to recording, observed via an `effect` in `record.component` or the store), call `assistant.clear()` so a new meeting opens with an empty thread. This mirrors the backend's per-recording live-transcript clear (0.6.2, `transcribe/live.rs::run`).

### Backend
**No Rust changes at all.** `ask_assistant_chat`, `ask_assistant_text`, the voice path (`begin/end_voice_command` → `EVENT_VOICE_ACTION_RESULT`), and both tool-trace events all stay exactly as-is — this is a pure FE consolidation.

## Constraints (binding — Murmur FE rules)
- Zoneless / signals-only: all state stays `signal`/`computed`; `asReadonly()` public; no NgRx, no `subscribe`-into-a-field, no `async` pipe. Event streams subscribed once in `init()`, unlisten on teardown.
- `@if`/`@for track id`; `afterNextRender(fn, {injector})` for the auto-scroll (never `setTimeout`/`rAF` in the component); `input()`/`output()`/`viewChild()` signal APIs.
- The thread surface is in-flow (the frosted `.card` is fine here — it is NOT a floating popover). The deleted panel WAS a floating overlay (opaque `--surface-overlay`); the inline thread does not need that.
- Tokens for all colours/spacing/radii; 16 kB per-component style budget.
- No new npm packages.

## Testing / DoD
- `npx ng lint` + `npx ng build` green (incl. the style budget).
- Playwright smoke against `:1420` with a mocked `window.__TAURI_INTERNALS__.invoke`: a text turn appends user+assistant bubbles and a follow-up ships prior history; a mocked `EVENT_VOICE_ACTION_RESULT` lands a voice turn in the SAME thread; a mocked recording-start clears the thread.
- Adversarial-verifier owns PASS/FAIL; hunt the FE failure modes (NG0600 on a signal write in an effect — the clear-on-record effect needs `{allowSignalWrites:true}`; import-cycle `ɵcmp`; opacity bleed is N/A now).
- No backend behavior change → `cargo test --lib` unaffected (run once at the end via `scripts/ci.sh`).

## Out of scope
- Voice-with-full-history memory (route voice through `ask_assistant_chat`).
- Post-meeting / detail-view chat (this is the in-meeting surface only).
- Any change to the brain, tools, redaction, or the live-transcript injection (0.6.2).
