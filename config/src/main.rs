//! bootpick-config: desktop settings for the bootpick boot menu.
//!
//!   bootpick-config                    the settings window (asks for authorization once)
//!   bootpick-config --helper           privileged helper (started by the app through pkexec)
//!   bootpick-config --demo DIR[:LABEL][,DIR2[:LABEL]] [--demo-root UUID=DIR]...
//!                                      use plain directories as EFI volumes (no root)
//!   bootpick-config --screenshot OUT.png [--open-theme-editor] [--preview-lang-menu]
//!                   [--preview-settings] [--preview-all] [--scroll-entries] [--show-dialog quit|revert]

mod app;
mod helper;
mod host;
mod i18n_app;
mod imaging;
mod menus;
mod proto;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--helper") {
        helper::run();
    }
    let mut mode = app::Mode::Real;
    let mut screenshot = None;
    let mut open_theme_editor = false;
    let mut preview_lang_menu = false;
    let mut preview_settings = false;
    let mut preview_all = false;
    let mut scroll_entries = false;
    let mut show_dialog = String::new();
    let mut overrides = host::RootOverride::new();
    let mut demo: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--demo" => {
                demo = args.get(i + 1).cloned();
                i += 1;
            }
            "--demo-root" => {
                if let Some((k, v)) = args.get(i + 1).and_then(|s| s.split_once('=')) {
                    overrides.insert(k.to_string(), PathBuf::from(v));
                }
                i += 1;
            }
            "--screenshot" => {
                screenshot = args.get(i + 1).map(PathBuf::from);
                i += 1;
            }
            "--open-theme-editor" => open_theme_editor = true,
            "--preview-lang-menu" => preview_lang_menu = true,
            "--preview-settings" => preview_settings = true,
            "--preview-all" => preview_all = true,
            "--scroll-entries" => scroll_entries = true,
            "--show-dialog" => {
                show_dialog = args.get(i + 1).cloned().unwrap_or_default();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    if let Some(d) = demo {
        let dirs = d
            .split(',')
            .enumerate()
            .map(|(n, spec)| {
                let (dir, label) = spec.split_once(':').unwrap_or((spec, if n == 0 { "SATA p1" } else { "NVMe p1" }));
                (format!("demo-{}", n), label.to_string(), PathBuf::from(dir))
            })
            .collect();
        mode = app::Mode::Demo { dirs, overrides };
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1500.0, 920.0]).with_min_inner_size([1100.0, 700.0]).with_title("bootpick settings").with_app_id("bootpick-settings"),
        ..Default::default()
    };
    let opts = app::Opts { mode, screenshot, open_theme_editor, preview_lang_menu, preview_settings, preview_all, scroll_entries, show_dialog };
    eframe::run_native("bootpick settings", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, opts).start(&cc.egui_ctx)))))
}
