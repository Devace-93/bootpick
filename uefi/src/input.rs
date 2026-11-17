//! Unified input: keyboard (Simple Text Input), mice (Simple Pointer),
//! touch/tablets (Absolute Pointer), a PS/2 touchpad read directly when the
//! firmware has no pointer device (see ps2.rs), and a periodic tick timer.

use alloc::vec::Vec;
use core::time::Duration;
use alloc::format;
use alloc::string::String;
use uefi::boot::{self, EventType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, SearchType, TimerTrigger, Tpl};
use uefi::proto::console::text::Input as TextInput;
use uefi::proto::ProtocolPointer;
use uefi::proto::console::pointer::{AbsolutePointer, Pointer};
use uefi::proto::console::text::Key;
use crate::ps2::Ps2Mouse;
use uefi::{system, Event};

#[derive(Clone, Copy, Debug)]
pub enum InputEvent {
    Key(Key),
    Move { x: i32, y: i32 },
    Down { x: i32, y: i32 },
    #[allow(dead_code)]
    Up { x: i32, y: i32 },
    Tick,
}

pub struct Input {
    simple: Vec<ScopedProtocol<Pointer>>,
    abs: Vec<ScopedProtocol<AbsolutePointer>>,
    /// Polled on its own timer, the last entry of `events`.
    ps2: Option<Ps2Mouse>,
    events: Vec<Event>,
    pub x: i32,
    pub y: i32,
    w: i32,
    h: i32,
    pub speed: i32,
    down: bool,
    pub stats: String,
    moves: u32,
    /// A tick is owed: a pointer event was returned in its place.
    pending_tick: bool,
}

/// The console splitter publishes an aggregate pointer on the ConIn handle
/// (the one that also carries Simple Text Input). Prefer real device handles
/// so movements are not counted twice; fall back to the aggregate otherwise.
/// The second value tells whether real devices were found. A real device
/// has a device path; the firmware's virtual pointers (console splitter and
/// the like) have none, whether or not they sit on the ConIn handle.
fn pick_device_handles(what: &str, handles: Vec<uefi::Handle>) -> (Vec<uefi::Handle>, bool) {
    let is_console = |h: &uefi::Handle| {
        boot::test_protocol::<TextInput>(OpenProtocolParams { handle: *h, agent: boot::image_handle(), controller: None })
            .unwrap_or(false)
    };
    for h in &handles {
        crate::trace::line(&format!("input: {} on {}{}", what, crate::display::path_text(*h), if is_console(h) { " (console)" } else { "" }));
    }
    let devices: Vec<uefi::Handle> = handles.iter().copied().filter(|h| !is_console(h) && crate::esp::device_path_of(*h).is_some()).collect();
    if devices.is_empty() { (handles, false) } else { (devices, true) }
}

/// Opens a protocol without taking it away from the firmware's own drivers
/// (the console splitter keeps working, like rEFInd does with HandleProtocol).
fn open_shared<P: ProtocolPointer + ?Sized>(handle: uefi::Handle) -> Option<ScopedProtocol<P>> {
    let params = OpenProtocolParams { handle, agent: boot::image_handle(), controller: None };
    // SAFETY: GetProtocol does not disconnect existing drivers; we only poll state.
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }.ok()
}

pub const TICK_MS: u64 = 50;
/// How often a PS/2 touchpad is read.
const PS2_POLL_MS: u64 = 10;
/// Counts per millimetre a PS/2 mouse reports by default.
const PS2_RESOLUTION: i32 = 4;

impl Input {
    pub fn new(w: usize, h: usize, speed: u32) -> Input {
        let mut events = Vec::new();
        // 0: timer
        // SAFETY: plain timer event without notify function.
        let timer = unsafe { boot::create_event(EventType::TIMER, Tpl::APPLICATION, None, None) }
            .expect("timer event");
        boot::set_timer(&timer, TimerTrigger::Periodic(Duration::from_millis(TICK_MS))).expect("set_timer");
        events.push(timer);
        // 1: keyboard
        let key_evt = system::with_stdin(|i| i.wait_for_key_event()).expect("key event");
        events.push(key_evt);
        let _ = system::with_stdin(|i| i.reset(false));

        // Firmware often leaves USB HID devices unconnected until an OS asks;
        // connect everything so mice and touch devices show up (rEFInd does this too).
        if let Ok(all) = boot::locate_handle_buffer(SearchType::AllHandles) {
            for h in all.iter() {
                let _ = boot::connect_controller(*h, &[], None, true);
            }
        }
        let mut simple = Vec::new();
        let mut stats = String::new();
        let (pointer_handles, real_mice) = pick_device_handles("pointer", boot::find_handles::<Pointer>().unwrap_or_default());
        for h in pointer_handles {
            if let Some(mut p) = open_shared::<Pointer>(h) {
                let _ = p.reset(false);
                if let Ok(e) = p.wait_for_input_event() {
                    let m = p.mode();
                    stats.push_str(&format!("ptr res={}x{} ", m.resolution_x, m.resolution_y));
                    events.push(e);
                    simple.push(p);
                }
            }
        }
        let mut abs = Vec::new();
        let (abs_handles, real_abs) = pick_device_handles("absolute pointer", boot::find_handles::<AbsolutePointer>().unwrap_or_default());
        for h in abs_handles {
            if let Some(mut p) = open_shared::<AbsolutePointer>(h) {
                let _ = p.reset(false);
                if let Ok(e) = p.wait_for_input_event() {
                    let m = p.mode();
                    stats.push_str(&format!("abs {}..{} ", m.absolute_min_x, m.absolute_max_x));
                    events.push(e);
                    abs.push(p);
                }
            }
        }
        crate::trace::line(&alloc::format!("input: {} mice, {} absolute pointers ({}): {}", simple.len(), abs.len(), if real_mice || real_abs { "devices" } else { "console aggregate only" }, stats));
        // No pointer device from the firmware: a PS/2 touchpad may still be
        // there without a firmware driver.
        let mut ps2 = None;
        if !real_mice && !real_abs {
            match Ps2Mouse::init() {
                Ok(m) => {
                    // SAFETY: plain timer event without notify function.
                    let timer = unsafe { boot::create_event(EventType::TIMER, Tpl::APPLICATION, None, None) };
                    match timer {
                        Ok(t) if boot::set_timer(&t, TimerTrigger::Periodic(Duration::from_millis(PS2_POLL_MS))).is_ok() => {
                            events.push(t);
                            ps2 = Some(m);
                            stats.push_str("ps2 ");
                            crate::trace::line("input: PS/2 mouse or touchpad found, read directly");
                        }
                        _ => crate::trace::line("input: PS/2 mouse found but no timer to read it"),
                    }
                }
                Err(e) => crate::trace::line(&alloc::format!("input: no PS/2 mouse ({})", e)),
            }
        }
        Input {
            simple,
            abs,
            ps2,
            events,
            x: w as i32 / 2,
            y: h as i32 / 2,
            w: w as i32,
            h: h as i32,
            speed: speed as i32,
            down: false,
            stats,
            moves: 0,
            pending_tick: false,
        }
    }

    pub fn has_pointer(&self) -> bool {
        !self.simple.is_empty() || !self.abs.is_empty() || self.ps2.is_some()
    }

    /// Blocks until something happens.
    pub fn next(&mut self) -> InputEvent {
        if self.pending_tick {
            self.pending_tick = false;
            return InputEvent::Tick;
        }
        loop {
            let idx = match boot::wait_for_event(&self.events) {
                Ok(i) => i,
                Err(_) => {
                    boot::stall(Duration::from_millis(TICK_MS));
                    return InputEvent::Tick;
                }
            };
            match idx {
                0 => {
                    // Some firmware never signals a pointer's event although
                    // it has movement to report: ask on every tick as well.
                    for pi in 0..self.simple.len() + self.abs.len() {
                        if let Some(e) = self.read_device(pi) {
                            self.pending_tick = true;
                            return e;
                        }
                    }
                    return InputEvent::Tick;
                }
                1 => {
                    if let Ok(Some(k)) = system::with_stdin(|i| i.read_key()) {
                        return InputEvent::Key(k);
                    }
                }
                i if i - 2 < self.simple.len() + self.abs.len() => {
                    if let Some(e) = self.read_device(i - 2) {
                        return e;
                    }
                }
                _ => {
                    // The PS/2 timer.
                    if let Some(m) = self.ps2.as_mut().and_then(|p| p.poll()) {
                        self.moves += 1;
                        self.stats = format!("ps2 raw=({},{}) btn={} n={}", m.dx, m.dy, m.left as u8, self.moves);
                        let dx = m.dx * self.speed / PS2_RESOLUTION;
                        let dy = m.dy * self.speed / PS2_RESOLUTION;
                        let moved = dx != 0 || dy != 0;
                        self.x = (self.x + dx).clamp(0, self.w - 1);
                        self.y = (self.y + dy).clamp(0, self.h - 1);
                        if let Some(e) = self.button(m.left) {
                            return e;
                        }
                        if moved {
                            return InputEvent::Move { x: self.x, y: self.y };
                        }
                    }
                }
            }
        }
    }

    /// Reads one firmware pointer (mice first, then absolute pointers).
    fn read_device(&mut self, pi: usize) -> Option<InputEvent> {
        if pi < self.simple.len() {
            let st = self.simple[pi].read_state().ok()??;
            let mode = self.simple[pi].mode();
            let rx = (mode.resolution_x as i32).max(1);
            let ry = (mode.resolution_y as i32).max(1);
            let dx = st.relative_movement_x * self.speed / rx;
            let dy = st.relative_movement_y * self.speed / ry;
            let left: bool = st.left_button.into();
            self.moves += 1;
            self.stats = format!("mouse#{} raw=({},{}) btn={} res={} n={}", pi, st.relative_movement_x, st.relative_movement_y, left as u8, rx, self.moves);
            if dx != 0 || dy != 0 {
                self.x = (self.x + dx).clamp(0, self.w - 1);
                self.y = (self.y + dy).clamp(0, self.h - 1);
                if let Some(e) = self.button(left) {
                    return Some(e);
                }
                return Some(InputEvent::Move { x: self.x, y: self.y });
            }
            return self.button(left);
        }
        let ai = pi - self.simple.len();
        let st = self.abs.get_mut(ai)?.read_state().ok()??;
        let mode = self.abs[ai].mode();
        let span_x = (mode.absolute_max_x.saturating_sub(mode.absolute_min_x)).max(1) as i64;
        let span_y = (mode.absolute_max_y.saturating_sub(mode.absolute_min_y)).max(1) as i64;
        let nx = ((st.current_x.saturating_sub(mode.absolute_min_x)) as i64 * (self.w as i64 - 1) / span_x) as i32;
        let ny = ((st.current_y.saturating_sub(mode.absolute_min_y)) as i64 * (self.h as i64 - 1) / span_y) as i32;
        let moved = nx != self.x || ny != self.y;
        self.moves += 1;
        self.stats = format!("abs#{} raw=({},{}) span={} n={}", ai, st.current_x, st.current_y, span_x, self.moves);
        self.x = nx.clamp(0, self.w - 1);
        self.y = ny.clamp(0, self.h - 1);
        let left = st.active_buttons & 1 != 0;
        if let Some(e) = self.button(left) {
            return Some(e);
        }
        moved.then_some(InputEvent::Move { x: self.x, y: self.y })
    }

    fn button(&mut self, left: bool) -> Option<InputEvent> {
        if left && !self.down {
            self.down = true;
            Some(InputEvent::Down { x: self.x, y: self.y })
        } else if !left && self.down {
            self.down = false;
            Some(InputEvent::Up { x: self.x, y: self.y })
        } else {
            None
        }
    }
}
