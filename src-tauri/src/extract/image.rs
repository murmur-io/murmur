//! Direct image import — OCR a standalone image file (png / jpg / jpeg / heic / tiff / bmp / gif) into
//! ONE [`ExtractedBlock`] via Apple Vision (`super::ocr`). macOS-only. No new binary; the file is
//! decoded to a `CGImage` (via `NSImage`, ImageIO under the hood) and OCR'd through the SAME proven
//! `ocr_cgimage` core the scanned-PDF page uses (`ocr_image_file`) — the `initWithData` Vision handler
//! returns zero results on a valid PNG on this Mac, the CGImage handler reads the same pixels.
//!
//! CRASH-SAFE FFI (rules §7): the whole decode + OCR path is wrapped in `objc2::exception::catch`
//! inside `super::ocr`; a corrupt / non-image / undecodable / missing file fails CLOSED to
//! `AppError::InvalidArg`, never an abort. An image with NO recognizable text is a clear `InvalidArg`
//! ("no text found in this image").
//!
//! LOCK MODEL: extraction is a pure `path → Vec<ExtractedBlock>` transform, no DB / keychain touch. The
//! recognized text rides the SAME `documents.text` seal/gate the md/txt/pdf path already uses — no new
//! seal path, no new read gate. NO PII is logged (the caller logs counts/stages only; never the text).
//!
//! REAL-MAC CAVEAT: OCR fidelity only TRULY verifies on a signed build on a real Mac; the headless test
//! here only asserts the fail-closed contract for a non-image file (no abort, `InvalidArg`).

use std::path::Path;

use super::ExtractedBlock;
use crate::error::{AppError, Result};

/// The image extensions direct import accepts. Kept in sync with the `commands::DOC_ALLOWED_EXTS`
/// additions; dispatch routes each of these to [`extract_image`].
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "heic", "tiff", "tif", "bmp", "gif"];

/// Extract an image file into ONE block (page `Some(1)`, no heading) via on-device OCR. Decodes the
/// file to a `CGImage` and runs the PROVEN `ocr_cgimage` core (the same path the scanned-PDF page
/// uses — Vision's `initWithData` handler returns zero results on a valid PNG on this Mac). Fails
/// CLOSED with `AppError::InvalidArg` for an unreadable/missing file, an undecodable/corrupt image, or
/// an image with no recognizable text — never a panic / abort.
pub fn extract_image(path: &Path) -> Result<Vec<ExtractedBlock>> {
    // Decode → up-scale → OCR ENTIRELY inside `objc2::exception::catch` (NSImage/CoreGraphics/Vision),
    // returning `None` on any exception / missing file / undecodable data / empty result — so this is
    // the fail-closed boundary. No PII logged.
    match super::ocr::ocr_image_file(path) {
        Some(text) if !text.trim().is_empty() => Ok(vec![ExtractedBlock {
            text: text.trim().to_string(),
            page: Some(1),
            heading_path: None,
        }]),
        _ => Err(AppError::InvalidArg("no text found in this image".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(ext: &str, bytes: &[u8]) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-ocr-image-{}-{}.{ext}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// IMAGE_EXTS is the exact set the dispatch + allowlist accept (regression guard).
    #[test]
    fn image_exts_are_the_expected_set() {
        assert_eq!(
            IMAGE_EXTS,
            &["png", "jpg", "jpeg", "heic", "tiff", "tif", "bmp", "gif"]
        );
    }

    /// A non-image file (plain text bytes with an image extension) fails CLOSED with `InvalidArg` and
    /// does NOT abort — proves the Vision FFI (`initWithData` on undecodable bytes) is caught, not
    /// crashed. (Real OCR of a REAL image needs a signed build on a real Mac.)
    #[test]
    fn non_image_file_fails_closed_without_abort() {
        let p = write_tmp("png", b"this is definitely not a PNG, just ascii text");
        let err = extract_image(&p).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)), "got {err:?}");
    }

    /// A missing file fails closed (no panic).
    #[test]
    fn missing_image_fails_closed() {
        let p = std::path::Path::new("/nonexistent/murmur/does-not-exist.png");
        let err = extract_image(p).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)), "got {err:?}");
    }
}
