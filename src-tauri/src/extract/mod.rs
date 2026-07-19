//! Universal document extraction (Brain v3 PR-2). Turns a local file of a supported format into a
//! sequence of [`ExtractedBlock`]s — plain text, optionally tagged with a page number and a
//! heading path — that the ingest pipeline chunks + embeds. We store the EXTRACTED TEXT only
//! (`documents.text`), never the source binary, so there is no new seal path here; the extracted
//! text rides the SAME `documents` seal/unseal the md/txt path already uses.
//!
//! Format matrix (dispatched by lowercased extension in [`extract_blocks`]):
//! - `md` / `txt` — the whole file as ONE block (page `None`, heading `None`). Byte-for-byte the
//!   pre-PR-2 behavior (a plain `read_to_string`), so an existing import is unchanged.
//! - `docx` / `pptx` — PURE-RUST OOXML: [`ooxml`] walks the zip + streams the XML (no docx crate).
//! - `pdf` — Apple PDFKit ([`pdf`], macOS-only), every call wrapped in `objc2::exception::catch`
//!   so a malformed/encrypted PDF fails CLOSED with `AppError::InvalidArg`, never an FFI abort.
//! - `xlsx` — [`calamine`](self::xlsx) (sheet name = heading, rows → pipe text).
//! - `html` / `htm` — [`html`] via `html2text` (one plain-text block).
//! - `png`/`jpg`/`jpeg`/`heic`/`tiff`/`tif`/`bmp`/`gif` — direct image import ([`image`], macOS-only):
//!   on-device Apple Vision OCR ([`ocr`]) into ONE block. A scanned/image-only PDF (no text layer)
//!   also falls back to the SAME Vision OCR inside [`pdf`]. NO cloud egress; every FFI call wrapped in
//!   `objc2::exception::catch` — fail-closed to `AppError::InvalidArg`, never an abort.
//! - anything else — [`AppError::InvalidArg`].
//!
//! Lock model: extraction is a pure `path → Vec<ExtractedBlock>` transform with no DB / keychain
//! access. Every gate lives at the ingest seam (`ingest_into_folder`) that consumes the result; the
//! WRITE-GATE there refuses a sealed folder before any block is persisted. NO PII is logged here.

use std::path::Path;

use crate::error::{AppError, Result};

pub mod html;
#[cfg(target_os = "macos")]
pub mod image;
#[cfg(target_os = "macos")]
pub mod ocr;
pub mod ooxml;
#[cfg(target_os = "macos")]
pub mod pdf;
pub mod reflow;
pub mod xlsx;

/// One extracted unit of a document: a run of plain text, optionally located by 1-based `page`
/// (PDF page / PPTX slide; `None` for flow formats) and by `heading_path` — the accumulated
/// heading trail to this block joined with " › " (e.g. `"Design › Storage"`), `None` when the
/// block sits under no heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedBlock {
    pub text: String,
    pub page: Option<u32>,
    pub heading_path: Option<String>,
}

impl ExtractedBlock {
    /// A flow block with no page / heading context (md/txt/html and un-headed paragraphs).
    pub fn plain(text: impl Into<String>) -> Self {
        ExtractedBlock {
            text: text.into(),
            page: None,
            heading_path: None,
        }
    }
}

/// The heading-trail separator used everywhere a `heading_path` is built or rendered (deterministic).
pub const HEADING_SEP: &str = " › ";

/// DECOMPRESSION-BOMB CEILING for a SINGLE OOXML/XLSX (zip-container) extraction — the maximum TOTAL
/// number of DECOMPRESSED bytes we will accumulate across all inflated entries of one document before
/// failing closed with [`AppError::InvalidArg`]. This is NOT a cap on the original (compressed) file
/// size — a legitimately huge document stays importable ("arbitrary size" promise); it caps only the
/// EXPANSION, so a tiny "zip bomb" that inflates to gigabytes (a classic OOM-availability attack via
/// the ingest surface) is stopped. 512 MiB is far above any real docx/pptx/xlsx's decompressed XML
/// yet small enough to never let a bomb exhaust memory. Enforced in [`ooxml`] and [`xlsx`].
pub const MAX_EXTRACT_DECOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

/// UNIVERSAL EXTRACTED-TEXT CEILING (Brain v3 PR-4, audit finding: asymmetric memory bounds) — the
/// maximum TOTAL number of UTF-8 bytes of EXTRACTED TEXT one document may accumulate across all its
/// blocks before extraction fails closed with [`AppError::InvalidArg`]. Where
/// [`MAX_EXTRACT_DECOMPRESSED_BYTES`] guards ONLY the zip-container formats (the decompressed XML),
/// this guards the ACCUMULATED OUTPUT of EVERY format — PDF content-stream text, calamine's in-memory
/// range, the flow formats (md/txt/html) — so no path can materialize an unbounded String at rest in
/// the ingest surface. 256 MiB of extracted plain text is far beyond any real document a user imports
/// into a note vault (a 256 MiB PLAIN-TEXT corpus is ~40M words / a small library) yet bounds the
/// worst case. Enforced once in [`extract_blocks`] over the assembled block list, and mirrored as a
/// per-file read cap for the flow formats ([`MAX_FLOW_FILE_BYTES`]) so a huge md/txt/html never even
/// reaches `read_to_string`.
pub const MAX_EXTRACTED_TEXT_BYTES: u64 = 256 * 1024 * 1024;

/// FILE-SIZE SANITY CAP for the FLOW formats (md/txt/html) read whole via `read_to_string`/`read`.
/// These formats have no decompression step, so [`MAX_EXTRACT_DECOMPRESSED_BYTES`] never applied to
/// them and a multi-gigabyte `.txt` would be slurped entirely into RAM before any block-level check.
/// This caps the ON-DISK file size BEFORE the read, so an oversized flow file fails closed
/// immediately (no giant allocation). Same 256 MiB ceiling as [`MAX_EXTRACTED_TEXT_BYTES`] (a flow
/// file's text ≈ its bytes), applied at the read seam in [`extract_text`] and [`html::extract_html`].
pub const MAX_FLOW_FILE_BYTES: u64 = MAX_EXTRACTED_TEXT_BYTES;

/// Sum the UTF-8 byte length of every block's text and fail closed if the running total exceeds
/// [`MAX_EXTRACTED_TEXT_BYTES`] — the universal post-extraction memory guard for ALL formats. Runs
/// once over the assembled block list in [`extract_blocks`]. NO PII (a byte count only).
fn guard_extracted_text_size(blocks: &[ExtractedBlock]) -> Result<()> {
    guard_extracted_text_size_with_ceiling(blocks, MAX_EXTRACTED_TEXT_BYTES)
}

/// [`guard_extracted_text_size`] with an explicit ceiling — production passes the shared const; a
/// test injects a tiny ceiling so a small synthetic block list trips the guard without materializing
/// 256 MiB. Uses a saturating sum so a pathological count can't overflow.
fn guard_extracted_text_size_with_ceiling(blocks: &[ExtractedBlock], ceiling: u64) -> Result<()> {
    let total: u64 = blocks
        .iter()
        .fold(0u64, |acc, b| acc.saturating_add(b.text.len() as u64));
    if total > ceiling {
        return Err(AppError::InvalidArg(
            "this document is too large to import — its extracted text exceeds the size limit".into(),
        ));
    }
    Ok(())
}

/// Read a FLOW-format file (md/txt) to a String with a file-size sanity cap applied BEFORE the read
/// (so an oversized file never allocates a giant String). Fails closed with [`AppError::InvalidArg`]
/// on a missing/unreadable/non-UTF-8 file OR one whose on-disk size exceeds [`MAX_FLOW_FILE_BYTES`].
/// The `label` is a non-PII format name ("document" / "HTML") for the error text.
pub(crate) fn read_flow_file_to_string(path: &Path, label: &str) -> Result<String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| AppError::InvalidArg(format!("could not read {label}: {e}")))?;
    if meta.len() > MAX_FLOW_FILE_BYTES {
        return Err(AppError::InvalidArg(format!(
            "this {label} is too large to import — it exceeds the size limit"
        )));
    }
    std::fs::read_to_string(path)
        .map_err(|e| AppError::InvalidArg(format!("could not read {label}: {e}")))
}

/// An extraction-progress signal emitted by the paged formats (PDF) as they process. Threaded from
/// the import command through [`extract_blocks`] so the FE can render live progress during a
/// multi-minute scanned-PDF OCR AND learn when the OCR page cap truncated a huge scanned document.
/// The extract module stays a pure `path → blocks` transform (no `AppHandle`/DB/state): this signal
/// is the ONLY outward channel, and the caller owns the emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractProgress {
    /// `done` pages of `total` have been processed (text-layer read or OCR'd). "page k of N".
    Page { done: usize, total: usize },
    /// The OCR page cap ([`pdf::MAX_OCR_PAGES`]) was reached: `ocred` scanned pages were OCR'd and
    /// `skipped` further scanned pages were left un-OCR'd (their text is absent). The import still
    /// SUCCEEDS with partial content — this lets the caller tell the FE the result was truncated.
    OcrTruncated { ocred: usize, skipped: usize },
}

/// A progress callback for [`extract_blocks`]. The caller (the import command) owns the emit; the
/// non-paged formats never call it. A no-op default ([`no_progress`]) is used by the tests and any
/// caller that doesn't care.
pub type ProgressFn<'a> = dyn Fn(ExtractProgress) + 'a;

/// A no-op [`ProgressFn`] — the default for formats that report no page-level progress and for tests.
pub fn no_progress(_p: ExtractProgress) {}

/// Extract a supported document into its blocks, dispatching on the LOWERCASED `ext` (the extension
/// WITHOUT the dot). An unsupported extension is [`AppError::InvalidArg`]. `path` must point at a
/// readable local file; a read/parse failure is mapped to `InvalidArg` (fail-closed), never a
/// panic. Deterministic: the same file always yields the same block sequence.
///
/// `progress` is called with `(pages_done, page_count)` for the PAGED formats (PDF) as pages are
/// processed — the FE renders it as "page k of N" so a large scanned-PDF OCR isn't a frozen dialog.
/// The non-paged formats never call it. The UNIVERSAL extracted-text ceiling
/// ([`MAX_EXTRACTED_TEXT_BYTES`]) is enforced once here over the assembled blocks, so EVERY format's
/// accumulated output is memory-bounded (not just the zip containers).
pub fn extract_blocks(path: &Path, ext: &str, progress: &ProgressFn<'_>) -> Result<Vec<ExtractedBlock>> {
    let blocks = match ext.to_ascii_lowercase().as_str() {
        "md" | "txt" => extract_text(path),
        "docx" => ooxml::extract_docx(path),
        "pptx" => ooxml::extract_pptx(path),
        "xlsx" => xlsx::extract_xlsx(path),
        "html" | "htm" => html::extract_html(path),
        #[cfg(target_os = "macos")]
        "pdf" => pdf::extract_pdf(path, progress),
        #[cfg(not(target_os = "macos"))]
        "pdf" => Err(AppError::InvalidArg(
            "PDF extraction is only available on macOS".into(),
        )),
        // Direct image import → on-device Vision OCR (macOS-only). `png`/`jpg`/`jpeg`/`heic`/`tiff`/
        // `tif`/`bmp`/`gif` all route to the OCR path in `image::extract_image`.
        #[cfg(target_os = "macos")]
        "png" | "jpg" | "jpeg" | "heic" | "tiff" | "tif" | "bmp" | "gif" => {
            image::extract_image(path)
        }
        #[cfg(not(target_os = "macos"))]
        "png" | "jpg" | "jpeg" | "heic" | "tiff" | "tif" | "bmp" | "gif" => Err(
            AppError::InvalidArg("image OCR is only available on macOS".into()),
        ),
        other => Err(AppError::InvalidArg(format!(
            "unsupported document type: .{other}"
        ))),
    }?;
    // UNIVERSAL memory guard: cap the accumulated extracted text for ALL formats (finding: the
    // 512 MiB guard covered only zip containers). A PDF/xlsx/flow document whose text output exceeds
    // the ceiling fails closed here BEFORE it's serialized + inserted.
    guard_extracted_text_size(&blocks)?;
    Ok(blocks)
}

/// md/txt: the whole file as ONE block, byte-for-byte the pre-PR-2 `read_to_string` behavior. A
/// non-UTF-8 / unreadable / OVER-SIZE file fails closed with `InvalidArg` (the file-size sanity cap
/// in [`read_flow_file_to_string`] rejects a huge flow file before it allocates a giant String).
fn extract_text(path: &Path) -> Result<Vec<ExtractedBlock>> {
    let text = read_flow_file_to_string(path, "document")?;
    Ok(vec![ExtractedBlock::plain(text)])
}

/// Join two heading-path components with [`HEADING_SEP`], skipping empties. Public for the chunker
/// (which builds the same trail from a block's `heading_path`).
pub fn join_heading(prefix: Option<&str>, leaf: &str) -> String {
    match prefix {
        Some(p) if !p.is_empty() => format!("{p}{HEADING_SEP}{leaf}"),
        _ => leaf.to_string(),
    }
}

/// A control-char sentinel prefix for the per-block metadata line in the stored text. `\u{001E}`
/// (RECORD SEPARATOR) never appears in extracted document text, so a stored document round-trips its
/// block structure losslessly WITHOUT a new column — the hierarchy (page/heading) survives a
/// seal/unseal/re-index cycle that only has `documents.text` to work from. NON-md/txt documents use
/// this; md/txt store their single block verbatim (no sentinel), so the plaintext a user sees for a
/// `.md` upload is byte-identical to before.
const BLOCK_SENTINEL: char = '\u{001E}';

/// Serialize extracted `blocks` into the `documents.text` string. A single un-located block (md/txt,
/// html) is stored VERBATIM (no sentinel — the readable plaintext is unchanged). Otherwise each block
/// is preceded by a `<RS>page<US>heading` metadata line, then its text, so [`blocks_from_stored_text`]
/// can reconstruct the hierarchy on re-index. Deterministic + lossless.
pub fn blocks_to_stored_text(blocks: &[ExtractedBlock]) -> String {
    // The common flow-format case: exactly one block, no page/heading → store its text verbatim.
    if blocks.len() == 1 && blocks[0].page.is_none() && blocks[0].heading_path.is_none() {
        return blocks[0].text.clone();
    }
    let mut out = String::new();
    for b in blocks {
        // Metadata line: <RS> <page-or-empty> <US> <heading-or-empty>
        out.push(BLOCK_SENTINEL);
        if let Some(p) = b.page {
            out.push_str(&p.to_string());
        }
        out.push('\u{001F}'); // UNIT SEPARATOR between page and heading
        if let Some(h) = &b.heading_path {
            out.push_str(h);
        }
        out.push('\n');
        out.push_str(&b.text);
        out.push('\n');
    }
    out
}

/// Reconstruct blocks from a `documents.text` produced by [`blocks_to_stored_text`]. Text WITHOUT the
/// sentinel (a legacy md/txt upload, a typed note, or any pre-PR-2 row) is returned as ONE plain
/// block — so every existing document re-indexes correctly (backward compatible). Deterministic.
pub fn blocks_from_stored_text(text: &str) -> Vec<ExtractedBlock> {
    if !text.contains(BLOCK_SENTINEL) {
        // Legacy / flow format: one whole-text block (identical to the pre-PR-2 chunking input).
        return vec![ExtractedBlock::plain(text.to_string())];
    }
    let mut blocks: Vec<ExtractedBlock> = Vec::new();
    // Split on the sentinel; the first fragment before the first sentinel (if any) is preamble-less.
    for segment in text.split(BLOCK_SENTINEL) {
        if segment.is_empty() {
            continue;
        }
        // segment = "<meta-line>\n<body...>". Split off the first line as metadata.
        let (meta, body) = match segment.split_once('\n') {
            Some((m, b)) => (m, b),
            None => (segment, ""),
        };
        // meta = "<page><US><heading>"
        let (page_s, heading_s) = match meta.split_once('\u{001F}') {
            Some((p, h)) => (p, h),
            None => ("", meta),
        };
        let page = page_s.trim().parse::<u32>().ok();
        let heading = {
            let h = heading_s.trim();
            (!h.is_empty()).then(|| h.to_string())
        };
        // Trim exactly one trailing '\n' we appended after the body (keep interior text intact).
        let body = body.strip_suffix('\n').unwrap_or(body);
        blocks.push(ExtractedBlock {
            text: body.to_string(),
            page,
            heading_path: heading,
        });
    }
    blocks
}

/// Render a stored `documents.text` into CLEAN human-readable text for display / agent reading: the
/// per-block metadata markers (page/heading sentinel lines) are stripped and blocks are joined by
/// blank lines. Text with NO sentinel (md/txt/note/legacy) is returned UNCHANGED — so a `.md`
/// upload, a typed note, and every pre-PR-2 row read byte-identically to before. Deterministic.
///
/// READ-TIME REFLOW (doc-preview fix): each LOCATED block's text is run through
/// [`reflow::reflow_fragmented_text`] before joining — a self-targeting, conservative de-fragmentation
/// of pathologically letter-spaced PDF text (`"Fron\nt\nend"` → `"Frontend"`). The gate no-ops on
/// clean text, so a normal PDF page / invoice is byte-identical; a fragmented CV page reads clean. This
/// is READ-ONLY (a copy of the block text) — `documents.text` at rest is never touched. The sentinel
/// guard above leaves md/txt/note/legacy rows unreflowed (they never carry located blocks).
pub fn render_display_text(stored: &str) -> String {
    if !stored.contains(BLOCK_SENTINEL) {
        return stored.to_string();
    }
    let blocks = blocks_from_stored_text(stored);
    let mut parts: Vec<String> = Vec::with_capacity(blocks.len());
    for b in blocks {
        let reflowed = reflow::reflow_fragmented_text(&b.text);
        let t = reflowed.trim();
        if t.is_empty() {
            continue;
        }
        parts.push(t.to_string());
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(ext: &str, bytes: &[u8]) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-extract-{}-{}.{ext}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// md/txt: the whole file becomes ONE block with no page / heading — byte-for-byte the
    /// pre-PR-2 `read_to_string` behavior (regression guard against the ingest text changing).
    #[test]
    fn md_and_txt_extract_the_whole_file_as_one_plain_block() {
        let body = "# Spec\n\nThe budget is 100k.\n\nAnna owns delivery.";
        for ext in ["md", "txt"] {
            let p = write_tmp(ext, body.as_bytes());
            let blocks = extract_blocks(&p, ext, &no_progress).unwrap();
            assert_eq!(blocks.len(), 1, ".{ext} must be one block");
            assert_eq!(blocks[0].text, body, ".{ext} text must be byte-identical");
            assert_eq!(blocks[0].page, None);
            assert_eq!(blocks[0].heading_path, None);
        }
    }

    /// An unknown extension fails closed with `InvalidArg` (never a panic / never a leak).
    #[test]
    fn unknown_extension_is_invalid_arg() {
        let p = write_tmp("xyz", b"whatever");
        let err = extract_blocks(&p, "xyz", &no_progress).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)), "got {err:?}");
    }

    /// `extract_blocks` lowercases the extension before dispatch (a `.MD` / `.TXT` still routes to
    /// the text path).
    #[test]
    fn extension_dispatch_is_case_insensitive() {
        let p = write_tmp("MD", b"hello");
        let blocks = extract_blocks(&p, "MD", &no_progress).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "hello");
    }

    #[test]
    fn join_heading_builds_the_trail_and_skips_empty_prefix() {
        assert_eq!(join_heading(None, "Design"), "Design");
        assert_eq!(join_heading(Some(""), "Design"), "Design");
        assert_eq!(join_heading(Some("Design"), "Storage"), "Design › Storage");
    }

    /// Fix 4 (RED→GREEN): the UNIVERSAL extracted-text budget passes blocks under the ceiling and
    /// fails closed once the ACCUMULATED text across all blocks EXCEEDS it — the memory guard that now
    /// covers EVERY format (not just the zip containers). Uses a TINY injected ceiling (like the
    /// decompress-budget tests) so a small synthetic block list trips the REAL fold-and-compare path
    /// without materializing 256 MiB.
    #[test]
    fn extracted_text_budget_passes_normal_and_fails_oversize() {
        // Two blocks summing to 30 bytes.
        let blocks = vec![
            ExtractedBlock::plain("a".repeat(10)),
            ExtractedBlock::plain("b".repeat(20)),
        ];
        // Ceiling above the total → OK.
        assert!(
            guard_extracted_text_size_with_ceiling(&blocks, 30).is_ok(),
            "a total exactly AT the ceiling is allowed (fail only when EXCEEDED)"
        );
        assert!(
            guard_extracted_text_size_with_ceiling(&blocks, 1_000).is_ok(),
            "well under the ceiling passes"
        );
        // Ceiling one below the total → the fold sums past it and fails closed with InvalidArg.
        let err = guard_extracted_text_size_with_ceiling(&blocks, 29).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)), "over-ceiling fails closed: {err:?}");
        // Production const is generously large — normal blocks never trip it.
        assert!(guard_extracted_text_size(&blocks).is_ok(), "the real ceiling passes normal text");
    }

    /// Fix 4: the flow-format file-size sanity cap. A real small file passes the cap and reads back
    /// verbatim (the happy path is unchanged); the cap arithmetic (`meta.len() > MAX_FLOW_FILE_BYTES`)
    /// only rejects an oversized file, which we don't materialize here.
    #[test]
    fn flow_file_size_cap_reads_small_file_verbatim() {
        let p = write_tmp("txt", b"under the cap");
        let text = read_flow_file_to_string(&p, "document").unwrap();
        assert_eq!(text, "under the cap");
        // A missing file fails closed (no panic).
        let missing = std::path::Path::new("/nonexistent/murmur/flow-missing.txt");
        assert!(read_flow_file_to_string(missing, "document").is_err());
    }

    /// A single flow block is stored VERBATIM (no sentinel) so a `.md` upload's plaintext is
    /// byte-identical to the pre-PR-2 behavior, and it round-trips back to one plain block.
    #[test]
    fn single_flow_block_stores_verbatim_and_round_trips() {
        let block = ExtractedBlock::plain("# Spec\n\nThe budget is 100k.");
        let stored = blocks_to_stored_text(std::slice::from_ref(&block));
        assert_eq!(stored, "# Spec\n\nThe budget is 100k.", "stored verbatim, no markers");
        let back = blocks_from_stored_text(&stored);
        assert_eq!(back, vec![block], "round-trips to one plain block");
    }

    /// Located blocks (page + heading) round-trip losslessly through the stored-text form.
    #[test]
    fn located_blocks_round_trip_losslessly() {
        let blocks = vec![
            ExtractedBlock {
                text: "The budget is 100k.".into(),
                page: Some(1),
                heading_path: Some("Design".into()),
            },
            ExtractedBlock {
                text: "Anna owns delivery.\nSecond line.".into(),
                page: Some(2),
                heading_path: Some("Design › Storage".into()),
            },
            ExtractedBlock {
                text: "No heading here.".into(),
                page: None,
                heading_path: None,
            },
        ];
        let stored = blocks_to_stored_text(&blocks);
        let back = blocks_from_stored_text(&stored);
        assert_eq!(back, blocks, "page + heading + text all survive the round trip");
    }

    /// Legacy text WITHOUT any sentinel (a pre-PR-2 row) reconstructs as ONE plain block (backward
    /// compatible — every existing document re-indexes correctly).
    #[test]
    fn legacy_text_without_sentinel_is_one_block() {
        let back = blocks_from_stored_text("plain legacy note body\n\nwith paragraphs");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].text, "plain legacy note body\n\nwith paragraphs");
        assert_eq!(back[0].page, None);
        assert_eq!(back[0].heading_path, None);
    }

    /// `render_display_text` strips the block markers to clean readable text; sentinel-free text
    /// (md/txt/note/legacy) passes through byte-identically.
    #[test]
    fn render_display_text_strips_markers_and_passes_plain_through() {
        // Plain passes through unchanged.
        assert_eq!(
            render_display_text("# Spec\n\nThe budget is 100k."),
            "# Spec\n\nThe budget is 100k."
        );
        // Located blocks → clean joined text, no control chars.
        let blocks = vec![
            ExtractedBlock {
                text: "The budget is 100k.".into(),
                page: Some(1),
                heading_path: Some("Design".into()),
            },
            ExtractedBlock {
                text: "Anna owns delivery.".into(),
                page: Some(2),
                heading_path: Some("Design › Storage".into()),
            },
        ];
        let stored = blocks_to_stored_text(&blocks);
        let display = render_display_text(&stored);
        assert_eq!(display, "The budget is 100k.\n\nAnna owns delivery.");
        assert!(
            !display.contains('\u{001E}') && !display.contains('\u{001F}'),
            "display text must carry no control-char markers"
        );
    }
}
