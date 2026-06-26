//! Topic Threads: aggregate the per-meeting topic spans (already persisted in the `timelines`
//! table) across the whole library into cross-meeting "threads" — a chronological line of every
//! time a topic was discussed. Deterministic label-clustering, no LLM call.

use std::collections::BTreeMap;

use crate::storage::models::{TopicMention, TopicThread};

/// One meeting's topic spans, ready for threading.
pub struct MeetingTopics {
    pub meeting_id: String,
    pub title: String,
    pub started_at: String,
    pub topics: Vec<(String, f64, f64)>, // (label, start_s, end_s)
}

/// Group topic spans across meetings by normalized label into threads, newest mention last
/// within each thread, threads ordered by mention count (desc) then label.
pub fn build_threads(meetings: &[MeetingTopics]) -> Vec<TopicThread> {
    let mut map: BTreeMap<String, (String, Vec<TopicMention>)> = BTreeMap::new();
    for m in meetings {
        for (label, start, end) in &m.topics {
            let key = label.trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            let entry = map.entry(key).or_insert_with(|| (label.trim().to_string(), Vec::new()));
            entry.1.push(TopicMention {
                meeting_id: m.meeting_id.clone(),
                title: m.title.clone(),
                started_at: m.started_at.clone(),
                start_s: *start,
                end_s: *end,
            });
        }
    }
    let mut threads: Vec<TopicThread> = map
        .into_values()
        .map(|(label, mut mentions)| {
            mentions.sort_by(|a, b| a.started_at.cmp(&b.started_at));
            TopicThread {
                label,
                count: mentions.len(),
                mentions,
            }
        })
        .collect();
    threads.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    threads
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mt(id: &str, when: &str, topics: &[(&str, f64, f64)]) -> MeetingTopics {
        MeetingTopics {
            meeting_id: id.into(),
            title: format!("Meeting {id}"),
            started_at: when.into(),
            topics: topics.iter().map(|(l, s, e)| (l.to_string(), *s, *e)).collect(),
        }
    }

    #[test]
    fn clusters_across_meetings_chronologically() {
        let ms = vec![
            mt("2", "2026-07-02", &[("Budget", 0.0, 60.0)]),
            mt("1", "2026-07-01", &[("budget", 10.0, 50.0), ("Hiring", 0.0, 20.0)]),
        ];
        let threads = build_threads(&ms);
        // "Budget"/"budget" merge into one thread of 2, ordered first (highest count)
        assert_eq!(threads[0].count, 2);
        assert_eq!(threads[0].label, "Budget"); // first-seen casing kept
        // chronological: meeting 1 (07-01) before meeting 2 (07-02)
        assert_eq!(threads[0].mentions[0].meeting_id, "1");
        assert_eq!(threads[0].mentions[1].meeting_id, "2");
        assert!(threads.iter().any(|t| t.label == "Hiring" && t.count == 1));
    }
}
