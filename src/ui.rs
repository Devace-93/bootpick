//! The user interface: main picker screen, settings screen, cursor and
//! event loop.

use crate::bmp;
use crate::boot;
use crate::config::{theme_by_id, theme_name, Config, EntryPref, Theme, THEMES};
use crate::esp;
use crate::font::{self, Fonts};
use crate::gfx::{Canvas, Color, Image, Rect};
use crate::input::{Input, InputEvent, TICK_MS};
use crate::scan::{Kind, Scan};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use uefi::boot::{self as uboot, ScopedProtocol};
use uefi::fs::FileSystem;
use uefi::proto::console::gop::{BltOp, BltRegion, GraphicsOutput};
use uefi::proto::console::text::{Key, ScanCode};

pub const CONF_DIR: &str = "\\EFI\\bootpick";
pub const CONF_PATH: &str = "\\EFI\\bootpick\\bootpick.conf";
pub const ICON_DIR: &str = "\\EFI\\bootpick\\icons";
pub const BG_DIR: &str = "\\EFI\\bootpick\\backgrounds";
pub const LAST_PATH: &str = "\\EFI\\bootpick\\last-entry";
/// Config value for "start whatever was picked last time".
pub const DEFAULT_LAST: &str = "@last";

#[derive(Clone, Debug)]
pub struct Item {
    pub entry: usize,
    pub alias: String,
    pub icon: String,
    pub hidden: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Tile(usize),
    OpenSettings,
    Cancel,
    Save,
    Firmware,
    Reboot,
    Shutdown,
    ToggleHidden(usize),
    EditAlias(usize),
    MoveUp(usize),
    MoveDown(usize),
    Selector(usize, i32),
    FocusRow(usize),
    ScrollUp,
    ScrollDown,
    ToggleReveal,
}

struct Hit {
    rect: Rect,
    action: Action,
}

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Main,
    Settings,
}

pub enum Outcome {
    Launch { entry: usize, remember: bool },
    Firmware,
    Reboot,
    Shutdown,
}

struct Snapshot {
    items: Vec<Item>,
    theme_idx: usize,
    bg_idx: usize,
    timeout: u32,
    default_id: String,
}

pub struct App<'a> {
    gop: ScopedProtocol<GraphicsOutput>,
    w: usize,
    h: usize,
    s: f32,
    scene: Canvas,
    frame: Canvas,
    bg: Canvas,
    fonts: Fonts,
    glyphs: font::GlyphCache,
    theme_idx: usize,
    bg_list: Vec<String>,
    bg_idx: usize,
    icon_cache: BTreeMap<(String, i32), Option<Image>>,
    cfg: Config,
    scan: &'a Scan,
    items: Vec<Item>,
    fs: Option<FileSystem>,
    input: Input,
    screen: Screen,
    hits: Vec<Hit>,
    hover: Option<Action>,
    sel: usize,
    reveal: bool,
    countdown: Option<i32>,
    status: Option<(String, i32, bool)>,
    focus: usize,
    scroll: usize,
    editing: Option<String>,
    snapshot: Option<Snapshot>,
    cursor: Image,
    cursor_rect: Rect,
    default_id: String,
    last_id: String,
    debug: bool,
    frame_cycles: u64,
    draw_cycles: u64,
}

const SELECTORS: usize = 4;

// ---- tiny profiler for the debug overlay ([rect, icon, text, bg])
static mut PROF: [u64; 4] = [0; 4];
#[inline]
fn tsc() -> u64 {
    // SAFETY: rdtsc has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}
#[inline]
fn prof_add(i: usize, since: u64) {
    // SAFETY: single-threaded boot services environment.
    unsafe { PROF[i] += tsc() - since }
}

impl<'a> App<'a> {
    pub fn new(scan: &'a Scan, mut fs: Option<FileSystem>, status: Option<String>) -> Result<App<'a>, String> {
        let gop_handle = uboot::get_handle_for_protocol::<GraphicsOutput>().map_err(|e| format!("no GOP: {:?}", e.status()))?;
        let mut gop = uboot::open_protocol_exclusive::<GraphicsOutput>(gop_handle).map_err(|e| format!("GOP busy: {:?}", e.status()))?;
        pick_mode(&mut gop);
        let (w, h) = gop.current_mode_info().resolution();
        let s = (h as f32 / 1080.0).clamp(0.55, 2.5);

        let cfg = fs
            .as_mut()
            .and_then(|f| esp::read_string(f, CONF_PATH))
            .map(|t| Config::parse(&t))
            .unwrap_or_default();

        let fonts = Fonts::load().ok_or("font load failed")?;
        let items = build_items(scan, &cfg);
        let default_id = if cfg.default_entry.is_empty() {
            items.iter().find(|i| !i.hidden).map(|i| scan.entries[i.entry].id.clone()).unwrap_or_default()
        } else {
            cfg.default_entry.clone()
        };
        let last_id = fs.as_mut().and_then(|f| esp::read_string(f, LAST_PATH)).map(|s| s.trim().to_string()).unwrap_or_default();

        let mut bg_list = alloc::vec![String::new()];
        if let Some(f) = fs.as_mut() {
            for (name, is_dir) in esp::list_dir(f, BG_DIR) {
                if !is_dir && name.to_ascii_lowercase().ends_with(".bmp") {
                    bg_list.push(name);
                }
            }
        }
        let bg_idx = bg_list.iter().position(|b| b.eq_ignore_ascii_case(&cfg.background)).unwrap_or(0);
        let theme_idx = theme_by_id(&cfg.theme);
        let input = Input::new(w, h, cfg.mouse_speed);
        crate::trace::line(&format!("display {}x{}, input: {}", w, h, input.stats));
        crate::trace::flush();
        let cursor = make_cursor(if s >= 1.4 { 3 } else if s >= 0.8 { 2 } else { 1 });
        let countdown = if cfg.timeout > 0 && status.is_none() { Some((cfg.timeout as i32) * 1000 / TICK_MS as i32) } else { None };

        let mut app = App {
            gop,
            w,
            h,
            s,
            scene: Canvas::new(w, h),
            frame: Canvas::new(w, h),
            bg: Canvas::new(w, h),
            fonts,
            glyphs: font::GlyphCache::default(),
            theme_idx,
            bg_list,
            bg_idx,
            icon_cache: BTreeMap::new(),
            cfg,
            scan,
            items,
            fs,
            input,
            screen: Screen::Main,
            hits: Vec::new(),
            hover: None,
            sel: 0,
            reveal: false,
            countdown,
            status: status.map(|m| (m, 200, true)),
            focus: 0,
            scroll: 0,
            editing: None,
            snapshot: None,
            cursor,
            cursor_rect: Rect::new(0, 0, 0, 0),
            default_id,
            last_id,
            debug: false,
            frame_cycles: 0,
            draw_cycles: 0,
        };
        let eff = app.effective_default();
        app.sel = app.visible().iter().position(|&i| scan.entries[app.items[i].entry].id == eff).unwrap_or(0);
        app.render_background();
        Ok(app)
    }

    fn theme(&self) -> Theme {
        THEMES[self.theme_idx]
    }

    fn has_photo(&self) -> bool {
        self.bg_list.get(self.bg_idx).map(|b| !b.is_empty()).unwrap_or(false)
    }

    fn px(&self, v: f32) -> i32 {
        (v * self.s) as i32
    }

    fn visible(&self) -> Vec<usize> {
        (0..self.items.len()).filter(|&i| self.reveal || !self.items[i].hidden).collect()
    }

    /// The entry id that is pre-selected (and auto-started) on this boot.
    fn effective_default(&self) -> String {
        let want = if self.default_id == DEFAULT_LAST { &self.last_id } else { &self.default_id };
        if self.items.iter().any(|it| !it.hidden && self.scan.entries[it.entry].id == *want) {
            return want.clone();
        }
        self.items.iter().find(|it| !it.hidden).map(|it| self.scan.entries[it.entry].id.clone()).unwrap_or_default()
    }

    fn launch_item(&self, item: usize) -> Outcome {
        Outcome::Launch { entry: self.items[item].entry, remember: self.default_id == DEFAULT_LAST }
    }

    fn entry_id(&self, item: usize) -> &str {
        &self.scan.entries[self.items[item].entry].id
    }

    // ----- background / icons ---------------------------------------------------

    fn render_background(&mut self) {
        let t = self.theme();
        let full = Rect::new(0, 0, self.w as i32, self.h as i32);
        self.bg.fill_gradient(full, t.bg0, t.bg1);
        let name = self.bg_list.get(self.bg_idx).cloned().unwrap_or_default();
        if !name.is_empty() {
            if let Some(f) = self.fs.as_mut() {
                if let Some(data) = esp::read(f, &format!("{}\\{}", BG_DIR, name)) {
                    if let Some(img) = bmp::decode(&data) {
                        let img = img.cover(self.w, self.h);
                        self.bg.blit_opaque(&img, 0, 0);
                        // Slight darkening so text stays readable on bright photos.
                        let shade = if t.id == "light" { Color::rgba(255, 255, 255, 40) } else { Color::rgba(0, 0, 0, 70) };
                        self.bg.fill_rect(full, shade);
                    }
                }
            }
        }
    }

    fn icon(&mut self, name: &str, size: i32) -> Option<Image> {
        let key = (name.to_string(), size);
        if let Some(c) = self.icon_cache.get(&key) {
            return c.clone();
        }
        let loaded = self
            .fs
            .as_mut()
            .and_then(|f| esp::read(f, &format!("{}\\{}.bmp", ICON_DIR, name)))
            .and_then(|d| bmp::decode(&d))
            .map(|img| img.resized(size as usize, size as usize));
        self.icon_cache.insert(key, loaded.clone());
        loaded
    }

    fn draw_icon(&mut self, name: &str, cx: i32, cy: i32, size: i32, label: &str) {
        let t0 = tsc();
        self.draw_icon_inner(name, cx, cy, size, label);
        prof_add(1, t0);
    }

    fn draw_icon_inner(&mut self, name: &str, cx: i32, cy: i32, size: i32, label: &str) {
        match self.icon(name, size) {
            Some(img) => self.scene.blit(&img, cx - size / 2, cy - size / 2),
            None => {
                let t = self.theme();
                self.scene.fill_circle(cx, cy, size / 2 - 4, t.accent.alpha(200));
                let initial: String = label.chars().next().unwrap_or('?').to_uppercase().collect();
                let fs = size as f32 * 0.5;
                let lh = font::line_height(&self.fonts.bold, fs);
                self.text_c(true, fs, cx, cy - lh / 2, t.bg0, &initial);
            }
        }
    }

    fn draw_ui_icon(&mut self, name: &str, cx: i32, cy: i32, size: i32, color: Color) {
        if let Some(img) = self.icon(name, size) {
            self.scene.blit_tinted(&img, cx - size / 2, cy - size / 2, color);
        } else {
            self.scene.fill_circle(cx, cy, size / 3, color);
        }
    }

    // ----- presenting -----------------------------------------------------------

    fn present_full(&mut self) {
        self.frame.copy_from(&self.scene);
        self.cursor_rect = Rect::new(0, 0, 0, 0);
        self.draw_cursor();
        let (w, h) = (self.w, self.h);
        let _ = self.gop.blt(BltOp::BufferToVideo {
            buffer: self.frame.as_blt(),
            src: BltRegion::Full,
            dest: (0, 0),
            dims: (w, h),
        });
    }

    fn draw_cursor(&mut self) {
        if !self.input.has_pointer() {
            return;
        }
        let (x, y) = (self.input.x, self.input.y);
        self.frame.blit(&self.cursor, x, y);
        self.cursor_rect = self.frame.clip(Rect::new(x, y, self.cursor.w as i32, self.cursor.h as i32));
    }

    fn present_cursor(&mut self) {
        let old = self.cursor_rect;
        self.frame.copy_rect_from(&self.scene, old);
        self.draw_cursor();
        let r = old.union(&self.cursor_rect);
        let r = self.frame.clip(r);
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        let stride = self.w;
        let _ = self.gop.blt(BltOp::BufferToVideo {
            buffer: self.frame.as_blt(),
            src: BltRegion::SubRectangle { coords: (r.x as usize, r.y as usize), px_stride: stride },
            dest: (r.x as usize, r.y as usize),
            dims: (r.w as usize, r.h as usize),
        });
    }

    fn redraw(&mut self) {
        // SAFETY: rdtsc has no side effects; used only for the debug overlay.
        let t0 = unsafe { core::arch::x86_64::_rdtsc() };
        // SAFETY: single-threaded.
        let prof = unsafe { let p = PROF; PROF = [0; 4]; p };
        match self.screen {
            Screen::Main => self.draw_main(),
            Screen::Settings => self.draw_settings(),
        }
        // SAFETY: see above.
        let t1 = unsafe { core::arch::x86_64::_rdtsc() };
        if self.debug {
            let t = self.theme();
            let msg = format!(
                "frame {:.1} Mc (draw {:.1}, present {:.1}) rect {:.1} icon {:.1} text {:.1} bg {:.1}  {}",
                self.frame_cycles as f32 / 1.0e6,
                self.draw_cycles as f32 / 1.0e6, (self.frame_cycles.saturating_sub(self.draw_cycles)) as f32 / 1.0e6,
                prof[0] as f32 / 1.0e6, prof[1] as f32 / 1.0e6, prof[2] as f32 / 1.0e6, prof[3] as f32 / 1.0e6, self.input.stats
            );
            self.text(false, 14.0 * self.s, self.px(12.0), self.px(8.0), t.text_dim, &msg);
        }
        self.present_full();
        // SAFETY: see above.
        self.frame_cycles = unsafe { core::arch::x86_64::_rdtsc() } - t0;
        self.draw_cycles = t1 - t0;
    }

    fn text(&mut self, bold: bool, size: f32, x: i32, y: i32, color: Color, s: &str) -> i32 {
        let t = tsc();
        let r = font::draw_cached(&mut self.scene, &self.fonts, &self.glyphs, bold, size, x, y, color, s);
        prof_add(2, t);
        r
    }

    fn text_c(&mut self, bold: bool, size: f32, cx: i32, y: i32, color: Color, s: &str) -> i32 {
        let t = tsc();
        let r = font::draw_centered_cached(&mut self.scene, &self.fonts, &self.glyphs, bold, size, cx, y, color, s);
        prof_add(2, t);
        r
    }

    // ----- widgets --------------------------------------------------------------

    fn hit_at(&self, x: i32, y: i32) -> Option<Action> {
        self.hits.iter().rev().find(|h| h.rect.contains(x, y)).map(|h| h.action)
    }

    fn is_hover(&self, a: Action) -> bool {
        self.hover == Some(a)
    }

    fn button(&mut self, r: Rect, label: &str, primary: bool, action: Action) {
        let t = self.theme();
        let hovered = self.is_hover(action);
        let (bgc, fg) = if primary {
            (if hovered { t.accent.mix(t.text, 0.2) } else { t.accent }, t.bg0)
        } else {
            (if hovered { t.tile_hover } else { t.tile }, t.text)
        };
        let rad = self.px(12.0);
        self.scene.fill_round_rect(r, rad, bgc);
        if !primary {
            self.scene.stroke_round_rect(r, rad, 1, t.text_dim.alpha(120));
        }
        let fs = 22.0 * self.s;
        let lh = font::line_height(&self.fonts.bold, fs);
        self.text_c(true, fs, r.x + r.w / 2, r.y + (r.h - lh) / 2, fg, label);
        self.hits.push(Hit { rect: r, action });
    }

    fn tool_width(&self, label: &str) -> i32 {
        font::measure(&self.fonts.regular, 15.0 * self.s, label).max(self.px(76.0)) + self.px(28.0)
    }

    /// Draws a bottom-bar button whose left edge is `x`; returns its width.
    fn tool(&mut self, x: i32, y: i32, icon: &str, label: &str, action: Action) -> i32 {
        let t = self.theme();
        let size = self.px(44.0);
        let hovered = self.is_hover(action);
        let tw = self.tool_width(label);
        let cx = x + tw / 2;
        let r = Rect::new(x, y - self.px(8.0), tw, self.px(84.0));
        if hovered {
            self.scene.fill_round_rect(r, self.px(14.0), t.tile_hover);
        }
        let color = if hovered { t.accent } else { t.text };
        self.draw_ui_icon(icon, cx, y + size / 2, size, color);
        let fs = 15.0 * self.s;
        self.text_c(false, fs, cx, y + size + self.px(6.0), t.text_dim, label);
        self.hits.push(Hit { rect: r, action });
        tw
    }

    // ----- main screen ----------------------------------------------------------

    fn draw_main(&mut self) {
        let t0 = tsc();
        self.scene.copy_from(&self.bg);
        prof_add(3, t0);
        self.hits.clear();
        let t = self.theme();
        let (w, h) = (self.w as i32, self.h as i32);
        let vis = self.visible();
        if self.sel >= vis.len() {
            self.sel = 0;
        }

        let tile = self.px(200.0);
        let icon = self.px(150.0);
        let tile_w = tile + self.px(56.0);
        let tile_h = tile + self.px(64.0);
        let per_row = ((w - self.px(120.0)) / tile_w).max(1) as usize;
        let rows = if vis.is_empty() { 0 } else { (vis.len() + per_row - 1) / per_row };
        let y0 = (h - rows as i32 * tile_h) / 2 - self.px(40.0);

        // Soft backdrop behind the grid so labels stay readable on photos.
        if rows > 0 {
            let in_row = per_row.min(vis.len()) as i32;
            let bw = in_row * tile_w + self.px(64.0);
            let bh = rows as i32 * tile_h + self.px(100.0);
            let br = Rect::new((w - bw) / 2, y0 - self.px(32.0), bw, bh);
            self.scene.fill_round_rect(br, self.px(36.0), t.bg0.alpha(if self.has_photo() { 150 } else { 0 }));
        }
        if vis.is_empty() {
            let fs = 30.0 * self.s;
            self.text_c(true, fs, w / 2, h / 2 - self.px(40.0), t.text, "No boot entries to show");
            self.text_c(false, 20.0 * self.s, w / 2, h / 2 + self.px(4.0), t.text_dim, "Press Esc to reveal hidden entries, or open Settings.");
        }

        for (k, &i) in vis.iter().enumerate() {
            let row = k / per_row;
            let col = k % per_row;
            let in_row = per_row.min(vis.len() - row * per_row) as i32;
            let x0 = (w - in_row * tile_w) / 2;
            let x = x0 + col as i32 * tile_w;
            let y = y0 + row as i32 * tile_h;
            let outer = Rect::new(x, y, tile_w, tile_h);
            let r = Rect::new(x + self.px(8.0), y, tile_w - self.px(16.0), tile_h - self.px(12.0));
            let selected = k == self.sel;
            let hovered = self.is_hover(Action::Tile(k));
            let rad = self.px(26.0);
            let tr = tsc();
            if selected {
                self.scene.fill_round_rect(r, rad, t.accent.alpha(55));
                self.scene.stroke_round_rect(r, rad, self.px(3.0).max(2), t.accent);
            } else if hovered {
                self.scene.fill_round_rect(r, rad, t.tile_hover);
            } else {
                self.scene.fill_round_rect(r, rad, t.tile);
            }
            prof_add(0, tr);
            let item = self.items[i].clone();
            let cx = r.x + r.w / 2;
            let cy = r.y + self.px(24.0) + icon / 2;
            self.draw_icon(&item.icon, cx, cy, icon, &item.alias);
            let fs = 28.0 * self.s;
            let label = font::fit(&self.fonts.bold, fs, &item.alias, r.w - self.px(24.0));
            let ly = r.y + self.px(24.0) + icon + self.px(14.0);
            let color = if item.hidden { t.text_dim } else { t.text };
            if self.has_photo() {
                self.text_c(true, fs, cx + 1, ly + 2, t.bg0.alpha(160), &label);
            }
            self.text_c(true, fs, cx, ly, color, &label);
            let location = self.scan.entries[item.entry].location.clone();
            let sub = match (location.is_empty(), item.hidden) {
                (false, true) => format!("{}  ·  hidden", location),
                (false, false) => location,
                (true, true) => "hidden".to_string(),
                (true, false) => String::new(),
            };
            if !sub.is_empty() {
                let lh = font::line_height(&self.fonts.bold, fs);
                self.text_c(false, 15.0 * self.s, cx, ly + lh, t.text_dim, &sub);
            }
            self.hits.push(Hit { rect: outer, action: Action::Tile(k) });
        }

        // Hint / countdown line.
        let hint_y = y0 + rows as i32 * tile_h + self.px(26.0);
        let fs = 20.0 * self.s;
        if let Some(ticks) = self.countdown {
            let secs = (ticks as i64 * TICK_MS as i64 + 999) / 1000;
            let name = vis.get(self.sel).map(|&i| self.items[i].alias.clone()).unwrap_or_default();
            let msg = format!("Starting {} in {}s  ·  move the mouse or press a key to cancel", name, secs);
            self.text_c(false, fs, w / 2, hint_y, t.text, &msg);
        } else if !vis.is_empty() {
            let msg = if self.input.has_pointer() {
                "Click a selected entry (or press Enter) to start it"
            } else {
                "Use the arrow keys and Enter to start an entry"
            };
            self.text_c(false, fs, w / 2, hint_y, t.text_dim, msg);
        }

        // Bottom bar.
        if self.has_photo() {
            self.scene.fill_gradient(Rect::new(0, h - self.px(150.0), w, self.px(150.0)), t.bg0.alpha(0), t.bg0.alpha(190));
        }
        let bar_y = h - self.px(104.0);
        let gap = self.px(10.0);
        let mut lx = self.px(24.0);
        lx += self.tool(lx, bar_y, "ui_gear", "Settings", Action::OpenSettings) + gap;
        let hidden_n = self.items.iter().filter(|i| i.hidden).count();
        if hidden_n > 0 {
            let label = if self.reveal { "Hide extras".to_string() } else { format!("Show hidden ({})", hidden_n) };
            self.tool(lx, bar_y, if self.reveal { "ui_eye_off" } else { "ui_eye" }, &label, Action::ToggleReveal);
        }
        let mut rx = w - self.px(24.0);
        rx -= self.tool_width("Shut down");
        self.tool(rx, bar_y, "ui_power", "Shut down", Action::Shutdown);
        rx -= self.tool_width("Restart") + gap;
        self.tool(rx, bar_y, "ui_restart", "Restart", Action::Reboot);
        if self.scan.fw_setup_supported {
            rx -= self.tool_width("Firmware setup") + gap;
            self.tool(rx, bar_y, "ui_firmware", "Firmware setup", Action::Firmware);
        }

        // Keyboard legend.
        let legend = "←↑↓→ select   Enter start   Esc hidden   F2 settings   F12 firmware";
        self.text_c(false, 15.0 * self.s, w / 2, h - self.px(30.0), t.text_dim.alpha(170), legend);
        if !self.input.has_pointer() {
            self.text_c(false, 15.0 * self.s, w / 2, h - self.px(52.0), t.text_dim.alpha(170), "No mouse exposed by the firmware — keyboard only");
        }

        self.draw_status();
    }

    fn draw_status(&mut self) {
        if let Some((msg, _, is_err)) = self.status.clone() {
            let t = self.theme();
            let fs = 20.0 * self.s;
            let tw = font::measure(&self.fonts.regular, fs, &msg);
            let pw = tw + self.px(48.0);
            let ph = self.px(52.0);
            let r = Rect::new((self.w as i32 - pw) / 2, self.px(28.0), pw, ph);
            let c = if is_err { t.danger.alpha(225) } else { t.accent.alpha(225) };
            self.scene.fill_round_rect(r, self.px(14.0), c);
            let lh = font::line_height(&self.fonts.regular, fs);
            self.text(false, fs, r.x + self.px(24.0), r.y + (ph - lh) / 2, t.bg0, &msg);
        }
    }

    // ----- settings screen --------------------------------------------------------

    fn selector_value(&self, i: usize) -> String {
        match i {
            0 => theme_name(self.theme_idx),
            1 => {
                let n = &self.bg_list[self.bg_idx];
                if n.is_empty() { "None (gradient)".into() } else { n.trim_end_matches(".bmp").trim_end_matches(".BMP").replace(['_', '-'], " ") }
            }
            2 => format!("{} s", self.cfg.timeout),
            _ if self.default_id == DEFAULT_LAST => "Last selected".into(),
            _ => self
                .items
                .iter()
                .find(|it| self.scan.entries[it.entry].id == self.default_id)
                .map(|it| it.alias.clone())
                .unwrap_or_else(|| "—".into()),
        }
    }

    fn change_selector(&mut self, i: usize, dir: i32) {
        match i {
            0 => {
                self.theme_idx = (self.theme_idx as i32 + dir).rem_euclid(THEMES.len() as i32) as usize;
                self.render_background();
            }
            1 => {
                self.bg_idx = (self.bg_idx as i32 + dir).rem_euclid(self.bg_list.len() as i32) as usize;
                self.render_background();
            }
            2 => {
                // 5, 10, ... 60 seconds.
                let pos = (self.cfg.timeout.clamp(5, 60) / 5 - 1) as i32;
                let np = (pos + dir).rem_euclid(12) as u32;
                self.cfg.timeout = (np + 1) * 5;
            }
            _ => {
                // "Last selected" first, then every visible entry.
                let mut cands: Vec<String> = alloc::vec![DEFAULT_LAST.to_string()];
                cands.extend((0..self.items.len()).filter(|&k| !self.items[k].hidden).map(|k| self.entry_id(k).to_string()));
                let cur = cands.iter().position(|c| *c == self.default_id).unwrap_or(0) as i32;
                self.default_id = cands[(cur + dir).rem_euclid(cands.len() as i32) as usize].clone();
            }
        }
    }

    fn draw_settings(&mut self) {
        self.scene.copy_from(&self.bg);
        self.hits.clear();
        let t = self.theme();
        let (w, h) = (self.w as i32, self.h as i32);
        let pw = (w - self.px(80.0)).min(self.px(1240.0));
        let ph = h - self.px(80.0);
        let px = (w - pw) / 2;
        let py = self.px(40.0);
        let panel = Rect::new(px, py, pw, ph);
        self.scene.fill_round_rect(panel, self.px(28.0), t.panel);
        self.scene.stroke_round_rect(panel, self.px(28.0), 1, t.accent.alpha(90));
        let m = self.px(36.0);

        self.text(true, 34.0 * self.s, px + m, py + m - self.px(6.0), t.text, "Settings");
        let bw = self.px(130.0);
        let bh = self.px(44.0);
        self.button(Rect::new(px + pw - m - bw, py + m - self.px(4.0), bw, bh), "Save", true, Action::Save);
        self.button(Rect::new(px + pw - m - bw * 2 - self.px(14.0), py + m - self.px(4.0), bw, bh), "Cancel", false, Action::Cancel);

        // Selectors.
        let labels = ["Theme", "Background", "Auto-start after", "Default entry"];
        let sy = py + m + self.px(70.0);
        let gap = self.px(16.0);
        let sw = (pw - 2 * m - gap * (SELECTORS as i32 - 1)) / SELECTORS as i32;
        let sh = self.px(74.0);
        for i in 0..SELECTORS {
            let x = px + m + i as i32 * (sw + gap);
            let r = Rect::new(x, sy, sw, sh);
            // Registered first: hit testing picks the last match, so the arrows
            // drawn below must come after the box that contains them.
            self.hits.push(Hit { rect: r, action: Action::FocusRow(usize::MAX - i) });
            let focused = self.focus == i;
            self.scene.fill_round_rect(r, self.px(14.0), t.tile);
            if focused {
                self.scene.stroke_round_rect(r, self.px(14.0), 2, t.accent);
            }
            self.text(false, 14.0 * self.s, x + self.px(14.0), sy + self.px(8.0), t.text_dim, labels[i]);
            let arrow = self.px(34.0);
            let la = Rect::new(x + self.px(6.0), sy + self.px(30.0), arrow, arrow);
            let ra = Rect::new(x + sw - self.px(6.0) - arrow, sy + self.px(30.0), arrow, arrow);
            for (ar, dir, name) in [(la, -1, "ui_left"), (ra, 1, "ui_right")] {
                let a = Action::Selector(i, dir);
                let hovered = self.is_hover(a);
                if hovered {
                    self.scene.fill_round_rect(ar, self.px(10.0), t.tile_hover);
                }
                self.draw_ui_icon(name, ar.x + arrow / 2, ar.y + arrow / 2, self.px(22.0), if hovered { t.accent } else { t.text });
                self.hits.push(Hit { rect: ar, action: a });
            }
            let val = self.selector_value(i);
            let fs = 21.0 * self.s;
            let val = font::fit(&self.fonts.bold, fs, &val, sw - 2 * arrow - self.px(24.0));
            self.text_c(true, fs, x + sw / 2, sy + self.px(36.0), t.text, &val);
        }

        // List.
        let ly = sy + sh + self.px(34.0);
        self.text(true, 22.0 * self.s, px + m, ly, t.text, "Boot entries");
        let hint = "checkbox = shown on the main screen  ·  click a name to rename  ·  arrows reorder";
        let hint = font::fit(&self.fonts.regular, 15.0 * self.s, hint, pw - 2 * m - self.px(170.0));
        self.text(false, 15.0 * self.s, px + m + self.px(160.0), ly + self.px(6.0), t.text_dim, &hint);

        let list_top = ly + self.px(44.0);
        let list_bottom = py + ph - self.px(58.0);
        let row_h = self.px(62.0);
        let max_rows = ((list_bottom - list_top) / row_h).max(1) as usize;
        if self.focus >= SELECTORS {
            let row = self.focus - SELECTORS;
            if row < self.scroll {
                self.scroll = row;
            } else if row >= self.scroll + max_rows {
                self.scroll = row + 1 - max_rows;
            }
        }
        let n = self.items.len();
        self.scroll = self.scroll.min(n.saturating_sub(max_rows));
        let end = (self.scroll + max_rows).min(n);
        for i in self.scroll..end {
            let y = list_top + (i - self.scroll) as i32 * row_h;
            let r = Rect::new(px + m, y, pw - 2 * m, row_h - self.px(6.0));
            let focused = self.focus == SELECTORS + i;
            self.hits.push(Hit { rect: r, action: Action::FocusRow(i) });
            if focused {
                self.scene.fill_round_rect(r, self.px(12.0), t.tile_hover);
                self.scene.stroke_round_rect(r, self.px(12.0), 1, t.accent.alpha(160));
            } else if i % 2 == 0 {
                self.scene.fill_round_rect(r, self.px(12.0), t.tile.alpha(10));
            }
            let item = self.items[i].clone();
            let entry = &self.scan.entries[item.entry];
            // checkbox
            let cb = self.px(28.0);
            let cbr = Rect::new(r.x + self.px(16.0), r.y + (r.h - cb) / 2, cb, cb);
            let cba = Action::ToggleHidden(i);
            if !item.hidden {
                self.scene.fill_round_rect(cbr, self.px(7.0), t.accent);
                self.draw_ui_icon("ui_check", cbr.x + cb / 2, cbr.y + cb / 2, self.px(20.0), t.bg0);
            } else {
                self.scene.stroke_round_rect(cbr, self.px(7.0), 2, if self.is_hover(cba) { t.accent } else { t.text_dim });
            }
            self.hits.push(Hit { rect: cbr.inset(-self.px(8.0)), action: cba });
            // icon
            let isz = self.px(40.0);
            self.draw_icon(&item.icon, r.x + self.px(80.0), r.y + r.h / 2, isz, &item.alias);
            // alias / editing
            let tx = r.x + self.px(118.0);
            let right_controls = self.px(100.0);
            let kind_w = self.px(150.0);
            let text_w = r.w - (tx - r.x) - right_controls - kind_w;
            let fs = 22.0 * self.s;
            let ea = Action::EditAlias(i);
            if let (true, Some(buf)) = (focused, self.editing.as_ref()) {
                let er = Rect::new(tx - self.px(8.0), r.y + self.px(6.0), text_w + self.px(8.0), self.px(34.0));
                self.scene.fill_round_rect(er, self.px(8.0), t.bg0.alpha(200));
                self.scene.stroke_round_rect(er, self.px(8.0), 2, t.accent);
                let shown = format!("{}|", buf);
                self.text(true, fs, tx, r.y + self.px(8.0), t.text, &shown);
            } else {
                let name = font::fit(&self.fonts.bold, fs, &item.alias, text_w);
                let color = if self.is_hover(ea) { t.accent } else { t.text };
                let tw = self.text(true, fs, tx, r.y + self.px(8.0), color, &name);
                if self.is_hover(ea) {
                    self.draw_ui_icon("ui_edit", tx + tw + self.px(18.0), r.y + self.px(22.0), self.px(18.0), t.accent);
                }
                self.hits.push(Hit { rect: Rect::new(tx - self.px(8.0), r.y, text_w + self.px(8.0), r.h), action: ea });
            }
            let detail = if entry.title == item.alias { entry.detail.clone() } else { format!("{}  ·  {}", entry.title, entry.detail) };

            let detail = font::fit(&self.fonts.regular, 14.0 * self.s, &detail, text_w);
            self.text(false, 14.0 * self.s, tx, r.y + self.px(36.0), t.text_dim, &detail);
            // kind tag
            let tag = match entry.kind {
                Kind::Efi { .. } => "EFI loader",
                Kind::LinuxBls { .. } => "systemd-boot",
                Kind::BootNext(_) => "Firmware option",
            };
            let is_default = entry.id == self.effective_default();
            let tag = if is_default { format!("{} · default", tag) } else { tag.to_string() };
            let tagx = r.x + r.w - right_controls - kind_w;
            self.text(false, 14.0 * self.s, tagx, r.y + self.px(20.0), if is_default { t.accent } else { t.text_dim }, &tag);
            // reorder arrows
            let asz = self.px(36.0);
            for (k, (name, act)) in [("ui_up", Action::MoveUp(i)), ("ui_down", Action::MoveDown(i))].into_iter().enumerate() {
                let ar = Rect::new(r.x + r.w - self.px(12.0) - (2 - k as i32) * (asz + self.px(6.0)), r.y + (r.h - asz) / 2, asz, asz);
                let hovered = self.is_hover(act);
                self.scene.fill_round_rect(ar, self.px(9.0), if hovered { t.tile_hover } else { t.tile });
                self.draw_ui_icon(name, ar.x + asz / 2, ar.y + asz / 2, self.px(20.0), if hovered { t.accent } else { t.text });
                self.hits.push(Hit { rect: ar, action: act });
            }
        }
        // Scroll indicators.
        if self.scroll > 0 {
            let r = Rect::new(px + pw - m - self.px(40.0), list_top - self.px(30.0), self.px(40.0), self.px(26.0));
            self.draw_ui_icon("ui_up", r.x + r.w / 2, r.y + r.h / 2, self.px(20.0), t.accent);
            self.hits.push(Hit { rect: r, action: Action::ScrollUp });
        }
        if end < n {
            let r = Rect::new(px + pw - m - self.px(40.0), list_bottom - self.px(2.0), self.px(40.0), self.px(26.0));
            self.draw_ui_icon("ui_down", r.x + r.w / 2, r.y + r.h / 2, self.px(20.0), t.accent);
            self.hits.push(Hit { rect: r, action: Action::ScrollDown });
            self.text(false, 14.0 * self.s, px + m, list_bottom, t.text_dim, &format!("{} more below", n - end));
        }
        let legend = "↑↓ move   Space show/hide   Enter rename   ←→ change value   PgUp/PgDn reorder   F10 save   Esc cancel";
        let legend = font::fit(&self.fonts.regular, 15.0 * self.s, legend, pw - 2 * m);
        self.text_c(false, 15.0 * self.s, w / 2, py + ph - self.px(34.0), t.text_dim.alpha(170), &legend);
        self.draw_status();
    }

    // ----- state changes ------------------------------------------------------------

    fn open_settings(&mut self) {
        self.snapshot = Some(Snapshot {
            items: self.items.clone(),
            theme_idx: self.theme_idx,
            bg_idx: self.bg_idx,
            timeout: self.cfg.timeout,
            default_id: self.default_id.clone(),
        });
        self.screen = Screen::Settings;
        self.focus = SELECTORS;
        self.scroll = 0;
        self.editing = None;
        self.countdown = None;
    }

    fn cancel_settings(&mut self) {
        if let Some(sn) = self.snapshot.take() {
            let bg_changed = sn.theme_idx != self.theme_idx || sn.bg_idx != self.bg_idx;
            self.items = sn.items;
            self.theme_idx = sn.theme_idx;
            self.bg_idx = sn.bg_idx;
            self.cfg.timeout = sn.timeout;
            self.default_id = sn.default_id;
            if bg_changed {
                self.render_background();
            }
        }
        self.editing = None;
        self.screen = Screen::Main;
        self.sel = 0;
    }

    fn save_settings(&mut self) {
        self.snapshot = None;
        self.editing = None;
        self.cfg.theme = THEMES[self.theme_idx].id.to_string();
        self.cfg.background = self.bg_list[self.bg_idx].clone();
        self.cfg.default_entry = self.default_id.clone();
        self.cfg.entries = self
            .items
            .iter()
            .map(|it| {
                let e = &self.scan.entries[it.entry];
                EntryPref {
                    id: e.id.clone(),
                    alias: if it.alias == e.title { String::new() } else { it.alias.clone() },
                    icon: if it.icon == e.icon { String::new() } else { it.icon.clone() },
                    hidden: Some(it.hidden),
                }
            })
            .collect();
        let text = self.cfg.serialize();
        let result = match self.fs.as_mut() {
            Some(f) => {
                let _ = f.create_dir_all(uefi::fs::Path::new(&esp::cpath(CONF_DIR).unwrap()));
                esp::write(f, CONF_PATH, text.as_bytes())
            }
            None => Err("configuration volume not available".into()),
        };
        match result {
            Ok(()) => self.status = Some(("Settings saved".into(), 60, false)),
            Err(e) => self.status = Some((format!("Could not save settings: {}", e), 200, true)),
        }
        self.screen = Screen::Main;
        self.sel = 0;
    }

    fn move_item(&mut self, i: usize, dir: i32) {
        let j = i as i32 + dir;
        if j < 0 || j >= self.items.len() as i32 {
            return;
        }
        self.items.swap(i, j as usize);
        if self.focus == SELECTORS + i {
            self.focus = SELECTORS + j as usize;
        }
    }

    fn act(&mut self, a: Action) -> Option<Outcome> {
        match a {
            Action::Tile(k) => {
                if self.sel == k {
                    let vis = self.visible();
                    let item = vis[k];
                    return Some(self.launch_item(item));
                }
                self.sel = k;
            }
            Action::OpenSettings => self.open_settings(),
            Action::Cancel => self.cancel_settings(),
            Action::Save => self.save_settings(),
            Action::Firmware => return Some(Outcome::Firmware),
            Action::Reboot => return Some(Outcome::Reboot),
            Action::Shutdown => return Some(Outcome::Shutdown),
            Action::ToggleHidden(i) => {
                self.items[i].hidden = !self.items[i].hidden;
                self.focus = SELECTORS + i;
            }
            Action::EditAlias(i) => {
                self.focus = SELECTORS + i;
                self.editing = Some(self.items[i].alias.clone());
            }
            Action::MoveUp(i) => self.move_item(i, -1),
            Action::MoveDown(i) => self.move_item(i, 1),
            Action::Selector(i, dir) => {
                self.focus = i;
                self.change_selector(i, dir);
            }
            Action::FocusRow(i) => {
                if i > usize::MAX - SELECTORS {
                    self.focus = usize::MAX - i;
                } else {
                    self.focus = SELECTORS + i;
                }
                self.editing = None;
            }
            Action::ScrollUp => self.scroll = self.scroll.saturating_sub(1),
            Action::ScrollDown => self.scroll += 1,
            Action::ToggleReveal => {
                self.reveal = !self.reveal;
                self.sel = 0;
            }
        }
        None
    }

    fn key_main(&mut self, key: Key) -> Option<Outcome> {
        let vis = self.visible();
        let n = vis.len();
        let per_row = {
            let tile_w = self.px(200.0) + self.px(56.0);
            ((self.w as i32 - self.px(120.0)) / tile_w).max(1) as usize
        };
        match key {
            Key::Special(ScanCode::RIGHT) => {
                if n > 0 {
                    self.sel = (self.sel + 1) % n;
                }
            }
            Key::Special(ScanCode::LEFT) => {
                if n > 0 {
                    self.sel = (self.sel + n - 1) % n;
                }
            }
            Key::Special(ScanCode::DOWN) => {
                if n > 0 && self.sel + per_row < n {
                    self.sel += per_row;
                }
            }
            Key::Special(ScanCode::UP) => {
                if self.sel >= per_row {
                    self.sel -= per_row;
                }
            }
            Key::Special(ScanCode::ESCAPE) => return self.act(Action::ToggleReveal),
            Key::Special(ScanCode::FUNCTION_2) => return self.act(Action::OpenSettings),
            Key::Special(ScanCode::FUNCTION_12) => {
                if self.scan.fw_setup_supported {
                    return self.act(Action::Firmware);
                }
            }
            Key::Printable(c) => {
                let ch: char = c.into();
                match ch {
                    '\r' | '\n' => {
                        if n > 0 {
                            return Some(self.launch_item(vis[self.sel]));
                        }
                    }
                    's' | 'S' => return self.act(Action::OpenSettings),
                    'h' | 'H' => return self.act(Action::ToggleReveal),
                    'd' | 'D' => self.debug = !self.debug,
                    d @ '1'..='9' => {
                        let k = d as usize - '1' as usize;
                        if k < n {
                            self.sel = k;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        None
    }

    fn key_settings(&mut self, key: Key) -> Option<Outcome> {
        if let Some(buf) = self.editing.as_mut() {
            match key {
                Key::Special(ScanCode::ESCAPE) => self.editing = None,
                Key::Printable(c) => {
                    let ch: char = c.into();
                    match ch {
                        '\r' | '\n' => {
                            let v = self.editing.take().unwrap_or_default();
                            let i = self.focus - SELECTORS;
                            let v = v.trim().to_string();
                            self.items[i].alias = if v.is_empty() { self.scan.entries[self.items[i].entry].title.clone() } else { v };
                        }
                        '\u{8}' => {
                            buf.pop();
                        }
                        c if !c.is_control() && buf.chars().count() < 40 => buf.push(c),
                        _ => {}
                    }
                }
                _ => {}
            }
            return None;
        }
        let n = self.items.len();
        let on_row = self.focus >= SELECTORS;
        match key {
            Key::Special(ScanCode::ESCAPE) => self.cancel_settings(),
            Key::Special(ScanCode::FUNCTION_10) => self.save_settings(),
            Key::Special(ScanCode::DOWN) => {
                if !on_row {
                    if n > 0 {
                        self.focus = SELECTORS;
                    }
                } else if self.focus + 1 < SELECTORS + n {
                    self.focus += 1;
                }
            }
            Key::Special(ScanCode::UP) => {
                if on_row {
                    if self.focus == SELECTORS {
                        self.focus = 0;
                    } else {
                        self.focus -= 1;
                    }
                }
            }
            Key::Special(ScanCode::LEFT) => {
                if !on_row {
                    self.change_selector(self.focus, -1);
                }
            }
            Key::Special(ScanCode::RIGHT) => {
                if !on_row {
                    self.change_selector(self.focus, 1);
                }
            }
            Key::Special(ScanCode::PAGE_UP) => {
                if on_row {
                    self.move_item(self.focus - SELECTORS, -1);
                }
            }
            Key::Special(ScanCode::PAGE_DOWN) => {
                if on_row {
                    self.move_item(self.focus - SELECTORS, 1);
                }
            }
            Key::Printable(c) => {
                let ch: char = c.into();
                match ch {
                    ' ' => {
                        if on_row {
                            let i = self.focus - SELECTORS;
                            self.items[i].hidden = !self.items[i].hidden;
                        } else {
                            self.change_selector(self.focus, 1);
                        }
                    }
                    '\r' | '\n' => {
                        if on_row {
                            let i = self.focus - SELECTORS;
                            self.editing = Some(self.items[i].alias.clone());
                        } else {
                            self.change_selector(self.focus, 1);
                        }
                    }
                    '\t' => {
                        if !on_row {
                            self.focus = (self.focus + 1) % SELECTORS;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        None
    }

    // ----- main loop -----------------------------------------------------------------

    pub fn run(&mut self) -> Outcome {
        self.redraw();
        loop {
            let ev = self.input.next();
            match ev {
                InputEvent::Tick => {
                    let mut dirty = false;
                    if let Some((_, ttl, _)) = self.status.as_mut() {
                        *ttl -= 1;
                        if *ttl <= 0 {
                            self.status = None;
                            dirty = true;
                        }
                    }
                    if let Some(t) = self.countdown {
                        let nt = t - 1;
                        if nt <= 0 {
                            let vis = self.visible();
                            if let Some(&item) = vis.get(self.sel) {
                                return self.launch_item(item);
                            }
                            self.countdown = None;
                        } else {
                            self.countdown = Some(nt);
                            let per_sec = (1000 / TICK_MS) as i32;
                            if nt % per_sec == 0 {
                                dirty = true;
                            }
                        }
                    }
                    if dirty {
                        self.redraw();
                    }
                }
                InputEvent::Move { x, y } => {
                    let cancelled = self.countdown.take().is_some();
                    let h = self.hit_at(x, y);
                    if h != self.hover || cancelled || self.debug {
                        self.hover = h;
                        self.redraw();
                    } else {
                        self.present_cursor();
                    }
                }
                InputEvent::Down { x, y } => {
                    self.countdown = None;
                    if let Some(a) = self.hit_at(x, y) {
                        if let Some(o) = self.act(a) {
                            return o;
                        }
                    } else if self.editing.is_some() {
                        self.editing = None;
                    }
                    self.hover = self.hit_at(x, y);
                    self.redraw();
                }
                InputEvent::Up { .. } => {}
                InputEvent::Key(k) => {
                    self.countdown = None;
                    let r = match self.screen {
                        Screen::Main => self.key_main(k),
                        Screen::Settings => self.key_settings(k),
                    };
                    if let Some(o) = r {
                        return o;
                    }
                    self.redraw();
                }
            }
        }
    }
}

/// Stores the id of the entry being started, for the "Last selected" default.
/// Only writes when it changed, to keep writes to the ESP rare.
fn remember_last(id: &str) {
    let Some(mut fs) = uboot::get_image_file_system(uboot::image_handle()).ok().map(FileSystem::new) else { return };
    let old = esp::read_string(&mut fs, LAST_PATH).unwrap_or_default();
    if old.trim() == id {
        return;
    }
    match esp::write(&mut fs, LAST_PATH, id.as_bytes()) {
        Ok(()) => crate::trace::line(&format!("remembered last entry: {}", id)),
        Err(e) => crate::trace::line(&format!("could not remember last entry: {}", e)),
    }
}

fn pick_mode(gop: &mut GraphicsOutput) {
    let (cw, ch) = gop.current_mode_info().resolution();
    if cw >= 1280 && ch >= 720 {
        return;
    }
    let mut best: Option<(usize, uefi::proto::console::gop::Mode)> = None;
    for mode in gop.modes() {
        let (w, h) = mode.info().resolution();
        if w > 1920 || h > 1200 {
            continue;
        }
        let area = w * h;
        if best.as_ref().map(|b| area > b.0).unwrap_or(true) {
            best = Some((area, mode));
        }
    }
    if let Some((_, m)) = best {
        let _ = gop.set_mode(&m);
    }
}

fn build_items(scan: &Scan, cfg: &Config) -> Vec<Item> {
    let mut items = Vec::new();
    let mut used = alloc::vec![false; scan.entries.len()];
    for pref in &cfg.entries {
        if let Some(i) = scan.entries.iter().position(|e| e.id == pref.id) {
            if used[i] {
                continue;
            }
            used[i] = true;
            let e = &scan.entries[i];
            items.push(Item {
                entry: i,
                alias: if pref.alias.is_empty() { e.title.clone() } else { pref.alias.clone() },
                icon: if pref.icon.is_empty() { e.icon.clone() } else { pref.icon.clone() },
                hidden: pref.hidden.unwrap_or(e.hidden_default),
            });
        }
    }
    for (i, e) in scan.entries.iter().enumerate() {
        if !used[i] {
            items.push(Item { entry: i, alias: e.title.clone(), icon: e.icon.clone(), hidden: e.hidden_default });
        }
    }
    items
}

fn make_cursor(scale: usize) -> Image {
    const ROWS: [&str; 19] = [
        "X          ",
        "XX         ",
        "X.X        ",
        "X..X       ",
        "X...X      ",
        "X....X     ",
        "X.....X    ",
        "X......X   ",
        "X.......X  ",
        "X........X ",
        "X.....XXXXX",
        "X..X..X    ",
        "X.X X..X   ",
        "XX  X..X   ",
        "X    X..X  ",
        "     X..X  ",
        "      XX   ",
        "           ",
        "           ",
    ];
    let w = 11 * scale;
    let h = ROWS.len() * scale;
    let mut img = Image::new(w, h);
    for (y, row) in ROWS.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            let c = match ch {
                'X' => 0xff10_1010u32,
                '.' => 0xffff_ffffu32,
                _ => 0,
            };
            for dy in 0..scale {
                for dx in 0..scale {
                    img.px[(y * scale + dy) * w + x * scale + dx] = c;
                }
            }
        }
    }
    img
}

/// Runs the picker until something is launched.
pub fn run(scan: &Scan) -> ! {
    let mut status: Option<String> = None;
    loop {
        let fs = uboot::get_image_file_system(uboot::image_handle()).ok().map(FileSystem::new);
        let outcome = match App::new(scan, fs, status.take()) {
            Ok(mut app) => app.run(),
            Err(e) => {
                log::error!("bootpick: {}", e);
                uboot::stall(core::time::Duration::from_secs(5));
                boot::reboot();
            }
        };
        match outcome {
            Outcome::Launch { entry, remember } => {
                let e_ref = &scan.entries[entry];
                if remember {
                    remember_last(&e_ref.id);
                }
                if let Err(e) = boot::launch(&e_ref.kind, &scan.volumes) {
                    crate::trace::line(&format!("launch failed: {}", e));
                    crate::trace::flush();
                    status = Some(format!("Could not start: {}", e));
                }
            }
            Outcome::Firmware => {
                if let Err(e) = boot::firmware_setup() {
                    status = Some(e);
                }
            }
            Outcome::Reboot => {
                crate::trace::line("user: restart");
                crate::trace::flush();
                boot::reboot()
            }
            Outcome::Shutdown => {
                crate::trace::line("user: shut down");
                crate::trace::flush();
                boot::shutdown()
            }
        }
    }
}
