//! Minimal BMP decoder (24-bit BGR, 32-bit BGRA, BI_RGB or BI_BITFIELDS).

use crate::gfx::Image;

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn mask_shift(mask: u32) -> (u32, u32) {
    if mask == 0 {
        return (0, 0);
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).trailing_ones();
    (shift, bits)
}

pub fn decode(data: &[u8]) -> Option<Image> {
    if data.len() < 54 || &data[0..2] != b"BM" {
        return None;
    }
    let offset = u32le(data, 10) as usize;
    let dib = u32le(data, 14) as usize;
    let w = u32le(data, 18) as i32;
    let h_raw = u32le(data, 22) as i32;
    let bpp = u16le(data, 28);
    let compression = u32le(data, 30);
    if w <= 0 || h_raw == 0 || w > 16384 || h_raw.abs() > 16384 {
        return None;
    }
    let top_down = h_raw < 0;
    let h = h_raw.abs() as usize;
    let w = w as usize;
    if !(bpp == 24 || bpp == 32) || !(compression == 0 || compression == 3) {
        return None;
    }
    let (mut rm, mut gm, mut bm, mut am) = (0x00ff_0000u32, 0x0000_ff00, 0x0000_00ff, 0xff00_0000);
    if compression == 3 {
        let base = 14 + 40;
        if dib >= 52 {
            rm = u32le(data, base);
            gm = u32le(data, base + 4);
            bm = u32le(data, base + 8);
        } else if data.len() >= base + 12 {
            rm = u32le(data, base);
            gm = u32le(data, base + 4);
            bm = u32le(data, base + 8);
        }
        am = if dib >= 56 { u32le(data, base + 12) } else { 0 };
    } else if bpp == 24 {
        am = 0;
    }
    let stride = ((bpp as usize * w + 31) / 32) * 4;
    if data.len() < offset + stride * h {
        return None;
    }
    let (rs, rb) = mask_shift(rm);
    let (gs, gb) = mask_shift(gm);
    let (bs, bb) = mask_shift(bm);
    let (as_, ab) = mask_shift(am);
    let norm = |v: u32, bits: u32| -> u32 {
        if bits == 0 {
            0
        } else if bits >= 8 {
            v >> (bits - 8)
        } else {
            (v << (8 - bits)) | (v >> (2 * bits).saturating_sub(8))
        }
    };
    let mut img = Image::new(w, h);
    let mut any_alpha = false;
    for row in 0..h {
        let src_row = if top_down { row } else { h - 1 - row };
        let base = offset + src_row * stride;
        for x in 0..w {
            let (r, g, b, a) = if bpp == 24 {
                let o = base + x * 3;
                (data[o + 2] as u32, data[o + 1] as u32, data[o] as u32, 255)
            } else {
                let v = u32le(data, base + x * 4);
                let a = if am == 0 { 255 } else { norm((v & am) >> as_, ab) };
                (
                    norm((v & rm) >> rs, rb),
                    norm((v & gm) >> gs, gb),
                    norm((v & bm) >> bs, bb),
                    a,
                )
            };
            if a != 255 {
                any_alpha = true;
            }
            img.px[row * w + x] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }
    // Some writers emit 32-bit BI_RGB with an all-zero alpha channel.
    if bpp == 32 && am != 0 && !any_alpha {
        // all 255 -> fine
    } else if bpp == 32 && am != 0 {
        let all_zero = img.px.iter().all(|p| p >> 24 == 0);
        if all_zero {
            img.px.iter_mut().for_each(|p| *p |= 0xff00_0000);
        }
    }
    Some(img)
}
