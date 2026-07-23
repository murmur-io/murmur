# IPC payload patterns (Tauri event vs Channel, payload size, the lock trap)

Deep reference for `/app-perf-audit`. The Rust/Tauri ruleset is `.codex/rules/rust-tauri.md` (§2
commands/events) and the lock invariants are `.codex/rules/lock-model.md` — this file is the
**performance-specific** view of the Rust→FE data seam: when `app.emit` is the wrong tool, how to
window a large read, and the lock trap you must never open while making a read faster. Cite by SYMBOL,
not line number.

## How the seam works today

**Backend → FE** is entirely `app.emit(EVENT, payload)` plus one-shot command return values. The typed
event constants live in `src-tauri/src/events.rs` (`EVENT_STATUS = "meetnotes://status"`,
`EVENT_LIVE_CAPTION = "murmur://live-caption"`, `EVENT_WHISPER_CARD`, `EVENT_PROACTIVE_HINT`,
`EVENT_RECORDING_CAPPED`, …). The high-frequency emitters:

- `src-tauri/src/transcribe/live.rs` — live captions (`EVENT_LIVE_CAPTION`), whisper reaction cards
  (`EVENT_WHISPER_CARD`), proactive hints (`EVENT_PROACTIVE_HINT`), wake/voice-action results — every
  live-loop tick.
- `src-tauri/src/pipeline.rs::emit_status` — `EVENT_STATUS` `StatusPayload` at each post-Stop stage.
- `src-tauri/src/audio/listener.rs` — `VOICE_START_EVENT`.

**FE** side is `src/app/core/ipc.service.ts`: one `invoke`-wrapping method per command, and
`listen<T>(EVENT, cb)` for each stream (`onStatus`, `onLiveCaption`, `onWhisperCard`, …), each returning
`Promise<UnlistenFn>` kept for teardown. Polled scalars (`level`, `elapsed`) are the exception — bridged
via `toSignal`+`interval` in `recorder.store.ts`, not events.

## What `app.emit` actually costs

Every `app.emit` **JSON-serializes the payload** and dispatches it into the webview, where Tauri
`eval`s it to deliver the event. For a small scalar/boolean at a low rate (a status transition, a
"recording capped" notice) that is free. It becomes a problem on two axes:

1. **High frequency** — a payload emitted many times per second (a live scalar you're polling, an audio
   level, a token stream). Each emit pays the serialize + eval tax; at rate it steals main-thread time
   and floods the event listener.
2. **Large payload** — shipping a whole array (the entire transcript, a full segment list, a big graph)
   in one emit or one command return. The serialize cost scales with payload size, the webview holds a
   full copy, and it spikes RSS (feeds the OOM anatomy — see rust-profiling-recipe).

**There is no `tauri::ipc::Channel<T>` anywhere in the tree today** (grep confirms zero hits). Channel
is the right tool for a genuinely high-frequency or streaming backend→FE channel: it delivers typed
messages over a dedicated IPC channel handle without the per-message global-event serialize+eval, and
the FE consumes it as a stream. If a new feature needs to stream many messages (a token-by-token
generation surface, a fine-grained progress feed), design it on `Channel<T>` from the start rather than
hammering `app.emit`. (Adding `Channel` is stdlib Tauri, not a new dependency — no approval needed.)

**Rule of thumb:** an event is for a discrete state change the FE reacts to; a Channel is for a stream.
A poll loop that emits a scalar every tick is neither — prefer the `toSignal`+`interval` bridge already
used for `level`/`elapsed` (the FE owns the cadence; the backend exposes a cheap getter command),
which is why those two are NOT events.

## Windowing / paginating a large read

Do NOT ship the whole transcript in one command return when the FE renders a window of it. Two layers:

- **Backend:** a read command that returns a bounded slice (offset/limit or a "since cursor") instead of
  the full array. Any such command stays registered in the `generate_handler!` in `lib.rs` and returns
  `AppError`/`Result`. **The lock gate is non-negotiable in the paginated version too** — the slice still
  routes through `meeting_is_unlocked` (content commands) or `visibility_clause` / the `*_visible`
  helpers (`db.rs` / MCP). A faster read that skips the gate is a leak, not an optimization.
- **Frontend:** window what you render even if you already have the array (the `RENDER_CAP = 80` pattern
  in `audio-panel.component.ts` — see fe-cd-checklist). Rendering fewer DOM nodes cuts both RSS and the
  per-pass CD walk. Reach for a `RENDER_CAP` window before `@angular/cdk` virtual scroll (a new dep needs
  explicit user approval — there is no `@angular/cdk` in `package.json`).

The transcript that actually feeds an on-device model is bounded on the BACKEND by uniform decimation in
`src-tauri/src/summarize/timeline.rs` (NOT head-truncation — the decimated transcript must still SPAN
the whole meeting; a cloud provider gets no cap). That is the model-input bound; the IPC/render bound is
separate and additional.

## The DTO-shape-for-lock trap (never open it for speed)

The single most dangerous thing to get wrong while reshaping a read for performance: the masked DTO for
a sealed-and-not-session-unlocked meeting **must carry no on-disk audio path**. The FE feeds
`meeting.audioPath` straight into Tauri's `convertFileSrc` (the `asset:` protocol, scoped to the audio
dir) WITHOUT going through any backend command or the `meeting_is_unlocked` gate — it is the one audio
read path that bypasses the gate. The masked DTO sets `audio_path: None` on purpose
(`src-tauri/src/commands/mod.rs`, the masked-detail path; grep `audio_path: None`), and the FE's
`src/app/features/detail/detail/detail.component.ts` `audioSrc` computed honors it:

```ts
readonly audioSrc = computed(() => {
  const path = this.detail()?.meeting.audioPath;
  return path ? convertFileSrc(path) : null;   // null path ⇒ no asset URL ⇒ no leak
});
```

If a perf refactor of the detail read (caching, a new lighter DTO, a paginated variant) ever lets a real
on-disk path survive in the masked shape, `convertFileSrc` will serve a plaintext WAV for a locked
meeting. `export_audio` also fails closed on `!meeting_is_unlocked` (`AppError::Locked`). **Any read
refactor is a lock-touching change → `lock-security-reviewer` is a required gate**, and the
adversarial hunt (masked DTO = `locked: true`, `note: null`, `segments: []`, `audioPath: null`) must
pass on the new shape.

## No PII on the seam

An event/DTO/metric carries IDs, stage names, counts, durations — never note/transcript text, titles,
attendee names, or a path that embeds content. The existing payloads follow this (e.g.
`EVENT_PROACTIVE_HINT` ships IDs + a SHORT title from an already-VISIBLE row only). A new
performance-instrumentation event/log obeys the same bar.

## Checklist when reshaping the seam

1. Is this stream high-frequency or large? → `Channel<T>`, not repeated `app.emit`; or poll via a cheap
   getter + `toSignal`+`interval` on the FE.
2. Is the FE rendering a window of a big array? → bound the read (backend slice) AND window the render
   (`RENDER_CAP`).
3. Does the read return meeting content? → it stays gated (`meeting_is_unlocked` /
   `visibility_clause`); the masked shape carries `audioPath: null`; no on-disk path in a locked DTO.
4. New command? → registered in `generate_handler!` in `lib.rs`, one typed method in `ipc.service.ts`,
   `T` declared in `core/models.ts`, `AppError`/`Result`.
5. New payload/metric? → IDs/stages/counts/durations only, no PII.
6. Lock-path touched? → `lock-security-reviewer`. Perf-budget verdict → `adversarial-verifier`.
