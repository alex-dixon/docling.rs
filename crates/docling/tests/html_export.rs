//! `export_to_html` against the HTML groundtruth upstream docling ships for
//! its fixtures (#492): every `tests/data/<fmt>/groundtruth/<name>.html` in
//! the mirrored corpus is byte-for-byte what `DoclingDocument.export_to_html(
//! image_mode=EMBEDDED)` produced for `sources/<name>`, so the port is scored
//! the way the Markdown and JSON exports are.
use std::fs;
use std::path::{Path, PathBuf};

use docling::{DocumentConverter, ImageMode, SourceDocument};

/// `src="data:<mime>;base64,<payload>"` → `src="data:…"`: the payload and
/// its media type both come from the JSON export's picture bytes.
fn mask_data_uris(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find("\"data:") {
        let start = i + "\"data:".len();
        let end = rest[start..]
            .find('"')
            .map(|e| start + e)
            .unwrap_or(rest.len());
        out.push_str(&rest[..start]);
        out.push('…');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/data")
}

#[test]
fn html_matches_upstream_groundtruth() {
    let corpus = corpus();
    let mut cases: Vec<(PathBuf, PathBuf)> = Vec::new();
    for fmt in fs::read_dir(&corpus).expect("corpus") {
        let fmt = fmt.expect("entry").path();
        let gt_dir = fmt.join("groundtruth");
        let Ok(entries) = fs::read_dir(&gt_dir) else {
            continue;
        };
        for e in entries.flatten() {
            let gt = e.path();
            if gt.extension().and_then(|x| x.to_str()) != Some("html") {
                continue;
            }
            let stem = gt.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let src = fmt.join("sources").join(stem);
            if src.is_file() {
                cases.push((src, gt));
            }
        }
    }
    cases.sort();
    assert!(
        !cases.is_empty(),
        "no HTML groundtruth under {}",
        corpus.display()
    );

    let converter = DocumentConverter::new();
    let mut failures = Vec::new();
    for (src, gt) in &cases {
        let name = src.file_name().unwrap().to_string_lossy().into_owned();
        let source = SourceDocument::from_file(src).expect("read source");
        let doc = match converter.convert(source) {
            Ok(r) => r.document,
            Err(e) => {
                failures.push(format!("{name}: convert error: {e}"));
                continue;
            }
        };
        let want = fs::read_to_string(gt).expect("read groundtruth");
        let (got, _) = doc.export_to_html_with_images(ImageMode::Embedded, "artifacts");
        // Picture payloads are the JSON export's business (upstream's
        // `save_as_html` re-encodes every picture through PIL as PNG, so
        // neither its bytes nor its media type are the file's): compare the
        // markup with the `data:` URIs masked, the trailing newline aside.
        let (got, want) = (
            mask_data_uris(got.trim_end()),
            mask_data_uris(want.trim_end()),
        );
        if got != want {
            if let Some(dir) = std::env::var_os("DOCLING_RS_HTML_DUMP") {
                let dir = Path::new(&dir);
                fs::create_dir_all(dir).unwrap();
                fs::write(dir.join(format!("{name}.got.html")), &got).unwrap();
                fs::write(dir.join(format!("{name}.want.html")), &want).unwrap();
            }
            let first = got
                .lines()
                .zip(want.lines())
                .enumerate()
                .find(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| {
                    format!("line {}:\n  got:  {:.200}\n  want: {:.200}", i + 1, a, b)
                })
                .unwrap_or_else(|| {
                    format!("lengths differ: got {} want {}", got.len(), want.len())
                });
            failures.push(format!("{name}: {first}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} HTML groundtruth mismatch(es):\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
