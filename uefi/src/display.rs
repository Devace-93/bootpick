//! Read-only probe of the firmware's video outputs, written to the trace
//! log: how many Graphics Output instances exist, their resolutions, device
//! paths and attached monitors (EDID). Groundwork for choosing a monitor.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, SearchType};
use uefi::proto::console::gop::{EdidDiscovered, GraphicsOutput};
use uefi::proto::console::text::Output;
use uefi::proto::unsafe_protocol;
use uefi::{Handle, Identify};

/// EFI_EDID_ACTIVE_PROTOCOL (same layout as EDID Discovered).
#[repr(C)]
#[unsafe_protocol("bd8c1056-9f36-44ec-92a8-a6337f817986")]
struct EdidActive {
    size: u32,
    edid: *const u8,
}

fn open_shared<P: uefi::proto::ProtocolPointer + ?Sized>(handle: Handle) -> Option<ScopedProtocol<P>> {
    let params = OpenProtocolParams { handle, agent: boot::image_handle(), controller: None };
    // SAFETY: GetProtocol, read-only use.
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }.ok()
}

/// "MFG name" from an EDID block: manufacturer id and the monitor name
/// descriptor when present.
fn edid_summary(e: &[u8]) -> String {
    if e.len() < 128 {
        return format!("{} bytes", e.len());
    }
    let id = u16::from_be_bytes([e[8], e[9]]);
    let ch = |v: u16| (b'A' + ((v & 31) as u8).saturating_sub(1)) as char;
    let mfg: String = [ch(id >> 10), ch(id >> 5), ch(id)].iter().collect();
    let mut name = String::new();
    for d in 0..4 {
        let o = 54 + d * 18;
        if e[o] == 0 && e[o + 1] == 0 && e[o + 3] == 0xFC {
            name = e[o + 5..o + 18].iter().take_while(|b| **b != 0x0A).map(|b| *b as char).collect();
        }
    }
    let (w, h) = (e[54 + 2] as u32 | ((e[54 + 4] as u32 & 0xF0) << 4), e[54 + 5] as u32 | ((e[54 + 7] as u32 & 0xF0) << 4));
    format!("{} '{}' native {}x{}", mfg, name.trim(), w, h)
}

fn path_text(handle: Handle) -> String {
    match crate::esp::device_path_of(handle) {
        Some(dp) => dp.node_iter().map(|n| format!("{}", n)).collect::<Vec<_>>().join("/"),
        None => String::from("(no device path: virtual / console splitter)"),
    }
}

pub fn probe() {
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&GraphicsOutput::GUID)) else {
        crate::trace::line("display: no Graphics Output");
        return;
    };
    let first = boot::get_handle_for_protocol::<GraphicsOutput>().ok();
    crate::trace::line(&format!("display: {} Graphics Output instance(s)", handles.len()));
    for (i, &h) in handles.iter().enumerate() {
        let mut line = format!("display[{}]{}:", i, if Some(h) == first { " (used)" } else { "" });
        if let Some(gop) = open_shared::<GraphicsOutput>(h) {
            let (w, ht) = gop.current_mode_info().resolution();
            let modes: Vec<String> = gop.modes().map(|m| {
                let (a, b) = m.info().resolution();
                format!("{}x{}", a, b)
            }).collect();
            line.push_str(&format!(" current {}x{}, {} modes [{}]", w, ht, modes.len(), modes.join(" ")));
        }
        line.push_str(&format!(", console {}", if open_shared::<Output>(h).is_some() { "yes" } else { "no" }));
        if let Some(e) = open_shared::<EdidActive>(h) {
            if !e.edid.is_null() {
                // SAFETY: the firmware guarantees `size` readable bytes.
                let b = unsafe { core::slice::from_raw_parts(e.edid, e.size as usize) };
                line.push_str(&format!(", EDID active {}", edid_summary(b)));
            }
        } else if let Some(e) = open_shared::<EdidDiscovered>(h) {
            if let Some(b) = e.edid() {
                line.push_str(&format!(", EDID discovered {}", edid_summary(b)));
            }
        }
        line.push_str(&format!(", path {}", path_text(h)));
        crate::trace::line(&line);
    }
}
