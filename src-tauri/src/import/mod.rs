//! Bulk IMPORT of an external knowledge base into Murmur's canonical store.
//!
//! Distinct from `crate::extract` (one file → extracted text for the retrieval corpus) and from
//! `crate::connectors` (a live, consented, ledgered CLOUD query). An importer here is **local and
//! offline**: it reads something the user already has on this Mac, and writes ordinary authored
//! notes through the existing gated funnel. Nothing leaves the machine, so no consent surface, no
//! redaction firewall involvement, and no egress-ledger row.
//!
//! Each source contributes ONE function — `path (or nothing) → ImportScan` — that is pure enough to
//! prove with `cargo test --lib` against synthetic fixtures. The DB-touching orchestration is shared
//! and lives once, in `crate::commands::import`.
//!
//! Sources differ only in how they answer three questions:
//!
//! | Source | Identity (`external_id`) | Body | Links |
//! |---|---|---|---|
//! | Notion | the 32-hex page id in every filename | Markdown, verbatim | relative paths rewritten to `[[wikilinks]]` |
//! | Obsidian | the vault-relative path, SCOPED to a fingerprint of the vault root | Markdown, verbatim | already `[[wikilinks]]` — left alone |
//! | Apple Notes | the note's Core Data id | HTML rendered to text | none to speak of |

use std::collections::BTreeMap;

pub(crate) mod apple_notes;
pub(crate) mod notion;
pub(crate) mod obsidian;

/// Longest title kept from any source. Notion truncates its own; the others can produce a whole
/// first paragraph as a name. Re-truncated on a char boundary so a pathological title cannot yield
/// an unwritable filename.
pub(crate) const MAX_TITLE_CHARS: usize = 200;

/// Hard cap on pages accepted from ONE import. A larger workspace should come in parts — Tana caps
/// at 1500 for the same reason. Refusing loudly beats a multi-hour silent run.
pub(crate) const MAX_PAGES_PER_IMPORT: usize = 5_000;

/// Where an import is reading from. The wire value is the lowercase name the FE sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportSource {
    Notion,
    Obsidian,
    AppleNotes,
}

impl ImportSource {
    /// Parse the FE's wire value. Unknown values fail closed rather than defaulting to a source.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "notion" => Some(Self::Notion),
            "obsidian" => Some(Self::Obsidian),
            "apple-notes" => Some(Self::AppleNotes),
            _ => None,
        }
    }

    /// The value stored in `documents.source`. Stable — it is the idempotency key half.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Notion => "notion",
            Self::Obsidian => "obsidian",
            Self::AppleNotes => "apple-notes",
        }
    }
}

/// One page recovered from an export, ready to become a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedPage {
    /// The source's own stable key, when it has one. Half of the re-import idempotency key; `None`
    /// means the page can only ever be created, never matched to an earlier run.
    pub external_id: Option<String>,
    pub title: String,
    /// Ancestor names, outermost first — mirrored as nested note folders when the user asks.
    pub parents: Vec<String>,
    /// The page body as Markdown.
    pub markdown: String,
}

/// What a scan found, WITHOUT writing anything. The dry-run contract the UI renders before the user
/// commits. Fields that cannot apply to a source stay zero (Apple Notes has no `.csv` twins).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ImportScan {
    pub pages: Vec<ImportedPage>,
    /// Non-page files shipped alongside (images, PDFs) — counted, never imported yet.
    pub attachments: usize,
    pub attachment_bytes: u64,
    /// Database exports. `csv_all_duplicates` are the `…_all.csv` twins deliberately ignored.
    pub databases: usize,
    pub csv_all_duplicates: usize,
    /// Nested archives descended into automatically.
    pub nested_archives: usize,
    /// Titles occurring more than once — the caller disambiguates by folder.
    pub title_collisions: Vec<String>,
    /// True when [`MAX_PAGES_PER_IMPORT`] cut the scan short.
    pub truncated: bool,
}

impl ImportScan {
    /// Fill in `title_collisions` from the collected pages. Every source calls this last, so the
    /// duplicate-title warning cannot be a per-source oversight.
    pub(crate) fn finish(&mut self) {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for page in &self.pages {
            *counts.entry(page.title.as_str()).or_insert(0) += 1;
        }
        self.title_collisions = counts
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(t, _)| t.to_string())
            .collect();
    }
}

/// Truncate to [`MAX_TITLE_CHARS`] on a char boundary.
pub(crate) fn clamp_title(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed.chars().count() <= MAX_TITLE_CHARS {
        return trimmed.to_string();
    }
    trimmed.chars().take(MAX_TITLE_CHARS).collect()
}

/// The display title for a Markdown page: the first ATX `# ` heading when present (it is the FULL
/// title — a filename may be truncated), else the supplied fallback, else the Untitled sentinel
/// every picker and audit guard already agrees on.
pub(crate) fn title_from_body_or_stem(markdown: &str, fallback: &str) -> String {
    for line in markdown.lines().take(10) {
        if let Some(rest) = line.trim().strip_prefix("# ") {
            let heading = rest.trim();
            if !heading.is_empty() {
                return clamp_title(heading);
            }
        }
    }
    let clamped = clamp_title(fallback);
    if clamped.is_empty() {
        crate::storage::db::UNTITLED_TITLE.to_string()
    } else {
        clamped
    }
}
