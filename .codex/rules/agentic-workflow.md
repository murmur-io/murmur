# Agentic workflow — Murmur (binding)

## Kto weryfikuje

**Implementujący nigdy nie wydaje werdyktu.** Weryfikuje inny model niż ten,
który pisał kod. To jedyna rzecz z poprzedniego harnessu, która obroniła się
w danych.

## Harness (`scripts/h`)

Opt-in, do zadań wieloetapowych albo ryzykownych. Zwykłe commity idą normalnie.

```bash
scripts/h run <task-id> --prompt "co ma być zrobione"
```

```
worktree → plan → implementacja → checki + weryfikacja → max 2 poprawki → koniec
```

Weryfikator dostaje zadanie, plan, diff i wynik checków. Odpowiada wyłącznie na
pytanie **czy ta funkcjonalność działa** — nie recenzuje kodu spoza zadania, nie
żąda dodatkowych dowodów, nie proponuje ulepszeń. `NIE_DZIALA` wraca do agenta
implementacyjnego z konkretem. Po dwóch nieudanych rundach harness staje i pyta
człowieka, zamiast otwierać `-r3`.

Szczegóły: `.agents/h/README.md`.

## Czego nie robimy

- **Nie mnożymy reviewerów.** Jeden weryfikator. Na 283 diffach specjaliści
  (lock/egress/protocol) znaleźli coś ponad generalistę **16 razy — 5,7%**,
  kosztem 448 uruchomień i 63 Mtok.
- **Nie weryfikujemy dowodów, tylko działanie.** 46% findingów MAJOR/BLOCKER
  starego harnessu dotyczyło kompletności artefaktów ("provide evidence",
  "provide receipt"), nie tego, czy funkcja działa.
- **Nie odpalamy pełnych suite'ów na trzyliniowej zmianie.** Checki są zawężane
  do zmienionych ścieżek. `cargo test --lib` w całości = 464 s; zawężony = 28 s.
- **Nie zostawiamy pętli bez limitu.** Dwie poprawki, potem człowiek.

## Adwersaryjna weryfikacja

Kompilujący się kod to nie „zrobione". Bugfix wymaga testu, który **pada na
starym kodzie i przechodzi na nowym** — test przechodzący przed poprawką nie
złapał buga. Dla Rusta pętla wewnętrzna to `cargo test --lib`, nigdy gołe
`clippy --all-targets`.

Znane klasy błędów, które już raz wyszły na produkcji — sprawdź, jeśli zmiana
ich dotyka: utrata treści przy seal, wyciek zapieczętowanej treści lub assetu,
abort FFI na macOS, nieaktualne efekty IPC, cykle importów w standalone,
przebijanie przezroczystości overlayów, utrata stylów przez CSP w spakowanym
WebKit.

## Wspólny pas zasobów

Ciężkie Cargo/rustc/CI odpalaj przez `scripts/agent-resource-run`, żeby dwa
worktree nie mieliły równolegle:

```bash
scripts/agent-resource-run --chdir src-tauri -- cargo test --lib
scripts/agent-dev-run -- npm run dev
```

## Ufaj kodowi, nie prozie

Dokumentacja w repo się rozjeżdża. Każde nośne twierdzenie potwierdź w aktualnym
pliku i symbolu. Nie ufaj pierwszemu odczytowi.
