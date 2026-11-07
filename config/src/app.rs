//! The settings window (egui). Edits a staging copy of \EFI\bootpick owned by
//! the user; the privileged helper copies it back on Save. The preview is the
//! real boot menu, rendered by bootpick-core at 1920x1080.

use crate::host::{self, DirStorage, RootOverride};
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
}

enum Job {
    Load,
    Save,
    SetEnabled(bool),
    Uninstall,
}

enum JobResult {
    Loaded(Box<LoadReply>),
    Done(String),
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
        let mut child = Command::new("pkexec")
            .arg(exe)
            .arg("--helper")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start pkexec: {}", e))?;
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
                    JobResult::Loaded(Box::new(LoadReply {
                        esp_device: dirs[0].2.display().to_string(),
                        esp_label: dirs[0].1.clone(),
                        scan: ScanJ::from(&hs.scan),
                        detections: hs.detections,
                        boot_entry: Some((6, demo_enabled, demo_enabled)),
                    }))
                }
                Job::Save => JobResult::Done("Saved (demo mode: nothing was written to an EFI partition)".into()),
                Job::SetEnabled(e) => {
                    demo_enabled = e;
                    JobResult::Done(if e { "bootpick enabled (demo)".into() } else { "bootpick disabled (demo)".into() })
                }
                Job::Uninstall => JobResult::Done("Uninstall is not available in demo mode".into()),
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
                };
                match h.call(&req) {
                    Ok(Reply::Ok { load: Some(l), .. }) => JobResult::Loaded(Box::new(l)),
                    Ok(Reply::Ok { message, .. }) => JobResult::Done(message),
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
    files_changed: bool,
    message: Option<(String, bool)>,
    preview: Option<TextureHandle>,
    preview_dirty: bool,
    show_lang_menu: bool,
    textures: HashMap<String, Option<TextureHandle>>,
    text: Option<TextEngine>,
    confirm_uninstall: bool,
    uninstalled: bool,
    frames_ready: u32,
    shot_requested: bool,
    shot_done: Arc<AtomicBool>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Opts) -> App {
        let staging = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join(format!("bootpick-staging-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&staging);
        let (jtx, jrx) = channel();
        let (rtx, rrx) = channel();
        let ctx = cc.egui_ctx.clone();
        let st = staging.clone();
        std::thread::spawn(move || worker(opts.mode, st, jrx, rtx, ctx));
        let _ = jtx.send(Job::Load);
        // DejaVu as a fallback: arrows, bullets and other symbols egui's
        // default fonts lack.
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert("dejavu".into(), Arc::new(egui::FontData::from_static(include_bytes!("../../assets/DejaVuSans.ttf"))));
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().push("dejavu".into());
        }
        cc.egui_ctx.set_fonts(fonts);
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 7.0);
        cc.egui_ctx.set_global_style(style);
        App {
            opts_screenshot: opts.screenshot,
            open_theme_editor: opts.open_theme_editor,
            staging,
            jobs: jtx,
            results: rrx,
            busy: Some("Waiting for authorization and reading the boot configuration…".into()),
            loaded: None,
            scan: Scan::default(),
            cfg: Config::default(),
            saved_text: String::new(),
            files_changed: false,
            message: None,
            preview: None,
            preview_dirty: false,
            show_lang_menu: opts.preview_lang_menu,
            textures: HashMap::new(),
            text: None,
            confirm_uninstall: false,
            uninstalled: false,
            frames_ready: 0,
            shot_requested: false,
            shot_done: Arc::new(AtomicBool::new(false)),
        }
    }

    fn storage(&self) -> DirStorage {
        DirStorage::new(&self.staging)
    }

    fn conf_path(&self) -> PathBuf {
        self.storage().resolve(bootpick_core::paths::CONF)
    }

    fn dirty(&self) -> bool {
        self.files_changed || self.cfg.serialize() != self.saved_text
    }

    fn on_loaded(&mut self, reply: LoadReply) {
        self.scan = Scan::from(&reply.scan);
        let mut st = self.storage();
        let text = st.read_string(bootpick_core::paths::CONF).unwrap_or_default();
        self.cfg = Config::parse(&text);
        self.saved_text = self.cfg.serialize();
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
            self.message = Some((format!("Detected real system names: {}. Save to keep them.", applied.join(", ")), false));
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
        self.preview_dirty = true;
        self.files_changed = false;
    }

    fn poll_jobs(&mut self) {
        while let Ok(r) = self.results.try_recv() {
            self.busy = None;
            match r {
                JobResult::Loaded(l) => self.on_loaded(*l),
                JobResult::Done(m) => {
                    if m.contains("removed") {
                        self.uninstalled = true;
                    }
                    if m.contains("updated") || m.contains("Saved") {
                        self.saved_text = self.cfg.serialize();
                        self.files_changed = false;
                    }
                    if m.contains("enabled") || m.contains("disabled") {
                        if let Some(l) = self.loaded.as_mut() {
                            if let Some(be) = l.boot_entry.as_mut() {
                                be.1 = m.contains("enabled");
                                be.2 = be.1;
                            }
                        }
                    }
                    self.message = Some((m, false));
                }
                JobResult::Failed(e) => self.message = Some((e, true)),
            }
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
        let tex = self.storage().read(&format!("{}\\{}", dir, file)).and_then(|d| imaging::decode_bmp_rgba(&d, max, max)).map(|(w, h, rgba)| {
            ctx.load_texture(&key, egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba), TextureOptions::LINEAR)
        });
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
        ui.heading("General");
        ui.add_space(4.0);
        egui::Grid::new("general").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
            ui.label("Language");
            let current = langs::resolve(&self.cfg.language, "");
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
                self.preview_dirty = true;
            }
            ui.end_row();

            ui.label("Auto-start after");
            let mut t = self.cfg.timeout as f64;
            if ui.add(egui::Slider::new(&mut t, 5.0..=60.0).step_by(5.0).fixed_decimals(0).suffix(" s")).changed() {
                self.cfg.timeout = normalize_timeout(t as u32);
                self.preview_dirty = true;
            }
            ui.end_row();

            ui.label("Default entry");
            let names: Vec<(String, String)> = std::iter::once((DEFAULT_LAST.to_string(), "Last selected".to_string()))
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
        ui.heading("Theme");
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
            if ui.button("Duplicate").on_hover_text("Create an editable copy of this theme").clicked() {
                let mut n = 1;
                while themes.iter().any(|t| t.id == format!("custom-{}", n)) {
                    n += 1;
                }
                let base = themes[cur].clone();
                let t = Theme { id: format!("custom-{}", n), name: format!("{} (custom {})", base.name, n), ..base };
                self.cfg.theme = t.id.clone();
                self.cfg.themes.push(t);
                self.open_theme_editor = true;
                self.preview_dirty = true;
            }
            let is_custom = !builtin_themes().iter().any(|b| b.id == themes[cur].id);
            if is_custom && ui.button("Delete").clicked() {
                let id = themes[cur].id.clone();
                self.cfg.themes.retain(|t| t.id != id);
                self.cfg.theme = "midnight".into();
                self.preview_dirty = true;
            }
        });
        let is_custom = self.cfg.themes.iter().any(|t| t.id == self.cfg.theme);
        egui::CollapsingHeader::new("Colours").open(if self.open_theme_editor { Some(true) } else { None }).show(ui, |ui| {
            self.open_theme_editor = false;
            if !is_custom {
                ui.label(RichText::new("Built-in themes are read-only. Press Duplicate to edit a copy.").weak());
            }
            let theme_id = self.cfg.theme.clone();
            if let Some(t) = self.cfg.themes.iter_mut().find(|t| t.id == theme_id) {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut t.name);
                });
            }
            let current = self.cfg.all_themes().into_iter().find(|t| t.id == theme_id).unwrap_or_else(|| builtin_themes().remove(0));
            egui::Grid::new("colours").num_columns(4).spacing([10.0, 8.0]).show(ui, |ui| {
                for (i, (key, label)) in THEME_FIELDS.iter().enumerate() {
                    let mut c = to_c32(current.color(key).unwrap());
                    ui.add_enabled_ui(is_custom, |ui| {
                        if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::OnlyBlend).changed() {
                            if let Some(t) = self.cfg.themes.iter_mut().find(|t| t.id == theme_id) {
                                t.set_color(key, from_c32(c));
                                self.preview_dirty = true;
                            }
                        }
                    });
                    ui.label(*label);
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        });
    }

    fn section_backgrounds(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading("Background");
        ui.add_space(4.0);
        let mut names: Vec<String> = vec![String::new()];
        names.extend(self.storage().list_dir(bootpick_core::paths::BACKGROUNDS).into_iter().filter(|(n, d)| !d && n.to_ascii_lowercase().ends_with(".bmp")).map(|(n, _)| n));
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
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Gradient", egui::FontId::proportional(13.0), Color32::WHITE);
                    r
                } else {
                    match self.bmp_texture(&ctx, bootpick_core::paths::BACKGROUNDS, n, 256) {
                        Some(t) => ui.add(egui::Image::new((t.id(), thumb)).sense(egui::Sense::click())),
                        None => ui.add_sized(thumb, egui::Button::new(n.as_str())),
                    }
                };
                let stroke = if selected { egui::Stroke::new(3.0, ui.visuals().selection.stroke.color) } else { egui::Stroke::new(1.0, Color32::from_gray(70)) };
                ui.painter().rect_stroke(resp.rect, 4.0, stroke, egui::StrokeKind::Outside);
                let resp = resp.on_hover_text(if n.is_empty() { "Theme gradient".to_string() } else { n.trim_end_matches(".bmp").to_string() });
                if resp.clicked() {
                    self.cfg.background = n.clone();
                    self.preview_dirty = true;
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Add image…").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp"]).pick_file() {
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
                            self.cfg.background = name;
                            self.files_changed = true;
                            self.preview_dirty = true;
                        }
                        Err(e) => self.message = Some((e, true)),
                    }
                }
            }
            if !self.cfg.background.is_empty() && ui.button("Remove selected").clicked() {
                let p = self.storage().resolve(&format!("{}\\{}", bootpick_core::paths::BACKGROUNDS, self.cfg.background));
                let _ = std::fs::remove_file(p);
                self.cfg.background.clear();
                self.textures.retain(|k, _| !k.contains("backgrounds"));
                self.files_changed = true;
                self.preview_dirty = true;
            }
        });
    }

    fn section_entries(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading("Boot entries");
        ui.label(RichText::new("Checked entries appear on the main screen. Names and logos are what the menu shows.").weak());
        ui.add_space(4.0);
        let detections = self.loaded.as_ref().map(|l| l.detections.clone()).unwrap_or_default();
        let n = self.cfg.entries.len();
        let mut swap: Option<(usize, usize)> = None;
        for i in 0..n {
            let id = self.cfg.entries[i].id.clone();
            let Some(entry) = self.scan.entries.iter().find(|e| e.id == id).cloned() else { continue };
            let icon_name = if self.cfg.entries[i].icon.is_empty() { entry.icon.clone() } else { self.cfg.entries[i].icon.clone() };
            let logo = self.bmp_texture(&ctx, bootpick_core::paths::ICONS, &icon_name, 64);
            egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut visible = self.cfg.entries[i].hidden != Some(true);
                    if ui.checkbox(&mut visible, "").on_hover_text("Show on the main screen").changed() {
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
                            detail = format!("Detected: {} {}  ·  {}", d.name, d.version, detail);
                        }
                        ui.label(RichText::new(detail).small().weak());
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(i + 1 < n, egui::Button::new("⬇")).on_hover_text("Move down").clicked() {
                            swap = Some((i, i + 1));
                        }
                        if ui.add_enabled(i > 0, egui::Button::new("⬆")).on_hover_text("Move up").clicked() {
                            swap = Some((i, i - 1));
                        }
                        if !self.cfg.entries[i].icon.is_empty() && ui.button("Reset logo").clicked() {
                            self.cfg.entries[i].icon.clear();
                            self.preview_dirty = true;
                        }
                        if ui.button("Logo…").on_hover_text("Use an image from your computer as this entry's logo").clicked() {
                            if let Some(path) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "svg"]).pick_file() {
                                match imaging::import_logo(&path) {
                                    Ok(bytes) => {
                                        let name = format!("custom-{}", imaging::slug(&path));
                                        let _ = self.storage().write(&format!("{}\\{}.bmp", bootpick_core::paths::ICONS, name), &bytes);
                                        self.textures.retain(|k, _| !k.contains(&name));
                                        self.cfg.entries[i].icon = name;
                                        self.files_changed = true;
                                        self.preview_dirty = true;
                                    }
                                    Err(e) => self.message = Some((e, true)),
                                }
                            }
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

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(RichText::new("bootpick settings").strong());
            if let Some(l) = &self.loaded {
                ui.label(RichText::new(format!("on {} ({})", l.esp_device, l.esp_label)).weak());
                let enabled = l.boot_entry.map(|b| b.1).unwrap_or(false);
                let status = if enabled { RichText::new("● enabled").color(Color32::from_rgb(74, 222, 128)) } else { RichText::new("● disabled").color(Color32::from_rgb(248, 113, 113)) };
                ui.label(status);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = self.busy.is_some() || self.loaded.is_none();
                if ui.add_enabled(!busy, egui::Button::new("Uninstall…")).clicked() {
                    self.confirm_uninstall = true;
                }
                let enabled = self.loaded.as_ref().and_then(|l| l.boot_entry).map(|b| b.1).unwrap_or(false);
                let label = if enabled { "Disable bootpick" } else { "Enable bootpick" };
                if ui
                    .add_enabled(!busy, egui::Button::new(label))
                    .on_hover_text("Disabled: the firmware skips bootpick and starts the next boot loader. Nothing is deleted.")
                    .clicked()
                {
                    self.busy = Some(if enabled { "Disabling…".into() } else { "Enabling…".into() });
                    let _ = self.jobs.send(Job::SetEnabled(!enabled));
                }
            });
        });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let can_save = self.dirty() && self.busy.is_none();
            let save = egui::Button::new(RichText::new("  Save  ").strong()).fill(if can_save { Color32::from_rgb(37, 99, 235) } else { Color32::TRANSPARENT });
            if ui.add_enabled(can_save, save).clicked() {
                self.write_staging_conf();
                self.busy = Some("Saving to the EFI partition…".into());
                let _ = self.jobs.send(Job::Save);
            }
            if ui.add_enabled(self.dirty() && self.busy.is_none(), egui::Button::new("Revert")).clicked() {
                self.busy = Some("Reloading…".into());
                let _ = self.jobs.send(Job::Load);
            }
            if let Some(b) = &self.busy {
                ui.spinner();
                ui.label(b.as_str());
            } else if let Some((m, err)) = &self.message {
                ui.label(if *err { RichText::new(m).color(Color32::from_rgb(248, 113, 113)) } else { RichText::new(m) });
            } else if self.dirty() {
                ui.label(RichText::new("Unsaved changes").weak());
            }
        });
    }

    fn uninstall_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_uninstall {
            return;
        }
        egui::Window::new("Uninstall bootpick?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.label("This removes bootpick from the EFI partition, deletes its firmware boot entry and removes this app.");
            ui.label("Your other boot loaders (GRUB, systemd-boot, Windows) are not touched; the next one in the firmware boot order starts.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    self.confirm_uninstall = false;
                }
                if ui.add(egui::Button::new(RichText::new("Uninstall").color(Color32::WHITE)).fill(Color32::from_rgb(185, 28, 28))).clicked() {
                    self.confirm_uninstall = false;
                    self.busy = Some("Uninstalling…".into());
                    let _ = self.jobs.send(Job::Uninstall);
                }
            });
        });
    }

    fn maybe_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.opts_screenshot.clone() else { return };
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
        if self.uninstalled {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(120.0);
                    ui.heading("bootpick was uninstalled");
                    ui.label("Your previous boot loader starts on the next boot. You can close this window.");
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
                        if ui.button("Try again").clicked() {
                            self.busy = Some("Waiting for authorization…".into());
                            let _ = self.jobs.send(Job::Load);
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
                self.section_general(ui);
                ui.separator();
                self.section_theme(ui);
                ui.separator();
                self.section_backgrounds(ui);
                ui.separator();
                self.section_entries(ui);
                ui.add_space(12.0);
            });
        });
        self.render_preview(&ctx);
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Preview");
                ui.label(RichText::new("how the boot menu will look (not interactive)").weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.checkbox(&mut self.show_lang_menu, "Show language picker").changed() {
                        self.preview_dirty = true;
                    }
                });
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
        self.maybe_screenshot(&ctx);
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.staging);
    }
}
