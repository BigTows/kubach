use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_i18n::t;

/// Items listed before "and N more".
const PREVIEW_ITEMS: usize = 8;

/// Receives the state of the dialog's options, in the order added.
type ConfirmHandler = Rc<dyn Fn(&[bool], &mut Window, &mut App)>;

/// Builds the parts of the dialog that follow its options.
type LiveBuilder = Rc<dyn Fn(&[bool]) -> ConfirmLive>;

/// Parts of a [`ConfirmDialog`] recomputed whenever an option changes
/// ([`ConfirmDialog::live`]): they replace the command, items and notice
/// set on the builder.
#[derive(Clone, Debug, Default)]
pub struct ConfirmLive {
    pub command: Option<SharedString>,
    pub items: Vec<SharedString>,
    /// Highlighted text with its own list.
    pub notice: Option<(SharedString, Vec<SharedString>)>,
    /// Keeps the confirm button disabled (the options do not allow the
    /// action yet).
    pub blocked: bool,
}

/// Splits `items` into the ones to list and how many more there are.
pub fn preview<T>(items: &[T], limit: usize) -> (&[T], usize) {
    let shown = items.len().min(limit);
    (&items[..shown], items.len() - shown)
}

/// Whether `input` is the text the user was asked to type (spaces around it
/// are ignored).
pub fn typed_matches(input: &str, expected: &str) -> bool {
    !expected.is_empty() && input.trim() == expected
}

/// Confirmation for an action on several items: a message, the items (first
/// few, then "and N more") and, for risky cases, a text the user must type
/// (e.g. the number of objects) before the confirm button unlocks. Optional
/// checkboxes tune the action (`--keep-history`, say), and a highlighted
/// notice with its own list warns about what the action leaves behind.
///
/// The command the action runs goes in its own monospace block under the
/// message ([`ConfirmDialog::command`]), with a copy button.
///
/// The cluster context the action runs in goes above the message
/// ([`ConfirmDialog::context`]).
///
/// Open it with [`ConfirmDialog::open`]; it runs `on_confirm` and closes.
/// The returned [`ConfirmHandle`] closes it without confirming, e.g. when
/// the context it was asked in is gone.
pub struct ConfirmDialog {
    title: SharedString,
    /// Cluster context the action runs in.
    context: Option<SharedString>,
    message: Option<SharedString>,
    /// Command the action runs, shown in a monospace block.
    command: Option<SharedString>,
    items: Vec<SharedString>,
    /// `None`: "Confirm" in the current language.
    confirm_label: Option<SharedString>,
    danger: bool,
    /// Text to type and its input.
    typed: Option<(SharedString, Entity<InputState>)>,
    /// Highlighted text and its items, shown after the items.
    notice: Option<(SharedString, Vec<SharedString>)>,
    /// Checkbox labels and states.
    options: Vec<(SharedString, bool)>,
    /// Command, items and notice following the options.
    live: Option<LiveBuilder>,
    on_confirm: Option<ConfirmHandler>,
    _subscriptions: Vec<Subscription>,
}

impl ConfirmDialog {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            context: None,
            message: None,
            command: None,
            items: Vec::new(),
            confirm_label: None,
            danger: false,
            typed: None,
            notice: None,
            options: Vec::new(),
            live: None,
            on_confirm: None,
            _subscriptions: Vec::new(),
        }
    }

    /// The cluster context the action runs in, shown above the message.
    pub fn context(mut self, context: impl Into<SharedString>) -> Self {
        self.context = Some(context.into());
        self
    }

    pub fn message(mut self, message: impl Into<SharedString>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// The command the action runs (secrets already masked), shown under
    /// the message in a monospace block with a copy button.
    pub fn command(mut self, command: impl Into<SharedString>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn items(mut self, items: impl IntoIterator<Item = impl Into<SharedString>>) -> Self {
        self.items = items.into_iter().map(Into::into).collect();
        self
    }

    pub fn confirm_label(mut self, label: impl Into<SharedString>) -> Self {
        self.confirm_label = Some(label.into());
        self
    }

    /// Draws the confirm button as destructive.
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Requires typing `expected` before confirming.
    pub fn require_typed(
        mut self,
        expected: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let expected = expected.into();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(expected.clone()));
        self._subscriptions.push(cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.confirm(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        ));
        self.typed = Some((expected, input));
        self
    }

    /// A highlighted (warning) text with its own list, e.g. what the action
    /// does not remove.
    pub fn notice(
        mut self,
        message: impl Into<SharedString>,
        items: impl IntoIterator<Item = impl Into<SharedString>>,
    ) -> Self {
        self.notice = Some((message.into(), items.into_iter().map(Into::into).collect()));
        self
    }

    /// Adds a checkbox, initially `checked`; its state goes to
    /// [`Self::on_confirm_with_option`] (the first one) or
    /// [`Self::on_confirm_with_options`].
    pub fn option(mut self, label: impl Into<SharedString>, checked: bool) -> Self {
        self.options.push((label.into(), checked));
        self
    }

    /// Recomputes the command, items and notice from the options' state
    /// as they change (e.g. `kubectl drain` flags and the pods they let go).
    pub fn live(mut self, build: impl Fn(&[bool]) -> ConfirmLive + 'static) -> Self {
        self.live = Some(Rc::new(build));
        self
    }

    fn checked(&self) -> Vec<bool> {
        self.options.iter().map(|(_, checked)| *checked).collect()
    }

    pub fn on_confirm(self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_confirm_with_options(move |_, window, cx| handler(window, cx))
    }

    /// Like [`Self::on_confirm`], with whether the first option was checked
    /// (`false` without one).
    pub fn on_confirm_with_option(
        self,
        handler: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_confirm_with_options(move |checked, window, cx| {
            handler(checked.first().copied().unwrap_or(false), window, cx)
        })
    }

    /// Like [`Self::on_confirm`], with the state of every option in the
    /// order they were added.
    pub fn on_confirm_with_options(
        mut self,
        handler: impl Fn(&[bool], &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_confirm = Some(Rc::new(handler));
        self
    }

    /// Shows the dialog built by `build` in `window`.
    pub fn open(
        window: &mut Window,
        cx: &mut App,
        build: impl FnOnce(&mut Window, &mut Context<Self>) -> Self,
    ) -> ConfirmHandle {
        let dialog = cx.new(|cx| build(window, cx));
        let handle = ConfirmHandle {
            dialog: dialog.downgrade(),
            window: window.window_handle(),
        };
        let (title, input) = {
            let this = dialog.read(cx);
            (
                this.title.clone(),
                this.typed.as_ref().map(|(_, i)| i.clone()),
            )
        };
        window.open_dialog(cx, move |base, _, _| {
            let target = dialog.clone();
            // Enter on the dialog confirms through the same checks.
            base.title(title.clone())
                .w(px(460.))
                .on_ok(move |_, window, cx| {
                    target.update(cx, |this, cx| this.confirm(window, cx));
                    false
                })
                .child(dialog.clone())
        });
        if let Some(input) = input {
            input.focus_handle(cx).focus(window, cx);
        }
        handle
    }

    fn can_confirm(&self, cx: &App) -> bool {
        if let Some(live) = &self.live
            && live(&self.checked()).blocked
        {
            return false;
        }
        match &self.typed {
            Some((expected, input)) => typed_matches(&input.read(cx).value(), expected),
            None => true,
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_confirm(cx) {
            return;
        }
        let checked = self.checked();
        if let Some(handler) = self.on_confirm.take() {
            handler(&checked, window, cx);
        }
        window.close_dialog(cx);
    }
}

/// An open [`ConfirmDialog`], to close it from outside.
#[derive(Clone)]
pub struct ConfirmHandle {
    dialog: WeakEntity<ConfirmDialog>,
    window: AnyWindowHandle,
}

impl ConfirmHandle {
    /// Whether the dialog is still shown (neither confirmed nor cancelled).
    pub fn is_open(&self) -> bool {
        self.dialog.upgrade().is_some()
    }

    /// Closes the dialog without confirming, once the current update is
    /// over; nothing happens if it is already closed.
    pub fn dismiss(&self, cx: &mut App) {
        let (dialog, window) = (self.dialog.clone(), self.window);
        if dialog.update(cx, |this, _| this.on_confirm = None).is_err() {
            return;
        }
        cx.defer(move |cx| {
            // Shown dialogs are owned by the window's dialog stack: alive
            // means still open, and a confirm dialog is modal, so on top.
            if dialog.upgrade().is_none() {
                return;
            }
            window
                .update(cx, |_, window, cx| window.close_dialog(cx))
                .ok();
        });
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(live) = self.live.clone() {
            let parts = live(&self.checked());
            self.command = parts.command;
            self.items = parts.items;
            self.notice = parts.notice;
        }
        let theme = cx.theme();
        let item_list = |items: &[SharedString]| {
            let (shown, more) = preview(items, PREVIEW_ITEMS);
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.foreground)
                .children(
                    shown
                        .iter()
                        .map(|item| div().min_w_0().truncate().child(item.clone())),
                )
                .when(more > 0, |list| {
                    list.child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(t!("ui-and-more", count = more)),
                    )
                })
        };
        let notice = self.notice.as_ref().map(|(message, items)| {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.warning)
                        .whitespace_normal()
                        .child(message.clone()),
                )
                .when(!items.is_empty(), |notice| notice.child(item_list(items)))
        });
        let command = self.command.clone().map(|command| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t!("ui-confirm-command"))
                        .child(
                            Clipboard::new("confirm-dialog-command-copy")
                                .value(command.clone())
                                .tooltip(t!("ui-confirm-copy-command")),
                        ),
                )
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .whitespace_normal()
                        .child(command),
                )
        });
        let list = (!self.items.is_empty()).then(|| item_list(&self.items));
        let typed = self.typed.as_ref().map(|(expected, input)| {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t!(
                    "ui-confirm-type-to-confirm",
                    text = expected.to_string()
                ))
                .child(Input::new(input).small())
        });
        let options = self
            .options
            .iter()
            .enumerate()
            .map(|(ix, (label, checked))| {
                Checkbox::new(("confirm-dialog-option", ix))
                    .label(label.clone())
                    .checked(*checked)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        if let Some((_, state)) = this.options.get_mut(ix) {
                            *state = *checked;
                            cx.notify();
                        }
                    }))
            });
        let can_confirm = self.can_confirm(cx);
        let confirm = Button::new("confirm-dialog-ok")
            .small()
            .label(
                self.confirm_label
                    .clone()
                    .unwrap_or_else(|| t!("ui-confirm").into()),
            )
            .disabled(!can_confirm)
            .map(|button| {
                if self.danger {
                    button.danger()
                } else {
                    button.primary()
                }
            })
            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx)));
        let context = self.context.clone().map(|context| {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t!("ui-confirm-context"))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .child(context),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(context)
            .when_some(self.message.clone(), |this, message| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(theme.foreground)
                        .whitespace_normal()
                        .child(message),
                )
            })
            .children(command)
            .children(list)
            .children(notice)
            .children(options)
            .children(typed)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("confirm-dialog-cancel")
                            .small()
                            .ghost()
                            .label(t!("ui-cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(confirm),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{preview, typed_matches};

    #[test]
    fn preview_lists_first_items_and_counts_the_rest() {
        let items: Vec<u32> = (0..12).collect();
        let (shown, more) = preview(&items, 8);
        assert_eq!(shown.len(), 8);
        assert_eq!(more, 4);
        let (shown, more) = preview(&items[..3], 8);
        assert_eq!(shown, &[0, 1, 2]);
        assert_eq!(more, 0);
    }

    #[test]
    fn typed_text_must_match() {
        assert!(typed_matches("12", "12"));
        assert!(typed_matches(" 12 ", "12"));
        assert!(!typed_matches("", "12"));
        assert!(!typed_matches("11", "12"));
        assert!(!typed_matches("12a", "12"));
        assert!(!typed_matches("", ""));
    }
}
