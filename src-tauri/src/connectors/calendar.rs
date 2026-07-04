//! LOCAL CALENDAR connector — surfaces the user's on-device macOS calendar as live brain context
//! ("who's in my next meeting", a pre-meeting brief). The CALENDAR leg of the multi-source roadmap.
//!
//! ## Egress posture (NO new egress class — audited by the lock-security reviewer)
//! This connector is [`EgressClass::Local`]. Reading the local calendar is ON-DEVICE: the events
//! are pulled by the bundled `meetnotes-calendar` EventKit sidecar ([`crate::calendar::fetch_events`])
//! and handed to this connector as a plain `Vec<CalendarEventFull>` — NOTHING leaves the device.
//! Therefore it is correctly NOT consent-gated (unlike the [`EgressClass::External`] web connector):
//! there is no off-device request to gate. It needs the macOS Calendars (TCC) permission at runtime,
//! but the upstream fetch degrades to an empty `Vec` on a denied/missing permission, so a denied
//! permission simply yields no hits here (graceful, never an error).
//!
//! If the resulting context text is LATER folded into a CLOUD brain prompt, it rides the EXISTING
//! `make_provider` redaction firewall + consent exactly like the transcript — this connector creates
//! no network path of its own, and adds no new egress class.
//!
//! ## Stateless by design (blueprint Option A)
//! The connector holds the already-fetched events; it does NOT itself touch EventKit or hold an
//! `AppHandle`. This keeps the [`crate::connectors::ConnectorRegistry`] (which is built from the
//! config alone) AppHandle-free, and keeps this connector unit-testable headless over a fixed event
//! set with NO EventKit. The async fetch lives at the dispatch site ([`crate::tools::execute_calendar_search`]).
//!
//! ## No PII in logs
//! This module logs nothing — the events carry attendee names + agenda text (PII). The match is
//! purely in-memory; only hit counts (if logged at all by the caller) ever leave this seam.

use async_trait::async_trait;

use super::{Connector, ConnectorHit, ConnectorResult, EgressClass};
use crate::storage::models::{CalendarContext, CalendarEventFull};

/// The loud attribution label every calendar hit carries, so the brain's answer is visibly grounded
/// on the user's calendar (and never silently passed off as vault knowledge).
const SOURCE_LABEL: &str = "calendar";

/// The local calendar connector — wraps a snapshot of the user's calendar events behind the
/// [`Connector`] seam. Stateless w.r.t. EventKit: the events are fetched by the caller
/// ([`crate::calendar::fetch_events`]) and passed in, so this type does no I/O and holds no handle.
pub struct CalendarConnector {
    events: Vec<CalendarEventFull>,
}

impl CalendarConnector {
    /// Build a connector over an already-fetched event snapshot. An empty `Vec` (no calendar access,
    /// denied permission, no events in the window) is a valid input → it simply yields no hits.
    pub fn new(events: Vec<CalendarEventFull>) -> Self {
        Self { events }
    }

    /// Does this event match `needle` (an already-lowercased query)? Matches against the event's
    /// title, any attendee name, and the agenda/notes — the fields the brain would ground a
    /// "who's in / what's the agenda" answer on. Pure + case-insensitive.
    fn event_matches(ev: &CalendarEventFull, needle: &str) -> bool {
        if ev.title.to_lowercase().contains(needle) {
            return true;
        }
        if ev.notes.to_lowercase().contains(needle) {
            return true;
        }
        ev.attendees
            .iter()
            .any(|a| a.to_lowercase().contains(needle))
    }

    /// Map an event to a [`ConnectorHit`] whose snippet is the bounded [`CalendarContext`] block
    /// (Meeting / When / Attendees / Agenda). `url` is empty — a local calendar event has no URL;
    /// the loud `source_label` is what attributes it.
    fn hit_for(ev: &CalendarEventFull) -> ConnectorHit {
        ConnectorHit {
            title: ev.title.clone(),
            snippet: CalendarContext::from_event(ev).text,
            url: String::new(),
            source_label: SOURCE_LABEL.to_string(),
        }
    }
}

#[async_trait]
impl Connector for CalendarConnector {
    fn id(&self) -> &str {
        "calendar"
    }

    fn egress_class(&self) -> EgressClass {
        // ON-DEVICE: reading the local calendar reaches no external service → never consent-gated.
        EgressClass::Local
    }

    async fn search(&self, query: &str) -> ConnectorResult {
        let needle = query.trim().to_lowercase();
        // DESIGN CHOICE: an empty/whitespace query returns ALL events in the fetched window (the
        // "what's coming up / who's in my next meeting" case), rather than nothing — the caller
        // already bounds the window (e.g. [now-60m, now+720m]) when it fetches, so "all of it" is a
        // small, intentional set, not an unbounded dump. A non-empty query filters case-insensitively.
        let hits = self
            .events
            .iter()
            .filter(|ev| needle.is_empty() || Self::event_matches(ev, &needle))
            .map(Self::hit_for)
            .collect();
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// A fixed event set — NO EventKit. Mirrors the calendar.rs sidecar-parser fixtures.
    fn fixture_events() -> Vec<CalendarEventFull> {
        vec![
            CalendarEventFull {
                id: "E1".into(),
                title: "Sprint Planning".into(),
                start: Some("2026-06-28T10:00:00Z".into()),
                end: Some("2026-06-28T11:00:00Z".into()),
                attendees: vec!["Alice".into(), "bob@example.com".into()],
                notes: "Agenda:\n- velocity\n- scope".into(),
            },
            CalendarEventFull {
                id: "E2".into(),
                title: "1:1 with Carol".into(),
                start: Some("2026-06-28T14:00:00Z".into()),
                end: None,
                attendees: vec!["Carol".into()],
                notes: String::new(),
            },
        ]
    }

    #[test]
    fn matches_query_against_title() {
        let c = CalendarConnector::new(fixture_events());
        let hits = block_on(c.search("sprint")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Sprint Planning");
        // The snippet is the bounded CalendarContext block (Meeting / When / Attendees / Agenda).
        assert!(hits[0].snippet.contains("Meeting: Sprint Planning"));
        assert!(hits[0]
            .snippet
            .contains("Attendees: Alice, bob@example.com"));
        assert!(hits[0].snippet.contains("velocity"));
        // LOUD: every hit is attributed to the calendar; local events have no URL.
        assert_eq!(hits[0].source_label, "calendar");
        assert_eq!(hits[0].url, "");
    }

    #[test]
    fn matches_query_against_attendee() {
        let c = CalendarConnector::new(fixture_events());
        // "carol" matches the attendee on E2 (case-insensitive), not the title.
        let hits = block_on(c.search("CAROL")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "1:1 with Carol");
    }

    #[test]
    fn matches_query_against_agenda_notes() {
        let c = CalendarConnector::new(fixture_events());
        // "velocity" appears only in E1's agenda/notes.
        let hits = block_on(c.search("velocity")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Sprint Planning");
    }

    #[test]
    fn no_match_yields_no_hits() {
        let c = CalendarConnector::new(fixture_events());
        let hits = block_on(c.search("nonexistent-topic-zzz")).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn empty_events_yields_no_hits_gracefully() {
        // The denied-permission / no-calendar-access shape: fetch_events returned an empty Vec.
        let c = CalendarConnector::new(Vec::new());
        assert!(block_on(c.search("anything")).unwrap().is_empty());
        assert!(block_on(c.search("")).unwrap().is_empty());
    }

    #[test]
    fn empty_query_returns_all_events_in_window() {
        // Empty query = "what's coming up" → every fetched event (the caller already bounds the
        // window), each as a loud calendar hit.
        let c = CalendarConnector::new(fixture_events());
        let hits = block_on(c.search("   ")).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.source_label == "calendar"));
    }

    #[test]
    fn id_and_egress_class_are_local() {
        let c = CalendarConnector::new(Vec::new());
        assert_eq!(c.id(), "calendar");
        // ON-DEVICE: Local, never consent-gated.
        assert_eq!(c.egress_class(), EgressClass::Local);
    }
}
