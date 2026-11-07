//! Image import (logos, backgrounds) into the BMP formats the boot menu reads,
//! and BMP decoding for thumbnails.

use bootpick_core::bmp;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, RgbaImage};
use std::path::Path;

/// 32-bit BGRA BMP with a V4 header (BI_BITFIELDS) so alpha survives.
pub fn bmp32(img: &RgbaImage) -> Vec<u8> {
    let (w, h) = img.dimensions();
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in (0..h).rev() {
        for x in 0..w {
            let p = img.get_pixel(x, y).0;
            data.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    let mut dib = Vec::new();
    for v in [108u32] {
        dib.extend_from_slice(&v.to_le_bytes());
    }
    dib.extend_from_slice(&(w as i32).to_le_bytes());
    dib.extend_from_slice(&(h as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&3u32.to_le_bytes());
    dib.extend_from_slice(&(data.len() as u32).to_le_bytes());
    dib.extend_from_slice(&2835i32.to_le_bytes());
    dib.extend_from_slice(&2835i32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    for m in [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000] {
        dib.extend_from_slice(&m.to_le_bytes());
    }
    dib.extend_from_slice(b"sRGB");
    dib.extend_from_slice(&[0u8; 36]);
    dib.extend_from_slice(&[0u8; 12]);
    assert_eq!(dib.len(), 108);
    let off = 14 + dib.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(off + data.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&off.to_le_bytes());
    out.extend_from_slice(&dib);
    out.extend_from_slice(&data);
    out
}

/// 24-bit BGR BMP (backgrounds: smaller, no alpha needed).
pub fn bmp24(img: &image::RgbImage) -> Vec<u8> {
    let (w, h) = img.dimensions();
    let stride = ((w * 3 + 3) & !3) as usize;
    let mut data = Vec::with_capacity(stride * h as usize);
    for y in (0..h).rev() {
        let start = data.len();
        for x in 0..w {
            let p = img.get_pixel(x, y).0;
            data.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        data.resize(start + stride, 0);
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(&data);
    out
}

fn open(path: &Path) -> Result<DynamicImage, String> {
    image::open(path).map_err(|e| format!("{}: {}", path.display(), e))
}

/// A logo: trimmed of transparent borders, fitted into 256x256 with a small
/// margin, centred.
pub fn import_logo(path: &Path) -> Result<Vec<u8>, String> {
    let img = open(path)?.to_rgba8();
    let (w, h) = img.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p.0[3] > 8 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    let cropped = if x1 >= x0 && y1 >= y0 { DynamicImage::ImageRgba8(img).crop_imm(x0, y0, x1 - x0 + 1, y1 - y0 + 1) } else { DynamicImage::ImageRgba8(img) };
    let inner = 236;
    let fitted = cropped.resize(inner, inner, FilterType::Lanczos3).to_rgba8();
    let mut out = RgbaImage::new(256, 256);
    let (fw, fh) = fitted.dimensions();
    image::imageops::overlay(&mut out, &fitted, ((256 - fw) / 2) as i64, ((256 - fh) / 2) as i64);
    Ok(bmp32(&out))
}

/// A wallpaper scaled to cover 1920x1080 and cropped centred.
pub fn import_background(path: &Path) -> Result<Vec<u8>, String> {
    let img = open(path)?;
    let filled = img.resize_to_fill(1920, 1080, FilterType::Lanczos3).to_rgb8();
    Ok(bmp24(&filled))
}

/// Decodes one of our BMPs into RGBA (straight alpha) for a thumbnail.
pub fn decode_bmp_rgba(data: &[u8], max_w: u32, max_h: u32) -> Option<(usize, usize, Vec<u8>)> {
    let im = bmp::decode(data)?;
    let mut rgba = RgbaImage::new(im.w as u32, im.h as u32);
    for (i, p) in im.px.iter().enumerate() {
        let (x, y) = ((i % im.w) as u32, (i / im.w) as u32);
        rgba.put_pixel(x, y, image::Rgba([(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]));
    }
    let dynimg = DynamicImage::ImageRgba8(rgba);
    let (w, h) = dynimg.dimensions();
    let dynimg = if w > max_w || h > max_h { dynimg.resize(max_w, max_h, FilterType::Triangle) } else { dynimg };
    let (w, h) = dynimg.dimensions();
    Some((w as usize, h as usize, dynimg.to_rgba8().into_raw()))
}

/// A file-name friendly slug ("My photo.jpg" -> "my-photo").
pub fn slug(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut s: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').chars().take(40).collect::<String>();
    if s.is_empty() { "image".into() } else { s }
}
