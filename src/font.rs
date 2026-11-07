//! Text rendering with ab_glyph (TrueType, anti-aliased).

use crate::gfx::{Canvas, Color};
use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};

pub struct Fonts {
    pub bold: FontRef<'static>,
    pub regular: FontRef<'static>,
}

static BOLD_TTF: &[u8] = include_bytes!("../assets/DejaVuSans-Bold.ttf");
static REGULAR_TTF: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");

impl Fonts {
    pub fn load() -> Option<Fonts> {
        Some(Fonts {
            bold: FontRef::try_from_slice(BOLD_TTF).ok()?,
            regular: FontRef::try_from_slice(REGULAR_TTF).ok()?,
        })
    }
}

/// Width in pixels of `text` at `size`.
pub fn measure(font: &FontRef<'static>, size: f32, text: &str) -> i32 {
    let sf = font.as_scaled(PxScale::from(size));
    let mut w = 0.0f32;
    let mut prev = None;
    for ch in text.chars() {
        let id = sf.glyph_id(ch);
        if let Some(p) = prev {
            w += sf.kern(p, id);
        }
        w += sf.h_advance(id);
        prev = Some(id);
    }
    libm::ceilf(w) as i32
}

pub fn line_height(font: &FontRef<'static>, size: f32) -> i32 {
    let sf = font.as_scaled(PxScale::from(size));
    libm::ceilf(sf.ascent() - sf.descent()) as i32
}

/// Truncates with an ellipsis so the text fits in `max_w`.
pub fn fit(font: &FontRef<'static>, size: f32, text: &str, max_w: i32) -> alloc::string::String {
    if measure(font, size, text) <= max_w {
        return text.into();
    }
    let mut s: alloc::string::String = text.into();
    while !s.is_empty() {
        s.pop();
        let t = alloc::format!("{}…", s.trim_end());
        if measure(font, size, &t) <= max_w {
            return t;
        }
    }
    "…".into()
}

// ----- glyph cache -----------------------------------------------------------
//
// Rasterising outlines is by far the most expensive part of a frame in
// firmware (no SIMD-friendly rasteriser, hundreds of glyphs per screen). Each
// glyph is rasterised once per (font, size) at an integer pixel origin and then
// only blended on later frames.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cell::RefCell;

struct Bitmap {
    ox: i32,
    oy: i32,
    w: usize,
    h: usize,
    cov: Vec<u8>,
}

#[derive(Default)]
pub struct GlyphCache {
    map: RefCell<BTreeMap<(bool, u32, u16), Option<Bitmap>>>,
}

fn size_key(size: f32) -> u32 {
    libm::roundf(size * 4.0) as u32
}

impl Fonts {
    fn font(&self, bold: bool) -> &FontRef<'static> {
        if bold { &self.bold } else { &self.regular }
    }
}

/// Cached version of [`draw`]: same metrics, glyphs snapped to whole pixels.
pub fn draw_cached(c: &mut Canvas, fonts: &Fonts, cache: &GlyphCache, bold: bool, size: f32, x: i32, y: i32, color: Color, text: &str) -> i32 {
    let font = fonts.font(bold);
    let sf = font.as_scaled(PxScale::from(size));
    let ascent = sf.ascent();
    let base = color.packed() & 0x00ff_ffff;
    let key_size = size_key(size);
    let mut caret = x as f32;
    let mut prev = None;
    let mut map = cache.map.borrow_mut();
    for ch in text.chars() {
        let id = sf.glyph_id(ch);
        if let Some(p) = prev {
            caret += sf.kern(p, id);
        }
        let entry = map.entry((bold, key_size, id.0)).or_insert_with(|| {
            let glyph = id.with_scale_and_position(PxScale::from(size), point(0.0, ascent));
            font.outline_glyph(glyph).map(|og| {
                let b = og.px_bounds();
                let w = (b.max.x - b.min.x).max(0.0) as usize + 1;
                let h = (b.max.y - b.min.y).max(0.0) as usize + 1;
                let mut cov = alloc::vec![0u8; w * h];
                og.draw(|gx, gy, v| {
                    let i = gy as usize * w + gx as usize;
                    if i < cov.len() {
                        cov[i] = (v.min(1.0) * 255.0) as u8;
                    }
                });
                Bitmap { ox: b.min.x as i32, oy: b.min.y as i32, w, h, cov }
            })
        });
        if let Some(bm) = entry {
            let gx0 = libm::roundf(caret) as i32 + bm.ox;
            let gy0 = y + bm.oy;
            for yy in 0..bm.h {
                for xx in 0..bm.w {
                    let cv = bm.cov[yy * bm.w + xx] as u32;
                    if cv == 0 {
                        continue;
                    }
                    let a = cv * color.a as u32 / 255;
                    c.blend_px(gx0 + xx as i32, gy0 + yy as i32, (a << 24) | base);
                }
            }
        }
        caret += sf.h_advance(id);
        prev = Some(id);
    }
    libm::ceilf(caret - x as f32) as i32
}

pub fn draw_centered_cached(c: &mut Canvas, fonts: &Fonts, cache: &GlyphCache, bold: bool, size: f32, cx: i32, y: i32, color: Color, text: &str) -> i32 {
    let w = measure(fonts.font(bold), size, text);
    draw_cached(c, fonts, cache, bold, size, cx - w / 2, y, color, text)
}
