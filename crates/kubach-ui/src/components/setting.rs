//! Rows for settings forms: a label column on the left, the control on the
//! right and a reset-to-default button while the value is modified.
//!
//! All rows are controlled: they render the value they are given and report
//! changes through `on_change`; the owner stores the value and re-renders.

use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_i18n::t;

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type ValueHandler<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;
/// Receives the path chosen in [`pick_path`].
pub type PathHandler = ValueHandler<PathBuf>;

/// Small "reset to default" icon button.
#[derive(IntoElement)]
pub struct ResetButton {
    id: ElementId,
    on_click: Handler,
}

impl ResetButton {
    pub fn new(
        id: impl Into<ElementId>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            on_click: Rc::new(on_click),
        }
    }
}

impl RenderOnce for ResetButton {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let on_click = self.on_click;
        Button::new(self.id)
            .ghost()
            .xsmall()
            .icon(IconName::Undo2)
            .tooltip(t!("ui-setting-reset"))
            .accessibility_label(t!("ui-setting-reset"))
            .on_click(move |_, window, cx| on_click(window, cx))
    }
}

/// Titled group of setting rows separated by dividers.
#[derive(IntoElement)]
pub struct SettingGroup {
    title: Option<SharedString>,
    description: Option<SharedString>,
    children: Vec<AnyElement>,
}

impl SettingGroup {
    pub fn new() -> Self {
        Self {
            title: None,
            description: None,
            children: Vec::new(),
        }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl Default for SettingGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for SettingGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for SettingGroup {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .when(
                self.title.is_some() || self.description.is_some(),
                |group| {
                    group.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .when_some(self.title, |header, title| {
                                header.child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.foreground)
                                        .child(title),
                                )
                            })
                            .when_some(self.description, |header, description| {
                                header.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(description),
                                )
                            }),
                    )
                },
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .bg(theme.group_box)
                    .children(self.children.into_iter().enumerate().map(|(ix, row)| {
                        div()
                            .px_4()
                            .when(ix > 0, |row| row.border_t_1().border_color(border))
                            .child(row)
                    })),
            )
    }
}

/// One setting: title and description, a control, an optional reset button
/// and optional content below (lists, hints).
#[derive(IntoElement)]
pub struct SettingRow {
    id: ElementId,
    title: SharedString,
    description: Option<SharedString>,
    control: Option<AnyElement>,
    below: Option<AnyElement>,
    on_reset: Option<Handler>,
}

impl SettingRow {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
            control: None,
            below: None,
            on_reset: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// Full-width content under the title row.
    pub fn below(mut self, content: impl IntoElement) -> Self {
        self.below = Some(content.into_any_element());
        self
    }

    /// Shows a reset button calling `on_reset` when `modified`.
    pub fn reset(
        mut self,
        modified: bool,
        on_reset: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_reset = modified.then(|| Rc::new(on_reset) as Handler);
        self
    }
}

impl RenderOnce for SettingRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let reset_id = child_id(&self.id, "reset");
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .gap_2()
            .py_3()
            .child(
                // In a narrow window the control wraps under the label
                // instead of overflowing.
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x_4()
                    .gap_y_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(180.))
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.foreground)
                                    .child(self.title),
                            )
                            .when_some(self.description, |label, description| {
                                label.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(description),
                                )
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .max_w_full()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when_some(self.on_reset, |controls, on_reset| {
                                controls.child(ResetButton::new(reset_id, move |window, cx| {
                                    on_reset(window, cx)
                                }))
                            })
                            .children(self.control),
                    ),
            )
            .children(self.below)
    }
}

/// On/off setting with a switch.
#[derive(IntoElement)]
pub struct ToggleRow {
    row: SettingRow,
    checked: bool,
    default: Option<bool>,
    disabled: bool,
    on_change: Option<ValueHandler<bool>>,
}

impl ToggleRow {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            row: SettingRow::new(id, title),
            checked: false,
            default: None,
            disabled: false,
            on_change: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.row = self.row.description(description);
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Enables the reset button while the value differs from `default`.
    pub fn default_value(mut self, default: bool) -> Self {
        self.default = Some(default);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ToggleRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let switch_id = child_id(&self.row.id, "switch");
        let label = self.row.title.clone();
        let mut row = self.row;
        if let (Some(default), Some(handler)) = (self.default, self.on_change.clone()) {
            row = row.reset(
                self.checked != default && !self.disabled,
                move |window, cx| handler(default, window, cx),
            );
        }
        let on_change = self.on_change;
        row.control(
            Switch::new(switch_id)
                .checked(self.checked)
                .disabled(self.disabled)
                .accessibility_label(label)
                .when_some(on_change, |switch, handler| {
                    switch.on_click(move |checked, window, cx| handler(*checked, window, cx))
                }),
        )
    }
}

/// Choice from a list, shown as a dropdown button. Options are addressed by
/// index.
#[derive(IntoElement)]
pub struct SelectRow {
    row: SettingRow,
    options: Vec<SharedString>,
    selected: usize,
    default: Option<usize>,
    disabled: bool,
    on_change: Option<ValueHandler<usize>>,
}

impl SelectRow {
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        options: impl IntoIterator<Item = impl Into<SharedString>>,
    ) -> Self {
        Self {
            row: SettingRow::new(id, title),
            options: options.into_iter().map(Into::into).collect(),
            selected: 0,
            default: None,
            disabled: false,
            on_change: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.row = self.row.description(description);
        self
    }

    pub fn selected(mut self, ix: usize) -> Self {
        self.selected = ix;
        self
    }

    /// Enables the reset button while the selection differs from `default`.
    pub fn default_index(mut self, default: usize) -> Self {
        self.default = Some(default);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SelectRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let button_id = child_id(&self.row.id, "select");
        let label = self.options.get(self.selected).cloned().unwrap_or_default();
        let mut row = self.row;
        if let (Some(default), Some(handler)) = (self.default, self.on_change.clone()) {
            row = row.reset(
                self.selected != default && !self.disabled,
                move |window, cx| handler(default, window, cx),
            );
        }
        let options = self.options;
        let selected = self.selected;
        let on_change = self.on_change;
        let accessible = format!("{}: {label}", row.title);
        row.control(
            Button::new(button_id)
                .outline()
                .small()
                .label(label)
                .accessibility_label(accessible)
                .dropdown_caret(true)
                .disabled(self.disabled)
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    options.iter().enumerate().fold(menu, |menu, (ix, option)| {
                        let handler = on_change.clone();
                        menu.item(
                            PopupMenuItem::new(option.clone())
                                .checked(ix == selected)
                                .on_click(move |_, window, cx| {
                                    if let Some(handler) = &handler {
                                        handler(ix, window, cx);
                                    }
                                }),
                        )
                    })
                }),
        )
    }
}

/// Whole number with a unit, changed with − / + buttons in `step`s.
#[derive(IntoElement)]
pub struct NumberRow {
    row: SettingRow,
    value: u32,
    range: RangeInclusive<u32>,
    step: u32,
    unit: SharedString,
    format: Option<Rc<dyn Fn(u32) -> SharedString>>,
    default: Option<u32>,
    disabled: bool,
    on_change: Option<ValueHandler<u32>>,
}

impl NumberRow {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            row: SettingRow::new(id, title),
            value: 0,
            range: 0..=u32::MAX,
            step: 1,
            unit: SharedString::default(),
            format: None,
            default: None,
            disabled: false,
            on_change: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.row = self.row.description(description);
        self
    }

    pub fn value(mut self, value: u32) -> Self {
        self.value = value;
        self
    }

    pub fn range(mut self, range: RangeInclusive<u32>) -> Self {
        self.range = range;
        self
    }

    pub fn step(mut self, step: u32) -> Self {
        self.step = step.max(1);
        self
    }

    /// Unit shown after the value, e.g. `min`.
    pub fn unit(mut self, unit: impl Into<SharedString>) -> Self {
        self.unit = unit.into();
        self
    }

    /// Custom display of the value (replaces `value unit`).
    pub fn format(mut self, format: impl Fn(u32) -> SharedString + 'static) -> Self {
        self.format = Some(Rc::new(format));
        self
    }

    pub fn default_value(mut self, default: u32) -> Self {
        self.default = Some(default);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(u32, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for NumberRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let id = self.row.id.clone();
        let (min, max) = (*self.range.start(), *self.range.end());
        let value = self.value.clamp(min, max);
        let text: SharedString = match &self.format {
            Some(format) => format(value),
            None if self.unit.is_empty() => value.to_string().into(),
            None => format!("{value} {}", self.unit).into(),
        };
        let mut row = self.row;
        if let (Some(default), Some(handler)) = (self.default, self.on_change.clone()) {
            row = row.reset(value != default && !self.disabled, move |window, cx| {
                handler(default, window, cx)
            });
        }
        let stepper = |name: &'static str, icon: IconName, target: u32, enabled: bool| {
            let handler = self.on_change.clone();
            Button::new(child_id(&id, name))
                .ghost()
                .xsmall()
                .icon(icon)
                .accessibility_label(name)
                .disabled(self.disabled || !enabled)
                .on_click(move |_, window, cx| {
                    if let Some(handler) = &handler {
                        handler(target, window, cx);
                    }
                })
        };
        let down = stepper(
            "decrease",
            IconName::Minus,
            value.saturating_sub(self.step).max(min),
            value > min,
        );
        let up = stepper(
            "increase",
            IconName::Plus,
            value.saturating_add(self.step).min(max),
            value < max,
        );
        row.control(
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .h_7()
                .rounded_md()
                .border_1()
                .border_color(theme.input)
                .child(down)
                .child(
                    div()
                        .min_w(px(64.))
                        .text_center()
                        .text_sm()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(if self.disabled {
                            theme.muted_foreground
                        } else {
                            theme.foreground
                        })
                        .child(text),
                )
                .child(up),
        )
    }
}

/// File or directory path chosen with the system picker. Shows a
/// placeholder (e.g. the auto-detected value) while no path is set; reset
/// clears the path.
#[derive(IntoElement)]
pub struct PathRow {
    row: SettingRow,
    path: Option<SharedString>,
    placeholder: SharedString,
    directories: bool,
    on_pick: Option<PathHandler>,
    on_clear: Option<Handler>,
}

impl PathRow {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            row: SettingRow::new(id, title),
            path: None,
            placeholder: t!("ui-setting-not-set").into(),
            directories: false,
            on_pick: None,
            on_clear: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.row = self.row.description(description);
        self
    }

    pub fn path(mut self, path: Option<impl Into<SharedString>>) -> Self {
        self.path = path.map(Into::into);
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Picks directories instead of files.
    pub fn directories(mut self, directories: bool) -> Self {
        self.directories = directories;
        self
    }

    pub fn on_pick(mut self, handler: impl Fn(PathBuf, &mut Window, &mut App) + 'static) -> Self {
        self.on_pick = Some(Rc::new(handler));
        self
    }

    /// Enables the reset button while a path is set.
    pub fn on_clear(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_clear = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for PathRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let id = self.row.id.clone();
        let mut row = self.row;
        if let Some(on_clear) = self.on_clear {
            row = row.reset(self.path.is_some(), move |window, cx| on_clear(window, cx));
        }
        let (text, color) = match self.path {
            Some(path) => (path, theme.foreground),
            None => (self.placeholder, theme.muted_foreground),
        };
        let on_pick = self.on_pick;
        let directories = self.directories;
        row.control(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .max_w(px(280.))
                        .truncate()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(color)
                        .child(text),
                )
                .child(
                    Button::new(child_id(&id, "browse"))
                        .outline()
                        .small()
                        .icon(IconName::FolderOpen)
                        .label(t!("ui-setting-choose"))
                        .on_click(move |_, window, cx| {
                            let Some(on_pick) = on_pick.clone() else {
                                return;
                            };
                            pick_path(directories, on_pick, window, cx);
                        }),
                ),
        )
    }
}

/// Opens the system file picker and passes the chosen path to `on_pick`.
pub fn pick_path(directories: bool, on_pick: PathHandler, window: &mut Window, cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: !directories,
        directories,
        multiple: false,
        prompt: None,
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                cx.update(|window, cx| on_pick(path, window, cx)).ok();
            }
        })
        .detach();
}

/// Id of a child element derived from its row's id.
fn child_id(parent: &ElementId, name: &'static str) -> ElementId {
    ElementId::NamedChild(std::sync::Arc::new(parent.clone()), name.into())
}
