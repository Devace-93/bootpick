//! The installer window (Windows): what opens when the single-file installer
//! is started, or with `--setup`. It asks for administrator rights once,
//! shows what was found on this computer, and runs the same installation as
//! `--install` (install.rs).

use crate::app::{apply_fonts, bidi_visual, system_fonts};
use crate::i18n_app::{COUNT, T, TABLE};
use crate::install::{self, Survey};
use crate::sys;
use bootpick_core::langs;
use eframe::egui::{self, Color32, RichText, TextureHandle, TextureOptions, Vec2};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const ACCENT: Color32 = Color32::from_rgb(2, 132, 199);
const DANGER: Color32 = Color32::from_rgb(185, 28, 28);
const GOOD: Color32 = Color32::from_rgb(74, 222, 128);
const BAD: Color32 = Color32::from_rgb(248, 113, 113);

#[derive(PartialEq)]
enum Step {
    Checking,
    Ready,
    Installing,
    Done,
    Failed(String),
}

/// What the window is asked to do.
#[derive(Clone, Copy, PartialEq)]
enum Choice {
    /// Install, or update keeping the settings.
    Install,
    /// Update with factory settings.
    Reset,
    /// Remove the boot menu, its boot entry and the settings app.
    Remove,
    /// Remove the settings app only.
    RemoveApp,
}

impl Choice {
    fn removes(self) -> bool {
        matches!(self, Choice::Remove | Choice::RemoveApp)
    }
}

enum Msg {
    Survey(Result<Survey, String>),
    Installed(Result<(), String>),
}

pub fn run(args: &[String]) -> eframe::Result {
    let value = |flag: &str| args.iter().position(|a| a == flag).map(|i| args.get(i + 1).cloned().unwrap_or_default());
    let preview = value("--setup-preview");
    let screenshot = value("--screenshot").map(PathBuf::from);
    if preview.is_none() && !sys::is_elevated() {
        // One administrator prompt; the window then opens with those rights.
        sys::run_elevated("--setup");
        return Ok(());
    }
    let lang = langs::resolve("", &sys::locale());
    let strings = TABLE.iter().find(|(b, _)| *b == langs::base_of(lang)).map(|(_, r)| r).unwrap_or(&TABLE[0].1);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([660.0, 540.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_title(strings[T::su_title as usize])
            .with_app_id("bootpick-setup")
            .with_icon(egui::IconData { rgba: logo(64), width: 64, height: 64 }),
        centered: true,
        ..Default::default()
    };
    eframe::run_native("bootpick setup", options, Box::new(move |cc| Ok(Box::new(Setup::new(cc, lang, strings, preview, screenshot)))))
}

struct Setup {
    strings: &'static [&'static str; COUNT],
    rtl: bool,
    step: Step,
    survey: Option<Survey>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    choice: Choice,
    first: bool,
    app: bool,
    logo: TextureHandle,
    /// Made-up findings; nothing is installed.
    preview: Option<String>,
    screenshot: Option<PathBuf>,
    frames: u32,
    shot_requested: bool,
    shot_done: Arc<AtomicBool>,
}

impl Setup {
    fn new(cc: &eframe::CreationContext<'_>, lang: &'static str, strings: &'static [&'static str; COUNT], preview: Option<String>, screenshot: Option<PathBuf>) -> Setup {
        let ctx = &cc.egui_ctx;
        apply_fonts(ctx, &system_fonts(), langs::base_of(lang));
        let mut style = (*ctx.global_style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(14.0, 7.0);
        for font in style.text_styles.values_mut() {
            font.size *= 1.12;
        }
        ctx.set_global_style(style);
        let n = 160;
        let logo = ctx.load_texture("logo", egui::ColorImage::from_rgba_unmultiplied([n, n], &logo(n)), TextureOptions::LINEAR);
        let (tx, rx) = channel();
        let mut s = Setup {
            strings,
            rtl: langs::is_rtl(lang),
            step: Step::Checking,
            survey: None,
            tx,
            rx,
            choice: Choice::Install,
            first: true,
            app: true,
            logo,
            preview,
            screenshot,
            frames: 0,
            shot_requested: false,
            shot_done: Arc::new(AtomicBool::new(false)),
        };
        match s.preview.as_deref() {
            Some(state) => {
                let warn = state == "warnings";
                let installed = matches!(state, "installed" | "removed" | "app-removed");
                s.survey = Some(Survey { target: "Disk 0 partition 1 (SATA p1)".into(), existing: state == "existing" || warn || installed, app_installed: installed, secure_boot: warn, bitlocker: warn });
                s.choice = match state {
                    "removed" => Choice::Remove,
                    "app-removed" => Choice::RemoveApp,
                    _ => Choice::Install,
                };
                s.step = match state {
                    "done" | "removed" | "app-removed" => Step::Done,
                    "failed" => Step::Failed("not enough free space on Disk 0 partition 1 (SATA p1): 28 MB needed, 11 MB free".into()),
                    "installing" => Step::Installing,
                    _ => Step::Ready,
                };
            }
            None => {
                let (tx, ctx) = (s.tx.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Survey(install::survey()));
                    ctx.request_repaint();
                });
            }
        }
        s
    }

    fn t(&self, k: T) -> String {
        let s = self.strings[k as usize];
        if self.rtl {
            bidi_visual(s)
        } else {
            s.to_string()
        }
    }

    /// With `{0}` replaced. The argument (a partition name) stays a
    /// left-to-right block in right-to-left text.
    fn tf(&self, k: T, arg: &str) -> String {
        let s = self.strings[k as usize];
        if self.rtl {
            return bidi_visual(&s.replace("{0}", "\u{E000}")).replace('\u{E000}', arg);
        }
        s.replace("{0}", arg)
    }

    fn start_install(&mut self, ctx: &egui::Context) {
        self.step = Step::Installing;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        let pretend = self.preview.is_some();
        let choice = self.choice;
        let o = install::Opts { source: None, reset: Some(choice == Choice::Reset), boot_entry: self.first, app: self.app, open_app: false, uninstall: choice == Choice::Remove };
        std::thread::spawn(move || {
            let result = if pretend {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                Ok(())
            } else if choice == Choice::RemoveApp {
                install::remove_app()
            } else {
                install::install(o)
            };
            let _ = tx.send(Msg::Installed(result));
            ctx.request_repaint();
        });
    }

    fn warning(&self, ui: &mut egui::Ui, text: String) {
        egui::Frame::new().fill(Color32::from_rgb(66, 52, 18)).stroke(egui::Stroke::new(1.0, Color32::from_rgb(202, 138, 4))).corner_radius(6.0).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(Color32::from_rgb(253, 230, 138)));
        });
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        match &self.step {
            Step::Checking | Step::Installing => {
                ui.add_space(50.0);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.add_space(6.0);
                    ui.label(self.t(if self.step == Step::Checking {
                        T::su_checking
                    } else if self.choice.removes() {
                        T::uninstalling
                    } else {
                        T::su_installing
                    }));
                });
            }
            Step::Ready => {
                let Some(sv) = &self.survey else { return };
                let (existing, in_windows, bitlocker, secure) = (sv.existing, sv.app_installed, sv.bitlocker, sv.secure_boot);
                let where_to = self.tf(if existing { T::su_existing } else { T::su_target }, &sv.target);
                let (keep, reset, first, app) = (self.t(T::su_keep), self.t(T::su_reset), self.t(T::su_first), self.t(T::su_app));
                let (remove, remove_app, install) = (self.t(if in_windows { T::su_remove_all } else { T::su_remove }), self.t(T::su_remove_app), self.t(T::su_install));
                egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(where_to).strong());
                    if existing {
                        ui.radio_value(&mut self.choice, Choice::Install, keep);
                        ui.radio_value(&mut self.choice, Choice::Reset, reset);
                        ui.radio_value(&mut self.choice, Choice::Remove, remove);
                    } else if in_windows {
                        ui.radio_value(&mut self.choice, Choice::Install, install);
                    }
                    if in_windows {
                        ui.radio_value(&mut self.choice, Choice::RemoveApp, remove_app);
                    }
                });
                if self.choice.removes() {
                    return;
                }
                ui.add_space(4.0);
                ui.checkbox(&mut self.first, first);
                ui.checkbox(&mut self.app, app);
                ui.add_space(4.0);
                if bitlocker && self.first {
                    self.warning(ui, self.t(T::su_bitlocker));
                }
                if secure {
                    self.warning(ui, self.t(T::su_secure));
                }
            }
            Step::Done => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("✔").size(44.0).color(GOOD));
                    ui.add_space(6.0);
                    match self.choice {
                        Choice::Remove => {
                            ui.label(RichText::new(self.t(T::uninstalled_title)).strong());
                            ui.label(self.t(T::uninstalled_body));
                        }
                        Choice::RemoveApp => {
                            ui.label(RichText::new(self.t(T::su_app_removed)).strong());
                        }
                        _ => {
                            ui.label(RichText::new(self.t(T::su_done)).strong());
                        }
                    }
                });
                if !self.choice.removes() && self.survey.as_ref().is_some_and(|s| s.secure_boot) {
                    ui.add_space(14.0);
                    self.warning(ui, self.t(T::su_secure));
                }
            }
            Step::Failed(why) => {
                ui.add_space(30.0);
                ui.label(RichText::new(self.t(T::su_failed)).strong().color(BAD));
                ui.add_space(4.0);
                egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(why.as_str());
                });
            }
        }
    }

    fn buttons(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let primary = |text: String| egui::Button::new(RichText::new(text).strong().color(Color32::WHITE)).fill(ACCENT).min_size(Vec2::new(130.0, 36.0));
        let plain = |text: String| egui::Button::new(text).min_size(Vec2::new(100.0, 36.0));
        // A fixed height: this layout would otherwise take all the room there is.
        ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 36.0), egui::Layout::right_to_left(egui::Align::Center), |ui| match self.step {
            Step::Checking | Step::Installing => {
                ui.add_enabled(false, primary(self.t(T::su_install)));
            }
            Step::Ready => {
                let go = if self.choice.removes() { primary(self.t(T::uninstall)).fill(DANGER) } else { primary(self.t(T::su_install)) };
                if ui.add(go).clicked() {
                    self.start_install(&ctx);
                }
                if ui.add(plain(self.t(T::cancel))).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Step::Done => {
                if self.app && !self.choice.removes() {
                    if ui.add(primary(self.t(T::su_open))).clicked() {
                        if self.preview.is_none() {
                            install::open_app();
                        }
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.add(plain(self.t(T::su_close))).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                } else if ui.add(primary(self.t(T::su_close))).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Step::Failed(_) => {
                if ui.add(primary(self.t(T::su_close))).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        });
    }

    fn maybe_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else { return };
        if self.shot_done.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.frames += 1;
        if self.frames > 15 && !self.shot_requested {
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

impl eframe::App for Setup {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Survey(Ok(s)) => {
                    self.survey = Some(s);
                    self.step = Step::Ready;
                }
                Msg::Installed(Ok(())) => self.step = Step::Done,
                Msg::Survey(Err(e)) | Msg::Installed(Err(e)) => self.step = Step::Failed(e),
            }
        }
        // Not in the middle of writing to the EFI partition.
        if self.step == Step::Installing && self.screenshot.is_none() && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        egui::Panel::bottom("buttons").show(ui, |ui| {
            ui.add_space(10.0);
            self.buttons(ui);
            ui.add_space(10.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::Frame::new().inner_margin(14.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new((self.logo.id(), Vec2::splat(64.0))));
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.add_space(6.0);
                        ui.label(RichText::new(self.t(T::su_title)).size(26.0).strong());
                        ui.label(RichText::new(concat!("bootpick ", env!("CARGO_PKG_VERSION"))).weak());
                    });
                });
                ui.add_space(10.0);
                ui.label(self.t(T::su_intro));
                ui.add_space(10.0);
                self.body(ui);
            });
        });
        self.maybe_screenshot(&ctx);
    }
}

/// The bootpick logo (site/icon.svg) as straight-alpha RGBA, `size` pixels
/// square: the window icon and the picture in the header.
fn logo(size: usize) -> Vec<u8> {
    const SAMPLES: usize = 4;
    // In the 128-unit square of the drawing.
    let rounded = |x: f32, y: f32, x0: f32, y0: f32, w: f32, r: f32| {
        let (cx, cy) = (x.clamp(x0 + r, x0 + w - r), y.clamp(y0 + r, y0 + w - r));
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };
    const ARROW: [(f32, f32); 7] = [(84.0, 78.0), (84.0, 112.0), (92.0, 104.0), (98.0, 116.0), (104.0, 113.0), (98.0, 101.0), (109.0, 101.0)];
    let in_arrow = |x: f32, y: f32, grow: f32| {
        // Even-odd rule; `grow` moves the test point for a rough outline.
        let mut inside = false;
        for i in 0..ARROW.len() {
            let (ax, ay) = ARROW[i];
            let (bx, by) = ARROW[(i + 1) % ARROW.len()];
            if (ay > y) != (by > y) && x + grow < ax + (y - ay) * (bx - ax) / (by - ay) {
                inside = !inside;
            }
        }
        inside
    };
    let mix = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
    let sample = |x: f32, y: f32| -> Option<[f32; 3]> {
        if !rounded(x, y, 6.0, 6.0, 116.0, 26.0) {
            return None;
        }
        let mut c = mix([30.0, 41.0, 59.0], [11.0, 18.0, 32.0], (y - 6.0) / 116.0);
        for (tx, ty, first) in [(24.0, 30.0, true), (70.0, 30.0, false), (24.0, 72.0, false), (70.0, 72.0, false)] {
            if rounded(x, y, tx, ty, 34.0, 8.0) {
                c = if first { [56.0, 189.0, 248.0] } else { mix(c, [241.0, 245.0, 249.0], 0.25) };
            }
        }
        if in_arrow(x, y, 0.0) {
            c = [255.0, 255.0, 255.0];
        } else if [(-1.5, 0.0), (1.5, 0.0), (0.0, -1.5), (0.0, 1.5)].iter().any(|(dx, dy)| in_arrow(x + dx, y + dy, 0.0)) {
            c = [11.0, 18.0, 32.0];
        }
        Some(c)
    };
    let mut out = Vec::with_capacity(size * size * 4);
    let step = 128.0 / (size * SAMPLES) as f32;
    for py in 0..size {
        for px in 0..size {
            let (mut sum, mut hits) = ([0.0f32; 3], 0usize);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    if let Some(c) = sample(((px * SAMPLES + sx) as f32 + 0.5) * step, ((py * SAMPLES + sy) as f32 + 0.5) * step) {
                        sum = [sum[0] + c[0], sum[1] + c[1], sum[2] + c[2]];
                        hits += 1;
                    }
                }
            }
            let n = hits.max(1) as f32;
            out.extend_from_slice(&[(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, (hits * 255 / (SAMPLES * SAMPLES)) as u8]);
        }
    }
    out
}
