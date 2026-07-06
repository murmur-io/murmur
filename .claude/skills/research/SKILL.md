---
name: research
description: Obszerny, ugruntowany research w kontekście naszej apki Murmur/brain2 — co możemy dodać, ulepszyć, jak to zrobić technicznie i jak wypadamy vs konkurencja. Rozbija temat na kąty, odpala równolegle subagentów `murmur-researcher` (web + kod), syntetyzuje i zapisuje raport do docs/research/. Użyj, gdy użytkownik chce zbadać pomysł/feature/usprawnienie/podejście techniczne dla apki, albo napisze /research.
---

# /research — ugruntowany research produktowo-techniczny dla Murmur

Prowadzisz dogłębny research **w kontekście naszej apki** (Murmur, ewoluującej w brain2): lokalna, prywatna apka do notatek ze spotkań (Tauri 2 + Rust + Angular 22 + whisper.cpp + Obsidian + pluggable LLM). Cel: ocenić co warto **dodać/ulepszyć**, jak to zrobić technicznie, i jak to się ma do konkurencji — z cytatami i konkretną rekomendacją, a nie listą linków.

**Rozmawiaj z użytkownikiem po polsku.** Raport-artefakt pisz po angielsku (spójnie z `docs/` — `COMPETITIVE-LANDSCAPE.md`, specy brain2 są po angielsku).

Mechanizm: ten skill **orkiestruje**, a ciężką robotę robi subagent **`murmur-researcher`** (`.claude/agents/murmur-researcher.md`) — ma zaszyty kontekst apki, robi web research (WebSearch/WebFetch) + grounding w kodzie i adwersaryjnie weryfikuje. Zwykle odpalasz **kilku równolegle**, każdy na inny kąt.

## Procedura

### 1. Zrozum i doprecyzuj temat
Wyłap z prośby: temat researchu + INTENCJĘ (nowy feature? ulepszenie istniejącego? podejście techniczne/feasibility? pozycjonowanie vs konkurencja? wybór biblioteki/modelu?).

Jeśli temat jest **zbyt szeroki lub niejasny** (np. „zbadaj co dodać do apki" bez kierunku), zadaj 1–3 krótkie pytania doprecyzowujące zanim odpalisz agentów — np. obszar (capture / transkrypcja / notatka AI / graf / Ask / MCP / multi-source), cel (jakość notatki / szybkość / prywatność / nowi userzy / moat), albo czy chodzi o szybki research czy głęboki sweep. Nie odpalaj 6 agentów na mgliste pytanie.

### 2. Odśwież realny stan apki (tanio, zanim rozdzielisz pracę)
Zerknij na najświeższe źródła prawdy w repo, żeby dobrze sformułować kąty i nie kazać agentom badać czegoś, co już mamy:
- `docs/STATUS.md` (co zaimplementowane/zweryfikowane vs gated), `docs/KILLER-FEATURES.md` (co już shippnięte), `docs/COMPETITIVE-LANDSCAPE.md`, `docs/superpowers/specs/` (najnowsze plany/decyzje, np. spec brain2).
- W razie potrzeby `git log --oneline -15` i struktura `src-tauri/src/` + `src/app/features/`.

Zasada zespołu: **ufaj kodowi, nie docsom** — docsy bywały nieaktualne. Kluczowe twierdzenia agenci weryfikują w kodzie.

### 3. Rozbij temat na niezależne kąty
Dobierz 1–5 kątów do tematu (nie odpalaj na siłę wszystkich). Typowe osie:
- **Prior art / konkurencja** — kto już to robi, jak, gdzie mają luki (baseline: `COMPETITIVE-LANDSCAPE.md`).
- **Feasibility techniczna** — biblioteki/crate'y/SDK, ScreenCaptureKit/Whisper/Tauri/Angular realia, koszt integracji, licencje.
- **UX / wzorce interakcji** — jak to powinno wyglądać dla usera Obsidiana.
- **Dopasowanie do architektury** — provider seam, redaction firewall, SQLite-canonical, MCP, eksport `.md`.
- **Popyt / sygnały od userów** — fora, issue, Reddit/HN: czy ludzie tego chcą.
- **Ryzyka / koszt / prywatność** — egress do chmury, wydajność na słabszych Makach, uprawnienia macOS.

Jeden agent = jeden kąt, zero współdzielonego stanu → idealne pod równoległość.

### 4. Odpal subagentów `murmur-researcher` RÓWNOLEGLE
W **jednej wiadomości** wywołaj kilka `Agent` (subagent_type: `murmur-researcher`), po jednym na kąt. Każdemu daj: (a) precyzyjny kąt, (b) oryginalny temat usera, (c) czego konkretnie szukasz i jaka decyzja od tego zależy. Agent zna kontekst apki i kontrakt outputu — nie powtarzaj architektury, podaj kąt.

Dla wąskiego / szybkiego pytania wystarczy **jeden** agent. Dla „zrób obszerny sweep" — 3–5.

### 5. Syntetyzuj (nie zlepiaj)
Zbierz briefy. Usuń duplikaty, pogódź sprzeczności (jak agenci się różnią — powiedz to wprost i zważ dowody/confidence). Zbuduj **jedną** spójną rekomendację. Nie wklejaj surowych outputów agentów.

### 6. Zapisz raport + odpowiedz
Zapisz syntezę do `docs/research/<YYYY-MM-DD>-<slug>.md` (datę weź z `date +%F`; utwórz folder jeśli brak). Struktura:

```markdown
<!-- Generated <date> via /research (murmur-researcher fan-out). Pricing/funding/version = point-in-time. -->
# Research: <temat>

## TL;DR / Verdict
## Co już mamy (z repo, z file:line)
## Findings  (per kąt; każde twierdzenie z URL-em lub file:line + confidence)
## Fit z ograniczeniami Murmur  (local-first / Obsidian-native / SQLite-canonical / provider seam + redaction / macOS / CI)
## Opcje i tradeoffy  (S/M/L effort, ryzyko, co odblokowuje)
## Rekomendacja i pierwszy krok  (najmniejszy weryfikowalny wycinek / spike)
## Otwarte pytania / czego nie udało się zweryfikować
## Sources  (URL-e + kluczowe file:line)
```

Potem w czacie **po polsku**: krótkie streszczenie (verdict + rekomendacja + 1. krok) i ścieżka do zapisanego raportu. Bądź szczery co do confidence i tego, co wymaga prawdziwego Maca / nie dało się zweryfikować.

## Zasady
- **Cytuj albo nie istnieje.** Każde zewnętrzne twierdzenie → URL, który ktoś naprawdę pobrał; każde o naszym kodzie → `file:line`.
- **Decyzja, nie ankieta.** Kończ rekomendacją i najmniejszym następnym krokiem.
- **Szczerość commodity vs differentiated.** Jeśli pomysł to table-stakes (lokalny Whisper, Ollama, „jakiś Ask") — powiedz; przewaga zwykle leży w integracji, nie w pojedynczym feature.
- **Read-only dla kodu apki.** Skill zapisuje tylko raport w `docs/research/`. Nie ruszaj kodu Murmur bez wyraźnej prośby.
- **Skaluj do pytania.** Wąskie pytanie → 1 agent, krótki raport. „Obszerny sweep" → 3–5 agentów, pełna synteza.

## Przykłady wywołań
- `/research czy warto dodać live transcription i jak (Whisper streaming vs serwerowy)?`
- `/research jak najlepiej zrobić diaryzację mówców on-device — opcje, modele ONNX, koszt`
- `/research gdzie mamy realny moat vs Granola/Talat i co dobudować, żeby go pogłębić`
- `/research zbadaj integrację z kalendarzem/Slackiem jako 2. źródłem dla brain2`
