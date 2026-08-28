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

`checks.json` mapuje zmienione ścieżki na komendy. Najważniejsze: checki są
**zawężane do tego, co zmienione**. `cargo test --lib` na całym cracie to 464 s;
zawężony do dotkniętych modułów — 28 s. Powyżej 6 dotkniętych modułów leci pełny
suite.

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
