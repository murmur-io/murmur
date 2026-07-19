//! Pure-Rust OOXML (DOCX / PPTX) extraction — NO docx/pptx crate. We open the file as a zip
//! container (`zip`) and stream its XML parts (`quick-xml`), both ALREADY compiled in the tree and
//! only promoted to direct deps for PR-2. Everything here is deterministic and fully unit-testable
//! headless (the tests build tiny in-memory .docx/.pptx zips), so unlike the PDFKit path this
//! verifies on any machine.
//!
//! DOCX (`word/document.xml`): stream paragraphs (`w:p`). A paragraph whose `w:pStyle` resolves to
//! a heading — via `word/styles.xml` `w:outlineLvl` (language-agnostic: a Polish `Nagłówek1` or
//! German `Überschrift1` styleId carries no English prefix) or the `Heading1`..`Heading9` prefix
//! fallback — updates the running heading trail; `w:t` runs concatenate into the paragraph text
//! (`w:br`/`w:cr` → newline, `w:tab` → tab); a table (`w:tbl`) renders its rows as pipe-delimited
//! (`a | b | c`) text, with NESTED tables flattened into the outer cell in reading order. Field
//! instructions (`w:instrText`) and tracked-changes deletions (`w:delText`) are NOT content and are
//! suppressed. Each non-heading paragraph → one [`ExtractedBlock`] carrying the CURRENT heading
//! trail (`page = None`).
//!
//! PPTX (`ppt/slides/slideN.xml`, ordered by N): each slide is one page (`page = Some(N)`). `a:t`
//! runs are the text; the TITLE placeholder (`p:ph` with `type="title"` or `type="ctrTitle"`) becomes
//! the slide's heading; a slide table (`p:graphicFrame` → `a:tbl`, which lives OUTSIDE any `p:sp`
//! shape) renders pipe-delimited rows like the DOCX idiom. One block per slide (all its body text
//! joined), heading = the title.
//!
//! Lock model: pure `path → blocks`, no DB/keychain. Failures map to `AppError::InvalidArg`
//! (fail-closed), never a panic. No PII logged.

use std::io::Read;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use super::{join_heading, ExtractedBlock};
use crate::error::{AppError, Result};

/// The XML version we normalize entities under — OOXML parts are XML 1.0. Used by the
/// feature-agnostic `decoded_and_normalized_value` / `xml_content` calls (the deprecated
/// `unescape`/`unescape_value` helpers are cfg'd OUT whenever quick-xml's `encoding` feature is
/// enabled anywhere in the tree — which it is here transitively — so we use the stable ones).
const XML_1_0: XmlVersion = XmlVersion::Implicit1_0;

/// Open `path` as a zip archive, reading the whole file into memory first (documents are bounded —
/// the ingest caller sizes them). Fail-closed to `InvalidArg`.
fn open_zip(path: &Path) -> Result<zip::ZipArchive<std::io::Cursor<Vec<u8>>>> {
    let bytes = std::fs::read(path)
        .map_err(|e| AppError::InvalidArg(format!("could not read document: {e}")))?;
    zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| AppError::InvalidArg(format!("not a valid OOXML (zip) document: {e}")))
}

/// A running DECOMPRESSED-bytes budget for ONE document's extraction — the decompression-bomb guard
/// (finding: OOM availability). Starts at [`MAX_EXTRACT_DECOMPRESSED_BYTES`] and is drained as each
/// zip entry is inflated; the first entry that would push the TOTAL past the ceiling fails closed
/// with [`AppError::InvalidArg`]. Bounds the ACTUAL bytes read (not the zip header's claimed size,
/// which a bomb lies about), so a tiny archive that inflates to gigabytes is stopped mid-read before
/// it can exhaust memory. NOT a cap on the original file size — a legitimately large document
/// (bounded, real XML) fits comfortably under the generous ceiling.
struct DecompressBudget {
    remaining: u64,
}

impl DecompressBudget {
    fn new() -> Self {
        Self::with_ceiling(crate::extract::MAX_EXTRACT_DECOMPRESSED_BYTES)
    }
    /// A budget with an explicit ceiling — the production path uses [`Self::new`]
    /// ([`crate::extract::MAX_EXTRACT_DECOMPRESSED_BYTES`]); tests inject a tiny ceiling so a small
    /// synthetic archive can trip the bomb guard without materializing 512 MiB.
    fn with_ceiling(ceiling: u64) -> Self {
        DecompressBudget { remaining: ceiling }
    }
}

/// DECOMPRESSION-BOMB pre-flight for a zip-container document whose entries we DON'T read directly
/// (the XLSX path hands the file to `calamine`, which inflates internally). Opens `path` as a zip and
/// bounded-inflates EVERY entry against one [`DecompressBudget`]; the first entry that pushes the
/// running total past [`crate::extract::MAX_EXTRACT_DECOMPRESSED_BYTES`] fails closed with
/// `InvalidArg("document too large / possible zip bomb")`. A non-zip / corrupt file also fails closed
/// here (mirrors calamine's own open failure into the same InvalidArg domain). Reads each entry via
/// `Read::take` so a lying header can't force a giant allocation — the guard itself is bounded by the
/// ceiling. This inflates once up front, then calamine inflates the (now-proven-bounded) file again;
/// the cost is bounded by the ceiling and only paid on the (rare) large XLSX.
pub(crate) fn guard_zip_not_a_bomb(path: &Path) -> Result<()> {
    guard_zip_with_ceiling(path, crate::extract::MAX_EXTRACT_DECOMPRESSED_BYTES)
}

/// [`guard_zip_not_a_bomb`] with an explicit ceiling — production passes the shared const; tests pass
/// a tiny ceiling so a small synthetic over-limit archive trips the guard without a 512 MiB payload.
fn guard_zip_with_ceiling(path: &Path, ceiling: u64) -> Result<()> {
    let mut zip = open_zip(path)?;
    let mut budget = DecompressBudget::with_ceiling(ceiling);
    for i in 0..zip.len() {
        let file = zip
            .by_index(i)
            .map_err(|e| AppError::InvalidArg(format!("corrupt zip entry: {e}")))?;
        // Discard the inflated bytes — we only need to prove the TOTAL stays under the ceiling.
        let mut sink = SinkCounter::default();
        let limit = budget.remaining.saturating_add(1);
        let read = std::io::copy(&mut file.take(limit), &mut sink)
            .map_err(|e| AppError::InvalidArg(format!("could not decode zip entry: {e}")))?;
        if read > budget.remaining {
            return Err(AppError::InvalidArg(
                "document too large / possible zip bomb".into(),
            ));
        }
        budget.remaining -= read;
    }
    Ok(())
}

/// A `Write` sink that only counts bytes (never allocates) — for the [`guard_zip_not_a_bomb`]
/// pre-flight, which needs the inflated SIZE, not the inflated content.
#[derive(Default)]
struct SinkCounter {
    _n: u64,
}

impl std::io::Write for SinkCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self._n = self._n.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Read ONE named entry from the archive as a UTF-8 string, charging its DECOMPRESSED size against
/// `budget`. `None` if the entry is absent; `Err` on a corrupt entry OR on a decompression-bomb
/// (the inflated bytes would exceed the remaining budget). We read at most `remaining + 1` bytes via
/// `Read::take` so a lying zip header can't trick us into allocating gigabytes: if the reader yields
/// MORE than `remaining`, it's over the ceiling and we fail closed without holding the whole bomb.
fn read_entry(
    zip: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
    name: &str,
    budget: &mut DecompressBudget,
) -> Result<Option<String>> {
    let file = match zip.by_name(name) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(AppError::InvalidArg(format!("corrupt OOXML entry: {e}"))),
    };
    read_capped(file, budget).map(Some)
}

/// Inflate a single zip entry into a `String`, bounded by `budget`. Reads at most `remaining + 1`
/// bytes; if the source produced more than `remaining` (or the running total would exceed the
/// ceiling), fail closed as a possible zip bomb. On success, charges the actual byte count against
/// the budget. Shared by DOCX/PPTX (and the sibling XLSX guard mirrors the same ceiling).
fn read_capped<R: Read>(reader: R, budget: &mut DecompressBudget) -> Result<String> {
    // Allow reading up to `remaining + 1` so an entry that EXACTLY fills the remaining budget still
    // succeeds, while the +1 overflow byte trips the bomb check. `usize` guard: on a 32-bit target
    // clamp so `take`'s limit is representable (the budget is far below usize::MAX on 64-bit).
    let limit = budget.remaining.saturating_add(1);
    let mut buf = Vec::new();
    let read = reader
        .take(limit)
        .read_to_end(&mut buf)
        .map_err(|e| AppError::InvalidArg(format!("could not decode OOXML entry: {e}")))?
        as u64;
    if read > budget.remaining {
        return Err(AppError::InvalidArg(
            "document too large / possible zip bomb".into(),
        ));
    }
    budget.remaining -= read;
    String::from_utf8(buf).map_err(|e| AppError::InvalidArg(format!("could not decode OOXML entry: {e}")))
}

/// The local name of an element (strip the `w:` / `a:` / `p:` namespace prefix).
fn local_name(raw: &[u8]) -> &[u8] {
    match raw.iter().position(|&b| b == b':') {
        Some(i) => &raw[i + 1..],
        None => raw,
    }
}

/// The value of an attribute whose LOCAL name matches `want` (namespace-prefix-insensitive), decoded
/// + normalized to an owned `String`. `None` when absent. `decoder` comes from the active reader.
fn attr_local<'a>(
    e: &'a quick_xml::events::BytesStart<'a>,
    want: &[u8],
    decoder: quick_xml::encoding::Decoder,
) -> Option<String> {
    for a in e.attributes().flatten() {
        if local_name(a.key.as_ref()) == want {
            return a
                .decoded_and_normalized_value(XML_1_0, decoder)
                .ok()
                .map(|v| v.into_owned());
        }
    }
    None
}

/// Parse a `w:pStyle` / heading style value into a 1..=9 heading level, or `None` if it is not a
/// `HeadingN` style. Case-insensitive on the "heading" prefix; accepts the common `Heading1` and the
/// display-name `heading 1` shapes. FALLBACK only — a localized Word style (`Nagłówek1`,
/// `Überschrift1`) has no English prefix and resolves through [`parse_styles_heading_levels`].
fn heading_level(style_val: &str) -> Option<u8> {
    let v = style_val.trim().to_ascii_lowercase();
    let rest = v.strip_prefix("heading")?.trim_start();
    let n: u8 = rest.parse().ok()?;
    (1..=9).contains(&n).then_some(n)
}

/// Heading levels resolved from `word/styles.xml`: `w:styleId` → 1..=9.
type StyleHeadings = std::collections::HashMap<String, u8>;

/// Best-effort parse of `word/styles.xml` into a styleId → heading-level map via `w:outlineLvl`
/// (0-based outline level 0..=8 → heading level `lvl + 1`; 9 means body text per OOXML). The
/// outline level is the language-agnostic truth for heading detection, so localized style names
/// resolve without any per-language table. Fidelity METADATA only: a malformed styles.xml degrades
/// to whatever parsed cleanly before the error (the document body still extracts; the English
/// prefix fallback in [`heading_level`] still applies) rather than failing the whole extraction.
fn parse_styles_heading_levels(xml: &str) -> StyleHeadings {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let decoder = reader.decoder();

    let mut map = StyleHeadings::new();
    let mut current_style: Option<String> = None;
    let mut current_level: Option<u8> = None;

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) => match local_name(e.name().as_ref()) {
                b"style" => {
                    current_style = attr_local(&e, b"styleId", decoder);
                    current_level = None;
                }
                b"outlineLvl" => {
                    if let Some(v) = attr_local(&e, b"val", decoder) {
                        if let Ok(lvl) = v.trim().parse::<u8>() {
                            if lvl <= 8 {
                                current_level = Some(lvl + 1);
                            }
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => {
                if local_name(e.name().as_ref()) == b"style" {
                    if let (Some(id), Some(lvl)) = (current_style.take(), current_level.take()) {
                        map.insert(id, lvl);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break, // best-effort metadata — keep what parsed cleanly
            _ => {}
        }
        buf.clear();
    }
    map
}

/// The running heading trail: a stack of (level, label). Pushing a heading of level L pops every
/// entry at level ≥ L first (so an H2 replaces the previous H2/H3/… but keeps the H1), then pushes
/// the new one. [`Self::path`] renders the trail joined by ` › `.
#[derive(Default)]
struct HeadingStack {
    stack: Vec<(u8, String)>,
}

impl HeadingStack {
    fn push(&mut self, level: u8, label: String) {
        self.stack.retain(|(l, _)| *l < level);
        self.stack.push((level, label));
    }
    /// The current trail as an `Option<String>` (`None` when empty), built with the shared joiner.
    fn path(&self) -> Option<String> {
        if self.stack.is_empty() {
            return None;
        }
        let mut acc: Option<String> = None;
        for (_, label) in &self.stack {
            acc = Some(join_heading(acc.as_deref(), label));
        }
        acc
    }
}

/// Extract a DOCX into blocks (one per non-heading paragraph, carrying the current heading trail).
pub fn extract_docx(path: &Path) -> Result<Vec<ExtractedBlock>> {
    let mut zip = open_zip(path)?;
    let mut budget = DecompressBudget::new();
    // Heading styles first, charged against the SAME decompression budget as the body (one ceiling
    // per document — styles.xml is not a free side-channel for a bomb). Absent → empty map, and the
    // English `HeadingN` prefix fallback still applies.
    let style_headings = read_entry(&mut zip, "word/styles.xml", &mut budget)?
        .map(|xml| parse_styles_heading_levels(&xml))
        .unwrap_or_default();
    let Some(xml) = read_entry(&mut zip, "word/document.xml", &mut budget)? else {
        return Err(AppError::InvalidArg(
            "DOCX is missing word/document.xml".into(),
        ));
    };
    parse_docx_xml(&xml, &style_headings)
}

/// Per-open-`w:tbl` accumulation state. Tables NEST (a cell may contain a whole inner table), and a
/// flat clear-on-`w:tr`/`w:tc` state machine wipes the outer cell's captured text and its earlier
/// sibling cells the moment an inner table starts — one frame per open table keeps each nesting
/// level's state isolated so nothing already extracted is destroyed.
#[derive(Default)]
struct TableFrame {
    /// Completed cells of the current row.
    row_cells: Vec<String>,
    /// Text accumulating for the currently open cell.
    cell_text: String,
    /// Whether a `w:tc` is open in THIS table (routes text into `cell_text`).
    in_cell: bool,
}

/// Route one rendered table row to its destination: with no enclosing table it becomes a content
/// block under the current heading trail; inside an outer table it FLATTENS into that table's open
/// cell text (newline-separated), so nested-table content survives in reading order.
fn deposit_docx_row(
    parent: Option<&mut TableFrame>,
    blocks: &mut Vec<ExtractedBlock>,
    headings: &HeadingStack,
    row: String,
) {
    if row.trim().is_empty() {
        return;
    }
    match parent {
        Some(f) => {
            if !f.cell_text.is_empty() && !f.cell_text.ends_with('\n') {
                f.cell_text.push('\n');
            }
            f.cell_text.push_str(&row);
        }
        None => blocks.push(ExtractedBlock {
            text: row,
            page: None,
            heading_path: headings.path(),
        }),
    }
}

/// Push a literal whitespace char produced by an empty layout element (`w:br`/`w:cr` → newline,
/// `w:tab` → tab) into whatever sink is accumulating: the open table cell if any, else the
/// paragraph. Without these, visually separate lines glue into one word.
fn push_layout_char(tables: &mut [TableFrame], para_text: &mut String, c: char) {
    match tables.last_mut() {
        Some(f) if f.in_cell => f.cell_text.push(c),
        _ => para_text.push(c),
    }
}

/// The DOCX body walk, split out for direct unit testing on a raw `document.xml` string.
/// `style_headings` maps `w:styleId` → heading level (from `word/styles.xml`); the English
/// `HeadingN` prefix remains as fallback for documents without a styles part.
fn parse_docx_xml(xml: &str, style_headings: &StyleHeadings) -> Result<Vec<ExtractedBlock>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let decoder = reader.decoder();

    let mut blocks: Vec<ExtractedBlock> = Vec::new();
    let mut headings = HeadingStack::default();

    // Per-paragraph accumulator (top-level paragraphs only — cell text lives in its TableFrame).
    let mut in_paragraph = false;
    let mut para_text = String::new();
    let mut para_heading: Option<u8> = None;
    // One frame per open `w:tbl`, innermost on top.
    let mut tables: Vec<TableFrame> = Vec::new();
    // Whether a char-run is being captured (armed at `w:p`/`w:t` start, disarmed at `w:t` end).
    let mut capture_text = false;
    // Depth inside content that is NOT document text and must never reach the brain: field
    // plumbing (`w:instrText`, and its deleted-tracked-changes form `w:delInstrText`) and
    // tracked-changes DELETED runs (`w:del`, which wraps `w:delText` AND any layout breaks the
    // author deleted — suppressing the whole `w:del` wrapper, not just its `w:delText`, keeps a
    // `w:br`/`w:tab`/`w:cr` inside a deleted run from injecting stray whitespace).
    let mut suppress_depth: u32 = 0;

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local_name(e.name().as_ref()) {
                b"tbl" => tables.push(TableFrame::default()),
                b"tr" => {
                    if let Some(f) = tables.last_mut() {
                        f.row_cells.clear();
                    }
                }
                b"tc" => {
                    if let Some(f) = tables.last_mut() {
                        f.in_cell = true;
                        f.cell_text.clear();
                    }
                }
                b"p" => {
                    match tables.last_mut() {
                        // A later paragraph in an open cell: newline-separate it from what the
                        // cell already holds (earlier paragraphs / a flattened nested table).
                        Some(f) if f.in_cell => {
                            if !f.cell_text.is_empty() && !f.cell_text.ends_with('\n') {
                                f.cell_text.push('\n');
                            }
                        }
                        _ => {
                            in_paragraph = true;
                            para_text.clear();
                            para_heading = None;
                        }
                    }
                    capture_text = true;
                }
                b"t" => capture_text = true,
                b"instrText" | b"delInstrText" | b"delText" | b"del" => {
                    suppress_depth = suppress_depth.saturating_add(1)
                }
                // Word writes breaks self-closed (see the Empty arm); tolerate the expanded form.
                b"br" | b"cr" if suppress_depth == 0 => {
                    push_layout_char(&mut tables, &mut para_text, '\n');
                }
                b"tab" if suppress_depth == 0 => {
                    push_layout_char(&mut tables, &mut para_text, '\t');
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match local_name(e.name().as_ref()) {
                // `w:pStyle w:val="X"`: resolve through styles.xml outlineLvl first (language-
                // agnostic), then the English HeadingN prefix fallback.
                b"pStyle" => {
                    if let Some(v) = attr_local(&e, b"val", decoder) {
                        let lvl = style_headings
                            .get(v.trim())
                            .copied()
                            .or_else(|| heading_level(&v));
                        if let Some(lvl) = lvl {
                            para_heading = Some(lvl);
                        }
                    }
                }
                b"br" | b"cr" if suppress_depth == 0 => {
                    push_layout_char(&mut tables, &mut para_text, '\n');
                }
                b"tab" if suppress_depth == 0 => {
                    push_layout_char(&mut tables, &mut para_text, '\t');
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if capture_text && suppress_depth == 0 {
                    let txt = t
                        .xml_content(XML_1_0)
                        .map_err(|e| AppError::InvalidArg(format!("bad DOCX text: {e}")))?;
                    match tables.last_mut() {
                        Some(f) if f.in_cell => f.cell_text.push_str(&txt),
                        _ => para_text.push_str(&txt),
                    }
                }
            }
            Ok(Event::End(e)) => match local_name(e.name().as_ref()) {
                b"t" => capture_text = false,
                b"instrText" | b"delInstrText" | b"delText" | b"del" => {
                    suppress_depth = suppress_depth.saturating_sub(1)
                }
                b"tc" => {
                    if let Some(f) = tables.last_mut() {
                        f.in_cell = false;
                        let cell = std::mem::take(&mut f.cell_text);
                        f.row_cells.push(cell.trim().to_string());
                    }
                }
                b"tr" => {
                    let row = match tables.last_mut() {
                        Some(f) => {
                            let cells = std::mem::take(&mut f.row_cells);
                            cells.join(" | ")
                        }
                        None => String::new(),
                    };
                    let n = tables.len();
                    let parent = if n >= 2 { tables.get_mut(n - 2) } else { None };
                    deposit_docx_row(parent, &mut blocks, &headings, row);
                }
                b"tbl" => {
                    // Salvage any leftover state (a malformed table missing its tr/tc closes) —
                    // text is never dropped on the floor — then dispose the frame.
                    if let Some(f) = tables.pop() {
                        let mut cells = f.row_cells;
                        let tail = f.cell_text.trim();
                        if !tail.is_empty() {
                            cells.push(tail.to_string());
                        }
                        deposit_docx_row(tables.last_mut(), &mut blocks, &headings, cells.join(" | "));
                    }
                }
                b"p" => {
                    let in_cell = tables.last().map(|f| f.in_cell).unwrap_or(false);
                    if in_paragraph && !in_cell {
                        let text = para_text.trim().to_string();
                        if let Some(lvl) = para_heading {
                            if !text.is_empty() {
                                headings.push(lvl, text);
                            }
                        } else if !text.is_empty() {
                            blocks.push(ExtractedBlock {
                                text,
                                page: None,
                                heading_path: headings.path(),
                            });
                        }
                    }
                    in_paragraph = false;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(AppError::InvalidArg(format!("malformed DOCX XML: {e}")));
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(blocks)
}

/// Extract a PPTX into blocks — one per slide, in slide-number order (`ppt/slides/slideN.xml`).
pub fn extract_pptx(path: &Path) -> Result<Vec<ExtractedBlock>> {
    let mut zip = open_zip(path)?;
    // ONE decompression budget across ALL slides — a bomb split into many entries can't evade the
    // ceiling by staying under it per-entry (the total across slides is what's charged).
    let mut budget = DecompressBudget::new();

    // Collect the slide entry names, then sort by the numeric N in `slideN.xml` (a lexicographic
    // sort would order slide10 before slide2 — wrong page order).
    let mut slide_names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .collect();
    slide_names.sort_by_key(|n| slide_number(n));

    let mut blocks: Vec<ExtractedBlock> = Vec::new();
    for name in slide_names {
        let num = slide_number(&name);
        if let Some(xml) = read_entry(&mut zip, &name, &mut budget)? {
            if let Some(block) = parse_pptx_slide(&xml, num)? {
                blocks.push(block);
            }
        }
    }
    Ok(blocks)
}

/// The trailing integer N of a `.../slideN.xml` name (0 when it can't be parsed — deterministic).
fn slide_number(name: &str) -> u32 {
    let stem = name
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .trim_end_matches(".xml")
        .trim_start_matches("slide");
    stem.parse().unwrap_or(0)
}

/// Parse ONE slide's XML into a block: title placeholder → heading; all `a:t` runs → body text;
/// slide tables (`p:graphicFrame` → `a:tbl`, OUTSIDE any shape) → pipe-delimited rows.
/// `None` when the slide has no extractable text at all.
fn parse_pptx_slide(xml: &str, slide_num: u32) -> Result<Option<ExtractedBlock>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let decoder = reader.decoder();

    let mut title: Option<String> = None;
    let mut body_paras: Vec<String> = Vec::new();

    // A `p:sp` (shape) is one text container. Track whether the current shape is the TITLE placeholder
    // (its `p:ph type="title"|"ctrTitle"`), and accumulate its text. `a:p` = a paragraph inside the
    // shape's text body; `a:t` = a text run.
    let mut in_shape = false;
    let mut shape_is_title = false;
    let mut shape_text = String::new();
    let mut capture_text = false;

    // A DrawingML table (`a:tbl` inside `p:graphicFrame`) lives OUTSIDE any `p:sp`, so the
    // `in_shape` gate alone drops table-only slides entirely. Its cells (`a:tr`/`a:tc`, text under
    // the cell's `a:txBody`) render pipe-delimited rows like the DOCX table idiom. DrawingML
    // tables cannot nest (a cell holds only a text body), so flat state suffices here.
    let mut in_table = false;
    let mut table_row_cells: Vec<String> = Vec::new();
    let mut table_cell_text = String::new();
    let mut in_table_cell = false;

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local_name(e.name().as_ref()) {
                b"sp" => {
                    in_shape = true;
                    shape_is_title = false;
                    shape_text.clear();
                }
                b"ph" => {
                    if in_shape {
                        if let Some(ty) = attr_local(&e, b"type", decoder) {
                            let ty = ty.to_ascii_lowercase();
                            if ty == "title" || ty == "ctrtitle" {
                                shape_is_title = true;
                            }
                        }
                    }
                }
                b"tbl" => in_table = true,
                b"tr" if in_table => table_row_cells.clear(),
                b"tc" if in_table => {
                    in_table_cell = true;
                    table_cell_text.clear();
                }
                b"t" => capture_text = true,
                // paragraph boundary inside a shape → newline between paragraphs.
                b"p" if in_shape && !shape_text.is_empty() && !shape_text.ends_with('\n') => {
                    shape_text.push('\n');
                }
                // paragraph boundary inside a table cell → same newline separation.
                b"p" if in_table_cell
                    && !table_cell_text.is_empty()
                    && !table_cell_text.ends_with('\n') =>
                {
                    table_cell_text.push('\n');
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                if local_name(e.name().as_ref()) == b"ph" && in_shape {
                    if let Some(ty) = attr_local(&e, b"type", decoder) {
                        let ty = ty.to_ascii_lowercase();
                        if ty == "title" || ty == "ctrtitle" {
                            shape_is_title = true;
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if capture_text && (in_shape || in_table_cell) {
                    let txt = t
                        .xml_content(XML_1_0)
                        .map_err(|e| AppError::InvalidArg(format!("bad PPTX text: {e}")))?;
                    if in_table_cell {
                        table_cell_text.push_str(&txt);
                    } else {
                        shape_text.push_str(&txt);
                    }
                }
            }
            Ok(Event::End(e)) => match local_name(e.name().as_ref()) {
                b"t" => capture_text = false,
                b"sp" => {
                    let text = shape_text.trim().to_string();
                    if !text.is_empty() {
                        if shape_is_title && title.is_none() {
                            title = Some(text);
                        } else {
                            body_paras.push(text);
                        }
                    }
                    in_shape = false;
                }
                b"tc" if in_table => {
                    in_table_cell = false;
                    let cell = std::mem::take(&mut table_cell_text);
                    table_row_cells.push(cell.trim().to_string());
                }
                b"tr" if in_table => {
                    let row = std::mem::take(&mut table_row_cells).join(" | ");
                    if !row.trim().is_empty() {
                        body_paras.push(row);
                    }
                }
                b"tbl" => in_table = false,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(AppError::InvalidArg(format!("malformed PPTX XML: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    let body = body_paras.join("\n");
    if title.is_none() && body.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(ExtractedBlock {
        text: body,
        page: Some(slide_num),
        heading_path: title,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal OOXML .docx/.pptx zip in memory from (entry-name, content) pairs and write it
    /// to a temp file, returning the path.
    fn build_ooxml(ext: &str, entries: &[(&str, &str)]) -> std::path::PathBuf {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cursor);
            let opts: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, content) in entries {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(content.as_bytes()).unwrap();
            }
            zw.finish().unwrap();
        }
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-ooxml-{}-{}.{ext}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&p, cursor.into_inner()).unwrap();
        p
    }

    const DOCX_XML: &str = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Design</w:t></w:r></w:p>
    <w:p><w:r><w:t>The budget is </w:t></w:r><w:r><w:t>100k.</w:t></w:r></w:p>
    <w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Storage</w:t></w:r></w:p>
    <w:p><w:r><w:t>Anna owns delivery.</w:t></w:r></w:p>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Owner</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>API</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Bob</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;

    /// DOCX: paragraph text is extracted, `w:t` runs concatenate, headings build the trail, and a
    /// table renders pipe-delimited rows under the current heading.
    #[test]
    fn docx_extracts_paragraphs_headings_and_table_rows() {
        let p = build_ooxml("docx", &[("word/document.xml", DOCX_XML)]);
        let blocks = extract_docx(&p).unwrap();

        // Body paragraphs: budget (under Design), Anna (under Design › Storage), + 2 table rows.
        let budget = blocks.iter().find(|b| b.text.contains("budget")).unwrap();
        assert_eq!(budget.text, "The budget is 100k.", "w:t runs must concatenate");
        assert_eq!(budget.heading_path.as_deref(), Some("Design"));

        let anna = blocks.iter().find(|b| b.text.contains("Anna")).unwrap();
        assert_eq!(anna.heading_path.as_deref(), Some("Design › Storage"));

        let header_row = blocks.iter().find(|b| b.text.contains("Owner")).unwrap();
        assert_eq!(header_row.text, "Name | Owner", "table row → pipe-delimited");
        assert_eq!(header_row.heading_path.as_deref(), Some("Design › Storage"));
        assert!(
            blocks.iter().any(|b| b.text == "API | Bob"),
            "second table row must render too"
        );
        // Headings themselves are NOT emitted as content blocks.
        assert!(
            !blocks.iter().any(|b| b.text == "Design" || b.text == "Storage"),
            "heading text must not become a content block"
        );
        // Every block has page None (DOCX is a flow format).
        assert!(blocks.iter().all(|b| b.page.is_none()));
    }

    const PPTX_SLIDE1: &str = r#"<?xml version="1.0"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld><p:spTree>
    <p:sp>
      <p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
      <p:txBody><a:p><a:r><a:t>Quarterly Plan</a:t></a:r></a:p></p:txBody>
    </p:sp>
    <p:sp>
      <p:txBody>
        <a:p><a:r><a:t>Ship the ingest feature</a:t></a:r></a:p>
        <a:p><a:r><a:t>Hire two engineers</a:t></a:r></a:p>
      </p:txBody>
    </p:sp>
  </p:spTree></p:cSld>
</p:sld>"#;

    const PPTX_SLIDE2: &str = r#"<?xml version="1.0"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld><p:spTree>
    <p:sp><p:txBody><a:p><a:r><a:t>Closing thoughts</a:t></a:r></a:p></p:txBody></p:sp>
  </p:spTree></p:cSld>
</p:sld>"#;

    /// PPTX: title placeholder → heading, body runs → text, slide N → page, in slide-number order
    /// (slide2 does NOT sort before slide10-style names).
    #[test]
    fn pptx_extracts_title_body_and_slide_page_in_order() {
        // Insert slide2 BEFORE slide1 in the zip to prove we sort by slide number, not zip order.
        let p = build_ooxml(
            "pptx",
            &[
                ("ppt/slides/slide2.xml", PPTX_SLIDE2),
                ("ppt/slides/slide1.xml", PPTX_SLIDE1),
            ],
        );
        let blocks = extract_pptx(&p).unwrap();
        assert_eq!(blocks.len(), 2, "one block per slide");

        assert_eq!(blocks[0].page, Some(1), "slide1 must come first");
        assert_eq!(blocks[0].heading_path.as_deref(), Some("Quarterly Plan"));
        assert!(blocks[0].text.contains("Ship the ingest feature"));
        assert!(blocks[0].text.contains("Hire two engineers"));

        assert_eq!(blocks[1].page, Some(2), "slide2 second");
        assert!(blocks[1].text.contains("Closing thoughts"));
    }

    /// A non-zip / corrupt file fails closed with InvalidArg.
    #[test]
    fn corrupt_docx_is_invalid_arg() {
        let mut p = std::env::temp_dir();
        p.push(format!("murmur-badzip-{}.docx", std::process::id()));
        std::fs::write(&p, b"not a zip at all").unwrap();
        let err = extract_docx(&p).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)), "got {err:?}");
    }

    #[test]
    fn heading_level_parses_word_styles() {
        assert_eq!(heading_level("Heading1"), Some(1));
        assert_eq!(heading_level("heading 3"), Some(3));
        assert_eq!(heading_level("Heading9"), Some(9));
        assert_eq!(heading_level("Heading10"), None);
        assert_eq!(heading_level("BodyText"), None);
        assert_eq!(heading_level("Title"), None);
    }

    // ── DECOMPRESSION-BOMB GUARD (finding 2: OOM availability) ────────────────────────────────────
    //
    // A highly-compressible payload (a long run of one char) deflates to a few bytes but inflates to
    // its full length — the classic zip-bomb shape. We inject a TINY ceiling so a modest synthetic
    // entry trips the guard without materializing 512 MiB. The production ceiling
    // (`MAX_EXTRACT_DECOMPRESSED_BYTES`) is proven separately to be far above real documents.

    /// The `read_capped` entry-read path (used by DOCX/PPTX extraction) fails CLOSED when an entry's
    /// DECOMPRESSED size exceeds the remaining budget — the bytes are never fully buffered.
    #[test]
    fn read_capped_rejects_oversized_entry() {
        // 4 KiB of a single char (deflates tiny) against a 1 KiB budget → over the ceiling.
        let payload = "A".repeat(4096);
        let p = build_ooxml("docx", &[("word/document.xml", &payload)]);
        let mut zip = open_zip(&p).unwrap();
        let mut budget = DecompressBudget::with_ceiling(1024);
        let err = read_entry(&mut zip, "word/document.xml", &mut budget).unwrap_err();
        assert!(
            matches!(&err, AppError::InvalidArg(m) if m.contains("zip bomb")),
            "oversized entry must be rejected as a zip bomb, got {err:?}"
        );
    }

    /// A normal-sized entry passes `read_capped` and DRAINS exactly its byte count from the budget.
    #[test]
    fn read_capped_accepts_normal_entry_and_charges_budget() {
        let payload = "hello world"; // 11 bytes
        let p = build_ooxml("docx", &[("word/document.xml", payload)]);
        let mut zip = open_zip(&p).unwrap();
        let mut budget = DecompressBudget::with_ceiling(1_000_000);
        let got = read_entry(&mut zip, "word/document.xml", &mut budget)
            .unwrap()
            .unwrap();
        assert_eq!(got, payload);
        assert_eq!(
            budget.remaining,
            1_000_000 - payload.len() as u64,
            "the budget must be charged the actual decompressed byte count"
        );
    }

    /// The `guard_zip_not_a_bomb` PRE-FLIGHT (used by the XLSX path, which lets calamine inflate)
    /// fails CLOSED when the TOTAL decompressed size across entries exceeds the ceiling.
    #[test]
    fn guard_zip_rejects_bomb_across_entries() {
        // Two entries of 3 KiB each = 6 KiB total, against a 4 KiB ceiling → tripped.
        let big = "Z".repeat(3072);
        let p = build_ooxml(
            "xlsx",
            &[("xl/a.xml", &big), ("xl/b.xml", &big)],
        );
        let err = guard_zip_with_ceiling(&p, 4096).unwrap_err();
        assert!(
            matches!(&err, AppError::InvalidArg(m) if m.contains("zip bomb")),
            "total decompressed size over the ceiling must be rejected, got {err:?}"
        );
    }

    /// The pre-flight guard PASSES a within-ceiling archive (a legitimate document is untouched).
    #[test]
    fn guard_zip_accepts_within_ceiling() {
        let small = "ok".repeat(50); // 100 bytes
        let p = build_ooxml("xlsx", &[("xl/a.xml", &small)]);
        assert!(
            guard_zip_with_ceiling(&p, 1_000_000).is_ok(),
            "a within-ceiling archive must pass the bomb guard"
        );
    }

    /// The production ceiling is generous (512 MiB) — a normal DOCX extracts fine under it (the guard
    /// is invisible to real documents; only a bomb hits it).
    #[test]
    fn production_ceiling_leaves_normal_docx_unaffected() {
        let p = build_ooxml("docx", &[("word/document.xml", DOCX_XML)]);
        let blocks = extract_docx(&p).unwrap();
        assert!(!blocks.is_empty(), "a normal DOCX must extract under the production ceiling");
    }

    // ── OOXML FIDELITY (audit fixes: nested tables, localized headings, whitespace, field junk) ──

    /// A table NESTED inside an outer cell must not destroy the outer cell's already-captured text
    /// nor its earlier sibling cells, and reading order must hold (the flat clear-on-`w:tr`/`w:tc`
    /// state machine wiped both).
    #[test]
    fn docx_nested_table_preserves_outer_and_sibling_text() {
        const XML: &str = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>FIRST CELL</w:t></w:r></w:p></w:tc>
        <w:tc>
          <w:p><w:r><w:t>OUTER TEXT</w:t></w:r></w:p>
          <w:tbl>
            <w:tr><w:tc><w:p><w:r><w:t>INNER</w:t></w:r></w:p></w:tc></w:tr>
          </w:tbl>
          <w:p><w:r><w:t>AFTER</w:t></w:r></w:p>
        </w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;
        let p = build_ooxml("docx", &[("word/document.xml", XML)]);
        let blocks = extract_docx(&p).unwrap();
        let all = blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            all.contains("FIRST CELL"),
            "sibling cell before the nested table must survive; got: {all:?}"
        );
        assert!(
            all.contains("OUTER TEXT"),
            "outer-cell text captured before the nested table must survive; got: {all:?}"
        );
        assert!(all.contains("INNER"), "nested-table text must extract; got: {all:?}");
        assert!(
            all.contains("AFTER"),
            "outer-cell text after the nested table must survive; got: {all:?}"
        );
        let (a, b, c, d) = (
            all.find("FIRST CELL").unwrap(),
            all.find("OUTER TEXT").unwrap(),
            all.find("INNER").unwrap(),
            all.find("AFTER").unwrap(),
        );
        assert!(a < b && b < c && c < d, "reading order must be preserved; got: {all:?}");
    }

    /// Localized Word heading styles (a Polish `Nagłówek1`, no English `heading` prefix) resolve
    /// through `word/styles.xml` `w:outlineLvl` — the language-agnostic OOXML truth.
    #[test]
    fn docx_localized_heading_styles_resolve_via_styles_xml_outline_level() {
        const STYLES: &str = r#"<?xml version="1.0"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Nagłówek1">
    <w:name w:val="heading 1"/>
    <w:pPr><w:outlineLvl w:val="0"/></w:pPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="Nagłówek2">
    <w:name w:val="heading 2"/>
    <w:pPr><w:outlineLvl w:val="1"/></w:pPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="Zwykly">
    <w:name w:val="Normal"/>
  </w:style>
</w:styles>"#;
        const DOC: &str = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:pPr><w:pStyle w:val="Nagłówek1"/></w:pPr><w:r><w:t>Wprowadzenie</w:t></w:r></w:p>
    <w:p><w:r><w:t>Pierwszy akapit.</w:t></w:r></w:p>
    <w:p><w:pPr><w:pStyle w:val="Nagłówek2"/></w:pPr><w:r><w:t>Szczegóły</w:t></w:r></w:p>
    <w:p><w:pPr><w:pStyle w:val="Zwykly"/></w:pPr><w:r><w:t>Drugi akapit.</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        let p = build_ooxml(
            "docx",
            &[("word/styles.xml", STYLES), ("word/document.xml", DOC)],
        );
        let blocks = extract_docx(&p).unwrap();
        let first = blocks.iter().find(|b| b.text.contains("Pierwszy")).unwrap();
        assert_eq!(
            first.heading_path.as_deref(),
            Some("Wprowadzenie"),
            "a localized H1 style must set the heading trail"
        );
        let second = blocks.iter().find(|b| b.text.contains("Drugi")).unwrap();
        assert_eq!(
            second.heading_path.as_deref(),
            Some("Wprowadzenie › Szczegóły"),
            "a localized H2 style must nest under the H1"
        );
        assert!(
            !blocks
                .iter()
                .any(|b| b.text == "Wprowadzenie" || b.text == "Szczegóły"),
            "localized heading paragraphs must not leak as content blocks"
        );
    }

    /// `w:br`/`w:cr` render a newline and `w:tab` a tab — without them, visually separate lines
    /// glue into one word.
    #[test]
    fn docx_break_tab_cr_render_whitespace() {
        const XML: &str = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>line1</w:t><w:br/><w:t>line2</w:t><w:tab/><w:t>col</w:t><w:cr/><w:t>line3</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        let p = build_ooxml("docx", &[("word/document.xml", XML)]);
        let blocks = extract_docx(&p).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].text, "line1\nline2\tcol\nline3",
            "w:br and w:cr must render a newline, w:tab a tab"
        );
    }

    /// Multiple paragraphs in ONE table cell join with a newline instead of gluing together.
    #[test]
    fn docx_multi_paragraph_cell_joins_with_newline() {
        const XML: &str = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>para one</w:t></w:r></w:p><w:p><w:r><w:t>para two</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;
        let p = build_ooxml("docx", &[("word/document.xml", XML)]);
        let blocks = extract_docx(&p).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].text, "para one\npara two | x",
            "cell paragraphs must join with a newline, not concatenate"
        );
    }

    /// Field-instruction text (`w:instrText` — TOC/PAGEREF plumbing) is NOT document content and
    /// must not leak into the extraction.
    #[test]
    fn docx_field_instruction_text_is_not_extracted() {
        const XML: &str = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:fldChar w:fldCharType="begin"/></w:r>
      <w:r><w:instrText> PAGEREF _Toc12345 \h </w:instrText></w:r>
      <w:r><w:fldChar w:fldCharType="end"/></w:r>
      <w:r><w:t>Chapter one</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;
        let p = build_ooxml("docx", &[("word/document.xml", XML)]);
        let blocks = extract_docx(&p).unwrap();
        let all = blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !all.contains("PAGEREF"),
            "field-instruction junk must be suppressed; got: {all:?}"
        );
        assert!(all.contains("Chapter one"), "real run text must survive; got: {all:?}");
    }

    /// Tracked-changes DELETED text (`w:delText`) must not be resurrected into the brain — the
    /// author removed it (a privacy surprise if it comes back). The deleted run sits BEFORE the
    /// first `w:t` — the arm-at-paragraph-start capture window that leaked it.
    #[test]
    fn docx_tracked_change_deleted_text_is_not_extracted() {
        const XML: &str = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:del w:id="1"><w:r><w:delText>REDACTED SECRET</w:delText></w:r></w:del>
      <w:r><w:t>kept after deletion</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;
        let p = build_ooxml("docx", &[("word/document.xml", XML)]);
        let blocks = extract_docx(&p).unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(
            !blocks[0].text.contains("REDACTED"),
            "deleted (tracked-changes) text must be suppressed; got: {:?}",
            blocks[0].text
        );
        assert_eq!(blocks[0].text, "kept after deletion");
    }

    /// Deleted-tracked-changes FIELD-instruction text (`w:delInstrText` — the deleted form of the
    /// PAGEREF/TOC plumbing) is field junk AND author-removed content; it must never leak. Same
    /// class as the `w:instrText` / `w:delText` suppression.
    #[test]
    fn docx_deleted_field_instruction_text_is_not_extracted() {
        const XML: &str = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:delInstrText>PAGEREF _Toc99</w:delInstrText></w:r><w:r><w:t>REAL</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        let p = build_ooxml("docx", &[("word/document.xml", XML)]);
        let blocks = extract_docx(&p).unwrap();
        assert!(!blocks.iter().any(|b| b.text.contains("PAGEREF")),
            "deleted field-instruction text (w:delInstrText) must not leak: {blocks:?}");
    }

    /// A layout break (`w:br`) inside a DELETED run (`w:del`) must emit NOTHING — suppressing only
    /// `w:delText` left the break to inject a stray newline between the surrounding kept runs.
    #[test]
    fn docx_break_inside_suppressed_run_emits_no_whitespace() {
        const XML: &str = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>A</w:t></w:r><w:del><w:r><w:br/><w:delText>GONE</w:delText></w:r></w:del><w:r><w:t>B</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        let p = build_ooxml("docx", &[("word/document.xml", XML)]);
        let blocks = extract_docx(&p).unwrap();
        assert_eq!(blocks[0].text, "AB", "a break inside a suppressed run must emit nothing (got {:?})", blocks[0].text);
    }

    /// A PPTX slide table (`p:graphicFrame` → `a:tbl`) lives OUTSIDE any `p:sp` shape — its cells
    /// must still extract, as pipe-delimited rows like the DOCX table idiom.
    #[test]
    fn pptx_graphic_frame_table_extracts_pipe_rows() {
        const SLIDE: &str = r#"<?xml version="1.0"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld><p:spTree>
    <p:graphicFrame>
      <a:graphic><a:graphicData>
        <a:tbl>
          <a:tr>
            <a:tc><a:txBody><a:p><a:r><a:t>Milestone</a:t></a:r></a:p></a:txBody></a:tc>
            <a:tc><a:txBody><a:p><a:r><a:t>Date</a:t></a:r></a:p></a:txBody></a:tc>
          </a:tr>
          <a:tr>
            <a:tc><a:txBody><a:p><a:r><a:t>Beta</a:t></a:r></a:p></a:txBody></a:tc>
            <a:tc><a:txBody><a:p><a:r><a:t>March</a:t></a:r></a:p></a:txBody></a:tc>
          </a:tr>
        </a:tbl>
      </a:graphicData></a:graphic>
    </p:graphicFrame>
  </p:spTree></p:cSld>
</p:sld>"#;
        let p = build_ooxml("pptx", &[("ppt/slides/slide1.xml", SLIDE)]);
        let blocks = extract_pptx(&p).unwrap();
        assert_eq!(
            blocks.len(),
            1,
            "a table-only slide must still yield a block; got {blocks:?}"
        );
        assert!(
            blocks[0].text.contains("Milestone | Date"),
            "table rows must render pipe-delimited; got: {:?}",
            blocks[0].text
        );
        assert!(blocks[0].text.contains("Beta | March"));
        assert_eq!(blocks[0].page, Some(1));
    }
}
