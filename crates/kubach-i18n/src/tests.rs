use std::collections::BTreeMap;

use crate::{KEYS, Locale, Message, RESOURCES, ftl, has_message, locale, msg, set_locale, t};

/// Messages of every file of `locale`, merged.
fn messages(locale: Locale) -> ftl::Messages {
    let mut all = ftl::Messages::new();
    for (name, source) in RESOURCES[locale.index()] {
        let messages = ftl::messages(source)
            .unwrap_or_else(|error| panic!("locales/{}/{name}: {error}", locale.tag()));
        for (id, args) in messages {
            assert!(
                all.insert(id.clone(), args).is_none(),
                "`{id}` is defined twice in locales/{}",
                locale.tag()
            );
        }
    }
    all
}

#[test]
fn every_message_is_translated_with_the_same_arguments() {
    let english = messages(Locale::En);
    assert!(!english.is_empty());
    for locale in [Locale::Ru, Locale::ZhHans] {
        let translated = messages(locale);
        let tag = locale.tag();
        let missing: Vec<_> = english
            .keys()
            .filter(|id| !translated.contains_key(*id))
            .collect();
        assert!(missing.is_empty(), "not translated into {tag}: {missing:?}");
        let extra: Vec<_> = translated
            .keys()
            .filter(|id| !english.contains_key(*id))
            .collect();
        assert!(extra.is_empty(), "in {tag} but not in English: {extra:?}");
        let differ: BTreeMap<_, _> = english
            .iter()
            .filter(|(id, args)| translated[*id] != **args)
            .map(|(id, args)| (id, (args, &translated[id])))
            .collect();
        assert!(
            differ.is_empty(),
            "arguments differ in {tag} (en, {tag}): {differ:?}"
        );
    }
}

#[test]
fn every_locale_loads_into_a_bundle() {
    for locale in Locale::ALL {
        // Panics on a syntax error or a duplicate id.
        let _ = crate::bundle(locale);
    }
}

#[test]
fn compile_time_keys_match_english() {
    let english = messages(Locale::En);
    assert_eq!(KEYS.len(), english.len());
    for (id, args) in KEYS {
        let expected: Vec<&str> = english[*id].iter().map(String::as_str).collect();
        assert_eq!(*args, expected.as_slice(), "{id}");
        assert!(has_message(id));
    }
}

#[test]
fn russian_plurals() {
    let forwards = |n: u32| msg!("status-forwards", count = n).translate(Locale::Ru);
    assert_eq!(forwards(1), "1 переадресация портов");
    assert_eq!(forwards(2), "2 переадресации портов");
    assert_eq!(forwards(5), "5 переадресаций портов");
    assert_eq!(forwards(11), "11 переадресаций портов");
    assert_eq!(forwards(21), "21 переадресация портов");
    assert_eq!(forwards(22), "22 переадресации портов");
}

#[test]
fn english_and_chinese_plurals() {
    let forwards = |n: u32, locale| msg!("status-forwards", count = n).translate(locale);
    assert_eq!(forwards(1, Locale::En), "1 port forward");
    assert_eq!(forwards(21, Locale::En), "21 port forwards");
    assert_eq!(forwards(1, Locale::ZhHans), "1 个端口转发");
    assert_eq!(forwards(5, Locale::ZhHans), "5 个端口转发");
}

#[test]
fn arguments_are_substituted_without_isolation_marks() {
    let message = msg!("settings-saved-to", path = "/tmp/settings.toml");
    assert_eq!(message.english(), "Saved to /tmp/settings.toml");
    assert_eq!(
        message.translate(Locale::Ru),
        "Сохраняются в /tmp/settings.toml"
    );
    assert_eq!(
        msg!(
            "status-cluster-version",
            context = "kind",
            version = "v1.31.0"
        )
        .translate(Locale::ZhHans),
        "kind · v1.31.0"
    );
}

#[test]
fn missing_messages_fall_back_to_english_then_to_the_id() {
    use fluent_bundle::FluentResource;
    use fluent_bundle::concurrent::FluentBundle;

    let bundle = |locale: Locale, source: &str| {
        let mut bundle = FluentBundle::new_concurrent(vec![locale.tag().parse().unwrap()]);
        bundle.set_use_isolating(false);
        bundle
            .add_resource(FluentResource::try_new(source.to_owned()).unwrap())
            .unwrap();
        bundle
    };
    let bundles = [
        bundle(
            Locale::En,
            "hello = Hello, { $name }\nonly-english = Only in English, { $name }",
        ),
        bundle(Locale::Ru, "hello = Привет, { $name }"),
        bundle(Locale::ZhHans, ""),
    ];
    let text = |id: &'static str, locale| {
        Message::new(id)
            .arg("name", "Ann")
            .translate_with(&bundles, locale)
    };
    assert_eq!(text("hello", Locale::Ru), "Привет, Ann");
    assert_eq!(text("only-english", Locale::Ru), "Only in English, Ann");
    assert_eq!(text("hello", Locale::ZhHans), "Hello, Ann");
    assert_eq!(text("nowhere", Locale::Ru), "nowhere");
    assert!(!has_message("nowhere"));
}

#[test]
fn switching_the_locale_changes_t() {
    // The only test that touches the process locale.
    assert_eq!(locale(), Locale::En);
    assert_eq!(t!("settings-theme"), "Theme");
    set_locale(Locale::Ru);
    assert_eq!(locale(), Locale::Ru);
    assert_eq!(t!("settings-theme"), "Тема");
    assert_eq!(t!("status-forwards", count = 3), "3 переадресации портов");
    assert_eq!(msg!("app-menu-quit").to_string(), "Завершить Kubach");
    set_locale(Locale::ZhHans);
    assert_eq!(t!("settings-theme"), "主题");
    set_locale(Locale::En);
    assert_eq!(t!("settings-theme"), "Theme");
}

/// Rough width of one line of `text` in em of the UI font (Geist, CJK from
/// the fallbacks): wide characters 1 em, capitals 0.66, spaces 0.28, the
/// rest 0.56. Good to a few percent for Latin and Cyrillic, enough to catch
/// a label that outgrew its place.
fn em_width(text: &str) -> f32 {
    text.chars()
        .map(|c| match c {
            ' ' => 0.28,
            '\u{1100}'..='\u{115F}'
            | '\u{2E80}'..='\u{A4CF}'
            | '\u{AC00}'..='\u{D7A3}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FE30}'..='\u{FE4F}'
            | '\u{FF00}'..='\u{FF60}'
            | '\u{FFE0}'..='\u{FFE6}' => 1.0,
            c if c.is_uppercase() => 0.66,
            _ => 0.56,
        })
        .sum()
}

/// Labels of fixed-width places stay readable in every language (#444):
/// the workspace sidebar (240 px) and the settings sidebar (220 px) leave a
/// nav label about 160 px at 14 px, 11.5 em. A longer one is cut with "…"
/// (the full text is then a tooltip), so shorten the translation instead.
#[test]
fn sidebar_labels_fit() {
    const BUDGET_EM: f32 = 11.5;
    let english = messages(Locale::En);
    let ids: Vec<&String> = english
        .keys()
        .filter(|id| {
            (id.starts_with("nav-") && !id.starts_with("nav-back-"))
                || id.starts_with("settings-section-")
                || id.starts_with("settings-nav-")
        })
        .collect();
    assert!(ids.len() > 20, "sidebar labels not found: {ids:?}");
    let mut long = Vec::new();
    for locale in Locale::ALL {
        for id in &ids {
            let text = Message::new((*id).clone()).translate(locale);
            let width = em_width(&text);
            if width > BUDGET_EM {
                long.push(format!("{} {id} = {text:?} ({width:.1} em)", locale.tag()));
            }
        }
    }
    assert!(
        long.is_empty(),
        "longer than {BUDGET_EM} em:\n{}",
        long.join("\n")
    );
}

#[test]
fn em_width_counts_wide_characters_double() {
    assert!((em_width("ab") - 1.12).abs() < 1e-4);
    assert!((em_width("工作负载") - 4.0).abs() < 1e-4);
    assert!(em_width("Рабочие нагрузки") < em_width("Пользовательские ресурсы"));
}
