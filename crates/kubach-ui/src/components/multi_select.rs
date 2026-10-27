use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_i18n::t;

use crate::TruncatedText;

/// Items named in a trigger label before the rest is summarized as `+N`.
const LABEL_ITEMS: usize = 2;
const LIST_WIDTH: Pixels = px(260.);
const LIST_MAX_HEIGHT: Pixels = px(320.);
/// Picks this close together apply at once (each change restarts watches).
const APPLY_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

/// A choice of a [`MultiSelect`]: everything, or specific items in the order
/// they were picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultiSelection {
    All,
    Items(Vec<SharedString>),
}

impl Default for MultiSelection {
    fn default() -> Self {
        Self::Items(Vec::new())
    }
}

impl MultiSelection {
    pub fn items(&self) -> &[SharedString] {
        match self {
            Self::All => &[],
            Self::Items(items) => items,
        }
    }

    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }

    /// Whether `item` is picked individually (never under `All`).
    pub fn contains(&self, item: &str) -> bool {
        self.items().iter().any(|picked| picked == item)
    }

    /// Checks or unchecks `item`; checking one leaves `All`.
    pub fn toggle(&mut self, item: &SharedString) {
        match self {
            Self::All => *self = Self::Items(vec![item.clone()]),
            Self::Items(items) => match items.iter().position(|picked| picked == item) {
                Some(ix) => {
                    items.remove(ix);
                }
                None => items.push(item.clone()),
            },
        }
    }

    /// Checks or unchecks `All`, which excludes individual items.
    pub fn toggle_all(&mut self) {
        *self = match self {
            Self::All => Self::Items(Vec::new()),
            Self::Items(_) => Self::All,
        };
    }

    /// Picks `item` alone (Alt-click).
    pub fn only(&mut self, item: &SharedString) {
        *self = Self::Items(vec![item.clone()]);
    }

    /// Short summary: `web`, `web, api`, `web, api +2`, or `all_label`.
    pub fn label(&self, all_label: &str) -> SharedString {
        match self {
            Self::All => all_label.to_owned().into(),
            Self::Items(items) if items.is_empty() => t!("ui-multi-none").into(),
            Self::Items(items) => {
                let shown = items[..items.len().min(LABEL_ITEMS)].join(", ");
                match items.len().saturating_sub(LABEL_ITEMS) {
                    0 => shown.into(),
                    rest => format!("{shown} +{rest}").into(),
                }
            }
        }
    }
}

/// Emitted by [`MultiSelect`] when the user applies a different selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultiSelectEvent {
    Change(MultiSelection),
}

/// Dropdown with a search field, an "all" option, recent items and
/// checkboxes. Toggling keeps it open; Enter or a click outside applies.
/// Alt-click picks one item alone and applies at once. With custom input
/// allowed, a query that matches nothing can be added as an item.
pub struct MultiSelect {
    id: ElementId,
    items: Vec<SharedString>,
    recent: Vec<SharedString>,
    /// `None`: "All" in the current language.
    all_label: Option<SharedString>,
    note: Option<SharedString>,
    label_override: Option<SharedString>,
    allow_custom: bool,
    disabled: bool,
    selection: MultiSelection,
    draft: MultiSelection,
    open: bool,
    query: Entity<InputState>,
    /// Language of the query placeholder; it follows a switch on render.
    query_locale: kubach_i18n::Locale,
    /// Applies the draft shortly after the last pick while the list is open.
    _apply: Option<Task<()>>,
    /// Recent items that arrived while the list was open.
    pending_recent: Option<Vec<SharedString>>,
    _subscription: Subscription,
}

impl EventEmitter<MultiSelectEvent> for MultiSelect {}

impl MultiSelect {
    pub fn new(id: impl Into<ElementId>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(t!("ui-multi-filter")));
        let subscription =
            cx.subscribe_in(&query, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.on_enter(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            });
        Self {
            id: id.into(),
            items: Vec::new(),
            recent: Vec::new(),
            all_label: None,
            query_locale: kubach_i18n::locale(),
            note: None,
            label_override: None,
            allow_custom: false,
            disabled: false,
            selection: MultiSelection::All,
            draft: MultiSelection::All,
            open: false,
            query,
            _apply: None,
            pending_recent: None,
            _subscription: subscription,
        }
    }

    /// Label of the "everything" option, e.g. `All namespaces`.
    pub fn all_label(mut self, label: impl Into<SharedString>) -> Self {
        self.all_label = Some(label.into());
        self
    }

    fn all_text(&self) -> SharedString {
        self.all_label
            .clone()
            .unwrap_or_else(|| t!("ui-multi-all").into())
    }

    /// Lets the user add a typed value that is not in the list.
    pub fn allow_custom(mut self, allow: bool) -> Self {
        self.allow_custom = allow;
        self
    }

    /// Items shown (sorted alphabetically in the list).
    pub fn set_items(&mut self, mut items: Vec<SharedString>, cx: &mut Context<Self>) {
        items.sort();
        items.dedup();
        self.items = items;
        cx.notify();
    }

    /// Items shown first, most recent first. While the list is open they
    /// change when it closes, so rows don't move under the pointer.
    pub fn set_recent(&mut self, recent: Vec<SharedString>, cx: &mut Context<Self>) {
        if self.open {
            self.pending_recent = Some(recent);
            return;
        }
        self.recent = recent;
        cx.notify();
    }

    /// Hint shown above the list, e.g. why items are missing.
    pub fn set_note(&mut self, note: Option<SharedString>, cx: &mut Context<Self>) {
        self.note = note;
        cx.notify();
    }

    /// Replaces the trigger label (e.g. when the choice does not apply).
    pub fn set_label_override(&mut self, label: Option<SharedString>, cx: &mut Context<Self>) {
        self.label_override = label;
        cx.notify();
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        if disabled {
            self.open = false;
        }
        cx.notify();
    }

    /// Sets the applied selection without emitting an event.
    pub fn set_selection(&mut self, selection: MultiSelection, cx: &mut Context<Self>) {
        self.selection = selection.clone();
        if !self.open {
            self.draft = selection;
        }
        cx.notify();
    }

    pub fn selection(&self) -> &MultiSelection {
        &self.selection
    }

    /// Opens the list with the focus in its search (e.g. from a shortcut).
    /// Does nothing while disabled.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        self.set_open(true, window, cx);
        self.query.update(cx, |query, cx| query.focus(window, cx));
    }

    fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.open == open {
            return;
        }
        self.open = open;
        if open {
            self.draft = self.selection.clone();
            self.query
                .update(cx, |query, cx| query.set_value("", window, cx));
        } else {
            self.apply(cx);
            if let Some(recent) = self.pending_recent.take() {
                self.recent = recent;
            }
        }
        cx.notify();
    }

    /// The draft changed while the list is open: the choice applies as it
    /// is made, a quick run of clicks at once.
    fn draft_changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        self._apply = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(APPLY_DELAY).await;
            this.update(cx, |this, cx| this.apply(cx)).ok();
        }));
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        self._apply = None;
        if self.draft != self.selection {
            self.selection = self.draft.clone();
            cx.emit(MultiSelectEvent::Change(self.selection.clone()));
        }
    }

    fn query_text(&self, cx: &App) -> String {
        self.query.read(cx).value().trim().to_owned()
    }

    /// The typed value when it can be added as a new item.
    fn custom_candidate(&self, cx: &App) -> Option<SharedString> {
        let query = self.query_text(cx);
        (self.allow_custom
            && !query.is_empty()
            && !self.items.iter().any(|item| item.as_ref() == query)
            && !self.draft.contains(&query))
        .then(|| query.into())
    }

    fn on_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(custom) = self.custom_candidate(cx) {
            self.draft.toggle(&custom);
            self.query
                .update(cx, |query, cx| query.set_value("", window, cx));
            self.draft_changed(cx);
            return;
        }
        self.set_open(false, window, cx);
    }

    fn on_item_click(
        &mut self,
        item: &SharedString,
        alt: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if alt {
            self.draft.only(item);
            self.set_open(false, window, cx);
        } else {
            self.draft.toggle(item);
            self.draft_changed(cx);
        }
    }

    fn render_list(&self, this: WeakEntity<Self>, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let query = self.query_text(cx).to_lowercase();
        let matches =
            |item: &SharedString| query.is_empty() || item.to_lowercase().contains(&query);

        // Items picked by hand but not listed (typed in, or gone) stay visible.
        let extra: Vec<SharedString> = self
            .draft
            .items()
            .iter()
            .filter(|item| !self.items.contains(item))
            .cloned()
            .collect();
        let recent: Vec<SharedString> = self
            .recent
            .iter()
            .filter(|item| matches(item))
            .cloned()
            .collect();
        let mut rest: Vec<SharedString> = self
            .items
            .iter()
            .chain(&extra)
            .filter(|item| matches(item) && !recent.contains(item))
            .cloned()
            .collect();
        rest.sort();
        rest.dedup();
        let custom = self.custom_candidate(cx);

        let heading = |title: String| {
            div()
                .px_2()
                .pt_2()
                .pb_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(title)
        };
        let row = |id: ElementId, label: SharedString, checked: bool| {
            let check = div()
                .size_4()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .border_1()
                .map(|check| {
                    if checked {
                        check.border_color(theme.primary).bg(theme.primary).child(
                            Icon::new(IconName::Check)
                                .size_3()
                                .text_color(theme.primary_foreground),
                        )
                    } else {
                        check.border_color(theme.input)
                    }
                });
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .h(px(28.))
                .px_2()
                .rounded_md()
                .cursor_pointer()
                .text_sm()
                .text_color(theme.popover_foreground)
                .hover(|row| row.bg(theme.accent))
                .child(check)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(TruncatedText::new("label", label)),
                )
        };
        let item_row = |section: &'static str, item: SharedString| {
            let this = this.clone();
            let clicked = item.clone();
            row(
                ElementId::Name(format!("{section}-{item}").into()),
                item.clone(),
                self.draft.contains(&item),
            )
            .on_click(move |event, window, cx| {
                let alt = event.modifiers().alt;
                this.update(cx, |this, cx| this.on_item_click(&clicked, alt, window, cx))
                    .ok();
            })
        };

        let mut list = div()
            .id("multi-select-items")
            .max_h(LIST_MAX_HEIGHT)
            .overflow_y_scroll()
            .flex()
            .flex_col();
        if query.is_empty() {
            let this = this.clone();
            list = list.child(
                row(
                    "multi-select-all".into(),
                    self.all_text(),
                    self.draft.is_all(),
                )
                .on_click(move |_, _, cx| {
                    this.update(cx, |this, cx| {
                        this.draft.toggle_all();
                        this.draft_changed(cx);
                    })
                    .ok();
                }),
            );
        }
        if !recent.is_empty() {
            list = list
                .child(heading(t!("ui-multi-recent")))
                .children(recent.into_iter().map(|item| item_row("recent", item)));
        }
        if !rest.is_empty() {
            list = list
                .child(heading(t!("ui-multi-all")))
                .children(rest.into_iter().map(|item| item_row("item", item)));
        }
        if let Some(custom) = custom {
            let this = this.clone();
            let added = custom.clone();
            list = list.child(
                div()
                    .id("multi-select-custom")
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(28.))
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .hover(|row| row.bg(theme.accent))
                    .child(Icon::new(IconName::Plus).size_4())
                    .child(
                        div()
                            .truncate()
                            .child(t!("ui-multi-use", value = custom.to_string())),
                    )
                    .on_click(move |_, window, cx| {
                        this.update(cx, |this, cx| {
                            this.draft.toggle(&added);
                            this.query
                                .update(cx, |query, cx| query.set_value("", window, cx));
                            this.draft_changed(cx);
                        })
                        .ok();
                    }),
            );
        } else if !query.is_empty() && list_is_empty(self, &query) {
            list = list.child(
                div()
                    .px_2()
                    .py_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(t!("ui-multi-no-matches")),
            );
        }

        div()
            .w(LIST_WIDTH)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                Input::new(&self.query)
                    .small()
                    .prefix(Icon::new(IconName::Search).size_4())
                    .cleanable(true),
            )
            .when_some(self.note.clone(), |panel, note| {
                panel.child(
                    div()
                        .px_2()
                        .pt_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(note),
                )
            })
            .child(list)
            .child(
                div()
                    .px_2()
                    .pt_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("ui-multi-hint")),
            )
            .into_any_element()
    }
}

/// Whether the filtered list shows no item rows.
fn list_is_empty(select: &MultiSelect, query: &str) -> bool {
    !select
        .items
        .iter()
        .chain(&select.recent)
        .chain(select.draft.items())
        .any(|item| item.to_lowercase().contains(query))
}

impl Render for MultiSelect {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = kubach_i18n::locale();
        if self.query_locale != locale {
            self.query_locale = locale;
            self.query.update(cx, |query, cx| {
                query.set_placeholder(t!("ui-multi-filter"), window, cx)
            });
        }
        let label = self
            .label_override
            .clone()
            .unwrap_or_else(|| match &self.selection {
                MultiSelection::Items(items) if items.is_empty() => t!("ui-multi-select").into(),
                selection => selection.label(&self.all_text()),
            });
        let this = cx.entity().downgrade();
        let on_change = this.clone();
        let focus = self.query.read(cx).focus_handle(cx);

        Popover::new(self.id.clone())
            .open(self.open)
            .track_focus(&focus)
            .p_1()
            .on_open_change(move |open, window, cx| {
                on_change
                    .update(cx, |this, cx| this.set_open(*open, window, cx))
                    .ok();
            })
            .trigger(
                Button::new("multi-select-trigger")
                    .outline()
                    .small()
                    .icon(IconName::Folder)
                    .label(label)
                    .dropdown_caret(true)
                    .disabled(self.disabled),
            )
            .content(move |_, _, cx| {
                let Some(entity) = this.upgrade() else {
                    return div().into_any_element();
                };
                let cx: &App = cx;
                entity.read(cx).render_list(this.clone(), cx)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::MultiSelection;

    fn items(names: &[&str]) -> MultiSelection {
        MultiSelection::Items(names.iter().map(|&n| n.into()).collect())
    }

    #[test]
    fn labels_name_two_items_then_count() {
        assert_eq!(
            MultiSelection::All.label("All namespaces"),
            "All namespaces"
        );
        assert_eq!(items(&["web"]).label("All"), "web");
        assert_eq!(items(&["web", "api"]).label("All"), "web, api");
        assert_eq!(
            items(&["web", "api", "worker", "queue"]).label("All"),
            "web, api +2"
        );
    }

    #[test]
    fn all_excludes_individual_items() {
        let mut selection = MultiSelection::All;
        assert!(!selection.contains("web"));
        selection.toggle(&"web".into());
        assert_eq!(selection, items(&["web"]));
        selection.toggle(&"api".into());
        assert_eq!(selection, items(&["web", "api"]));
        selection.toggle_all();
        assert_eq!(selection, MultiSelection::All);
        // Unchecking All leaves nothing picked; the caller decides the fallback.
        selection.toggle_all();
        assert_eq!(selection, items(&[]));
    }

    #[test]
    fn toggling_twice_removes_and_only_picks_one() {
        let mut selection = items(&["web", "api"]);
        selection.toggle(&"web".into());
        assert_eq!(selection, items(&["api"]));
        selection.only(&"queue".into());
        assert_eq!(selection, items(&["queue"]));
        let mut all = MultiSelection::All;
        all.only(&"web".into());
        assert_eq!(all, items(&["web"]));
    }
}
