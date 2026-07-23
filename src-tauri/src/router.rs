//! Brain v2 L3 — the EXPLICIT, PURE routing decision layer over the existing roles/postures data.
//!
//! Today Murmur's dispatch decisions live implicitly inside the call sites: the agentic-eligibility
//! gate (`!roles::resolve(role, cfg).is_reasoner_only()` in `transcribe::live::run_informational` /
//! `commands::ask_vault`) plus the `ReasonerCell` connection match. This module makes the decision
//! EXPLICIT and unit-testable: [`route`] maps a [`RouterInput`] (role + config + a coarse
//! [`QueryClass`] + local-model availability) to ONE [`RouteDecision`].
//!
//! ## ADDITIVE, not yet wired into dispatch (load-bearing)
//! Per spec §L3, NOTHING dispatches through this yet — the legacy paths keep deciding. The one
//! integration point is the SHADOW ROUTER log in `transcribe::live::run_informational`: it logs
//! (debug, content-free) the decision this router WOULD take next to what the legacy path chose,
//! so parity can be validated on real usage before any cutover. Divergence is EXPECTED for the
//! `local` connection (the router plans local-model tiers the legacy path floors on) — that gap is
//! exactly what the shadow log measures.
//!
//! Everything here is pure (config + booleans in, decision out): no I/O except the caller-side
//! [`class_model_available`] filesystem probe, no consent/egress logic — those stay inside the
//! provider factory and can never be bypassed by a routing decision.

use crate::reason::{class_model_id, resolve_brain_model, ModelClass};
use crate::settings::AppConfig;
use crate::summarize::roles::{self, Role};

/// A coarse, content-derived class of what the user is asking for. Computed by the KEYWORD
/// classifier [`classify_query`] today (a model classifier may replace it later — the enum is the
/// stable seam). Only the CLASS is ever logged; never the query text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryClass {
    /// "What did we decide / when did X happen" — a lookup over owned notes.
    Recall,
    /// "Summarize / compare / analyze across meetings" — synthesis work (the heavy-model class).
    Synthesis,
    /// Needs the outside world (web/news/Jira/Slack) — connector territory.
    External,
    /// No keyword matched — the conservative default.
    Unknown,
}

impl QueryClass {
    /// Stable lowercase token for logs. Never PII.
    pub fn as_str(self) -> &'static str {
        match self {
            QueryClass::Recall => "recall",
            QueryClass::Synthesis => "synthesis",
            QueryClass::External => "external",
            QueryClass::Unknown => "unknown",
        }
    }
}

/// Classify a user query by KEYWORDS (English + Polish, diacritic and diacritic-less variants).
/// Deliberately simple and conservative: precedence External > Synthesis > Recall (an "analyze the
/// news about X" question needs connectors more than it needs the heavy model), default `Unknown`.
/// Pure + headless-testable; a future model classifier slots in behind the same [`QueryClass`].
pub fn classify_query(query: &str) -> QueryClass {
    let q = query.to_lowercase();
    // External keywords match on WORD BOUNDARIES (never substrings): Polish inflections routinely
    // CONTAIN them ("webinaru" ⊃ "web", "newsletterze" ⊃ "news") and a substring match poisons
    // the shadow-router parity data (adversarial finding 2026-07-10). "online" was dropped — as a
    // standalone word it describes owned content ("trendy sprzedaży online") as often as the web.
    const EXTERNAL: &[&str] = &[
        "web",
        "google",
        "news",
        "weather",
        "pogoda",
        "jira",
        "slack",
        "internet",
        "wyszukaj w",
        "search the",
    ];
    const SYNTHESIS: &[&str] = &[
        "summarize",
        "summary",
        "podsumuj",
        "podsumowanie",
        "compare",
        "porównaj",
        "porownaj",
        "analyze",
        "analyse",
        "przeanalizuj",
        "across",
        "trend",
        "overview",
        "synthesize",
        "zestawienie",
    ];
    const RECALL: &[&str] = &[
        "when did",
        "what did",
        "what was",
        "who said",
        "did we",
        "kiedy",
        "co ustalil", // matches "co ustaliliśmy" / "co ustalili"
        "co powiedzial",
        "czy ustali",
        "co mówil",
        "co mowil",
    ];
    if EXTERNAL.iter().any(|k| contains_word(&q, k)) {
        return QueryClass::External;
    }
    // Synthesis/Recall stay SUBSTRING matches on purpose — their Polish entries are deliberate
    // stems ("co ustalil" matches "co ustaliliśmy", "trend" matches "trendy").
    if SYNTHESIS.iter().any(|k| q.contains(k)) {
        return QueryClass::Synthesis;
    }
    if RECALL.iter().any(|k| q.contains(k)) {
        return QueryClass::Recall;
    }
    QueryClass::Unknown
}

/// Does `haystack` contain `needle` as a whole word/phrase — i.e. NOT butted against another
/// alphanumeric char on either side? ("web" matches "search the web" but not "webinaru".)
/// UTF-8-safe: `find` returns char-boundary offsets for a char-boundary needle, and the
/// neighbour checks read whole `char`s (Polish diacritics count as word chars).
fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .map_or(true, |c| !c.is_alphanumeric()); // MSRV 1.77: no Option::is_none_or (1.82)
        let after_ok = haystack[end..]
            .chars()
            .next()
            .map_or(true, |c| !c.is_alphanumeric()); // MSRV 1.77: no Option::is_none_or (1.82)
        if before_ok && after_ok {
            return true;
        }
        // Advance past the first char of this (rejected) match and keep scanning.
        from = start
            + haystack[start..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
    }
    false
}

/// The routing decision for one request. The four ways Murmur can serve a brain turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// The deterministic, zero-model floor (gated reads + template synthesis).
    DeterministicFloor,
    /// The on-device LIGHT engine (fast, small-context).
    LocalLight,
    /// The on-device HEAVY engine (synthesis-class work).
    LocalHeavy,
    /// The cloud agentic loop on `connection` (a provider id — consent + redaction gates apply
    /// INSIDE the provider factory; the router only names the target, it can't bypass them).
    CloudAgentic { connection: String },
}

impl RouteDecision {
    /// Stable, CONTENT-FREE label for logs (the shadow-router line). Never carries the query.
    pub fn label(&self) -> &'static str {
        match self {
            RouteDecision::DeterministicFloor => "floor",
            RouteDecision::LocalLight => "local_light",
            RouteDecision::LocalHeavy => "local_heavy",
            RouteDecision::CloudAgentic { .. } => "cloud_agentic",
        }
    }
}

/// Everything [`route`] decides from. `heavy_available` / `light_available` are the CALLER-probed
/// "is that class's GGUF resolvable on disk" booleans ([`class_model_available`]) — injected so the
/// decision table stays pure and exhaustively testable.
pub struct RouterInput<'a> {
    pub role: Role,
    pub config: &'a AppConfig,
    pub query_class: QueryClass,
    pub heavy_available: bool,
    pub light_available: bool,
}

/// THE decision table (spec §L3). Keyed on the role's RESOLVED connection
/// ([`roles::resolve`] — explicit role keys win, else the exact legacy fallback):
///
/// - `off`   → [`RouteDecision::DeterministicFloor`] (the stub floor, exactly today's dispatch).
/// - `local` → the HEAVY engine when it is available AND the query is [`QueryClass::Synthesis`];
///   else the LIGHT engine when available; else the floor. (Spec table verbatim — a heavy-only
///   install asking a non-synthesis question floors; revisit with shadow-log data before cutover.)
/// - `apple` → the floor, conservatively: the AFM sidecar serves no light/heavy GGUF class, and
///   AFM routing is not specced — matching today's "reasoner-only ⇒ no agentic loop" behavior.
/// - any provider connection → [`RouteDecision::CloudAgentic`] on that connection.
pub fn route(input: &RouterInput) -> RouteDecision {
    let target = roles::resolve(input.role, input.config);
    match target.connection.as_str() {
        roles::CONN_OFF => RouteDecision::DeterministicFloor,
        roles::CONN_LOCAL => {
            if input.heavy_available && input.query_class == QueryClass::Synthesis {
                RouteDecision::LocalHeavy
            } else if input.light_available {
                RouteDecision::LocalLight
            } else {
                RouteDecision::DeterministicFloor
            }
        }
        roles::CONN_AFM => RouteDecision::DeterministicFloor,
        _ => RouteDecision::CloudAgentic {
            connection: target.connection,
        },
    }
}

/// Is `class`'s effective model (explicit pick, else the registry default) RESOLVABLE on disk?
/// The availability probe [`RouterInput`] is fed with — a cheap filesystem existence check
/// (mirrors `ReasonerCell::class_or_stub`'s presence recheck), never a model load. Content-free.
pub fn class_model_available(cfg: &AppConfig, class: ModelClass) -> bool {
    let id = class_model_id(cfg, class);
    resolve_brain_model(None, id.as_deref())
        .ok()
        .flatten()
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::BrainBackend;

    fn cfg(backend: BrainBackend) -> AppConfig {
        AppConfig {
            provider_id: "claude_code".to_string(),
            brain_backend: backend,
            ..AppConfig::default()
        }
    }

    const ALL_CLASSES: [QueryClass; 4] = [
        QueryClass::Recall,
        QueryClass::Synthesis,
        QueryClass::External,
        QueryClass::Unknown,
    ];

    fn input(config: &AppConfig, qc: QueryClass, heavy: bool, light: bool) -> RouterInput<'_> {
        RouterInput {
            role: Role::Live,
            config,
            query_class: qc,
            heavy_available: heavy,
            light_available: light,
        }
    }

    /// OFF → the deterministic floor, for EVERY query class and EVERY availability combination.
    #[test]
    fn route_off_is_always_the_floor() {
        let c = cfg(BrainBackend::Off);
        for qc in ALL_CLASSES {
            for heavy in [false, true] {
                for light in [false, true] {
                    assert_eq!(
                        route(&input(&c, qc, heavy, light)),
                        RouteDecision::DeterministicFloor,
                        "{qc:?}/h{heavy}/l{light}"
                    );
                }
            }
        }
    }

    /// LOCAL — the full spec table, exhaustively: heavy+Synthesis → heavy; else light-if-available;
    /// else floor (including the heavy-only non-synthesis corner, which the spec table floors).
    #[test]
    fn route_local_decision_table_is_exhaustive() {
        let c = cfg(BrainBackend::Local);
        for qc in ALL_CLASSES {
            for heavy in [false, true] {
                for light in [false, true] {
                    let want = if heavy && qc == QueryClass::Synthesis {
                        RouteDecision::LocalHeavy
                    } else if light {
                        RouteDecision::LocalLight
                    } else {
                        RouteDecision::DeterministicFloor
                    };
                    assert_eq!(
                        route(&input(&c, qc, heavy, light)),
                        want,
                        "{qc:?}/h{heavy}/l{light}"
                    );
                }
            }
        }
    }

    /// CLOUD → CloudAgentic on the RESOLVED connection, for every provider id and query class
    /// (availability of local models is irrelevant on a cloud target).
    #[test]
    fn route_cloud_targets_the_resolved_connection() {
        for provider in ["claude_code", "anthropic", "ollama", "gateway"] {
            let c = AppConfig {
                provider_id: provider.to_string(),
                brain_backend: BrainBackend::Cloud,
                ..AppConfig::default()
            };
            for qc in ALL_CLASSES {
                assert_eq!(
                    route(&input(&c, qc, true, true)),
                    RouteDecision::CloudAgentic {
                        connection: provider.to_string()
                    },
                    "{provider}/{qc:?}"
                );
            }
        }
    }

    /// An EXPLICIT role connection key wins over the legacy backend (the resolver's contract,
    /// honored by the router): Live pinned to `local` routes local even under a Cloud backend.
    #[test]
    fn route_honors_explicit_role_keys() {
        let c = AppConfig {
            role_live_connection: "local".to_string(),
            brain_backend: BrainBackend::Cloud,
            ..AppConfig::default()
        };
        assert_eq!(
            route(&input(&c, QueryClass::Synthesis, true, true)),
            RouteDecision::LocalHeavy
        );
        // And an explicit cloud key under an Off backend routes cloud.
        let c2 = AppConfig {
            role_live_connection: "anthropic".to_string(),
            brain_backend: BrainBackend::Off,
            ..AppConfig::default()
        };
        assert_eq!(
            route(&input(&c2, QueryClass::Unknown, false, false)),
            RouteDecision::CloudAgentic {
                connection: "anthropic".to_string()
            }
        );
    }

    /// AFM (Apple Foundation) → the conservative floor (no light/heavy GGUF class to route to).
    #[test]
    fn route_afm_is_the_floor() {
        let c = cfg(BrainBackend::AppleFoundation);
        for qc in ALL_CLASSES {
            assert_eq!(
                route(&input(&c, qc, true, true)),
                RouteDecision::DeterministicFloor,
                "{qc:?}"
            );
        }
    }

    /// The keyword classifier: representative EN + PL phrases per class, precedence
    /// External > Synthesis > Recall, and the Unknown default.
    #[test]
    fn classify_query_keywords_and_precedence() {
        assert_eq!(
            classify_query("what did we decide on pricing?"),
            QueryClass::Recall
        );
        assert_eq!(
            classify_query("co ustaliliśmy z Weroniką?"),
            QueryClass::Recall
        );
        assert_eq!(classify_query("kiedy był deadline?"), QueryClass::Recall);
        assert_eq!(
            classify_query("summarize the last three syncs"),
            QueryClass::Synthesis
        );
        assert_eq!(
            classify_query("podsumuj to spotkanie"),
            QueryClass::Synthesis
        );
        assert_eq!(
            classify_query("porównaj oba podejścia"),
            QueryClass::Synthesis
        );
        assert_eq!(
            classify_query("jaka jest pogoda w Krakowie"),
            QueryClass::External
        );
        assert_eq!(
            classify_query("check Jira for the login bug"),
            QueryClass::External
        );
        // Precedence: an external keyword wins even when a synthesis keyword is present.
        assert_eq!(
            classify_query("summarize the news about the acquisition"),
            QueryClass::External
        );
        // No keyword → Unknown (the conservative default).
        assert_eq!(classify_query("hello there"), QueryClass::Unknown);
        assert_eq!(classify_query(""), QueryClass::Unknown);
    }

    /// REGRESSION (adversarial finding 2026-07-10, MINOR — RED on substring matching): Polish
    /// inflections that merely CONTAIN an External keyword ("webinaru" ⊃ "web", "newsletterze" ⊃
    /// "news") and the ambiguous standalone "online" must NOT classify as External — they poison
    /// the shadow-router parity data. External keywords match on WORD BOUNDARIES; "online" is
    /// dropped from the list (too ambiguous — it describes owned content as often as the web).
    #[test]
    fn classify_query_external_keywords_do_not_misfire_on_substrings() {
        assert_eq!(
            classify_query("podsumuj notatki z webinaru"),
            QueryClass::Synthesis,
            "'webinaru' must not substring-match 'web'"
        );
        assert_eq!(
            classify_query("co ustaliliśmy o newsletterze"),
            QueryClass::Recall,
            "'newsletterze' must not substring-match 'news'"
        );
        assert_eq!(
            classify_query("porównaj trendy sprzedaży online"),
            QueryClass::Synthesis,
            "standalone 'online' is too ambiguous to mean External"
        );
        // The genuine whole-word External hits keep firing.
        assert_eq!(
            classify_query("search the web for it"),
            QueryClass::External
        );
        assert_eq!(
            classify_query("summarize the news about the acquisition"),
            QueryClass::External
        );
    }

    /// Labels are stable, content-free log tokens.
    #[test]
    fn route_decision_labels_are_stable() {
        assert_eq!(RouteDecision::DeterministicFloor.label(), "floor");
        assert_eq!(RouteDecision::LocalLight.label(), "local_light");
        assert_eq!(RouteDecision::LocalHeavy.label(), "local_heavy");
        assert_eq!(
            RouteDecision::CloudAgentic {
                connection: "claude_code".to_string()
            }
            .label(),
            "cloud_agentic"
        );
        assert_eq!(QueryClass::Recall.as_str(), "recall");
        assert_eq!(QueryClass::Synthesis.as_str(), "synthesis");
        assert_eq!(QueryClass::External.as_str(), "external");
        assert_eq!(QueryClass::Unknown.as_str(), "unknown");
    }

    /// `class_model_available` is a pure presence probe: with no model selected and none on disk it
    /// is false for both classes (the default test environment has no GGUF in the models dir —
    /// and if one ever is present, the probe returning true is equally correct, so assert only
    /// that the call is crash-free and boolean-stable across repeat calls).
    #[test]
    fn class_model_available_is_crash_free_and_stable() {
        let c = AppConfig::default();
        let h1 = class_model_available(&c, ModelClass::Heavy);
        let h2 = class_model_available(&c, ModelClass::Heavy);
        let l1 = class_model_available(&c, ModelClass::Light);
        let l2 = class_model_available(&c, ModelClass::Light);
        assert_eq!(h1, h2);
        assert_eq!(l1, l2);
    }
}
