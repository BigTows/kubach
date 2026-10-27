//! ANSI colors mapped onto the active theme.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui_kit::component::Theme;
use gpui_kit::{Hsla, Rgba};

/// Colors a terminal draws with. The 16 ANSI colors come from the theme's
/// accent palette so programs look at home in Kubach Dark and Light; the
/// 256-color cube and true colors are used as the program sends them.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalPalette {
    pub foreground: Hsla,
    pub dim_foreground: Hsla,
    /// Used where the default background must be drawn as a color
    /// (inverse video, text over the cursor).
    pub background: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
    pub ansi: [Hsla; 16],
}

impl TerminalPalette {
    pub fn from_theme(theme: &Theme) -> Self {
        let opaque = |color: Hsla| Hsla { a: 1.0, ..color };
        let background = opaque(theme.background);
        let foreground = theme.foreground;
        let muted = theme.muted_foreground;
        let (black, white, bright_black, bright_white) = if theme.mode.is_dark() {
            (background, muted, muted.opacity(0.7), foreground)
        } else {
            (foreground, muted.opacity(0.7), muted, background)
        };
        Self {
            foreground,
            dim_foreground: muted,
            background,
            cursor: theme.caret,
            selection: theme.selection,
            ansi: [
                black,
                theme.red,
                theme.green,
                theme.yellow,
                theme.blue,
                theme.magenta,
                theme.cyan,
                white,
                bright_black,
                theme.red_light,
                theme.green_light,
                theme.yellow_light,
                theme.blue_light,
                theme.magenta_light,
                theme.cyan_light,
                bright_white,
            ],
        }
    }

    /// Foreground color of a cell. `overrides` holds colors a program set
    /// with OSC 4/10/11.
    pub fn foreground(&self, color: Color, overrides: &Colors) -> Hsla {
        self.resolve(color, overrides).unwrap_or(self.background)
    }

    /// Background color of a cell; `None` for the default background, which
    /// is left transparent.
    pub fn background(&self, color: Color, overrides: &Colors) -> Option<Hsla> {
        match color {
            Color::Named(NamedColor::Background) => overrides[NamedColor::Background].map(rgb),
            color => self.resolve(color, overrides),
        }
    }

    fn resolve(&self, color: Color, overrides: &Colors) -> Option<Hsla> {
        match color {
            Color::Spec(spec) => Some(rgb(spec)),
            Color::Indexed(index) => Some(
                overrides[index as usize]
                    .map(rgb)
                    .unwrap_or_else(|| self.indexed(index)),
            ),
            Color::Named(named) => {
                if let Some(color) = overrides[named] {
                    return Some(rgb(color));
                }
                Some(match named {
                    NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
                    NamedColor::DimForeground => self.dim_foreground,
                    NamedColor::Background => return None,
                    NamedColor::Cursor => self.cursor,
                    named => {
                        let index = named as usize;
                        if index < 16 {
                            self.ansi[index]
                        } else {
                            // Dim variants follow `DimBlack` in ANSI order.
                            let base = index - NamedColor::DimBlack as usize;
                            self.ansi[base.min(7)].opacity(0.7)
                        }
                    }
                })
            }
        }
    }

    /// Color `index` of the 256-color palette.
    pub fn indexed(&self, index: u8) -> Hsla {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let index = index - 16;
                let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
                rgb(Rgb {
                    r: level(index / 36),
                    g: level((index / 6) % 6),
                    b: level(index % 6),
                })
            }
            232..=255 => {
                let gray = 8 + (index - 232) * 10;
                rgb(Rgb {
                    r: gray,
                    g: gray,
                    b: gray,
                })
            }
        }
    }

    /// Bold text in one of the 8 base colors is drawn in its bright variant.
    pub fn brighten(color: Color) -> Color {
        match color {
            Color::Named(named) if (named as usize) < 8 => Color::Indexed(named as u8 + 8),
            Color::Indexed(index) if index < 8 => Color::Indexed(index + 8),
            color => color,
        }
    }

    /// The color as RGB, for answering OSC color queries.
    pub fn to_rgb(color: Hsla) -> Rgb {
        let rgba = Rgba::from(color);
        let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgb {
            r: channel(rgba.r),
            g: channel(rgba.g),
            b: channel(rgba.b),
        }
    }
}

fn rgb(color: Rgb) -> Hsla {
    Rgba {
        r: color.r as f32 / 255.0,
        g: color.g as f32 / 255.0,
        b: color.b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use gpui_kit::hsla;

    use super::*;

    fn palette() -> TerminalPalette {
        let ansi = std::array::from_fn(|ix| hsla(ix as f32 / 16.0, 0.5, 0.5, 1.0));
        TerminalPalette {
            foreground: hsla(0.0, 0.0, 0.9, 1.0),
            dim_foreground: hsla(0.0, 0.0, 0.6, 1.0),
            background: hsla(0.0, 0.0, 0.05, 1.0),
            cursor: hsla(0.0, 0.0, 1.0, 1.0),
            selection: hsla(0.6, 0.5, 0.5, 0.4),
            ansi,
        }
    }

    #[test]
    fn named_colors_come_from_the_palette() {
        let palette = palette();
        let none = Colors::default();
        assert_eq!(
            palette.foreground(Color::Named(NamedColor::Red), &none),
            palette.ansi[1]
        );
        assert_eq!(
            palette.foreground(Color::Named(NamedColor::BrightCyan), &none),
            palette.ansi[14]
        );
        assert_eq!(
            palette.foreground(Color::Named(NamedColor::Foreground), &none),
            palette.foreground
        );
        assert_eq!(
            palette.foreground(Color::Named(NamedColor::DimGreen), &none),
            palette.ansi[2].opacity(0.7)
        );
        assert_eq!(
            palette.background(Color::Named(NamedColor::Background), &none),
            None
        );
        assert_eq!(
            palette.background(Color::Indexed(4), &none),
            Some(palette.ansi[4])
        );
    }

    #[test]
    fn cube_and_grayscale() {
        let palette = palette();
        assert_eq!(
            TerminalPalette::to_rgb(palette.indexed(16)),
            Rgb { r: 0, g: 0, b: 0 }
        );
        assert_eq!(
            TerminalPalette::to_rgb(palette.indexed(231)),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        // 16 + 36·1 + 6·2 + 3 → (95, 135, 175)
        assert_eq!(
            TerminalPalette::to_rgb(palette.indexed(67)),
            Rgb {
                r: 95,
                g: 135,
                b: 175
            }
        );
        assert_eq!(
            TerminalPalette::to_rgb(palette.indexed(232)),
            Rgb { r: 8, g: 8, b: 8 }
        );
        assert_eq!(
            TerminalPalette::to_rgb(palette.indexed(255)),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
    }

    #[test]
    fn true_color_and_overrides() {
        let palette = palette();
        let spec = Rgb {
            r: 10,
            g: 20,
            b: 30,
        };
        let none = Colors::default();
        assert_eq!(
            TerminalPalette::to_rgb(palette.foreground(Color::Spec(spec), &none)),
            spec
        );
        let mut overrides = Colors::default();
        overrides[NamedColor::Red] = Some(spec);
        assert_eq!(
            TerminalPalette::to_rgb(palette.foreground(Color::Named(NamedColor::Red), &overrides)),
            spec
        );
    }

    #[test]
    fn bold_brightens_base_colors() {
        assert_eq!(
            TerminalPalette::brighten(Color::Named(NamedColor::Blue)),
            Color::Indexed(12)
        );
        assert_eq!(
            TerminalPalette::brighten(Color::Indexed(9)),
            Color::Indexed(9)
        );
        assert_eq!(
            TerminalPalette::brighten(Color::Named(NamedColor::Foreground)),
            Color::Named(NamedColor::Foreground)
        );
    }
}
