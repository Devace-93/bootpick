//! bootpick-config: desktop settings for the bootpick boot menu.
//!
//!   bootpick-config                    the settings window (asks for authorization once)
//!   bootpick-config --helper           privileged helper (started by the app: through pkexec on
//!                                      Linux, elevated through UAC with --pipe NAME on Windows)
//!   bootpick-config --install [--source DIR] [--keep|--reset] [--no-boot-entry] [--no-app] [--uninstall]
//!                                      Windows installer (elevated; run by install.ps1)
//!   bootpick-config --probe            Windows: print what the installer sees, change nothing
//!   bootpick-config --upgrade-menus    rewrite GRUB blocks from an older bootpick (root; install.sh)
//!   bootpick-config --demo DIR[:LABEL][,DIR2[:LABEL]] [--demo-root UUID=DIR]...
//!                                      use plain directories as EFI volumes (no root)
//!   bootpick-config --screenshot OUT.png [--open-theme-editor] [--preview-lang-menu]
//!                   [--preview-settings] [--preview-all] [--scroll-entries] [--show-dialog quit|revert]

// A GUI program on Windows: no console window behind the settings window.
// The command line modes attach to the parent's console (see sys_windows.rs).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod helper;
mod host;
mod i18n_app;
mod imaging;
mod menus;
mod proto;
#[cfg_attr(windows, path = "sys_windows.rs")]
#[cfg_attr(not(windows), path = "sys_linux.rs")]
mod sys;
#[cfg(windows)]
mod install;

use eframe::egui;
use std::path::PathBuf;

/// `DIR:LABEL` -> (DIR, LABEL). The colon of a Windows drive letter
/// (`C:\esp`) is part of the directory.
fn split_label(spec: &str) -> Option<(&str, &str)> {
    let (dir, label) = spec.rsplit_once(':')?;
    (dir.len() > 1 && !label.contains(['\\', '/'])).then_some((dir, label))
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--helper") {
        helper::run(&args);
    }
    #[cfg(windows)]
    if args.iter().any(|a| a == "--install" || a == "--probe") {
        install::run(&args);
    }
    if args.iter().any(|a| a == "--upgrade-menus") {
        helper::upgrade_menus();
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
                let (dir, label) = split_label(spec).unwrap_or((spec, if n == 0 { "SATA p1" } else { "NVMe p1" }));
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
