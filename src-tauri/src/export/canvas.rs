//! Obsidian Canvas export: lay a meeting out as a spatial board — a central meeting node with
//! a topic card per timeline topic-span, connected by edges. Emits `.canvas` JSON.

use serde_json::json;

fn mmss(s: f64) -> String {
    let s = s.max(0.0) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Build Obsidian Canvas JSON for a meeting + its topic spans `(label, start_s, end_s)`.
pub fn build_canvas(title: &str, topics: &[(String, f64, f64)]) -> String {
    let mut nodes = vec![json!({
        "id": "meeting", "type": "text", "text": format!("# {title}"),
        "x": 0, "y": 0, "width": 340, "height": 100
    })];
    let mut edges = Vec::new();
    let span = 360i64;
    let cols = topics.len().max(1) as i64;
    let start_x = -(cols * span) / 2 + span / 2;
    for (i, (label, s, e)) in topics.iter().enumerate() {
        let id = format!("t{i}");
        let x = start_x + (i as i64) * span;
        nodes.push(json!({
            "id": id.clone(), "type": "text",
            "text": format!("## {label}\n{} – {}", mmss(*s), mmss(*e)),
            "x": x, "y": 260, "width": 300, "height": 140
        }));
        edges.push(json!({
            "id": format!("e{i}"), "fromNode": "meeting", "fromSide": "bottom",
            "toNode": id, "toSide": "top"
        }));
    }
    json!({ "nodes": nodes, "edges": edges }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_canvas_json() {
        let c = build_canvas(
            "Sync",
            &[("Budget".into(), 0.0, 65.0), ("Hiring".into(), 65.0, 120.0)],
        );
        let v: serde_json::Value = serde_json::from_str(&c).unwrap();
        assert_eq!(v["nodes"].as_array().unwrap().len(), 3); // meeting + 2 topics
        assert_eq!(v["edges"].as_array().unwrap().len(), 2);
        assert!(c.contains("1:05")); // 65s formatted
    }
}
