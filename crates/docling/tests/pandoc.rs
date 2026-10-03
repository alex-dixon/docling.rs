//! Pandoc AST output (#515) pinned two ways for a set of documents chosen
//! to cover the mapping — headings, nested and ordered lists, inline
//! formatting and links, code blocks, inline and display formulas,
//! checkboxes, tables with spans / header rows / rich cells, key-value forms
//! and definition lists:
//!
//! - `tests/data/pandoc/<name>.pandoc.json` — the exact `--to pandoc` output,
//!   always compared;
//! - `tests/data/pandoc/<name>.native` — what `pandoc -f json -t native`
//!   reads from it, the acceptance check that Pandoc itself takes the
//!   document as meant. Compared when a `pandoc` binary is on `PATH` (or
//!   named by `PANDOC`); skipped otherwise, so CI without Pandoc stays green.
//!
//! Regenerate after an intentional change (the natives need Pandoc 3.x):
//!
//! ```bash
//! DOCLING_RS_REGEN=1 cargo test -p docling --test pandoc
//! ```
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use docling::{DocumentConverter, SourceDocument};

/// `<format>/<file>` under the repository-root corpus.
const CASES: &[&str] = &[
    "md/nested.md",
    "md/inline_and_formatting.md",
    "md/blocks.md",
    "docx/unit_test_formatting.docx",
    "docx/equations.docx",
    "docx/docx_code_blocks.docx",
    "docx/word_tables.docx",
    "docx/docx_checkboxes.docx",
    "html/html_rich_table_cells.html",
    "html/table_03.html",
    "html/kvp_data_example.html",
    "html/html_description_list.html",
    "latex/example_01.tex",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn expected_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/pandoc")
}

fn pandoc_bin() -> Option<String> {
    let bin = std::env::var("PANDOC").unwrap_or_else(|_| "pandoc".to_string());
    let ok = Command::new(&bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(bin)
}

/// Pandoc's `native` reading of `json`, or its error.
fn native(bin: &str, json: &str) -> Result<String, String> {
    let mut child = Command::new(bin)
        .args(["-f", "json", "-t", "native"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(json.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

#[test]
fn pandoc_output_matches_the_stored_references() {
    let regen = std::env::var_os("DOCLING_RS_REGEN").is_some();
    let pandoc = pandoc_bin();
    if pandoc.is_none() {
        eprintln!("pandoc not found: comparing the JSON only, not Pandoc's reading of it");
    }
    let converter = DocumentConverter::new();
    let mut failures = Vec::new();
    for case in CASES {
        let (fmt, file) = case.split_once('/').expect("<format>/<file>");
        let src = root()
            .join("tests/data")
            .join(fmt)
            .join("sources")
            .join(file);
        let doc = converter
            .convert(SourceDocument::from_file(&src).expect("read source"))
            .unwrap_or_else(|e| panic!("{case}: {e}"))
            .document;
        let json = doc.export_to_pandoc_json();

        let json_path = expected_dir().join(format!("{file}.pandoc.json"));
        let native_path = expected_dir().join(format!("{file}.native"));
        if regen {
            fs::create_dir_all(expected_dir()).expect("mkdir");
            fs::write(&json_path, &json).expect("write json");
            if let Some(bin) = &pandoc {
                let n = native(bin, &json).unwrap_or_else(|e| panic!("{case}: pandoc: {e}"));
                fs::write(&native_path, n).expect("write native");
            }
            continue;
        }
        match fs::read_to_string(&json_path) {
            Ok(want) if want == json => {}
            Ok(_) => failures.push(format!(
                "{case}: Pandoc JSON changed (DOCLING_RS_REGEN=1 to update)"
            )),
            Err(_) => failures.push(format!("{case}: missing {}", json_path.display())),
        }
        if let Some(bin) = &pandoc {
            match native(bin, &json) {
                Ok(got) => {
                    let want = fs::read_to_string(&native_path).unwrap_or_default();
                    if got != want {
                        failures.push(format!("{case}: pandoc's native reading changed"));
                    }
                }
                Err(e) => failures.push(format!("{case}: pandoc rejected the output: {e}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn unsupported_api_version_is_an_error() {
    let src = root().join("tests/data/md/sources/nested.md");
    let doc = DocumentConverter::new()
        .convert(SourceDocument::from_file(&src).expect("read source"))
        .expect("convert")
        .document;
    let opts = docling::pandoc::PandocExportOptions {
        api_version: Some("1.22".into()),
        ..Default::default()
    };
    let err = doc.export_to_pandoc_json_with(&opts).unwrap_err();
    assert_eq!(
        err.to_string(),
        "unsupported Pandoc API version '1.22': only 1.23 (pandoc-types 1.23.1.1, Pandoc 3.x) is supported"
    );
    let ok = docling::pandoc::PandocExportOptions {
        api_version: Some("1.23.1".into()),
        ..Default::default()
    };
    assert!(doc.export_to_pandoc_json_with(&ok).is_ok());
}
