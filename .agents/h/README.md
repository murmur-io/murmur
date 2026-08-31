# h — mały harness

Jedno zadanie, jedna komenda, koniec.

```bash
scripts/h run <task-id> --prompt "co ma być zrobione"
```

Co się dzieje:

```
worktree → plan → implementacja → checki + weryfikacja
                       ↑                    │
                       └──── max 2 poprawki ┘
```

Weryfikator odpowiada na jedno pytanie: **czy to zadanie zostało zrobione i czy
ta funkcjonalność działa?** Nie recenzuje kodu, nie żąda dowodów, nie proponuje
ulepszeń. Trzy wyjścia: `DZIALA`, `NIE_DZIALA` + co konkretnie, `NIE_WIEM`.

Jeśli `NIE_DZIALA` — ten sam agent implementacyjny dostaje werdykt i poprawia.
Po dwóch nieudanych poprawkach STOP i pytanie do człowieka. Worktree zostaje
nietknięty.

## Komendy

```
run <id> --prompt "…"    całość
list                     otwarte taski
status <id>              stan
check [<id>]             odpal checki ręcznie
clean <id>               usuń worktree + branch
```

## Vendorzy

Domyślnie **plan: codex, kod: claude, weryfikacja: codex** — weryfikuje inny
model niż ten, który pisał. Zmiana: `--planner/--dev/--verifier` albo
`H_PLANNER`/`H_DEV`/`H_VERIFIER`.

## Checki

`checks.json` mapuje zmienione ścieżki na komendy. Checki są **zawężane do tego,
co zmienione** — ale zawężanie ma granice, które trzeba znać, bo inaczej czytasz
zielone tam, gdzie nic nie poleciało.

Co realnie kosztuje pełny przebieg (M4 Max, ciepły `target/`, 2026-08-31):

| check | pełny | zawężony |
| --- | --- | --- |
| `rust-test` (3548 testów, `--test-threads=1`) | 201 s | kilka sekund |
| `rust-clippy` | ~45 s (ciepły) | nie zawęża się |
| `playwright` (974 przebiegi: 487 × chromium + webkit) | 181 s @ `--workers=6` | tylko gdy ruszony `e2e/*.spec.ts` |
| `ng lint` + `ng build` | 3,5 s + 5,3 s | — |

Dwie liczby, które wyglądają na literówkę, a nie są:

- `rust-test` leci **równolegle wolniej niż szeregowo** (296 s vs 199 s zmierzone) —
  SQLite i SQLCipher mają globalne muteksy, więc `--test-threads=1` jest tu wyborem
  wydajnościowym, nie ostrożnością.
- `playwright` leci u nas na `--workers=6`, a `scripts/ci.sh` na `--workers=2` — i tak
  ma zostać. CI stoi na macos-14 z 3 rdzeniami; ta maszyna ma 16. Lokalnie zmierzone
  469 s @ 2 vs 181 s @ 6, 974/974 passed w obu.

Obu nie zmieniaj bez pomiaru.

Granice zawężania:

- **Powyżej 6 dotkniętych modułów** leci pełny suite.
- **`lib.rs` / `main.rs` → zawsze pełny suite.** Ich „nazwa modułu" to katalog
  `src`, a `cargo test --lib -- src` odpala 1 test z 3548 i raportuje zielone
  w 0,05 s. `lib.rs` to rejestr `generate_handler!` — fałszywa zieleń tam jest
  gorsza niż wolny check.
- **Zmiana samego `src/app/**` nie zawęża playwrighta.** `playwright_filters`
  zbiera wyłącznie ścieżki `e2e/*.spec.ts`; typowa zmiana FE ich nie rusza, więc
  komenda wraca nieskrócona. Jeśli chcesz zawężenia — dotknij speca, który to
  pokrywa.

Świeży worktree dostaje symlinki do `target/` **i** `node_modules/` głównego
checkoutu (to drugie tylko przy identycznym `package-lock.json` — inaczej harness
mówi, żeby odpalić `npm ci`). Bez tego checki FE nie miały lokalnego `ng`.

`protocol-server` i `perf-contracts` są w `manual_only` — w 74 uruchomieniach
starego harnessu nie złapały nic, więc nie lecą automatycznie.

## Granice

Ten harness ma ~415 linii i ma takim zostać. Poprzedni miał 38 226 i to jest
dokładny powód, dla którego go nie ma. Zanim coś tu dopiszesz, sprawdź w danych,
czy to coś kiedykolwiek złapało realny błąd.

Czego tu celowo NIE ma: receiptów, attestacji, ledgerów zdarzeń, proof-gapów,
protokołu probe'ów, reviewerów-specjalistów, maszyny stanów z ośmioma statusami,
selftestów control-plane'u. Każda z tych rzeczy była mierzona na 205 zadaniach i
żadna nie zarobiła na swoje utrzymanie.
