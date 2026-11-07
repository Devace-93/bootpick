//! UI strings. The catalog lives in `assets/i18n/strings.json` and is
//! compiled into `i18n_data.rs` by `tools/gen-i18n.py`.

use crate::i18n_data::{KEY_COUNT, TABLE};
use crate::langs;
use alloc::string::String;

pub use crate::i18n_data::Msg;

#[derive(Clone, Copy, Debug)]
pub struct Strings {
    row: &'static [&'static str; KEY_COUNT],
    rtl: bool,
}

impl Strings {
    /// Strings for a locale code such as `es-MX`; falls back to English.
    pub fn for_lang(code: &str) -> Strings {
        let base = langs::base_of(code);
        let row = TABLE.iter().find(|(b, _)| *b == base).or_else(|| TABLE.iter().find(|(b, _)| *b == "en")).map(|(_, r)| r).unwrap_or(&TABLE[0].1);
        Strings { row, rtl: langs::is_rtl(code) }
    }

    pub fn get(&self, m: Msg) -> &'static str {
        self.row[m as usize]
    }

    /// Replaces `{0}`, `{1}`... with `args`.
    pub fn fmt(&self, m: Msg, args: &[&str]) -> String {
        let mut out = String::from(self.get(m));
        for (i, a) in args.iter().enumerate() {
            let pat = alloc::format!("{{{}}}", i);
            out = out.replace(&pat, a);
        }
        out
    }

    pub fn rtl(&self) -> bool {
        self.rtl
    }
}
