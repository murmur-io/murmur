//! PDF extraction via Apple **PDFKit** (`objc2-pdf-kit`) — macOS-only. Per-page text + the outline
//! tree come for free from the framework; there is NO new binary and no dylib to sign (same objc2
//! 0.6/0.3 family we already ship).
//!
//! CRASH-SAFE FFI (rules §7, the `screenshare.rs` war story): a malformed / encrypted / unusual PDF
//! can make PDFKit raise an Objective-C `NSException`, which — if it unwinds across the FFI boundary
//! — ABORTS the process ("Rust cannot catch foreign exceptions"). So EVERY PDFKit call here runs
//! inside `objc2::exception::catch`, and ANY caught exception (or a nil document) is mapped to a
//! fail-closed `AppError::InvalidArg` — never a panic, never an abort. `objc2`'s `exception` feature
//! is enabled in Cargo.toml for exactly this.
//!
//! Output: one [`ExtractedBlock`] per page (`page = Some(i+1)`), text = the page's `string`. When the
//! document has an outline (`outlineRoot`), each page's `heading_path` is the outline entry whose
//! destination page is nearest-at-or-before that page (best-effort; `None` when no outline).
//!
//! SCANNED-PDF OCR FALLBACK (Brain v3; PER-PAGE since PR-4): OCR is decided PER PAGE, not for the
//! whole document. For each page we read its text-layer string; a page whose trimmed text is empty or
//! shorter than [`OCR_MIN_TEXT_CHARS`] (a scanned page, or a scanned page behind a text-layer cover)
//! is rendered to a CGImage and OCR'd via on-device Apple Vision ([`crate::extract::ocr`]); a page
//! that already has a real text layer keeps it and is NEVER rendered (so a normal text PDF pays no OCR
//! cost). This closes the "one text-layer cover page suppresses OCR for 299 scanned pages" bug — a
//! mixed document now gets text-layer text where it exists AND OCR text where it doesn't.
//!
//! OCR PAGE CAP (PR-4, availability): OCR is expensive (render + Vision per page, multi-second), so at
//! most [`MAX_OCR_PAGES`] pages are OCR'd per document. Scanned pages BEYOND the cap are skipped (their
//! text is absent) and the import still SUCCEEDS with partial content — never a hang, never total
//! failure. The truncation is surfaced to the caller via [`crate::extract::ExtractProgress::OcrTruncated`]
//! (the FE shows a "some scanned pages exceeded the limit" notice) and logged (counts only, NO PII).
//! Text-layer pages are NEVER capped — only the OCR work is bounded.
//!
//! PROGRESS (PR-4): a per-page [`crate::extract::ExtractProgress::Page`] is emitted as pages are
//! processed, so a multi-minute scanned-PDF OCR shows live "page k of N" progress instead of a frozen
//! dialog. Only if the document has NO text on ANY page AND OCR yields nothing on EVERY page do we fail
//! closed with `InvalidArg("no text found in this document, even with OCR")`.
//!
//! REAL-MAC CAVEAT: this compiles everywhere but only TRULY verifies on a signed build on a real Mac
//! (PDFKit text/outline fidelity, RAM on a 500-page PDF, and — new — Vision OCR fidelity on a real
//! scanned page). `cargo test` cannot exercise the FFI path — the headless test here only asserts the
//! fail-closed behavior for a non-PDF input.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use objc2::exception::catch;
use objc2::AllocAnyThread;
use objc2_foundation::{NSString, NSURL};
use objc2_pdf_kit::{PDFDocument, PDFPage};

use super::{ExtractProgress, ExtractedBlock, ProgressFn};
use crate::error::{AppError, Result};

/// The minimum length (in CHARACTERS, after trimming) of a page's text-layer string for us to treat
/// the page as having a real text layer. A page whose text-layer string is empty or shorter than this
/// is treated as a scanned/image page and OCR'd (Fix 1: per-page OCR). 16 chars is short enough that a
/// genuinely sparse text page (a single word/heading) still counts as text, but long enough that a
/// scanned page carrying only an empty string or a stray artifact ligature falls through to OCR.
const OCR_MIN_TEXT_CHARS: usize = 16;

/// The maximum number of pages we will OCR in ONE document (Fix 2: availability). OCR is expensive
/// (render + Vision per page, multi-second each) and holds the ONE heavy-inference permit, so an
/// unbounded scanned book could starve ASR/summarize for many minutes. 300 pages is generous (a full
/// scanned book) yet finite: the worst case is bounded at ~300 × per-page-OCR. Scanned pages BEYOND
/// the cap are skipped (partial content, never a hang) and the truncation is surfaced to the caller
/// via [`ExtractProgress::OcrTruncated`]. Text-layer pages are never subject to this cap.
///
/// COOPERATIVE CANCEL DECISION (documented): the import runs inside `perf::run_heavy`'s
/// `spawn_blocking` closure, which is NOT cancellable (a dropped caller future orphans the closure —
/// see `perf.rs`), and `AppState` carries no per-import abort handle. Adding a cooperative cancel
/// signal (a shared `AtomicBool` registry keyed by document id, checked between pages + wired to a
/// `cancel_import` command + FE button) is >~40 lines across `state.rs`/`commands.rs`/`lib.rs`/the FE
/// and out of scope here. Instead THIS CAP + the per-page progress bound the worst case: an import can
/// take at most `MAX_OCR_PAGES` OCR steps, with live progress, rather than running unbounded. Mid-OCR
/// cancel is therefore NOT supported in this PR — it is bounded, not interruptible.
pub const MAX_OCR_PAGES: usize = 300;

/// Whether a page's text-layer string is too short to be a real text layer → OCR it. Pure logic
/// (no FFI), so the per-page OCR DECISION is unit-testable headless. Counts CHARACTERS (not bytes) so
/// a short multi-byte (e.g. Polish) heading is judged by its glyph count, not its UTF-8 length.
fn page_needs_ocr(text_layer: &str) -> bool {
    text_layer.trim().chars().count() < OCR_MIN_TEXT_CHARS
}

/// Map a caught-exception / nil / empty result into a single fail-closed error.
fn read_failed() -> AppError {
    AppError::InvalidArg(crate::errcode::tag(
        crate::errcode::DOC_UNREADABLE,
        "could not read PDF",
    ))
}

/// Run an ObjC closure that borrows non-`UnwindSafe` PDFKit handles inside `objc2::exception::catch`.
/// PDFKit reads have no interior-mutability hazard across a caught exception (we only read text /
/// outline; nothing is left half-mutated), so wrapping the borrows in `AssertUnwindSafe` is sound —
/// and it is REQUIRED because `Retained<_>`/`&PDFDocument` are not `UnwindSafe`. Returns `None` on a
/// caught ObjC exception, `Some(r)` otherwise (the closure's own `Option`/value is inside `r`).
fn catch_objc<R>(f: impl FnOnce() -> R) -> Option<R> {
    catch(AssertUnwindSafe(f)).ok()
}

/// Extract a PDF into per-page blocks with a PER-PAGE OCR fallback. For each page: keep its text-layer
/// string if it's a real text layer; otherwise (empty/short — a scanned page) OCR it, up to
/// [`MAX_OCR_PAGES`] OCR'd pages. Fail-closed on any FFI exception / unreadable / password-locked
/// document; fail closed only if EVERY page yields nothing (text layer AND OCR). `progress` receives a
/// per-page [`ExtractProgress::Page`] as pages are processed, and an [`ExtractProgress::OcrTruncated`]
/// if the OCR cap skipped scanned pages.
pub fn extract_pdf(path: &Path, progress: &ProgressFn<'_>) -> Result<Vec<ExtractedBlock>> {
    let path_str = path.to_string_lossy().to_string();

    // Open the document. `initWithURL` + everything downstream is UNSAFE ObjC that MAY throw — wrap
    // the whole open in catch. On a caught exception OR a nil document, fail closed.
    let doc: objc2::rc::Retained<PDFDocument> = catch_objc(|| {
        // SAFETY: standard PDFKit open. `fileURLWithPath` never throws for a well-formed path; the
        // whole closure is inside `catch` regardless so any ObjC exception is contained.
        let ns_path = NSString::from_str(&path_str);
        let url = NSURL::fileURLWithPath(&ns_path);
        let alloc = PDFDocument::alloc();
        unsafe { PDFDocument::initWithURL(alloc, &url) }
    })
    .flatten() // Some(None) = nil document (unreadable) / None = ObjC exception → both fail closed
    .ok_or_else(read_failed)?;

    // Refuse an encrypted-and-locked PDF explicitly (a nicer error than empty text). `isLocked` means
    // the content is password-protected and unreadable without the password.
    let locked = catch_objc(|| unsafe { doc.isLocked() }).unwrap_or(false);
    if locked {
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::DOC_PASSWORD,
            "this PDF is password-protected",
        )));
    }

    let page_count: usize = catch_objc(|| unsafe { doc.pageCount() }).ok_or_else(read_failed)?;
    if page_count == 0 {
        return Err(read_failed());
    }

    // Best-effort outline → (page-index → heading label). Never let it fail the extraction.
    let headings = outline_headings(&doc, page_count).unwrap_or_default();

    let mut blocks: Vec<ExtractedBlock> = Vec::new();
    let mut any_text = false; // any real text-layer OR OCR text on any page
    let mut pages_ocred = 0usize; // pages we actually OCR'd (under the cap)
    let mut pages_ocr_skipped = 0usize; // scanned pages skipped because the cap was reached

    for i in 0..page_count {
        // 1) Read the page's text layer (each fetch + `string` read is a separate catch — one bad
        //    page never aborts the rest).
        let text_layer: String = catch_objc(|| {
            let page = unsafe { doc.pageAtIndex(i) }?;
            let s = unsafe { page.string() }?;
            Some(s.to_string())
        })
        .flatten()
        .unwrap_or_default();

        // 2) PER-PAGE decision: a page with a real text layer keeps it (no render/OCR); a page with
        //    no/short text layer (scanned) is OCR'd — but only up to the cap.
        let page_text: String = if page_needs_ocr(&text_layer) {
            if pages_ocred < MAX_OCR_PAGES {
                // Fetch + OCR this page (inside catch). A page that fails to render/recognize simply
                // contributes no text — it never aborts the loop.
                let ocr_text = match catch_objc(|| unsafe { doc.pageAtIndex(i) }).flatten() {
                    Some(page) => crate::extract::ocr::ocr_pdf_page(&page).unwrap_or_default(),
                    None => String::new(),
                };
                pages_ocred += 1;
                // Prefer OCR text; fall back to whatever short text-layer string existed (rare).
                if ocr_text.trim().is_empty() {
                    text_layer
                } else {
                    ocr_text
                }
            } else {
                // Past the OCR cap: skip OCR (partial content). Keep any short text-layer text that
                // existed so we never DROP text a page did have.
                pages_ocr_skipped += 1;
                text_layer
            }
        } else {
            // Real text layer — keep it verbatim, never render.
            text_layer
        };

        let trimmed = page_text.trim();
        if !trimmed.is_empty() {
            any_text = true;
        }
        // Emit a block per page even when a single page is blank, so page numbering stays 1:1 with
        // the source; a page with no text simply carries empty text (still located by page number).
        blocks.push(ExtractedBlock {
            text: trimmed.to_string(),
            page: Some((i + 1) as u32),
            heading_path: heading_for_page(&headings, i),
        });

        // Per-page progress (1-based done). Cheap; the caller throttles/emits as it sees fit.
        progress(ExtractProgress::Page {
            done: i + 1,
            total: page_count,
        });
    }

    if !any_text {
        // Every page yielded nothing — no text layer anywhere AND OCR (where attempted) found nothing.
        return Err(AppError::InvalidArg(crate::errcode::tag(
            crate::errcode::DOC_NO_TEXT,
            "no text found in this document, even with OCR",
        )));
    }

    // Surface OCR-cap truncation to the caller (the FE shows a partial-import notice). Counts only.
    if pages_ocr_skipped > 0 {
        tracing::warn!(
            target: "documents",
            pages = page_count,
            ocred = pages_ocred,
            skipped = pages_ocr_skipped,
            cap = MAX_OCR_PAGES,
            "pdf: OCR page cap reached — some scanned pages were skipped (partial content)"
        );
        progress(ExtractProgress::OcrTruncated {
            ocred: pages_ocred,
            skipped: pages_ocr_skipped,
        });
    } else if pages_ocred > 0 {
        tracing::info!(target: "documents", pages = page_count, ocred = pages_ocred, "pdf: per-page OCR fallback produced text");
    }
    Ok(blocks)
}

/// Walk the PDF outline (`outlineRoot` tree) and collect `(page_index, label)` pairs, sorted by page
/// index. Every PDFKit call is inside `catch`; a missing/broken outline yields an empty map (headings
/// are best-effort). Returns `None` only if the outline root fetch itself throws.
fn outline_headings(doc: &PDFDocument, page_count: usize) -> Option<Vec<(usize, String)>> {
    let root = catch_objc(|| unsafe { doc.outlineRoot() }).flatten()?;

    let mut out: Vec<(usize, String)> = Vec::new();
    // Iterative DFS over the outline tree, bounded so a pathological/cyclic outline cannot spin.
    let mut stack: Vec<objc2::rc::Retained<objc2_pdf_kit::PDFOutline>> = vec![root];
    let mut visited = 0usize;
    const MAX_OUTLINE_NODES: usize = 4096;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_OUTLINE_NODES {
            break;
        }
        let n: usize = catch_objc(|| unsafe { node.numberOfChildren() }).unwrap_or(0);
        for idx in 0..n {
            let child = match catch_objc(|| unsafe { node.childAtIndex(idx) }).flatten() {
                Some(c) => c,
                None => continue,
            };
            // Label + destination page for this child (both inside catch).
            let label = catch_objc(|| unsafe { child.label() })
                .flatten()
                .map(|s| s.to_string())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let page_idx = outline_child_page_index(doc, &child, page_count);
            if let (Some(label), Some(pi)) = (label, page_idx) {
                out.push((pi, label));
            }
            stack.push(child);
        }
    }
    out.sort_by_key(|(pi, _)| *pi);
    Some(out)
}

/// The 0-based page index an outline entry points at, via `destination().page()` →
/// `doc.indexForPage(&page)`. All PDFKit calls inside `catch`; `None` on any failure / out-of-range.
fn outline_child_page_index(
    doc: &PDFDocument,
    child: &objc2_pdf_kit::PDFOutline,
    page_count: usize,
) -> Option<usize> {
    let page: objc2::rc::Retained<PDFPage> = catch_objc(|| {
        let dest = unsafe { child.destination() }?;
        unsafe { dest.page() }
    })
    .flatten()?;
    let idx: usize = catch_objc(|| unsafe { doc.indexForPage(&page) })?;
    // PDFKit returns NSNotFound (a very large value) when the page isn't in the doc — reject it.
    (idx < page_count).then_some(idx)
}

/// The heading label for a 0-based page: the outline entry whose destination page is the greatest
/// index ≤ the target page (i.e. the nearest heading at or above this page). `None` when no entry
/// qualifies (a page before the first outline entry, or no outline).
fn heading_for_page(headings: &[(usize, String)], page_index: usize) -> Option<String> {
    // `headings` is sorted by page index (asc), so the LAST entry whose page ≤ target is the nearest
    // preceding heading. Iterate from the back and take the first match (cheaper than `.last()`).
    headings
        .iter()
        .rev()
        .find(|(pi, _)| *pi <= page_index)
        .map(|(_, label)| label.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// heading_for_page picks the nearest-at-or-before outline entry (pure logic, no FFI).
    #[test]
    fn heading_for_page_picks_nearest_preceding_entry() {
        let headings = vec![
            (0usize, "Intro".to_string()),
            (3usize, "Design".to_string()),
            (7usize, "Appendix".to_string()),
        ];
        assert_eq!(heading_for_page(&headings, 0).as_deref(), Some("Intro"));
        assert_eq!(heading_for_page(&headings, 2).as_deref(), Some("Intro"));
        assert_eq!(heading_for_page(&headings, 3).as_deref(), Some("Design"));
        assert_eq!(heading_for_page(&headings, 6).as_deref(), Some("Design"));
        assert_eq!(heading_for_page(&headings, 99).as_deref(), Some("Appendix"));
        assert_eq!(heading_for_page(&[], 0), None);
    }

    /// A non-PDF file fails CLOSED with InvalidArg — proves the nil-document path (the FFI open
    /// returns nil for non-PDF bytes) does NOT abort and maps to our error. (True PDF fidelity needs
    /// a real Mac + a signed build; this only guards the fail-closed contract.)
    #[test]
    fn non_pdf_file_fails_closed_without_abort() {
        let mut p = std::env::temp_dir();
        p.push(format!("murmur-notpdf-{}.pdf", std::process::id()));
        std::fs::write(&p, b"this is definitely not a pdf").unwrap();
        let err = extract_pdf(&p, &crate::extract::no_progress).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)), "got {err:?}");
    }

    /// A missing path fails closed too (no panic).
    #[test]
    fn missing_pdf_fails_closed() {
        let p = std::path::Path::new("/nonexistent/murmur/does-not-exist.pdf");
        let err = extract_pdf(p, &crate::extract::no_progress).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)), "got {err:?}");
    }

    /// Fix 1 (per-page OCR DECISION, headless): a page whose trimmed text-layer string is empty or
    /// shorter than [`OCR_MIN_TEXT_CHARS`] needs OCR; a page with a real text layer does not. This is
    /// the pure core of the mixed-document fix — one text-layer COVER page (page 1) no longer
    /// suppresses OCR for the scanned pages behind it, because the decision is per-page. (End-to-end —
    /// does Vision actually read page 2/3 — needs a real Mac + a signed build + a real scanned PDF; the
    /// FFI render/OCR can't run headless. This test pins the DECISION that routes each page.)
    #[test]
    fn page_needs_ocr_decides_per_page() {
        // A real text-layer page (≥ 16 chars) does NOT need OCR.
        assert!(!page_needs_ocr(
            "This page has a real, extractable text layer."
        ));
        // An empty page (a scanned/image page with no text layer) needs OCR.
        assert!(page_needs_ocr(""));
        assert!(page_needs_ocr("   \n  "));
        // A near-empty page (a stray artifact under the threshold) needs OCR.
        assert!(page_needs_ocr("Ch. 1"));
        // Exactly at the threshold (16 chars) counts as a text layer (no OCR).
        assert!(!page_needs_ocr("abcdefghijklmnop")); // 16 chars
        assert!(page_needs_ocr("abcdefghijklmno")); // 15 chars → OCR
                                                    // Character count, not byte count: a short Polish heading is judged by glyphs. "zażółć gęślą"
                                                    // is 12 chars (< 16) → OCR, even though its UTF-8 byte length is larger.
        assert!(page_needs_ocr("zażółć gęślą"));
    }

    /// Fix 2: the OCR page cap is a named, generous-but-finite const — the availability bound that
    /// stops an unbounded scanned book from starving the heavy-inference permit. (The end-to-end cap
    /// behavior — a >300-page scanned PDF truncating with an OcrTruncated signal — needs a real Mac +
    /// signed build + a huge scanned fixture; here we pin the const is set to the documented value.)
    #[test]
    fn max_ocr_pages_is_bounded() {
        // Pinned to the documented value: generous (a full scanned book) yet finite (bounds the
        // worst-case OCR work + heavy-permit hold). A change here is a deliberate policy change.
        assert_eq!(
            MAX_OCR_PAGES, 300,
            "the OCR page cap is the documented bound"
        );
    }
}
