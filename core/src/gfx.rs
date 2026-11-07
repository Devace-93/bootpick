//! Software renderer: an ARGB canvas, images, alpha blending and simple
//! anti-aliased shapes. The canvas memory layout matches `BltPixel`
//! (B, G, R, X little-endian) so it can be sent to GOP without conversion.

use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub const fn hex(v: u32) -> Self {
        Self::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
    pub const fn alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }
    pub fn mix(self, other: Color, t: f32) -> Color {
        let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
        Color::rgba(l(self.r, other.r), l(self.g, other.g), l(self.b, other.b), l(self.a, other.a))
    }
    #[inline]
    pub const fn packed(self) -> u32 {
        ((self.a as u32) << 24) | ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

#[inline]
fn blend(dst: u32, src: u32) -> u32 {
    let a = (src >> 24) & 0xff;
    if a == 0 {
        return dst;
    }
    if a == 255 {
        return src & 0x00ff_ffff;
    }
    let ia = 255 - a;
    let dr = (dst >> 16) & 0xff;
    let dg = (dst >> 8) & 0xff;
    let db = dst & 0xff;
    let sr = (src >> 16) & 0xff;
    let sg = (src >> 8) & 0xff;
    let sb = src & 0xff;
    let r = (sr * a + dr * ia + 127) / 255;
    let g = (sg * a + dg * ia + 127) / 255;
    let b = (sb * a + db * ia + 127) / 255;
    (r << 16) | (g << 8) | b
}

/// An image with straight (non-premultiplied) alpha, 0xAARRGGBB.
#[derive(Clone)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

impl Image {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![0; w * h] }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u32 {
        self.px[y * self.w + x]
    }

    /// Bilinear resize (good enough for both up- and down-scaling of icons;
    /// for large downscales we first box-filter to avoid aliasing).
    pub fn resized(&self, nw: usize, nh: usize) -> Image {
        if nw == self.w && nh == self.h {
            return self.clone();
        }
        let mut src = self;
        let tmp;
        // Pre-shrink by integer factor when downscaling a lot.
        let fx = self.w / nw.max(1);
        let fy = self.h / nh.max(1);
        if fx >= 2 || fy >= 2 {
            let f = fx.min(fy).max(1);
            tmp = self.box_down(f);
            src = &tmp;
        }
        let mut out = Image::new(nw, nh);
        let sx = (src.w as f32 - 1.0).max(0.0) / (nw as f32 - 1.0).max(1.0);
        let sy = (src.h as f32 - 1.0).max(0.0) / (nh as f32 - 1.0).max(1.0);
        for y in 0..nh {
            let fy = y as f32 * sy;
            let y0 = fy as usize;
            let y1 = (y0 + 1).min(src.h - 1);
            let ty = fy - y0 as f32;
            for x in 0..nw {
                let fx = x as f32 * sx;
                let x0 = fx as usize;
                let x1 = (x0 + 1).min(src.w - 1);
                let tx = fx - x0 as f32;
                let p = |xx: usize, yy: usize| src.get(xx, yy);
                let c00 = p(x0, y0);
                let c10 = p(x1, y0);
                let c01 = p(x0, y1);
                let c11 = p(x1, y1);
                let mut acc = [0f32; 4];
                let mut wsum = 0f32;
                for (c, w) in [
                    (c00, (1.0 - tx) * (1.0 - ty)),
                    (c10, tx * (1.0 - ty)),
                    (c01, (1.0 - tx) * ty),
                    (c11, tx * ty),
                ] {
                    let a = ((c >> 24) & 0xff) as f32 / 255.0;
                    // weight colour by alpha so transparent pixels do not bleed.
                    let wa = w * a;
                    acc[0] += wa * ((c >> 16) & 0xff) as f32;
                    acc[1] += wa * ((c >> 8) & 0xff) as f32;
                    acc[2] += wa * (c & 0xff) as f32;
                    acc[3] += w * a;
                    wsum += wa;
                }
                let (r, g, b) = if wsum > 0.0 {
                    (acc[0] / wsum, acc[1] / wsum, acc[2] / wsum)
                } else {
                    (0.0, 0.0, 0.0)
                };
                let a = (acc[3] * 255.0 + 0.5) as u32;
                out.px[y * nw + x] = (a.min(255) << 24)
                    | ((r as u32).min(255) << 16)
                    | ((g as u32).min(255) << 8)
                    | (b as u32).min(255);
            }
        }
        out
    }

    fn box_down(&self, f: usize) -> Image {
        let nw = (self.w / f).max(1);
        let nh = (self.h / f).max(1);
        let mut out = Image::new(nw, nh);
        for y in 0..nh {
            for x in 0..nw {
                let mut acc = [0u32; 4];
                let mut wsum = 0u32;
                for yy in 0..f {
                    for xx in 0..f {
                        let c = self.get(x * f + xx, y * f + yy);
                        let a = (c >> 24) & 0xff;
                        acc[0] += a * ((c >> 16) & 0xff);
                        acc[1] += a * ((c >> 8) & 0xff);
                        acc[2] += a * (c & 0xff);
                        acc[3] += a;
                        wsum += a;
                    }
                }
                let n = (f * f) as u32;
                let (r, g, b) = if wsum > 0 {
                    (acc[0] / wsum, acc[1] / wsum, acc[2] / wsum)
                } else {
                    (0, 0, 0)
                };
                out.px[y * nw + x] = ((acc[3] / n) << 24) | (r << 16) | (g << 8) | b;
            }
        }
        out
    }

    /// Scale to cover `w`x`h` and crop centred.
    pub fn cover(&self, w: usize, h: usize) -> Image {
        let sw = w as f32 / self.w as f32;
        let sh = h as f32 / self.h as f32;
        let s = if sw > sh { sw } else { sh };
        let rw = ((self.w as f32 * s) as usize).max(w);
        let rh = ((self.h as f32 * s) as usize).max(h);
        let r = self.resized(rw, rh);
        let ox = (rw - w) / 2;
        let oy = (rh - h) / 2;
        let mut out = Image::new(w, h);
        for y in 0..h {
            let row = &r.px[(y + oy) * rw + ox..(y + oy) * rw + ox + w];
            out.px[y * w..y * w + w].copy_from_slice(row);
        }
        out
    }
}

// ----- corner masks ------------------------------------------------------------

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use core::cell::RefCell;

struct MaskCache(RefCell<BTreeMap<(i32, i32), Rc<Vec<u8>>>>);
// SAFETY: UEFI boot services are single-threaded.
unsafe impl Sync for MaskCache {}
static MASKS: MaskCache = MaskCache(RefCell::new(BTreeMap::new()));

/// Coverage (0..=255) of the top-left `radius`x`radius` corner of a rounded
/// rectangle: filled when `thickness` is 0, otherwise a ring that thick.
/// Computed once per (radius, thickness) with floats, then reused.
fn corner_mask(radius: i32, thickness: i32) -> Rc<Vec<u8>> {
    if let Some(m) = MASKS.0.borrow().get(&(radius, thickness)) {
        return m.clone();
    }
    let n = radius as usize;
    let rf = radius as f32;
    let mut m = alloc::vec![0u8; n * n];
    for j in 0..n {
        for i in 0..n {
            let dx = rf - (i as f32 + 0.5);
            let dy = rf - (j as f32 + 0.5);
            let d = libm::sqrtf(dx * dx + dy * dy) - rf;
            let outer = (0.5 - d).clamp(0.0, 1.0);
            let cov = if thickness == 0 {
                outer
            } else {
                outer - (0.5 - (d + thickness as f32)).clamp(0.0, 1.0)
            };
            m[j * n + i] = (cov * 255.0 + 0.5) as u8;
        }
    }
    let m = Rc::new(m);
    MASKS.0.borrow_mut().insert((radius, thickness), m.clone());
    m
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2 * d, self.h - 2 * d)
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.w).max(o.x + o.w);
        let y1 = (self.y + self.h).max(o.y + o.h);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![0; w * h] }
    }

    pub fn copy_from(&mut self, other: &Canvas) {
        self.px.copy_from_slice(&other.px);
    }

    pub fn copy_rect_from(&mut self, other: &Canvas, r: Rect) {
        let r = self.clip(r);
        for y in r.y..r.y + r.h {
            let s = y as usize * self.w + r.x as usize;
            let e = s + r.w as usize;
            self.px[s..e].copy_from_slice(&other.px[s..e]);
        }
    }

    pub fn clip(&self, r: Rect) -> Rect {
        let x0 = r.x.max(0);
        let y0 = r.y.max(0);
        let x1 = (r.x + r.w).min(self.w as i32);
        let y1 = (r.y + r.h).min(self.h as i32);
        if x1 <= x0 || y1 <= y0 {
            return Rect::new(0, 0, 0, 0);
        }
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    #[inline]
    pub fn blend_px(&mut self, x: i32, y: i32, c: u32) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let i = y as usize * self.w + x as usize;
        self.px[i] = blend(self.px[i], c);
    }

    pub fn fill_rect(&mut self, r: Rect, c: Color) {
        let r = self.clip(r);
        let v = c.packed();
        if c.a == 255 {
            let v = v & 0x00ff_ffff;
            for y in r.y..r.y + r.h {
                let s = y as usize * self.w + r.x as usize;
                self.px[s..s + r.w as usize].iter_mut().for_each(|p| *p = v);
            }
        } else {
            for y in r.y..r.y + r.h {
                let s = y as usize * self.w + r.x as usize;
                for p in &mut self.px[s..s + r.w as usize] {
                    *p = blend(*p, v);
                }
            }
        }
    }

    /// Vertical gradient fill.
    pub fn fill_gradient(&mut self, r: Rect, top: Color, bottom: Color) {
        let r = self.clip(r);
        for y in r.y..r.y + r.h {
            let t = (y - r.y) as f32 / (r.h.max(1)) as f32;
            let c = top.mix(bottom, t).packed() & 0x00ff_ffff;
            let s = y as usize * self.w + r.x as usize;
            self.px[s..s + r.w as usize].iter_mut().for_each(|p| *p = c);
        }
    }

    /// Anti-aliased rounded rectangle. Corner coverage comes from a cached
    /// quarter-circle mask, so the per-pixel work is integer only (the UEFI
    /// target has no FPU: every f32 operation is emulated in software).
    pub fn fill_round_rect(&mut self, r: Rect, radius: i32, c: Color) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        if radius == 0 {
            self.fill_rect(r, c);
            return;
        }
        let clip = self.clip(r);
        if clip.w <= 0 || clip.h <= 0 {
            return;
        }
        let mask = corner_mask(radius, 0);
        let rad = radius as usize;
        let base = c.packed() & 0x00ff_ffff;
        let full = c.packed();
        for y in clip.y..clip.y + clip.h {
            let ry = y - r.y;
            let my = if ry < radius { Some(ry as usize) } else if ry >= r.h - radius { Some((r.h - 1 - ry) as usize) } else { None };
            let row = y as usize * self.w;
            match my {
                None => {
                    for p in &mut self.px[row + clip.x as usize..row + (clip.x + clip.w) as usize] {
                        *p = blend(*p, full);
                    }
                }
                Some(my) => {
                    for x in clip.x..clip.x + clip.w {
                        let rx = x - r.x;
                        let mx = if rx < radius { Some(rx as usize) } else if rx >= r.w - radius { Some((r.w - 1 - rx) as usize) } else { None };
                        let v = match mx {
                            None => full,
                            Some(mx) => {
                                let cov = mask[my * rad + mx] as u32;
                                if cov == 0 {
                                    continue;
                                }
                                ((cov * c.a as u32 / 255) << 24) | base
                            }
                        };
                        let i = row + x as usize;
                        self.px[i] = blend(self.px[i], v);
                    }
                }
            }
        }
    }

    /// Anti-aliased rounded outline `t` pixels thick (integer only, see above).
    pub fn stroke_round_rect(&mut self, r: Rect, radius: i32, t: i32, c: Color) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        let t = t.max(1);
        if radius == 0 {
            self.fill_rect(Rect::new(r.x, r.y, r.w, t), c);
            self.fill_rect(Rect::new(r.x, r.y + r.h - t, r.w, t), c);
            self.fill_rect(Rect::new(r.x, r.y + t, t, r.h - 2 * t), c);
            self.fill_rect(Rect::new(r.x + r.w - t, r.y + t, t, r.h - 2 * t), c);
            return;
        }
        let clip = self.clip(r);
        if clip.w <= 0 || clip.h <= 0 {
            return;
        }
        let mask = corner_mask(radius, t);
        let rad = radius as usize;
        let base = c.packed() & 0x00ff_ffff;
        let full = c.packed();
        let band = radius.max(t);
        for y in clip.y..clip.y + clip.h {
            let ry = y - r.y;
            let edge_row = ry < t || ry >= r.h - t;
            let my = if ry < radius { Some(ry as usize) } else if ry >= r.h - radius { Some((r.h - 1 - ry) as usize) } else { None };
            let row = y as usize * self.w;
            let in_band = ry < band || ry >= r.h - band;
            // Middle rows: only the left and right edges.
            let spans: [(i32, i32); 2] = if in_band {
                [(clip.x, clip.x + clip.w), (0, 0)]
            } else {
                [(clip.x, (r.x + t).min(clip.x + clip.w)), ((r.x + r.w - t).max(clip.x), clip.x + clip.w)]
            };
            for (x0, x1) in spans {
                for x in x0..x1 {
                    let rx = x - r.x;
                    let mx = if rx < radius { Some(rx as usize) } else if rx >= r.w - radius { Some((r.w - 1 - rx) as usize) } else { None };
                    let v = match (my, mx) {
                        (Some(my), Some(mx)) => {
                            let cov = mask[my * rad + mx] as u32;
                            if cov == 0 {
                                continue;
                            }
                            ((cov * c.a as u32 / 255) << 24) | base
                        }
                        _ => {
                            let on_edge = edge_row || rx < t || rx >= r.w - t;
                            if !on_edge {
                                continue;
                            }
                            full
                        }
                    };
                    let i = row + x as usize;
                    self.px[i] = blend(self.px[i], v);
                }
            }
        }
    }

    pub fn fill_circle(&mut self, cx: i32, cy: i32, radius: i32, c: Color) {
        self.fill_round_rect(Rect::new(cx - radius, cy - radius, radius * 2, radius * 2), radius, c);
    }

    pub fn blit(&mut self, img: &Image, x: i32, y: i32) {
        let r = self.clip(Rect::new(x, y, img.w as i32, img.h as i32));
        for yy in r.y..r.y + r.h {
            let sy = (yy - y) as usize;
            let di = yy as usize * self.w;
            for xx in r.x..r.x + r.w {
                let sx = (xx - x) as usize;
                let s = img.px[sy * img.w + sx];
                let d = &mut self.px[di + xx as usize];
                *d = blend(*d, s);
            }
        }
    }

    /// Blit an opaque image without blending (fast path for backgrounds).
    pub fn blit_opaque(&mut self, img: &Image, x: i32, y: i32) {
        let r = self.clip(Rect::new(x, y, img.w as i32, img.h as i32));
        for yy in r.y..r.y + r.h {
            let sy = (yy - y) as usize;
            let sx0 = (r.x - x) as usize;
            let src = &img.px[sy * img.w + sx0..sy * img.w + sx0 + r.w as usize];
            let di = yy as usize * self.w + r.x as usize;
            for (d, s) in self.px[di..di + r.w as usize].iter_mut().zip(src) {
                *d = *s & 0x00ff_ffff;
            }
        }
    }

    /// Draws `img` tinted: alpha from the image, colour from `c`.
    pub fn blit_tinted(&mut self, img: &Image, x: i32, y: i32, c: Color) {
        let r = self.clip(Rect::new(x, y, img.w as i32, img.h as i32));
        let base = c.packed() & 0x00ff_ffff;
        for yy in r.y..r.y + r.h {
            let sy = (yy - y) as usize;
            let di = yy as usize * self.w;
            for xx in r.x..r.x + r.w {
                let sx = (xx - x) as usize;
                let a = (img.px[sy * img.w + sx] >> 24) * c.a as u32 / 255;
                let d = &mut self.px[di + xx as usize];
                *d = blend(*d, (a << 24) | base);
            }
        }
    }

    /// Pixels as little-endian 0x00RRGGBB words, the layout of UEFI's
    /// `BltPixel` (blue, green, red, reserved).
    pub fn pixels(&self) -> &[u32] {
        &self.px
    }
}
