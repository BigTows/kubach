//! The main window with the accessibility tree on (#354): a debug build
//! panics on a duplicate node id. Ignored because it opens the window, once
//! per interface language (#444);
//! run with `cargo test -p kubach-app --test a11y_check -- --ignored`.

#[cfg(target_os = "macos")]
#[test]
#[ignore = "opens the main window"]
fn main_window_a11y_tree_has_unique_ids() {
    use kubach_core::settings::{self, Language, Settings};

    for (tag, language) in [
        ("en", Language::En),
        ("ru", Language::Ru),
        ("zh-Hans", Language::ZhHans),
    ] {
        // A fresh config dir: no saved layout, the first-run window, in
        // `language`.
        let config =
            std::env::temp_dir().join(format!("kubach-a11y-check-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&config).unwrap();
        let mut prefs = Settings::default();
        prefs.general.language = language;
        settings::save(&config.join(settings::SETTINGS_FILE), &prefs).unwrap();
        let result = kubach_ui::a11y_check::run(
            std::process::Command::new(env!("CARGO_BIN_EXE_kubach"))
                .env(settings::CONFIG_DIR_ENV, &config),
        );
        std::fs::remove_dir_all(&config).ok();
        if let Err(err) = result {
            panic!("{tag}: {err}");
        }
    }
}
