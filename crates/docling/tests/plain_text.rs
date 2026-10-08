//! Plain-text output (#613, docling's `--to text`) against docling-core's
//! own `DoclingDocument.export_to_text()`.
//!
//! `tests/data/text/<name>.txt` is **not** our output: it is what Python
//! docling-core (2.101) writes for the upstream groundtruth JSON of the same
//! fixture —
//!
//! ```python
//! DoclingDocument.load_from_json("tests/data/<format>/groundtruth/<name>.json").export_to_text()
//! ```
//!
//! — so the test pins the port to upstream's serializer, not to itself. The
//! cases cover what the plain serializer strips (heading markers, bold /
//! italic / strikethrough, link URLs, code fences and backticks, image
//! placeholders, GFM hard line breaks) and what it keeps (list bullets and
//! numbers, checkbox marks, table grids — rich cells included — and inline
//! formulas). Re-create a reference with the snippet above after an
//! upstream serializer change; never from our own output.
use std::fs;
use std::path::{Path, PathBuf};

use docling::{DocumentConverter, SourceDocument};

/// `<format>/<file>` under the repository-root corpus.
const CASES: &[&str] = &[
    "docx/unit_test_formatting.docx",
    "docx/docx_checkboxes.docx",
    "docx/docx_rich_cells.docx",
    "docx/docx_code_blocks.docx",
    "docx/docx_lists.docx",
    "docx/equations.docx",
    "html/hyperlink_02.html",
    "html/html_rich_table_cells.html",
    "html/html_heading_in_p.html",
    "html/table_03.html",
    "latex/example_01.tex",
    "odf/text_document_03.odt",
    "md/escaped_characters.md",
    "md/line_breaks.md",
    "jats/pmc2231364.nxml",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn text_matches_docling_core_export_to_text() {
    let converter = DocumentConverter::new();
    let mut failures = Vec::new();
    for case in CASES {
        let (format, name) = case.split_once('/').unwrap();
        let src = root()
            .join("tests/data")
            .join(format)
            .join("sources")
            .join(name);
        let source =
            SourceDocument::from_file(&src).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
        let document = converter
            .convert(source)
            .unwrap_or_else(|e| panic!("{case}: {e}"))
            .document;
        let expected_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/text")
            .join(format!("{name}.txt"));
        let expected = fs::read_to_string(&expected_path)
            .unwrap_or_else(|e| panic!("{}: {e}", expected_path.display()));
        let got = document.export_to_text();
        if got != expected {
            failures.push(format!(
                "{case}:\n--- docling-core\n{expected}\n--- ours\n{got}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
