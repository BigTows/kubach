//! "New namespace": a dialog with the name (checked as a DNS-1123 label
//! while typing) and optional labels, opened by the "+" of the Namespaces
//! list.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _,
};
use gpui_kit::*;
use kubach_core::namespace;
use kubach_core::runtime;
use kubach_i18n::t;

use crate::action_context;
use crate::cluster::Cluster;

/// The "+" button of the Namespaces list.
pub fn new_namespace_button(cluster: &Entity<Cluster>, cx: &App) -> impl IntoElement + use<> {
    let connected = cluster.read(cx).connection().is_some();
    let cluster = cluster.clone();
    Button::new("new-namespace")
        .outline()
        .small()
        .icon(IconName::Plus)
        .tooltip(t!("editor-namespace-new"))
        .disabled(!connected)
        .on_click(move |_, window, cx| open(cluster.clone(), window, cx))
}

/// Opens the dialog.
pub fn open(cluster: Entity<Cluster>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| NamespaceForm::new(cluster, window, cx));
    let focus = form.read(cx).name.clone();
    focus.update(cx, |input, cx| input.focus(window, cx));
    let title: SharedString = match &form.read(cx).connection {
        Some(connection) => t!("editor-namespace-new-in", context = &connection.context).into(),
        None => t!("editor-namespace-new").into(),
    };
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title(title.clone()).w(px(440.)).child(form.clone())
    });
}

/// Readable reason for a failed create.
fn describe(error: &kube::Error) -> String {
    match error {
        kube::Error::Api(status) if status.is_forbidden() => {
            t!("editor-namespace-forbidden", message = &status.message)
        }
        kube::Error::Api(status) if !status.message.is_empty() => status.message.clone(),
        other => other.to_string(),
    }
}

/// What is wrong with the typed name, once something is typed.
fn name_error(name: &str) -> Option<String> {
    (!name.is_empty())
        .then(|| namespace::validate_name(name).err())
        .flatten()
}

pub struct NamespaceForm {
    cluster: Entity<Cluster>,
    /// Where the dialog was opened: the namespace is created there, and
    /// only while it is still the active connection (#344).
    connection: Option<kubach_core::Connection>,
    name: Entity<InputState>,
    labels: Entity<InputState>,
    /// Why the last create failed or was refused before sending.
    error: Option<String>,
    creating: bool,
    _subscriptions: Vec<Subscription>,
    _create: Option<Task<()>>,
}

impl NamespaceForm {
    fn new(cluster: Entity<Cluster>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        let labels = cx.new(|cx| InputState::new(window, cx));
        let on_input = |this: &mut Self,
                        event: &InputEvent,
                        window: &mut Window,
                        cx: &mut Context<Self>| match event {
            InputEvent::PressEnter { .. } => this.submit(window, cx),
            InputEvent::Change => {
                this.error = None;
                cx.notify();
            }
            _ => {}
        };
        let mut subscriptions = vec![
            cx.subscribe_in(&name, window, move |this, _, event, window, cx| {
                on_input(this, event, window, cx)
            }),
            cx.subscribe_in(&labels, window, move |this, _, event, window, cx| {
                on_input(this, event, window, cx)
            }),
        ];
        subscriptions.extend(crate::i18n::localized_placeholder(
            &name,
            || t!("editor-namespace-name-placeholder"),
            window,
            cx,
        ));
        subscriptions.extend(crate::i18n::localized_placeholder(
            &labels,
            || t!("editor-namespace-labels-placeholder"),
            window,
            cx,
        ));
        let connection = cluster.read(cx).connection().cloned();
        Self {
            cluster,
            connection,
            name,
            labels,
            error: None,
            creating: false,
            _subscriptions: subscriptions,
            _create: None,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        let name = self.name.read(cx).value().trim().to_owned();
        let checked = namespace::validate_name(&name)
            .and_then(|()| namespace::parse_labels(&self.labels.read(cx).value()));
        let labels = match checked {
            Ok(labels) => labels,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let Some(connection) = &self.connection else {
            self.error = Some(t!("editor-namespace-not-connected"));
            cx.notify();
            return;
        };
        if let Err(changed) = action_context::check(self.cluster.read(cx), connection) {
            self.error = Some(changed.to_string());
            cx.notify();
            return;
        }
        let client = connection.client.clone();
        let task = {
            let name = name.clone();
            runtime::spawn(async move { namespace::create(&client, &name, labels).await })
        };
        self.creating = true;
        self.error = None;
        cx.notify();
        self._create = Some(cx.spawn_in(window, async move |this, cx| {
            let result = match task.await {
                Ok(result) => result.map_err(|error| describe(&error)),
                Err(error) => Err(error.to_string()),
            };
            this.update_in(cx, |this, window, cx| {
                this.creating = false;
                match result {
                    Ok(()) => {
                        window.close_dialog(cx);
                        window.push_notification(
                            Notification::success(t!("editor-namespace-created", name = &name)),
                            cx,
                        );
                    }
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }
}

impl Render for NamespaceForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let label = |text: String| {
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let note = |text: String, color: Hsla| {
            div()
                .text_xs()
                .whitespace_normal()
                .text_color(color)
                .child(text)
        };
        let name = self.name.read(cx).value().trim().to_owned();
        let name_note = match name_error(&name) {
            Some(error) => note(error, theme.danger),
            None => note(t!("editor-namespace-name-hint"), theme.muted_foreground),
        };
        let valid = name_error(&name).is_none() && !name.is_empty();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(label(t!("editor-field-name")))
            .child(Input::new(&self.name).small())
            .child(name_note)
            .child(div().pt_1().child(label(t!("editor-namespace-labels"))))
            .child(Input::new(&self.labels).small())
            .child(note(
                t!("editor-namespace-labels-hint"),
                theme.muted_foreground,
            ))
            .children(self.error.clone().map(|error| note(error, theme.danger)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("namespace-cancel")
                            .ghost()
                            .small()
                            .label(t!("ui-cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("namespace-create")
                            .primary()
                            .small()
                            .label(if self.creating {
                                t!("editor-creating")
                            } else {
                                t!("editor-create")
                            })
                            .disabled(self.creating || !valid)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::name_error;

    #[test]
    fn name_is_checked_once_typed() {
        assert_eq!(name_error(""), None);
        assert_eq!(name_error("kubach-test-277"), None);
        assert!(name_error("Team_Web").is_some());
    }
}
