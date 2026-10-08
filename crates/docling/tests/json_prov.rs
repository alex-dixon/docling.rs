//! #609: every text item of a layout-pipeline PDF carries its own `prov` in
//! the JSON, so a chunk made of it still knows its page and box. On the
//! reporter's two-page sample (`tests/fixtures/prov/`, a ReportLab document
//! with a bar chart, a table and a checkbox list) docling 2.135 gives every
//! text a `prov`; so must we. The chart's y-axis ticks `3`…`8`, each set by
//! its own `cm` under one shared `Tm`, are six picture children with their
//! own boxes there — the text layer used to glue them into one `345678`
//! cell carrying the first tick's box.
//!
//! Needs the layout model only (`--skip-ocr`), so it skips on a checkout
//! without `.models/`.

use std::path::{Path, PathBuf};

use docling::{DocumentConverter, SourceDocument};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn layout_ready() -> bool {
    let m = repo_root().join(".models");
    let ready = ["layout_heron_int8.onnx", "layout_heron.onnx"]
        .iter()
        .any(|f| m.join(f).exists());
    ready && std::env::set_current_dir(repo_root()).is_ok()
}

#[test]
fn every_text_item_has_prov_and_axis_ticks_keep_their_boxes() {
    if !layout_ready() {
        eprintln!("skipping: the layout model is not present");
        return;
    }
    let path = repo_root().join("crates/docling/tests/fixtures/prov/issue609_sample.pdf");
    let doc = DocumentConverter::new()
        .skip_ocr(true)
        .convert(SourceDocument::from_file(&path).expect("fixture"))
        .expect("conversion")
        .document;
    let json: serde_json::Value = serde_json::from_str(&doc.export_to_json()).unwrap();
    let texts = json["texts"].as_array().unwrap();

    let bare: Vec<&str> = texts
        .iter()
        .filter(|t| t["prov"].as_array().is_none_or(|p| p.is_empty()))
        .map(|t| t["text"].as_str().unwrap_or(""))
        .collect();
    assert!(bare.is_empty(), "text items without prov: {bare:?}");

    // The ticks hang under the chart picture, bottom (`3`) to top (`8`):
    // one item each, its box on its own baseline (y-up, so `t` rises).
    let ticks: Vec<(String, f64)> = json["pictures"][0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let i: usize = c["$ref"]
                .as_str()
                .unwrap()
                .trim_start_matches("#/texts/")
                .parse()
                .unwrap();
            let t = &texts[i];
            let top = t["prov"][0]["bbox"]["t"].as_f64().unwrap();
            (t["text"].as_str().unwrap().to_string(), top)
        })
        .filter(|(s, _)| s.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let labels: Vec<&str> = ticks.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(labels, ["3", "4", "5", "6", "7", "8"], "{ticks:?}");
    assert!(
        ticks.windows(2).all(|w| w[1].1 - w[0].1 > 20.0),
        "each tick sits ~26 pt above the last: {ticks:?}"
    );
}
