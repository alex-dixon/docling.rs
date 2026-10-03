//! CLI surface tests: the flags a script or a container smoke test reaches for
//! before any document exists. They run the real binary, so they also pin that
//! `--help`/`--version` answer with **no models and no arguments** — the case
//! that broke the CUDA image smoke test in issue #333.

use std::process::Command;

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_docling-rs"))
        .args(args)
        .output()
        .expect("run docling-rs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `--version` / `-V`: exit 0, the crate version on stdout, nothing on stderr.
#[test]
fn version_flag_reports_the_crate_version() {
    for flag in ["--version", "-V"] {
        let (code, stdout, stderr) = run(&[flag]);
        assert_eq!(code, 0, "{flag}: stderr: {stderr}");
        assert!(
            stdout.starts_with(&format!("docling-rs {}", env!("CARGO_PKG_VERSION"))),
            "{flag}: {stdout:?}"
        );
        assert!(stderr.is_empty(), "{flag}: stderr: {stderr:?}");
    }
}

/// The version line names the optional features the binary carries, so a bug
/// report says which execution providers were even compiled in.
#[test]
fn version_lists_compiled_features() {
    let (_, stdout, _) = run(&["--version"]);
    #[cfg(feature = "chunking")]
    assert!(stdout.contains("chunking"), "{stdout:?}");
    #[cfg(not(feature = "chunking"))]
    assert!(!stdout.contains('('), "{stdout:?}");
}

/// `--help` / `-h`: exit 0, the flag list on stdout (not stderr — it is the
/// requested output, not a diagnostic).
#[test]
fn help_flag_prints_the_flag_list() {
    for flag in ["--help", "-h"] {
        let (code, stdout, stderr) = run(&[flag]);
        assert_eq!(code, 0, "{flag}: stderr: {stderr}");
        assert!(stdout.contains("usage: docling-rs"), "{flag}: {stdout:?}");
        for expected in ["--to md|json", "--pages A-B", "--pipeline standard|vlm"] {
            assert!(stdout.contains(expected), "{flag}: missing {expected}");
        }
        assert!(stderr.is_empty(), "{flag}: stderr: {stderr:?}");
    }
}

/// No arguments is a usage error, not a panic or a silent success.
#[test]
fn no_arguments_is_a_usage_error() {
    let (code, stdout, stderr) = run(&[]);
    assert_eq!(code, 2);
    assert!(stdout.is_empty(), "{stdout:?}");
    assert!(stderr.contains("no input file"), "{stderr:?}");
    assert!(stderr.contains("--help"), "{stderr:?}");
}

/// An unknown flag names itself and points at `--help`.
#[test]
fn unknown_flag_points_at_help() {
    let (code, _, stderr) = run(&["--no-such-flag"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("--no-such-flag"), "{stderr:?}");
    assert!(stderr.contains("--help"), "{stderr:?}");
}

/// `--help` after other flags still prints help rather than treating the flag
/// as a file name — a smoke test may append it to a canned argument list.
#[test]
fn help_is_recognized_in_any_position() {
    let (code, stdout, _) = run(&["--strict", "--to", "json", "--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("usage: docling-rs"), "{stdout:?}");
}

/// `--page-break-placeholder TEXT` (docling's `page_break_placeholder`):
/// TEXT lands between two pages' blocks and nowhere else. The DjVu fixture
/// converts on every build — no models, no pdfium — and streams through the
/// default Markdown path, so this also pins the streamer's output.
#[test]
fn page_break_placeholder_separates_pages() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docling/tests/data/djvu/sources/example.djvu"
    );
    let (code, plain, stderr) = run(&[fixture]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let (code, with, stderr) = run(&["--page-break-placeholder", "<!-- page break -->", fixture]);
    assert_eq!(code, 0, "stderr: {stderr}");
    // Three pages → two breaks, each a block of its own between two others.
    assert_eq!(with.matches("<!-- page break -->").count(), 2, "{with}");
    assert_eq!(with.replace("<!-- page break -->\n\n", ""), plain);
    assert!(!with.starts_with("<!-- page break -->"), "{with}");
    assert!(!with.trim_end().ends_with("<!-- page break -->"), "{with}");
    // `--no-stream` (the buffered serializer) agrees byte for byte.
    let (_, buffered, _) = run(&[
        "--page-break-placeholder",
        "<!-- page break -->",
        "--no-stream",
        fixture,
    ]);
    assert_eq!(buffered, with);
}

/// The flag needs its text — a bare flag is a usage error, like the other
/// value-taking flags.
#[test]
fn page_break_placeholder_requires_a_value() {
    let (code, _, stderr) = run(&["--page-break-placeholder"]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stderr.contains("--page-break-placeholder"), "{stderr}");
}

/// `--video-frames` rejects a missing or non-numeric value with a usage error
/// instead of silently falling back to the default (used to apply 8 frames).
#[test]
fn video_frames_requires_a_number() {
    for args in [
        &["--video-frames"][..],
        &["--video-frames", "1O", "x.mp4"][..],
    ] {
        let (code, _, stderr) = run(args);
        assert_eq!(code, 2, "args {args:?}, stderr: {stderr}");
        assert!(stderr.contains("--video-frames"), "{stderr}");
    }
}

/// `--xbrl-taxonomy DIR` (#466) needs its directory, and with it an XBRL
/// instance's JSON carries the fact graph with the presentation hierarchy the
/// taxonomy's linkbases give it (`to_child` links), where the instance alone
/// yields only the facts' own `to_value` links.
#[test]
fn xbrl_taxonomy_flag_feeds_the_fact_graph() {
    let (code, _, stderr) = run(&["--xbrl-taxonomy"]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stderr.contains("--xbrl-taxonomy"), "{stderr}");

    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/data/xbrl/sources/mlac-20251231.xml"
    );
    let taxonomy = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/data/xbrl/sources/mlac-taxonomy"
    );
    let (code, json, stderr) = run(&["--to", "json", "--xbrl-taxonomy", taxonomy, fixture]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        json.contains("\"self_ref\": \"#/key_value_items/0\""),
        "{}",
        &json[..200]
    );
    assert!(
        json.contains("\"label\": \"to_child\""),
        "no hierarchy links"
    );
    assert!(
        json.contains("\"text\": \"weight: 1.0\""),
        "no calculation weights"
    );
    // The instance's own directory holds no schema: the facts and their
    // concepts, but none of the linkbases' ancestors or weights.
    let (code, json, stderr) = run(&["--to", "json", fixture]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(json.contains("\"label\": \"to_value\""));
    assert!(!json.contains("\"text\": \"weight: 1.0\""));
    assert!(
        !json.contains("CoverAbstract"),
        "presentation ancestor without a taxonomy"
    );
}

/// #460: `--ocr-engine` takes ppocr | tesseract, and `--ocr-lang` is checked
/// against the engine whichever order the flags come in — `deu` is a
/// Tesseract language, not a PP-OCR model.
#[test]
fn ocr_engine_and_lang_validate_together() {
    let (code, _, stderr) = run(&["--ocr-engine", "easyocr", "x.pdf"]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stderr.contains("--ocr-engine"), "{stderr}");

    let (code, _, stderr) = run(&["--ocr-engine", "ppocr", "--ocr-lang", "deu", "x.pdf"]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stderr.contains("--ocr-lang"), "{stderr}");

    // Accepted under Tesseract in either flag order: the run then fails on
    // the missing input, not on the option.
    for args in [
        &[
            "--ocr-engine",
            "tesseract",
            "--ocr-lang",
            "deu+fra",
            "missing.pdf",
        ][..],
        &[
            "--ocr-lang",
            "iso:de",
            "--ocr-engine",
            "tesseract",
            "missing.pdf",
        ][..],
    ] {
        let (_, _, stderr) = run(args);
        assert!(!stderr.contains("--ocr-lang"), "args {args:?}: {stderr}");
        assert!(!stderr.contains("--ocr-engine"), "args {args:?}: {stderr}");
    }
}

/// A scratch directory under the system temp dir, unique per test, removed on
/// drop so a failed assertion doesn't leave outputs behind for the next run.
struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "docling-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().into_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const MD_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/data/md/sources");

/// Several positional sources are one batch (#489, Python's
/// `docling convert a.md b.md --output out/`): every file converts in the one
/// process, lands in `--output` by stem, its path prints on stdout, and the
/// batch summary counts them.
#[test]
fn several_positional_sources_convert_into_the_output_dir() {
    let out = Scratch::new("multi");
    let a = format!("{MD_FIXTURES}/duck.md");
    let b = format!("{MD_FIXTURES}/blocks.md");
    let (code, stdout, stderr) = run(&[&a, &b, "--output", &out.path("")]);
    assert_eq!(code, 0, "stderr: {stderr}");
    for stem in ["duck", "blocks"] {
        let written = out.0.join(format!("{stem}.md"));
        assert!(
            written.is_file(),
            "{} missing; stderr: {stderr}",
            written.display()
        );
        assert!(stdout.contains(&format!("{stem}.md")), "stdout: {stdout}");
    }
    assert!(
        stderr.contains("batch: 2 converted, 0 failed"),
        "stderr: {stderr}"
    );
    // The single-file contract is untouched: one positional source without
    // `--output` still prints the Markdown itself.
    let (code, stdout, _) = run(&[&a]);
    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        std::fs::read_to_string(out.0.join("duck.md")).unwrap()
    );
}

/// A positional directory sweeps its tree like `--input DIR`, and mixes with
/// plain files in the same batch; more than one source without `--output` is
/// a usage error (stdout can hold only one document).
#[test]
fn positional_directories_and_files_mix_and_need_an_output_dir() {
    let src = Scratch::new("tree");
    std::fs::create_dir_all(src.0.join("deep/er")).unwrap();
    std::fs::write(src.0.join("deep/er/x.md"), "# x\n").unwrap();
    std::fs::write(
        src.0.join("deep/notes.log"),
        "ignored: not a convertible extension\n",
    )
    .unwrap();
    let out = Scratch::new("tree-out");
    let single = format!("{MD_FIXTURES}/duck.md");
    let (code, stdout, stderr) = run(&[&src.path("deep"), &single, "--output", &out.path("")]);
    assert_eq!(code, 0, "stderr: {stderr}");
    // The directory's structure below it is kept; the file lands by stem.
    assert!(
        out.0.join("er/x.md").is_file(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        out.0.join("duck.md").is_file(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("batch: 2 converted, 0 failed"),
        "stderr: {stderr}"
    );

    let (code, _, stderr) = run(&[&single, &src.path("deep/er/x.md")]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stderr.contains("needs --output DIR"), "stderr: {stderr}");

    let (code, _, stderr) = run(&[
        &src.path("deep/missing.md"),
        &single,
        "--output",
        &out.path(""),
    ]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(
        stderr.contains("no such file or directory"),
        "stderr: {stderr}"
    );
}

/// Two files with the same stem from different directories would overwrite
/// each other in `--output`; the batch refuses up front (Python overwrites
/// silently) and names both files and the remedy.
#[test]
fn same_stem_sources_are_refused_before_converting() {
    let src = Scratch::new("collide");
    std::fs::create_dir_all(src.0.join("a")).unwrap();
    std::fs::create_dir_all(src.0.join("b")).unwrap();
    std::fs::write(src.0.join("a/report.md"), "# a\n").unwrap();
    std::fs::write(src.0.join("b/report.md"), "# b\n").unwrap();
    let out = Scratch::new("collide-out");
    let (code, stdout, stderr) = run(&[
        &src.path("a/report.md"),
        &src.path("b/report.md"),
        "--output",
        &out.path(""),
    ]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(
        stderr.contains("would both be written to"),
        "stderr: {stderr}"
    );
    assert!(stdout.is_empty(), "nothing converted: {stdout}");
    assert!(!out.0.join("report.md").exists());
    // The remedy: the common parent directory, whose tree is kept.
    let (code, _, stderr) = run(&[&src.path(""), "--output", &out.path("")]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(out.0.join("a/report.md").is_file(), "stderr: {stderr}");
    assert!(out.0.join("b/report.md").is_file(), "stderr: {stderr}");
}

/// `--abort-on-error` (Python's flag): the first failed file ends the batch
/// and the rest are reported as skipped; without it the failure is reported,
/// the batch continues, and the exit code is 1.
#[test]
fn abort_on_error_stops_at_the_first_failure() {
    let src = Scratch::new("abort");
    // Unknown extension: `SourceDocument::from_file` rejects it at convert
    // time, which is a per-file failure rather than an expansion error.
    std::fs::write(src.0.join("bad.xyz"), "not a document\n").unwrap();
    std::fs::write(src.0.join("good.md"), "# good\n").unwrap();
    let out = Scratch::new("abort-out");
    let (code, _, stderr) = run(&[
        &src.path("bad.xyz"),
        &src.path("good.md"),
        "--output",
        &out.path(""),
        "--abort-on-error",
    ]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(
        stderr.contains("aborting the batch (--abort-on-error)"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("batch: 0 converted, 1 failed, 1 skipped"),
        "stderr: {stderr}"
    );
    assert!(!out.0.join("good.md").exists(), "stderr: {stderr}");

    let (code, _, stderr) = run(&[
        &src.path("bad.xyz"),
        &src.path("good.md"),
        "--output",
        &out.path(""),
    ]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(
        stderr.contains("batch: 1 converted, 1 failed"),
        "stderr: {stderr}"
    );
    assert!(out.0.join("good.md").is_file(), "stderr: {stderr}");
}

/// `--to` is repeatable (#491, Python's `docling convert --to md --to json`):
/// one conversion, every format written under `--output`, each path on
/// stdout; the Markdown is byte-identical to the single-format stdout run,
/// so existing `--to md` callers see no change.
#[test]
fn repeated_to_writes_every_format_from_one_conversion() {
    let out = Scratch::new("to-multi");
    let src = format!("{MD_FIXTURES}/duck.md");
    let (code, stdout, stderr) = run(&[
        "--to",
        "md",
        "--to",
        "json",
        "--output",
        &out.path(""),
        &src,
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(out.0.join("duck.md").is_file(), "stderr: {stderr}");
    assert!(out.0.join("duck.json").is_file(), "stderr: {stderr}");
    assert!(
        stdout.contains("duck.md") && stdout.contains("duck.json"),
        "stdout: {stdout}"
    );
    assert_eq!(stdout.lines().count(), 2, "one path per format: {stdout}");
    assert!(
        stderr.contains("batch: 1 converted, 0 failed"),
        "stderr: {stderr}"
    );
    let (code, single, _) = run(&["--to", "md", &src]);
    assert_eq!(code, 0);
    assert_eq!(
        single,
        std::fs::read_to_string(out.0.join("duck.md")).unwrap()
    );
    let (code, single_json, _) = run(&["--to", "json", &src]);
    assert_eq!(code, 0);
    assert_eq!(
        single_json.trim_end(),
        std::fs::read_to_string(out.0.join("duck.json"))
            .unwrap()
            .trim_end()
    );
}

/// Several sources × several formats in one run; the comma form
/// (`--to md,json`) and `markdown` spell the same list, and a format named
/// twice is written once.
#[test]
fn repeated_to_covers_every_source_and_dedupes() {
    let out = Scratch::new("to-matrix");
    let a = format!("{MD_FIXTURES}/duck.md");
    let b = format!("{MD_FIXTURES}/blocks.md");
    let (code, stdout, stderr) = run(&[
        "--to",
        "markdown,json",
        "--to",
        "latex",
        "--to",
        "md",
        "--output",
        &out.path(""),
        &a,
        &b,
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    for stem in ["duck", "blocks"] {
        for ext in ["md", "json", "tex"] {
            assert!(
                out.0.join(format!("{stem}.{ext}")).is_file(),
                "{stem}.{ext}; stderr: {stderr}"
            );
        }
    }
    assert_eq!(
        stdout.lines().count(),
        6,
        "2 documents × 3 formats, md once: {stdout}"
    );
    assert!(
        stderr.contains("batch: 2 converted, 0 failed"),
        "stderr: {stderr}"
    );
}

/// Several formats without `--output` is a usage error (stdout carries one
/// document), and an unknown entry anywhere in the list is rejected.
#[test]
fn repeated_to_needs_an_output_dir_and_validates_each_entry() {
    let src = format!("{MD_FIXTURES}/duck.md");
    let (code, _, stderr) = run(&["--to", "md", "--to", "json", &src]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(
        stderr.contains("several --to formats need --output DIR"),
        "stderr: {stderr}"
    );
    let (code, _, stderr) = run(&["--to", "md,pdf", &src]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stderr.contains("unknown --to 'pdf'"), "stderr: {stderr}");
    let (code, _, stderr) = run(&["--to"]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stderr.contains("--to needs a format"), "stderr: {stderr}");
}

/// A failed document fails all of its formats at once, and `--abort-on-error`
/// still stops the batch before the next document writes anything.
#[test]
fn repeated_to_respects_abort_on_error() {
    let src = Scratch::new("to-abort");
    std::fs::write(src.0.join("bad.xyz"), "not a document\n").unwrap();
    std::fs::write(src.0.join("good.md"), "# good\n").unwrap();
    let out = Scratch::new("to-abort-out");
    let (code, stdout, stderr) = run(&[
        "--to",
        "md",
        "--to",
        "json",
        "--abort-on-error",
        "--output",
        &out.path(""),
        &src.path("bad.xyz"),
        &src.path("good.md"),
    ]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        stderr.contains("batch: 0 converted, 1 failed, 1 skipped"),
        "stderr: {stderr}"
    );
    assert!(!out.0.join("good.md").exists() && !out.0.join("good.json").exists());
    let (code, stdout, stderr) = run(&[
        "--to",
        "md",
        "--to",
        "json",
        "--output",
        &out.path(""),
        &src.path("bad.xyz"),
        &src.path("good.md"),
    ]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert_eq!(stdout.lines().count(), 2, "stdout: {stdout}");
    assert!(out.0.join("good.md").is_file() && out.0.join("good.json").is_file());
}

/// `--to html` (#492): a complete HTML document on stdout for one file, and
/// `<stem>.html` per document in batch mode.
#[test]
fn to_html_writes_a_complete_document() {
    let src = format!("{MD_FIXTURES}/duck.md");
    let (code, stdout, stderr) = run(&["--to", "html", &src]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.starts_with("<!DOCTYPE html>\n<html>\n<head>\n"),
        "{stdout:.80}"
    );
    assert!(stdout.contains("<title>duck</title>"), "{stdout:.400}");
    assert!(stdout.contains("<div class='page'>"), "{stdout:.400}");
    assert!(stdout.trim_end().ends_with("</html>"), "{stdout:.80}");
    let out = Scratch::new("html");
    let (code, paths, stderr) = run(&["--to", "html", "--output", &out.path(""), &src]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(paths.trim_end().ends_with("duck.html"), "{paths}");
    let written = std::fs::read_to_string(out.0.join("duck.html")).unwrap();
    assert_eq!(written.trim_end(), stdout.trim_end());
}

/// `--to pandoc` (#515): Pandoc's JSON AST on stdout, `<stem>.pandoc.json`
/// in batch mode; `--pandoc-api-version` other than 1.23 is a usage error
/// named before anything converts.
#[test]
fn to_pandoc_writes_the_ast_and_checks_the_api_version() {
    let src = format!("{MD_FIXTURES}/duck.md");
    let (code, stdout, stderr) = run(&["--to", "pandoc", &src]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.starts_with("{\"pandoc-api-version\":[1,23,1,1],\"meta\":{},\"blocks\":["),
        "{stdout}"
    );

    let out = Scratch::new("to-pandoc");
    let (code, _, stderr) = run(&["--to", "pandoc", "--output", &out.path(""), &src]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let written = std::fs::read_to_string(out.0.join("duck.pandoc.json")).unwrap();
    assert_eq!(written.trim_end(), stdout.trim_end());

    let (code, _, stderr) = run(&["--to", "pandoc", "--pandoc-api-version", "1.23", &src]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let (code, stdout, stderr) = run(&["--to", "pandoc", "--pandoc-api-version", "1.22", &src]);
    assert_eq!(code, 2, "stdout: {stdout}");
    assert!(
        stderr.contains("unsupported Pandoc API version '1.22'"),
        "stderr: {stderr}"
    );
}

/// `--images-scale` (#520) takes the same 0.1-4.0 window as `--scale`; a
/// value outside it is a usage error before anything converts.
#[test]
fn images_scale_out_of_range_is_a_usage_error() {
    for bad in ["0", "9", "abc"] {
        let (code, _, err) = run(&["--images-scale", bad, "x.pdf"]);
        assert_eq!(code, 2, "--images-scale {bad}");
        assert!(err.contains("--images-scale"), "{err}");
    }
}
