//! On-device OCR via Apple **Vision** (`objc2-vision`) — macOS-only. Turns a rendered PDF page (or a
//! decoded image file) into recognized text WITHOUT any cloud egress and WITHOUT a new binary/dylib
//! (same objc2 0.3 family we already ship). Used by the scanned-PDF fallback in [`super::pdf`] and by
//! direct image import in [`super::image`].
//!
//! ONE OCR CORE — always via a `CGImage`. Both the scanned-PDF page ([`ocr_pdf_page`]) and a standalone
//! image file ([`ocr_image_file`]) decode/render to a `CGImage` and run [`ocr_cgimage`], which uses
//! `VNImageRequestHandler::initWithCGImage:options:`. We deliberately do NOT use the `initWithData`
//! handler: on this Mac it returns ZERO observations for a valid PNG while the CGImage handler reads
//! the identical pixels correctly. Both paths share the up-scale-to-~2000px logic ([`ocr_render_pixels`]
//! + [`with_rgbx_bitmap`]) so small / low-DPI inputs are scaled up for recognition fidelity.
//!
//! CRASH-SAFE FFI (rules §7, the `screenshare.rs` war story): a malformed image, an unusual PDF page,
//! or an unexpected Vision/CoreGraphics state can raise an Objective-C `NSException`, which — if it
//! unwinds across the FFI boundary — ABORTS the process ("Rust cannot catch foreign exceptions"). So
//! EVERY Vision/CoreGraphics/PDFKit call here runs inside `objc2::exception::catch`; ANY caught
//! exception (or a nil handle / empty result) fails CLOSED to `None` — never a panic, never an abort.
//! The caller maps `None` to a fail-closed `AppError::InvalidArg`.
//!
//! NO PII IN LOGS: this module logs nothing but (in the caller) counts/stages — never a recognized
//! string. The recognized text flows straight into the EXISTING `documents.text` seal/gate; there is
//! NO new seal path and NO new read gate.
//!
//! REAL-MAC CAVEAT: this compiles everywhere it's gated to, but OCR FIDELITY (does Vision actually
//! read this scanned page? Polish diacritics? layout order?) only TRULY verifies on a real Mac with a
//! signed build. `cargo test` cannot exercise the Vision FFI path — the headless tests only assert the
//! fail-closed contract (a corrupt/non-image input returns `None`, never aborts).

use std::panic::AssertUnwindSafe;
use std::path::Path;

use objc2::exception::catch;
use objc2::rc::Retained;
use objc2::{AllocAnyThread, ClassType};
use objc2_app_kit::NSImage;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage,
    CGImageAlphaInfo, CGInterpolationQuality,
};
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2_pdf_kit::{PDFDisplayBox, PDFPage};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedTextObservation, VNRequest,
    VNRequestTextRecognitionLevel,
};

/// The longest edge (in pixels) we render a PDF page to before OCR. ~2000px is a good accuracy/RAM
/// tradeoff (Vision reads small type well at this DPI) and CAPS memory: a bitmap is at most
/// `MAX_OCR_LONG_EDGE² * 4` bytes ≈ 16 MiB, so a pathologically large page box can never allocate
/// unbounded RAM in the render step.
const MAX_OCR_LONG_EDGE: f64 = 2000.0;

/// Run an ObjC closure that borrows non-`UnwindSafe` Vision/CG/PDFKit handles inside
/// `objc2::exception::catch`. These are pure reads/renders with no interior-mutability hazard left
/// half-mutated across a caught exception, so `AssertUnwindSafe` is sound — and REQUIRED because
/// `Retained<_>`/`&CGImage` are not `UnwindSafe`. Returns `None` on a caught ObjC exception.
fn catch_objc<R>(f: impl FnOnce() -> R) -> Option<R> {
    catch(AssertUnwindSafe(f)).ok()
}

/// Murmur's PREFERRED OCR languages, in priority order: Polish first, English second (Murmur's two
/// primary languages — the multilingual whisper ships Polish too). Intersected at runtime with the
/// system's actually-supported set (see [`choose_ocr_languages`]) so an older Vision that lacks "pl"
/// on the app's 13.4 floor never fails the whole import — it degrades to the supported subset.
const PREFERRED_OCR_LANGUAGES: &[&str] = &["pl", "en"];

/// Choose the recognition languages to request, intersecting our [`PREFERRED_OCR_LANGUAGES`] (in
/// PRIORITY order) with the `supported` set Vision reports at runtime. Pure logic (no FFI) so it is
/// headless-testable — the FFI wrapper [`supported_recognition_languages`] feeds it the live set.
///
/// - Keep every preferred language the system supports, in our priority order (pl before en).
/// - If NONE of our preferred languages are supported (a very old Vision, or an empty/failed probe),
///   return EMPTY — the caller then leaves Vision's OWN default language selection in place rather
///   than forcing an unsupported language (forcing "pl" on a Vision that doesn't support it is the
///   "no text found" failure this fix closes). Fail-open to the system default, never fail-closed.
fn choose_ocr_languages(supported: &[String], preferred: &[&str]) -> Vec<String> {
    preferred
        .iter()
        .filter(|p| supported.iter().any(|s| s.eq_ignore_ascii_case(p)))
        .map(|p| p.to_string())
        .collect()
}

/// The languages Vision reports as supported for the request's current configuration
/// (`supportedRecognitionLanguagesAndReturnError:`). Crash-safe: the ObjC call is inside `catch`; a
/// caught exception / error / nil yields an EMPTY vec (the caller then keeps Vision's default — see
/// [`choose_ocr_languages`]). Call AFTER `setRecognitionLevel:` (the supported set is level-dependent).
fn supported_recognition_languages(req: &VNRecognizeTextRequest) -> Vec<String> {
    catch_objc(|| {
        // SAFETY: reads the supported-language collection for the request's current state; wrapped in
        // `catch`. On an ObjC error (`Err`) or nil, fall through to an empty vec.
        match unsafe { req.supportedRecognitionLanguagesAndReturnError() } {
            Ok(arr) => arr.iter().map(|s| s.to_string()).collect(),
            Err(_) => Vec::new(),
        }
    })
    .unwrap_or_default()
}

/// Build a configured Accurate/language-correcting `VNRecognizeTextRequest`. Inside `catch`
/// because the alloc/init + property setters are ObjC that could (in principle) throw. `None` on any
/// caught exception.
///
/// LANGUAGE SELECTION (Brain v3 PR-4, finding: hardcoded "pl"/"en" can fail on an older Vision):
/// after setting the recognition level, we INTERSECT [`PREFERRED_OCR_LANGUAGES`] with the languages
/// Vision reports as supported at runtime and request only that subset (in priority order). If the
/// intersection is empty (a very old Vision without "pl"/"en", or a failed probe) we DON'T force a
/// language at all — Vision's own default selection stays, so the import still attempts recognition
/// rather than failing "no text found". This is FFI — it only TRULY verifies on a real Mac; the pure
/// intersection ([`choose_ocr_languages`]) is unit-tested headless.
fn make_text_request() -> Option<Retained<VNRecognizeTextRequest>> {
    catch_objc(|| {
        // SAFETY: standard `[[VNRecognizeTextRequest alloc] init]`; the whole closure is inside
        // `catch` so any ObjC exception is contained.
        let req = unsafe { VNRecognizeTextRequest::init(VNRecognizeTextRequest::alloc()) };
        req.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        // Intersect our preferred languages with what THIS system's Vision actually supports (queried
        // after the level is set — the supported set is level-dependent). Only set them when the
        // intersection is non-empty; otherwise leave Vision's default selection (fail-open).
        let supported = supported_recognition_languages(&req);
        let chosen = choose_ocr_languages(&supported, PREFERRED_OCR_LANGUAGES);
        if !chosen.is_empty() {
            let ns_langs: Vec<Retained<NSString>> =
                chosen.iter().map(|l| NSString::from_str(l)).collect();
            let refs: Vec<&NSString> = ns_langs.iter().map(|r| &**r).collect();
            let langs = NSArray::from_slice(&refs);
            req.setRecognitionLanguages(&langs);
        }
        req.setUsesLanguageCorrection(true);
        req
    })
}

/// Run a configured text-recognition `request` against `handler` and collect the recognized text:
/// for each `VNRecognizedTextObservation` take `topCandidates(1)`'s first string, joined by newlines
/// in Vision's (roughly top-to-bottom) result order. Returns `None` on a caught exception or when no
/// observation yielded a non-empty candidate. All ObjC inside `catch`.
fn run_and_collect(
    handler: &VNImageRequestHandler,
    request: &VNRecognizeTextRequest,
) -> Option<String> {
    catch_objc(|| {
        // `VNRecognizeTextRequest` is a `VNRequest` subclass; up-cast for the requests array.
        let as_req: &VNRequest = request.as_super().as_super();
        let requests = NSArray::from_slice(&[as_req]);
        // `performRequests` blocks until done; on a scheduling error we get Err → fail closed.
        if handler.performRequests_error(&requests).is_err() {
            return None;
        }
        let results = request.results()?;
        let mut lines: Vec<String> = Vec::new();
        for obs in results.iter() {
            let line = top_candidate_string(&obs);
            if let Some(s) = line {
                let t = s.trim();
                if !t.is_empty() {
                    lines.push(t.to_string());
                }
            }
        }
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    })
    .flatten()
}

/// The top-candidate string for one observation (`topCandidates(1)` → first `VNRecognizedText`'s
/// `string`). `None` when the observation has no candidate.
fn top_candidate_string(obs: &VNRecognizedTextObservation) -> Option<String> {
    let candidates = obs.topCandidates(1);
    let first = candidates.iter().next()?;
    Some(first.string().to_string())
}

/// OCR a `CGImage` → recognized text (newline-joined observations). Crash-safe: any FFI exception /
/// nil handle / empty result yields `None`. This is the core the PDF-page path calls after rendering
/// a page to a bitmap image.
pub fn ocr_cgimage(image: &CGImage) -> Option<String> {
    let request = make_text_request()?;
    // Build the image handler for this CGImage with an empty options dict. `initWithCGImage:options:`
    // is `unsafe` (the options generic must be correct — an empty `NSDictionary<NSString, AnyObject>`
    // is trivially correct) and could throw for a degenerate image → wrapped in `catch`.
    let handler: Retained<VNImageRequestHandler> = catch_objc(|| {
        let options: Retained<NSDictionary<NSString, _>> = NSDictionary::new();
        // SAFETY: `image` is a valid CGImage; the options dict is empty and correctly typed; the whole
        // closure is inside `catch`.
        unsafe {
            VNImageRequestHandler::initWithCGImage_options(
                VNImageRequestHandler::alloc(),
                image,
                &options,
            )
        }
    })?;
    run_and_collect(&handler, &request)
}

/// OCR a standalone image FILE → recognized text. Decodes the file to a `CGImage` (via `NSImage`),
/// up-scales it to ~[`MAX_OCR_LONG_EDGE`] on the long edge for OCR fidelity (the SAME scaling the
/// scanned-PDF page path uses — see [`ocr_render_pixels`]), then runs the PROVEN [`ocr_cgimage`] core.
///
/// WHY NOT `initWithData`: `VNImageRequestHandler::initWithData:options:` returns ZERO observations on
/// a valid PNG on this machine, while `initWithCGImage:options:` reads the identical pixels correctly.
/// So the image path is routed through the same CGImage handler the (working) PDF-page path uses.
///
/// Crash-safe: the decode + up-scale + recognize all run inside `objc2::exception::catch` (fail-closed
/// to `None`) — a corrupt / undecodable / non-image file yields `None`, never a panic / abort. `None`
/// on a missing/unreadable path, an undecodable image, or an image with no recognizable text.
pub fn ocr_image_file(path: &Path) -> Option<String> {
    let decoded = cgimage_from_file(path)?;
    // Up-scale for OCR fidelity (small / low-DPI photos). Fall back to the decoded image if the
    // up-scale render can't allocate (still OCR the native-resolution pixels rather than fail). The
    // two owned handles differ (`CFRetained` from the CG bitmap snapshot vs `Retained` from NSImage),
    // so branch and pass a `&CGImage` from each arm rather than unify the owned type.
    match upscale_cgimage_for_ocr(&decoded) {
        Some(scaled) => ocr_cgimage(&scaled),
        None => ocr_cgimage(&decoded),
    }
}

/// Decode an image FILE to a `CGImage` via `NSImage` (ImageIO under the hood — png / jpg / jpeg / heic
/// / tiff / bmp / gif). `NSImage::initWithContentsOfFile` returns `nil` (→ `None`) for a missing /
/// undecodable / non-image file; `CGImageForProposedRect:context:hints:` with a NULL proposed rect
/// and no reference context/hints yields the image's natural-size CGImage. Every ObjC/CG call inside
/// `catch` → fail-closed `None`, never an FFI abort. Returns an ObjC-`Retained<CGImage>`.
fn cgimage_from_file(path: &Path) -> Option<Retained<CGImage>> {
    let path_str = path.to_str()?;
    catch_objc(|| {
        let ns_path = NSString::from_str(path_str);
        let ns_image = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns_path)?;
        // SAFETY: a NULL `proposed_dest_rect` (allowed — means "natural size"), no reference context,
        // no hints; the whole closure is inside `catch`, so any ObjC exception is contained. Returns
        // `nil` (→ None) if the image has no CGImage representation.
        unsafe {
            ns_image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None)
        }
    })
    .flatten()
}

/// Up-scale a decoded `CGImage` to ~[`MAX_OCR_LONG_EDGE`] on the long edge for OCR fidelity, reusing
/// the SAME pixel-target logic ([`ocr_render_pixels`]) as the scanned-PDF page renderer. Draws the
/// source image into a white RGBX bitmap at the target size (high interpolation) and snapshots a new
/// CGImage. Returns `None` if the source has no usable dimensions or the bitmap can't allocate — the
/// caller then falls back to OCR-ing the native-resolution image. All CG calls inside `catch`.
fn upscale_cgimage_for_ocr(src: &CGImage) -> Option<CFRetained<CGImage>> {
    let sw = CGImage::width(Some(src));
    let sh = CGImage::height(Some(src));
    if sw == 0 || sh == 0 {
        return None;
    }
    let (px_w, px_h) = ocr_render_pixels(sw as f64, sh as f64);
    if px_w == 0 || px_h == 0 {
        return None;
    }
    render_cgimage_into_rgbx(src, px_w, px_h)
}

/// Draw `src` into a fresh white RGBX bitmap of `px_w × px_h` (CG owns the backing store) and snapshot
/// an immutable `CGImage`. Shared bitmap-render primitive: the scanned-PDF page path and the image
/// up-scale path both funnel their draw through this (white bg → high-quality interpolation → snapshot)
/// via [`with_rgbx_bitmap`]. `None` on any allocation failure / caught exception.
fn render_cgimage_into_rgbx(src: &CGImage, px_w: usize, px_h: usize) -> Option<CFRetained<CGImage>> {
    with_rgbx_bitmap(px_w, px_h, |ctx| {
        let full = CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(px_w as f64, px_h as f64),
        );
        // High interpolation so an up-scaled small image stays legible to Vision.
        CGContext::set_interpolation_quality(Some(ctx), CGInterpolationQuality::High);
        CGContext::draw_image(Some(ctx), full, Some(src));
    })
}

/// Render a `PDFPage` into a `CGImage` at ~[`MAX_OCR_LONG_EDGE`] on the long edge (scaled up from the
/// page's media box for OCR fidelity, but capped so a huge page can't allocate unbounded RAM), then
/// OCR it. Draws into a self-allocated RGBX `CGBitmapContext` (CG owns the backing store — `data:
/// null`), fills white first (a transparent bitmap OCRs poorly), then `drawWithBox:toContext:`.
/// Crash-safe: every PDFKit/CoreGraphics call is inside `catch`; any failure yields `None`.
pub fn ocr_pdf_page(page: &PDFPage) -> Option<String> {
    let image = render_page_to_cgimage(page)?;
    ocr_cgimage(&image)
}

/// Render a `PDFPage` to a `CGImage`. `None` on any FFI exception / zero-area page / allocation
/// failure. Kept separate so the size/clamp logic is unit-visible; the render itself needs a real Mac.
/// CG functions return `CFRetained<_>` (Core Foundation types, not ObjC objects).
fn render_page_to_cgimage(page: &PDFPage) -> Option<CFRetained<CGImage>> {
    // Page box (media box) in PDF points. Inside `catch` — `boundsForBox` is `unsafe` ObjC.
    let bounds = catch_objc(|| unsafe { page.boundsForBox(PDFDisplayBox::MediaBox) })?;
    let pw = bounds.size.width;
    let ph = bounds.size.height;
    // Guard degenerate / non-finite boxes.
    if !(pw.is_finite() && ph.is_finite()) || pw <= 1.0 || ph <= 1.0 {
        return None;
    }
    let (px_w, px_h) = ocr_render_pixels(pw, ph);
    if px_w == 0 || px_h == 0 {
        return None;
    }
    let scale = px_w as f64 / pw; // uniform scale (px_w/pw == px_h/ph by construction)

    with_rgbx_bitmap(px_w, px_h, |ctx| {
        // Scale the CTM so the page's point-space fills the pixel bitmap, then draw the media box.
        CGContext::scale_ctm(Some(ctx), scale, scale);
        // SAFETY: valid page + context, both live for the call; inside `catch`.
        unsafe { page.drawWithBox_toContext(PDFDisplayBox::MediaBox, ctx) };
    })
}

/// Create a white-filled RGBX `CGBitmapContext` of `px_w × px_h` (CG owns the backing store, `data:
/// null`), run `draw` to render into it, then snapshot an immutable `CGImage`. The ONE bitmap-render
/// primitive shared by the scanned-PDF page renderer and the image up-scaler: a white background
/// (scanned pages / photos OCR best on white — a transparent bitmap reads poorly) is filled BEFORE
/// `draw`. Every CG call is inside `catch`; `None` on any allocation failure / caught exception.
fn with_rgbx_bitmap(
    px_w: usize,
    px_h: usize,
    draw: impl FnOnce(&CGContext),
) -> Option<CFRetained<CGImage>> {
    catch_objc(|| {
        // Device RGB colorspace; 8 bits/component; RGBX (alpha skipped) so 4 bytes/pixel.
        let space = CGColorSpace::new_device_rgb()?;
        let bytes_per_row = px_w.checked_mul(4)?;
        let bitmap_info = CGImageAlphaInfo::NoneSkipLast.0;
        // SAFETY: `data = null` → CG allocates + owns the backing store; params are internally
        // consistent (RGBX 8bpc). Returns nil on bad params → mapped to None.
        let ctx: CFRetained<CGContext> = unsafe {
            CGBitmapContextCreate(
                std::ptr::null_mut(),
                px_w,
                px_h,
                8,
                bytes_per_row,
                Some(&space),
                bitmap_info,
            )
        }?;

        // White background first (scanned pages / photos read best on white).
        CGContext::set_rgb_fill_color(Some(&ctx), 1.0, 1.0, 1.0, 1.0);
        let full = CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(px_w as f64, px_h as f64),
        );
        CGContext::fill_rect(Some(&ctx), full);

        draw(&ctx);

        // Snapshot the drawn bitmap into an immutable CGImage.
        CGBitmapContextCreateImage(Some(&ctx))
    })
    .flatten()
}

/// Pixel dimensions for OCR-rendering a page whose media box is `pw × ph` points: scale the LONG edge
/// to exactly [`MAX_OCR_LONG_EDGE`] — a small page is scaled UP (better OCR fidelity for low-DPI type)
/// and an oversized page is scaled DOWN to the cap (the RAM guard: a bitmap is at most
/// `MAX_OCR_LONG_EDGE² * 4` bytes). Pure arithmetic (unit-testable without FFI).
fn ocr_render_pixels(pw: f64, ph: f64) -> (usize, usize) {
    // A degenerate box (either edge non-finite or <= 0) yields zero pixels — the renderer bails with
    // no allocation. Both edges must be positive-finite for a valid page.
    if !(pw.is_finite() && ph.is_finite()) || pw <= 0.0 || ph <= 0.0 {
        return (0, 0);
    }
    let long_edge = pw.max(ph);
    if long_edge <= 0.0 {
        return (0, 0);
    }
    // Scale the long edge to exactly the target (up for a small page, down for an oversized one — the
    // cap is a hard RAM ceiling, so downscaling a huge page wins).
    let scale = MAX_OCR_LONG_EDGE / long_edge;
    let w = (pw * scale).round();
    let h = (ph * scale).round();
    // Final hard ceiling on each edge (defense-in-depth against a non-finite/overflow scale).
    let clamp = |v: f64| -> usize {
        if !v.is_finite() || v <= 0.0 {
            0
        } else {
            (v as usize).min(MAX_OCR_LONG_EDGE as usize)
        }
    };
    (clamp(w), clamp(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ocr_render_pixels` scales a page's long edge to the target and caps each edge — pure logic,
    /// no FFI, so it runs headless. (Real render + OCR needs a signed build on a real Mac.)
    #[test]
    fn render_pixels_scales_long_edge_to_target_and_caps() {
        // A US-Letter page (612 × 792 pt) scales so the LONG edge (792) → 2000px.
        let (w, h) = ocr_render_pixels(612.0, 792.0);
        assert_eq!(h, 2000, "long edge scales to the target");
        // width scaled by the same factor (612 * 2000/792 ≈ 1545), within the cap.
        assert!((1540..=1550).contains(&w), "short edge scaled proportionally, got {w}");
        assert!(w <= MAX_OCR_LONG_EDGE as usize && h <= MAX_OCR_LONG_EDGE as usize);
    }

    /// A page already LARGER than the cap is scaled DOWN to the cap (the RAM guard wins).
    #[test]
    fn render_pixels_caps_an_oversized_page() {
        let (w, h) = ocr_render_pixels(10000.0, 5000.0);
        assert_eq!(w, 2000, "oversized long edge is clamped to the cap");
        assert!(h <= 2000);
        assert!(w * h * 4 <= (MAX_OCR_LONG_EDGE as usize).pow(2) * 4, "bitmap RAM is bounded");
    }

    /// A degenerate (zero / negative) box yields zero pixels (the renderer bails, no allocation).
    #[test]
    fn render_pixels_rejects_degenerate_box() {
        assert_eq!(ocr_render_pixels(0.0, 0.0), (0, 0));
        assert_eq!(ocr_render_pixels(-1.0, 100.0), (0, 0));
    }

    /// Fix 6 regression: a page SMALLER than the cap is scaled UP (not left at native size) — proves
    /// the removed `.max(1.0)`/`.min(...)` no-op pair never gated upscaling. A 100×100pt page → the
    /// long edge hits the target (2000px).
    #[test]
    fn render_pixels_scales_a_small_page_up_to_the_target() {
        let (w, h) = ocr_render_pixels(100.0, 100.0);
        assert_eq!((w, h), (2000, 2000), "a small page is scaled UP to the cap, not left native");
    }

    /// Fix 5 (pure logic, headless): the chosen OCR languages are our preferred set intersected with
    /// what the system supports, in PRIORITY order (pl before en).
    #[test]
    fn choose_ocr_languages_intersects_in_priority_order() {
        // Both supported → both requested, pl first.
        let both = vec!["en".to_string(), "fr".to_string(), "pl".to_string()];
        assert_eq!(choose_ocr_languages(&both, PREFERRED_OCR_LANGUAGES), vec!["pl", "en"]);
        // Only English supported (an older Vision without Polish) → request only "en", never force pl.
        let en_only = vec!["en-US".to_string(), "en".to_string(), "de".to_string()];
        assert_eq!(choose_ocr_languages(&en_only, PREFERRED_OCR_LANGUAGES), vec!["en"]);
        // Matching is case-insensitive (Vision may report "EN"/"PL").
        let upper = vec!["PL".to_string(), "EN".to_string()];
        assert_eq!(choose_ocr_languages(&upper, PREFERRED_OCR_LANGUAGES), vec!["pl", "en"]);
    }

    /// Fix 5: when NONE of our preferred languages are supported (or the probe returned nothing), the
    /// chosen set is EMPTY — the caller then leaves Vision's own default selection (fail-open, so the
    /// import still attempts OCR instead of forcing an unsupported language and failing "no text").
    #[test]
    fn choose_ocr_languages_empty_when_none_supported_falls_open() {
        let none = vec!["ja".to_string(), "zh-Hans".to_string()];
        assert!(choose_ocr_languages(&none, PREFERRED_OCR_LANGUAGES).is_empty());
        assert!(choose_ocr_languages(&[], PREFERRED_OCR_LANGUAGES).is_empty(), "empty probe → empty (system default)");
    }

    /// A missing image FILE fails CLOSED (None) — `NSImage::initWithContentsOfFile` returns nil for a
    /// nonexistent path, so the CGImage decode yields None BEFORE any Vision call.
    #[test]
    fn ocr_image_file_missing_path_is_none() {
        let p = std::path::Path::new("/nonexistent/murmur/does-not-exist-ocr.png");
        assert_eq!(ocr_image_file(p), None);
    }

    /// A non-image FILE (random garbage bytes on disk) fails CLOSED (None) without aborting — this
    /// DOES touch the NSImage/CGImage/Vision FFI (decode of undecodable bytes) and must be caught, not
    /// crash. On a real Mac `initWithContentsOfFile` returns nil for garbage; the `catch` wrapper turns
    /// any exception into None. The point is: no FFI abort.
    #[test]
    fn ocr_image_file_garbage_fails_closed_without_abort() {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-ocr-garbage-{}-{}.png",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&p, b"this is definitely not an image file, just plain ascii bytes")
            .expect("write tmp garbage file");
        // The ONLY assertion we can make headless is "it returns (no panic/abort)". The value is
        // None on a real Mac (undecodable) but we accept any return — the point is no FFI abort.
        let _ = ocr_image_file(&p);
        let _ = std::fs::remove_file(&p);
    }
}
