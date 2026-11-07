//! Text engine: font fallback, shaping (rustybuzz) and cached glyph bitmaps.
//!
//! * DejaVu Sans (regular + bold) is built in and covers Latin, Greek and
//!   Cyrillic. Extra fonts (CJK, Devanagari, Thai, Arabic subsets) are added at
//!   run time; each character uses the first font in the fallback chain that
//!   has it, with the active language's font first.
//! * Every run is shaped by rustybuzz, so scripts that need it (Devanagari
//!   conjuncts, Thai marks, Arabic joining) render correctly. A line whose
//!   first strong character is right-to-left lays its runs out right to left.
//! * Shaped lines and rasterised glyphs are cached: the UEFI target has no
//!   FPU, so floating point work must not be repeated every frame.

use crate::gfx::{Canvas, Color};
use ab_glyph::{point, Font, FontRef, GlyphId, PxScale};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

static DEJAVU: &[u8] = include_bytes!("../../assets/DejaVuSans.ttf");
static DEJAVU_BOLD: &[u8] = include_bytes!("../../assets/DejaVuSans-Bold.ttf");

struct Face {
    name: String,
    bold: bool,
    ab: FontRef<'static>,
    rb: rustybuzz::Face<'static>,
    upem: f32,
    /// ascent - descent in font units (ab_glyph's PxScale is relative to it).
    height_units: f32,
}

struct Bitmap {
    ox: i32,
    oy: i32,
    w: usize,
    h: usize,
    cov: Vec<u8>,
}

#[derive(Clone, Copy)]
struct PlacedGlyph {
    face: u16,
    gid: u16,
    /// Pen position in pixels from the start of the line.
    x: f32,
    /// Vertical offset in pixels, positive up.
    y: f32,
}

struct Shaped {
    width: i32,
    glyphs: Vec<PlacedGlyph>,
}

pub struct TextEngine {
    faces: Vec<Face>,
    regular_chain: Vec<usize>,
    bold_chain: Vec<usize>,
    /// Same fonts, DejaVu first (for Latin, Greek, Cyrillic, digits...).
    regular_latin: Vec<usize>,
    bold_latin: Vec<usize>,
    preferred: &'static [&'static str],
    glyphs: RefCell<BTreeMap<(u16, u32, u16), Option<Rc<Bitmap>>>>,
    shaped: RefCell<BTreeMap<(String, u32, bool), Rc<Shaped>>>,
}

fn size_key(size: f32) -> u32 {
    libm::roundf(size * 4.0) as u32
}

fn is_rtl_char(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

/// Scripts DejaVu does not cover well: their own fonts are tried first.
fn is_complex_script(c: char) -> bool {
    matches!(c as u32,
        0x0600..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF   // Arabic
        | 0x0900..=0x097F | 0xA8E0..=0xA8FF                      // Devanagari
        | 0x0E00..=0x0E7F                                        // Thai
        | 0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF    // Hangul
        | 0x2E80..=0x2FFF | 0x3000..=0x30FF | 0x3400..=0x4DBF    // CJK punctuation, kana
        | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)   // CJK ideographs, fullwidth
}

/// Characters that should stay in the current run (spaces, punctuation,
/// joiners and combining marks).
fn is_neutral(c: char) -> bool {
    c.is_whitespace()
        || matches!(c as u32, 0x200B..=0x200F | 0x0300..=0x036F | 0xFE00..=0xFE0F)
        || (!c.is_alphanumeric() && (c as u32) < 0x2000 && !is_rtl_char(c))
}

impl TextEngine {
    pub fn new() -> TextEngine {
        let mut t = TextEngine {
            faces: Vec::new(),
            regular_chain: Vec::new(),
            bold_chain: Vec::new(),
            regular_latin: Vec::new(),
            bold_latin: Vec::new(),
            preferred: &[],
            glyphs: RefCell::new(BTreeMap::new()),
            shaped: RefCell::new(BTreeMap::new()),
        };
        t.add_static("dejavu-regular", DEJAVU, false);
        t.add_static("dejavu-bold", DEJAVU_BOLD, true);
        t.rebuild_chains();
        t
    }

    fn add_static(&mut self, name: &str, bytes: &'static [u8], bold: bool) -> bool {
        let (Ok(ab), Some(rb)) = (FontRef::try_from_slice(bytes), rustybuzz::Face::from_slice(bytes, 0)) else {
            return false;
        };
        let upem = ab.units_per_em().unwrap_or(1000.0);
        let height_units = ab.ascent_unscaled() - ab.descent_unscaled();
        self.faces.push(Face { name: String::from(name), bold, ab, rb, upem, height_units });
        true
    }

    /// Adds a fallback font (file name decides weight: contains "bold").
    /// The bytes are kept for the lifetime of the program.
    pub fn add_font(&mut self, name: &str, bytes: Vec<u8>) -> bool {
        let bold = name.to_ascii_lowercase().contains("bold");
        let leaked: &'static [u8] = alloc::boxed::Box::leak(bytes.into_boxed_slice());
        let ok = self.add_static(name, leaked, bold);
        self.rebuild_chains();
        ok
    }

    /// Font name prefixes to prefer for a language (CJK variants differ).
    pub fn set_language(&mut self, code: &str) {
        self.preferred = match crate::langs::base_of(code) {
            "zh" => &["cjk-sc"],
            "ja" => &["cjk-jp"],
            "ko" => &["cjk-kr"],
            "hi" => &["deva"],
            "th" => &["thai"],
            "ar" => &["arab"],
            _ => &[],
        };
        self.rebuild_chains();
        self.shaped.borrow_mut().clear();
    }

    fn rebuild_chains(&mut self) {
        for bold in [false, true] {
            let mut chain: Vec<usize> = Vec::new();
            let push = |i: usize, chain: &mut Vec<usize>| {
                if !chain.contains(&i) {
                    chain.push(i);
                }
            };
            // Preferred script fonts, then DejaVu, then everything else; each
            // in the wanted weight first, the other weight as a last resort.
            for want_bold in [bold, !bold] {
                for p in self.preferred {
                    for (i, f) in self.faces.iter().enumerate() {
                        if f.bold == want_bold && f.name.starts_with(p) {
                            push(i, &mut chain);
                        }
                    }
                }
                for (i, f) in self.faces.iter().enumerate() {
                    if f.bold == want_bold && f.name.starts_with("dejavu") {
                        push(i, &mut chain);
                    }
                }
                for (i, f) in self.faces.iter().enumerate() {
                    if f.bold == want_bold {
                        push(i, &mut chain);
                    }
                }
            }
            // Latin chain: DejaVu (wanted weight, then other) first.
            let mut latin: Vec<usize> = chain.iter().copied().filter(|&i| self.faces[i].name.starts_with("dejavu")).collect();
            latin.sort_by_key(|&i| self.faces[i].bold != bold);
            for &i in &chain {
                if !latin.contains(&i) {
                    latin.push(i);
                }
            }
            if bold {
                self.bold_chain = chain;
                self.bold_latin = latin;
            } else {
                self.regular_chain = chain;
                self.regular_latin = latin;
            }
        }
    }

    fn primary(&self, bold: bool) -> &Face {
        &self.faces[if bold { 1 } else { 0 }]
    }

    /// Pixels per em for a nominal size (sizes are DejaVu "line height" sizes).
    fn em_px(&self, size: f32) -> f32 {
        let p = self.primary(false);
        size * p.upem / p.height_units
    }

    pub fn ascent(&self, size: f32) -> f32 {
        let p = self.primary(false);
        p.ab.ascent_unscaled() * self.em_px(size) / p.upem
    }

    pub fn line_height(&self, size: f32) -> i32 {
        libm::ceilf(size) as i32
    }

    fn covers(&self, face: usize, c: char) -> bool {
        self.faces[face].ab.glyph_id(c).0 != 0
    }

    fn shape(&self, text: &str, size: f32, bold: bool) -> Rc<Shaped> {
        let key = (String::from(text), size_key(size), bold);
        if let Some(s) = self.shaped.borrow().get(&key) {
            return s.clone();
        }
        // 1. Simplified Unicode bidi: classify, resolve neutrals from their
        //    neighbours, split into directional runs, order the runs visually.
        let chars: Vec<char> = text.chars().collect();
        #[derive(Clone, Copy, PartialEq)]
        enum Dir {
            L,
            R,
            N,
        }
        let class = |c: char| {
            if is_rtl_char(c) && !is_neutral(c) {
                Dir::R
            } else if c.is_alphanumeric() {
                Dir::L
            } else {
                Dir::N
            }
        };
        let classes: Vec<Dir> = chars.iter().map(|c| class(*c)).collect();
        let para_rtl = classes.iter().find(|d| **d != Dir::N).map(|d| *d == Dir::R).unwrap_or(false);
        let para = if para_rtl { Dir::R } else { Dir::L };
        let mut resolved = classes.clone();
        for i in 0..resolved.len() {
            if classes[i] != Dir::N {
                continue;
            }
            let prev = classes[..i].iter().rev().find(|d| **d != Dir::N).copied().unwrap_or(para);
            let next = classes[i + 1..].iter().find(|d| **d != Dir::N).copied().unwrap_or(para);
            resolved[i] = if prev == next { prev } else { para };
        }
        let mut dir_runs: Vec<(bool, Vec<char>)> = Vec::new();
        for (c, d) in chars.iter().zip(&resolved) {
            let rtl = *d == Dir::R;
            match dir_runs.last_mut() {
                Some((r, v)) if *r == rtl => v.push(*c),
                _ => dir_runs.push((rtl, alloc::vec![*c])),
            }
        }
        if para_rtl {
            dir_runs.reverse();
        }
        // 2. Within each directional run, split by font. Complex scripts try
        //    their own fonts first; everything else prefers DejaVu so Latin
        //    text looks the same in every UI language.
        let (script_chain, latin_chain) = if bold { (&self.bold_chain, &self.bold_latin) } else { (&self.regular_chain, &self.regular_latin) };
        let mut runs: Vec<(usize, bool, String)> = Vec::new();
        for (rtl, cs) in dir_runs {
            let mut font_runs: Vec<(usize, bool, String)> = Vec::new();
            for c in cs {
                if let Some((face, _, s)) = font_runs.last_mut() {
                    if is_neutral(c) && self.covers(*face, c) {
                        s.push(c);
                        continue;
                    }
                }
                let chain = if is_complex_script(c) { script_chain } else { latin_chain };
                let face = chain.iter().copied().find(|&f| self.covers(f, c)).unwrap_or(chain[0]);
                match font_runs.last_mut() {
                    Some((f, _, s)) if *f == face => s.push(c),
                    _ => {
                        let mut s = String::new();
                        s.push(c);
                        font_runs.push((face, rtl, s));
                    }
                }
            }
            if rtl {
                font_runs.reverse();
            }
            runs.extend(font_runs);
        }
        // 2. shape each run and place it.
        let em = self.em_px(size);
        let mut glyphs = Vec::new();
        let mut pen = 0.0f32;
        for (face_idx, rtl, s) in runs {
            let face = &self.faces[face_idx];
            let mut buf = rustybuzz::UnicodeBuffer::new();
            buf.push_str(&s);
            buf.guess_segment_properties();
            buf.set_direction(if rtl { rustybuzz::Direction::RightToLeft } else { rustybuzz::Direction::LeftToRight });
            let out = rustybuzz::shape(&face.rb, &[], buf);
            let k = em / face.upem;
            for (info, pos) in out.glyph_infos().iter().zip(out.glyph_positions()) {
                glyphs.push(PlacedGlyph {
                    face: face_idx as u16,
                    gid: info.glyph_id as u16,
                    x: pen + pos.x_offset as f32 * k,
                    y: pos.y_offset as f32 * k,
                });
                pen += pos.x_advance as f32 * k;
            }
        }
        let shaped = Rc::new(Shaped { width: libm::ceilf(pen) as i32, glyphs });
        let mut cache = self.shaped.borrow_mut();
        if cache.len() > 600 {
            cache.clear();
        }
        cache.insert(key, shaped.clone());
        shaped
    }

    fn bitmap(&self, face: u16, size: f32, gid: u16) -> Option<Rc<Bitmap>> {
        let key = (face, size_key(size), gid);
        if let Some(b) = self.glyphs.borrow().get(&key) {
            return b.clone();
        }
        let f = &self.faces[face as usize];
        let scale = PxScale::from(self.em_px(size) * f.height_units / f.upem);
        let glyph = GlyphId(gid).with_scale_and_position(scale, point(0.0, 0.0));
        let bm = f.ab.outline_glyph(glyph).map(|og| {
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
            Rc::new(Bitmap { ox: b.min.x as i32, oy: b.min.y as i32, w, h, cov })
        });
        self.glyphs.borrow_mut().insert(key, bm.clone());
        bm
    }

    pub fn measure(&self, text: &str, size: f32, bold: bool) -> i32 {
        self.shape(text, size, bold).width
    }

    /// Draws `text` with its top-left at (x, y). Returns the width.
    pub fn draw(&self, c: &mut Canvas, text: &str, size: f32, bold: bool, x: i32, y: i32, color: Color) -> i32 {
        let shaped = self.shape(text, size, bold);
        let baseline = y + libm::roundf(self.ascent(size)) as i32;
        let base = color.packed() & 0x00ff_ffff;
        for g in &shaped.glyphs {
            let Some(bm) = self.bitmap(g.face, size, g.gid) else { continue };
            let gx0 = x + libm::roundf(g.x) as i32 + bm.ox;
            let gy0 = baseline - libm::roundf(g.y) as i32 + bm.oy;
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
        shaped.width
    }

    pub fn draw_centered(&self, c: &mut Canvas, text: &str, size: f32, bold: bool, cx: i32, y: i32, color: Color) -> i32 {
        let w = self.measure(text, size, bold);
        self.draw(c, text, size, bold, cx - w / 2, y, color)
    }

    /// Truncates with an ellipsis so the text fits in `max_w`.
    pub fn fit(&self, text: &str, size: f32, bold: bool, max_w: i32) -> String {
        if self.measure(text, size, bold) <= max_w {
            return String::from(text);
        }
        let chars: Vec<char> = text.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            let s: String = chars[..mid].iter().collect();
            let cand = alloc::format!("{}…", s.trim_end());
            if self.measure(&cand, size, bold) <= max_w {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let s: String = chars[..lo].iter().collect();
        alloc::format!("{}…", s.trim_end())
    }
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}
