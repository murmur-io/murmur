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
//! SCANNED-PDF OCR FALLBACK (Brain v3): a PDF with NO extractable text layer on ANY page (a
//! scanned/image-only PDF) is NOT rejected — it falls back to on-device Apple Vision OCR
//! ([`crate::extract::ocr`]): each page is rendered to a CGImage and OCR'd, and any recognized text is
//! emitted as page blocks (heading `None`). The OCR path runs ONLY on the no-text-layer fallback, so a
//! normal text PDF is never slowed. Only if OCR ALSO yields nothing on EVERY page do we fail closed
//! with `InvalidArg("no text found in this document, even with OCR")`.
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

use super::ExtractedBlock;
use crate::error::{AppError, Result};

/// Map a caught-exception / nil / empty result into a single fail-closed error.
fn read_failed() -> AppError {
    AppError::InvalidArg("could not read PDF".into())
}

/// Run an ObjC closure that borrows non-`UnwindSafe` PDFKit handles inside `objc2::exception::catch`.
/// PDFKit reads have no interior-mutability hazard across a caught exception (we only read text /
/// outline; nothing is left half-mutated), so wrapping the borrows in `AssertUnwindSafe` is sound —
/// and it is REQUIRED because `Retained<_>`/`&PDFDocument` are not `UnwindSafe`. Returns `None` on a
/// caught ObjC exception, `Some(r)` otherwise (the closure's own `Option`/value is inside `r`).
fn catch_objc<R>(f: impl FnOnce() -> R) -> Option<R> {
    catch(AssertUnwindSafe(f)).ok()
}

/// Extract a PDF into per-page blocks. Fail-closed on any FFI exception / unreadable document; a
/// text-layer-less (scanned) PDF returns a distinct OCR-hint error.
pub fn extract_pdf(path: &Path) -> Result<Vec<ExtractedBlock>> {
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
        return Err(AppError::InvalidArg(
            "this PDF is password-protected — unlock it and re-import".into(),
        ));
    }

    let page_count: usize = catch_objc(|| unsafe { doc.pageCount() }).ok_or_else(read_failed)?;
    if page_count == 0 {
        return Err(read_failed());
    }

    // Best-effort outline → (page-index → heading label). Never let it fail the extraction.
    let headings = outline_headings(&doc, page_count).unwrap_or_default();

    let mut blocks: Vec<ExtractedBlock> = Vec::new();
    let mut any_text = false;
    for i in 0..page_count {
        // Each page fetch + `string` read is a separate catch — one bad page never aborts the rest.
        let page_text: Option<String> = catch_objc(|| {
            let page = unsafe { doc.pageAtIndex(i) }?;
            let s = unsafe { page.string() }?;
            Some(s.to_string())
        })
        .flatten();

        let text = page_text.unwrap_or_default();
        let trimmed = text.trim();
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
    }

    if !any_text {
        // A PDF with text on NO page is a scanned/image PDF → fall back to on-device Vision OCR.
        // This branch ONLY runs when the fast text-layer path found nothing, so a normal text PDF is
        // never slowed by rendering + OCR.
        return ocr_scanned_pdf(&doc, page_count, &headings);
    }

    Ok(blocks)
}

/// OCR fallback for a text-layer-less (scanned) PDF: render each page to a CGImage and run Vision OCR
/// ([`crate::extract::ocr::ocr_pdf_page`]). Emits one block per page that yielded recognized text
/// (page number preserved, heading from the outline if any); a page OCR can fail/blank without failing
/// the whole document. If OCR yields NOTHING on EVERY page, fail closed with a clear `InvalidArg`.
/// Every PDFKit/Vision/CoreGraphics call is crash-safe (wrapped in `catch` inside `ocr`).
fn ocr_scanned_pdf(
    doc: &PDFDocument,
    page_count: usize,
    headings: &[(usize, String)],
) -> Result<Vec<ExtractedBlock>> {
    let mut blocks: Vec<ExtractedBlock> = Vec::new();
    let mut any_ocr_text = false;
    for i in 0..page_count {
        // Fetch the page (inside catch) then OCR it. A page that fails to fetch / render / recognize
        // simply contributes no text — it never aborts the loop.
        let page = match catch_objc(|| unsafe { doc.pageAtIndex(i) }).flatten() {
            Some(p) => p,
            None => continue,
        };
        let ocr_text = crate::extract::ocr::ocr_pdf_page(&page).unwrap_or_default();
        let trimmed = ocr_text.trim();
        if !trimmed.is_empty() {
            any_ocr_text = true;
            blocks.push(ExtractedBlock {
                text: trimmed.to_string(),
                page: Some((i + 1) as u32),
                heading_path: heading_for_page(headings, i),
            });
        }
    }

    if !any_ocr_text {
        // No text layer AND OCR found nothing on any page — the document has no readable text.
        return Err(AppError::InvalidArg(
            "no text found in this document, even with OCR".into(),
        ));
    }
    tracing::info!(target: "documents", pages = page_count, ocr_blocks = blocks.len(), "pdf: scanned-PDF OCR fallback produced text");
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
        let err = extract_pdf(&p).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)), "got {err:?}");
    }

    /// A missing path fails closed too (no panic).
    #[test]
    fn missing_pdf_fails_closed() {
        let p = std::path::Path::new("/nonexistent/murmur/does-not-exist.pdf");
        let err = extract_pdf(p).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)), "got {err:?}");
    }
}
