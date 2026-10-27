//! Interface language (ADR 0018): applies `general.language` from the
//! settings at startup and on every change, without a restart.
//!
//! What a switch touches: the locale of `kubach-i18n` (read by `t!` in
//! `render`), gpui-component's own strings, the fallback fonts for Chinese,
//! the application menu, and every window, which is redrawn. Text stored in
//! state rather than built in `render`, such as input placeholders, follows
//! through [`localized_placeholder`].

use gpui_kit::component::input::InputState;
use gpui_kit::*;
use kubach_core::settings::Language;
use kubach_i18n::Locale;

use crate::settings::AppSettings;

/// The language in use; observe it to update text kept in state (it
/// notifies after the switch is applied).
pub struct LanguageState {
    locale: Locale,
}

impl LanguageState {
    /// Switches to `language`; `false` when it is already in use.
    fn set(&mut self, language: Language) -> bool {
        let locale = locale_of(language);
        if locale == self.locale {
            return false;
        }
        self.locale = locale;
        true
    }
}

struct GlobalLanguage(Entity<LanguageState>);

impl Global for GlobalLanguage {}

pub fn global(cx: &App) -> Option<Entity<LanguageState>> {
    cx.try_global::<GlobalLanguage>()
        .map(|global| global.0.clone())
}

pub fn locale_of(language: Language) -> Locale {
    match language {
        Language::En => Locale::En,
        Language::Ru => Locale::Ru,
        Language::ZhHans => Locale::ZhHans,
    }
}

/// gpui-component's name for `locale` (dialog buttons, calendar, input
/// menus). It has no Russian yet, so Russian keeps its English strings.
pub fn component_locale(locale: Locale) -> &'static str {
    match locale {
        Locale::ZhHans => "zh-CN",
        Locale::En | Locale::Ru => "en",
    }
}

/// Fallback families for the text of `locale`: Simplified Chinese forms of
/// Han characters for Chinese, the OS choice otherwise.
pub fn text_fallbacks(locale: Locale) -> &'static [&'static str] {
    match locale {
        Locale::ZhHans => &kubach_ui::theme::SIMPLIFIED_CHINESE_FALLBACKS,
        Locale::En | Locale::Ru => &[],
    }
}

/// Applies the language of the settings now and on every change. Call
/// after `settings::init` and before the first window opens.
pub fn init(cx: &mut App) {
    let settings = AppSettings::global(cx);
    let language = settings.read(cx).get().general.language;
    let mut state = LanguageState { locale: Locale::En };
    state.set(language);
    apply(state.locale, cx);
    let entity = cx.new(|_| state);
    cx.set_global(GlobalLanguage(entity.clone()));

    cx.observe(&settings, move |settings, cx| {
        let language = settings.read(cx).get().general.language;
        let changed = entity.update(cx, |state, cx| {
            let changed = state.set(language);
            if changed {
                cx.notify();
            }
            changed
        });
        if changed {
            apply(locale_of(language), cx);
            crate::set_app_menus(cx);
            cx.refresh_windows();
        }
    })
    .detach();
}

/// Process-wide side effects of `locale`.
fn apply(locale: Locale, cx: &mut App) {
    kubach_i18n::set_locale(locale);
    gpui_kit::component::set_locale(component_locale(locale));
    kubach_ui::theme::set_text_fallbacks(text_fallbacks(locale), cx);
}

/// Keeps the placeholder of `input` in the current language: sets it now
/// and again after each language switch. Keep the subscription with the
/// view that owns the input.
pub fn localized_placeholder<V: 'static>(
    input: &Entity<InputState>,
    text: fn() -> String,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Option<Subscription> {
    input.update(cx, |input, cx| input.set_placeholder(text(), window, cx));
    let language = global(cx)?;
    let input = input.clone();
    Some(cx.observe_in(&language, window, move |_, _, window, cx| {
        input.update(cx, |input, cx| input.set_placeholder(text(), window, cx));
    }))
}

#[cfg(test)]
mod tests {
    use kubach_core::settings::Language;
    use kubach_i18n::Locale;

    use super::{LanguageState, component_locale, locale_of, text_fallbacks};

    #[test]
    fn every_setting_has_its_locale() {
        assert_eq!(locale_of(Language::En), Locale::En);
        assert_eq!(locale_of(Language::Ru), Locale::Ru);
        assert_eq!(locale_of(Language::ZhHans), Locale::ZhHans);
        // gpui-component knows `zh-CN`, not `zh-Hans`, and has no Russian.
        assert_eq!(component_locale(Locale::ZhHans), "zh-CN");
        assert_eq!(component_locale(Locale::Ru), "en");
        assert_eq!(component_locale(Locale::En), "en");
    }

    #[test]
    fn only_chinese_sets_fallback_fonts() {
        assert!(text_fallbacks(Locale::En).is_empty());
        assert!(text_fallbacks(Locale::Ru).is_empty());
        let chinese = text_fallbacks(Locale::ZhHans);
        assert_eq!(chinese.first(), Some(&"PingFang SC"));
        assert!(chinese.contains(&"Microsoft YaHei"));
        assert!(chinese.contains(&"Noto Sans CJK SC"));
    }

    #[test]
    fn switching_the_language_reports_a_change_once() {
        let mut state = LanguageState { locale: Locale::En };
        assert!(!state.set(Language::En));
        assert!(state.set(Language::Ru));
        assert_eq!(state.locale, Locale::Ru);
        assert!(!state.set(Language::Ru));
        assert!(state.set(Language::ZhHans));
        assert_eq!(state.locale, Locale::ZhHans);
    }
}
