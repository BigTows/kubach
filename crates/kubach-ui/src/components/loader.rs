//! The loading indicator of the app: a circle turning at a steady pace,
//! without the ease-in-out pauses of the stock spinner.

use gpui_kit::component::button::Button;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;

/// Linear progress of one turn: a constant angular speed, so repeated turns
/// join without slowing down.
pub fn linear(t: f32) -> f32 {
    t
}

/// A [`Spinner`] turning linearly and endlessly, `LoaderCircle` by default.
/// Size, color and icon are set on the returned spinner as usual.
pub fn spinner() -> Spinner {
    Spinner::new()
        .icon(Icon::new(IconName::LoaderCircle))
        .ease(linear)
}

/// Busy state of a button with the app's [`spinner`] in place of its icon.
pub trait ButtonLoading {
    /// Like `Button::loading`, but turns the app's linear spinner.
    fn spinning(self, loading: bool) -> Self;
}

impl ButtonLoading for Button {
    fn spinning(self, loading: bool) -> Self {
        self.loading(loading)
            .when(loading, |button| button.icon(spinner()))
    }
}

#[cfg(test)]
mod tests {
    use super::linear;

    #[test]
    fn turns_at_a_constant_pace() {
        for step in 0..=10 {
            let t = step as f32 / 10.;
            assert_eq!(linear(t), t);
        }
    }
}
