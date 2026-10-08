//! Encrypted ("password protected") Office documents (#624): every format
//! fails with one clear error — never an empty document, never "bad zip".
//!
//! Fixtures (`tests/data/encrypted/`, password `1234`) were written by
//! Office 16.0 (build 20430) and attached to #624: `min_encrypted.*` with an
//! open password, `min_writepw.*` with a modify password only, and four
//! PowerPoint COM variants of one slide (`B` open password, `C` modify
//! password, `D` both, `H` a password added to a plain file saved in place
//! — a version 4 compound file).

use std::io::Write;
use std::path::{Path, PathBuf};

use docling::{DocumentConverter, InputFormat, SourceDocument};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/encrypted")
        .join(name)
}

fn convert(source: SourceDocument) -> Result<String, String> {
    DocumentConverter::new()
        .convert(source)
        .map(|r| r.document.export_to_markdown())
        .map_err(|e| e.to_string())
}

fn convert_file(name: &str) -> Result<String, String> {
    convert(SourceDocument::from_file(fixture(name)).unwrap())
}

#[test]
fn every_encrypted_office_format_says_so() {
    for (name, fmt) in [
        ("min_encrypted.doc", "doc"),
        ("min_encrypted.docx", "docx"),
        ("min_encrypted.xls", "xls"),
        ("min_encrypted.xlsx", "xlsx"),
        ("min_encrypted.ppt", "ppt"),
        ("min_encrypted.pptx", "pptx"),
        ("B_openpw.ppt", "ppt"),
        ("D_both.ppt", "ppt"),
        ("H_A_addpw_save.ppt", "ppt"),
    ] {
        let err = convert_file(name).expect_err(name);
        assert!(
            err.ends_with(&format!("{fmt}: document is encrypted")),
            "{name}: {err}"
        );
    }
}

/// PowerPoint encrypts a presentation that has only a modify password too
/// (with its built-in default password), so its slides are ciphertext like
/// an open-password file's — an error, not the empty document it used to be.
#[test]
fn a_modify_password_ppt_is_encrypted_too() {
    for name in ["min_writepw.ppt", "C_writepw.ppt"] {
        let err = convert_file(name).expect_err(name);
        assert!(err.ends_with("ppt: document is encrypted"), "{name}: {err}");
    }
}

/// Word and Excel leave a modify-password-only file unencrypted: it still
/// converts.
#[test]
fn modify_password_docx_and_xlsx_still_convert() {
    for name in ["min_writepw.docx", "min_writepw.xlsx"] {
        let md = convert_file(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(md.contains("Hello write-only"), "{name}: {md}");
    }
}

/// A mislabelled encrypted file reports the encryption, not the declared
/// format's parse error: an encrypted `.ppt` named `.pptx` used to fail as
/// a bad ZIP.
#[test]
fn a_mislabelled_encrypted_file_reports_the_encryption() {
    let bytes = std::fs::read(fixture("min_encrypted.ppt")).unwrap();
    let err = convert(SourceDocument::from_bytes(
        "deck.pptx",
        InputFormat::Pptx,
        bytes,
    ))
    .expect_err("encrypted");
    assert!(err.ends_with("ppt: document is encrypted"), "{err}");
}

/// A password-protected ODF package lists `content.xml` with
/// `manifest:encryption-data`; its content is ciphertext, which used to fail
/// as "no content.xml".
#[test]
fn an_encrypted_odf_package_says_so() {
    let mut out = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut out);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("mimetype", stored).unwrap();
    zip.write_all(b"application/vnd.oasis.opendocument.text")
        .unwrap();
    zip.start_file("META-INF/manifest.xml", stored).unwrap();
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">
 <manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>
 <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml" manifest:size="2048">
  <manifest:encryption-data manifest:checksum-type="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0#sha256-1k" manifest:checksum="AAAA">
   <manifest:algorithm manifest:algorithm-name="http://www.w3.org/2001/04/xmlenc#aes256-cbc" manifest:initialisation-vector="AAAA"/>
   <manifest:key-derivation manifest:key-derivation-name="PBKDF2" manifest:key-size="32" manifest:iteration-count="100000" manifest:salt="AAAA"/>
  </manifest:encryption-data>
 </manifest:file-entry>
</manifest:manifest>"#,
    )
    .unwrap();
    zip.start_file("content.xml", stored).unwrap();
    zip.write_all(&[0x9C, 0x01, 0xFF, 0x80, 0x00, 0x7F])
        .unwrap();
    zip.finish().unwrap();
    let err = convert(SourceDocument::from_bytes(
        "locked.odt",
        InputFormat::Odt,
        out.into_inner(),
    ))
    .expect_err("encrypted");
    assert!(err.ends_with("odt: document is encrypted"), "{err}");
}
