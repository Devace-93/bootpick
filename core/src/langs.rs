//! The locales offered in the language picker: the same list as the other
//! 3m4 apps (one entry per country with that country's official language).

use crate::i18n_data::LANGS;

pub const DEFAULT: &str = "en-US";

#[derive(Clone, Copy, Debug)]
pub struct Lang {
    pub code: &'static str,
    /// ISO 3166 country code, used for the flag (`flags\<country>.bmp`).
    pub country: &'static str,
    pub name: &'static str,
}

pub fn all() -> impl Iterator<Item = Lang> {
    LANGS.iter().map(|(code, country, name)| Lang { code, country, name })
}

pub fn count() -> usize {
    LANGS.len()
}

pub fn get(i: usize) -> Lang {
    let (code, country, name) = LANGS[i.min(LANGS.len() - 1)];
    Lang { code, country, name }
}

pub fn index_of(code: &str) -> Option<usize> {
    LANGS.iter().position(|(c, _, _)| c.eq_ignore_ascii_case(code))
}

pub fn base_of(code: &str) -> &str {
    code.get(..2).unwrap_or(code)
}

pub fn is_rtl(code: &str) -> bool {
    matches!(base_of(code), "ar")
}

/// Picks the active locale from a saved value and the firmware's
/// `PlatformLang` (e.g. "en-US"), falling back to English (US).
pub fn resolve(saved: &str, platform: &str) -> &'static str {
    for want in [saved, platform] {
        if want.is_empty() {
            continue;
        }
        if let Some(i) = index_of(want) {
            return LANGS[i].0;
        }
        let base = base_of(want);
        if let Some((c, _, _)) = LANGS.iter().find(|(c, _, _)| base_of(c).eq_ignore_ascii_case(base)) {
            return c;
        }
    }
    DEFAULT
}
