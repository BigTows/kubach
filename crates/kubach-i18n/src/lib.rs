//! Interface translations of Kubach (ADR 0018), without UI dependencies.
//!
//! Messages live in Fluent files `locales/<tag>/*.ftl`, embedded in the
//! binary: English (the source), Russian and Simplified Chinese. Code refers
//! to a message by its id:
//!
//! ```
//! use kubach_i18n::{Locale, msg, t};
//!
//! // Translated into the current locale of the process.
//! let label: String = t!("status-forwards", count = 2);
//! assert_eq!(label, "2 port forwards");
//!
//! // Kept and translated later, e.g. by the UI after a language switch.
//! let message = msg!("status-forwards", count = 21);
//! assert_eq!(message.translate(Locale::Ru), "21 переадресация портов");
//! ```
//!
//! `t!` and `msg!` check at compile time that the id exists in English and
//! takes exactly the given arguments:
//!
//! ```compile_fail
//! // No such message in locales/en.
//! let _ = kubach_i18n::t!("status-no-such-message");
//! ```
//!
//! ```compile_fail
//! // `status-forwards` takes `count`, not `n`.
//! let _ = kubach_i18n::t!("status-forwards", n = 2);
//! ```
//!
//! Plurals and word order are up to each
//! translation (`{ $count -> [one] … *[other] … }`), so code passes numbers
//! as arguments instead of choosing between "1 item" and "N items". A
//! message missing in a locale falls back to English, then to its id.

#[cfg(test)]
mod ftl;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::fmt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

include!(concat!(env!("OUT_DIR"), "/resources.rs"));

/// A language of the interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Locale {
    #[default]
    En,
    Ru,
    /// Simplified Chinese.
    ZhHans,
}

impl Locale {
    pub const ALL: [Locale; 3] = [Locale::En, Locale::Ru, Locale::ZhHans];

    /// BCP 47 tag, also the directory under `locales/`.
    pub fn tag(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Ru => "ru",
            Locale::ZhHans => "zh-Hans",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    fn from_index(index: u8) -> Self {
        Locale::ALL
            .get(usize::from(index))
            .copied()
            .unwrap_or_default()
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(Locale::En as u8);

/// Switches the language `t!` and [`Message::to_string`] translate into.
/// The UI redraws itself; this only changes what it reads.
pub fn set_locale(locale: Locale) {
    CURRENT.store(locale as u8, Ordering::Relaxed);
}

/// The language of the process, English until [`set_locale`].
pub fn locale() -> Locale {
    Locale::from_index(CURRENT.load(Ordering::Relaxed))
}

type Bundle = FluentBundle<FluentResource>;

fn bundles() -> &'static [Bundle; 3] {
    static BUNDLES: OnceLock<[Bundle; 3]> = OnceLock::new();
    BUNDLES.get_or_init(|| Locale::ALL.map(bundle))
}

fn bundle(locale: Locale) -> Bundle {
    let tag: LanguageIdentifier = locale.tag().parse().expect("valid language tag");
    let mut bundle = FluentBundle::new_concurrent(vec![tag]);
    // Bidi isolation marks around arguments only matter for right-to-left
    // scripts; GPUI would draw them as missing glyphs.
    bundle.set_use_isolating(false);
    for (name, source) in RESOURCES[locale.index()] {
        let resource = FluentResource::try_new((*source).to_owned())
            .unwrap_or_else(|(_, errors)| panic!("locales/{}/{name}: {errors:?}", locale.tag()));
        bundle
            .add_resource(resource)
            .unwrap_or_else(|errors| panic!("locales/{}/{name}: {errors:?}", locale.tag()));
    }
    bundle
}

/// Whether English has a message `id` (ids built at run time).
pub fn has_message(id: &str) -> bool {
    bundles()[Locale::En.index()].has_message(id)
}

/// Value of a message argument: a number (plural forms select on it),
/// text, or other messages translated in the same language (one, or a
/// list joined with commas).
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Number(f64),
    Text(String),
    Message(Box<Message>),
    List(Vec<Message>),
}

impl From<Message> for Arg {
    fn from(value: Message) -> Self {
        Arg::Message(Box::new(value))
    }
}

impl From<Vec<Message>> for Arg {
    fn from(value: Vec<Message>) -> Self {
        Arg::List(value)
    }
}

macro_rules! number_args {
    ($($ty:ty),*) => {$(
        impl From<$ty> for Arg {
            fn from(value: $ty) -> Self {
                Arg::Number(value as f64)
            }
        }
    )*};
}
number_args!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, f32, f64);

impl From<&str> for Arg {
    fn from(value: &str) -> Self {
        Arg::Text(value.to_owned())
    }
}

impl From<String> for Arg {
    fn from(value: String) -> Self {
        Arg::Text(value)
    }
}

impl From<&String> for Arg {
    fn from(value: &String) -> Self {
        Arg::Text(value.clone())
    }
}

impl From<Cow<'_, str>> for Arg {
    fn from(value: Cow<'_, str>) -> Self {
        Arg::Text(value.into_owned())
    }
}

/// A message to translate later: its id and arguments. Code that has no
/// UI (`kubach-core`) returns these; the UI translates them when drawing,
/// so a language switch shows at once.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: Cow<'static, str>,
    pub args: Vec<(Cow<'static, str>, Arg)>,
}

impl Message {
    /// A message by an id not known at compile time; prefer [`msg!`],
    /// which checks the id and arguments.
    pub fn new(id: impl Into<Cow<'static, str>>) -> Self {
        Self {
            id: id.into(),
            args: Vec::new(),
        }
    }

    pub fn arg(mut self, name: impl Into<Cow<'static, str>>, value: impl Into<Arg>) -> Self {
        self.args.push((name.into(), value.into()));
        self
    }

    /// The text in `locale`, English when the locale lacks the message, the
    /// id when English lacks it too.
    pub fn translate(&self, locale: Locale) -> String {
        self.translate_with(bundles(), locale)
    }

    fn translate_with(&self, bundles: &[Bundle; 3], locale: Locale) -> String {
        let mut args = FluentArgs::with_capacity(self.args.len());
        for (name, value) in &self.args {
            let value = match value {
                Arg::Number(number) => FluentValue::from(*number),
                Arg::Text(text) => FluentValue::from(text.as_str()),
                Arg::Message(message) => FluentValue::from(message.translate_with(bundles, locale)),
                Arg::List(messages) => FluentValue::from(
                    messages
                        .iter()
                        .map(|message| message.translate_with(bundles, locale))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            };
            args.set(name.as_ref(), value);
        }
        for locale in [locale, Locale::En] {
            let bundle = &bundles[locale.index()];
            if let Some(pattern) = bundle.get_message(&self.id).and_then(|m| m.value()) {
                let mut errors = Vec::new();
                return bundle
                    .format_pattern(pattern, Some(&args), &mut errors)
                    .into_owned();
            }
        }
        self.id.to_string()
    }

    /// The English text, for logs, bug reports and AI agents, which get
    /// English whatever the interface language.
    pub fn english(&self) -> String {
        self.translate(Locale::En)
    }
}

/// The text in the current locale ([`locale`]).
impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.translate(locale()))
    }
}

/// Result of [`__check`].
#[doc(hidden)]
pub enum __Check {
    Ok,
    UnknownId,
    WrongArgs,
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn contains(list: &[&str], item: &str) -> bool {
    let mut i = 0;
    while i < list.len() {
        if str_eq(list[i], item) {
            return true;
        }
        i += 1;
    }
    false
}

/// Compile-time check of `t!`/`msg!`: `id` is an English message and takes
/// exactly `args` (each once).
#[doc(hidden)]
pub const fn __check(id: &str, args: &[&str]) -> __Check {
    let mut k = 0;
    while k < KEYS.len() {
        let (key, expected) = KEYS[k];
        if str_eq(key, id) {
            if args.len() != expected.len() {
                return __Check::WrongArgs;
            }
            let mut i = 0;
            while i < args.len() {
                if !contains(expected, args[i]) {
                    return __Check::WrongArgs;
                }
                i += 1;
            }
            return __Check::Ok;
        }
        k += 1;
    }
    __Check::UnknownId
}

/// A [`Message`] with a checked id and arguments:
/// `msg!("status-forwards", count = n)`. Argument names are the Fluent
/// variables without `$`; values are numbers or text.
#[macro_export]
macro_rules! msg {
    ($id:literal $(, $name:ident = $value:expr)* $(,)?) => {{
        const _: () = match $crate::__check($id, &[$(stringify!($name)),*]) {
            $crate::__Check::Ok => {}
            $crate::__Check::UnknownId => {
                panic!(concat!("no message `", $id, "` in kubach-i18n/locales/en"))
            }
            $crate::__Check::WrongArgs => panic!(concat!(
                "message `", $id, "` in kubach-i18n/locales/en takes other arguments than: ",
                stringify!($($name),*)
            )),
        };
        $crate::Message::new($id) $(.arg(stringify!($name), $value))*
    }};
}

/// The text of a checked message in the current locale:
/// `t!("settings-theme")`, `t!("status-forwards", count = n)`.
#[macro_export]
macro_rules! t {
    ($($tokens:tt)*) => {
        $crate::msg!($($tokens)*).translate($crate::locale())
    };
}
