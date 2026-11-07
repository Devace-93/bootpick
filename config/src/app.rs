//! The settings window (egui). Edits a staging copy of \EFI\bootpick owned by
//! the user; the privileged helper copies it back on Save. The preview is the
//! real boot menu, rendered by bootpick-core at 1920x1080.

use crate::host::{self, DirStorage, RootOverride};
use crate::i18n_app::{T, TABLE};
use crate::menus::{self, LoaderMenu, MenuKind, Problem};
use crate::imaging;
use crate::proto::{LoadReply, Reply, Request, ScanJ};
use bootpick_core::config::{builtin_themes, normalize_timeout, Config, EntryPref, Theme, THEME_FIELDS};
use bootpick_core::gfx::{Canvas, Color};
use bootpick_core::langs;
use bootpick_core::menu::{build_items, Menu, MenuOptions, DEFAULT_LAST};
use bootpick_core::scan::Scan;
use bootpick_core::storage::Storage;
use bootpick_core::text::TextEngine;
use eframe::egui::{self, Color32, RichText, TextureHandle, TextureOptions, Vec2};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

pub enum Mode {
    /// Talk to the privileged helper through pkexec.
    Real,
    /// Use plain directories as volumes (no root); for demos and screenshots.
    Demo { dirs: Vec<(String, String, PathBuf)>, overrides: RootOverride },
}

pub struct Opts {
    pub mode: Mode,
    pub screenshot: Option<PathBuf>,
    pub open_theme_editor: bool,
    pub preview_lang_menu: bool,
    pub preview_settings: bool,
    pub preview_all: bool,
    pub scroll_entries: bool,
    pub show_dialog: String,
}

#[derive(Clone, PartialEq, Debug)]
enum Job {
    Load,
    Save,
    SetEnabled(bool),
    Uninstall,
    /// Hide (false) or restore (true) another boot loader's menu.
    SetMenu(String, bool),
}

enum JobResult {
    Loaded(Box<LoadReply>),
    /// Success; the boot menus as they are now, when the job may change them.
    Done(Option<Vec<LoaderMenu>>),
    Failed(String),
}

struct HelperProc {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl HelperProc {
    fn spawn() -> Result<HelperProc, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new("pkexec").arg(exe).arg("--helper").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| format!("could not start pkexec: {}", e))?;
        let stdin = child.stdin.take().ok_or("no helper stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no helper stdout")?);
        Ok(HelperProc { child, stdin, stdout })
    }

    fn call(&mut self, req: &Request) -> Result<Reply, String> {
        let line = serde_json::to_string(req).map_err(|e| e.to_string())?;
        writeln!(self.stdin, "{}", line).map_err(|_| "Authorization was cancelled or the helper stopped.".to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        let mut answer = String::new();
        let n = self.stdout.read_line(&mut answer).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("Authorization was cancelled or the helper stopped.".into());
        }
        serde_json::from_str(&answer).map_err(|e| format!("bad helper reply: {}", e))
    }
}

impl Drop for HelperProc {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "{}", serde_json::to_string(&Request::Quit).unwrap_or_default());
        let _ = self.child.wait();
    }
}

/// Copies regular files from `src` to `dst` (demo mode staging).
fn copy_dir(src: &Path, dst: &Path) {
    let _ = std::fs::create_dir_all(dst);
    for e in std::fs::read_dir(src).into_iter().flatten().flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            copy_dir(&e.path(), &dst.join(e.file_name()));
        } else if ft.is_file() && !e.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".efi") {
            let _ = std::fs::copy(e.path(), dst.join(e.file_name()));
        }
    }
}

fn worker(mode: Mode, staging: PathBuf, rx: Receiver<Job>, tx: Sender<JobResult>, ctx: egui::Context) {
    let mut helper: Option<HelperProc> = None;
    let mut demo_enabled = true;
    for job in rx {
        let result = match &mode {
            Mode::Demo { dirs, overrides } => match job {
                Job::Load => {
                    let src = DirStorage::new(&dirs[0].2).resolve("\\EFI\\bootpick");
                    let dst = staging.join("EFI").join("bootpick");
                    let _ = std::fs::remove_dir_all(&dst);
                    copy_dir(&src, &dst);
                    let hs = host::scan_dirs(dirs, overrides);
                    let menus = menus::scan(&menus::Env::Demo { dirs, overrides });
                    JobResult::Loaded(Box::new(LoadReply {
                        esp_device: dirs[0].2.display().to_string(),
                        esp_label: dirs[0].1.clone(),
                        scan: ScanJ::from(&hs.scan),
                        detections: hs.detections,
                        boot_entry: Some((6, demo_enabled, demo_enabled)),
                        menus,
                    }))
                }
                Job::Save => JobResult::Done(None),
                Job::SetEnabled(e) => {
                    demo_enabled = e;
                    let env = menus::Env::Demo { dirs, overrides };
                    if !e {
                        menus::restore_all(&env);
                    }
                    JobResult::Done(Some(menus::scan(&env)))
                }
                Job::Uninstall => JobResult::Done(None),
                Job::SetMenu(id, visible) => {
                    let env = menus::Env::Demo { dirs, overrides };
                    match menus::set(&env, &id, visible) {
                        Ok(()) => JobResult::Done(Some(menus::scan(&env))),
                        Err(e) => JobResult::Failed(e),
                    }
                }
            },
            Mode::Real => {
                if helper.is_none() {
                    match HelperProc::spawn() {
                        Ok(h) => helper = Some(h),
                        Err(e) => {
                            let _ = tx.send(JobResult::Failed(e));
                            ctx.request_repaint();
                            continue;
                        }
                    }
                }
                let h = helper.as_mut().unwrap();
                let staging_s = staging.display().to_string();
                let req = match job {
                    Job::Load => Request::Load { staging: staging_s },
                    Job::Save => Request::Save { staging: staging_s },
                    Job::SetEnabled(enabled) => Request::SetEnabled { enabled },
                    Job::Uninstall => Request::Uninstall,
                    Job::SetMenu(id, visible) => Request::SetMenu { id, visible },
                };
                match h.call(&req) {
                    Ok(Reply::Ok { load: Some(l), .. }) => JobResult::Loaded(Box::new(l)),
                    Ok(Reply::Ok { menus, .. }) => JobResult::Done(menus),
                    Ok(Reply::Error { message }) => JobResult::Failed(message),
                    Err(e) => {
                        helper = None;
                        JobResult::Failed(e)
                    }
                }
            }
        };
        let _ = tx.send(result);
        ctx.request_repaint();
    }
}

fn to_c32(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

fn from_c32(c: Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color::rgba(r, g, b, a)
}

fn canvas_rgba(c: &Canvas) -> Vec<u8> {
    let mut v = Vec::with_capacity(c.w * c.h * 4);
    for p in c.pixels() {
        v.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]);
    }
    v
}

pub struct App {
    opts_screenshot: Option<PathBuf>,
    open_theme_editor: bool,
    staging: PathBuf,
    jobs: Sender<Job>,
    results: Receiver<JobResult>,
    busy: Option<String>,
    loaded: Option<LoadReply>,
    scan: Scan,
    cfg: Config,
    saved_text: String,
    /// Background files currently offered (removals take effect on save).
    bgs: Vec<String>,
    saved_bgs: Vec<String>,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// Last committed state; edits in progress are compared against it.
    committed: Snap,
    message: Option<(String, bool)>,
    preview: Option<TextureHandle>,
    preview_dirty: bool,
    show_lang_menu: bool,
    show_settings: bool,
    show_all: bool,
    scroll_entries: bool,
    textures: HashMap<String, Option<TextureHandle>>,
    text: Option<TextEngine>,
    confirm_uninstall: bool,
    confirm_quit: bool,
    confirm_revert: bool,
    close_after_save: bool,
    allow_close: bool,
    uninstalled: bool,
    lang: &'static str,
    strings: &'static [&'static str; crate::i18n_app::COUNT],
    fonts_lang: &'static str,
    font_files: Vec<(String, Arc<egui::FontData>, &'static str)>,
    pending: Vec<Job>,
    demo: bool,
    ctx: egui::Context,
    frames_ready: u32,
    shot_requested: bool,
    shot_done: Arc<AtomicBool>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Opts) -> App {
        let staging = std::env::var("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir()).join(format!("bootpick-staging-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&staging);
        let demo = matches!(opts.mode, Mode::Demo { .. });
        let (jtx, jrx) = channel();
        let (rtx, rrx) = channel();
        let ctx = cc.egui_ctx.clone();
        let st = staging.clone();
        std::thread::spawn(move || worker(opts.mode, st, jrx, rtx, ctx));
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 7.0);
        cc.egui_ctx.set_global_style(style);
        App {
            opts_screenshot: opts.screenshot,
            open_theme_editor: opts.open_theme_editor,
            staging,
            jobs: jtx,
            results: rrx,
            busy: None,
            loaded: None,
            scan: Scan::default(),
            cfg: Config::default(),
            saved_text: String::new(),
            bgs: Vec::new(),
            saved_bgs: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            committed: Snap::default(),
            message: None,
            preview: None,
            preview_dirty: false,
            show_lang_menu: opts.preview_lang_menu,
            show_settings: opts.preview_settings,
            show_all: opts.preview_all,
            scroll_entries: opts.scroll_entries,
            textures: HashMap::new(),
            text: None,
            confirm_uninstall: false,
            confirm_quit: opts.show_dialog == "quit",
            confirm_revert: opts.show_dialog == "revert",
            close_after_save: false,
            allow_close: false,
            uninstalled: false,
            lang: langs::DEFAULT,
            strings: &TABLE[0].1,
            fonts_lang: "",
            font_files: system_fonts(),
            pending: vec![Job::Load],
            demo,
            ctx: cc.egui_ctx.clone(),
            frames_ready: 0,
            shot_requested: false,
            shot_done: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Starts the first load once the struct exists (so `busy` is localized).
    pub fn start(mut self, ctx: &egui::Context) -> Self {
        let sys = std::env::var("LC_ALL").or_else(|_| std::env::var("LC_MESSAGES")).or_else(|_| std::env::var("LANG")).unwrap_or_default();
        let sys = sys.split('.').next().unwrap_or("").replace('_', "-");
        self.set_lang(langs::resolve("", &sys), ctx);
        self.pending.clear();
        self.send(Job::Load);
        self
    }

    fn t(&self, k: T) -> &'static str {
        let s = self.strings[k as usize];
        if self.rtl() {
            // Leaked once per distinct string; the catalog is small.
            return visual_cache(s);
        }
        s
    }

    fn tf(&self, k: T, args: &[&str]) -> String {
        let mut s = self.strings[k as usize].to_string();
        if self.rtl() {
            // Arguments (paths, names) stay as left-to-right blocks: order the
            // template with placeholder markers, then substitute.
            for i in 0..args.len() {
                s = s.replace(&format!("{{{}}}", i), &char::from_u32(0xE000 + i as u32).unwrap().to_string());
            }
            let mut v = bidi_visual(&s);
            for (i, a) in args.iter().enumerate() {
                v = v.replace(char::from_u32(0xE000 + i as u32).unwrap(), a);
            }
            return v;
        }
        for (i, a) in args.iter().enumerate() {
            s = s.replace(&format!("{{{}}}", i), a);
        }
        s
    }

    fn rtl(&self) -> bool {
        langs::is_rtl(self.lang)
    }

    fn set_lang(&mut self, code: &'static str, ctx: &egui::Context) {
        self.lang = code;
        let base = langs::base_of(code);
        self.strings = TABLE.iter().find(|(b, _)| *b == base).map(|(_, r)| r).unwrap_or(&TABLE[0].1);
        if self.fonts_lang != base {
            apply_fonts(ctx, &self.font_files, base);
            self.fonts_lang = langs::base_of(code);
        }
    }

    fn send(&mut self, job: Job) {
        self.busy = Some(
            match &job {
                Job::Load => self.t(T::waiting_auth),
                Job::Save => self.t(T::saving),
                Job::SetEnabled(true) => self.t(T::enabling),
                Job::SetEnabled(false) => self.t(T::disabling),
                Job::Uninstall => self.t(T::uninstalling),
                Job::SetMenu(_, false) => self.t(T::hiding_menu),
                Job::SetMenu(_, true) => self.t(T::showing_menu),
            }
            .to_string(),
        );
        self.pending.push(job.clone());
        let _ = self.jobs.send(job);
    }

    fn storage(&self) -> DirStorage {
        DirStorage::new(&self.staging)
    }

    fn conf_path(&self) -> PathBuf {
        self.storage().resolve(bootpick_core::paths::CONF)
    }

    fn dirty(&self) -> bool {
        self.bgs != self.saved_bgs || self.cfg.serialize() != self.saved_text
    }

    fn snap(&self) -> Snap {
        Snap { conf: self.cfg.serialize(), bgs: self.bgs.clone() }
    }

    /// Records a history step once an edit settles: no button held and no
    /// text field focused (typing a name is one step, not one per letter).
    fn track_history(&mut self, ctx: &egui::Context) {
        if self.loaded.is_none() || ctx.input(|i| i.pointer.any_down()) || ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let now = self.snap();
        if now != self.committed {
            let prev = std::mem::replace(&mut self.committed, now);
            self.undo.push(prev);
            self.redo.clear();
        }
    }

    fn restore(&mut self, s: Snap) {
        let mut cfg = Config::parse(&s.conf);
        // Keep the preference list explicit, as after loading.
        if cfg.entries.is_empty() {
            cfg.entries = self.cfg.entries.clone();
        }
        self.cfg = cfg;
        self.bgs = s.bgs.clone();
        self.committed = s;
        self.textures.clear();
        self.preview_dirty = true;
    }

    fn do_undo(&mut self) {
        self.track_history(&self.ctx.clone());
        if let Some(s) = self.undo.pop() {
            let cur = self.snap();
            self.redo.push(cur);
            self.restore(s);
        }
    }

    fn do_redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            let cur = self.snap();
            self.undo.push(cur);
            self.restore(s);
        }
    }

    /// Before saving: drop removed backgrounds and custom logos nothing uses,
    /// so the helper deletes them from the EFI partition too.
    fn prune_staging(&self) {
        let mut st = self.storage();
        for (n, dir) in st.list_dir(bootpick_core::paths::BACKGROUNDS) {
            if !dir && n.to_ascii_lowercase().ends_with(".bmp") && !self.bgs.iter().any(|b| b.eq_ignore_ascii_case(&n)) {
                let _ = std::fs::remove_file(st.resolve(&format!("{}\\{}", bootpick_core::paths::BACKGROUNDS, n)));
            }
        }
        for (n, dir) in st.list_dir(bootpick_core::paths::ICONS) {
            let stem = n.trim_end_matches(".bmp").trim_end_matches(".BMP");
            if !dir && stem.starts_with("custom-") && !self.cfg.entries.iter().any(|e| e.icon == stem) {
                let _ = std::fs::remove_file(st.resolve(&format!("{}\\{}", bootpick_core::paths::ICONS, n)));
            }
        }
    }

    fn on_loaded(&mut self, reply: LoadReply) {
        self.scan = Scan::from(&reply.scan);
        let mut st = self.storage();
        let text = st.read_string(bootpick_core::paths::CONF).unwrap_or_default();
        self.cfg = Config::parse(&text);
        self.saved_text = self.cfg.serialize();
        if !self.cfg.language.is_empty() {
            let ctx = self.ctx.clone();
            self.set_lang(langs::resolve(&self.cfg.language, ""), &ctx);
        }
        // Every entry gets an explicit preference, in display order.
        let items = build_items(&self.scan, &self.cfg);
        let mut applied = Vec::new();
        self.cfg.entries = items
            .iter()
            .map(|it| {
                let e = &self.scan.entries[it.entry];
                let mut p = EntryPref {
                    id: e.id.clone(),
                    alias: if it.alias == e.title { String::new() } else { it.alias.clone() },
                    icon: if it.icon == e.icon { String::new() } else { it.icon.clone() },
                    hidden: Some(it.hidden),
                };
                if let Some(d) = reply.detections.get(&e.id) {
                    if p.alias.is_empty() && !d.name.is_empty() && d.name != e.title && !e.title.contains(&d.name) {
                        // Keep qualifiers such as "(grubx64.efi)".
                        p.alias = match e.title.find(" (") {
                            Some(i) => format!("{}{}", d.name, &e.title[i..]),
                            None => d.name.clone(),
                        };
                        if !it.hidden {
                            applied.push(format!("{} → {}", e.title, p.alias));
                        }
                    }
                    if p.icon.is_empty() && d.icon != e.icon && st.exists(&format!("{}\\{}.bmp", bootpick_core::paths::ICONS, d.icon)) {
                        p.icon = d.icon.clone();
                    }
                }
                p
            })
            .collect();
        if !applied.is_empty() {
            self.message = Some((self.tf(T::detected_names, &[&applied.join(", ")]), false));
        }
        let mut engine = TextEngine::new();
        for (name, is_dir) in st.list_dir(bootpick_core::paths::FONTS) {
            if !is_dir {
                if let Some(b) = st.read(&format!("{}\\{}", bootpick_core::paths::FONTS, name)) {
                    engine.add_font(&name.to_ascii_lowercase(), b);
                }
            }
        }
        self.text = Some(engine);
        self.textures.clear();
        self.loaded = Some(reply);
        self.bgs = self.list_bgs();
        self.saved_bgs = self.bgs.clone();
        self.undo.clear();
        self.redo.clear();
        self.committed = self.snap();
        self.preview_dirty = true;
    }

    fn list_bgs(&self) -> Vec<String> {
        let mut v: Vec<String> = self.storage().list_dir(bootpick_core::paths::BACKGROUNDS).into_iter().filter(|(n, d)| !d && n.to_ascii_lowercase().ends_with(".bmp")).map(|(n, _)| n).collect();
        v.sort();
        v
    }

    fn poll_jobs(&mut self) {
        while let Ok(r) = self.results.try_recv() {
            self.busy = None;
            let job = if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) };
            match r {
                JobResult::Loaded(l) => self.on_loaded(*l),
                JobResult::Done(menus) => {
                    let restored = match (&menus, self.loaded.as_ref()) {
                        (Some(new), Some(l)) => l.menus.iter().filter(|m| m.managed).count() > new.iter().filter(|m| m.managed).count(),
                        _ => false,
                    };
                    if let (Some(new), Some(l)) = (menus, self.loaded.as_mut()) {
                        l.menus = new;
                    }
                    self.on_done(job, restored);
                }
                JobResult::Failed(e) => {
                    self.close_after_save = false;
                    self.message = Some((self.tf(T::error, &[&e]), true));
                }
            }
            if self.busy.is_none() && !self.pending.is_empty() {
                self.busy = Some(String::new());
            }
        }
    }

    fn on_done(&mut self, job: Option<Job>, restored: bool) {
        let note = if restored { format!(" {}", self.t(T::menus_restore_note)) } else { String::new() };
        match job {
            Some(Job::Save) => {
                self.saved_text = self.cfg.serialize();
                self.saved_bgs = self.bgs.clone();
                let mut m = self.t(T::saved_ok).to_string();
                if self.demo {
                    m = format!("{} ({})", m, self.t(T::demo_note));
                }
                self.message = Some((m, false));
                if self.close_after_save {
                    self.allow_close = true;
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Some(Job::SetEnabled(enabled)) => {
                if let Some(be) = self.loaded.as_mut().and_then(|l| l.boot_entry.as_mut()) {
                    be.1 = enabled;
                    be.2 = enabled;
                }
                self.message = Some((format!("{}{}", self.t(if enabled { T::enabled_msg } else { T::disabled_msg }), note), false));
            }
            Some(Job::SetMenu(id, visible)) => {
                let name = self.loaded.as_ref().and_then(|l| l.menus.iter().find(|m| m.id == id)).map(|m| m.name.clone()).unwrap_or_default();
                self.message = Some((self.tf(if visible { T::menu_shown_ok } else { T::menu_hidden_ok }, &[&name]), false));
            }
            Some(Job::Uninstall) => self.uninstalled = !self.demo,
            _ => {}
        }
    }

    fn write_staging_conf(&self) {
        let p = self.conf_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, self.cfg.serialize());
    }

    fn render_preview(&mut self, ctx: &egui::Context) {
        if !self.preview_dirty || self.loaded.is_none() {
            return;
        }
        self.preview_dirty = false;
        self.write_staging_conf();
        let opts = MenuOptions { width: 1920, height: 1080, status: None, has_pointer: true, platform_lang: String::new(), preview: true };
        let mut menu = Menu::new(self.scan.clone(), Box::new(self.storage()), opts);
        menu.set_preview_view(self.show_settings, self.show_all);
        menu.set_lang_menu_open(self.show_lang_menu);
        menu.draw();
        let img = egui::ColorImage::from_rgba_unmultiplied([1920, 1080], &canvas_rgba(menu.scene()));
        match self.preview.as_mut() {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => self.preview = Some(ctx.load_texture("preview", img, TextureOptions::LINEAR)),
        }
    }

    /// A texture from one of the staging BMPs (icons, flags, backgrounds).
    fn bmp_texture(&mut self, ctx: &egui::Context, dir: &str, name: &str, max: u32) -> Option<TextureHandle> {
        let key = format!("{}\\{}@{}", dir, name, max);
        if let Some(t) = self.textures.get(&key) {
            return t.clone();
        }
        let file = if name.to_ascii_lowercase().ends_with(".bmp") { name.to_string() } else { format!("{}.bmp", name) };
        let tex = self
            .storage()
            .read(&format!("{}\\{}", dir, file))
            .and_then(|d| imaging::decode_bmp_rgba(&d, max, max))
            .map(|(w, h, rgba)| ctx.load_texture(&key, egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba), TextureOptions::LINEAR));
        self.textures.insert(key, tex.clone());
        tex
    }

    /// Text rendered by the boot menu's own engine (correct shaping for
    /// Devanagari, Thai and Arabic, which egui does not shape).
    fn shaped_text(&mut self, ctx: &egui::Context, s: &str, color: Color32) -> Option<TextureHandle> {
        let key = format!("text:{}:{:?}", s, color);
        if let Some(t) = self.textures.get(&key) {
            return t.clone();
        }
        let engine = self.text.as_ref()?;
        let size = 30.0;
        let w = engine.measure(s, size, false).max(1) as usize + 4;
        let h = engine.line_height(size) as usize + 10;
        let mut c = Canvas::new(w, h);
        engine.draw(&mut c, s, size, false, 2, 3, Color::rgb(255, 255, 255));
        // The canvas is opaque black; use the glyph coverage as alpha.
        let mut rgba = Vec::with_capacity(w * h * 4);
        for p in c.pixels() {
            let a = (*p & 0xff) as u8;
            rgba.extend_from_slice(&[color.r(), color.g(), color.b(), a]);
        }
        let tex = ctx.load_texture(&key, egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba), TextureOptions::LINEAR);
        self.textures.insert(key, Some(tex.clone()));
        Some(tex)
    }

    fn lang_row(&mut self, ui: &mut egui::Ui, code: &str, text_height: f32) -> egui::Response {
        let ctx = ui.ctx().clone();
        let l = langs::get(langs::index_of(code).unwrap_or(0));
        let flag = self.bmp_texture(&ctx, bootpick_core::paths::FLAGS, l.country, 64);
        let name = self.shaped_text(&ctx, l.name, ui.visuals().text_color());
        ui.horizontal(|ui| {
            if let Some(f) = flag {
                ui.add(egui::Image::new((f.id(), Vec2::new(text_height * 4.0 / 3.0, text_height))));
            }
            match name {
                Some(t) => {
                    let s = t.size_vec2();
                    ui.add(egui::Image::new((t.id(), Vec2::new(s.x * text_height / s.y, text_height))))
                }
                None => ui.label(l.name),
            }
        })
        .response
    }

    // ----- sections ---------------------------------------------------------------

    fn section_general(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.t(T::general));
        ui.add_space(4.0);
        egui::Grid::new("general").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
            ui.label(self.t(T::language));
            let current = self.lang;
            let resp = egui::ComboBox::from_id_salt("lang").width(260.0).selected_text(langs::get(langs::index_of(current).unwrap_or(0)).name).show_ui(ui, |ui| {
                let mut chosen = None;
                for i in 0..langs::count() {
                    let l = langs::get(i);
                    let selected = l.code == current;
                    let r = ui.add(egui::Button::selectable(selected, "").min_size(Vec2::new(250.0, 24.0)));
                    let rect = r.rect;
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(6.0, 3.0))));
                    self.lang_row(&mut child, l.code, 18.0);
                    if r.clicked() {
                        chosen = Some(l.code);
                    }
                }
                chosen
            });
            if let Some(Some(code)) = resp.inner {
                self.cfg.language = code.to_string();
                let ctx = ui.ctx().clone();
                self.set_lang(code, &ctx);
                self.preview_dirty = true;
            }
            ui.end_row();

            ui.label(self.t(T::autostart));
            let mut t = self.cfg.timeout as f64;
            if ui.add(egui::Slider::new(&mut t, 5.0..=60.0).step_by(5.0).fixed_decimals(0).suffix(" s")).changed() {
                self.cfg.timeout = normalize_timeout(t as u32);
                self.preview_dirty = true;
            }
            ui.end_row();

            ui.label(self.t(T::default_entry));
            let names: Vec<(String, String)> = std::iter::once((DEFAULT_LAST.to_string(), self.t(T::last_selected).to_string()))
                .chain(self.cfg.entries.iter().filter(|p| p.hidden != Some(true)).filter_map(|p| {
                    let e = self.scan.entries.iter().find(|e| e.id == p.id)?;
                    Some((p.id.clone(), if p.alias.is_empty() { e.title.clone() } else { p.alias.clone() }))
                }))
                .collect();
            let cur = if self.cfg.default_entry.is_empty() { names.get(1).map(|n| n.0.clone()).unwrap_or_default() } else { self.cfg.default_entry.clone() };
            let cur_name = names.iter().find(|n| n.0 == cur).map(|n| n.1.clone()).unwrap_or_else(|| "—".into());
            egui::ComboBox::from_id_salt("default").width(260.0).selected_text(cur_name).show_ui(ui, |ui| {
                for (id, name) in &names {
                    if ui.selectable_label(*id == cur, name).clicked() {
                        self.cfg.default_entry = id.clone();
                        self.preview_dirty = true;
                    }
                }
            });
            ui.end_row();
        });
    }

    fn section_theme(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.t(T::theme));
        ui.add_space(4.0);
        let themes = self.cfg.all_themes();
        let cur = themes.iter().position(|t| t.id == self.cfg.theme).unwrap_or(0);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("theme").width(200.0).selected_text(themes[cur].name.clone()).show_ui(ui, |ui| {
                for t in &themes {
                    if ui.selectable_label(t.id == themes[cur].id, &t.name).clicked() {
                        self.cfg.theme = t.id.clone();
                        self.preview_dirty = true;
                    }
                }
            });
            if ui.button(self.t(T::duplicate)).on_hover_text(self.t(T::duplicate_hint)).clicked() {
                let mut n = 1;
                while themes.iter().any(|t| t.id == format!("custom-{}", n)) {
                    n += 1;
                }
                let base = themes[cur].clone();
                let t = Theme { id: format!("custom-{}", n), name: self.tf(T::custom_theme, &[&base.name, &n.to_string()]), ..base };
                self.cfg.theme = t.id.clone();
                self.cfg.themes.push(t);
                self.open_theme_editor = true;
                self.preview_dirty = true;
            }
            let is_custom = !builtin_themes().iter().any(|b| b.id == themes[cur].id);
            if is_custom && ui.button(self.t(T::delete)).clicked() {
                let id = themes[cur].id.clone();
                self.cfg.themes.retain(|t| t.id != id);
                self.cfg.theme = "midnight".into();
                self.preview_dirty = true;
            }
        });
        let is_custom = self.cfg.themes.iter().any(|t| t.id == self.cfg.theme);
        egui::CollapsingHeader::new(self.t(T::colours)).open(if self.open_theme_editor { Some(true) } else { None }).show(ui, |ui| {
            self.open_theme_editor = false;
            if !is_custom {
                ui.label(RichText::new(self.t(T::builtin_readonly)).weak());
            }
            let theme_id = self.cfg.theme.clone();
            let name_label = self.t(T::name);
            if let Some(t) = self.cfg.themes.iter_mut().find(|t| t.id == theme_id) {
                ui.horizontal(|ui| {
                    ui.label(name_label);
                    ui.text_edit_singleline(&mut t.name);
                });
            }
            let current = self.cfg.all_themes().into_iter().find(|t| t.id == theme_id).unwrap_or_else(|| builtin_themes().remove(0));
            egui::Grid::new("colours").num_columns(4).spacing([10.0, 8.0]).show(ui, |ui| {
                for (i, (key, _)) in THEME_FIELDS.iter().enumerate() {
                    let label = self.t(match *key {
                        "bg0" => T::c_bg0,
                        "bg1" => T::c_bg1,
                        "accent" => T::c_accent,
                        "text" => T::c_text,
                        "text_dim" => T::c_text_dim,
                        "panel" => T::c_panel,
                        "tile" => T::c_tile,
                        "tile_hover" => T::c_tile_hover,
                        _ => T::c_danger,
                    });
                    let mut c = to_c32(current.color(key).unwrap());
                    ui.add_enabled_ui(is_custom, |ui| {
                        if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::OnlyBlend).changed() {
                            if let Some(t) = self.cfg.themes.iter_mut().find(|t| t.id == theme_id) {
                                t.set_color(key, from_c32(c));
                                self.preview_dirty = true;
                            }
                        }
                    });
                    ui.label(label);
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        });
    }

    fn section_backgrounds(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading(self.t(T::background));
        ui.add_space(4.0);
        let mut names: Vec<String> = vec![String::new()];
        names.extend(self.bgs.iter().cloned());
        let thumb = Vec2::new(128.0, 72.0);
        ui.horizontal_wrapped(|ui| {
            for n in &names {
                let selected = self.cfg.background.eq_ignore_ascii_case(n);
                let resp = if n.is_empty() {
                    let (rect, r) = ui.allocate_exact_size(thumb, egui::Sense::click());
                    let t = self.cfg.all_themes().into_iter().find(|t| t.id == self.cfg.theme).unwrap_or_else(|| builtin_themes().remove(0));
                    let mesh = {
                        let mut m = egui::Mesh::default();
                        let (top, bot) = (to_c32(t.bg0), to_c32(t.bg1));
                        m.colored_vertex(rect.left_top(), top);
                        m.colored_vertex(rect.right_top(), top);
                        m.colored_vertex(rect.right_bottom(), bot);
                        m.colored_vertex(rect.left_bottom(), bot);
                        m.add_triangle(0, 1, 2);
                        m.add_triangle(0, 2, 3);
                        m
                    };
                    ui.painter().add(mesh);
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, self.t(T::gradient), egui::FontId::proportional(13.0), Color32::WHITE);
                    r
                } else {
                    match self.bmp_texture(&ctx, bootpick_core::paths::BACKGROUNDS, n, 256) {
                        Some(t) => ui.add(egui::Image::new((t.id(), thumb)).sense(egui::Sense::click())),
                        None => ui.add_sized(thumb, egui::Button::new(n.as_str())),
                    }
                };
                let stroke = if selected { egui::Stroke::new(3.0, ui.visuals().selection.stroke.color) } else { egui::Stroke::new(1.0, Color32::from_gray(70)) };
                ui.painter().rect_stroke(resp.rect, 4.0, stroke, egui::StrokeKind::Outside);
                let resp = resp.on_hover_text(if n.is_empty() { self.t(T::theme_gradient).to_string() } else { n.trim_end_matches(".bmp").to_string() });
                if resp.clicked() {
                    self.cfg.background = n.clone();
                    self.preview_dirty = true;
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button(self.t(T::add_image)).clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter(self.t(T::images), &["png", "jpg", "jpeg", "webp", "bmp"]).pick_file() {
                    match imaging::import_background(&path) {
                        Ok(bytes) => {
                            let mut name = format!("{}.bmp", imaging::slug(&path));
                            let mut st = self.storage();
                            let mut i = 2;
                            while st.exists(&format!("{}\\{}", bootpick_core::paths::BACKGROUNDS, name)) {
                                name = format!("{}-{}.bmp", imaging::slug(&path), i);
                                i += 1;
                            }
                            let _ = st.write(&format!("{}\\{}", bootpick_core::paths::BACKGROUNDS, name), &bytes);
                            self.bgs.push(name.clone());
                            self.cfg.background = name;
                            self.preview_dirty = true;
                        }
                        Err(e) => self.message = Some((e, true)),
                    }
                }
            }
            if !self.cfg.background.is_empty() && ui.button(self.t(T::remove_selected)).clicked() {
                let cur = self.cfg.background.clone();
                self.bgs.retain(|b| !b.eq_ignore_ascii_case(&cur));
                self.cfg.background.clear();
                self.preview_dirty = true;
            }
        });
    }

    fn section_entries(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading(self.t(T::boot_entries));
        ui.label(RichText::new(self.t(T::entries_hint)).weak());
        ui.add_space(4.0);
        let detections = self.loaded.as_ref().map(|l| l.detections.clone()).unwrap_or_default();
        let n = self.cfg.entries.len();
        let mut swap: Option<(usize, usize)> = None;
        for i in 0..n {
            let id = self.cfg.entries[i].id.clone();
            let Some(entry) = self.scan.entries.iter().find(|e| e.id == id).cloned() else {
                continue;
            };
            let icon_name = if self.cfg.entries[i].icon.is_empty() { entry.icon.clone() } else { self.cfg.entries[i].icon.clone() };
            let logo = self.bmp_texture(&ctx, bootpick_core::paths::ICONS, &icon_name, 64);
            egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut visible = self.cfg.entries[i].hidden != Some(true);
                    if ui.checkbox(&mut visible, "").on_hover_text(self.t(T::show_on_main)).changed() {
                        self.cfg.entries[i].hidden = Some(!visible);
                        self.preview_dirty = true;
                    }
                    match logo {
                        Some(t) => {
                            ui.add(egui::Image::new((t.id(), Vec2::splat(36.0))));
                        }
                        None => {
                            ui.add_sized([36.0, 36.0], egui::Label::new("?"));
                        }
                    }
                    ui.vertical(|ui| {
                        let mut alias = if self.cfg.entries[i].alias.is_empty() { entry.title.clone() } else { self.cfg.entries[i].alias.clone() };
                        if ui.add(egui::TextEdit::singleline(&mut alias).desired_width(220.0)).changed() {
                            self.cfg.entries[i].alias = if alias.trim() == entry.title || alias.trim().is_empty() { String::new() } else { alias.trim().to_string() };
                            self.preview_dirty = true;
                        }
                        let mut detail = entry.detail.clone();
                        if let Some(d) = detections.get(&entry.id) {
                            detail = format!("{}  ·  {}", self.tf(T::detected, &[format!("{} {}", d.name, d.version).trim()]), detail);
                        }
                        ui.label(RichText::new(detail).small().weak());
                    });
                });
                // Actions on their own row so long names never collide with them.
                ui.horizontal(|ui| {
                    ui.add_space(70.0);
                    if ui.button(self.t(T::logo_btn)).on_hover_text(self.t(T::logo_hint)).clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter(self.t(T::images), &["png", "jpg", "jpeg", "webp", "bmp", "svg"]).pick_file() {
                            match imaging::import_logo(&path) {
                                Ok(bytes) => {
                                    // A new name per import, so undo can return to the previous logo.
                                    let mut st = self.storage();
                                    let base = format!("custom-{}", imaging::slug(&path));
                                    let mut name = base.clone();
                                    let mut k = 2;
                                    while st.exists(&format!("{}\\{}.bmp", bootpick_core::paths::ICONS, name)) {
                                        name = format!("{}-{}", base, k);
                                        k += 1;
                                    }
                                    let _ = st.write(&format!("{}\\{}.bmp", bootpick_core::paths::ICONS, name), &bytes);
                                    self.cfg.entries[i].icon = name;
                                    self.preview_dirty = true;
                                }
                                Err(e) => self.message = Some((e, true)),
                            }
                        }
                    }
                    if !self.cfg.entries[i].icon.is_empty() && ui.button(self.t(T::reset_logo)).clicked() {
                        self.cfg.entries[i].icon.clear();
                        self.preview_dirty = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(i + 1 < n, egui::Button::new("⬇")).on_hover_text(self.t(T::move_down)).clicked() {
                            swap = Some((i, i + 1));
                        }
                        if ui.add_enabled(i > 0, egui::Button::new("⬆")).on_hover_text(self.t(T::move_up)).clicked() {
                            swap = Some((i, i - 1));
                        }
                    });
                });
            });
        }
        if let Some((a, b)) = swap {
            self.cfg.entries.swap(a, b);
            self.preview_dirty = true;
        }
    }

    fn any_managed(&self) -> bool {
        self.loaded.as_ref().map(|l| l.menus.iter().any(|m| m.managed)).unwrap_or(false)
    }

    fn menu_title(&self, m: &LoaderMenu) -> String {
        self.tf(if m.kind == MenuKind::Grub { T::menu_grub } else { T::menu_sdboot }, &[&m.name])
    }

    /// Callout at the top: menus that still appear after bootpick.
    fn menus_offer(&mut self, ui: &mut egui::Ui) {
        let Some(l) = &self.loaded else { return };
        let hideable: Vec<LoaderMenu> = l.menus.iter().filter(|m| m.can_hide()).cloned().collect();
        if hideable.is_empty() {
            return;
        }
        let names = hideable.iter().map(|m| self.menu_title(m)).collect::<Vec<_>>().join(", ");
        let text = self.tf(T::menus_offer, &[&names]);
        egui::Frame::new().fill(Color32::from_rgb(66, 52, 18)).stroke(egui::Stroke::new(1.0, Color32::from_rgb(202, 138, 4))).corner_radius(6.0).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(text).color(Color32::from_rgb(253, 230, 138)));
                if ui.add_enabled(self.busy.is_none(), egui::Button::new(RichText::new(self.t(T::hide_all)).strong())).clicked() {
                    for m in hideable {
                        self.send(Job::SetMenu(m.id, false));
                    }
                }
            });
        });
        ui.add_space(6.0);
    }

    fn section_menus(&mut self, ui: &mut egui::Ui) {
        ui.heading(self.t(T::menus_title));
        ui.label(RichText::new(self.t(T::menus_hint)).weak());
        ui.add_space(4.0);
        let menus = self.loaded.as_ref().map(|l| l.menus.clone()).unwrap_or_default();
        if menus.is_empty() {
            ui.label(RichText::new(self.t(T::menus_none)).italics());
            return;
        }
        let idle = self.busy.is_none();
        for m in menus {
            egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(self.menu_title(&m)).strong());
                        let status = if let Some(p) = &m.problem {
                            RichText::new(self.t(if *p == Problem::Unreadable { T::menu_unreadable } else { T::menu_no_custom })).color(Color32::from_rgb(248, 113, 113))
                        } else if m.visible {
                            let t = match m.timeout {
                                Some(n) => self.tf(T::menu_shown_for, &[&n.to_string()]),
                                None => self.t(T::menu_shown_wait).to_string(),
                            };
                            RichText::new(format!("● {}", t)).color(Color32::from_rgb(250, 204, 21))
                        } else if m.managed {
                            RichText::new(format!("● {}", self.t(T::menu_hidden_by_us))).color(Color32::from_rgb(74, 222, 128))
                        } else {
                            RichText::new(format!("● {}", self.t(T::menu_hidden))).weak()
                        };
                        ui.label(status.small());
                        ui.label(RichText::new(m.location.clone()).small().weak());
                    });
                });
                ui.horizontal(|ui| {
                    if m.can_hide() && ui.add_enabled(idle, egui::Button::new(self.t(T::hide_menu))).clicked() {
                        self.send(Job::SetMenu(m.id.clone(), false));
                    }
                    if m.managed && ui.add_enabled(idle, egui::Button::new(self.t(T::show_menu))).clicked() {
                        self.send(Job::SetMenu(m.id.clone(), true));
                    }
                });
            });
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(RichText::new(self.t(T::title)).strong());
            if let Some(l) = &self.loaded {
                ui.label(RichText::new(self.tf(T::on_device, &[&l.esp_device, &l.esp_label])).weak());
                let enabled = l.boot_entry.map(|b| b.1).unwrap_or(false);
                let status = if enabled { RichText::new(self.t(T::enabled)).color(Color32::from_rgb(74, 222, 128)) } else { RichText::new(self.t(T::disabled)).color(Color32::from_rgb(248, 113, 113)) };
                ui.label(status);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = self.busy.is_some() || self.loaded.is_none();
                if ui.add_enabled(!busy, egui::Button::new(self.t(T::uninstall_btn))).clicked() {
                    self.confirm_uninstall = true;
                }
                let enabled = self.loaded.as_ref().and_then(|l| l.boot_entry).map(|b| b.1).unwrap_or(false);
                let label = if enabled { self.t(T::disable_btn) } else { self.t(T::enable_btn) };
                if ui.add_enabled(!busy, egui::Button::new(label)).on_hover_text(if self.any_managed() { format!("{} {}", self.t(T::disable_hint), self.t(T::menus_restore_note)) } else { self.t(T::disable_hint).to_string() }).clicked() {
                    self.send(Job::SetEnabled(!enabled));
                }
            });
        });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let can_save = self.dirty() && self.busy.is_none();
            let save = egui::Button::new(RichText::new(format!("  {}  ", self.t(T::save))).strong()).fill(if can_save { Color32::from_rgb(37, 99, 235) } else { Color32::TRANSPARENT });
            if ui.add_enabled(can_save, save).clicked() {
                self.write_staging_conf();
                self.prune_staging();
                self.send(Job::Save);
            }
            if ui.add_enabled(self.dirty() && self.busy.is_none(), egui::Button::new(self.t(T::revert))).clicked() {
                self.confirm_revert = true;
            }
            ui.separator();
            let idle = self.busy.is_none();
            if ui.add_enabled(idle && !self.undo.is_empty(), egui::Button::new(format!("↶ {}", self.t(T::undo)))).on_hover_text("Ctrl+Z").clicked() {
                self.do_undo();
            }
            if ui.add_enabled(idle && !self.redo.is_empty(), egui::Button::new(format!("↷ {}", self.t(T::redo)))).on_hover_text("Ctrl+Shift+Z / Ctrl+Y").clicked() {
                self.do_redo();
            }
            ui.separator();
            if let Some(b) = &self.busy {
                ui.spinner();
                ui.label(b.as_str());
            } else if let Some((m, err)) = &self.message {
                ui.label(if *err { RichText::new(m).color(Color32::from_rgb(248, 113, 113)) } else { RichText::new(m) });
            } else if self.dirty() {
                ui.label(RichText::new(self.t(T::unsaved)).weak());
            }
        });
    }

    /// Closing the window with unsaved changes asks first.
    fn quit_guard(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.loaded.is_some() && !self.uninstalled && self.dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_quit = true;
        }
        if self.confirm_quit {
            let busy = self.busy.is_some();
            egui::Window::new(self.t(T::quit_title)).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.label(self.t(T::unsaved_body));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(self.t(T::cancel)).clicked() {
                        self.confirm_quit = false;
                    }
                    if ui.button(self.t(T::discard_close)).clicked() {
                        self.confirm_quit = false;
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.add_enabled(!busy, egui::Button::new(RichText::new(self.t(T::save_close)).strong().color(Color32::WHITE)).fill(Color32::from_rgb(37, 99, 235))).clicked() {
                        self.confirm_quit = false;
                        self.close_after_save = true;
                        self.write_staging_conf();
                        self.prune_staging();
                        self.send(Job::Save);
                    }
                });
            });
        }
        if self.confirm_revert {
            egui::Window::new(self.t(T::revert_title)).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.label(self.t(T::revert_body));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(self.t(T::cancel)).clicked() {
                        self.confirm_revert = false;
                    }
                    if ui.add(egui::Button::new(RichText::new(self.t(T::revert)).color(Color32::WHITE)).fill(Color32::from_rgb(185, 28, 28))).clicked() {
                        self.confirm_revert = false;
                        self.send(Job::Load);
                    }
                });
            });
        }
    }

    fn uninstall_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_uninstall {
            return;
        }
        egui::Window::new(self.t(T::confirm_title)).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.label(self.t(T::confirm_body1));
            ui.label(self.t(T::confirm_body2));
            if self.any_managed() {
                ui.label(self.t(T::menus_restore_note));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(self.t(T::cancel)).clicked() {
                    self.confirm_uninstall = false;
                }
                if ui.add(egui::Button::new(RichText::new(self.t(T::uninstall)).color(Color32::WHITE)).fill(Color32::from_rgb(185, 28, 28))).clicked() {
                    self.confirm_uninstall = false;
                    self.send(Job::Uninstall);
                }
            });
        });
    }

    fn maybe_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.opts_screenshot.clone() else {
            return;
        };
        if self.shot_done.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.loaded.is_some() && self.preview.is_some() && self.busy.is_none() {
            self.frames_ready += 1;
        }
        if self.frames_ready > 15 && !self.shot_requested {
            self.shot_requested = true;
            let done = self.shot_done.clone();
            let c2 = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                let [w, h] = img.size;
                let mut buf = image::RgbaImage::new(w as u32, h as u32);
                for (i, p) in img.pixels.iter().enumerate() {
                    buf.put_pixel((i % w) as u32, (i / w) as u32, image::Rgba(p.to_srgba_unmultiplied()));
                }
                let _ = buf.save(&path);
                done.store(true, Ordering::SeqCst);
                c2.request_repaint();
            })));
        }
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_jobs();
        if self.busy.is_none() && ctx.memory(|m| m.focused().is_none()) {
            let (undo, redo) = ctx.input_mut(|i| {
                let redo = i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
                let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
                (undo, redo)
            });
            if undo {
                self.do_undo();
            } else if redo {
                self.do_redo();
            }
        }
        if self.uninstalled {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(120.0);
                    ui.heading(self.t(T::uninstalled_title));
                    ui.label(self.t(T::uninstalled_body));
                });
            });
            return;
        }
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            self.top_bar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("bottom").show(ui, |ui| {
            ui.add_space(6.0);
            self.bottom_bar(ui);
            ui.add_space(6.0);
        });
        if self.loaded.is_none() {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(160.0);
                    if self.busy.is_some() {
                        ui.spinner();
                        ui.label(self.busy.clone().unwrap_or_default());
                    } else if let Some((m, _)) = &self.message {
                        ui.label(RichText::new(m).color(Color32::from_rgb(248, 113, 113)));
                        if ui.button(self.t(T::try_again)).clicked() {
                            self.send(Job::Load);
                        }
                    }
                });
            });
            self.maybe_screenshot(&ctx);
            return;
        }
        egui::Panel::left("settings").resizable(true).default_size(560.0).min_size(460.0).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(8.0);
                self.menus_offer(ui);
                self.section_general(ui);
                ui.separator();
                self.section_theme(ui);
                ui.separator();
                self.section_backgrounds(ui);
                ui.separator();
                self.section_entries(ui);
                ui.separator();
                self.section_menus(ui);
                ui.add_space(12.0);
                if self.scroll_entries {
                    ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                }
            });
        });
        self.render_preview(&ctx);
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(self.t(T::preview));
                ui.label(RichText::new(self.t(T::preview_hint)).weak());
            });
            ui.horizontal_wrapped(|ui| {
                let (l1, l2, l3) = (self.t(T::show_lang_picker), self.t(T::show_settings), self.t(T::show_all));
                if ui.checkbox(&mut self.show_all, l3).changed() {
                    self.preview_dirty = true;
                }
                if ui.checkbox(&mut self.show_settings, l2).changed() {
                    self.preview_dirty = true;
                }
                if ui.checkbox(&mut self.show_lang_menu, l1).changed() {
                    self.preview_dirty = true;
                }
            });
            ui.add_space(6.0);
            if let Some(t) = &self.preview {
                let w = ui.available_width();
                let h = (w * 9.0 / 16.0).min(ui.available_height());
                let w = h * 16.0 / 9.0;
                ui.add(egui::Image::new((t.id(), Vec2::new(w, h))).corner_radius(8.0));
            }
        });
        self.uninstall_dialog(&ctx);
        self.quit_guard(&ctx);
        self.track_history(&ctx);
        self.maybe_screenshot(&ctx);
    }
}

#[derive(Clone, Default, PartialEq)]
struct Snap {
    conf: String,
    bgs: Vec<String>,
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.staging);
    }
}

/// System fonts for scripts egui's defaults lack: (name, data, script tag).
fn system_fonts() -> Vec<(String, Arc<egui::FontData>, &'static str)> {
    let mut v = Vec::new();
    let mut add = |name: &str, path: &str, index: u32, tag: &'static str| {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fd = egui::FontData::from_owned(bytes);
            fd.index = index;
            v.push((name.to_string(), Arc::new(fd), tag));
        }
    };
    let cjk = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
    add("noto-cjk-jp", cjk, 0, "ja");
    add("noto-cjk-kr", cjk, 1, "ko");
    add("noto-cjk-sc", cjk, 2, "zh");
    add("noto-deva", "/usr/share/fonts/truetype/noto/NotoSansDevanagari-Regular.ttf", 0, "hi");
    add("noto-thai", "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf", 0, "th");
    add("noto-arab", "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf", 0, "ar");
    v
}

/// egui fonts: defaults, DejaVu, then script fonts with the current
/// language's font first (CJK variants share code points).
fn apply_fonts(ctx: &egui::Context, files: &[(String, Arc<egui::FontData>, &'static str)], base: &str) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("dejavu".into(), Arc::new(egui::FontData::from_static(include_bytes!("../../assets/DejaVuSans.ttf"))));
    let mut order: Vec<String> = vec!["dejavu".into()];
    let mut rest = Vec::new();
    for (name, data, tag) in files {
        fonts.font_data.insert(name.clone(), data.clone());
        if *tag == base {
            order.push(name.clone());
        } else {
            rest.push(name.clone());
        }
    }
    order.extend(rest);
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(fam).or_default().extend(order.iter().cloned());
    }
    ctx.set_fonts(fonts);
}

/// egui shapes text but does not reorder mixed right-to-left / left-to-right
/// runs. Reorders a right-to-left string into visual run order (same
/// simplified bidi as the boot menu) so egui lays the runs out correctly;
/// each Arabic run is still shaped right to left by egui itself.
fn bidi_visual(s: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum D {
        L,
        R,
        N,
    }
    let is_r = |c: char| matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF) && c.is_alphabetic();
    let chars: Vec<char> = s.chars().collect();
    let is_marker = |c: char| (0xE000..0xE010).contains(&(c as u32));
    let cls: Vec<D> = chars
        .iter()
        .map(|&c| {
            if is_r(c) {
                D::R
            } else if c.is_alphanumeric() || is_marker(c) {
                D::L
            } else {
                D::N
            }
        })
        .collect();
    let para = cls.iter().copied().find(|d| *d != D::N).unwrap_or(D::L);
    if para != D::R {
        return s.to_string();
    }
    let mut res = cls.clone();
    for i in 0..res.len() {
        if cls[i] != D::N {
            continue;
        }
        let prev = cls[..i].iter().rev().copied().find(|d| *d != D::N).unwrap_or(para);
        let next = cls[i + 1..].iter().copied().find(|d| *d != D::N).unwrap_or(para);
        res[i] = if prev == next { prev } else { para };
    }
    let mut runs: Vec<(D, String)> = Vec::new();
    for (c, d) in chars.iter().zip(&res) {
        match runs.last_mut() {
            Some((rd, t)) if *rd == *d => t.push(*c),
            _ => runs.push((*d, c.to_string())),
        }
    }
    runs.reverse();
    // egui shapes each word on its own (so an Arabic word comes out right)
    // but lays words out left to right. Inside a right-to-left run, reverse
    // the order of words and punctuation and mirror brackets; words stay as
    // they are.
    let mirror = |c: char| match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        other => other,
    };
    let in_word = |c: char| is_r(c) || matches!(c as u32, 0x0640..=0x065F | 0x0670 | 0x06D6..=0x06ED);
    let mut out = String::new();
    for (d, t) in runs {
        if d != D::R {
            out.push_str(&t);
            continue;
        }
        let mut tokens: Vec<String> = Vec::new();
        let mut word = String::new();
        for c in t.chars() {
            if in_word(c) {
                word.push(c);
            } else {
                if !word.is_empty() {
                    tokens.push(std::mem::take(&mut word));
                }
                tokens.push(mirror(c).to_string());
            }
        }
        if !word.is_empty() {
            tokens.push(word);
        }
        for tok in tokens.into_iter().rev() {
            out.push_str(&tok);
        }
    }
    out
}

fn visual_cache(s: &'static str) -> &'static str {
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<&'static str, &'static str>>> = Mutex::new(None);
    let mut g = CACHE.lock().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    *map.entry(s).or_insert_with(|| Box::leak(bidi_visual(s).into_boxed_str()))
}
