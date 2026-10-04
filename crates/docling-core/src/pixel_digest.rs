//! Content digest of a picture's pixels, as docling names exported image
//! assets.
//!
//! docling-core's `PictureItem._image_to_hexhash` hashes `PIL img.tobytes()` —
//! the *decoded* pixel buffer in the image's PIL mode — not the encoded file,
//! so the `image_{NNNNNN}_{sha256}.png` names of a referenced-image export are
//! independent of how the PNG was compressed. For a PNG source that buffer is
//! fully determined by the file: Pillow's `PngImagePlugin` maps every
//! (bit depth, colour type) pair to one mode/rawmode (`_MODES`), and the
//! backends' PNG round-trip (`save(format="PNG")` + reopen) keeps that mode.
//! [`pil_png_digest`] reproduces the same bytes from the raw (untransformed)
//! samples. Other encodings (JPEG's IDCT, GIF, …) are not reproduced and keep
//! the encoded-bytes digest.

use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The asset digest of an embedded image: docling's pixel hash when the
/// image is a PNG we can decode, else the hash of the encoded bytes.
pub(crate) fn image_digest(data: &[u8]) -> String {
    pil_png_digest(data).unwrap_or_else(|| sha256_hex(data))
}

/// `sha256(PIL.Image.open(png).tobytes())`, or `None` when `data` is not a
/// decodable PNG.
pub(crate) fn pil_png_digest(data: &[u8]) -> Option<String> {
    pil_png_bytes(data).map(|b| sha256_hex(&b))
}

/// The bytes `PIL.Image.open(png).tobytes()` returns for a PNG.
///
/// Pillow's `_MODES` table, per (bit depth, colour type):
/// gray 1 → `1` (rows bit-packed MSB-first, byte-padded: the PNG scanline
/// as-is); gray 2/4 → `L` (unpacked, scaled ×0x55 / ×0x11); gray 16 → `I;16`
/// (little-endian); RGB/RGBA/LA 16 → 8-bit `RGB`/`RGBA` (the MSB of each
/// sample) — 16-bit gray+alpha becomes `RGBA` (`LA;16B`: L copied to R, G,
/// B); palette 1/2/4/8 → `P` (one index byte per pixel). `tRNS` only sets
/// `info["transparency"]`, never the mode, so it does not enter the bytes.
fn pil_png_bytes(data: &[u8]) -> Option<Vec<u8>> {
    use png::{BitDepth, ColorType};
    if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::IDENTITY);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let line = info.line_size;
    let rows = buf.chunks_exact(line).take(h);
    let depth = info.bit_depth as u8;
    let out = match (info.color_type, info.bit_depth) {
        // `1`: the packed scanlines are PIL's packed rows.
        (ColorType::Grayscale, BitDepth::One) => rows.flat_map(|r| r.iter().copied()).collect(),
        (ColorType::Grayscale, BitDepth::Two | BitDepth::Four) => {
            let scale = if depth == 2 { 0x55 } else { 0x11 };
            rows.flat_map(|r| unpack(r, depth, w).map(move |v| v * scale))
                .collect()
        }
        (ColorType::Indexed, BitDepth::One | BitDepth::Two | BitDepth::Four) => {
            rows.flat_map(|r| unpack(r, depth, w)).collect()
        }
        // `I;16`: native little-endian 16-bit gray.
        (ColorType::Grayscale, BitDepth::Sixteen) => rows
            .flat_map(|r| r[..2 * w].chunks_exact(2).flat_map(|s| [s[1], s[0]]))
            .collect(),
        (ColorType::GrayscaleAlpha, BitDepth::Sixteen) => rows
            .flat_map(|r| {
                r[..4 * w]
                    .chunks_exact(4)
                    .flat_map(|s| [s[0], s[0], s[0], s[2]])
            })
            .collect(),
        // 16-bit RGB/RGBA: keep the most significant byte of each sample.
        (ColorType::Rgb | ColorType::Rgba, BitDepth::Sixteen) => {
            let n = info.color_type.samples() * w * 2;
            rows.flat_map(|r| r[..n].iter().step_by(2).copied())
                .collect()
        }
        // 8-bit L / LA / RGB / RGBA / P: the samples are the mode's bytes.
        (_, BitDepth::Eight) => {
            let n = info.color_type.samples() * w;
            rows.flat_map(|r| r[..n].iter().copied()).collect()
        }
        _ => return None,
    };
    Some(out)
}

/// The first `w` sub-byte samples (`depth` 1, 2 or 4 bits, MSB-first) of a
/// packed scanline, one value per byte.
fn unpack(row: &[u8], depth: u8, w: usize) -> impl Iterator<Item = u8> + '_ {
    let per = 8 / depth as usize;
    let mask = (1u16 << depth) as u8 - 1;
    (0..w).map(move |i| {
        let shift = 8 - depth as usize * (i % per + 1);
        (row[i / per] >> shift) & mask
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal PNG of `w`×`h` with the given IHDR depth/colour type, raw
    /// (filter-0) scanlines and an optional PLTE.
    fn png(w: u32, h: u32, depth: u8, color: u8, rows: &[&[u8]], plte: Option<&[u8]>) -> Vec<u8> {
        let mut enc_buf = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut enc_buf, w, h);
            enc.set_depth(png::BitDepth::from_u8(depth).unwrap());
            enc.set_color(png::ColorType::from_u8(color).unwrap());
            if let Some(p) = plte {
                enc.set_palette(p.to_vec());
            }
            let mut wr = enc.write_header().unwrap();
            let data: Vec<u8> = rows.iter().flat_map(|r| r.iter().copied()).collect();
            wr.write_image_data(&data).unwrap();
        }
        enc_buf
    }

    // Expected bytes cross-checked against Pillow 12.3: `Image.open(f).tobytes()`,
    // identical again after the backends' `save(format="PNG")` + reopen.

    #[test]
    fn rgb8_is_the_raw_samples() {
        let f = png(2, 1, 8, 2, &[&[1, 2, 3, 4, 5, 6]], None);
        assert_eq!(pil_png_bytes(&f).unwrap(), vec![1, 2, 3, 4, 5, 6]);
        assert_ne!(image_digest(&f), sha256_hex(&f));
    }

    #[test]
    fn sub_byte_gray_is_scaled_to_l() {
        let f = png(3, 1, 2, 0, &[&[0b00_01_10_11]], None);
        assert_eq!(pil_png_bytes(&f).unwrap(), vec![0x00, 0x55, 0xaa]);
        let f = png(3, 1, 4, 0, &[&[0x0f, 0x10]], None);
        assert_eq!(pil_png_bytes(&f).unwrap(), vec![0x00, 0xff, 0x11]);
    }

    #[test]
    fn one_bit_gray_stays_packed_and_palette_unpacks_to_indices() {
        let f = png(10, 1, 1, 0, &[&[0b1010_0000, 0b1100_0000]], None);
        assert_eq!(pil_png_bytes(&f).unwrap(), vec![0b1010_0000, 0b1100_0000]);
        let f = png(3, 1, 2, 3, &[&[0b11_01_00_00]], Some(&[0; 12]));
        assert_eq!(pil_png_bytes(&f).unwrap(), vec![3, 1, 0]);
    }

    #[test]
    fn sixteen_bit_modes_follow_pillow() {
        // I;16 little-endian.
        let f = png(1, 1, 16, 0, &[&[0x12, 0x34]], None);
        assert_eq!(pil_png_bytes(&f).unwrap(), vec![0x34, 0x12]);
        // RGB;16B → RGB (MSBs).
        let f = png(1, 1, 16, 2, &[&[1, 2, 3, 4, 5, 6]], None);
        assert_eq!(pil_png_bytes(&f).unwrap(), vec![1, 3, 5]);
        // LA;16B → RGBA.
        let f = png(1, 1, 16, 4, &[&[7, 8, 9, 10]], None);
        assert_eq!(pil_png_bytes(&f).unwrap(), vec![7, 7, 7, 9]);
    }

    #[test]
    fn non_png_keeps_the_encoded_bytes_digest() {
        assert_eq!(image_digest(b"\xff\xd8\xff"), sha256_hex(b"\xff\xd8\xff"));
    }

    #[test]
    fn matches_docling_for_the_issue_527_fixture_image() {
        // word/media/image1.png of rich_merged_span_list_image_nested.docx;
        // docling names it image_000000_fa7b78cc….png.
        const HEX: &str = "89504e470d0a1a0a0000000d49484452000000080000000808020000004b6d29dc\
                           0000000f49444154789c6368c001188696040082f360019cee0f2400000000\
                           49454e44ae426082";
        let f: Vec<u8> = (0..HEX.len() / 2)
            .map(|i| u8::from_str_radix(&HEX[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        assert_eq!(
            image_digest(&f),
            "fa7b78cc215df21d7ce54d8c3c6637c326dab95c10fbc12263101365973f4268"
        );
    }
}
