//! Two-language UI text. Call sites carry both strings side by side, which
//! keeps translations next to the code that uses them.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Es,
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn current() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == Lang::Es as u8 {
        Lang::Es
    } else {
        Lang::En
    }
}

/// The system's language, if it is one we speak.
pub fn system() -> Lang {
    match sys_locale::get_locale() {
        Some(locale) if locale.to_ascii_lowercase().starts_with("es") => Lang::Es,
        _ => Lang::En,
    }
}

/// Pick the string for the current language.
pub fn tr(en: &'static str, es: &'static str) -> &'static str {
    match current() {
        Lang::En => en,
        Lang::Es => es,
    }
}
