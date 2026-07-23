//! HTML extraction via `html2text` (pure-Rust). The whole document renders to plain text as one
//! [`ExtractedBlock`] (page `None`, heading `None`) — HTML has no reliable page axis and we keep the
//! flow-format contract identical to md/txt. Deterministic + headless-testable.
//!
//! Lock model: pure `path → blocks`, no DB/keychain; failures map to `AppError::InvalidArg`. No PII.

use std::path::Path;

use super::ExtractedBlock;
use crate::error::{AppError, Result};

/// Wrap width for `html2text` rendering. Wide enough that paragraphs are not aggressively hard-wrapped
/// (we want readable flow text for embedding/snippets, not a terminal column layout).
const RENDER_WIDTH: usize = 100;

/// Extract an HTML file into one plain-text block. A file-size sanity cap
/// ([`super::MAX_FLOW_FILE_BYTES`]) is applied BEFORE the read (HTML has no decompression step, so a
/// multi-gigabyte `.html` would otherwise be slurped whole into RAM before any block-level check).
pub fn extract_html(path: &Path) -> Result<Vec<ExtractedBlock>> {
    // FILE-SIZE SANITY CAP (flow format): reject an oversized file before reading it into memory.
    let meta = std::fs::metadata(path)
        .map_err(|e| AppError::InvalidArg(format!("could not read HTML: {e}")))?;
    if meta.len() > super::MAX_FLOW_FILE_BYTES {
        return Err(AppError::InvalidArg(
            "this HTML is too large to import — it exceeds the size limit".into(),
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|e| AppError::InvalidArg(format!("could not read HTML: {e}")))?;
    let text = html2text::from_read(bytes.as_slice(), RENDER_WIDTH)
        .map_err(|e| AppError::InvalidArg(format!("could not render HTML: {e}")))?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![ExtractedBlock::plain(text)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_html(body: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-html-{}-{}.html",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&p, body).unwrap();
        p
    }

    /// HTML: tags are stripped, visible text survives, one block, no page / heading.
    #[test]
    fn html_renders_to_one_plain_text_block() {
        let p = write_html(
            "<html><body><h1>Spec</h1><p>The budget is <b>100k</b>.</p>\
             <ul><li>Anna owns delivery</li></ul></body></html>",
        );
        let blocks = extract_html(&p).unwrap();
        assert_eq!(blocks.len(), 1, "one block for the whole HTML doc");
        let text = &blocks[0].text;
        // `html2text`'s plain decorator renders markdown-flavored plain text (a `#` heading, `**bold**`,
        // `*` list bullets) — the exact decoration is not load-bearing; what matters is that the
        // VISIBLE text survives and the raw HTML TAGS are gone.
        assert!(text.contains("Spec"), "heading text survives: {text:?}");
        assert!(text.contains("100k"), "inline text survives: {text:?}");
        assert!(
            text.contains("budget is"),
            "paragraph text survives: {text:?}"
        );
        assert!(
            text.contains("Anna owns delivery"),
            "list item survives: {text:?}"
        );
        assert!(!text.contains("<b>"), "raw tags must be stripped: {text:?}");
        assert!(
            !text.contains("<li>"),
            "raw tags must be stripped: {text:?}"
        );
        assert_eq!(blocks[0].page, None);
        assert_eq!(blocks[0].heading_path, None);
    }

    /// An empty / whitespace-only HTML file yields NO block (nothing to ingest).
    #[test]
    fn empty_html_yields_no_blocks() {
        let p = write_html("<html><body>   </body></html>");
        let blocks = extract_html(&p).unwrap();
        assert!(blocks.is_empty(), "no visible text → no block");
    }
}
