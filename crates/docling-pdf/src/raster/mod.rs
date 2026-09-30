//! Pure-Rust raster of an *image-only* page — a scan: one or a few `/Image`
//! XObjects blitted onto a white page — reproducing pdfium's bitmap byte for
//! byte, so the OCR path needs no `libpdfium` for the pages it runs on.
//!
//! Phase 2 of retiring pdfium (docs/PDF_CONFORMANCE.md, "Retiring pdfium").
//! The page is accepted only when everything pdfium would do to render it is
//! something this module does identically, and declined (`None`, the caller
//! falls back to pdfium or the docling-parse plugin) otherwise:
//!
//! * the content stream — through Form XObjects, with their `/Matrix` and
//!   `/BBox` — draws nothing but images: `q`/`Q`/`cm`, rectangular `re W n`
//!   clips, ExtGStates that change nothing visible, invisible (`3 Tr`) text;
//!   a painted path, shading, inline image, non-rectangular clip, visible
//!   text, or a soft mask/blend/alpha declines the page;
//! * each image is 1- or 8-bit DeviceGray/CalGray, or 8-bit
//!   DeviceRGB/CalRGB/sRGB-ICC, without `/SMask`/`/Mask`/`/ImageMask`, in a
//!   filter chain this module decodes (Flate/LZW/RunLength/ASCII with
//!   predictors, and `DCTDecode` through [`jpeg`]); JPX, JBIG2, CCITT, CMYK,
//!   Indexed and non-sRGB ICC images stay with pdfium;
//! * the image matrix is axis-aligned or a 90° rotation — pdfium's
//!   `CFX_AggImageRenderer` stretch paths, both ported in [`stretch`]; a
//!   general affine placement (its `CFX_ImageTransformer`) declines.
//!
//! The device geometry is pdfium's to the float: the display matrix of
//! `CPDF_Page::GetDisplayMatrix`, the CTM chain of the content parser
//! (`cm` prepends, forms concatenate), `GetUnitRect().GetOuterRect()` for
//! the image's device rectangle, the rectangular-clip shortcut of
//! `CFX_AggDeviceDriver::SetClip_PathFill`, the flips and swapped clip of a
//! 90° placement, and the image's own decode/palette rules of `CPDF_DIB`
//! (a `/Decode [1 0]` gray image inverts before the stretch, a 1-bit one
//! after it, the way pdfium's palettes fall). The oracle is pdfium itself:
//! `tests::matches_pdfium_on_the_scanned_fixtures` compares against
//! `render_with_config` when the library is installed.

pub mod filters;
pub mod jpeg;
pub mod stretch;

use std::collections::HashMap;

use image::RgbImage;
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::pdf_meta::PdfMeta;
use stretch::{Options, Rect, Source};

/// The renderer knob for image-only pages: `DOCLING_RS_SCAN_RASTER`.
/// `rust` (default) takes this module's bitmap whenever the page qualifies;
/// `pdfium` never does — an A/B switch for conformance runs.
pub fn enabled() -> bool {
    !matches!(
        docling_core::env::nonempty("DOCLING_RS_SCAN_RASTER").as_deref(),
        Some("pdfium") | Some("off") | Some("0")
    )
}

/// Render page `index` into a `width` × `height` bitmap the way pdfium's
/// `FPDF_RenderPageBitmap` (white-cleared, `FPDF_ANNOT`) would, if the page
/// is image-only in the sense above. `None` — with the reason under
/// `DOCLING_RS_DEBUG` — when it is not, or something in it is unsupported.
pub fn render(meta: &PdfMeta, index: usize, width: u32, height: u32) -> Option<RgbImage> {
    match render_inner(meta, index, width, height) {
        Ok(img) => Some(img),
        Err(why) => {
            docling_core::debug_log!(
                "docling-pdf: page {}: not rendered by the Rust raster ({why})",
                index + 1
            );
            None
        }
    }
}

/// pdfium's `CFX_Matrix`: `f32`, applied as `(a·x + c·y + e, b·x + d·y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct M {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl M {
    const ID: M = M {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// `CFX_Matrix::operator*(right)`: `self` first, then `right`.
    fn then(self, r: M) -> M {
        M {
            a: self.a * r.a + self.b * r.c,
            b: self.a * r.b + self.b * r.d,
            c: self.c * r.a + self.d * r.c,
            d: self.c * r.b + self.d * r.d,
            e: self.e * r.a + self.f * r.c + r.e,
            f: self.e * r.b + self.f * r.d + r.f,
        }
    }

    fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    fn from_array(v: &[f32]) -> Option<M> {
        if v.len() != 6 || v.iter().any(|x| !x.is_finite()) {
            return None;
        }
        Some(M {
            a: v[0],
            b: v[1],
            c: v[2],
            d: v[3],
            e: v[4],
            f: v[5],
        })
    }
}

/// `CFX_FloatRect` after `TransformRect`/`Normalize`: y-up naming, but the
/// values are device coordinates here.
#[derive(Clone, Copy, Debug)]
struct FRect {
    left: f32,
    bottom: f32,
    right: f32,
    top: f32,
}

impl FRect {
    fn from_points(pts: &[(f32, f32)]) -> FRect {
        let mut r = FRect {
            left: pts[0].0,
            right: pts[0].0,
            bottom: pts[0].1,
            top: pts[0].1,
        };
        for &(x, y) in &pts[1..] {
            r.left = r.left.min(x);
            r.right = r.right.max(x);
            r.bottom = r.bottom.min(y);
            r.top = r.top.max(y);
        }
        r
    }

    fn normalize(&mut self) {
        if self.left > self.right {
            std::mem::swap(&mut self.left, &mut self.right);
        }
        if self.bottom > self.top {
            std::mem::swap(&mut self.bottom, &mut self.top);
        }
    }

    /// `CFX_FloatRect::Intersect`: an empty intersection is the zero rect.
    fn intersect(&mut self, o: FRect) {
        self.normalize();
        let mut o = o;
        o.normalize();
        self.left = self.left.max(o.left);
        self.bottom = self.bottom.max(o.bottom);
        self.right = self.right.min(o.right);
        self.top = self.top.min(o.top);
        if self.left > self.right || self.bottom > self.top {
            *self = FRect {
                left: 0.0,
                bottom: 0.0,
                right: 0.0,
                top: 0.0,
            };
        }
    }

    /// `GetOuterRect`: floor the low edges, ceil the high ones (y down).
    fn outer(&self) -> Rect {
        let mut r = Rect::new(
            sat(self.left.floor()),
            sat(self.bottom.floor()),
            sat(self.right.ceil()),
            sat(self.top.ceil()),
        );
        r.normalize();
        r
    }
}

fn sat(v: f32) -> i32 {
    if v.is_nan() {
        0
    } else {
        v.clamp(i32::MIN as f32, i32::MAX as f32) as i32
    }
}

/// One image placement in device space.
struct Draw {
    stream: ObjectId,
    /// Image unit square → device.
    matrix: M,
    /// Device clip at the time of `Do` (integer, from the rectangular clips).
    clip: Rect,
}

/// The graphics state the walk tracks (`CPDF_AllStates` subset).
#[derive(Clone, Copy)]
struct GState {
    ctm: M,
    clip: Rect,
    /// `Tr` — only invisible text (3) is tolerated.
    text_render: i64,
}

/// Rendering context of one content stream: the `mtObj2Device` its objects
/// are rendered with (the display matrix for the page, `F * parent` for a
/// form), the resources and the page resources pdfium falls back to.
struct Ctx<'a> {
    doc: &'a Document,
    obj2dev: M,
    res: Option<&'a Dictionary>,
    page_res: Option<&'a Dictionary>,
    device: Rect,
    depth: usize,
}

struct Walker {
    draws: Vec<Draw>,
    ops: usize,
}

const MAX_OPS: usize = 500_000;
const MAX_DEPTH: usize = 12;

fn deref<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    match obj {
        Object::Reference(id) => doc.get_object(*id).unwrap_or(obj),
        o => o,
    }
}

fn as_dict<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Dictionary> {
    match deref(doc, obj) {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

fn num(o: &Object) -> Option<f32> {
    match o {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

fn nums(ops: &[Object]) -> Option<Vec<f32>> {
    ops.iter().map(num).collect()
}

fn name_is(o: Option<&Object>, n: &[u8]) -> bool {
    matches!(o, Some(Object::Name(v)) if v == n)
}

fn bool_or_int_true(doc: &Document, o: Option<&Object>) -> bool {
    match o.map(|o| deref(doc, o)) {
        Some(Object::Boolean(b)) => *b,
        Some(Object::Integer(i)) => *i != 0,
        Some(Object::Real(r)) => *r != 0.0,
        _ => false,
    }
}

/// A resource of `kind`/`name`: the stream's own resources, then the page's
/// (`CPDF_StreamContentParser::FindResourceHolder`).
fn find_resource<'a>(ctx: &Ctx<'a>, kind: &[u8], name: &[u8]) -> Option<&'a Object> {
    [ctx.res, ctx.page_res]
        .into_iter()
        .flatten()
        .find_map(|res| {
            res.get(kind)
                .ok()
                .and_then(|o| as_dict(ctx.doc, o))
                .and_then(|d| d.get(name).ok())
        })
}

/// Is this ExtGState invisible to an opaque image? Anything that could
/// change pixels (alpha, blend, soft mask, transfer, overprint) declines.
fn extgstate_is_neutral(doc: &Document, gs: &Dictionary) -> Result<(), String> {
    for (k, v) in gs.iter() {
        let v = deref(doc, v);
        let ok = match k.as_slice() {
            b"Type" | b"LW" | b"LC" | b"LJ" | b"ML" | b"D" | b"RI" | b"Font" | b"FL" | b"SM"
            | b"SA" | b"OPM" | b"TK" => true,
            b"CA" | b"ca" => num(v) == Some(1.0),
            b"OP" | b"op" => matches!(v, Object::Boolean(false)),
            b"BM" => match v {
                Object::Name(n) => n == b"Normal" || n == b"Compatible",
                Object::Array(a) => a.first().is_some_and(|o| {
                    name_is(Some(o), b"Normal") || name_is(Some(o), b"Compatible")
                }),
                _ => false,
            },
            b"SMask" => name_is(Some(v), b"None"),
            b"AIS" => matches!(v, Object::Boolean(false)),
            _ => false,
        };
        if !ok {
            return Err(format!("ExtGState /{}", String::from_utf8_lossy(k)));
        }
    }
    Ok(())
}

impl Walker {
    /// `re x y w h` under `ctm`, then `obj2dev`: pdfium's rectangular clip —
    /// the five path points transformed twice in `f32`, still a rectangle,
    /// intersected with the device and rounded outwards.
    fn rect_clip(&self, ctx: &Ctx<'_>, ctm: M, re: &[f32]) -> Result<Rect, String> {
        let (x, y, w, h) = (re[0], re[1], re[2], re[3]);
        let user = [(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)];
        let mut dev = [(0f32, 0f32); 5];
        for (i, &(px, py)) in user.iter().enumerate() {
            // `CPDF_StreamContentParser::AddPathObject`: the path is stored in
            // content space (CTM applied), then `CFX_Path::GetRect(&mtObj2Device)`.
            let (ux, uy) = ctm.apply(px, py);
            dev[i] = ctx.obj2dev.apply(ux, uy);
            if i > 0 && dev[i].0 != dev[i - 1].0 && dev[i].1 != dev[i - 1].1 {
                return Err("non-rectangular clip".into());
            }
        }
        if dev[0].0 != dev[3].0 && dev[0].1 != dev[3].1 {
            return Err("non-rectangular clip".into());
        }
        let mut r = FRect {
            left: dev[0].0,
            bottom: dev[0].1,
            right: dev[2].0,
            top: dev[2].1,
        };
        r.normalize();
        r.intersect(FRect {
            left: 0.0,
            bottom: 0.0,
            right: ctx.device.right as f32,
            top: ctx.device.bottom as f32,
        });
        Ok(r.outer())
    }

    fn walk<'a>(&mut self, ctx: &Ctx<'a>, content: &[u8], init: GState) -> Result<(), String> {
        if ctx.depth > MAX_DEPTH {
            return Err("form nesting".into());
        }
        let ops = lopdf::content::Content::decode(content).map_err(|e| format!("content: {e}"))?;
        let mut stack: Vec<GState> = Vec::new();
        let mut st = init;
        // The current path, as far as a clip can use it: exactly one `re`.
        let mut path_rect: Option<Vec<f32>> = None;
        let mut path_other = false;
        let mut clip_pending = false;
        let mut compat = 0usize;
        for op in &ops.operations {
            self.ops += 1;
            if self.ops > MAX_OPS {
                return Err("content too long".into());
            }
            let o = op.operator.as_str();
            let args = &op.operands;
            match o {
                "q" => {
                    stack.push(st);
                    if stack.len() > 256 {
                        return Err("q nesting".into());
                    }
                }
                "Q" => {
                    if let Some(s) = stack.pop() {
                        st = s;
                    }
                }
                "cm" => {
                    let v = nums(args).ok_or("cm operands")?;
                    let m = M::from_array(&v).ok_or("cm matrix")?;
                    // `prepend_to_current_transformation_matrix`: new × CTM.
                    st.ctm = m.then(st.ctm);
                }
                "gs" => {
                    let name = match args.first() {
                        Some(Object::Name(n)) => n,
                        _ => return Err("gs operand".into()),
                    };
                    let gs = find_resource(ctx, b"ExtGState", name)
                        .and_then(|o| as_dict(ctx.doc, o))
                        .ok_or("gs resource")?;
                    extgstate_is_neutral(ctx.doc, gs)?;
                }
                // Path construction: only a lone rectangle can become a clip.
                "re" => {
                    let v = nums(args).ok_or("re operands")?;
                    if v.len() != 4 {
                        return Err("re operands".into());
                    }
                    if path_rect.is_some() || path_other {
                        path_other = true;
                    } else {
                        path_rect = Some(v);
                    }
                }
                "m" | "l" | "c" | "v" | "y" | "h" => path_other = true,
                "W" | "W*" => clip_pending = true,
                "n" => {
                    if clip_pending {
                        match (&path_rect, path_other) {
                            (Some(re), false) => {
                                let r = self.rect_clip(ctx, st.ctm, re)?;
                                st.clip.intersect(&r);
                            }
                            _ => return Err("non-rectangular clip".into()),
                        }
                    }
                    path_rect = None;
                    path_other = false;
                    clip_pending = false;
                }
                "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "S" | "s" => {
                    return Err(format!("painted path ({o})"));
                }
                "sh" => return Err("shading".into()),
                "BI" | "ID" | "EI" => return Err("inline image".into()),
                "d0" | "d1" => return Err("type3 glyph".into()),
                // Text: state is fine, showing glyphs is not unless invisible.
                "BT" | "ET" | "Tc" | "Tw" | "Tz" | "TL" | "Tf" | "Ts" | "Td" | "TD" | "Tm"
                | "T*" => {}
                "Tr" => {
                    st.text_render = args.first().and_then(|o| o.as_i64().ok()).unwrap_or(0);
                }
                "Tj" | "TJ" | "'" | "\"" => {
                    if st.text_render != 3 {
                        return Err("visible text".into());
                    }
                }
                // Colour, line state, marked content, compatibility: no pixels.
                "g" | "rg" | "k" | "cs" | "sc" | "scn" | "G" | "RG" | "K" | "CS" | "SC" | "SCN"
                | "w" | "J" | "j" | "M" | "d" | "ri" | "i" | "BMC" | "BDC" | "EMC" | "MP"
                | "DP" => {}
                "BX" => compat += 1,
                "EX" => compat = compat.saturating_sub(1),
                "Do" => {
                    let name = match args.first() {
                        Some(Object::Name(n)) => n,
                        _ => return Err("Do operand".into()),
                    };
                    let xobj = find_resource(ctx, b"XObject", name).ok_or("Do resource")?;
                    let (id, stream) = match xobj {
                        Object::Reference(id) => match ctx.doc.get_object(*id) {
                            Ok(Object::Stream(s)) => (*id, s),
                            _ => return Err("XObject reference".into()),
                        },
                        // A direct XObject stream has no id to cache under;
                        // pdfium clones inline ones — rare enough to decline.
                        _ => return Err("direct XObject".into()),
                    };
                    let subtype = stream.dict.get(b"Subtype").ok().map(|o| deref(ctx.doc, o));
                    if name_is(subtype, b"Image") {
                        if st.clip.is_empty() {
                            continue;
                        }
                        self.draws.push(Draw {
                            stream: id,
                            matrix: st.ctm.then(ctx.obj2dev),
                            clip: st.clip,
                        });
                    } else if name_is(subtype, b"Form") {
                        self.form(ctx, stream, st)?;
                    } else {
                        return Err("XObject subtype".into());
                    }
                }
                _ if compat > 0 => {}
                other => return Err(format!("operator {other}")),
            }
        }
        Ok(())
    }

    /// `Do` on a Form XObject: pdfium parses it with `/Matrix` as the initial
    /// CTM, its `/BBox` as a clip in the form's own space, and renders its
    /// objects with `form_matrix * mtObj2Device` where `form_matrix` is the
    /// CTM at `Do`.
    fn form<'a>(
        &mut self,
        ctx: &Ctx<'a>,
        stream: &'a lopdf::Stream,
        st: GState,
    ) -> Result<(), String> {
        let d = &stream.dict;
        if let Some(group) = d.get(b"Group").ok().and_then(|o| as_dict(ctx.doc, o)) {
            // An isolated or knockout transparency group renders through a
            // separate bitmap (`ProcessTransparency`); not reproduced here.
            if bool_or_int_true(ctx.doc, group.get(b"I").ok())
                || bool_or_int_true(ctx.doc, group.get(b"K").ok())
            {
                return Err("isolated/knockout transparency group".into());
            }
        }
        if d.get(b"OC").is_ok() {
            return Err("optional content".into());
        }
        let form_matrix = match d.get(b"Matrix").ok().map(|o| deref(ctx.doc, o)) {
            Some(Object::Array(a)) => {
                let v: Vec<f32> = a.iter().filter_map(|o| num(deref(ctx.doc, o))).collect();
                M::from_array(&v).ok_or("form /Matrix")?
            }
            _ => M::ID,
        };
        let inner = Ctx {
            doc: ctx.doc,
            obj2dev: st.ctm.then(ctx.obj2dev),
            res: d
                .get(b"Resources")
                .ok()
                .and_then(|o| as_dict(ctx.doc, o))
                .or(ctx.res),
            page_res: ctx.page_res,
            device: ctx.device,
            depth: ctx.depth + 1,
        };
        let mut inner_state = GState {
            ctm: form_matrix,
            clip: st.clip,
            text_render: st.text_render,
        };
        if let Some(Object::Array(b)) = d.get(b"BBox").ok().map(|o| deref(ctx.doc, o)) {
            let v: Vec<f32> = b.iter().filter_map(|o| num(deref(ctx.doc, o))).collect();
            if v.len() != 4 {
                return Err("form /BBox".into());
            }
            // `CPDF_Path::AppendFloatRect` of the normalized bbox, transformed
            // by the form matrix — i.e. the rectangle `re` would build.
            let (l, r) = (v[0].min(v[2]), v[0].max(v[2]));
            let (bt, tp) = (v[1].min(v[3]), v[1].max(v[3]));
            let clip = self.rect_clip(&inner, form_matrix, &[l, bt, r - l, tp - bt])?;
            inner_state.clip.intersect(&clip);
        }
        if inner_state.clip.is_empty() {
            return Ok(());
        }
        let content = stream
            .decompressed_content()
            .map_err(|e| format!("form content: {e}"))?;
        self.walk(&inner, &content, inner_state)
    }
}

/// A decoded image in the form pdfium's `CPDF_DIB` would present it to the
/// stretch engine.
struct Decoded {
    width: i32,
    height: i32,
    kind: Kind,
    interpolate: bool,
}

enum Kind {
    /// 1 bpp with the two-entry palette pdfium builds for a non-default
    /// `/Decode` (`None` = the stock black/white, no palette).
    Bilevel {
        data: Vec<u8>,
        stride: usize,
        palette: Option<[u8; 2]>,
    },
    Gray8(Vec<u8>),
    Rgb8(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Gray,
    Rgb,
}

/// Resolve the image's `/ColorSpace` to a family this module reproduces.
fn color_family(
    doc: &Document,
    cs: &Object,
    res: Option<&Dictionary>,
    page_res: Option<&Dictionary>,
) -> Result<Family, String> {
    match deref(doc, cs) {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" => Ok(Family::Gray),
            b"DeviceRGB" | b"RGB" | b"CalRGB" => Ok(Family::Rgb),
            b"DeviceCMYK" | b"CMYK" | b"Pattern" | b"I" | b"Indexed" => {
                Err(format!("colour space /{}", String::from_utf8_lossy(n)))
            }
            other => {
                // A named resource.
                for r in [res, page_res].into_iter().flatten() {
                    if let Some(o) = r
                        .get(b"ColorSpace")
                        .ok()
                        .and_then(|o| as_dict(doc, o))
                        .and_then(|d| d.get(other).ok())
                    {
                        return color_family(doc, o, None, None);
                    }
                }
                Err(format!("colour space /{}", String::from_utf8_lossy(other)))
            }
        },
        Object::Array(a) => {
            let fam = a.first().map(|o| deref(doc, o));
            match fam {
                Some(Object::Name(n)) if n == b"CalGray" => Ok(Family::Gray),
                Some(Object::Name(n)) if n == b"CalRGB" => Ok(Family::Rgb),
                Some(Object::Name(n)) if n == b"ICCBased" => {
                    let stream = a
                        .get(1)
                        .map(|o| deref(doc, o))
                        .and_then(|o| o.as_stream().ok())
                        .ok_or("ICCBased stream")?;
                    let n = stream
                        .dict
                        .get(b"N")
                        .ok()
                        .and_then(|o| deref(doc, o).as_i64().ok())
                        .unwrap_or(0);
                    // pdfium treats exactly one profile as a plain swap — the
                    // 3144-byte sRGB IEC61966-2.1 — and runs Little-CMS on
                    // every other one.
                    let bytes = stream.decompressed_content().unwrap_or_default();
                    if n == 3
                        && bytes.len() == 3144
                        && bytes.get(400..417) == Some(b"sRGB IEC61966-2.1")
                    {
                        Ok(Family::Rgb)
                    } else {
                        Err("ICC profile (Little-CMS transform)".into())
                    }
                }
                Some(Object::Name(n)) => {
                    Err(format!("colour space /{}", String::from_utf8_lossy(n)))
                }
                _ => Err("colour space".into()),
            }
        }
        _ => Err("colour space".into()),
    }
}

/// Decode one image XObject the way `CPDF_DIB` loads it, or say why not.
/// `device` is the render bitmap size — pdfium decodes a JPEG at a reduced
/// DCT scale when the image is at least twice as large, which this module
/// does not (yet) reproduce.
fn decode_image(
    ctx: &Ctx<'_>,
    stream: &lopdf::Stream,
    device: (u32, u32),
) -> Result<Decoded, String> {
    let doc = ctx.doc;
    let d = &stream.dict;
    let int = |k: &[u8]| d.get(k).ok().and_then(|o| deref(doc, o).as_i64().ok());
    let width = int(b"Width").or_else(|| int(b"W")).ok_or("Width")?;
    let height = int(b"Height").or_else(|| int(b"H")).ok_or("Height")?;
    if width <= 0 || height <= 0 || width > 1 << 16 || height > 1 << 16 {
        return Err("image size".into());
    }
    if bool_or_int_true(doc, d.get(b"ImageMask").ok().or_else(|| d.get(b"IM").ok())) {
        return Err("image mask".into());
    }
    if d.get(b"SMask").is_ok() || d.get(b"Mask").is_ok() {
        return Err("soft/colour-key mask".into());
    }
    let interpolate =
        bool_or_int_true(doc, d.get(b"Interpolate").ok().or_else(|| d.get(b"I").ok()));
    let cs = d
        .get(b"ColorSpace")
        .ok()
        .or_else(|| d.get(b"CS").ok())
        .ok_or("no ColorSpace")?;
    let family = color_family(doc, cs, ctx.res, ctx.page_res)?;
    let bpc = int(b"BitsPerComponent")
        .or_else(|| int(b"BPC"))
        .unwrap_or(0);
    let decode: Option<Vec<f32>> = match d
        .get(b"Decode")
        .ok()
        .or_else(|| d.get(b"D").ok())
        .map(|o| deref(doc, o))
    {
        Some(Object::Array(a)) => Some(a.iter().filter_map(|o| num(deref(doc, o))).collect()),
        _ => None,
    };

    let chain = filters::filters(doc, d);
    let (data, codec) =
        filters::apply(doc, &stream.content, &chain).map_err(|e| format!("filter {e:?}"))?;
    let (w, h) = (width as usize, height as usize);

    let samples: Kind = match codec {
        Some(codec) if codec.name == "DCTDecode" => {
            let (dw, dh) = (i64::from(device.0), i64::from(device.1));
            if dw > 0 && dh > 0 {
                let ratio = (width / dw).min(height / dh).max(1);
                if ratio >= 2 {
                    return Err("JPEG decoded at a reduced DCT scale by pdfium".into());
                }
            }
            let transform = codec
                .parms
                .as_ref()
                .and_then(|p| p.get(b"ColorTransform").ok())
                .and_then(|o| deref(doc, o).as_i64().ok())
                .unwrap_or(1)
                != 0;
            let img = jpeg::decode(&data, transform).map_err(|e| format!("JPEG {e:?}"))?;
            if img.width != w || img.height != h {
                return Err("JPEG size differs from the dictionary".into());
            }
            match (img.channels, family) {
                (1, Family::Gray) => Kind::Gray8(img.data),
                (3, Family::Rgb) => Kind::Rgb8(img.data),
                _ => return Err("JPEG components vs colour space".into()),
            }
        }
        Some(codec) => return Err(format!("codec {}", codec.name)),
        None => {
            let comps = if family == Family::Gray { 1 } else { 3 };
            match (bpc, family) {
                (1, Family::Gray) => {
                    let stride = w.div_ceil(8);
                    if data.len() < stride * h {
                        return Err("truncated image data".into());
                    }
                    Kind::Bilevel {
                        data,
                        stride,
                        palette: None,
                    }
                }
                (8, _) => {
                    if data.len() < w * h * comps {
                        return Err("truncated image data".into());
                    }
                    if comps == 1 {
                        Kind::Gray8(data)
                    } else {
                        Kind::Rgb8(data)
                    }
                }
                _ => return Err(format!("{bpc} bits per component")),
            }
        }
    };

    // `/Decode`: pdfium's palette rules. Default arrays are a no-op; a gray
    // `[1 0]` inverts (a 1-bit image after the stretch through its palette,
    // an 8-bit one before it through the 256-entry palette); anything else
    // on RGB is a per-component transform this module does not do.
    let kind = match (decode, samples) {
        (None, s) => s,
        (Some(dec), s) => {
            let default_gray = dec == [0.0, 1.0];
            let default_rgb = dec == [0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
            match s {
                Kind::Bilevel { data, stride, .. } if default_gray => Kind::Bilevel {
                    data,
                    stride,
                    palette: None,
                },
                Kind::Bilevel { data, stride, .. } if dec == [1.0, 0.0] => Kind::Bilevel {
                    data,
                    stride,
                    palette: Some([255, 0]),
                },
                Kind::Gray8(v) if default_gray => Kind::Gray8(v),
                Kind::Gray8(mut v) if dec == [1.0, 0.0] => {
                    for b in &mut v {
                        *b = 255 - *b;
                    }
                    Kind::Gray8(v)
                }
                Kind::Rgb8(v) if default_rgb => Kind::Rgb8(v),
                _ => return Err("Decode array".into()),
            }
        }
    };
    Ok(Decoded {
        width: width as i32,
        height: height as i32,
        kind,
        interpolate,
    })
}

/// `CFX_AggImageRenderer` for one placement: the image rectangle, the clip,
/// the axis-aligned or 90° stretch, and the composite onto `canvas`.
fn draw(canvas: &mut RgbImage, img: &Decoded, m: M, clip: Rect) -> Result<(), String> {
    let unit = [
        m.apply(0.0, 1.0),
        m.apply(0.0, 0.0),
        m.apply(1.0, 1.0),
        m.apply(1.0, 0.0),
    ];
    let image_rect = FRect::from_points(&unit).outer();
    let mut clip_box = clip;
    clip_box.intersect(&image_rect);
    if clip_box.is_empty() {
        return Ok(());
    }
    // `StartDIBBase`: a huge image (> 60 MB of samples) is always bilinear.
    let bytes = i64::from(img.width)
        * i64::from(img.height)
        * match img.kind {
            Kind::Bilevel { .. } => 0,
            Kind::Gray8(_) => 1,
            Kind::Rgb8(_) => 3,
        };
    let options = Options {
        bilinear: img.interpolate || bytes > 60_000_000,
        no_smoothing: false,
    };
    let source = match &img.kind {
        Kind::Bilevel { data, stride, .. } => Source::Bilevel {
            data,
            stride: *stride,
        },
        Kind::Gray8(v) => Source::Gray8 {
            data: v,
            stride: img.width as usize,
        },
        Kind::Rgb8(v) => Source::Rgb8 {
            data: v,
            stride: 3 * img.width as usize,
        },
    };
    let palette = match &img.kind {
        Kind::Bilevel {
            palette: Some([p0, p1]),
            ..
        } => Some((*p0, *p1)),
        _ => None,
    };
    // `CFX_ImageStretcher::BuildPaletteFrom1BppSource`: the 256-step ramp
    // between the two palette colours, integer arithmetic.
    let map = |v: u8| -> u8 {
        match palette {
            None => v,
            Some((p0, p1)) => {
                (i32::from(p0) + (i32::from(p1) - i32::from(p0)) * i32::from(v) / 255) as u8
            }
        }
    };
    let rotated = (m.b.abs() >= 0.5 || m.a == 0.0) || (m.c.abs() >= 0.5 || m.d == 0.0);
    if rotated {
        if !(m.a.abs() < m.b.abs() / 20.0
            && m.d.abs() < m.c.abs() / 20.0
            && m.a.abs() < 0.5
            && m.d.abs() < 0.5)
        {
            return Err("general affine image placement".into());
        }
        let dest_width = image_rect.width();
        let dest_height = image_rect.height();
        let mut bitmap_clip = clip_box;
        bitmap_clip.offset(-image_rect.left, -image_rect.top);
        let flip_x = m.c > 0.0;
        let flip_y = m.b < 0.0;
        let bitmap_clip = bitmap_clip.swapped_clip_box(dest_width, dest_height, flip_x, flip_y);
        let s = stretch::stretch(
            &source,
            img.width,
            img.height,
            dest_height,
            dest_width,
            bitmap_clip,
            options,
        )
        .ok_or("stretch")?;
        // `ComposeScanlineV`: stretched row `line` → device column.
        let (cw, ch) = (clip_box.width() as usize, clip_box.height() as usize);
        if s.height != cw || s.width != ch {
            return Err("swapped clip size".into());
        }
        for line in 0..cw {
            let dx = clip_box.left as usize + if flip_x { cw - line - 1 } else { line };
            for i in 0..ch {
                let dy = if flip_y {
                    clip_box.top as usize + ch - 1 - i
                } else {
                    clip_box.top as usize + i
                };
                put(
                    canvas,
                    dx,
                    dy,
                    &s.data[(line * s.width + i) * s.channels..][..s.channels],
                    &map,
                );
            }
        }
        return Ok(());
    }
    let mut dest_width = image_rect.width();
    if m.a < 0.0 {
        dest_width = -dest_width;
    }
    let mut dest_height = image_rect.height();
    if m.d > 0.0 {
        dest_height = -dest_height;
    }
    if dest_width == 0 || dest_height == 0 {
        return Ok(());
    }
    let mut bitmap_clip = clip_box;
    bitmap_clip.offset(-image_rect.left, -image_rect.top);
    let s = stretch::stretch(
        &source,
        img.width,
        img.height,
        dest_width,
        dest_height,
        bitmap_clip,
        options,
    )
    .ok_or("stretch")?;
    for row in 0..s.height {
        for col in 0..s.width {
            put(
                canvas,
                clip_box.left as usize + col,
                clip_box.top as usize + row,
                &s.data[(row * s.width + col) * s.channels..][..s.channels],
                &map,
            );
        }
    }
    Ok(())
}

fn put(canvas: &mut RgbImage, x: usize, y: usize, px: &[u8], map: &dyn Fn(u8) -> u8) {
    if x >= canvas.width() as usize || y >= canvas.height() as usize {
        return;
    }
    let p = canvas.get_pixel_mut(x as u32, y as u32);
    if px.len() == 1 {
        let g = map(px[0]);
        *p = image::Rgb([g, g, g]);
    } else {
        *p = image::Rgb([px[0], px[1], px[2]]);
    }
}

fn render_inner(meta: &PdfMeta, index: usize, width: u32, height: u32) -> Result<RgbImage, String> {
    if !enabled() {
        return Err("DOCLING_RS_SCAN_RASTER=pdfium".into());
    }
    if width == 0 || height == 0 || width > 1 << 15 || height > 1 << 15 {
        return Err("bitmap size".into());
    }
    let doc = meta.doc();
    let pid = meta.page_id(index).ok_or("page index")?;
    let page = doc
        .get_object(pid)
        .ok()
        .and_then(|o| o.as_dict().ok())
        .ok_or("page dict")?;
    // Annotations: `FPDF_ANNOT` draws appearance streams and pdfium generates
    // some (Square, Text, …) itself; links draw nothing. Widgets are drawn by
    // the form-fill layer pdfium-render also runs.
    if let Some(annots) = page
        .get(b"Annots")
        .ok()
        .map(|o| deref(doc, o))
        .and_then(|o| o.as_array().ok())
    {
        for a in annots {
            let Some(ad) = as_dict(doc, a) else { continue };
            let hidden = ad
                .get(b"F")
                .ok()
                .and_then(|o| deref(doc, o).as_i64().ok())
                .is_some_and(|f| f & 2 != 0);
            if hidden {
                continue;
            }
            if !name_is(ad.get(b"Subtype").ok().map(|o| deref(doc, o)), b"Link") {
                return Err("annotation".into());
            }
        }
    }

    // pdfium's page geometry: the display box and `/Rotate` fold into
    // `page_matrix_`, the display matrix maps the (rotated) page size onto
    // the bitmap with y flipped (`CPDF_Page::GetDisplayMatrixForFloatRect`).
    let pb = crate::textparse::page_box(doc, pid);
    let geom = meta.geometry(index).ok_or("geometry")?;
    let (l, b, r, t) = (pb.l, pb.b, pb.l + pb.w, pb.b + pb.h);
    let page_matrix = match geom.rotation {
        90 => M {
            a: 0.0,
            b: -1.0,
            c: 1.0,
            d: 0.0,
            e: -b,
            f: r,
        },
        180 => M {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: r,
            f: t,
        },
        270 => M {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            e: t,
            f: -l,
        },
        _ => M {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: -l,
            f: -b,
        },
    };
    let (pw, ph) = (geom.width, geom.height);
    if pw == 0.0 || ph == 0.0 {
        return Err("page size".into());
    }
    let (wf, hf) = (width as f32, height as f32);
    // rotation 0: x0 = 0, y0 = H (FloatRect.top), x1 = 0, y1 = 0, x2 = W, y2 = H.
    let display = M {
        a: wf / pw,
        b: 0.0 / pw,
        c: 0.0 / ph,
        d: (0.0 - hf) / ph,
        e: 0.0,
        f: hf,
    };
    let obj2dev = page_matrix.then(display);
    let device = Rect::new(0, 0, width as i32, height as i32);

    let page_res = doc.get_page_resources(pid).ok().and_then(|(inline, ids)| {
        inline.or_else(|| ids.into_iter().find_map(|id| doc.get_dictionary(id).ok()))
    });
    let ctx = Ctx {
        doc,
        obj2dev,
        res: page_res,
        page_res,
        device,
        depth: 0,
    };
    let content = doc.get_page_content(pid);
    let mut walker = Walker {
        draws: Vec::new(),
        ops: 0,
    };
    walker.walk(
        &ctx,
        &content,
        GState {
            ctm: M::ID,
            clip: device,
            text_render: 0,
        },
    )?;
    if walker.draws.is_empty() {
        return Err("no image on the page".into());
    }

    let mut canvas = RgbImage::from_pixel(width, height, image::Rgb([255, 255, 255]));
    let mut cache: HashMap<ObjectId, Decoded> = HashMap::new();
    for dr in &walker.draws {
        if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(dr.stream) {
            let stream = doc
                .get_object(dr.stream)
                .ok()
                .and_then(|o| o.as_stream().ok())
                .ok_or("image stream")?;
            let decoded = crate::timing::timed("raster.decode", || {
                decode_image(&ctx, stream, (width, height))
            })?;
            e.insert(decoded);
        }
        let img = &cache[&dr.stream];
        crate::timing::timed("raster.stretch", || {
            draw(&mut canvas, img, dr.matrix, dr.clip)
        })?;
    }
    Ok(canvas)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn fixture(rel: &str) -> Vec<u8> {
        std::fs::read(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    /// The scanned fixtures this module must take over from pdfium.
    const IMAGE_ONLY: &[&str] = &[
        "tests/data/scanned/sources/ocr_test.pdf",
        "tests/data/scanned/sources/ocr_test_rotated_90.pdf",
        "tests/data/scanned/sources/ocr_test_rotated_180.pdf",
        "tests/data/scanned/sources/ocr_test_rotated_270.pdf",
        "tests/data/scanned/sources/ocr_test_raster.pdf",
        "tests/data/scanned/sources/ocr_test_raster_rot_90.pdf",
        "tests/data/scanned/sources/ocr_test_raster_rot_180.pdf",
        "tests/data/scanned/sources/ocr_test_raster_rot_270.pdf",
        "tests/data/scanned/sources/nemotron_multipage.pdf",
        "tests/data/scanned/sources/scanned_chart_table.pdf",
        "tests/data/pdf/sources/docling-rs-demotion-repro.pdf",
    ];

    /// The two render sizes the pipeline asks for: the OCR/TableFormer bitmap
    /// (1.5 × the 2.0 render scale, rounded) and the layout image (1.5 ×,
    /// ceiled) — the same arithmetic as `pdfium_backend::extract_page`.
    fn sizes(w: f32, h: f32) -> [(u32, u32); 2] {
        [
            (
                (w * 3.0).round().max(1.0) as u32,
                (h * 3.0).round().max(1.0) as u32,
            ),
            (
                (w * 1.5).ceil().max(1.0) as u32,
                (h * 1.5).ceil().max(1.0) as u32,
            ),
        ]
    }

    #[test]
    fn born_digital_pages_are_declined() {
        let meta = PdfMeta::open(&fixture("tests/data/pdf/sources/2206.01062.pdf")).unwrap();
        assert!(render(&meta, 0, 612, 792).is_none());
        // Text over a scan (skipped_2pages p2) and the ICC-profiled scan.
        let meta = PdfMeta::open(&fixture(
            "tests/data/scanned/sources/sample_with_rotation_mismatch.pdf",
        ))
        .unwrap();
        assert!(render(&meta, 0, 842, 595).is_none());
    }

    #[test]
    fn image_only_pages_render() {
        for rel in IMAGE_ONLY {
            let meta = PdfMeta::open(&fixture(rel)).unwrap();
            let g = meta.geometry(0).unwrap();
            let (w, h) = sizes(g.width, g.height)[1];
            let img = render(&meta, 0, w, h).unwrap_or_else(|| panic!("{rel} declined"));
            assert_eq!((img.width(), img.height()), (w, h), "{rel}");
            // A scan is mostly paper: white dominates, but not everything is.
            let dark = img.pixels().filter(|p| p[0] < 128).count();
            assert!(
                dark > 0 && dark < (w * h / 2) as usize,
                "{rel}: {dark} dark px"
            );
        }
    }

    /// The oracle: pdfium's own bitmap, byte for byte, on every image-only
    /// fixture page at both pipeline sizes. Skipped without `libpdfium`.
    #[test]
    fn matches_pdfium_on_the_scanned_fixtures() {
        let pdfium = match crate::pdfium_backend::bind_for_tests() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("pdfium not installed — skipping the raster oracle ({e:?})");
                return;
            }
        };
        use pdfium_render::prelude::*;
        let mut pages = 0;
        let mut failures = Vec::new();
        for rel in IMAGE_ONLY {
            let bytes = fixture(rel);
            let meta = PdfMeta::open(&bytes).unwrap();
            let doc = pdfium.load_pdf_from_byte_slice(&bytes, None).unwrap();
            for (i, page) in doc.pages().iter().enumerate() {
                let g = meta.geometry(i).unwrap();
                for (w, h) in sizes(g.width, g.height) {
                    let want = page
                        .render_with_config(
                            &PdfRenderConfig::new()
                                .set_target_width(w as i32)
                                .set_target_height(h as i32),
                        )
                        .unwrap()
                        .as_image()
                        .into_rgb8();
                    let Some(got) = render(&meta, i, w, h) else {
                        failures.push(format!("{rel} p{} @{w}x{h}: declined", i + 1));
                        continue;
                    };
                    pages += 1;
                    if got.as_raw() != want.as_raw() {
                        let diff = got
                            .as_raw()
                            .iter()
                            .zip(want.as_raw())
                            .filter(|(a, b)| a != b)
                            .count();
                        let max = got
                            .as_raw()
                            .iter()
                            .zip(want.as_raw())
                            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                            .max()
                            .unwrap_or(0);
                        failures.push(format!(
                            "{rel} p{} @{w}x{h}: {diff} of {} bytes differ (max |Δ| {max})",
                            i + 1,
                            want.len()
                        ));
                        if std::env::var_os("DOCLING_RS_RASTER_DUMP").is_some() {
                            let stem = std::path::Path::new(rel)
                                .file_stem()
                                .unwrap()
                                .to_string_lossy()
                                .into_owned();
                            let _ = got.save(format!("/tmp/{stem}.p{}.{w}.rust.png", i + 1));
                            let _ = want.save(format!("/tmp/{stem}.p{}.{w}.pdfium.png", i + 1));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        assert!(
            pages >= 2 * IMAGE_ONLY.len(),
            "{pages} page renders compared"
        );
    }

    fn describe(doc: &Document, res: Option<&Dictionary>, content: &[u8], depth: usize) {
        let pad = "  ".repeat(depth);
        let ops = lopdf::content::Content::decode(content).expect("content");
        let mut shown = std::collections::BTreeMap::<String, usize>::new();
        for op in &ops.operations {
            *shown.entry(op.operator.clone()).or_default() += 1;
            if matches!(
                op.operator.as_str(),
                "cm" | "Do" | "re" | "gs" | "Tr" | "W" | "n"
            ) {
                println!("{pad}{} {:?}", op.operator, op.operands);
            }
        }
        println!("{pad}ops: {shown:?}");
        let Some(res) = res else { return };
        let Some(xobjs) = res.get(b"XObject").ok().and_then(|o| as_dict(doc, o)) else {
            return;
        };
        for (name, obj) in xobjs.iter() {
            let Some(stream) = deref(doc, obj).as_stream().ok() else {
                continue;
            };
            let mut d = stream.dict.clone();
            d.remove(b"Length");
            println!("{pad}/{} {:?}", String::from_utf8_lossy(name), d);
            if name_is(d.get(b"Subtype").ok(), b"Form") {
                let data = stream.decompressed_content().unwrap_or_default();
                let fres = d.get(b"Resources").ok().and_then(|o| as_dict(doc, o));
                describe(doc, fres, &data, depth + 1);
            } else if let Ok(Object::Reference(cs)) = d.get(b"ColorSpace") {
                println!("{pad}  ColorSpace -> {:?}", doc.get_object(*cs).ok());
            }
        }
    }

    /// Development aid: dump the content streams and XObjects of the scanned
    /// fixtures (`cargo test -p docling-pdf --lib raster::tests::dump -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn dump() {
        for rel in IMAGE_ONLY {
            let bytes = fixture(rel);
            let Some(doc) = crate::textparse::load_document(&bytes) else {
                continue;
            };
            let mut pages: Vec<_> = doc.get_pages().into_iter().collect();
            pages.sort_by_key(|(n, _)| *n);
            for (n, pid) in pages.into_iter().take(3) {
                println!("=== {rel} p{n}");
                let content = doc.get_page_content(pid);
                let res = doc.get_page_resources(pid).ok().and_then(|(inline, ids)| {
                    inline.or_else(|| ids.into_iter().find_map(|id| doc.get_dictionary(id).ok()))
                });
                describe(&doc, res, &content, 0);
            }
        }
    }
}
