//! XLSX extraction via `calamine` (pure-Rust). Each worksheet becomes one heading (the sheet name);
//! every non-empty row becomes one pipe-delimited [`ExtractedBlock`] under that heading. `page` is
//! `None` (a spreadsheet has no page axis). Deterministic + headless-testable.
//!
//! Lock model: pure `path → blocks`, no DB/keychain; failures map to `AppError::InvalidArg`. No PII.

use std::path::Path;

use calamine::{open_workbook, Data, Reader, Xlsx};

use super::ExtractedBlock;
use crate::error::Result;

/// Extract every sheet of an XLSX into blocks (one per non-empty row, heading = sheet name).
pub fn extract_xlsx(path: &Path) -> Result<Vec<ExtractedBlock>> {
    // DECOMPRESSION-BOMB guard (OOM-availability): calamine inflates the workbook internally, so we
    // pre-flight the zip once and reject if the TOTAL decompressed size exceeds the shared ceiling
    // (`MAX_EXTRACT_DECOMPRESSED_BYTES`). NOT a cap on the original file size — a legitimately large
    // spreadsheet passes; only a tiny archive that inflates to gigabytes is stopped.
    super::ooxml::guard_zip_not_a_bomb(path)?;

    let mut workbook: Xlsx<_> =
        open_workbook(path).map_err(|e| super::unreadable(format!("could not open XLSX: {e}")))?;

    let mut blocks: Vec<ExtractedBlock> = Vec::new();
    for sheet in workbook.sheet_names() {
        let range = match workbook.worksheet_range(&sheet) {
            Ok(r) => r,
            Err(_) => continue, // skip an unreadable sheet, never abort the whole doc
        };
        for row in range.rows() {
            // Render each cell via its `Display`; an empty cell is an empty string. Join with " | ".
            let cells: Vec<String> = row.iter().map(cell_text).collect();
            let line = cells.join(" | ");
            // A wholly-empty row (every cell blank → " |  | " with no content) is skipped.
            if line.chars().all(|c| c == '|' || c.is_whitespace()) {
                continue;
            }
            blocks.push(ExtractedBlock {
                text: line,
                page: None,
                heading_path: Some(sheet.clone()),
            });
        }
    }
    Ok(blocks)
}

/// Render one cell to text. Strings/numbers/bools go through `Data`'s own `Display` (empty → ""),
/// trimmed so a padded cell stays clean. Date/time cells are special-cased: their `Display` is the
/// RAW Excel serial float (calamine prints `ExcelDateTime`'s inner value, e.g. `45123`), which is
/// meaningless to the brain and the user — render the actual date/time via calamine's own epoch
/// math (`to_ymd_hms_milli`, faithful to both the 1900 and 1904 date systems) instead.
fn cell_text(cell: &Data) -> String {
    match cell {
        Data::DateTime(dt) if dt.is_datetime() => {
            let (y, mo, d, h, mi, s, _ms) = dt.to_ymd_hms_milli();
            if (h, mi, s) == (0, 0, 0) {
                format!("{y:04}-{mo:02}-{d:02}")
            } else {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
            }
        }
        // A duration-formatted cell ([hh]:mm:ss style): the serial is a DAY count → h:mm:ss.
        Data::DateTime(dt) => {
            let total = (dt.as_f64() * 86_400.0).round() as i64;
            let (sign, t) = if total < 0 {
                ("-", -total)
            } else {
                ("", total)
            };
            format!("{sign}{}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
        }
        other => other.to_string().trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The extractor itself no longer names `AppError` (failures go through `super::unreadable`),
    // but the tests still assert on the VARIANT.
    use crate::error::AppError;
    use std::io::Write;

    /// Build a minimal valid XLSX (one sheet, inline strings) in memory and write it to a temp file.
    /// calamine reads inline strings (`t="inlineStr"`), so no sharedStrings part is needed.
    fn build_xlsx(sheet_name: &str, rows: &[&[&str]]) -> std::path::PathBuf {
        let mut sheet_data = String::from(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
        );
        for (r, row) in rows.iter().enumerate() {
            sheet_data.push_str(&format!("<row r=\"{}\">", r + 1));
            for (c, val) in row.iter().enumerate() {
                let col = (b'A' + c as u8) as char;
                sheet_data.push_str(&format!(
                    "<c r=\"{col}{}\" t=\"inlineStr\"><is><t>{}</t></is></c>",
                    r + 1,
                    val
                ));
            }
            sheet_data.push_str("</row>");
        }
        sheet_data.push_str("</sheetData></worksheet>");
        build_xlsx_parts(sheet_name, &sheet_data, None)
    }

    /// The zip plumbing shared by [`build_xlsx`] and the styles-bearing date fixture: one sheet
    /// built from raw worksheet XML, plus an optional `xl/styles.xml` part (calamine reads styles
    /// at that fixed path — no relationship entry needed).
    fn build_xlsx_parts(
        sheet_name: &str,
        sheet_data: &str,
        styles: Option<&str>,
    ) -> std::path::PathBuf {
        let sheet_data = sheet_data.to_string();

        let workbook_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="{sheet_name}" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#
        );
        let workbook_rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#;
        let root_rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;

        let mut entries = vec![
            ("[Content_Types].xml", content_types.to_string()),
            ("_rels/.rels", root_rels.to_string()),
            ("xl/workbook.xml", workbook_xml),
            ("xl/_rels/workbook.xml.rels", workbook_rels.to_string()),
            ("xl/worksheets/sheet1.xml", sheet_data),
        ];
        if let Some(styles) = styles {
            entries.push(("xl/styles.xml", styles.to_string()));
        }

        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, content) in &entries {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(content.as_bytes()).unwrap();
            }
            zw.finish().unwrap();
        }
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-xlsx-{}-{}.xlsx",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&p, cursor.into_inner()).unwrap();
        p
    }

    /// XLSX: sheet name → heading, each row → pipe-delimited text, page None.
    #[test]
    fn xlsx_extracts_rows_as_pipe_text_under_the_sheet_heading() {
        let p = build_xlsx(
            "Budget",
            &[
                &["Item", "Cost", "Owner"],
                &["API", "10000", "Bob"],
                &["Design", "5000", "Anna"],
            ],
        );
        let blocks = extract_xlsx(&p).unwrap();
        assert_eq!(blocks.len(), 3, "one block per non-empty row");
        assert_eq!(blocks[0].text, "Item | Cost | Owner");
        assert_eq!(blocks[0].heading_path.as_deref(), Some("Budget"));
        assert_eq!(blocks[0].page, None);
        assert_eq!(blocks[1].text, "API | 10000 | Bob");
        assert_eq!(blocks[2].text, "Design | 5000 | Anna");
    }

    #[test]
    fn corrupt_xlsx_is_invalid_arg() {
        let mut p = std::env::temp_dir();
        p.push(format!("murmur-badxlsx-{}.xlsx", std::process::id()));
        std::fs::write(&p, b"definitely not a workbook").unwrap();
        let err = extract_xlsx(&p).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)), "got {err:?}");
    }

    /// Date-formatted cells (builtin numFmt 14) must render as DATES, not the raw Excel serial
    /// float — serial 45123 is 2023-07-16, and 45123.5 adds noon. The raw serial is meaningless to
    /// the brain (and to the user a search hit of `45123` is not a date).
    #[test]
    fn xlsx_date_cells_render_as_dates_not_raw_serials() {
        let sheet = r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
<row r="1">
  <c r="A1" t="inlineStr"><is><t>Kickoff</t></is></c>
  <c r="B1" s="1"><v>45123</v></c>
  <c r="C1" s="1"><v>45123.5</v></c>
</row>
</sheetData></worksheet>"#;
        let styles = r#"<?xml version="1.0" encoding="UTF-8"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs>
</styleSheet>"#;
        let p = build_xlsx_parts("Plan", sheet, Some(styles));
        let blocks = extract_xlsx(&p).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].text, "Kickoff | 2023-07-16 | 2023-07-16 12:00:00",
            "date cells must render as dates, not raw serials"
        );
    }
}
