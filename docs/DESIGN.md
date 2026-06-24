# MeetNotes — Design (v1)

> Nazwa robocza (do zmiany). Desktopowa apka, która nagrywa spotkanie, **transkrybuje lokalnie**, zamienia transkrypt w czystą notatkę przez **wymiennego providera AI** i zapisuje ją do vaulta Obsidian. Następca naszego setupu Meetily + `claude -p` + launchd.

**Status:** Design — do recenzji. **Data:** 2026-06-24.

## 1. Cel
Własna, dystrybuowalna apka (dla nas + innych), która ma cały flow pod kontrolą: capture → transkrypcja lokalna → notatka AI → Obsidian. Uniezależnienie od Meetily, transkrypcja zostaje lokalna/prywatna, krok AI jest wymienny.

## 2. Poza zakresem v1
- Brak hosted/backendu i płatności (interfejs zaprojektowany pod to; implementacja później).
- Tylko macOS (Windows później — inny capture audio).
- Brak diaryzacji mówców (v2).
- Brak transkrypcji live — transkrybujemy po Stop (live = v2).
- Brak sync zespołowego / chmury. Local-first.

## 3. Decyzje (zatwierdzone)
- Stack: **Tauri** (rdzeń w Rust + UI web/TS).
- Dystrybucja: produkt dla innych; v1 bez backendu.
- Providerzy v1 (za jednym interfejsem): **Ollama (lokalny LLM)**, **lokalny Claude Code (`claude -p`)**, **Anthropic/Claude (BYO-key)**.
- Referencja dla capture+Whisper: **Meetily** (open-source Tauri; bundluje ffmpeg + llama.cpp + Ollama; capture audio systemowego przez ScreenCaptureKit, bez wirtualnego urządzenia).

## 4. Architektura
```
┌───────────────────────────── UI (web / TS) ─────────────────────────────┐
│  Record · Library · Detail (transkrypt+notatka) · Settings · Onboarding  │
└───────────────▲───────────────────────────────────────────▲─────────────┘
                │  komendy Tauri (IPC)                        │ zdarzenia/postęp
┌───────────────┴───────────────── Rdzeń (Rust) ─────────────┴─────────────┐
│  Capture ──► Transcribe ──► Summarize(Provider) ──► Export(Obsidian)      │
│     │             │                  │                      │            │
│  Storage (SQLite) ◄──────────────────┴──────────────────────┘            │
│  Settings / Secrets (macOS Keychain)                                     │
└──────────────────────────────────────────────────────────────────────────┘
```

## 5. Komponenty

### 5.1 Capture (Rust)
- Mikrofon (CoreAudio / `cpal`) + audio systemowe (**ScreenCaptureKit**, macOS 13+; bez wirtualnego urządzenia — jak Meetily).
- Miks do **16 kHz mono WAV** przez `ffmpeg` (format lubiany przez Whisper). Opcja dwóch ścieżek zostawiona pod przyszłą diaryzację.
- Uprawnienia: **Screen Recording + Microphone** — onboarding + łagodna obsługa odmowy.
- Referencja: kod capture Meetily.

### 5.2 Transkrypcja (Rust)
- whisper.cpp przez `whisper-rs`, akceleracja Metal. Tryb **batch** (po Stop).
- Zarządzanie modelem: pobranie GGUF przy 1. starcie; user wybiera rozmiar (base/small/medium) — szybkość vs jakość.
- Wynik: segmenty `[{start,end,text}]` + pełny tekst → do storage.

### 5.3 Summarizer Provider (serce „opcji")
Krok „notatka" to tekst→tekst, więc dowolny LLM go zrobi. Jeden interfejs, wymienne implementacje:

```rust
struct SummarizeRequest {
    transcript: String,
    meta: MeetingMeta,        // data, hint tytułu, czas, język
    template: String,         // nasz format notatki (prompt do Obsidian)
    vault_titles: Vec<String> // istniejące notatki → cele [[linków]]
}

#[async_trait]
trait SummarizerProvider {
    fn id(&self) -> &str;
    fn availability(&self) -> Availability;          // klucz ustawiony? ollama żywa? claude w PATH?
    async fn summarize(&self, req: &SummarizeRequest) -> Result<String>; // gotowy markdown
}
```

Implementacje v1:
- **OllamaProvider** — HTTP `localhost:11434`, model GGUF lokalnie. Offline, prywatne, bez klucza.
- **ClaudeCodeProvider** — spawn `claude -p` z **hermetycznymi flagami** (`--system-prompt`, `--disallowedTools`, walidacja „wyjście zaczyna się od `---`") — lekcje z naszego automatu. Wygoda dla nas/power-userów.
- **AnthropicProvider** — REST `api.anthropic.com`, klucz z Keychain, model `claude-opus-4-8`. (Brak oficjalnego SDK dla Rusta → surowe HTTPS to sankcjonowana ścieżka.)

Zaprojektowane tak, by **HostedProvider** (proxy+subskrypcja) oraz OpenAI/Groq/Gemini dało się dołożyć później bez ruszania reszty. `availability()` steruje UI (co pokazać/wyszarzyć).

### 5.4 Storage (Rust, SQLite)
- `meetings(id, started_at, ended_at, title, duration_s, audio_path, status)`
- `segments(meeting_id, idx, start_s, end_s, text)`
- `notes(meeting_id, provider_id, markdown, created_at, exported_path)`
- `settings(key, value)`

### 5.5 Eksport do Obsidian
- Zapis `.md` do skonfigurowanego folderu vaulta; nazwa `YYYY-MM-DD HHmm - <tytuł>.md`; nasz szablon + frontmatter. Idempotentnie (bez dublowania). Obsidian = folder `.md`, plik pojawia się sam.

### 5.6 UI (web/TS)
- **Record** — start/stop, wskaźniki poziomu, status.
- **Library** — lista z SQLite.
- **Detail** — transkrypt + notatka; „przerób innym providerem", „eksportuj ponownie".
- **Settings** — provider + dane (klucz), model Whisper, język, ścieżka vaulta, edytor szablonu.
- **Onboarding** — uprawnienia.

## 6. Ustawienia i sekrety
- Klucz API w **macOS Keychain** (nigdy w configu plaintext, nigdy w bundlu).
- Wybór providera + konfiguracja per provider.

## 7. Dystrybucja (faza późniejsza)
- Apple **Developer ID** signing + **notaryzacja**; auto-update (Tauri updater).
- Windows później (WASAPI loopback dla audio systemowego).

## 8. Prywatność
Local-first: audio + transkrypt zostają na maszynie. Co opuszcza maszynę zależy od providera: **Ollama / Claude Code = nic nie wychodzi**; **Anthropic BYO-key = transkrypt idzie do API Anthropic**. Pokazać to wprost w UI.

## 9. Flow end-to-end
Record → Stop → miks WAV → Whisper → transkrypt do storage → Summarize wybranym providerem (+ tytuły z vaulta) → notatka do storage → eksport `.md` do vaulta.

## 10. Fazy budowy (walking skeleton najpierw, kolejność wg ryzyka)
- **Faza 0 — szkielet:** Tauri app, capture **tylko mikrofonu** → WAV → Whisper → **jeden provider** (najszybszy do podpięcia — pewnie ClaudeCode, bo pipeline już mamy) → `.md` do vaulta. Dowodzi E2E.
- **Faza 1 — providerzy:** dołóż Ollama + Anthropic za traitem; Settings do przełączania; `availability()`.
- **Faza 2 — audio systemowe:** ScreenCaptureKit, miks mic+system, onboarding uprawnień. (Najtrudniejsze — osobno.)
- **Faza 3 — UX produktu:** Library/Detail (SQLite), edytor szablonu, wybór modelu Whisper, re-summarize/re-export.
- **Faza 4 — dystrybucja:** signing, notaryzacja, auto-update.
- **Później:** hosted tier, OpenAI/Groq/Gemini, diaryzacja, transkrypcja live, Windows.

## 11. Ryzyka / pytania otwarte
- **ScreenCaptureKit z Rusta** — dojrzałość bindingów; może być potrzebny mały shim Swift/ObjC albo gotowy crate. Największe ryzyko; de-ryzykowane referencją Meetily.
- `whisper-rs` rozmiar modelu vs latencja na słabszych Makach.
- ClaudeCodeProvider a komercyjne ToS — OK jako opcja osobista/dev, nie domyślna ścieżka dystrybucji.
- Jakość notatki między providerami (lokalny LLM < Claude) — ustawić oczekiwania per provider w UI.

## 12. Podsumowanie wyborów technicznych
| Obszar | Wybór |
|---|---|
| Powłoka | Tauri (Rust + WebView) |
| UI | web/TS |
| Capture | ScreenCaptureKit (system) + cpal (mic), miks ffmpeg |
| Transkrypcja | whisper.cpp via `whisper-rs` (Metal), batch |
| AI | trait `SummarizerProvider`: Ollama · Claude Code · Anthropic (REST) |
| Storage | SQLite |
| Sekrety | macOS Keychain |
| Eksport | `.md` do vaulta Obsidian |
| Dystrybucja | Developer ID + notaryzacja + Tauri updater (faza 4) |
