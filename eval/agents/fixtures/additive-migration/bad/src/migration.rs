//! Schema migration for the local SQLite database.
//!
//! The caller runs `migration_statements` on every launch, passing the columns the
//! `segments` table currently has, and executes the returned statements in order
//! against the user's existing database file.

pub fn migration_statements(existing_columns: &[&str]) -> Vec<String> {
    let mut statements: Vec<String> = Vec::new();

    if !existing_columns.contains(&"started_at") {
        statements.push("ALTER TABLE segments ADD COLUMN started_at INTEGER".to_string());
    }

    if !existing_columns.contains(&"speaker_id") {
        statements.push("ALTER TABLE segments ADD COLUMN speaker_id TEXT".to_string());
        statements.push(
            "UPDATE segments SET speaker_id = lower(replace(speaker_name, ' ', '-'))".to_string(),
        );
        statements.push("ALTER TABLE segments DROP COLUMN speaker_name".to_string());
    }

    statements
}
