//! Unified input: keyboard (Simple Text Input), mice (Simple Pointer),
//! touch/tablets (Absolute Pointer) and a periodic tick timer.

use alloc::vec::Vec;
use core::time::Duration;
use alloc::format;
use alloc::string::String;
use uefi::boot::{self, EventType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, SearchType, TimerTrigger, Tpl};
use uefi::proto::console::text::Input as TextInput;
use uefi::proto::ProtocolPointer;
use uefi::proto::console::pointer::{AbsolutePointer, Pointer};
use uefi::proto::console::text::Key;
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
    events: Vec<Event>,
    pub x: i32,
    pub y: i32,
    w: i32,
    h: i32,
    pub speed: i32,
    down: bool,
    pub stats: String,
    moves: u32,
}

/// The console splitter publishes an aggregate pointer on the ConIn handle
/// (the one that also carries Simple Text Input). Prefer real device handles
/// so movements are not counted twice; fall back to the aggregate otherwise.
fn pick_device_handles(handles: Vec<uefi::Handle>) -> Vec<uefi::Handle> {
    let is_console = |h: &uefi::Handle| {
        boot::test_protocol::<TextInput>(OpenProtocolParams { handle: *h, agent: boot::image_handle(), controller: None })
            .unwrap_or(false)
    };
    let devices: Vec<uefi::Handle> = handles.iter().copied().filter(|h| !is_console(h)).collect();
    if devices.is_empty() { handles } else { devices }
}

/// Opens a protocol without taking it away from the firmware's own drivers
/// (the console splitter keeps working, like rEFInd does with HandleProtocol).
fn open_shared<P: ProtocolPointer + ?Sized>(handle: uefi::Handle) -> Option<ScopedProtocol<P>> {
    let params = OpenProtocolParams { handle, agent: boot::image_handle(), controller: None };
    // SAFETY: GetProtocol does not disconnect existing drivers; we only poll state.
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }.ok()
}

pub const TICK_MS: u64 = 50;

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
        let pointer_handles = pick_device_handles(boot::find_handles::<Pointer>().unwrap_or_default());
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
        for h in pick_device_handles(boot::find_handles::<AbsolutePointer>().unwrap_or_default()) {
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
        log::info!("bootpick input: {} mice, {} absolute pointers: {}", simple.len(), abs.len(), stats);
        Input {
            simple,
            abs,
            events,
            x: w as i32 / 2,
            y: h as i32 / 2,
            w: w as i32,
            h: h as i32,
            speed: speed as i32,
            down: false,
            stats,
            moves: 0,
        }
    }

    pub fn has_pointer(&self) -> bool {
        !self.simple.is_empty() || !self.abs.is_empty()
    }

    /// Blocks until something happens.
    pub fn next(&mut self) -> InputEvent {
        loop {
            let idx = match boot::wait_for_event(&self.events) {
                Ok(i) => i,
                Err(_) => {
                    boot::stall(Duration::from_millis(TICK_MS));
                    return InputEvent::Tick;
                }
            };
            match idx {
                0 => return InputEvent::Tick,
                1 => {
                    if let Ok(Some(k)) = system::with_stdin(|i| i.read_key()) {
                        return InputEvent::Key(k);
                    }
                }
                i => {
                    let pi = i - 2;
                    if pi < self.simple.len() {
                        if let Ok(Some(st)) = self.simple[pi].read_state() {
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
                                    return e;
                                }
                                return InputEvent::Move { x: self.x, y: self.y };
                            }
                            if let Some(e) = self.button(left) {
                                return e;
                            }
                        }
                    } else {
                        let ai = pi - self.simple.len();
                        if ai < self.abs.len() {
                            if let Ok(Some(st)) = self.abs[ai].read_state() {
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
        }
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
