//! Configuration file (`\EFI\bootpick\bootpick.conf`) and built-in themes.
//!
//! Format: `key = value` lines, with `[entry]` sections for per-entry
//! preferences. Comments start with `#`.

use crate::gfx::Color;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};

#[derive(Clone, Debug, Default)]
pub struct EntryPref {
    pub id: String,
    pub alias: String,
    pub icon: String,
    pub hidden: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub theme: String,
    pub background: String,
    pub timeout: u32,
    pub default_entry: String,
    pub mouse_speed: u32,
    pub entries: Vec<EntryPref>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "midnight".into(),
            background: String::new(),
            timeout: 10,
            default_entry: String::new(),
            mouse_speed: 8,
            entries: vec![],
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Config {
        let mut cfg = Config::default();
        let mut cur: Option<EntryPref> = None;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.eq_ignore_ascii_case("[entry]") {
                if let Some(e) = cur.take() {
                    if !e.id.is_empty() {
                        cfg.entries.push(e);
                    }
                }
                cur = Some(EntryPref::default());
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if let Some(e) = cur.as_mut() {
                match k.as_str() {
                    "id" => e.id = v.into(),
                    "alias" => e.alias = v.into(),
                    "icon" => e.icon = v.into(),
                    "hidden" => e.hidden = Some(parse_bool(v)),
                    _ => {}
                }
            } else {
                match k.as_str() {
                    "theme" => cfg.theme = v.to_ascii_lowercase(),
                    "background" => cfg.background = if v.eq_ignore_ascii_case("none") { String::new() } else { v.into() },
                    "timeout" => cfg.timeout = v.parse().unwrap_or(10),
                    "default" => cfg.default_entry = v.into(),
                    "mouse_speed" => cfg.mouse_speed = v.parse().unwrap_or(8).clamp(1, 64),
                    _ => {}
                }
            }
        }
        if let Some(e) = cur.take() {
            if !e.id.is_empty() {
                cfg.entries.push(e);
            }
        }
        cfg
    }

    pub fn serialize(&self) -> String {
        let mut s = String::new();
        s.push_str("# bootpick configuration - edited from the boot menu (gear icon) or by hand.\n");
        s.push_str(&format!("theme = {}\n", self.theme));
        s.push_str(&format!(
            "background = {}\n",
            if self.background.is_empty() { "none" } else { &self.background }
        ));
        s.push_str(&format!("timeout = {}\n", self.timeout));
        s.push_str(&format!("default = {}\n", self.default_entry));
        s.push_str(&format!("mouse_speed = {}\n", self.mouse_speed));
        s.push_str("\n# Entries, in display order. `hidden = true` keeps them off the main screen\n# (press Esc on the main screen to reveal hidden entries).\n");
        for e in &self.entries {
            s.push_str("\n[entry]\n");
            s.push_str(&format!("id = {}\n", e.id));
            if !e.alias.is_empty() {
                s.push_str(&format!("alias = {}\n", e.alias));
            }
            if !e.icon.is_empty() {
                s.push_str(&format!("icon = {}\n", e.icon));
            }
            if let Some(h) = e.hidden {
                s.push_str(&format!("hidden = {}\n", h));
            }
        }
        s
    }
}

fn parse_bool(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub bg0: Color,
    pub bg1: Color,
    pub accent: Color,
    pub text: Color,
    pub text_dim: Color,
    pub panel: Color,
    pub tile: Color,
    pub tile_hover: Color,
    pub danger: Color,
}

pub const THEMES: &[Theme] = &[
    Theme {
        id: "midnight",
        name: "Midnight",
        bg0: Color::hex(0x0b1220),
        bg1: Color::hex(0x1e293b),
        accent: Color::hex(0x38bdf8),
        text: Color::hex(0xf1f5f9),
        text_dim: Color::hex(0x94a3b8),
        panel: Color::rgba(15, 23, 42, 215),
        tile: Color::rgba(255, 255, 255, 18),
        tile_hover: Color::rgba(255, 255, 255, 40),
        danger: Color::hex(0xf87171),
    },
    Theme {
        id: "pop",
        name: "Pop",
        bg0: Color::hex(0x1b1b1b),
        bg1: Color::hex(0x333333),
        accent: Color::hex(0xffa800),
        text: Color::hex(0xf6f6f6),
        text_dim: Color::hex(0xa8a8a8),
        panel: Color::rgba(27, 27, 27, 220),
        tile: Color::rgba(255, 255, 255, 16),
        tile_hover: Color::rgba(255, 255, 255, 36),
        danger: Color::hex(0xff6b6b),
    },
    Theme {
        id: "nord",
        name: "Nord",
        bg0: Color::hex(0x242933),
        bg1: Color::hex(0x3b4252),
        accent: Color::hex(0x88c0d0),
        text: Color::hex(0xeceff4),
        text_dim: Color::hex(0xa3abb9),
        panel: Color::rgba(46, 52, 64, 220),
        tile: Color::rgba(236, 239, 244, 18),
        tile_hover: Color::rgba(236, 239, 244, 40),
        danger: Color::hex(0xbf616a),
    },
    Theme {
        id: "ember",
        name: "Ember",
        bg0: Color::hex(0x160a0a),
        bg1: Color::hex(0x3b1515),
        accent: Color::hex(0xfb923c),
        text: Color::hex(0xfff7ed),
        text_dim: Color::hex(0xc7a79b),
        panel: Color::rgba(30, 10, 10, 220),
        tile: Color::rgba(255, 237, 213, 18),
        tile_hover: Color::rgba(255, 237, 213, 40),
        danger: Color::hex(0xff5f5f),
    },
    Theme {
        id: "light",
        name: "Light",
        bg0: Color::hex(0xf8fafc),
        bg1: Color::hex(0xcbd5e1),
        accent: Color::hex(0x2563eb),
        text: Color::hex(0x0f172a),
        text_dim: Color::hex(0x475569),
        panel: Color::rgba(255, 255, 255, 225),
        tile: Color::rgba(15, 23, 42, 14),
        tile_hover: Color::rgba(15, 23, 42, 30),
        danger: Color::hex(0xdc2626),
    },
];

pub fn theme_by_id(id: &str) -> usize {
    THEMES.iter().position(|t| t.id == id).unwrap_or(0)
}

pub fn theme_name(idx: usize) -> String {
    THEMES[idx % THEMES.len()].name.to_string()
}
