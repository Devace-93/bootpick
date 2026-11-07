//! Configuration file (`\EFI\bootpick\bootpick.conf`) and themes.
//!
//! Format: `key = value` lines, with `[entry]` sections for per-entry
//! preferences and `[theme]` sections for custom themes. Comments start with
//! `#`. Unknown keys are ignored so older builds can read newer files.

use crate::gfx::Color;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntryPref {
    pub id: String,
    pub alias: String,
    pub icon: String,
    pub hidden: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub theme: String,
    pub background: String,
    pub timeout: u32,
    pub default_entry: String,
    pub mouse_speed: u32,
    /// Locale code (e.g. "es-MX"); empty means "use the firmware language".
    pub language: String,
    pub entries: Vec<EntryPref>,
    pub themes: Vec<Theme>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "midnight".into(),
            background: String::new(),
            timeout: 10,
            default_entry: String::new(),
            mouse_speed: 8,
            language: String::new(),
            entries: vec![],
            themes: vec![],
        }
    }
}

/// Timeouts are 5..=60 seconds in steps of 5.
pub fn normalize_timeout(v: u32) -> u32 {
    ((v.clamp(5, 60) + 2) / 5) * 5
}

enum Section {
    Global,
    Entry(EntryPref),
    Theme(Theme),
}

impl Config {
    pub fn parse(text: &str) -> Config {
        let mut cfg = Config::default();
        let mut cur = Section::Global;
        let flush = |cfg: &mut Config, s: Section| match s {
            Section::Entry(e) if !e.id.is_empty() => cfg.entries.push(e),
            Section::Theme(t) if !t.id.is_empty() => cfg.themes.push(t),
            _ => {}
        };
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.eq_ignore_ascii_case("[entry]") {
                flush(&mut cfg, core::mem::replace(&mut cur, Section::Entry(EntryPref::default())));
                continue;
            }
            if line.eq_ignore_ascii_case("[theme]") {
                let base = builtin_themes().remove(0);
                let t = Theme { id: String::new(), name: String::new(), ..base };
                flush(&mut cfg, core::mem::replace(&mut cur, Section::Theme(t)));
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let k = k.trim().to_ascii_lowercase();
            // Values may carry a trailing "  # comment" (colours start with '#'
            // too, so only a '#' after the value counts).
            let v = v.trim();
            let v = match v.find(" #") {
                Some(i) if i > 0 => v[..i].trim(),
                _ => v,
            };
            match &mut cur {
                Section::Entry(e) => match k.as_str() {
                    "id" => e.id = v.into(),
                    "alias" => e.alias = v.into(),
                    "icon" => e.icon = v.into(),
                    "hidden" => e.hidden = Some(parse_bool(v)),
                    _ => {}
                },
                Section::Theme(t) => {
                    t.set(&k, v);
                }
                Section::Global => match k.as_str() {
                    "theme" => cfg.theme = v.to_ascii_lowercase(),
                    "background" => cfg.background = if v.eq_ignore_ascii_case("none") { String::new() } else { v.into() },
                    "timeout" => cfg.timeout = normalize_timeout(v.parse::<u32>().unwrap_or(10)),
                    "default" => cfg.default_entry = v.into(),
                    "mouse_speed" => cfg.mouse_speed = v.parse().unwrap_or(8).clamp(1, 64),
                    "language" => cfg.language = v.into(),
                    _ => {}
                },
            }
        }
        flush(&mut cfg, cur);
        cfg
    }

    pub fn serialize(&self) -> String {
        let mut s = String::new();
        s.push_str("# bootpick configuration: edited from the boot menu (gear icon),\n# the \"bootpick settings\" app, or by hand.\n");
        s.push_str(&format!("theme = {}\n", self.theme));
        s.push_str(&format!("background = {}\n", if self.background.is_empty() { "none" } else { &self.background }));
        s.push_str(&format!("timeout = {}\n", self.timeout));
        s.push_str(&format!("default = {}\n", self.default_entry));
        s.push_str(&format!("mouse_speed = {}\n", self.mouse_speed));
        if !self.language.is_empty() {
            s.push_str(&format!("language = {}\n", self.language));
        }
        for t in &self.themes {
            s.push_str("\n[theme]\n");
            s.push_str(&t.serialize());
        }
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

    /// Built-in themes followed by the custom ones from this config.
    pub fn all_themes(&self) -> Vec<Theme> {
        let mut v = builtin_themes();
        for t in &self.themes {
            if let Some(i) = v.iter().position(|b| b.id == t.id) {
                v[i] = t.clone();
            } else {
                v.push(t.clone());
            }
        }
        v
    }
}

fn parse_bool(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// `#rrggbb` or `#rrggbbaa`.
pub fn parse_color(v: &str) -> Option<Color> {
    let h = v.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        6 => Some(Color::rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

pub fn format_color(c: Color) -> String {
    if c.a == 255 {
        format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub id: String,
    pub name: String,
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

/// Editable colour fields: (config key, human label).
pub const THEME_FIELDS: &[(&str, &str)] = &[
    ("bg0", "Background top"),
    ("bg1", "Background bottom"),
    ("accent", "Accent"),
    ("text", "Text"),
    ("text_dim", "Secondary text"),
    ("panel", "Panels"),
    ("tile", "Tiles"),
    ("tile_hover", "Tiles (hover)"),
    ("danger", "Errors"),
];

impl Theme {
    pub fn color(&self, key: &str) -> Option<Color> {
        Some(match key {
            "bg0" => self.bg0,
            "bg1" => self.bg1,
            "accent" => self.accent,
            "text" => self.text,
            "text_dim" => self.text_dim,
            "panel" => self.panel,
            "tile" => self.tile,
            "tile_hover" => self.tile_hover,
            "danger" => self.danger,
            _ => return None,
        })
    }

    pub fn set_color(&mut self, key: &str, c: Color) {
        match key {
            "bg0" => self.bg0 = c,
            "bg1" => self.bg1 = c,
            "accent" => self.accent = c,
            "text" => self.text = c,
            "text_dim" => self.text_dim = c,
            "panel" => self.panel = c,
            "tile" => self.tile = c,
            "tile_hover" => self.tile_hover = c,
            "danger" => self.danger = c,
            _ => {}
        }
    }

    fn set(&mut self, k: &str, v: &str) {
        match k {
            "id" => self.id = v.to_ascii_lowercase(),
            "name" => self.name = v.into(),
            _ => {
                if let Some(c) = parse_color(v) {
                    self.set_color(k, c);
                }
            }
        }
    }

    fn serialize(&self) -> String {
        let mut s = format!("id = {}\nname = {}\n", self.id, self.name);
        for (k, _) in THEME_FIELDS {
            s.push_str(&format!("{} = {}\n", k, format_color(self.color(k).unwrap())));
        }
        s
    }

    /// Light themes darken photos less and use a light shade instead.
    pub fn is_light(&self) -> bool {
        let c = self.bg0;
        (c.r as u32 * 299 + c.g as u32 * 587 + c.b as u32 * 114) / 1000 > 150
    }
}

fn theme(id: &str, name: &str, c: [Color; 9]) -> Theme {
    Theme {
        id: id.to_string(),
        name: name.to_string(),
        bg0: c[0],
        bg1: c[1],
        accent: c[2],
        text: c[3],
        text_dim: c[4],
        panel: c[5],
        tile: c[6],
        tile_hover: c[7],
        danger: c[8],
    }
}

pub fn builtin_themes() -> Vec<Theme> {
    vec![
        theme("midnight", "Midnight", [
            Color::hex(0x0b1220), Color::hex(0x1e293b), Color::hex(0x38bdf8), Color::hex(0xf1f5f9), Color::hex(0x94a3b8),
            Color::rgba(15, 23, 42, 215), Color::rgba(255, 255, 255, 18), Color::rgba(255, 255, 255, 40), Color::hex(0xf87171),
        ]),
        theme("pop", "Pop", [
            Color::hex(0x1b1b1b), Color::hex(0x333333), Color::hex(0xffa800), Color::hex(0xf6f6f6), Color::hex(0xa8a8a8),
            Color::rgba(27, 27, 27, 220), Color::rgba(255, 255, 255, 16), Color::rgba(255, 255, 255, 36), Color::hex(0xff6b6b),
        ]),
        theme("nord", "Nord", [
            Color::hex(0x242933), Color::hex(0x3b4252), Color::hex(0x88c0d0), Color::hex(0xeceff4), Color::hex(0xa3abb9),
            Color::rgba(46, 52, 64, 220), Color::rgba(236, 239, 244, 18), Color::rgba(236, 239, 244, 40), Color::hex(0xbf616a),
        ]),
        theme("ember", "Ember", [
            Color::hex(0x160a0a), Color::hex(0x3b1515), Color::hex(0xfb923c), Color::hex(0xfff7ed), Color::hex(0xc7a79b),
            Color::rgba(30, 10, 10, 220), Color::rgba(255, 237, 213, 18), Color::rgba(255, 237, 213, 40), Color::hex(0xff5f5f),
        ]),
        theme("light", "Light", [
            Color::hex(0xf8fafc), Color::hex(0xcbd5e1), Color::hex(0x2563eb), Color::hex(0x0f172a), Color::hex(0x475569),
            Color::rgba(255, 255, 255, 225), Color::rgba(15, 23, 42, 14), Color::rgba(15, 23, 42, 30), Color::hex(0xdc2626),
        ]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_theme_round_trip() {
        let text = "theme = custom-1\ntimeout = 12 # comment\n\n[theme]\nid = custom-1\nname = Sunset\naccent = #ff8a5b\npanel = #24112ee0\n";
        let c = Config::parse(text);
        assert_eq!(c.timeout, 10);
        assert_eq!(c.themes.len(), 1);
        assert_eq!(c.themes[0].accent, Color::rgb(0xff, 0x8a, 0x5b));
        assert_eq!(c.themes[0].panel, Color::rgba(0x24, 0x11, 0x2e, 0xe0));
        let again = Config::parse(&c.serialize());
        assert_eq!(again, c);
        assert_eq!(c.all_themes().iter().find(|t| t.id == "custom-1").unwrap().name, "Sunset");
    }
}
