---
name: dreaming
description: Tryb kreatywnego śnienia o Murmurze — Codex nasiąka apką, wędruje po necie, kombinuje i WYMYŚLA co możemy dodać, nie jako suchy research tylko swobodne myślenie generatywne oparte o głębokie zrozumienie apki i o to, co akurat przyjdzie do głowy. Rozchodzi się szeroko (dużo dzikich pomysłów), skupia na iskrach, buduje klikalny prototyp HTML+JS „na czuja" i daje userowi do akceptu; zaakceptowane deleguje do /ship-feature. Użyj, gdy user chce pomarzyć / pobawić się pomysłami / „wymyśl coś fajnego" dla apki, albo napisze /dreaming.
---

# /dreaming — sen na jawie o Murmurze

To NIE jest `/research`. Research odpowiada na *zadane* pytanie: ugruntowany, cytowany, zbieżny, decision-ready. **Dreaming jest odwrotny: rozbieżny, generatywny, samosterowny.** Nasiąkasz Murmurem tak głęboko, że czujesz jego duszę, a potem *wymyślasz* — puszczasz wodze, kombinujesz, kradniesz mechaniki z innych światów, pytasz „a co gdyby…". Cel to nie raport — to **iskra**, którą można dotknąć: klikalny prototyp, który userowi coś *robi*, i który po akcepcie leci do `/ship-feature`.

**Rozmawiaj po polsku.** Artefakty (notatka-sen + kod prototypu) pisz po angielsku — spójnie z resztą `docs/` i kodem. Demo-świat w prototypie może być po polsku/angielsku.

## Jedno przykazanie: GENERYCZNOŚĆ = PORAŻKA

Badania są bezlitosne: AI podnosi *średnią* kreatywność, ale **regresuje do oczywistego** — top-idee cierpią. Twoim jedynym wrogiem w tym trybie jest pomysł, który generyczny cloudowy „AI notetaker" mógłby wydać w przyszły wtorek. **Jeśli konkurent mógłby to shipnąć bez naszych atomów — to nie sen, to ticket w backlogu.** Odrzuć albo oznacz i idź dalej.

**Nasze nie-do-skopiowania atomy** (każdy sen musi stać na którymś z nich):
- **głos** — wejściem jest mowa, nie klawiatura; voice-verified provenance (kto to powiedział, na głos, potwierdzone on-device)
- **on-device** — cała inteligencja lokalnie (whisper + brain + embedder), zero przymusowego egressu
- **the lock** — per-folder szyfrowanie treści, biometria, verify-before-destroy → *prywatność jako mechanika*, nie ustawienie
- **owned files** — czyste `.md` w vaulcie Obsidiana usera, `[[wikilinki]]`, żadnego lock-inu
- **the brain** — pamięć cross-meeting nad JEDNYM kanonicznym store'em (SQLite), FTS + wektory + graf + fakty
- **capture drugiej strony** — ScreenCaptureKit far-side bez bota (macOS-only supermoc)

Test na każdy pomysł: *„czy to możliwe TYLKO dlatego, że jesteśmy głosowi + lokalni + zamykalni + posiadający pliki?"* Jeśli nie — spal go.

## Przepływ (luźny, nie sztywny rurociąg)

### 0. Nasiąknij — wciągnij Murmura w kości
Nie bibliografia — *poczucie*. Zajrzyj w realny kod/feature'y (`trust code, not docs`), przypomnij sobie z pamięci co już mamy i gdzie leży moat (np. [[deep-analysis-v3-2026-07-03]], [[mega-analysis-2026-07-03]]). Musisz wiedzieć, na jakim substracie śnisz — sen ma wyrastać z TYCH atomów, nie z ogólnika „apka do notatek".

### 1. Rozejdź się SZEROKO — dużo, dziko, ilość przed jakością
Wygeneruj **wiele** pomysłów (celuj w 15–30), świadomie łamiąc pułapkę generyczności. Nie filtruj jeszcze. Narzędzia wymuszające dywergencję (użyj kilku, nie jednego):
- **SCAMPER na istniejących featurach** — Substitute / Combine / Adapt / Modify / Put-to-other-use / Eliminate / Reverse. Co gdyby *odwrócić* nagrywanie? *połączyć* the lock z udostępnianiem? *usunąć* ekran nagrywania w ogóle?
- **Najazd analogiczny** — ukradnij mechanikę z zupełnie innego świata (gry, szpital, produkcja muzyczna, szpiegostwo, biologia, DJ-ka, roguelike) i przeszczep na Murmura. Cross-domain to najbogatsze źródło oryginalności.
- **„Tylko Murmur mógłby…"** — pomysły niemożliwe bez głosu+lokalności+locka+plików. To serce trybu.
- **Wędrówka po necie** — WebSearch/WebFetch po iskry, nie po odpowiedź: sąsiednie narzędzia, dziwne wątki HN, świeże możliwości platform (Apple Foundation Models, nowe API). Zbierasz krzemień, nie piszesz raportu.
- **Prowokacja** — „a co gdyby to było szalone / zakazane / niemożliwe?". Przepchnij za bezpieczny pomysł.

**Opcja pod większą dywergencję (bije regresję-do-średniej):** odpal RÓWNOLEGLE kilku subagentów (Agent tool), każdy zaklinowany w *innej* soczewce — jeden tylko SCAMPER, jeden tylko analogie cross-domain, jeden tylko „tylko-Murmur", jeden „co zrobiłby wróg" — potem sam skuratoruj i połącz. Różnorodność z przymusu, nie z nadziei.

### 2. Skup się — zabij ukochane, zostaw iskry
Z rozsypu wybierz **1–3**, które: (a) przechodzą test „tylko Murmur", (b) coś Ci *robią* (jest emocja), (c) da się je sprototypować tak, żeby pokazać *uczucie*. Powiedz wprost DLACZEGO każdy przeżył — i że większość zginęła (to jest sens szerokiego rozejścia).

### 3. Prototyp UCZUCIA — klikalny HTML+JS
Dla top-pomysłu(ów) zbuduj **samowystarczalny statyczny prototyp**, który user otwiera w przeglądarce i *klika*. To nie jest kod produkcyjny — to „vibe prototype", którego jedyne zadanie to uczynić pomysł **namacalnym** w 30 sekund i odpowiedzieć „czy to iskrzy, czy warto to budować".
- **Czysty HTML + CSS + vanilla JS**, jeden plik (lub malutki folder), **zero buildu, zero npm**. Bez nowych zależności.
- **Żeby czuł się jak Murmur:** użyj tokenów z `src/styles.css` (`--surface-base` `#07070b`, `--accent` `#6e76ff`, `--text-primary`, `--radius-md`, ciemny motyw) i — gdy pomysł dotyka realnego UI — podłącz wzorzec `scripts/screenshots/mock-tauri.js` (mock `window.__TAURI_INTERNALS__` + fikcyjny demo-świat „Sonora"), żeby to wyglądało jak prawdziwy Murmur nad zmyślonymi danymi, nie jak drut.
- **Fejkuj wszystko** — dane, AI, backend. Zahardkoduj zachwycającą happy-path. Optymalizuj *uczucie i kierunek*, nie poprawność.
- **Zapisz** do `docs/dreams/prototypes/<slug>/index.html`. Otwórz go (Playwright/przeglądarka albo poproś usera o otwarcie pliku) i **zrzuć ekran** — najpierw sam obejrzyj PNG (nie ufaj, że „pewnie wygląda ok").

### 4. Pitch + werdykt — Codex proponuje, user dysponuje
Podaj każdy przeżyły sen jako mini-pitch: jednolinijkowe „a co gdyby", *dlaczego tylko Murmur*, link do prototypu i **szczerze** „co by to kosztowało" (+ granica signed-build/platformy — niektóre sny wymagają prawdziwego Maca/telefonu, patrz [[mobile-ios-android-feasibility]]). To jest **bramka**: user akceptuje / odrzuca / iteruje. Nigdy nie shipuj snu sam.

### 5. Handoff zaakceptowanych → `/ship-feature`
Dla zaakceptowanego snu napisz zwięzły handoff: koncept, **prototyp jako gwiazda-północna dla UCZUCIA**, oraz realne szwy, których dotknie w prawdziwej apce (komenda Tauri + `ipc.service.ts` + `models.ts` DTO + feature Angulara). Potem odpal `/ship-feature`. Prototyp staje się „spec-of-feel". Odrzucone sny i tak zostają w dzienniku — backlog prowokacji.

### Zapisz sen (dziennik snów)
Zapisz rozsyp + przeżyłych + wskaźniki do prototypów do `docs/dreams/<YYYY-MM-DD>-<slug>.md` (datę z `date +%F`; utwórz folder jeśli brak). Sny się kumulują — nawet martwe są paliwem na później.

```markdown
<!-- Dreamed <date> via /dreaming. Prototypes are vibe-prototypes (fake data, not production). -->
# Dream: <temat>

## The spark  (1–3 survivors — one line each: „what if…")
## The wide spread  (all 15–30, terse — the graveyard is the point)
## Why these survived  (per survivor: which un-copyable atom it stands on + the emotion)
## Prototype(s)  (path to docs/dreams/prototypes/<slug>/, what it fakes, screenshot)
## What it'd really take  (real seams touched + honest S/M/L + signed-build/platform limits)
## Verdict  (accepted / rejected / iterate — filled after the user disposes)
```

Potem w czacie **po polsku**: pokaż iskry, wklej prototyp, poproś o werdykt.

## Zasady (dusza trybu)

- **Generyczność = porażka.** Jeśli generyczny cloudowy notetaker mógłby to wydać — to nie sen. Każdy sen opiera się o nasz nie-do-skopiowania atom.
- **Rozejdź się, ZANIM się skupisz.** Nie skacz na pierwszy pomysł. Ilość najpierw, sąd potem. Pojedynczy „rozsądny" pomysł to sygnał, że tryb nie ruszył.
- **Prototypuj UCZUCIE, nie system.** Fejkuj wszystko; prototyp istnieje, by user *poczuł* pomysł w 30 s. Nigdy kod produkcyjny, nigdy nowe zależności.
- **Codex proponuje, user dysponuje.** Każdy sen idzie pod akcept. Handoff do `/ship-feature` tylko na „tak". Zero auto-shipu.
- **Lot ugruntowany.** Dzikie = dobre; oderwane = nie. Nazwij realne atomy Murmura pod każdym snem i bądź szczery o granicy signed-build/platformy.
- **Ufaj kodowi, nie docsom.** Substrat, na którym śnisz, sprawdź w drzewie (`file:line`), nie w STATUS.md.

## Czerwone flagi — STOP, tryb się nie odpalił

- Masz 3 „rozsądne" pomysły zamiast 20 dzikich → wróć do §1, użyj SCAMPER/analogii/prowokacji.
- Pomysł, który Otter/Granola mógłby shipnąć → generyczny, spal albo oznacz.
- Opisujesz feature słowami zamiast go *pokazać* → zbuduj prototyp, uczucie > opis.
- Zabierasz się za „prawdziwą" implementację w apce → to `/ship-feature`, nie `/dreaming`; tu tylko fejkowy prototyp.
- Zaczynasz shipować bez akceptu usera → STOP, to bramka.
```
