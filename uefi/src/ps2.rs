//! A PS/2 mouse or touchpad read straight from the keyboard controller
//! (ports 0x60 / 0x64). Many laptops have a PS/2 touchpad but firmware
//! without a driver for it, so no pointer protocol ever appears; this is the
//! fallback for that case and is only used when the firmware offers no
//! pointer device of its own.
//!
//! The controller is shared with the firmware's keyboard driver. Only bytes
//! the controller marks as coming from the mouse port are taken; keys stay
//! where they are. The controller's configuration byte is never rewritten.

use core::arch::asm;
use core::time::Duration;
use uefi::boot::{self, Tpl};

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
/// Status: a byte is waiting to be read.
const OUT_FULL: u8 = 0x01;
/// Status: the controller has not taken the last byte written yet.
const IN_FULL: u8 = 0x02;
/// Status: the waiting byte comes from the mouse port.
const FROM_MOUSE: u8 = 0x20;

const ACK: u8 = 0xFA;

fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: reading the keyboard controller's ports has no memory effects.
    unsafe { asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
    v
}

fn outb(port: u16, v: u8) {
    // SAFETY: writing the keyboard controller's ports has no memory effects.
    unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags)) };
}

fn pause() {
    boot::stall(Duration::from_micros(100));
}

/// Waits until the controller accepts another byte.
fn ready_to_write() -> Result<(), &'static str> {
    for _ in 0..500 {
        if inb(STATUS) & IN_FULL == 0 {
            return Ok(());
        }
        pause();
    }
    Err("the keyboard controller is busy")
}

fn command(c: u8) -> Result<(), &'static str> {
    ready_to_write()?;
    outb(STATUS, c);
    Ok(())
}

/// Sends one byte to the mouse and waits for its acknowledgement.
fn to_mouse(b: u8) -> Result<(), &'static str> {
    command(0xD4)?; // next data byte goes to the mouse port
    ready_to_write()?;
    outb(DATA, b);
    for _ in 0..2000 {
        if inb(STATUS) & OUT_FULL != 0 {
            match inb(DATA) {
                ACK => return Ok(()),
                0xFE => return Err("the PS/2 mouse rejected a command"),
                _ => {} // an old movement byte, or a key pressed right now
            }
        }
        pause();
    }
    Err("no PS/2 mouse answers")
}

pub struct Motion {
    pub dx: i32,
    pub dy: i32,
    pub left: bool,
}

pub struct Ps2Mouse {
    packet: [u8; 3],
    have: usize,
    left: bool,
}

impl Ps2Mouse {
    /// Turns on the mouse port and movement reports.
    pub fn init() -> Result<Ps2Mouse, &'static str> {
        // SAFETY: restored when the guard drops; keeps the firmware's own
        // keyboard polling from reading the answers meant for us.
        let _tpl = unsafe { boot::raise_tpl(Tpl::HIGH_LEVEL) };
        if inb(STATUS) == 0xFF {
            return Err("no keyboard controller");
        }
        command(0xA8)?; // enable the mouse port
        to_mouse(0xF6)?; // defaults: relative packets, 100 samples/s
        to_mouse(0xF4)?; // start reporting
        Ok(Ps2Mouse { packet: [0; 3], have: 0, left: false })
    }

    /// Reads the packets that are waiting; None when nothing changed.
    pub fn poll(&mut self) -> Option<Motion> {
        // SAFETY: as in `init`; held for a few milliseconds at most.
        let _tpl = unsafe { boot::raise_tpl(Tpl::HIGH_LEVEL) };
        let (mut dx, mut dy, mut changed) = (0i32, 0i32, false);
        let mut waited = 0;
        for _ in 0..400 {
            let st = inb(STATUS);
            if st & OUT_FULL == 0 {
                // Between the bytes of one packet: give the next one 2 ms.
                if self.have == 0 {
                    break;
                }
                if waited >= 20 {
                    self.have = 0; // the rest never came: drop the fragment
                    break;
                }
                waited += 1;
                pause();
                continue;
            }
            if st & FROM_MOUSE == 0 {
                break; // a key: left for the firmware's keyboard driver
            }
            let b = inb(DATA);
            waited = 0;
            // The first byte of a packet always has bit 3 set: resynchronise on it.
            if self.have == 0 && b & 0x08 == 0 {
                continue;
            }
            self.packet[self.have] = b;
            self.have += 1;
            if self.have < 3 {
                continue;
            }
            self.have = 0;
            let [flags, x, y] = self.packet;
            if flags & 0xC0 != 0 {
                continue; // overflow: the distances are meaningless
            }
            dx += x as i32 - if flags & 0x10 != 0 { 256 } else { 0 };
            // PS/2 counts upwards as positive; the screen grows downwards.
            dy -= y as i32 - if flags & 0x20 != 0 { 256 } else { 0 };
            let left = flags & 0x01 != 0;
            changed |= left != self.left || x != 0 || y != 0;
            self.left = left;
        }
        changed.then_some(Motion { dx, dy, left: self.left })
    }
}

impl Drop for Ps2Mouse {
    /// Stops the reports, so a boot loader started next (GRUB reads the same
    /// controller for its keyboard) does not find movement bytes waiting.
    /// Operating systems reset the device themselves.
    fn drop(&mut self) {
        // SAFETY: as in `init`.
        let _tpl = unsafe { boot::raise_tpl(Tpl::HIGH_LEVEL) };
        let _ = to_mouse(0xF5);
    }
}
