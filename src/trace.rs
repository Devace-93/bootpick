//! Persistent breadcrumb log at \EFI\bootpick\bootpick.log on the volume
//! bootpick was started from. Each run appends a session; the file keeps the
//! last few hundred lines. Flushed explicitly before anything that may not
//! return (chainloading, resets), so a hang can be diagnosed afterwards.

use crate::esp;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use uefi::boot;
use uefi::fs::FileSystem;

pub const LOG_PATH: &str = "\\EFI\\bootpick\\bootpick.log";
const KEEP_LINES: usize = 400;

struct State {
    lines: Vec<String>,
    loaded: bool,
}

struct Global(RefCell<State>);
// SAFETY: UEFI boot services run single-threaded; there is no concurrency.
unsafe impl Sync for Global {}

static STATE: Global = Global(RefCell::new(State { lines: Vec::new(), loaded: false }));

fn ticks() -> String {
    match uefi::runtime::get_time() {
        Ok(t) => alloc::format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.year(), t.month(), t.day(), t.hour(), t.minute(), t.second()),
        Err(_) => String::from("----"),
    }
}

fn open() -> Option<FileSystem> {
    boot::get_image_file_system(boot::image_handle()).ok().map(FileSystem::new)
}

/// Records one line (also echoed to the debug log/serial).
pub fn line(msg: &str) {
    log::info!("{}", msg);
    let mut st = STATE.0.borrow_mut();
    if !st.loaded {
        st.loaded = true;
        if let Some(mut fs) = open() {
            if let Some(old) = esp::read_string(&mut fs, LOG_PATH) {
                st.lines = old.lines().map(String::from).collect();
            }
        }
    }
    st.lines.push(alloc::format!("{}  {}", ticks(), msg));
    let n = st.lines.len();
    if n > KEEP_LINES {
        st.lines.drain(0..n - KEEP_LINES);
    }
}

/// Writes the log to disk. Call before anything that might not return.
pub fn flush() {
    let st = STATE.0.borrow();
    if st.lines.is_empty() {
        return;
    }
    let mut text = st.lines.join("\n");
    text.push('\n');
    drop(st);
    if let Some(mut fs) = open() {
        if let Err(e) = esp::write(&mut fs, LOG_PATH, text.as_bytes()) {
            log::warn!("bootpick: could not write log: {}", e);
        }
    }
}
