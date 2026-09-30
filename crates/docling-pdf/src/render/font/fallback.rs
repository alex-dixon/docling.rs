//! Substitute faces for fonts a PDF does not embed: the standard 14 and
//! anything else named by `/BaseFont` alone. Like docling-parse's
//! `blend2d_font_resolver`, the faces come from the host's font directories
//! (Liberation / DejaVu / URW base35 / Noto on Linux, the system fonts on
//! macOS and Windows); `.models/fonts/` is searched first so a release can
//! ship its own, and `DOCLING_RS_FONT_DIRS` (path-list separated) adds more.
//! Without any face the renderer outlines each glyph's box in the thin blue
//! docling-parse draws for an unresolved cell.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Which family a name asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Sans,
    Serif,
    Mono,
    Symbol,
    Dingbats,
    /// CJK text (non-embedded composite fonts with an Adobe-* ordering).
    Cjk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Style {
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
}

/// A face file loaded once per process.
pub struct FallbackFace {
    pub data: Vec<u8>,
    pub path: PathBuf,
}

struct Index {
    /// lower-case file stem → path.
    files: HashMap<String, PathBuf>,
}

fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut files = HashMap::new();
        for dir in font_dirs() {
            scan(&dir, 0, &mut files);
        }
        Index { files }
    })
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    dirs.push(PathBuf::from(crate::resolve_asset(".models/fonts")));
    if let Some(extra) = docling_core::env::nonempty("DOCLING_RS_FONT_DIRS") {
        for d in std::env::split_paths(&extra) {
            dirs.push(d);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".fonts"));
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join("Library/Fonts"));
    }
    for d in [
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "/usr/X11R6/lib/X11/fonts",
        "/Library/Fonts",
        "/System/Library/Fonts",
        "/System/Library/Fonts/Supplemental",
        "C:\\Windows\\Fonts",
    ] {
        dirs.push(PathBuf::from(d));
    }
    if let Some(windir) = std::env::var_os("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    }
    dirs
}

fn scan(dir: &Path, depth: usize, files: &mut HashMap<String, PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            scan(&p, depth + 1, files);
            continue;
        }
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "ttf" | "otf" | "ttc") {
            continue;
        }
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            let key = stem.to_ascii_lowercase().replace([' ', '_'], "");
            files.entry(key).or_insert(p);
        }
    }
}

/// Candidate file stems per family and style, in preference order — the
/// resolver's own lists (`Arial, Liberation Sans, DejaVu Sans …`) turned into
/// the file names those packages install under.
fn candidates(style: Style) -> Vec<String> {
    let (b, i) = (style.bold, style.italic);
    let suffix_dash = |reg: &'static str,
                       bold: &'static str,
                       it: &'static str,
                       bi: &'static str|
     -> &'static str {
        match (b, i) {
            (true, true) => bi,
            (true, false) => bold,
            (false, true) => it,
            (false, false) => reg,
        }
    };
    let mut out = Vec::new();
    let mut push = |s: String| out.push(s.to_ascii_lowercase().replace([' ', '_'], ""));
    match style.family {
        Family::Sans => {
            push(format!(
                "LiberationSans-{}",
                suffix_dash("Regular", "Bold", "Italic", "BoldItalic")
            ));
            push(format!("arial{}", suffix_dash("", "bd", "i", "bi")));
            push(format!(
                "Arial{}",
                suffix_dash("", " Bold", " Italic", " Bold Italic")
            ));
            push(format!(
                "NimbusSans-{}",
                suffix_dash("Regular", "Bold", "Italic", "BoldItalic")
            ));
            push(format!(
                "DejaVuSans{}",
                suffix_dash("", "-Bold", "-Oblique", "-BoldOblique")
            ));
            push(format!(
                "FreeSans{}",
                suffix_dash("", "Bold", "Oblique", "BoldOblique")
            ));
            push(format!(
                "NotoSans-{}",
                suffix_dash("Regular", "Bold", "Italic", "BoldItalic")
            ));
            push("Helvetica".into());
            push("DejaVuSans".into());
            push("LiberationSans-Regular".into());
        }
        Family::Serif => {
            push(format!(
                "LiberationSerif-{}",
                suffix_dash("Regular", "Bold", "Italic", "BoldItalic")
            ));
            push(format!("times{}", suffix_dash("", "bd", "i", "bi")));
            push(format!(
                "Times New Roman{}",
                suffix_dash("", " Bold", " Italic", " Bold Italic")
            ));
            push(format!(
                "NimbusRoman-{}",
                suffix_dash("Regular", "Bold", "Italic", "BoldItalic")
            ));
            push(format!(
                "DejaVuSerif{}",
                suffix_dash("", "-Bold", "-Italic", "-BoldItalic")
            ));
            push(format!(
                "FreeSerif{}",
                suffix_dash("", "Bold", "Italic", "BoldItalic")
            ));
            push(format!(
                "NotoSerif-{}",
                suffix_dash("Regular", "Bold", "Italic", "BoldItalic")
            ));
            push("Times".into());
            push("DejaVuSerif".into());
            push("LiberationSerif-Regular".into());
        }
        Family::Mono => {
            push(format!(
                "LiberationMono-{}",
                suffix_dash("Regular", "Bold", "Italic", "BoldItalic")
            ));
            push(format!("cour{}", suffix_dash("", "bd", "i", "bi")));
            push(format!(
                "Courier New{}",
                suffix_dash("", " Bold", " Italic", " Bold Italic")
            ));
            push(format!(
                "NimbusMonoPS-{}",
                suffix_dash("Regular", "Bold", "Italic", "BoldItalic")
            ));
            push(format!(
                "DejaVuSansMono{}",
                suffix_dash("", "-Bold", "-Oblique", "-BoldOblique")
            ));
            push(format!(
                "FreeMono{}",
                suffix_dash("", "Bold", "Oblique", "BoldOblique")
            ));
            push("Courier".into());
            push("DejaVuSansMono".into());
            push("LiberationMono-Regular".into());
        }
        Family::Symbol => {
            push("StandardSymbolsPS".into());
            push("Symbol".into());
            push("symbol".into());
            push("DejaVuSans".into());
        }
        Family::Dingbats => {
            push("D050000L".into());
            push("Dingbats".into());
            push("ZapfDingbats".into());
            push("DejaVuSans".into());
        }
        Family::Cjk => {
            push("NotoSansCJK-Regular".into());
            push("NotoSansCJKjp-Regular".into());
            push("NotoSansCJKsc-Regular".into());
            push("DroidSansFallbackFull".into());
            push("DroidSansFallback".into());
            push("wqy-microhei".into());
            push("wqy-zenhei".into());
            push("fonts-japanese-gothic".into());
            push("ipag".into());
            push("msgothic".into());
            push("simsun".into());
            push("PingFang".into());
            push("DejaVuSans".into());
        }
    }
    out
}

/// The face for `style`, loaded once; `None` when the host has nothing.
pub fn face(style: Style) -> Option<Arc<FallbackFace>> {
    static CACHE: OnceLock<Mutex<HashMap<Style, Option<Arc<FallbackFace>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().ok().and_then(|c| c.get(&style).cloned()) {
        return v;
    }
    let idx = index();
    let mut found = None;
    for cand in candidates(style) {
        if let Some(p) = idx.files.get(&cand) {
            if let Ok(data) = std::fs::read(p) {
                if ttf_parser::Face::parse(&data, 0).is_ok() {
                    found = Some(Arc::new(FallbackFace {
                        data,
                        path: p.clone(),
                    }));
                    break;
                }
            }
        }
    }
    // A styled variant missing → the regular face of the family.
    if found.is_none() && (style.bold || style.italic) {
        found = face(Style {
            family: style.family,
            bold: false,
            italic: false,
        });
    }
    if let Ok(mut c) = cache.lock() {
        c.insert(style, found.clone());
    }
    found
}

/// Classify a `/BaseFont` name (subset prefix stripped) and descriptor flags
/// into a fallback style, the way the standard-14 substitution table and
/// docling-parse's name normalizer do it.
pub fn style_for(base_font: &str, flags: Option<i64>, serif_hint: Option<bool>) -> Style {
    let name = base_font.to_ascii_lowercase();
    let style_part = name.split_once(['-', ',']).map(|(_, s)| s).unwrap_or("");
    let bold = style_part.contains("bold")
        || name.contains("bold")
        || name.contains("black")
        || name.contains("heavy")
        || name.contains("semibold")
        || flags.is_some_and(|f| f & (1 << 18) != 0);
    let italic = name.contains("italic")
        || name.contains("oblique")
        || flags.is_some_and(|f| f & (1 << 6) != 0);
    let family = if name.contains("symbol") {
        Family::Symbol
    } else if name.contains("dingbat") || name.contains("wingding") {
        Family::Dingbats
    } else if name.contains("courier")
        || name.contains("mono")
        || name.contains("consolas")
        || name.contains("menlo")
        || flags.is_some_and(|f| f & 1 != 0 && !name.contains("arial"))
    {
        Family::Mono
    } else if name.contains("times")
        || name.contains("georgia")
        || name.contains("garamond")
        || name.contains("book")
        || name.contains("palatino")
        || name.contains("century")
        || name.contains("cambria")
        || name.contains("minion")
        || name.contains("nimbusrom")
        || name.contains("roman")
        || name.contains("serif") && !name.contains("sans")
        || name.starts_with("cm")
            && (name.starts_with("cmr") || name.starts_with("cmbx") || name.starts_with("cmti"))
    {
        Family::Serif
    } else if name.contains("arial")
        || name.contains("helvetica")
        || name.contains("verdana")
        || name.contains("calibri")
        || name.contains("sans")
        || name.contains("tahoma")
        || name.contains("segoe")
    {
        Family::Sans
    } else if serif_hint == Some(true) || flags.is_some_and(|f| f & 2 != 0) {
        Family::Serif
    } else {
        Family::Sans
    };
    Style {
        family,
        bold,
        italic,
    }
}
