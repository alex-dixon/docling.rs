//! #244: `skip_ocr` — layout + TableFormer without OCR (docling's independent
//! `do_ocr=False`) — and the missing-OCR-model degradation.
//!
//! A **separate integration-test file on purpose**: these tests point
//! `DOCLING_OCR_REC_ONNX` at a nonexistent path to prove the recognition
//! model is never loaded (or degrades when it can't be), and env vars are
//! process-global — `pages.rs` sets the same vars to *real* paths for its OCR
//! tests. Each tests/ file is its own binary and process, so the sabotage
//! can't race a sibling suite.

use std::path::{Path, PathBuf};

use docling::{DocumentConverter, SourceDocument};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Layout (+ TableFormer) models, with the OCR recognition model deliberately
/// pointed into the void — every test in this binary must work without it.
fn layout_ready_ocr_sabotaged() -> bool {
    let m = repo_root().join(".models");
    let layout = ["layout_heron_int8.onnx", "layout_heron.onnx"]
        .iter()
        .map(|f| m.join(f))
        .find(|p| p.exists());
    let Some(layout) = layout else {
        return false;
    };
    std::env::set_var("DOCLING_LAYOUT_ONNX", layout);
    std::env::set_var(
        "DOCLING_OCR_REC_ONNX",
        m.join("definitely-not-a-model.onnx"),
    );
    let tf = m.join("tableformer");
    let dec = ["decoder_kv.onnx", "decoder_int8.onnx", "decoder.onnx"]
        .iter()
        .map(|f| tf.join(f))
        .find(|p| p.exists());
    if let Some(dec) = dec {
        if tf.join("encoder.onnx").exists() && tf.join("bbox.onnx").exists() {
            std::env::set_var("DOCLING_TABLEFORMER_ENCODER", tf.join("encoder.onnx"));
            std::env::set_var("DOCLING_TABLEFORMER_DECODER", dec);
            std::env::set_var("DOCLING_TABLEFORMER_BBOX", tf.join("bbox.onnx"));
        }
    }
    true
}

/// A digital page with a real table: `no_ocr` — docling's `--no-ocr` since
/// 2.0 (#611), `skip_ocr` its pre-2.0 name — must keep the layout + table
/// structure that `text_layer_only` throws away, with the OCR model
/// unloadable, which also proves it never touches it. Both spellings give
/// the same document.
#[test]
fn skip_ocr_keeps_layout_and_tables() {
    if !layout_ready_ocr_sabotaged() {
        eprintln!("skipping: the layout model is not present");
        return;
    }
    let path = repo_root().join("tests/data/pdf/sources/2305.03393v1-pg9.pdf");
    let src = SourceDocument::from_file(&path).expect("table fixture");
    let doc = DocumentConverter::new()
        .skip_ocr(true)
        .convert(src)
        .expect("skip_ocr conversion succeeds")
        .document;
    let md = doc.export_to_markdown();
    // Structure survived: the fixture's table serializes as a Markdown grid
    // (the text-layer path yields flat paragraphs — no pipes).
    assert!(
        md.contains('|'),
        "expected a Markdown table from layout + TableFormer, got:\n{md}"
    );
    // ...and its digital text layer still reads out.
    assert!(!md.trim().is_empty());
    let no_ocr = DocumentConverter::new()
        .no_ocr(true)
        .convert(SourceDocument::from_file(&path).expect("table fixture"))
        .expect("no_ocr conversion succeeds")
        .document
        .export_to_markdown();
    assert_eq!(no_ocr, md, "no_ocr and skip_ocr are one switch");
}

/// A scanned page (no text layer) with `skip_ocr`: converts cleanly to a
/// document whose regions are empty of text — not an error.
#[test]
fn skip_ocr_scanned_page_converts_empty() {
    if !layout_ready_ocr_sabotaged() {
        eprintln!("skipping: the layout model is not present");
        return;
    }
    let path = repo_root().join("tests/data/scanned/sources/ocr_test_raster.pdf");
    let src = SourceDocument::from_file(&path).expect("scanned fixture");
    let doc = DocumentConverter::new()
        .skip_ocr(true)
        .convert(src)
        .expect("skip_ocr on a scanned page must not error")
        .document;
    // The page's only text exists as pixels; without OCR nothing reads out.
    assert!(doc.export_to_markdown().trim().is_empty());
}

/// The degradation half of #244: OCR *wanted* (no flag) but the model is
/// missing — the conversion warns and completes instead of erroring (the
/// pre-#244 behavior surfaced as a 422 through docling-serve).
#[test]
fn missing_ocr_model_degrades_instead_of_erroring() {
    if !layout_ready_ocr_sabotaged() {
        eprintln!("skipping: the layout model is not present");
        return;
    }
    let path = repo_root().join("tests/data/scanned/sources/ocr_test_raster.pdf");
    let src = SourceDocument::from_file(&path).expect("scanned fixture");
    let doc = DocumentConverter::new()
        .convert(src)
        .expect("a missing OCR model must degrade, not fail the conversion")
        .document;
    assert!(doc.export_to_markdown().trim().is_empty());
}
