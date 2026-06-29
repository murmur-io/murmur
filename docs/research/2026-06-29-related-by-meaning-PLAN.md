<!-- Ship-feature plan, 2026-06-29. Wynik /research → docs/research/2026-06-29-embedding-visualization-graph-tab.md (Opcja A). -->
# Plan: "Powiązane wg znaczenia" (semantic related meetings) — Opcja A

> **Decyzja (z researchu):** zamiast atlasu/scatter embeddingów w Grafie (gadżet, zły timing) budujemy
> realny user-value: semantycznych sąsiadów spotkania/encji, jako sekcję w detalu. Reużywa już-istniejący
> **gated** `search_semantic_visible`. Zero nowego widoku, zero projekcji 2D, zero nowych zależności.

## Prerequisite (bramka — NIE mój krok, Twój @Mac)

**Najpierw bake-off (`docs/RAG-BAKEOFF.md`).** Tej funkcji nie warto shipować, jeśli e5 nie bije już-shippnięte FTS5
na Twoim realnym vaulcie — wtedy "powiązane wg znaczenia" to polerowany szum. Headless tego nie rozstrzygnie
(bez pobranego modelu działa StubEmbedder = hash-bag). Bake-off wymaga: `npm run dev` z pobranym modelem e5,
`semantic_search_enabled=true`, reindex, ~15–20 pytań ze scoringiem. **To uruchamiasz Ty na Macu.**
Jeśli e5 wygrywa → budujemy poniższe. Jeśli nie → odpuszczamy i wektory zostają tylko pod Ask.

Status faktów (zweryfikowane w kodzie 2026-06-29):
- Embedder zawsze kompilowany; prawdziwy e5 aktywuje się na obecność modelu (`embed.rs:146-160`, `Cargo.toml:147`). ✅
- `search_semantic_visible(&qvec, k, unlocked) -> Vec<SearchHit>` istnieje, gated, dedup po spotkaniu (`db.rs:937-1005`). ✅
- `semantic_search_enabled` domyślnie `false` (`config.rs:223`). ✅
- `SearchHit { meeting, snippet, matched_in }` (`storage/models.rs:233`). ✅

---

## Co budujemy (zakres MVP)

W **detalu spotkania** (`src/app/features/detail/`) — sekcja **"Powiązane wg znaczenia"**: lista do K (np. 5)
innych spotkań najbliższych semantycznie bieżącemu, z tytułem + datą + krótkim snippetem, klik → nawigacja
do tamtego spotkania. Widoczna tylko gdy `semantic_search_enabled` jest ON i jest model (inaczej sekcja ukryta —
nie pokazujemy sąsiadów ze stuba).

Świadomie POZA zakresem MVP: scatter/atlas 2D, klastry tematów, sąsiedzi encji w Grafie (osobna iteracja jeśli ta się sprawdzi).

---

## Backend (Rust) — `rust-tauri-dev`

### 1. `Db::related_meetings_visible` (`storage/db.rs`)
Cienki wrapper nad istniejącą gated KNN. Wektor zapytania = centroid chunków bieżącego spotkania
(albo embed jego notatki), KNN, **wyklucz samo spotkanie**, zwróć top-K.

```rust
/// Spotkania najbliższe semantycznie `meeting_id`, GATED przez `visibility_clause` (reużywa
/// `search_semantic_visible`). Wektor zapytania = średnia (centroid) embeddingów chunków tego
/// spotkania z `vec_chunks` (już tylko-widoczne, purge-on-lock). Wyklucza samo `meeting_id`.
/// Zwraca [] gdy spotkanie nie ma chunków (brak modelu / nie zaindeksowane) — NIGDY nie panikuje.
pub fn related_meetings_visible(
    &self,
    meeting_id: &str,
    k: i64,
    unlocked: &HashSet<String>,
) -> Result<Vec<SearchHit>> { /* centroid z vec_chunks WHERE meeting_id=?, potem search_semantic_visible(k+1), filtruj self */ }
```

Uwagi:
- **Centroid czytamy z `vec_chunks` danego spotkania** (te wektory już są tylko-widoczne i purgowane przy lock),
  uśredniamy, L2-normalizujemy (e5 = unit vectors). To NIE wymaga ponownego embedowania ani modelu w tej ścieżce.
- KNN pytamy o `k+1` i odfiltrowujemy bieżące `meeting_id` (zawsze będzie najbliższe samo sobie).
- Jeśli bieżące spotkanie samo jest locked/niewidoczne — wrapper wywoływany tylko z odblokowanego detalu;
  mimo to `search_semantic_visible` gatuje wynik, więc sąsiedzi z zablokowanych folderów nie wyciekną.

### 2. Komenda `related_meetings` (`commands.rs` + rejestr w `lib.rs`)
```rust
#[tauri::command]
pub async fn related_meetings(state: State<'_, AppState>, meeting_id: String) -> Result<Vec<SearchHit>> {
    let cfg = AppConfig::load(&state.db)?;
    if !cfg.semantic_search_enabled { return Ok(Vec::new()); }   // OFF → pusto, sekcja się nie pokaże
    let unlocked = state.unlocked_folders.lock().clone();
    state.db.related_meetings_visible(&meeting_id, 5, &unlocked)
}
```
- **Dodać do `generate_handler![]` w `lib.rs`** (inaczej IPC undefined — reguła rust-tauri §2).
- Błędy przez `AppError`/`Result` (§1). Logi: tylko id/count, **zero treści** (§8).

---

## Frontend (Angular zoneless) — `angular-zoneless-dev`

### 3. `ipc.service.ts` — jedna typowana metoda
```ts
relatedMeetings(meetingId: string): Promise<SearchHit[]> {
  return invoke('related_meetings', { meetingId });
}
```
Typ `SearchHit` w `core/models.ts` (jeśli go tam nie ma — sprawdzić; backend ma `{ meeting, snippet, matchedIn }`).

### 4. Sekcja w detalu spotkania (`src/app/features/detail/…`)
- Stan w signalach: `related = signal<SearchHit[]>([])`, `loadingRelated = signal(false)`.
- Pobranie wzorem **`effect()` + `await ipc` → signal ze stale-guard** (wzór `entity-detail.component.ts:305-336`),
  re-fetch przy zmianie `meetingId()` ORAZ przy zmianie lock-state (jak `graph.component.ts:512-521`,
  `{ allowSignalWrites: true }` — trap T1).
- Render: `@if (related().length)` → lista chipów/kart (reużyj istniejący `app-sources` z grafu jeśli pasuje),
  `@for (r of related(); track r.meeting.id)`. Klik → nawigacja do spotkania (istniejący mechanizm detalu).
- Pusto/OFF → sekcja w ogóle się nie renderuje (`@if`). Snippet w hoverze/overlayu → opaque `var(--surface-overlay)` (trap T3).
- Wszystko: OnPush, inline template+styles, `var(--token)`, bez `setTimeout`/nowych npm.

---

## Testy / weryfikacja (Definition of Done)

### RED-przed-GREEN (lock-security, wymagane)
Test gatingu wzorowany na `vec_semantic_search_is_gated_by_visibility` (`db.rs:4173`):
- Korpus: 2 widoczne spotkania semantycznie bliskie + 1 **sealed** (locked, nie-session-unlocked) bliskie.
- Assert: `related_meetings_visible(open_id, 5, &empty_set)` **NIE** zwraca sealed spotkania (ani jego snippetu);
  po dodaniu jego folderu do unlocked set — zwraca. RED gdyby gate zdjąć.
- Drugi test: wyklucza samo `meeting_id`; zwraca [] gdy brak chunków.

### Bramki
- `cargo test --lib` (NIE `clippy --all-targets` w pętli), `npx ng lint`, `npx ng build`, na końcu `bash scripts/ci.sh`.
- **`adversarial-verifier`** — owner werdyktu PASS/FAIL; live-repro w Playwright przeciw `:1420` z mockiem `invoke`.
- **`lock-security-reviewer`** — WYMAGANY (zmiana dotyka ścieżki read wektorów). Checklist inwariantów:
  1. Źródło przez `visibility_clause`/`search_semantic_visible` — żadnej ungated ścieżki.
  2. Surowy embedding **nigdy** nie przekracza IPC (centroid liczony i konsumowany w Rust; DTO = `SearchHit`, bez wektora).
  3. Snippet/tytuł = treść → już gated przez `search_semantic_visible`; sekcja FE nie renderuje nic dla niewidocznych.
  4. Brak nowego at-rest store; logi bez PII.

### Honest bar
Jakość sąsiadów na realnych (polskich) notatkach = weryfikowalne tylko @Mac z pobranym e5 + reindex (bake-off).
Headless test dowodzi GATINGU i plumbingu (stub wystarcza), **nie** jakości retrievalu.

---

## Constraints (fleet respektuje)
- Commit/PR tylko `QueaT <kgm004a@gmail.com>`, bez trailerów Claude; `gh` = JakubGawr.
- Nigdy direct-push na `murmur` — PR → merge (block-bash hook).
- Bez nowych npm/crate'ów (wszystko reużywa istniejącego).
- `com.meetnotes.app` niezmienne.

## Pierwszy krok do implementacji (po zielonym bake-offie)
`rust-tauri-dev`: napisz `related_meetings_visible` + komendę + rejestr w `lib.rs` + dwa testy gatingu →
`cargo test --lib`. To najmniejszy weryfikowalny plaster; FE jest mechanicznym następstwem.
