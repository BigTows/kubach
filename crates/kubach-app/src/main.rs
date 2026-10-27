mod action_context;
mod agent;
mod app_icon;
mod cluster;
#[cfg(debug_assertions)]
mod debug_overlay;
mod dock;
mod editor;
mod hotbar;
mod i18n;
mod images;
mod kubeconfig;
mod screens;
mod secret_clipboard;
mod settings;
mod shell_env;
mod state;
mod tools;
mod vault;
mod workspace;

use gpui_kit::component::ThemeMode;
use gpui_kit::*;

use workspace::Workspace;

actions!(kubach, [Quit]);

fn main() -> std::process::ExitCode {
    // `kubach mcp`: the MCP server over stdio, no window.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("mcp") {
        return agent::run_cli(&args[1..]);
    }

    // The login shell runs while the window opens (#299).
    let shell_env = shell_env::start();

    gpui_kit::application()
        .with_assets(kubach_ui::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            app_icon::set_dock_icon();
            shell_env::init(shell_env, cx);
            kubach_ui::theme::init(ThemeMode::Dark, cx);
            kubach_ui::init(cx);
            settings::init(cx);
            i18n::init(cx);
            state::init(cx);
            secret_clipboard::init(cx);
            vault::init(cx);
            agent::init(cx);
            tools::init(cx);
            images::init(cx);
            kubach_ui::terminal::init(cx);
            dock::init(cx);
            editor::init(cx);

            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("ctrl-q", Quit, None),
            ]);
            workspace::bind_keys(cx);
            #[cfg(debug_assertions)]
            debug_overlay::bind_keys(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            set_app_menus(cx);

            let options = kubach_ui::window::main_window_options("Kubach", cx);
            gpui_kit::open_window(options, cx, |window, cx| {
                settings::follow_system_appearance(window).detach();
                cx.new(|cx| {
                    agent::watch_confirmations(window, cx);
                    Workspace::new(window, cx)
                })
            })
            .expect("failed to open main window");
            cx.activate(true);
            kubach_ui::a11y_check::start(cx);
        });
    std::process::ExitCode::SUCCESS
}

/// The menu bar in the current language; set again after a language switch.
pub(crate) fn set_app_menus(cx: &mut App) {
    use kubach_i18n::t;

    let mut items = vec![
        MenuItem::action(t!("app-menu-settings"), settings::OpenSettings),
        MenuItem::separator(),
        MenuItem::action(t!("app-menu-new-terminal"), dock::NewTerminal),
        MenuItem::separator(),
    ];
    #[cfg(debug_assertions)]
    items.extend([debug_overlay::menu_item(), MenuItem::separator()]);
    items.push(MenuItem::action(t!("app-menu-quit"), Quit));
    cx.set_menus([Menu::new("Kubach").items(items)]);
}
