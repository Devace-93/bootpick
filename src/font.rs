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

/// Draws text with its top-left corner at (x, y). Returns the drawn width.
pub fn draw(c: &mut Canvas, font: &FontRef<'static>, size: f32, x: i32, y: i32, color: Color, text: &str) -> i32 {
    let sf = font.as_scaled(PxScale::from(size));
    let base = color.packed() & 0x00ff_ffff;
    let mut caret = x as f32;
    let baseline = y as f32 + sf.ascent();
    let mut prev = None;
    for ch in text.chars() {
        let id = sf.glyph_id(ch);
        if let Some(p) = prev {
            caret += sf.kern(p, id);
        }
        let glyph = id.with_scale_and_position(PxScale::from(size), point(caret, baseline));
        if let Some(og) = font.outline_glyph(glyph) {
            let b = og.px_bounds();
            let ox = b.min.x as i32;
            let oy = b.min.y as i32;
            og.draw(|gx, gy, cov| {
                if cov <= 0.002 {
                    return;
                }
                let a = (color.a as f32 * cov.min(1.0)) as u32;
                c.blend_px(ox + gx as i32, oy + gy as i32, (a << 24) | base);
            });
        }
        caret += sf.h_advance(id);
        prev = Some(id);
    }
    libm::ceilf(caret - x as f32) as i32
}

/// Draws text horizontally centred on `cx`.
pub fn draw_centered(c: &mut Canvas, font: &FontRef<'static>, size: f32, cx: i32, y: i32, color: Color, text: &str) -> i32 {
    let w = measure(font, size, text);
    draw(c, font, size, cx - w / 2, y, color, text)
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
