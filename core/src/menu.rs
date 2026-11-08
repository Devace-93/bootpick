//! The boot menu as a platform-independent state machine: main picker screen,
//! settings screen and language picker. The UEFI shell feeds it input events
//! and presents `scene()`; the desktop app renders it as a live preview.

use crate::bmp;
use crate::config::{normalize_timeout, Config, EntryPref, Theme};
use crate::gfx::{Canvas, Color, Image, Rect};
use crate::i18n::{Msg, Strings};
use crate::langs;
use crate::paths;
use crate::scan::{Entry, Kind, Scan};
use crate::storage::Storage;
use crate::text::TextEngine;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Milliseconds between `tick()` calls.
pub const TICK_MS: u64 = 50;
/// Config value for "start whatever was picked last time".
pub const DEFAULT_LAST: &str = "@last";
/// Two clicks on the same entry within this time start it.
pub const DOUBLE_CLICK_MS: u64 = 500;
const SELECTORS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Backspace,
    Tab,
    PageUp,
    PageDown,
    F2,
    F10,
    F12,
    Char(char),
}

pub enum Outcome {
    Launch { entry: usize, remember: bool },
    Firmware,
    Reboot,
    Shutdown,
}

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
    LangButton,
    Lang(usize),
    /// Outside the language popup: closes it.
    LangBackdrop,
    /// The popup itself (between cells): ignored.
    LangPanel,
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

struct Snapshot {
    items: Vec<Item>,
    theme_idx: usize,
    bg_idx: usize,
    timeout: u32,
    default_id: String,
}

pub struct MenuOptions {
    pub width: usize,
    pub height: usize,
    /// Why the previous launch failed (shown as "Could not start: ...").
    pub status: Option<String>,
    pub has_pointer: bool,
    /// Firmware language (`PlatformLang`), used when no language is saved.
    pub platform_lang: String,
    /// Preview mode: no countdown, no writes.
    pub preview: bool,
}

pub struct Menu {
    w: usize,
    h: usize,
    s: f32,
    scene: Canvas,
    bg: Canvas,
    text: TextEngine,
    storage: Box<dyn Storage>,
    images: BTreeMap<(String, i32), Option<Rc<Image>>>,
    cfg: Config,
    themes: Vec<Theme>,
    theme_idx: usize,
    bg_list: Vec<String>,
    bg_idx: usize,
    scan: Scan,
    items: Vec<Item>,
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
    default_id: String,
    last_id: String,
    lang: &'static str,
    strings: Strings,
    lang_open: bool,
    lang_focus: usize,
    has_pointer: bool,
    preview: bool,
    pub debug: bool,
    debug_line: String,
    /// Milliseconds from the platform clock (set before pointer events).
    now_ms: u64,
    last_click: Option<(usize, u64)>,
}

impl Menu {
    pub fn new(scan: Scan, mut storage: Box<dyn Storage>, opts: MenuOptions) -> Menu {
        let (w, h) = (opts.width, opts.height);
        let s = (h as f32 / 1080.0).clamp(0.55, 2.5);
        let cfg = storage.read_string(paths::CONF).map(|t| Config::parse(&t)).unwrap_or_default();

        let mut text = TextEngine::new();
        for (name, is_dir) in storage.list_dir(paths::FONTS) {
            let l = name.to_ascii_lowercase();
            if !is_dir && (l.ends_with(".ttf") || l.ends_with(".otf")) {
                if let Some(bytes) = storage.read(&format!("{}\\{}", paths::FONTS, name)) {
                    text.add_font(&l, bytes);
                }
            }
        }
        let lang = langs::resolve(&cfg.language, &opts.platform_lang);
        text.set_language(lang);

        let items = build_items(&scan, &cfg);
        let default_id = if cfg.default_entry.is_empty() {
            items.iter().find(|i| !i.hidden).map(|i| scan.entries[i.entry].id.clone()).unwrap_or_default()
        } else {
            cfg.default_entry.clone()
        };
        let last_id = storage.read_string(paths::LAST).map(|s| s.trim().to_string()).unwrap_or_default();

        let mut bg_list = alloc::vec![String::new()];
        for (name, is_dir) in storage.list_dir(paths::BACKGROUNDS) {
            if !is_dir && name.to_ascii_lowercase().ends_with(".bmp") {
                bg_list.push(name);
            }
        }
        let bg_idx = bg_list.iter().position(|b| b.eq_ignore_ascii_case(&cfg.background)).unwrap_or(0);
        let themes = cfg.all_themes();
        let theme_idx = themes.iter().position(|t| t.id == cfg.theme).unwrap_or(0);
        let countdown = if !opts.preview && opts.status.is_none() { Some((cfg.timeout as i32) * 1000 / TICK_MS as i32) } else { None };

        let strings = Strings::for_lang(lang);
        let status = opts.status.map(|m| (strings.fmt(Msg::could_not_start, &[&m]), 200, true));
        let mut m = Menu {
            w,
            h,
            s,
            scene: Canvas::new(w, h),
            bg: Canvas::new(w, h),
            text,
            storage,
            images: BTreeMap::new(),
            cfg,
            themes,
            theme_idx,
            bg_list,
            bg_idx,
            scan,
            items,
            screen: Screen::Main,
            hits: Vec::new(),
            hover: None,
            sel: 0,
            reveal: false,
            countdown,
            status,
            focus: 0,
            scroll: 0,
            editing: None,
            snapshot: None,
            default_id,
            last_id,
            lang,
            strings,
            lang_open: false,
            lang_focus: 0,
            has_pointer: opts.has_pointer,
            preview: opts.preview,
            debug: false,
            debug_line: String::new(),
            now_ms: 0,
            last_click: None,
        };
        let eff = m.effective_default();
        m.sel = m.visible().iter().position(|&i| m.scan.entries[m.items[i].entry].id == eff).unwrap_or(0);
        m.render_background();
        m
    }

    // ----- accessors for the platform shell ------------------------------------------

    pub fn scene(&self) -> &Canvas {
        &self.scene
    }

    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    pub fn mouse_speed(&self) -> u32 {
        self.cfg.mouse_speed
    }

    pub fn entry(&self, i: usize) -> &Entry {
        &self.scan.entries[i]
    }

    pub fn language(&self) -> &'static str {
        self.lang
    }

    pub fn set_debug_line(&mut self, line: String) {
        self.debug_line = line;
    }

    /// Current time in milliseconds, for double-click detection.
    pub fn set_now_ms(&mut self, ms: u64) {
        self.now_ms = ms;
    }

    /// The screen shown while the chosen system starts: the background and a
    /// small loading indicator, nothing else.
    pub fn launching_scene(&mut self) -> &Canvas {
        self.scene.copy_from(&self.bg);
        let t = self.themes[self.theme_idx].clone();
        let (w, h) = (self.w as i32, self.h as i32);
        draw_dots(&mut self.scene, w / 2, h / 2, self.s, t.accent, t.text_dim);
        &self.scene
    }

    pub fn set_has_pointer(&mut self, v: bool) {
        self.has_pointer = v;
    }

    /// Desktop preview: show hidden entries and/or the settings screen.
    pub fn set_preview_view(&mut self, settings: bool, reveal: bool) {
        self.reveal = reveal;
        if settings {
            self.open_settings();
        }
    }

    /// Shows the language picker open (used by the desktop preview).
    pub fn set_lang_menu_open(&mut self, open: bool) {
        self.lang_open = open;
        self.lang_focus = langs::index_of(self.lang).unwrap_or(0);
    }

    pub fn show_status(&mut self, msg: String, error: bool) {
        self.status = Some((msg, if error { 200 } else { 60 }, error));
    }

    /// Records the started entry for the "Last selected" default (only writes
    /// when it changed, to keep writes to the ESP rare).
    pub fn remember_last(&mut self, id: &str) -> Result<bool, String> {
        if self.last_id == id {
            return Ok(false);
        }
        self.storage.write(paths::LAST, id.as_bytes())?;
        self.last_id = id.to_string();
        Ok(true)
    }

    // ----- helpers ------------------------------------------------------------------

    fn theme(&self) -> Theme {
        self.themes[self.theme_idx.min(self.themes.len() - 1)].clone()
    }

    fn has_photo(&self) -> bool {
        self.bg_list.get(self.bg_idx).map(|b| !b.is_empty()).unwrap_or(false)
    }

    fn px(&self, v: f32) -> i32 {
        (v * self.s) as i32
    }

    fn tr(&self, m: Msg) -> &'static str {
        self.strings.get(m)
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

    fn text(&mut self, bold: bool, size: f32, x: i32, y: i32, color: Color, s: &str) -> i32 {
        self.text.draw(&mut self.scene, s, size, bold, x, y, color)
    }

    fn text_c(&mut self, bold: bool, size: f32, cx: i32, y: i32, color: Color, s: &str) -> i32 {
        self.text.draw_centered(&mut self.scene, s, size, bold, cx, y, color)
    }

    fn measure(&self, bold: bool, size: f32, s: &str) -> i32 {
        self.text.measure(s, size, bold)
    }

    fn fit(&self, bold: bool, size: f32, s: &str, max_w: i32) -> String {
        self.text.fit(s, size, bold, max_w)
    }

    // ----- background / images ----------------------------------------------------

    fn render_background(&mut self) {
        let t = self.theme();
        let full = Rect::new(0, 0, self.w as i32, self.h as i32);
        self.bg.fill_gradient(full, t.bg0, t.bg1);
        let name = self.bg_list.get(self.bg_idx).cloned().unwrap_or_default();
        if name.is_empty() {
            return;
        }
        if let Some(img) = self.storage.read(&format!("{}\\{}", paths::BACKGROUNDS, name)).and_then(|d| bmp::decode(&d)) {
            let img = img.cover(self.w, self.h);
            self.bg.blit_opaque(&img, 0, 0);
            // Slight shade so text stays readable on photos.
            let shade = if t.is_light() { Color::rgba(255, 255, 255, 40) } else { Color::rgba(0, 0, 0, 70) };
            self.bg.fill_rect(full, shade);
        }
    }

    fn image(&mut self, dir: &str, name: &str, w: i32, h: i32) -> Option<Rc<Image>> {
        let key = (format!("{}\\{}", dir, name), (w << 16) | h);
        if let Some(c) = self.images.get(&key) {
            return c.clone();
        }
        let loaded = self
            .storage
            .read(&format!("{}\\{}.bmp", dir, name))
            .and_then(|d| bmp::decode(&d))
            .map(|img| Rc::new(img.resized(w as usize, h as usize)));
        self.images.insert(key, loaded.clone());
        loaded
    }

    fn draw_icon(&mut self, name: &str, cx: i32, cy: i32, size: i32, label: &str) {
        match self.image(paths::ICONS, name, size, size) {
            Some(img) => self.scene.blit(&img, cx - size / 2, cy - size / 2),
            None => {
                let t = self.theme();
                self.scene.fill_circle(cx, cy, size / 2 - 4, t.accent.alpha(200));
                let initial: String = label.chars().next().unwrap_or('?').to_uppercase().collect();
                let fs = size as f32 * 0.5;
                let lh = self.text.line_height(fs);
                self.text_c(true, fs, cx, cy - lh / 2, t.bg0, &initial);
            }
        }
    }

    fn draw_ui_icon(&mut self, name: &str, cx: i32, cy: i32, size: i32, color: Color) {
        if let Some(img) = self.image(paths::ICONS, name, size, size) {
            self.scene.blit_tinted(&img, cx - size / 2, cy - size / 2, color);
        } else {
            self.scene.fill_circle(cx, cy, size / 3, color);
        }
    }

    fn draw_flag(&mut self, country: &str, x: i32, y: i32, h: i32) -> i32 {
        let w = h * 4 / 3;
        match self.image(paths::FLAGS, country, w, h) {
            Some(img) => self.scene.blit(&img, x, y),
            None => {
                let t = self.theme();
                self.scene.fill_round_rect(Rect::new(x, y, w, h), 3, t.tile_hover);
            }
        }
        w
    }

    // ----- drawing ----------------------------------------------------------------

    pub fn draw(&mut self) {
        match self.screen {
            Screen::Main => self.draw_main(),
            Screen::Settings => self.draw_settings(),
        }
        if self.debug && !self.debug_line.is_empty() {
            let t = self.theme();
            let line = self.debug_line.clone();
            self.text(false, 14.0 * self.s, self.px(12.0), self.px(8.0), t.text_dim, &line);
        }
    }

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
        let lh = self.text.line_height(fs);
        let label = self.fit(true, fs, label, r.w - self.px(16.0));
        self.text_c(true, fs, r.x + r.w / 2, r.y + (r.h - lh) / 2, fg, &label);
        self.hits.push(Hit { rect: r, action });
    }

    fn tool_width(&self, label: &str) -> i32 {
        self.measure(false, 15.0 * self.s, label).max(self.px(76.0)) + self.px(28.0)
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

    fn lang_button_rect(&self) -> Rect {
        let l = langs::get(langs::index_of(self.lang).unwrap_or(0));
        let fs = 17.0 * self.s;
        let w = self.px(28.0) * 4 / 3 + self.measure(false, fs, l.name) + self.px(78.0);
        Rect::new(self.w as i32 - self.px(24.0) - w, self.px(22.0), w, self.px(46.0))
    }

    fn draw_lang_button(&mut self) {
        let t = self.theme();
        let r = self.lang_button_rect();
        let hovered = self.is_hover(Action::LangButton) || self.lang_open;
        self.scene.fill_round_rect(r, self.px(12.0), if hovered { t.tile_hover } else { t.tile });
        self.scene.stroke_round_rect(r, self.px(12.0), 1, t.text_dim.alpha(90));
        let l = langs::get(langs::index_of(self.lang).unwrap_or(0));
        let fh = self.px(21.0);
        let fx = r.x + self.px(14.0);
        let fw = self.draw_flag(l.country, fx, r.y + (r.h - fh) / 2, fh);
        let fs = 17.0 * self.s;
        let lh = self.text.line_height(fs);
        self.text(false, fs, fx + fw + self.px(10.0), r.y + (r.h - lh) / 2, t.text, l.name);
        let chev = if self.lang_open { "ui_up" } else { "ui_down" };
        self.draw_ui_icon(chev, r.x + r.w - self.px(22.0), r.y + r.h / 2, self.px(16.0), t.text_dim);
        self.hits.push(Hit { rect: r, action: Action::LangButton });
    }

    fn lang_grid(&self) -> (usize, i32, i32, Rect) {
        let cols = if self.w as i32 > self.px(1100.0) { 3 } else { 2 };
        let cell_w = self.px(320.0);
        let cell_h = self.px(44.0);
        let rows = (langs::count() + cols - 1) / cols;
        let pad = self.px(14.0);
        let pw = cols as i32 * cell_w + 2 * pad;
        let ph = rows as i32 * cell_h + 2 * pad;
        let anchor = self.lang_button_rect();
        let x = (anchor.x + anchor.w - pw).max(self.px(16.0));
        let y = anchor.y + anchor.h + self.px(8.0);
        (cols, cell_w, cell_h, Rect::new(x, y, pw, ph))
    }

    fn draw_lang_menu(&mut self) {
        let t = self.theme();
        // Dim everything behind the popup; clicking there closes it.
        let full = Rect::new(0, 0, self.w as i32, self.h as i32);
        self.scene.fill_rect(full, Color::rgba(0, 0, 0, 90));
        self.hits.push(Hit { rect: full, action: Action::LangBackdrop });
        let (cols, cell_w, cell_h, panel) = self.lang_grid();
        self.scene.fill_round_rect(panel, self.px(18.0), t.panel.alpha(250));
        self.scene.stroke_round_rect(panel, self.px(18.0), 1, t.accent.alpha(110));
        self.hits.push(Hit { rect: panel, action: Action::LangPanel });
        let pad = self.px(14.0);
        let current = langs::index_of(self.lang).unwrap_or(0);
        let rows = (langs::count() + cols - 1) / cols;
        for i in 0..langs::count() {
            // Column-major order, like a reading list.
            let col = i / rows;
            let row = i % rows;
            let r = Rect::new(panel.x + pad + col as i32 * cell_w, panel.y + pad + row as i32 * cell_h, cell_w - self.px(6.0), cell_h - self.px(4.0));
            let a = Action::Lang(i);
            let hovered = self.is_hover(a) || self.lang_focus == i;
            if i == current {
                self.scene.fill_round_rect(r, self.px(9.0), t.accent.alpha(60));
            } else if hovered {
                self.scene.fill_round_rect(r, self.px(9.0), t.tile_hover);
            }
            if hovered {
                self.scene.stroke_round_rect(r, self.px(9.0), 1, t.accent.alpha(160));
            }
            let l = langs::get(i);
            let fh = self.px(20.0);
            let fw = self.draw_flag(l.country, r.x + self.px(10.0), r.y + (r.h - fh) / 2, fh);
            let fs = 17.0 * self.s;
            let lh = self.text.line_height(fs);
            let name = self.fit(false, fs, l.name, r.w - fw - self.px(30.0));
            let color = if i == current { t.accent } else { t.text };
            self.text(false, fs, r.x + self.px(20.0) + fw, r.y + (r.h - lh) / 2, color, &name);
            self.hits.push(Hit { rect: r, action: a });
        }
    }

    fn draw_main(&mut self) {
        self.scene.copy_from(&self.bg);
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

        if rows > 0 && self.has_photo() {
            let in_row = per_row.min(vis.len()) as i32;
            let bw = in_row * tile_w + self.px(64.0);
            let bh = rows as i32 * tile_h + self.px(100.0);
            let br = Rect::new((w - bw) / 2, y0 - self.px(32.0), bw, bh);
            self.scene.fill_round_rect(br, self.px(36.0), t.bg0.alpha(150));
        }
        if vis.is_empty() {
            let (a, b) = (self.tr(Msg::no_entries), self.tr(Msg::no_entries_hint));
            self.text_c(true, 30.0 * self.s, w / 2, h / 2 - self.px(40.0), t.text, a);
            self.text_c(false, 20.0 * self.s, w / 2, h / 2 + self.px(4.0), t.text_dim, b);
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
            if selected {
                self.scene.fill_round_rect(r, rad, t.accent.alpha(55));
                self.scene.stroke_round_rect(r, rad, self.px(3.0).max(2), t.accent);
            } else if hovered {
                self.scene.fill_round_rect(r, rad, t.tile_hover);
            } else {
                self.scene.fill_round_rect(r, rad, t.tile);
            }
            let item = self.items[i].clone();
            let cx = r.x + r.w / 2;
            let cy = r.y + self.px(24.0) + icon / 2;
            self.draw_icon(&item.icon, cx, cy, icon, &item.alias);
            let fs = 28.0 * self.s;
            let label = self.fit(true, fs, &item.alias, r.w - self.px(24.0));
            let ly = r.y + self.px(24.0) + icon + self.px(14.0);
            let color = if item.hidden { t.text_dim } else { t.text };
            if self.has_photo() {
                self.text_c(true, fs, cx + 1, ly + 2, t.bg0.alpha(160), &label);
            }
            self.text_c(true, fs, cx, ly, color, &label);
            let location = self.scan.entries[item.entry].location.clone();
            let hidden_word = self.tr(Msg::hidden);
            let sub = match (location.is_empty(), item.hidden) {
                (false, true) => format!("{}  ·  {}", location, hidden_word),
                (false, false) => location,
                (true, true) => hidden_word.to_string(),
                (true, false) => String::new(),
            };
            if !sub.is_empty() {
                let lh = self.text.line_height(fs);
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
            let msg = self.strings.fmt(Msg::starting, &[&name, &format!("{}", secs)]);
            self.text_c(false, fs, w / 2, hint_y, t.text, &msg);
        } else if !vis.is_empty() {
            let msg = if self.has_pointer { self.tr(Msg::click_hint) } else { self.tr(Msg::keys_hint) };
            self.text_c(false, fs, w / 2, hint_y, t.text_dim, msg);
        }

        // Bottom bar.
        if self.has_photo() {
            self.scene.fill_gradient(Rect::new(0, h - self.px(150.0), w, self.px(150.0)), t.bg0.alpha(0), t.bg0.alpha(190));
        }
        let bar_y = h - self.px(104.0);
        let gap = self.px(10.0);
        let mut lx = self.px(24.0);
        let settings = self.tr(Msg::settings);
        lx += self.tool(lx, bar_y, "ui_gear", settings, Action::OpenSettings) + gap;
        let hidden_n = self.items.iter().filter(|i| i.hidden).count();
        if hidden_n > 0 {
            let label = if self.reveal { self.tr(Msg::hide_extras).to_string() } else { self.strings.fmt(Msg::show_hidden, &[&format!("{}", hidden_n)]) };
            self.tool(lx, bar_y, if self.reveal { "ui_eye_off" } else { "ui_eye" }, &label, Action::ToggleReveal);
        }
        let (shut, restart, fw) = (self.tr(Msg::shut_down), self.tr(Msg::restart), self.tr(Msg::firmware));
        let mut rx = w - self.px(24.0);
        rx -= self.tool_width(shut);
        self.tool(rx, bar_y, "ui_power", shut, Action::Shutdown);
        rx -= self.tool_width(restart) + gap;
        self.tool(rx, bar_y, "ui_restart", restart, Action::Reboot);
        if self.scan.fw_setup_supported {
            rx -= self.tool_width(fw) + gap;
            self.tool(rx, bar_y, "ui_firmware", fw, Action::Firmware);
        }

        // Keyboard legend.
        let legend = self.fit(false, 15.0 * self.s, self.tr(Msg::legend_main), w - self.px(80.0));
        self.text_c(false, 15.0 * self.s, w / 2, h - self.px(30.0), t.text_dim.alpha(170), &legend);
        if !self.has_pointer {
            let nm = self.tr(Msg::no_mouse);
            self.text_c(false, 15.0 * self.s, w / 2, h - self.px(52.0), t.text_dim.alpha(170), nm);
        }

        self.draw_lang_button();
        if self.lang_open {
            self.draw_lang_menu();
        }
        self.draw_status();
    }

    fn draw_status(&mut self) {
        if let Some((msg, _, is_err)) = self.status.clone() {
            let t = self.theme();
            let fs = 20.0 * self.s;
            let max = self.w as i32 - self.px(120.0);
            let msg = self.fit(false, fs, &msg, max);
            let tw = self.measure(false, fs, &msg);
            let pw = tw + self.px(48.0);
            let ph = self.px(52.0);
            let r = Rect::new((self.w as i32 - pw) / 2, self.px(28.0), pw, ph);
            let c = if is_err { t.danger.alpha(225) } else { t.accent.alpha(225) };
            self.scene.fill_round_rect(r, self.px(14.0), c);
            let lh = self.text.line_height(fs);
            self.text(false, fs, r.x + self.px(24.0), r.y + (ph - lh) / 2, t.bg0, &msg);
        }
    }

    fn selector_value(&self, i: usize) -> String {
        match i {
            0 => self.theme().name,
            1 => {
                let n = &self.bg_list[self.bg_idx];
                if n.is_empty() {
                    self.tr(Msg::no_background).into()
                } else {
                    n.trim_end_matches(".bmp").trim_end_matches(".BMP").replace(['_', '-'], " ")
                }
            }
            2 => self.strings.fmt(Msg::seconds, &[&format!("{}", self.cfg.timeout)]),
            _ if self.default_id == DEFAULT_LAST => self.tr(Msg::last_selected).into(),
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
                self.theme_idx = (self.theme_idx as i32 + dir).rem_euclid(self.themes.len() as i32) as usize;
                self.render_background();
            }
            1 => {
                self.bg_idx = (self.bg_idx as i32 + dir).rem_euclid(self.bg_list.len() as i32) as usize;
                self.render_background();
            }
            2 => {
                let pos = (normalize_timeout(self.cfg.timeout) / 5 - 1) as i32;
                let np = (pos + dir).rem_euclid(12) as u32;
                self.cfg.timeout = (np + 1) * 5;
            }
            _ => {
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

        let title = self.tr(Msg::settings);
        self.text(true, 34.0 * self.s, px + m, py + m - self.px(6.0), t.text, title);
        let bw = self.px(150.0);
        let bh = self.px(44.0);
        let (save, cancel) = (self.tr(Msg::save), self.tr(Msg::cancel));
        self.button(Rect::new(px + pw - m - bw, py + m - self.px(4.0), bw, bh), save, true, Action::Save);
        self.button(Rect::new(px + pw - m - bw * 2 - self.px(14.0), py + m - self.px(4.0), bw, bh), cancel, false, Action::Cancel);

        let labels = [self.tr(Msg::theme), self.tr(Msg::background), self.tr(Msg::autostart), self.tr(Msg::default_entry)];
        let sy = py + m + self.px(70.0);
        let gap = self.px(16.0);
        let sw = (pw - 2 * m - gap * (SELECTORS as i32 - 1)) / SELECTORS as i32;
        let sh = self.px(74.0);
        for (i, label) in labels.iter().enumerate() {
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
            let label = self.fit(false, 14.0 * self.s, label, sw - self.px(28.0));
            self.text(false, 14.0 * self.s, x + self.px(14.0), sy + self.px(8.0), t.text_dim, &label);
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
            let val = self.fit(true, fs, &val, sw - 2 * arrow - self.px(24.0));
            self.text_c(true, fs, x + sw / 2, sy + self.px(36.0), t.text, &val);
        }

        let ly = sy + sh + self.px(34.0);
        let be = self.tr(Msg::boot_entries);
        let bew = self.text(true, 22.0 * self.s, px + m, ly, t.text, be);
        let hint = self.fit(false, 15.0 * self.s, self.tr(Msg::entries_hint), pw - 2 * m - bew - self.px(30.0));
        self.text(false, 15.0 * self.s, px + m + bew + self.px(20.0), ly + self.px(6.0), t.text_dim, &hint);

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
        let eff_default = self.effective_default();
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
            let entry = self.scan.entries[item.entry].clone();
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
            let isz = self.px(40.0);
            self.draw_icon(&item.icon, r.x + self.px(80.0), r.y + r.h / 2, isz, &item.alias);
            let tx = r.x + self.px(118.0);
            let right_controls = self.px(100.0);
            let kind_w = self.px(180.0);
            let text_w = r.w - (tx - r.x) - right_controls - kind_w;
            let fs = 22.0 * self.s;
            let ea = Action::EditAlias(i);
            if let (true, Some(buf)) = (focused, self.editing.clone()) {
                let er = Rect::new(tx - self.px(8.0), r.y + self.px(6.0), text_w + self.px(8.0), self.px(34.0));
                self.scene.fill_round_rect(er, self.px(8.0), t.bg0.alpha(200));
                self.scene.stroke_round_rect(er, self.px(8.0), 2, t.accent);
                let shown = format!("{}|", buf);
                self.text(true, fs, tx, r.y + self.px(8.0), t.text, &shown);
            } else {
                let name = self.fit(true, fs, &item.alias, text_w);
                let color = if self.is_hover(ea) { t.accent } else { t.text };
                let tw = self.text(true, fs, tx, r.y + self.px(8.0), color, &name);
                if self.is_hover(ea) {
                    self.draw_ui_icon("ui_edit", tx + tw + self.px(18.0), r.y + self.px(22.0), self.px(18.0), t.accent);
                }
                self.hits.push(Hit { rect: Rect::new(tx - self.px(8.0), r.y, text_w + self.px(8.0), r.h), action: ea });
            }
            let detail = if entry.title == item.alias { entry.detail.clone() } else { format!("{}  ·  {}", entry.title, entry.detail) };
            let detail = self.fit(false, 14.0 * self.s, &detail, text_w);
            self.text(false, 14.0 * self.s, tx, r.y + self.px(36.0), t.text_dim, &detail);
            let tag = match entry.kind {
                Kind::Efi { .. } => self.tr(Msg::kind_efi),
                Kind::LinuxBls { .. } => self.tr(Msg::kind_bls),
                Kind::BootNext(_) => self.tr(Msg::kind_fw),
            };
            let is_default = entry.id == eff_default;
            let tag = if is_default { format!("{} · {}", tag, self.tr(Msg::default_tag)) } else { tag.to_string() };
            let tag = self.fit(false, 14.0 * self.s, &tag, kind_w - self.px(10.0));
            let tagx = r.x + r.w - right_controls - kind_w;
            self.text(false, 14.0 * self.s, tagx, r.y + self.px(20.0), if is_default { t.accent } else { t.text_dim }, &tag);
            let asz = self.px(36.0);
            for (k, (name, act)) in [("ui_up", Action::MoveUp(i)), ("ui_down", Action::MoveDown(i))].into_iter().enumerate() {
                let ar = Rect::new(r.x + r.w - self.px(12.0) - (2 - k as i32) * (asz + self.px(6.0)), r.y + (r.h - asz) / 2, asz, asz);
                let hovered = self.is_hover(act);
                self.scene.fill_round_rect(ar, self.px(9.0), if hovered { t.tile_hover } else { t.tile });
                self.draw_ui_icon(name, ar.x + asz / 2, ar.y + asz / 2, self.px(20.0), if hovered { t.accent } else { t.text });
                self.hits.push(Hit { rect: ar, action: act });
            }
        }
        if self.scroll > 0 {
            let r = Rect::new(px + pw - m - self.px(40.0), list_top - self.px(30.0), self.px(40.0), self.px(26.0));
            self.draw_ui_icon("ui_up", r.x + r.w / 2, r.y + r.h / 2, self.px(20.0), t.accent);
            self.hits.push(Hit { rect: r, action: Action::ScrollUp });
        }
        if end < n {
            let r = Rect::new(px + pw - m - self.px(40.0), list_bottom - self.px(2.0), self.px(40.0), self.px(26.0));
            self.draw_ui_icon("ui_down", r.x + r.w / 2, r.y + r.h / 2, self.px(20.0), t.accent);
            self.hits.push(Hit { rect: r, action: Action::ScrollDown });
            let more = self.strings.fmt(Msg::more_below, &[&format!("{}", n - end)]);
            self.text(false, 14.0 * self.s, px + m, list_bottom, t.text_dim, &more);
        }
        let legend = self.fit(false, 15.0 * self.s, self.tr(Msg::legend_settings), pw - 2 * m);
        self.text_c(false, 15.0 * self.s, w / 2, py + ph - self.px(34.0), t.text_dim.alpha(170), &legend);
        self.draw_status();
    }

    // ----- state changes ----------------------------------------------------------

    fn open_settings(&mut self) {
        self.snapshot = Some(Snapshot {
            items: self.items.clone(),
            theme_idx: self.theme_idx,
            bg_idx: self.bg_idx,
            timeout: self.cfg.timeout,
            default_id: self.default_id.clone(),
        });
        self.screen = Screen::Settings;
        self.lang_open = false;
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

    fn write_config(&mut self) -> Result<(), String> {
        if self.preview {
            return Ok(());
        }
        let text = self.cfg.serialize();
        self.storage.write(paths::CONF, text.as_bytes())
    }

    fn save_settings(&mut self) {
        self.snapshot = None;
        self.editing = None;
        self.cfg.theme = self.theme().id;
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
        match self.write_config() {
            Ok(()) => self.status = Some((self.tr(Msg::saved).into(), 60, false)),
            Err(e) => self.status = Some((self.strings.fmt(Msg::could_not_save, &[&e]), 200, true)),
        }
        self.screen = Screen::Main;
        self.sel = 0;
    }

    fn set_language(&mut self, i: usize) {
        let l = langs::get(i);
        self.lang = l.code;
        self.strings = Strings::for_lang(l.code);
        self.text.set_language(l.code);
        self.lang_open = false;
        self.cfg.language = l.code.to_string();
        if let Err(e) = self.write_config() {
            self.status = Some((self.strings.fmt(Msg::could_not_save, &[&e]), 200, true));
        }
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
                let double = matches!(self.last_click, Some((lk, t)) if lk == k && self.now_ms.saturating_sub(t) <= DOUBLE_CLICK_MS);
                if self.sel == k && double {
                    self.last_click = None;
                    let vis = self.visible();
                    return Some(self.launch_item(vis[k]));
                }
                self.sel = k;
                self.last_click = Some((k, self.now_ms));
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
            Action::LangButton => {
                let open = !self.lang_open;
                self.set_lang_menu_open(open);
            }
            Action::Lang(i) => self.set_language(i),
            Action::LangBackdrop => self.lang_open = false,
            Action::LangPanel => {}
        }
        None
    }

    fn key_lang_menu(&mut self, key: Key) {
        let n = langs::count();
        let (cols, ..) = self.lang_grid();
        let rows = (n + cols - 1) / cols;
        match key {
            Key::Esc | Key::Char('l') | Key::Char('L') => self.lang_open = false,
            Key::Down => self.lang_focus = (self.lang_focus + 1).min(n - 1),
            Key::Up => self.lang_focus = self.lang_focus.saturating_sub(1),
            Key::Right => self.lang_focus = (self.lang_focus + rows).min(n - 1),
            Key::Left => self.lang_focus = self.lang_focus.saturating_sub(rows),
            Key::Enter | Key::Char(' ') => self.set_language(self.lang_focus),
            _ => {}
        }
    }

    fn key_main(&mut self, key: Key) -> Option<Outcome> {
        if self.lang_open {
            self.key_lang_menu(key);
            return None;
        }
        let vis = self.visible();
        let n = vis.len();
        let per_row = {
            let tile_w = self.px(200.0) + self.px(56.0);
            ((self.w as i32 - self.px(120.0)) / tile_w).max(1) as usize
        };
        match key {
            Key::Right if n > 0 => self.sel = (self.sel + 1) % n,
            Key::Left if n > 0 => self.sel = (self.sel + n - 1) % n,
            Key::Down if n > 0 && self.sel + per_row < n => self.sel += per_row,
            Key::Up if self.sel >= per_row => self.sel -= per_row,
            Key::Esc => return self.act(Action::ToggleReveal),
            Key::F2 => return self.act(Action::OpenSettings),
            Key::F12 if self.scan.fw_setup_supported => return self.act(Action::Firmware),
            Key::Enter if n > 0 => return Some(self.launch_item(vis[self.sel])),
            Key::Char(c) => match c {
                's' | 'S' => return self.act(Action::OpenSettings),
                'h' | 'H' => return self.act(Action::ToggleReveal),
                'l' | 'L' => self.set_lang_menu_open(true),
                'd' | 'D' => self.debug = !self.debug,
                d @ '1'..='9' => {
                    let k = d as usize - '1' as usize;
                    if k < n {
                        self.sel = k;
                    }
                }
                _ => {}
            },
            _ => {}
        }
        None
    }

    fn key_settings(&mut self, key: Key) -> Option<Outcome> {
        if let Some(buf) = self.editing.as_mut() {
            match key {
                Key::Esc => self.editing = None,
                Key::Enter => {
                    let v = self.editing.take().unwrap_or_default();
                    let i = self.focus - SELECTORS;
                    let v = v.trim().to_string();
                    self.items[i].alias = if v.is_empty() { self.scan.entries[self.items[i].entry].title.clone() } else { v };
                }
                Key::Backspace => {
                    buf.pop();
                }
                Key::Char(c) if !c.is_control() && buf.chars().count() < 40 => buf.push(c),
                _ => {}
            }
            return None;
        }
        let n = self.items.len();
        let on_row = self.focus >= SELECTORS;
        match key {
            Key::Esc => self.cancel_settings(),
            Key::F10 => self.save_settings(),
            Key::Down => {
                if !on_row {
                    if n > 0 {
                        self.focus = SELECTORS;
                    }
                } else if self.focus + 1 < SELECTORS + n {
                    self.focus += 1;
                }
            }
            Key::Up if on_row => {
                if self.focus == SELECTORS {
                    self.focus = 0;
                } else {
                    self.focus -= 1;
                }
            }
            Key::Left if !on_row => self.change_selector(self.focus, -1),
            Key::Right if !on_row => self.change_selector(self.focus, 1),
            Key::PageUp if on_row => self.move_item(self.focus - SELECTORS, -1),
            Key::PageDown if on_row => self.move_item(self.focus - SELECTORS, 1),
            Key::Char(' ') => {
                if on_row {
                    let i = self.focus - SELECTORS;
                    self.items[i].hidden = !self.items[i].hidden;
                } else {
                    self.change_selector(self.focus, 1);
                }
            }
            Key::Enter => {
                if on_row {
                    let i = self.focus - SELECTORS;
                    self.editing = Some(self.items[i].alias.clone());
                } else {
                    self.change_selector(self.focus, 1);
                }
            }
            Key::Tab if !on_row => self.focus = (self.focus + 1) % SELECTORS,
            _ => {}
        }
        None
    }

    // ----- input entry points -------------------------------------------------------

    /// Pointer moved. Returns true when the scene must be redrawn (otherwise
    /// only the cursor moved).
    pub fn pointer_move(&mut self, x: i32, y: i32) -> bool {
        let cancelled = self.countdown.take().is_some();
        let h = self.hit_at(x, y);
        if h != self.hover || cancelled || self.debug {
            self.hover = h;
            return true;
        }
        false
    }

    /// Primary button pressed. The scene must be redrawn afterwards.
    pub fn pointer_down(&mut self, x: i32, y: i32) -> Option<Outcome> {
        self.countdown = None;
        match self.hit_at(x, y) {
            Some(a) => {
                if let Some(o) = self.act(a) {
                    return Some(o);
                }
            }
            None => {
                self.editing = None;
                self.lang_open = false;
            }
        }
        self.hover = self.hit_at(x, y);
        None
    }

    /// A key was pressed. The scene must be redrawn afterwards.
    pub fn key(&mut self, k: Key) -> Option<Outcome> {
        self.countdown = None;
        match self.screen {
            Screen::Main => self.key_main(k),
            Screen::Settings => self.key_settings(k),
        }
    }

    /// Called every `TICK_MS`. Returns an outcome when the countdown expires,
    /// and whether the scene changed.
    pub fn tick(&mut self) -> (Option<Outcome>, bool) {
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
                self.countdown = None;
                let vis = self.visible();
                if let Some(&item) = vis.get(self.sel) {
                    return (Some(self.launch_item(item)), true);
                }
            } else {
                self.countdown = Some(nt);
                if nt % (1000 / TICK_MS) as i32 == 0 {
                    dirty = true;
                }
            }
        }
        (None, dirty)
    }
}

pub fn build_items(scan: &Scan, cfg: &Config) -> Vec<Item> {
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

/// The mouse cursor sprite at an integer scale.
pub fn make_cursor(scale: usize) -> Image {
    const ROWS: [&str; 17] = [
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

/// Three dots, the first highlighted: a static "working on it" sign.
fn draw_dots(c: &mut Canvas, cx: i32, cy: i32, s: f32, on: Color, off: Color) {
    let r = (11.0 * s).max(5.0) as i32;
    let gap = r * 4;
    let pill = Rect::new(cx - gap - r * 3, cy - r * 3, gap * 2 + r * 6, r * 6);
    c.fill_round_rect(pill, r * 3, Color::rgb(0, 0, 0).alpha(120));
    for i in 0..3 {
        let col = if i == 0 { on } else { off.alpha(150) };
        c.fill_circle(cx + (i - 1) * gap, cy, r, col);
    }
}

/// Loading screen while bootpick itself starts (before the menu exists):
/// the theme gradient and a progress bar, `step` of `steps` done.
pub fn splash(w: usize, h: usize, theme: &Theme, step: usize, steps: usize) -> Canvas {
    let mut c = Canvas::new(w, h);
    c.fill_gradient(Rect::new(0, 0, w as i32, h as i32), theme.bg0, theme.bg1);
    let s = h as f32 / 1080.0;
    let bw = (320.0 * s) as i32;
    let bh = (8.0 * s).max(4.0) as i32;
    let x = (w as i32 - bw) / 2;
    let y = h as i32 / 2 - bh / 2;
    c.fill_round_rect(Rect::new(x, y, bw, bh), bh / 2, theme.text_dim.alpha(70));
    let done = (bw as usize * step.min(steps) / steps.max(1)) as i32;
    if done > 0 {
        c.fill_round_rect(Rect::new(x, y, done.max(bh), bh), bh / 2, theme.accent);
    }
    c
}

/// The theme saved in bootpick.conf (for the loading screen).
pub fn saved_theme(storage: &mut dyn Storage) -> Theme {
    let cfg = storage.read_string(paths::CONF).map(|t| Config::parse(&t)).unwrap_or_default();
    cfg.all_themes().into_iter().find(|t| t.id == cfg.theme).unwrap_or_else(|| crate::config::builtin_themes().remove(0))
}
