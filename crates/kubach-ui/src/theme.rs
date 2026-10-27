use std::borrow::Cow;

use gpui_kit::base::{Root, RootPlugin};
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{
    App, Context, Div, Empty, FontFallbacks, Global, Hsla, IntoElement, Render, Rgba, SharedString,
    Stateful, Styled as _, Window, font,
};

const THEMES: &str = include_str!("../themes/kubach.json");

pub const DARK: &str = "Kubach Dark";
pub const LIGHT: &str = "Kubach Light";

/// UI font family, embedded from `assets/fonts` (SIL OFL 1.1).
pub const FONT_FAMILY: &str = "Geist";
/// Monospace font family for code, names and numbers.
pub const MONO_FONT_FAMILY: &str = "Geist Mono";

/// Static TTFs from the `vercel/geist-font` release.
const FONTS: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
];

/// Registers the Kubach fonts and themes and activates `mode`.
///
/// Must run after `gpui_kit::init`, which installs the theme registry.
pub fn init(mode: ThemeMode, cx: &mut App) {
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(THEMES)
        .expect("bundled Kubach themes must parse");

    cx.set_global(OpaqueBackgrounds {
        dark: opaque_background_of(DARK),
        light: opaque_background_of(LIGHT),
    });
    cx.set_global(LoadingBarColors {
        dark: loading_bar_of(DARK),
        light: loading_bar_of(LIGHT),
    });
    cx.set_global(TrafficPalettes {
        dark: traffic_palette_of(DARK),
        light: traffic_palette_of(LIGHT),
    });
    cx.set_global(AnsiPalettes {
        dark: ansi_palette_of(DARK),
        light: ansi_palette_of(LIGHT),
    });
    cx.set_global(UsageMarkers {
        dark: usage_markers_of(DARK),
        light: usage_markers_of(LIGHT),
    });

    let fonts = register_fonts(cx);
    let themes = ThemeRegistry::global(cx).themes();
    let dark = themes[DARK].clone();
    let light = themes[LIGHT].clone();
    Theme::update(cx, |theme| {
        theme.dark_theme = dark;
        theme.light_theme = light;
        // Set here rather than in kubach.json, which would apply the families
        // even when the fonts failed to load.
        if let Some((sans, mono)) = fonts {
            theme.font_family = sans;
            theme.mono_font_family = mono;
        }
    });
    Theme::change(mode, None, cx);

    cx.set_global(TextFallbacks(None));
    Root::register_plugin::<TextFallbackPlugin>(cx, |_, _| TextFallbackPlugin);
}

/// Families for Han characters in Simplified Chinese, which Geist lacks:
/// macOS, Windows, then Linux. Without them the OS may pick a Japanese face
/// for the same characters (Han unification, ADR 0018). Missing families
/// are skipped.
pub const SIMPLIFIED_CHINESE_FALLBACKS: [&str; 6] = [
    "PingFang SC",
    "Microsoft YaHei UI",
    "Microsoft YaHei",
    "Noto Sans CJK SC",
    "Source Han Sans SC",
    "WenQuanYi Micro Hei",
];

/// Fallback families of every window's text, `None` for the OS defaults.
struct TextFallbacks(Option<FontFallbacks>);

impl Global for TextFallbacks {}

/// Sets the fallback families of the text in every window, for example
/// [`SIMPLIFIED_CHINESE_FALLBACKS`] while the interface is in Chinese; an
/// empty list leaves the choice to the OS. Monospace text inherits them.
/// Windows pick the change up on their next draw (`cx.refresh_windows()`).
pub fn set_text_fallbacks(families: &[&str], cx: &mut App) {
    let fallbacks = (!families.is_empty())
        .then(|| FontFallbacks::from_fonts(families.iter().map(|f| (*f).to_owned()).collect()));
    cx.set_global(TextFallbacks(fallbacks));
}

/// The fallback families set by [`set_text_fallbacks`], `None` for the OS
/// defaults.
pub(crate) fn text_fallbacks(cx: &App) -> Option<FontFallbacks> {
    cx.try_global::<TextFallbacks>()
        .and_then(|TextFallbacks(fallbacks)| fallbacks.clone())
}

/// Puts [`TextFallbacks`] on the root of each window, so overlays (dialogs,
/// menus, notifications) get them too.
struct TextFallbackPlugin;

impl Render for TextFallbackPlugin {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl RootPlugin for TextFallbackPlugin {
    fn style(&self, surface: &mut Stateful<Div>, _: &mut Window, cx: &mut App) {
        if let Some(TextFallbacks(Some(fallbacks))) = cx.try_global::<TextFallbacks>() {
            surface.text_style().font_fallbacks = Some(fallbacks.clone());
        }
    }
}

/// Adds the embedded fonts to the text system and returns the UI and mono
/// families, or `None` (system fonts stay) when GPUI cannot resolve them.
fn register_fonts(cx: &mut App) -> Option<(SharedString, SharedString)> {
    let text_system = cx.text_system();
    let fonts = FONTS.iter().map(|data| Cow::Borrowed(*data)).collect();
    if let Err(err) = text_system.add_fonts(fonts) {
        eprintln!("kubach-ui: failed to load Geist fonts, using system fonts: {err:#}");
        return None;
    }
    // An unknown family silently falls back, so check that GPUI resolves the
    // names to the fonts just added.
    for family in [FONT_FAMILY, MONO_FONT_FAMILY] {
        let resolved = text_system
            .get_font_for_id(text_system.resolve_font(&font(family)))
            .map(|font| font.family);
        if resolved.as_deref() != Some(family) {
            eprintln!("kubach-ui: font {family:?} resolves to {resolved:?}, using system fonts");
            return None;
        }
    }
    Some((FONT_FAMILY.into(), MONO_FONT_FAMILY.into()))
}

/// Switches between Kubach Dark and Kubach Light.
pub fn toggle(window: &mut Window, cx: &mut App) {
    let mode = if cx.theme().mode.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(mode, Some(window), cx);
}

/// Light or dark, as the OS currently appears.
pub fn system_mode(cx: &App) -> ThemeMode {
    cx.window_appearance().into()
}

/// Activates `mode` unless it is already active.
pub fn set_mode(mode: ThemeMode, cx: &mut App) {
    if cx.theme().mode != mode {
        Theme::change(mode, None, cx);
    }
}

/// Whether the platform blurs what shows through a translucent window.
///
/// macOS and Windows do. On Linux only KDE implements the blur protocol, so
/// elsewhere the desktop would show through the translucent theme background
/// unblurred: Kubach windows stay opaque there.
pub const BLURS_BEHIND: bool = !cfg!(target_os = "linux");

/// Kubach-only key in `kubach.json`: the opaque window background for
/// platforms without blur. gpui-component's theme schema skips unknown keys.
const WINDOW_BACKGROUND_KEY: &str = "window.background";

/// Opaque window backgrounds of Kubach Dark and Kubach Light.
struct OpaqueBackgrounds {
    dark: Hsla,
    light: Hsla,
}

impl Global for OpaqueBackgrounds {}

/// Reads [`WINDOW_BACKGROUND_KEY`] of the bundled theme named `theme`.
fn opaque_background_of(theme: &str) -> Hsla {
    let set: serde_json::Value =
        serde_json::from_str(THEMES).expect("bundled Kubach themes must parse");
    let value = set["themes"]
        .as_array()
        .and_then(|themes| themes.iter().find(|t| t["name"] == theme))
        .and_then(|t| t["colors"][WINDOW_BACKGROUND_KEY].as_str())
        .unwrap_or_else(|| panic!("{theme} must define {WINDOW_BACKGROUND_KEY}"));
    let color = Rgba::try_from(value)
        .unwrap_or_else(|err| panic!("{theme} {WINDOW_BACKGROUND_KEY} {value:?}: {err}"));
    Hsla::from(color)
}

/// Opaque background of the active theme, for windows the desktop must not
/// show through.
pub fn opaque_background(cx: &App) -> Hsla {
    let theme = cx.theme();
    match cx.try_global::<OpaqueBackgrounds>() {
        Some(backgrounds) if theme.mode.is_dark() => backgrounds.dark,
        Some(backgrounds) => backgrounds.light,
        // `init` has not run: the theme background without its alpha.
        None => Hsla {
            a: 1.,
            ..theme.background
        },
    }
}

/// Background of a main window's root view: the translucent theme background
/// where the platform blurs behind it ([`BLURS_BEHIND`]), opaque otherwise.
pub fn window_background(cx: &App) -> Hsla {
    if BLURS_BEHIND {
        cx.theme().background
    } else {
        opaque_background(cx)
    }
}

/// Kubach-only keys in `kubach.json`: the moving segment and the track of
/// the indeterminate [`LoadingBar`](crate::LoadingBar).
pub const LOADING_BAR_KEYS: [&str; 2] = ["loading_bar.foreground", "loading_bar.track"];

/// `(bar, track)` colors of the loading bar in Kubach Dark and Kubach Light.
struct LoadingBarColors {
    dark: (Hsla, Hsla),
    light: (Hsla, Hsla),
}

impl Global for LoadingBarColors {}

/// Reads a Kubach-only color `key` of the bundled theme named `theme`.
fn kubach_color(theme: &str, key: &str) -> Hsla {
    let set: serde_json::Value =
        serde_json::from_str(THEMES).expect("bundled Kubach themes must parse");
    let value = set["themes"]
        .as_array()
        .and_then(|themes| themes.iter().find(|t| t["name"] == theme))
        .and_then(|t| t["colors"][key].as_str())
        .unwrap_or_else(|| panic!("{theme} must define {key}"));
    let color =
        Rgba::try_from(value).unwrap_or_else(|err| panic!("{theme} {key} {value:?}: {err}"));
    Hsla::from(color)
}

fn loading_bar_of(theme: &str) -> (Hsla, Hsla) {
    let [bar, track] = LOADING_BAR_KEYS.map(|key| kubach_color(theme, key));
    (bar, track)
}

/// `(bar, track)` colors of the loading bar in the active theme.
pub fn loading_bar_colors(cx: &App) -> (Hsla, Hsla) {
    let theme = cx.theme();
    match cx.try_global::<LoadingBarColors>() {
        Some(colors) if theme.mode.is_dark() => colors.dark,
        Some(colors) => colors.light,
        // `init` has not run: the theme's own progress colors.
        None => (theme.progress_bar, theme.transparent),
    }
}

/// Kubach-only keys in `kubach.json`: the request and limit marks of a
/// [`UsageBar`](crate::UsageBar) (#414).
pub const USAGE_MARKER_KEYS: [&str; 2] = ["usage.request", "usage.limit"];

/// `(request, limit)` marks of Kubach Dark and Kubach Light.
struct UsageMarkers {
    dark: (Hsla, Hsla),
    light: (Hsla, Hsla),
}

impl Global for UsageMarkers {}

fn usage_markers_of(theme: &str) -> (Hsla, Hsla) {
    let [request, limit] = USAGE_MARKER_KEYS.map(|key| kubach_color(theme, key));
    (request, limit)
}

/// `(request, limit)` mark colors of the active theme.
pub fn usage_marker_colors(cx: &App) -> (Hsla, Hsla) {
    let theme = cx.theme();
    match cx.try_global::<UsageMarkers>() {
        Some(markers) if theme.mode.is_dark() => markers.dark,
        Some(markers) => markers.light,
        // `init` has not run: the theme's own tones.
        None => (theme.info, theme.warning),
    }
}

/// Kubach-only keys in `kubach.json`: colors of measured traffic on graph
/// edges (#326), by [`TrafficLevel`](crate::TrafficLevel): low, medium and
/// high load, then degraded and failing (errors).
pub const TRAFFIC_COLOR_KEYS: [&str; 5] = [
    "graph.traffic.low",
    "graph.traffic.medium",
    "graph.traffic.high",
    "graph.traffic.degraded",
    "graph.traffic.failing",
];

/// Traffic colors of Kubach Dark and Kubach Light.
struct TrafficPalettes {
    dark: [Hsla; 5],
    light: [Hsla; 5],
}

impl Global for TrafficPalettes {}

/// Traffic colors of the bundled theme named `theme` ([`DARK`] or
/// [`LIGHT`]), without an app (exports, tests).
pub fn traffic_palette_of(theme: &str) -> [Hsla; 5] {
    TRAFFIC_COLOR_KEYS.map(|key| kubach_color(theme, key))
}

/// Traffic colors of the active theme, in [`TRAFFIC_COLOR_KEYS`] order.
pub fn traffic_palette(cx: &App) -> [Hsla; 5] {
    let theme = cx.theme();
    match cx.try_global::<TrafficPalettes>() {
        Some(palettes) if theme.mode.is_dark() => palettes.dark,
        Some(palettes) => palettes.light,
        // `init` has not run: the theme's own tones.
        None => fallback_traffic_palette(theme),
    }
}

/// Traffic colors from the theme's base tones, where the Kubach keys are
/// not loaded.
pub fn fallback_traffic_palette(theme: &gpui_kit::component::Theme) -> [Hsla; 5] {
    [
        theme.info.opacity(0.5),
        theme.info,
        theme.magenta,
        theme.warning,
        theme.danger,
    ]
}

/// Kubach-only keys in `kubach.json`: the 16 ANSI colors (normal, then
/// bright) that colored program output (logs) is drawn with.
pub const ANSI_COLOR_KEYS: [&str; 16] = [
    "ansi.black",
    "ansi.red",
    "ansi.green",
    "ansi.yellow",
    "ansi.blue",
    "ansi.magenta",
    "ansi.cyan",
    "ansi.white",
    "ansi.bright.black",
    "ansi.bright.red",
    "ansi.bright.green",
    "ansi.bright.yellow",
    "ansi.bright.blue",
    "ansi.bright.magenta",
    "ansi.bright.cyan",
    "ansi.bright.white",
];

/// ANSI palettes of Kubach Dark and Kubach Light.
struct AnsiPalettes {
    dark: [Hsla; 16],
    light: [Hsla; 16],
}

impl Global for AnsiPalettes {}

/// Reads [`ANSI_COLOR_KEYS`] of the bundled theme named `theme`.
fn ansi_palette_of(theme: &str) -> [Hsla; 16] {
    let set: serde_json::Value =
        serde_json::from_str(THEMES).expect("bundled Kubach themes must parse");
    let colors = set["themes"]
        .as_array()
        .and_then(|themes| themes.iter().find(|t| t["name"] == theme))
        .map(|t| &t["colors"])
        .unwrap_or_else(|| panic!("{theme} must be bundled"));
    ANSI_COLOR_KEYS.map(|key| {
        let value = colors[key]
            .as_str()
            .unwrap_or_else(|| panic!("{theme} must define {key}"));
        let color =
            Rgba::try_from(value).unwrap_or_else(|err| panic!("{theme} {key} {value:?}: {err}"));
        Hsla::from(color)
    })
}

/// The 16 ANSI colors of the active theme (normal, then bright).
pub fn ansi_palette(cx: &App) -> [Hsla; 16] {
    let theme = cx.theme();
    match cx.try_global::<AnsiPalettes>() {
        Some(palettes) if theme.mode.is_dark() => palettes.dark,
        Some(palettes) => palettes.light,
        // `init` has not run: the theme's accent colors.
        None => [
            theme.muted_foreground,
            theme.red,
            theme.green,
            theme.yellow,
            theme.blue,
            theme.magenta,
            theme.cyan,
            theme.foreground,
            theme.muted_foreground,
            theme.red_light,
            theme.green_light,
            theme.yellow_light,
            theme.blue_light,
            theme.magenta_light,
            theme.cyan_light,
            theme.foreground,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DARK, LIGHT, ansi_palette_of, loading_bar_of, opaque_background_of, traffic_palette_of,
        usage_markers_of,
    };

    #[test]
    fn both_themes_define_the_usage_markers() {
        for theme in [DARK, LIGHT] {
            let (request, limit) = usage_markers_of(theme);
            assert_eq!((request.a, limit.a), (1., 1.), "{theme}");
            assert_ne!(request, limit, "{theme}");
        }
        assert_ne!(usage_markers_of(DARK), usage_markers_of(LIGHT));
    }

    #[test]
    fn both_themes_define_the_traffic_colors() {
        for theme in [DARK, LIGHT] {
            let palette = traffic_palette_of(theme);
            // Low load is quieter than the rest.
            assert!(palette[0].a < 1., "{theme}");
            assert!(palette[1..].iter().all(|color| color.a == 1.), "{theme}");
        }
        assert_ne!(traffic_palette_of(DARK), traffic_palette_of(LIGHT));
    }

    #[test]
    fn both_themes_define_the_loading_bar() {
        for theme in [DARK, LIGHT] {
            let (bar, track) = loading_bar_of(theme);
            assert_eq!(bar.a, 1., "{theme}");
            assert!(track.a < 0.2, "{theme}: the track stays quiet");
        }
    }

    #[test]
    fn both_themes_define_the_ansi_palette() {
        for theme in [DARK, LIGHT] {
            let palette = ansi_palette_of(theme);
            assert!(palette.iter().all(|color| color.a == 1.), "{theme}");
        }
        assert_ne!(ansi_palette_of(DARK), ansi_palette_of(LIGHT));
    }

    #[test]
    fn opaque_backgrounds_are_opaque() {
        for theme in [DARK, LIGHT] {
            assert_eq!(opaque_background_of(theme).a, 1., "{theme}");
        }
    }

    #[test]
    fn opaque_backgrounds_follow_the_mode() {
        assert!(opaque_background_of(DARK).l < 0.1);
        assert!(opaque_background_of(LIGHT).l > 0.9);
    }
}
