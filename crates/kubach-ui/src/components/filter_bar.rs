//! Filter bar above a list: a filter on the left (usually a namespace
//! [`MultiSelect`](crate::MultiSelect)), a search by name after it and
//! actions right after the search, within reach of it.
//!
//! ```text
//! [ web, api +2 ▾ ] [ 🔍 Search by name…   ✕ ] [ + New ]
//! ```

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Icon, IconName, Sizable as _};
use gpui_kit::*;

use crate::EmptyState;
use kubach_i18n::t;

/// Width of the search field.
pub const SEARCH_WIDTH: Pixels = px(280.);

/// Whether `name` matches the search `query`: a substring, ignoring case
/// and surrounding spaces. An empty query matches everything.
pub fn name_matches(name: &str, query: &str) -> bool {
    let query = query.trim();
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

/// Row of a filter, a search field and actions. The search state comes
/// from [`FilterBar::search_state`], so `Esc` clears it.
#[derive(IntoElement)]
pub struct FilterBar {
    filter: Option<AnyElement>,
    search: Option<Entity<InputState>>,
    actions: Vec<AnyElement>,
}

impl Default for FilterBar {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterBar {
    pub fn new() -> Self {
        Self {
            filter: None,
            search: None,
            actions: Vec::new(),
        }
    }

    /// A search field for [`search`](Self::search): `placeholder` shown
    /// while empty, cleared by `Esc`.
    pub fn search_state(
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<InputState> {
        let placeholder = placeholder.into();
        cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .clean_on_escape()
        })
    }

    /// Left slot, e.g. the namespace picker.
    pub fn filter(mut self, filter: impl IntoElement) -> Self {
        self.filter = Some(filter.into_any_element());
        self
    }

    /// The search field after the filter.
    pub fn search(mut self, search: &Entity<InputState>) -> Self {
        self.search = Some(search.clone());
        self
    }

    /// Adds an action on the right, after the ones added before.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub fn actions(mut self, actions: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.actions
            .extend(actions.into_iter().map(IntoElement::into_any_element));
        self
    }

    /// Placeholder of a list that has objects, none of which match the
    /// search: `No Pods match "foo"` with a button that clears it.
    pub fn no_matches(
        noun: &str,
        query: &str,
        on_clear: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> EmptyState {
        EmptyState::new(
            IconName::Search,
            t!(
                "ui-filter-no-matches",
                noun = noun.to_owned(),
                query = query.trim().to_owned()
            ),
        )
        .description(t!("ui-filter-no-matches-description"))
        .action(
            Button::new("filter-bar-clear-search")
                .outline()
                .small()
                .label(t!("ui-filter-clear-search"))
                .on_click(on_clear),
        )
    }
}

impl RenderOnce for FilterBar {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .children(self.filter)
            .children(self.search.map(|search| {
                div().w(SEARCH_WIDTH).min_w(px(160.)).flex_shrink(1.).child(
                    Input::new(&search)
                        .small()
                        .prefix(Icon::new(IconName::Search).size_4())
                        .cleanable(true),
                )
            }))
            .children(self.actions)
    }
}

#[cfg(test)]
mod tests {
    use super::name_matches;

    #[test]
    fn matches_substrings_ignoring_case() {
        assert!(name_matches("api-server-7f9", ""));
        assert!(name_matches("api-server-7f9", "   "));
        assert!(name_matches("api-server-7f9", "SERVER"));
        assert!(name_matches("api-server-7f9", " 7f "));
        assert!(!name_matches("api-server-7f9", "web"));
        assert!(name_matches("Ünïcode", "üNÏ"));
    }
}
