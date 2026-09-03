//! Oracle: the listing/searching reads stay OFF the main thread.
//!
//! WHY A SOURCE-LEVEL ORACLE. Tauri runs a synchronous `#[tauri::command]` on the MAIN thread, and
//! every DB access funnels through one `Mutex<Connection>`. So a listing read issued while a long
//! write holds that mutex parks the UI thread for the whole wait. The fix is structural — the
//! command is `async` (so Tauri does not run it on the main thread) AND its blocking body runs in
//! `offload_read` (so it does not park an async worker either) — and a structural property is
//! exactly what a source assertion can pin. There is no in-process way to assert "the main thread
//! was not blocked"; asserting the mechanism that guarantees it is the honest substitute, and it
//! catches the regression that actually happens: someone converts one of these back to `pub fn`,
//! or drops the `offload_read` while keeping `async`.
//!
//! RED CONTROL (run 2026-09-03, both observed failing). Reverting `list_meetings` to
//! `pub fn … (state: State<'_, AppState>)` fails with "`list_meetings` is not `pub async fn`";
//! leaving `get_graph` async but replacing its `offload_read` with a direct
//! `app.state::<AppState>()` read fails with "`get_graph` is async but does not route through
//! `offload_read`". The two regressions report DIFFERENTLY, which is the point — a blunt pass/fail
//! would not tell a later author which half they broke.

use std::path::PathBuf;

/// `(file, command)` — every read the 2026-09-02 audit (S1) named as a hot listing/searching path.
const OFFLOADED_READS: &[(&str, &str)] = &[
    ("meetings.rs", "list_meetings"),
    ("meetings.rs", "search_meetings"),
    ("meetings.rs", "get_meeting_detail"),
    ("meetings.rs", "get_meeting_segments"),
    ("graph.rs", "get_graph"),
    ("graph.rs", "get_full_graph"),
    ("analytics.rs", "get_analytics"),
    ("notes.rs", "list_notes"),
    ("notes.rs", "list_notes_typed"),
    ("workspace.rs", "list_workspace_tree"),
    ("documents.rs", "list_documents"),
    ("dashboards.rs", "list_dashboards"),
    ("dashboards.rs", "get_dashboard"),
];

fn commands_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("commands")
}

/// Body of `pub async fn <name>(` up to its closing brace, by brace matching — not by a line
/// window, which a reformat silently shrinks past the thing being asserted.
fn command_body<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let sig = format!("pub async fn {name}(");
    let start = source.find(&sig)?;
    let open = start + source[start..].find('{')?;
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[open..=open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn every_hot_listing_read_is_async_and_offloaded() {
    let mut failures = Vec::new();
    for (file, name) in OFFLOADED_READS {
        let path = commands_dir().join(file);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        // A command that no longer exists must fail loudly rather than pass vacuously.
        if !source.contains(&format!("fn {name}(")) {
            failures.push(format!("{file}: `{name}` is gone — update this list deliberately"));
            continue;
        }
        let Some(body) = command_body(&source, name) else {
            failures.push(format!(
                "{file}: `{name}` is not `pub async fn` — a sync command runs on the MAIN thread, \
                 so a long write parks the UI for the whole wait"
            ));
            continue;
        };
        if !body.contains("offload_read(") {
            failures.push(format!(
                "{file}: `{name}` is async but does not route through `offload_read`, so its \
                 blocking DB work parks an async worker instead"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The list itself must not rot into a no-op: every entry names a real file.
#[test]
fn the_offloaded_read_list_names_real_files() {
    for (file, _) in OFFLOADED_READS {
        let path = commands_dir().join(file);
        assert!(path.is_file(), "{} does not exist", path.display());
    }
}
