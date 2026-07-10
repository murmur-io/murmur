//! SYNTHETIC eval corpus for the RAG bake-off (brain2 PR 2, spec §L1.6 adapted).
//!
//! ## Why synthetic
//!
//! The spec's eval gate wants a labeled set over the user's REAL vault — that stays the
//! higher-signal upgrade path (see `docs/RAG-BAKEOFF.md`). But a real-vault set needs manual
//! labeling and a copied dev DB, so the BOOTSTRAP baseline uses this seeded fixture corpus
//! instead: 16 fixed meetings whose ids/dates/content are constants, which makes every
//! `expected_meeting_ids` in `fixtures/rag-bakeoff-synthetic.json` correct BY CONSTRUCTION —
//! zero manual labeling, byte-deterministic across machines, safe to commit (no PII: every
//! name/project/topic below is invented).
//!
//! ## The four query categories the corpus covers
//!
//! - **entity-anchored** — recurring people (Anna Nowak, Marek Wiśniewski) and projects
//!   (Projekt Atlas, Orion Migration) spread across several meetings with commitments/decisions.
//! - **paraphrase** — notes say "umówiliśmy się na piątek" / "lecimy z tym" / "ship it" while the
//!   query asks about "deadline" / "decyzja" / "approve": no exact-token overlap by construction
//!   (Polish inflection means even topic nouns differ token-wise; FTS5 unicode61 does not stem).
//! - **cross-lingual** — PL-note meetings targeted by EN queries and vice versa, with zero shared
//!   tokens (no shared proper nouns in those queries).
//! - **temporal** — meetings clustered in distinct calendar weeks relative to the FIXED
//!   [`CORPUS_ANCHOR_DATE`] so "last week"-style queries have a unique answer set. Temporal
//!   EXPANSION is NOT implemented yet (spec L1.5, PR 3) — these queries establish the pre-L1
//!   baseline and are EXPECTED to score ~0; they are still labeled correctly.
//!
//! ## Gating / lock model
//!
//! Seeding uses only the NORMAL insert paths (`insert_meeting` / `upsert_note` /
//! `insert_segments` — the FTS triggers fire on these — plus `index_meeting_chunks` exactly as
//! the pipeline's auto-index does). No folder is created and nothing is sealed, so every seeded
//! meeting is plainly visible; the bake-off itself reads only through the gated
//! `search_visible` / `search_semantic_visible`. Not lock-touching: no new read path, no seal.

use crate::embed::Embedder;
use crate::error::Result;
use crate::storage::models::{Meeting, MeetingStatus, NoteRecord};
use crate::storage::Db;
use crate::transcribe::types::Segment;

/// The FIXED "today" every temporal query in the fixture is phrased against (a Monday). NEVER
/// `now()` — determinism is the whole point: "last week" relative to this anchor is always
/// 2026-06-22..2026-06-28, so the expected ids never rot.
pub const CORPUS_ANCHOR_DATE: &str = "2026-06-29";

/// One synthetic meeting spec: everything `seed_synthetic_corpus` writes, as compile-time
/// constants (fixed id, fixed ISO start, fixed note, fixed speaker turns).
pub struct SyntheticMeeting {
    pub id: &'static str,
    pub title: &'static str,
    /// ISO 8601 UTC start — fixed dates spread over ~8 weeks before [`CORPUS_ANCHOR_DATE`].
    pub started_at: &'static str,
    /// Realistic note markdown (~10–20 lines).
    pub note_markdown: &'static str,
    /// Transcript as `(speaker, text)` turns; timings are derived deterministically from the
    /// turn index in [`seed_synthetic_corpus`].
    pub turns: &'static [(&'static str, &'static str)],
}

/// The 16 fixed meetings. Week layout (Mon-anchored, anchor = 2026-06-29):
/// w-8: syn-001/011/013 · w-7: syn-003/006/015 · w-6: syn-002/009 · w-5: syn-012/007/014 ·
/// w-4: syn-004/008 · w-3: syn-005/010 · w-1 ("last week"): syn-016.
pub static SYNTHETIC_MEETINGS: &[SyntheticMeeting] = &[
    // ── entity-anchored cluster: Projekt Atlas + Anna Nowak ────────────────────────────────
    SyntheticMeeting {
        id: "syn-001",
        title: "Projekt Atlas — kickoff",
        started_at: "2026-05-11T10:00:00Z",
        note_markdown: "# Projekt Atlas — kickoff\n\n\
## Ustalenia\n\
- Projekt Atlas startuje z zespołem czteroosobowym.\n\
- Anna Nowak przygotuje specyfikację API do końca maja i obiecała przegląd z zespołem backendu.\n\
- Marek Wiśniewski wesprze Atlas przy architekturze, ale jego głównym tematem pozostaje Orion.\n\n\
## Decyzje\n\
- Zakres pierwszej fazy: tylko import danych i panel raportów.\n\
- Integracje zewnętrzne przesuwamy do fazy drugiej.\n\n\
## Do zrobienia\n\
- [ ] Anna Nowak: szkic specyfikacji API (koniec maja)\n\
- [ ] Zespół: przegląd zakresu za dwa tygodnie\n",
        turns: &[
            ("me", "Zaczynamy Projekt Atlas, cel to import danych i panel raportów w pierwszej fazie."),
            ("others", "Anna Nowak tutaj — biorę na siebie specyfikację API, dowiozę ją do końca maja."),
            ("others", "Marek Wiśniewski — pomogę przy architekturze, ale Orion zostaje moim głównym tematem."),
            ("me", "Dobrze, integracje zewnętrzne idą do fazy drugiej, zakres potwierdzimy za dwa tygodnie."),
        ],
    },
    SyntheticMeeting {
        id: "syn-002",
        title: "Projekt Atlas — status sync",
        started_at: "2026-05-25T10:00:00Z",
        note_markdown: "# Projekt Atlas — status sync\n\n\
## Postęp\n\
- Anna Nowak dostarczyła specyfikację API zgodnie z obietnicą; backend zaczyna implementację.\n\
- Import danych działa na środowisku testowym.\n\n\
## Ryzyka\n\
- Marek Wiśniewski zgłosił ryzyko wydajnościowe przy dużych plikach importu.\n\n\
## Decyzje\n\
- Przesuwamy start produkcyjny Atlasa o dwa tygodnie, żeby domknąć testy wydajności.\n\
- Anna Nowak zobowiązała się przygotować plan testów obciążeniowych.\n\n\
## Do zrobienia\n\
- [ ] Anna Nowak: plan testów obciążeniowych\n\
- [ ] Marek Wiśniewski: profilowanie importu dużych plików\n",
        turns: &[
            ("others", "Anna Nowak — specyfikacja API jest gotowa, backend może zaczynać."),
            ("others", "Marek Wiśniewski — widzę ryzyko wydajnościowe przy imporcie dużych plików, trzeba to sprofilować."),
            ("me", "W takim razie przesuwamy start produkcyjny Atlasa o dwa tygodnie."),
            ("others", "Anna Nowak — wezmę na siebie plan testów obciążeniowych."),
        ],
    },
    SyntheticMeeting {
        id: "syn-003",
        title: "Orion Migration — planning",
        started_at: "2026-05-18T14:00:00Z",
        note_markdown: "# Orion Migration — planning\n\n\
## Zakres\n\
- Orion Migration obejmuje przeniesienie bazy danych klientów na nowy klaster.\n\
- Marek Wiśniewski jest właścicielem migracji bazy danych end-to-end.\n\n\
## Harmonogram\n\
- Etap 1 (schemat + replikacja): do 5 czerwca.\n\
- Etap 2 (przełączenie ruchu): połowa czerwca, okno nocne.\n\n\
## Decyzje\n\
- Migrujemy klientów partiami po 10%, z możliwością rollbacku po każdej partii.\n\n\
## Do zrobienia\n\
- [ ] Marek Wiśniewski: skrypt replikacji i plan rollbacku\n\
- [ ] Ops: rezerwacja okna nocnego na przełączenie\n",
        turns: &[
            ("me", "Orion Migration: przenosimy bazę danych klientów na nowy klaster."),
            ("others", "Marek Wiśniewski — biorę migrację bazy danych end-to-end, ze skryptem replikacji i planem rollbacku."),
            ("me", "Przełączamy ruch partiami po dziesięć procent, każde przełączenie w oknie nocnym."),
        ],
    },
    SyntheticMeeting {
        id: "syn-004",
        title: "Orion Migration — checkpoint",
        started_at: "2026-06-08T14:00:00Z",
        note_markdown: "# Orion Migration — checkpoint\n\n\
## Postęp\n\
- Marek Wiśniewski zakończył etap 1: schemat przeniesiony, replikacja działa stabilnie.\n\
- Pierwsza partia 10% klientów przełączona bez incydentów.\n\n\
## Ryzyka\n\
- Dwa raporty wolniejszych zapytań po stronie nowego klastra — Marek analizuje indeksy.\n\n\
## Ustalenia\n\
- Anna Nowak przejrzy zapytania raportowe Atlasa pod kątem zgodności z nowym klastrem Oriona.\n\
- Kolejna partia przełączenia w przyszłym tygodniu, jeśli indeksy będą poprawione.\n\n\
## Do zrobienia\n\
- [ ] Marek Wiśniewski: poprawa indeksów na nowym klastrze\n\
- [ ] Anna Nowak: przegląd zapytań raportowych\n",
        turns: &[
            ("others", "Marek Wiśniewski — etap pierwszy zamknięty, replikacja stabilna, pierwsza partia klientów przełączona."),
            ("me", "Widzę dwa zgłoszenia wolniejszych zapytań na nowym klastrze."),
            ("others", "Marek Wiśniewski — analizuję indeksy, poprawki wejdą przed kolejną partią."),
            ("others", "Anna Nowak — przejrzę zapytania raportowe Atlasa pod nowy klaster."),
        ],
    },
    SyntheticMeeting {
        id: "syn-005",
        title: "Projekt Atlas — budget review",
        started_at: "2026-06-15T11:00:00Z",
        note_markdown: "# Projekt Atlas — budget review\n\n\
## Decyzje\n\
- Budżet Projektu Atlas na drugą fazę zatwierdzony w pełnej wysokości.\n\
- Anna Nowak dostała zgodę na zatrudnienie kontraktora do panelu raportów i obiecała domknąć\n\
  rekrutację przed końcem czerwca.\n\n\
## Ustalenia\n\
- Testy obciążeniowe z planu Anny wykazały stabilny import — start produkcyjny potwierdzony.\n\
- Koszty klastra Oriona rozliczamy osobno, poza budżetem Atlasa.\n\n\
## Do zrobienia\n\
- [ ] Anna Nowak: kontraktor do panelu raportów (koniec czerwca)\n\
- [ ] PM: aktualizacja planu wydatków drugiej fazy\n",
        turns: &[
            ("me", "Budżet drugiej fazy Projektu Atlas zatwierdzamy w pełnej wysokości."),
            ("others", "Anna Nowak — w takim razie ruszam z zatrudnieniem kontraktora do panelu raportów, domknę to przed końcem czerwca."),
            ("me", "Koszty klastra Oriona idą osobno, nie z budżetu Atlasa."),
        ],
    },
    // ── paraphrase cluster: the note says it colloquially; the query asks formally ─────────
    SyntheticMeeting {
        id: "syn-006",
        title: "Cotygodniowa synchronizacja zespołu",
        started_at: "2026-05-20T09:00:00Z",
        note_markdown: "# Cotygodniowa synchronizacja zespołu\n\n\
## Ustalenia\n\
- Umówiliśmy się na piątek z oddaniem bramki płatniczej — po piątku żadnych dosuwek.\n\
- QA dostaje build w środę wieczorem, żeby mieć dwa pełne dni.\n\n\
## Inne tematy\n\
- Stand-upy skracamy do piętnastu minut.\n\
- Nowa osoba w zespole wsparcia zaczyna od poniedziałku.\n\n\
## Do zrobienia\n\
- [ ] Backend: build dla QA w środę wieczorem\n\
- [ ] Wszyscy: oddanie bramki płatniczej w piątek\n",
        turns: &[
            ("me", "Kiedy realnie oddajemy bramkę płatniczą?"),
            ("others", "Umówmy się na piątek i po piątku już nic nie dosuwamy."),
            ("me", "Dobrze, QA dostaje build w środę wieczorem, ma dwa pełne dni."),
        ],
    },
    SyntheticMeeting {
        id: "syn-007",
        title: "Przegląd prototypu ścieżki powitalnej",
        started_at: "2026-06-03T13:00:00Z",
        note_markdown: "# Przegląd prototypu ścieżki powitalnej\n\n\
## Wynik przeglądu\n\
- Prototyp nowej ścieżki powitalnej pokazany na żywo; czas do pierwszej wartości spadł o połowę.\n\
- Lecimy z tym — nowa ścieżka powitalna wchodzi do aplikacji w najbliższym wydaniu.\n\n\
## Uwagi\n\
- Ekran trzeci wymaga krótszego tekstu; copy poprawi zespół produktowy.\n\
- Stara ścieżka zostaje jako fallback przez jedno wydanie.\n\n\
## Do zrobienia\n\
- [ ] Produkt: krótsze copy na ekranie trzecim\n\
- [ ] Frontend: flaga wydania dla nowej ścieżki\n",
        turns: &[
            ("others", "Prototyp ścieżki powitalnej skraca czas do pierwszej wartości o połowę."),
            ("me", "Lecimy z tym, wchodzi do najbliższego wydania."),
            ("others", "Poprawimy jeszcze tekst na trzecim ekranie, jest za długi."),
        ],
    },
    SyntheticMeeting {
        id: "syn-008",
        title: "Design review — checkout",
        started_at: "2026-06-10T15:00:00Z",
        note_markdown: "# Design review — checkout\n\n\
## Outcome\n\
- Walked through the redesigned checkout end to end with real card data on staging.\n\
- Everyone said ship it — the redesigned checkout goes live behind a release toggle next sprint.\n\n\
## Notes\n\
- Error states for declined cards look great; copy reviewed by support.\n\
- Analytics events added for every step, so we can watch drop-off from day one.\n\n\
## Follow-ups\n\
- [ ] Frontend: release toggle wiring\n\
- [ ] Data: drop-off dashboard before rollout\n",
        turns: &[
            ("me", "This is the redesigned checkout, end to end on staging with real card data."),
            ("others", "Honestly — ship it. The declined-card states alone are worth it."),
            ("me", "Then it goes live behind a release toggle next sprint, with the drop-off dashboard ready."),
        ],
    },
    SyntheticMeeting {
        id: "syn-009",
        title: "Burza mózgów — kampania letnia",
        started_at: "2026-05-27T12:00:00Z",
        note_markdown: "# Burza mózgów — kampania letnia\n\n\
## Pomysły\n\
- Współpraca z twórcami internetowymi przy serii krótkich wideo.\n\
- Letni konkurs dla obecnych użytkowników z nagrodami rzeczowymi.\n\n\
## Głosy krytyczne\n\
- Marek: nie jestem przekonany do twórców internetowych, widzę spore ryzyko wizerunkowe\n\
  i koszty bez gwarancji zasięgu.\n\
- Ania: konkurs tak, ale nagrody muszą być sensowne, inaczej nikt się nie ruszy.\n\n\
## Ustalenia\n\
- Robimy pilotaż z jednym twórcą zamiast pełnej serii; decyzja o reszcie po wynikach.\n\n\
## Do zrobienia\n\
- [ ] Marketing: shortlista twórców do pilotażu\n",
        turns: &[
            ("me", "Proponuję serię krótkich wideo z twórcami internetowymi na lato."),
            ("others", "Nie jestem przekonany — spore ryzyko wizerunkowe i koszty bez gwarancji zasięgu."),
            ("me", "To zróbmy pilotaż z jednym twórcą i wróćmy do tematu po wynikach."),
        ],
    },
    SyntheticMeeting {
        id: "syn-010",
        title: "Planowanie sprintu 14",
        started_at: "2026-06-17T09:30:00Z",
        note_markdown: "# Planowanie sprintu 14\n\n\
## Zakres\n\
- Do końca przyszłego tygodnia oddajemy wyszukiwarkę w aplikacji — pełny indeks i podpowiedzi.\n\
- Potem nic nowego nie bierzemy: reszta czasu idzie na dług techniczny i stabilizację.\n\n\
## Podział pracy\n\
- Backend: indeks i ranking wyników.\n\
- Frontend: pole wyszukiwania z podpowiedziami.\n\n\
## Ustalenia\n\
- Codzienne demo wyników wyszukiwania od czwartku.\n\n\
## Do zrobienia\n\
- [ ] Backend: indeks + ranking\n\
- [ ] Frontend: podpowiedzi w polu wyszukiwania\n",
        turns: &[
            ("me", "W tym sprincie oddajemy wyszukiwarkę: pełny indeks i podpowiedzi, do końca przyszłego tygodnia."),
            ("others", "Czyli nic nowego poza tym nie bierzemy?"),
            ("me", "Dokładnie, reszta czasu idzie na dług techniczny i stabilizację."),
        ],
    },
    // ── cross-lingual cluster: PL note ↔ EN query and vice versa ───────────────────────────
    SyntheticMeeting {
        id: "syn-011",
        title: "Kwartalne planowanie budżetu",
        started_at: "2026-05-13T10:00:00Z",
        note_markdown: "# Kwartalne planowanie budżetu\n\n\
## Decyzje\n\
- Tniemy wydatki na narzędzia o piętnaście procent; przegląd licencji do końca miesiąca.\n\
- Wstrzymujemy nowe zatrudnienia poza inżynierią do końca kwartału.\n\
- Zwiększamy środki na infrastrukturę o dziesięć procent w związku z migracją.\n\n\
## Ustalenia\n\
- Każdy zespół przygotuje listę licencji do wypowiedzenia.\n\
- Wracamy do tematu zatrudnień na początku przyszłego kwartału.\n\n\
## Do zrobienia\n\
- [ ] Liderzy zespołów: lista licencji do końca miesiąca\n",
        turns: &[
            ("me", "Tniemy wydatki na narzędzia o piętnaście procent i robimy przegląd licencji."),
            ("others", "A zatrudnienia? Mamy trzy otwarte rekrutacje poza inżynierią."),
            ("me", "Wstrzymujemy je do końca kwartału, wracamy do tematu na początku przyszłego."),
        ],
    },
    SyntheticMeeting {
        id: "syn-012",
        title: "Roadmapa produktu na drugie półrocze",
        started_at: "2026-06-01T10:00:00Z",
        note_markdown: "# Roadmapa produktu na drugie półrocze\n\n\
## Priorytety\n\
1. Aplikacja mobilna — wersja beta we wrześniu.\n\
2. Automatyczne raporty tygodniowe dla klientów biznesowych.\n\
3. Uproszczenie cennika do trzech pakietów.\n\n\
## Poza zakresem\n\
- Integracje z systemami księgowymi przesunięte na przyszły rok.\n\n\
## Ustalenia\n\
- Przegląd postępów co miesiąc, pierwszy na początku lipca.\n\n\
## Do zrobienia\n\
- [ ] Produkt: szczegółowy plan bety mobilnej\n",
        turns: &[
            ("me", "Trzy priorytety na drugie półrocze: aplikacja mobilna, automatyczne raporty i prostszy cennik."),
            ("others", "Co z integracjami księgowymi? Klienci pytają."),
            ("me", "Przesuwamy je na przyszły rok, nie zmieścimy się z betą mobilną."),
        ],
    },
    SyntheticMeeting {
        id: "syn-013",
        title: "Security incident retrospective",
        started_at: "2026-05-15T16:00:00Z",
        note_markdown: "# Security incident retrospective\n\n\
## What happened\n\
- A phishing email harvested one contractor account password; no customer data was accessed.\n\
- The account was disabled within forty minutes of the report.\n\n\
## Agreed actions\n\
- Mandatory MFA rollout for every account, including contractors, within two weeks.\n\
- Quarterly phishing simulation for the whole company.\n\
- Access review: contractors lose standing access to production systems.\n\n\
## Follow-ups\n\
- [ ] IT: MFA rollout plan\n\
- [ ] Security: first phishing simulation scheduled\n",
        turns: &[
            ("me", "Quick timeline: phishing email, one contractor password harvested, account disabled in forty minutes."),
            ("others", "Any customer data touched?"),
            ("me", "No. But we roll out mandatory MFA for everyone within two weeks, contractors included."),
        ],
    },
    SyntheticMeeting {
        id: "syn-014",
        title: "Customer feedback session",
        started_at: "2026-06-05T11:00:00Z",
        note_markdown: "# Customer feedback session\n\n\
## Top complaints\n\
- Synchronization is slow on large workspaces — three enterprise customers raised it unprompted.\n\
- The mobile experience lags behind desktop; two customers called it a blocker for field teams.\n\n\
## What customers praised\n\
- The new reporting layout and export quality.\n\n\
## Decisions\n\
- Performance of synchronization becomes the top engineering priority for June.\n\
- Field-team mobile needs go into the H2 roadmap discussion.\n\n\
## Follow-ups\n\
- [ ] Engineering: profiling plan for large-workspace synchronization\n",
        turns: &[
            ("others", "Three enterprise customers independently said synchronization is slow on large workspaces."),
            ("me", "Then synchronization performance is the top engineering priority for June."),
            ("others", "Two also called the mobile gap a blocker for their field teams."),
        ],
    },
    SyntheticMeeting {
        id: "syn-015",
        title: "Hiring pipeline review",
        started_at: "2026-05-22T13:00:00Z",
        note_markdown: "# Hiring pipeline review\n\n\
## Backend candidates\n\
- Two finalists after the technical rounds; both strong on distributed systems.\n\
- Kasia Zielińska stood out on system design and communication.\n\n\
## Decision\n\
- We extend the offer to Kasia Zielińska this week; the second finalist stays warm as backup.\n\n\
## Pipeline health\n\
- Frontend pipeline is thin — only one candidate past screening; sourcing push needed.\n\n\
## Follow-ups\n\
- [ ] Recruiting: offer letter for Kasia Zielińska\n\
- [ ] Recruiting: frontend sourcing push\n",
        turns: &[
            ("me", "Both backend finalists are strong, but Kasia Zielińska stood out on system design."),
            ("others", "Agreed — extend the offer to Kasia this week, keep the other finalist warm."),
            ("me", "Frontend pipeline is thin though, we need a sourcing push."),
        ],
    },
    // ── temporal cluster: the ONLY meeting in the anchor's "last week" ─────────────────────
    SyntheticMeeting {
        id: "syn-016",
        title: "Retro zespołu",
        started_at: "2026-06-24T15:00:00Z",
        note_markdown: "# Retro zespołu\n\n\
## Co poszło dobrze\n\
- Wyszukiwarka weszła na produkcję bez incydentów.\n\
- Mniej przełączania kontekstu po skróceniu stand-upów.\n\n\
## Co poprawić\n\
- Za dużo spotkań w środy — łączymy przeglądy w jeden blok.\n\
- Testy automatyczne pokrywają za mało ścieżek krytycznych; dopisujemy scenariusze.\n\n\
## Wnioski\n\
- Jedno popołudnie w tygodniu bez spotkań, zaczynamy od przyszłego tygodnia.\n\n\
## Do zrobienia\n\
- [ ] Liderzy: blok przeglądów w środy\n\
- [ ] QA: scenariusze dla ścieżek krytycznych\n",
        turns: &[
            ("me", "Wyszukiwarka weszła bez incydentów, to duży plus."),
            ("others", "Ale środy toną w spotkaniach, proponuję połączyć przeglądy w jeden blok."),
            ("me", "Zgoda, i od przyszłego tygodnia jedno popołudnie całkiem bez spotkań."),
        ],
    },
];

/// Seed the synthetic corpus into `db` (returns the meeting ids in seed order). Everything the
/// real pipeline would index is indexed: `insert_meeting` / `upsert_note` / `insert_segments`
/// populate the three FTS tables via their triggers, and `index_meeting_chunks` writes the
/// note + transcript chunk classes with `embedder` (the `#[ignore]` runner passes
/// `active_embedder()` — the REAL model when present; unit tests pass the deterministic stub).
/// Deterministic by construction: fixed ids/dates/content, fixed derived segment timings.
pub fn seed_synthetic_corpus(db: &Db, embedder: &dyn Embedder) -> Result<Vec<String>> {
    let mut ids = Vec::with_capacity(SYNTHETIC_MEETINGS.len());
    for m in SYNTHETIC_MEETINGS {
        db.insert_meeting(&Meeting {
            id: m.id.to_string(),
            started_at: m.started_at.to_string(),
            ended_at: None,
            title: Some(m.title.to_string()),
            duration_s: 1800,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })?;
        db.upsert_note(&NoteRecord {
            meeting_id: m.id.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: m.note_markdown.to_string(),
            created_at: m.started_at.to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })?;
        // Deterministic turn timings: turn i spans [i*20, i*20+18) seconds.
        let segments: Vec<Segment> = m
            .turns
            .iter()
            .enumerate()
            .map(|(i, (speaker, text))| Segment {
                idx: i as i64,
                start_s: i as f64 * 20.0,
                end_s: i as f64 * 20.0 + 18.0,
                text: (*text).to_string(),
                speaker: Some((*speaker).to_string()),
                confidence: None,
            })
            .collect();
        db.insert_segments(m.id, &segments)?;
        // Mirror the pipeline's auto-index: both chunk classes (note 'voice' + 'transcript'),
        // embedded with the caller's embedder — plus the Brain v2 L1.1 topic chunks, exactly as
        // `index_meeting_if_enabled` writes them (index_meeting_chunks FIRST — its clean-replace
        // purge covers all chunk classes — then the topic pass).
        db.index_meeting_chunks(m.id, &segments, embedder)?;
        db.index_meeting_topic_chunks(m.id, &segments, embedder, &Default::default())?;
        ids.push(m.id.to_string());
    }
    tracing::info!(target: "eval", meetings = ids.len(), "synthetic bake-off corpus seeded");
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Db;

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// A migrated, empty SQLCipher Db under the fixed test DEK (temp-file-backed, headless-safe).
    fn throwaway_db(label: &str) -> Db {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-eval-corpus-{label}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    /// DETERMINISM: seeding twice into two fresh DBs yields identical ids and identical content
    /// (title / started_at / note markdown / segments) for every meeting — the property that makes
    /// `expected_meeting_ids` correct by construction and the committed baseline reproducible.
    #[test]
    fn seed_synthetic_corpus_is_deterministic_across_dbs() {
        let a = throwaway_db("det-a");
        let b = throwaway_db("det-b");
        let stub = crate::embed::StubEmbedder;
        let ids_a = seed_synthetic_corpus(&a, &stub).unwrap();
        let ids_b = seed_synthetic_corpus(&b, &stub).unwrap();
        assert_eq!(ids_a, ids_b, "seed order + ids must be identical");
        assert_eq!(ids_a.len(), SYNTHETIC_MEETINGS.len());
        for id in &ids_a {
            let ma = a.get_meeting(id).unwrap().expect("meeting in db a");
            let mb = b.get_meeting(id).unwrap().expect("meeting in db b");
            assert_eq!(ma.title, mb.title);
            assert_eq!(ma.started_at, mb.started_at);
            let na = a.get_note(id, "claude_code").unwrap().expect("note in a");
            let nb = b.get_note(id, "claude_code").unwrap().expect("note in b");
            assert_eq!(na.markdown, nb.markdown, "note content must be identical");
            let sa = a.get_segments(id).unwrap();
            let sb = b.get_segments(id).unwrap();
            assert!(!sa.is_empty(), "every synthetic meeting has segments");
            assert_eq!(sa.len(), sb.len());
            for (x, y) in sa.iter().zip(sb.iter()) {
                assert_eq!(x.text, y.text);
                assert_eq!(x.speaker, y.speaker);
                assert_eq!(x.start_s, y.start_s);
                assert_eq!(x.end_s, y.end_s);
            }
        }
    }

    /// STRUCTURE: 16 meetings, unique ids in `syn-NNN` form, fixed dates strictly BEFORE the fixed
    /// anchor (never `now()`), and exactly one meeting in the anchor's "last week"
    /// (2026-06-22..2026-06-28) so the temporal queries have a unique answer set.
    #[test]
    fn synthetic_corpus_structure_is_fixed() {
        assert_eq!(SYNTHETIC_MEETINGS.len(), 16);
        let mut ids: Vec<&str> = SYNTHETIC_MEETINGS.iter().map(|m| m.id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "synthetic meeting ids must be unique");
        for m in SYNTHETIC_MEETINGS {
            assert!(m.id.starts_with("syn-"), "id form: {}", m.id);
            assert!(
                m.started_at < CORPUS_ANCHOR_DATE,
                "{} must start before the fixed anchor {CORPUS_ANCHOR_DATE}",
                m.id
            );
            assert!(!m.note_markdown.trim().is_empty());
            assert!(!m.turns.is_empty());
        }
        // Exactly ONE meeting in the anchor-relative "last week" window.
        let last_week: Vec<&str> = SYNTHETIC_MEETINGS
            .iter()
            .filter(|m| {
                let day = &m.started_at[..10];
                ("2026-06-22".."2026-06-29").contains(&day)
            })
            .map(|m| m.id)
            .collect();
        assert_eq!(
            last_week,
            vec!["syn-016"],
            "the temporal 'last week' answer set must be unique"
        );
    }

    /// FIXTURE ↔ CORPUS consistency: the committed labeled set parses, has 20 entries (5 per
    /// category via the `_comment` tag — unknown JSON fields are tolerated because `LabeledQuery`
    /// does not `deny_unknown_fields`), and every `expected_meeting_ids` entry names a seeded
    /// synthetic meeting — the "correct by construction" property.
    #[test]
    fn synthetic_fixture_parses_and_matches_corpus() {
        let json = include_str!("fixtures/rag-bakeoff-synthetic.json");
        let set = crate::eval::LabeledSet::from_json(json).unwrap();
        assert_eq!(set.len(), 20, "20 labeled queries");
        let corpus_ids: std::collections::HashSet<&str> =
            SYNTHETIC_MEETINGS.iter().map(|m| m.id).collect();
        for q in &set.0 {
            assert!(
                !q.expected_meeting_ids.is_empty(),
                "query {:?} must have a gold set",
                q.query
            );
            for id in &q.expected_meeting_ids {
                assert!(
                    corpus_ids.contains(id.as_str()),
                    "fixture expects unknown meeting id {id} (query {:?})",
                    q.query
                );
            }
        }
        // Category balance via the informational `_comment` tags: 5 per category.
        let raw: serde_json::Value = serde_json::from_str(json).unwrap();
        let entries = raw.as_array().unwrap();
        assert_eq!(entries.len(), 20);
        for cat in ["entity-anchored", "paraphrase", "cross-lingual", "temporal"] {
            let count = entries
                .iter()
                .filter(|e| {
                    e.get("_comment")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c.starts_with(cat))
                })
                .count();
            assert_eq!(count, 5, "expected 5 '{cat}' queries");
        }
    }
}
