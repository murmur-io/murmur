//! Brain v2 L1.5 — time-aware query expansion: a pure PL+EN parser that extracts a calendar
//! window from a natural-language query ("what did we discuss last week?" / "jakie wnioski z
//! zeszłego tygodnia?") so retrieval can add a `started_at` range filter to every leg.
//!
//! Pure + deterministic: `parse_temporal_constraint(query, today)` takes the anchor date
//! EXPLICITLY (the app passes `chrono::Utc::now().date_naive()`; the eval harness passes the
//! FIXED corpus anchor so the labeled set never rots). The ORIGINAL query text is NOT stripped —
//! BM25 tolerates the extra temporal tokens (spec §L1.5).
//!
//! Weeks are Monday-anchored (the PL convention and ISO-8601). All returned ranges are
//! half-open: `(from_inclusive, to_exclusive)`.

use chrono::{Datelike, Days, NaiveDate};
use regex::Regex;
use std::sync::OnceLock;

/// EN month names → month number (1-based).
const MONTHS_EN: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// PL month names in the GENITIVE (the form that follows "tygodniu"/"tydzień": "czerwca",
/// "pierwszym tygodniu czerwca"), plus diacritic-stripped variants folded in [`pl_month_number`].
const MONTHS_PL: [&str; 12] = [
    "stycznia",
    "lutego",
    "marca",
    "kwietnia",
    "maja",
    "czerwca",
    "lipca",
    "sierpnia",
    "września",
    "października",
    "listopada",
    "grudnia",
];

fn en_month_number(name: &str) -> Option<u32> {
    MONTHS_EN
        .iter()
        .position(|m| *m == name)
        .map(|i| i as u32 + 1)
}

fn pl_month_number(name: &str) -> Option<u32> {
    let stripped: String = name
        .chars()
        .map(|c| match c {
            'ś' => 's',
            'ź' => 'z',
            'ż' => 'z',
            'ą' => 'a',
            'ę' => 'e',
            'ó' => 'o',
            'ł' => 'l',
            'ć' => 'c',
            'ń' => 'n',
            other => other,
        })
        .collect();
    MONTHS_PL
        .iter()
        .position(|m| {
            let m_stripped: String = m
                .chars()
                .map(|c| match c {
                    'ś' => 's',
                    'ź' => 'z',
                    'ż' => 'z',
                    'ą' => 'a',
                    'ę' => 'e',
                    'ó' => 'o',
                    'ł' => 'l',
                    'ć' => 'c',
                    'ń' => 'n',
                    other => other,
                })
                .collect();
            m_stripped == stripped
        })
        .map(|i| i as u32 + 1)
}

/// The Monday of the ISO week containing `d`.
fn monday_of(d: NaiveDate) -> NaiveDate {
    let back = d.weekday().num_days_from_monday() as u64;
    d.checked_sub_days(Days::new(back)).unwrap_or(d)
}

/// First day of `d`'s month.
fn month_start(d: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap_or(d)
}

/// First day of the month AFTER `d`'s month.
fn next_month_start(d: NaiveDate) -> NaiveDate {
    let (y, m) = if d.month() == 12 {
        (d.year() + 1, 1)
    } else {
        (d.year(), d.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(d)
}

/// A small EN+PL number-word map for "two weeks ago"/"dwa tygodnie temu". Digits are handled by
/// the regexes directly.
fn number_word(w: &str) -> Option<u64> {
    match w {
        "one" | "jeden" => Some(1),
        "two" | "dwa" => Some(2),
        "three" | "trzy" => Some(3),
        "four" | "cztery" => Some(4),
        "five" | "pięć" | "piec" => Some(5),
        _ => w.parse::<u64>().ok(),
    }
}

// Compiled once. All patterns run on the LOWERCASED query; `regex` is Unicode-aware, so `\w`/`\b`
// handle Polish diacritics. Diacritic-stripped PL variants are included (users type both).
fn re_iso() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b(\d{4})-(\d{2})-(\d{2})\b").expect("static regex"))
}
fn re_last_n_days() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\b(?:last|past)\s+(\d{1,3})\s+days\b|\bostatni(?:e|ch)\s+(\d{1,3})\s+dni\b")
            .expect("static regex")
    })
}
fn re_weeks_ago() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
    Regex::new(
        r"\b(\d{1,2}|one|two|three|four|five|jeden|dwa|trzy|cztery|pięć|piec)\s+(?:weeks?|tygodni(?:e|a)?|tygodnie)\s+(?:ago|temu)\b",
    )
    .expect("static regex")
})
}
fn re_a_week_ago() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\b(?:a\s+week\s+ago)\b|\btydzie[ńn]\s+temu\b").expect("static regex")
    })
}
fn re_week_of() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
    Regex::new(
        r"\bweek of (january|february|march|april|may|june|july|august|september|october|november|december)\s+(\d{1,2})\b",
    )
    .expect("static regex")
})
}
fn re_first_week_of() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
    Regex::new(
        r"\bfirst week of (january|february|march|april|may|june|july|august|september|october|november|december)\b|\bpierwsz\w*\s+(?:tydzień|tydzien|tygodniu|tygodnia)\s+(\w+)\b",
    )
    .expect("static regex")
})
}
fn re_yesterday() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\byesterday\b|\bwczoraj\b").expect("static regex"))
}
fn re_today() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\btoday\b|\bdzisiaj\b|\bdziś\b|\bdzis\b").expect("static regex"))
}
fn re_last_week() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
    Regex::new(
        r"\blast week\b|\b(?:zeszł|zeszl|poprzedni|ubiegł|ubiegl|ostatni)\w*\s+(?:tygodniu|tygodnia|tydzień|tydzien)\b",
    )
    .expect("static regex")
})
}
fn re_this_week() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\bthis week\b|\b(?:tym|bieżąc\w*|biezac\w*)\s+tygodniu\b|\btego tygodnia\b")
            .expect("static regex")
    })
}
fn re_last_month() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
    Regex::new(
        r"\blast month\b|\b(?:zeszł|zeszl|poprzedni|ubiegł|ubiegl)\w*\s+(?:miesiącu|miesiacu|miesiąca|miesiaca)\b",
    )
    .expect("static regex")
})
}
fn re_this_month() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"\bthis month\b|\btym\s+(?:miesiącu|miesiacu)\b|\btego\s+(?:miesiąca|miesiaca)\b",
        )
        .expect("static regex")
    })
}

/// Extract a calendar window from a natural-language PL/EN query, anchored at `today`.
/// Returns `Some((from_inclusive, to_exclusive))` or `None` when the query names no time frame.
///
/// Precedence (first match wins): explicit ISO dates → "last/past N days" → "N weeks ago" →
/// "week of <month> <day>" → "first week of <month>" → yesterday → today → last week →
/// this week → last month → this month. Pure — no clock, no I/O.
pub fn parse_temporal_constraint(query: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let q = query.to_lowercase();
    let day = Days::new(1);
    let week = Days::new(7);

    // 1) Explicit ISO dates: one date = that day; two or more = the [min, max] range.
    let mut iso: Vec<NaiveDate> = re_iso()
        .captures_iter(&q)
        .filter_map(|c| {
            let y: i32 = c.get(1)?.as_str().parse().ok()?;
            let m: u32 = c.get(2)?.as_str().parse().ok()?;
            let d: u32 = c.get(3)?.as_str().parse().ok()?;
            NaiveDate::from_ymd_opt(y, m, d)
        })
        .collect();
    if !iso.is_empty() {
        iso.sort_unstable();
        let from = iso[0];
        let to = iso[iso.len() - 1].checked_add_days(day)?;
        return Some((from, to));
    }

    // 2) last/past N days · ostatnie N dni.
    if let Some(c) = re_last_n_days().captures(&q) {
        let n: u64 = c
            .get(1)
            .or_else(|| c.get(2))?
            .as_str()
            .parse()
            .ok()
            .filter(|n| *n > 0)?;
        let from = today.checked_sub_days(Days::new(n))?;
        return Some((from, today.checked_add_days(day)?));
    }

    // 3) N weeks ago · N tygodnie temu (the ISO week that many weeks back).
    if let Some(c) = re_weeks_ago().captures(&q) {
        let n = number_word(c.get(1)?.as_str()).filter(|n| *n > 0)?;
        let from = monday_of(today).checked_sub_days(Days::new(7 * n))?;
        return Some((from, from.checked_add_days(week)?));
    }
    if re_a_week_ago().is_match(&q) {
        let from = monday_of(today).checked_sub_days(week)?;
        return Some((from, from.checked_add_days(week)?));
    }

    // 4) week of <month> <day> (EN) — the ISO week containing that date, in today's year.
    if let Some(c) = re_week_of().captures(&q) {
        let whole = c.get(0)?;
        // An explicit 4-digit year after the day ("week of June 15 2025") would be silently
        // ignored and resolve to TODAY'S year — a WRONG window that hard-filters every leg
        // is worse than no filter. Bail to None so the query runs unfiltered instead.
        let rest = q[whole.end()..].trim_start_matches([' ', ',']);
        if rest.len() >= 4 && rest.as_bytes()[..4].iter().all(u8::is_ascii_digit) {
            return None;
        }
        let month = en_month_number(c.get(1)?.as_str())?;
        let d: u32 = c.get(2)?.as_str().parse().ok()?;
        let date = NaiveDate::from_ymd_opt(today.year(), month, d)?;
        let from = monday_of(date);
        return Some((from, from.checked_add_days(week)?));
    }

    // 5) first week of <month> — the ISO week containing the 1st of that month (today's year).
    if let Some(c) = re_first_week_of().captures(&q) {
        let month = c
            .get(1)
            .and_then(|m| en_month_number(m.as_str()))
            .or_else(|| c.get(2).and_then(|m| pl_month_number(m.as_str())))?;
        let first = NaiveDate::from_ymd_opt(today.year(), month, 1)?;
        let from = monday_of(first);
        return Some((from, from.checked_add_days(week)?));
    }

    // 6) yesterday · wczoraj.
    if re_yesterday().is_match(&q) {
        let from = today.checked_sub_days(day)?;
        return Some((from, today));
    }

    // 7) today · dziś/dzisiaj.
    if re_today().is_match(&q) {
        return Some((today, today.checked_add_days(day)?));
    }

    // 8) last week · zeszłym tygodniu / zeszłego tygodnia / w zeszłym tygodniu / poprzednim / ubiegłym.
    if re_last_week().is_match(&q) {
        let from = monday_of(today).checked_sub_days(week)?;
        return Some((from, from.checked_add_days(week)?));
    }

    // 9) this week · w tym tygodniu / tego tygodnia.
    if re_this_week().is_match(&q) {
        let from = monday_of(today);
        return Some((from, from.checked_add_days(week)?));
    }

    // 10) last month · zeszłym miesiącu / zeszłego miesiąca / poprzednim miesiącu.
    if re_last_month().is_match(&q) {
        let this_start = month_start(today);
        let prev_last_day = this_start.checked_sub_days(day)?;
        return Some((month_start(prev_last_day), this_start));
    }

    // 11) this month · w tym miesiącu / tego miesiąca.
    if re_this_month().is_match(&q) {
        return Some((month_start(today), next_month_start(today)));
    }

    None
}

/// Format a parsed window as the `(from_iso, to_iso_exclusive)` string pair the DB readers bind
/// (`m.started_at >= from AND m.started_at < to` — ISO-8601 strings compare lexicographically).
pub fn date_filter_strings(range: (NaiveDate, NaiveDate)) -> (String, String) {
    (
        range.0.format("%Y-%m-%d").to_string(),
        range.1.format("%Y-%m-%d").to_string(),
    )
}

/// Convenience for the retrieval call sites: parse + format in one step.
pub fn extract_date_filter(query: &str, today: NaiveDate) -> Option<(String, String)> {
    parse_temporal_constraint(query, today).map(date_filter_strings)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The eval corpus anchor — a Monday (2026-06-29).
    fn anchor() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 6, 29).unwrap()
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn no_temporal_phrase_yields_none() {
        assert_eq!(
            parse_temporal_constraint("Projekt Atlas budget review", anchor()),
            None
        );
        assert_eq!(parse_temporal_constraint("", anchor()), None);
        // "weekly" / "tygodniowy" must NOT trip the week matchers (word boundaries).
        assert_eq!(
            parse_temporal_constraint("weekly sync notes", anchor()),
            None
        );
    }

    #[test]
    fn last_week_en_and_pl_declensions() {
        // Anchor is Monday 2026-06-29 ⇒ last week = 2026-06-22 .. 2026-06-29 (exclusive).
        let want = Some((d(2026, 6, 22), d(2026, 6, 29)));
        assert_eq!(
            parse_temporal_constraint("What did we discuss last week?", anchor()),
            want
        );
        assert_eq!(
            parse_temporal_constraint("Jakie wnioski z zeszłego tygodnia?", anchor()),
            want
        );
        assert_eq!(
            parse_temporal_constraint("co było w zeszłym tygodniu", anchor()),
            want
        );
        assert_eq!(
            parse_temporal_constraint("w poprzednim tygodniu", anchor()),
            want
        );
        assert_eq!(
            parse_temporal_constraint("ubiegłego tygodnia", anchor()),
            want
        );
        // Mid-week anchor still resolves to the SAME Monday-anchored window.
        let wed = d(2026, 7, 1);
        assert_eq!(parse_temporal_constraint("last week", wed), want);
    }

    #[test]
    fn this_week_and_months() {
        assert_eq!(
            parse_temporal_constraint("plans for this week", anchor()),
            Some((d(2026, 6, 29), d(2026, 7, 6)))
        );
        assert_eq!(
            parse_temporal_constraint("co mamy w tym tygodniu", anchor()),
            Some((d(2026, 6, 29), d(2026, 7, 6)))
        );
        assert_eq!(
            parse_temporal_constraint("what happened last month", anchor()),
            Some((d(2026, 5, 1), d(2026, 6, 1)))
        );
        assert_eq!(
            parse_temporal_constraint("w zeszłym miesiącu", anchor()),
            Some((d(2026, 5, 1), d(2026, 6, 1)))
        );
        assert_eq!(
            parse_temporal_constraint("spending this month", anchor()),
            Some((d(2026, 6, 1), d(2026, 7, 1)))
        );
        assert_eq!(
            parse_temporal_constraint("wydatki w tym miesiącu", anchor()),
            Some((d(2026, 6, 1), d(2026, 7, 1)))
        );
    }

    #[test]
    fn yesterday_today_and_last_n_days() {
        assert_eq!(
            parse_temporal_constraint("notes from yesterday", anchor()),
            Some((d(2026, 6, 28), d(2026, 6, 29)))
        );
        assert_eq!(
            parse_temporal_constraint("co ustaliliśmy wczoraj", anchor()),
            Some((d(2026, 6, 28), d(2026, 6, 29)))
        );
        assert_eq!(
            parse_temporal_constraint("agenda for today", anchor()),
            Some((d(2026, 6, 29), d(2026, 6, 30)))
        );
        assert_eq!(
            parse_temporal_constraint("co mamy dziś", anchor()),
            Some((d(2026, 6, 29), d(2026, 6, 30)))
        );
        assert_eq!(
            parse_temporal_constraint("decisions from the last 10 days", anchor()),
            Some((d(2026, 6, 19), d(2026, 6, 30)))
        );
        assert_eq!(
            parse_temporal_constraint("ostatnie 3 dni", anchor()),
            Some((d(2026, 6, 26), d(2026, 6, 30)))
        );
    }

    #[test]
    fn weeks_ago_and_week_of() {
        // "two weeks ago" from Monday 2026-06-29 ⇒ week of 2026-06-15.
        assert_eq!(
            parse_temporal_constraint("What did we agree on two weeks ago?", anchor()),
            Some((d(2026, 6, 15), d(2026, 6, 22)))
        );
        assert_eq!(
            parse_temporal_constraint("dwa tygodnie temu", anchor()),
            Some((d(2026, 6, 15), d(2026, 6, 22)))
        );
        assert_eq!(
            parse_temporal_constraint("a week ago", anchor()),
            Some((d(2026, 6, 22), d(2026, 6, 29)))
        );
        assert_eq!(
            parse_temporal_constraint("tydzień temu", anchor()),
            Some((d(2026, 6, 22), d(2026, 6, 29)))
        );
        // "week of June 15" ⇒ the Monday-anchored week containing 2026-06-15.
        assert_eq!(
            parse_temporal_constraint(
                "Which decisions were made in the week of June 15?",
                anchor()
            ),
            Some((d(2026, 6, 15), d(2026, 6, 22)))
        );
        // An EXPLICIT year after the day must bail to None (today's-year resolution would build
        // a WRONG window that hard-filters every leg — worse than no filter).
        assert_eq!(
            parse_temporal_constraint("week of June 15 2025", anchor()),
            None
        );
        assert_eq!(
            parse_temporal_constraint("week of June 15, 2025", anchor()),
            None
        );
        // "first week of June" / PL "pierwszym tygodniu czerwca" ⇒ week of 2026-06-01 (a Monday).
        assert_eq!(
            parse_temporal_constraint("first week of June", anchor()),
            Some((d(2026, 6, 1), d(2026, 6, 8)))
        );
        assert_eq!(
            parse_temporal_constraint("Co działo się w pierwszym tygodniu czerwca?", anchor()),
            Some((d(2026, 6, 1), d(2026, 6, 8)))
        );
    }

    #[test]
    fn iso_dates_single_and_range() {
        assert_eq!(
            parse_temporal_constraint("what happened on 2026-05-11", anchor()),
            Some((d(2026, 5, 11), d(2026, 5, 12)))
        );
        assert_eq!(
            parse_temporal_constraint("between 2026-05-11 and 2026-05-20", anchor()),
            Some((d(2026, 5, 11), d(2026, 5, 21)))
        );
        // Invalid calendar date is ignored (no panic, no bogus window).
        assert_eq!(
            parse_temporal_constraint("2026-13-45 nonsense", anchor()),
            None
        );
    }

    #[test]
    fn date_filter_strings_are_half_open_iso() {
        let f = extract_date_filter("last week", anchor()).unwrap();
        assert_eq!(f, ("2026-06-22".to_string(), "2026-06-29".to_string()));
    }
}
