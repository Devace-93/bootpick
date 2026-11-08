//! UEFI shell around the core menu: graphics output, mouse cursor, input
//! translation, frame timing, and acting on the menu's outcome.

use crate::boot;
use crate::esp;
use crate::input::{Input, InputEvent};
use crate::scan::Vol;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use bootpick_core::gfx::{Canvas, Image, Rect};
use bootpick_core::menu::{make_cursor, Key as MKey, Menu, MenuOptions, Outcome};
use bootpick_core::scan::Scan;
use bootpick_core::storage::{NoStorage, Storage};
use uefi::boot::{self as uboot, ScopedProtocol};
use uefi::fs::FileSystem;
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};
use uefi::proto::console::text::{Key, ScanCode};

fn tsc() -> u64 {
    // SAFETY: rdtsc has no side effects; used for the debug overlay only.
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn as_blt(c: &Canvas) -> &[BltPixel] {
    let px = c.pixels();
    // SAFETY: BltPixel is #[repr(C)] { blue, green, red, reserved }, 4 bytes,
    // exactly the little-endian layout of a 0x00RRGGBB u32.
    unsafe { core::slice::from_raw_parts(px.as_ptr().cast::<BltPixel>(), px.len()) }
}

fn map_key(k: Key) -> Option<MKey> {
    Some(match k {
        Key::Special(ScanCode::UP) => MKey::Up,
        Key::Special(ScanCode::DOWN) => MKey::Down,
        Key::Special(ScanCode::LEFT) => MKey::Left,
        Key::Special(ScanCode::RIGHT) => MKey::Right,
        Key::Special(ScanCode::ESCAPE) => MKey::Esc,
        Key::Special(ScanCode::PAGE_UP) => MKey::PageUp,
        Key::Special(ScanCode::PAGE_DOWN) => MKey::PageDown,
        Key::Special(ScanCode::FUNCTION_2) => MKey::F2,
        Key::Special(ScanCode::FUNCTION_10) => MKey::F10,
        Key::Special(ScanCode::FUNCTION_12) => MKey::F12,
        Key::Printable(c) => match char::from(c) {
            '\r' | '\n' => MKey::Enter,
            '\u{8}' => MKey::Backspace,
            '\t' => MKey::Tab,
            ch => MKey::Char(ch),
        },
        _ => return None,
    })
}

struct Screen {
    gop: ScopedProtocol<GraphicsOutput>,
    w: usize,
    h: usize,
    frame: Canvas,
    cursor: Image,
    cursor_rect: Rect,
}

impl Screen {
    fn open() -> Result<Screen, String> {
        let handle = uboot::get_handle_for_protocol::<GraphicsOutput>().map_err(|e| format!("no GOP: {:?}", e.status()))?;
        let mut gop = uboot::open_protocol_exclusive::<GraphicsOutput>(handle).map_err(|e| format!("GOP busy: {:?}", e.status()))?;
        pick_mode(&mut gop);
        let (w, h) = gop.current_mode_info().resolution();
        let s = h as f32 / 1080.0;
        let cursor = make_cursor(if s >= 1.4 { 3 } else if s >= 0.8 { 2 } else { 1 });
        Ok(Screen { gop, w, h, frame: Canvas::new(w, h), cursor, cursor_rect: Rect::new(0, 0, 0, 0) })
    }

    fn draw_cursor(&mut self, input: &Input) {
        if !input.has_pointer() {
            return;
        }
        self.frame.blit(&self.cursor, input.x, input.y);
        self.cursor_rect = self.frame.clip(Rect::new(input.x, input.y, self.cursor.w as i32, self.cursor.h as i32));
    }

    fn present_full(&mut self, scene: &Canvas, input: &Input) {
        self.frame.copy_from(scene);
        self.cursor_rect = Rect::new(0, 0, 0, 0);
        self.draw_cursor(input);
        let (w, h) = (self.w, self.h);
        let _ = self.gop.blt(BltOp::BufferToVideo { buffer: as_blt(&self.frame), src: BltRegion::Full, dest: (0, 0), dims: (w, h) });
    }

    fn present_plain(&mut self, scene: &Canvas) {
        self.frame.copy_from(scene);
        let (w, h) = (self.w, self.h);
        let _ = self.gop.blt(BltOp::BufferToVideo { buffer: as_blt(&self.frame), src: BltRegion::Full, dest: (0, 0), dims: (w, h) });
    }

    fn present_cursor(&mut self, scene: &Canvas, input: &Input) {
        let old = self.cursor_rect;
        self.frame.copy_rect_from(scene, old);
        self.draw_cursor(input);
        let r = self.frame.clip(old.union(&self.cursor_rect));
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        let stride = self.w;
        let _ = self.gop.blt(BltOp::BufferToVideo {
            buffer: as_blt(&self.frame),
            src: BltRegion::SubRectangle { coords: (r.x as usize, r.y as usize), px_stride: stride },
            dest: (r.x as usize, r.y as usize),
            dims: (r.w as usize, r.h as usize),
        });
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

fn image_storage() -> Box<dyn Storage> {
    match uboot::get_image_file_system(uboot::image_handle()).ok().map(FileSystem::new) {
        Some(fs) => Box::new(esp::FsStorage(fs)),
        None => Box::new(NoStorage),
    }
}

/// Steps of the start-up loading screen (see `splash`).
pub const SPLASH_STEPS: usize = 4;

/// Draws the loading screen (theme gradient + progress) so no firmware or
/// console text shows while bootpick starts.
pub fn splash(step: usize) {
    if let Ok(mut screen) = Screen::open() {
        let theme = bootpick_core::menu::saved_theme(&mut *image_storage());
        let c = bootpick_core::menu::splash(screen.w, screen.h, &theme, step, SPLASH_STEPS);
        screen.present_plain(&c);
    }
}

/// TSC ticks per millisecond (for double clicks); measured once.
fn tsc_per_ms() -> u64 {
    let t0 = tsc();
    uboot::stall(core::time::Duration::from_millis(20));
    ((tsc() - t0) / 20).max(1)
}

/// Runs the menu until an outcome; redraws with frame timing.
fn run_menu(menu: &mut Menu, screen: &mut Screen, input: &mut Input, per_ms: u64) -> Outcome {
    let mut frame_cycles = 0u64;
    let mut draw_cycles = 0u64;
    let redraw = |menu: &mut Menu, screen: &mut Screen, input: &Input, fc: &mut u64, dc: &mut u64| {
        if menu.debug {
            menu.set_debug_line(format!(
                "frame {:.1} Mc (draw {:.1}, present {:.1})  {}",
                *fc as f32 / 1.0e6,
                *dc as f32 / 1.0e6,
                fc.saturating_sub(*dc) as f32 / 1.0e6,
                input.stats
            ));
        }
        let t0 = tsc();
        menu.draw();
        let t1 = tsc();
        screen.present_full(menu.scene(), input);
        *fc = tsc() - t0;
        *dc = t1 - t0;
    };
    redraw(menu, screen, input, &mut frame_cycles, &mut draw_cycles);
    loop {
        match input.next() {
            InputEvent::Tick => {
                let (outcome, dirty) = menu.tick();
                if let Some(o) = outcome {
                    return o;
                }
                if dirty {
                    redraw(menu, screen, input, &mut frame_cycles, &mut draw_cycles);
                }
            }
            InputEvent::Move { x, y } => {
                if menu.pointer_move(x, y) {
                    redraw(menu, screen, input, &mut frame_cycles, &mut draw_cycles);
                } else {
                    screen.present_cursor(menu.scene(), input);
                }
            }
            InputEvent::Down { x, y } => {
                menu.set_now_ms(tsc() / per_ms);
                if let Some(o) = menu.pointer_down(x, y) {
                    return o;
                }
                redraw(menu, screen, input, &mut frame_cycles, &mut draw_cycles);
            }
            InputEvent::Up { .. } => {}
            InputEvent::Key(k) => {
                if let Some(k) = map_key(k) {
                    if let Some(o) = menu.key(k) {
                        return o;
                    }
                    redraw(menu, screen, input, &mut frame_cycles, &mut draw_cycles);
                }
            }
        }
    }
}

/// Runs the picker until something is launched.
pub fn run(scan: Scan, vols: alloc::vec::Vec<Vol>) -> ! {
    let mut status: Option<String> = None;
    let platform_lang = crate::scan::platform_lang();
    let per_ms = tsc_per_ms();
    loop {
        splash(3);
        let mut screen = match Screen::open() {
            Ok(s) => s,
            Err(e) => {
                log::error!("bootpick: {}", e);
                uboot::stall(core::time::Duration::from_secs(5));
                boot::reboot();
            }
        };
        let opts = MenuOptions {
            width: screen.w,
            height: screen.h,
            status: status.take(),
            has_pointer: false,
            platform_lang: platform_lang.clone(),
            preview: false,
        };
        let mut menu = Menu::new(scan.clone(), image_storage(), opts);
        let mut input = Input::new(screen.w, screen.h, menu.mouse_speed());
        menu.set_has_pointer(input.has_pointer());
        crate::trace::line(&format!("display {}x{}, language {}, input: {}", screen.w, screen.h, menu.language(), input.stats));
        crate::trace::flush();
        let outcome = run_menu(&mut menu, &mut screen, &mut input, per_ms);
        match outcome {
            Outcome::Launch { entry, remember } => {
                let e = menu.entry(entry).clone();
                if remember {
                    match menu.remember_last(&e.id) {
                        Ok(true) => crate::trace::line(&format!("remembered last entry: {}", e.id)),
                        Ok(false) => {}
                        Err(err) => crate::trace::line(&format!("could not remember last entry: {}", err)),
                    }
                }
                // Only the background and a loading sign until the loader
                // draws (no console text in between).
                // The dots fill one by one, then the system takes over.
                for filled in 1..=3 {
                    screen.present_plain(menu.launching_scene(filled));
                    uboot::stall(core::time::Duration::from_millis(180));
                }
                drop(menu);
                drop(screen);
                if let Err(err) = boot::launch(&e.kind, &vols) {
                    crate::trace::line(&format!("launch failed: {}", err));
                    crate::trace::flush();
                    status = Some(format!("{}", err));
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
