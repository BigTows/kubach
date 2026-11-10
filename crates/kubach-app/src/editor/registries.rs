//! The value of a docker config key (`.dockerconfigjson`, `.dockercfg`) in
//! the Secret editor as a list of registries (#139): address, user, email
//! and password of each, instead of one JSON text. What the form does not
//! show (other fields of the JSON) is kept by [`DockerConfig`]; `auth` is
//! written by it too.
//!
//! The form exists only after the value was loaded (or is being replaced),
//! under the same rules as any other value. Passwords sit in masked fields.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::*;
use kubach_core::secrets::{DockerConfig, Registry, RegistryEdit, SecretBytes};
use kubach_core::validation::ValidationError;
use kubach_i18n::t;
use kubach_ui::{KeyValueList, Tone};

use super::secret::{SecretEditor, watch_input};

/// A text in the current language.
type Text = fn() -> String;

struct RegistryRow {
    id: usize,
    /// The address in the loaded config; `None` for an added registry.
    original: Option<String>,
    server: Entity<InputState>,
    username: Entity<InputState>,
    email: Entity<InputState>,
    password: Entity<InputState>,
    /// A loaded password that is not text: kept unless a new one is typed.
    binary_password: Option<SecretBytes>,
    removed: bool,
}

impl RegistryRow {
    fn registry(&self, cx: &App) -> Registry {
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let typed = text(&self.password);
        let password = match &self.binary_password {
            Some(kept) if typed.is_empty() => kept.clone(),
            _ => SecretBytes::from(typed.as_str()),
        };
        Registry {
            server: text(&self.server),
            username: text(&self.username),
            email: text(&self.email),
            password,
        }
    }
}

pub(super) struct RegistryForm {
    /// The loaded config; empty when the value is written from scratch.
    config: DockerConfig,
    /// The loaded value; `None` when it is replaced without loading.
    baseline: Option<SecretBytes>,
    rows: Vec<RegistryRow>,
    next_row: usize,
}

impl RegistryForm {
    /// A form over `config`. With a `baseline` (the value `config` was
    /// parsed from) the form edits it; without one it starts empty.
    pub fn new(
        config: DockerConfig,
        baseline: Option<SecretBytes>,
        window: &mut Window,
        cx: &mut Context<SecretEditor>,
    ) -> (Self, Vec<Subscription>) {
        let mut form = Self {
            config,
            baseline,
            rows: Vec::new(),
            next_row: 0,
        };
        let mut subscriptions = Vec::new();
        for registry in form.config.registries() {
            subscriptions.extend(form.push(Some(registry), window, cx));
        }
        if form.rows.is_empty() && form.baseline.is_none() {
            subscriptions.extend(form.add(window, cx));
        }
        (form, subscriptions)
    }

    /// Whether the form holds a value fetched from the cluster.
    pub fn loaded(&self) -> bool {
        self.baseline.is_some()
    }

    /// Adds an empty registry and focuses its address.
    pub fn add(
        &mut self,
        window: &mut Window,
        cx: &mut Context<SecretEditor>,
    ) -> Vec<Subscription> {
        let subscriptions = self.push(None, window, cx);
        if let Some(row) = self.rows.last() {
            row.server.update(cx, |input, cx| input.focus(window, cx));
        }
        subscriptions
    }

    fn push(
        &mut self,
        registry: Option<Registry>,
        window: &mut Window,
        cx: &mut Context<SecretEditor>,
    ) -> Vec<Subscription> {
        self.next_row += 1;
        let original = registry.as_ref().map(|registry| registry.server.clone());
        let registry = registry.unwrap_or_default();
        let (password, binary_password) = match registry.password.text() {
            Some(text) => (text.to_owned(), None),
            None => (String::new(), Some(registry.password.clone())),
        };
        let mut input = |text: String, masked: bool| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(text)
                    .masked(masked)
            })
        };
        let row = RegistryRow {
            id: self.next_row,
            original,
            server: input(registry.server.clone(), false),
            username: input(registry.username.clone(), false),
            email: input(registry.email.clone(), false),
            password: input(password, true),
            binary_password,
            removed: false,
        };
        let binary = row.binary_password.is_some();
        let placeholders: [(&Entity<InputState>, Text); 4] = [
            (&row.server, || t!("editor-registry-server-placeholder")),
            (&row.username, || t!("editor-secret-user-placeholder")),
            (
                &row.password,
                if binary {
                    || t!("editor-registry-password-binary")
                } else {
                    || t!("editor-secret-password-placeholder")
                },
            ),
            (&row.email, || t!("editor-secret-optional")),
        ];
        let mut subscriptions = Vec::new();
        for (input, text) in placeholders {
            subscriptions.extend(crate::i18n::localized_placeholder(input, text, window, cx));
            subscriptions.push(watch_input(input, cx));
        }
        self.rows.push(row);
        subscriptions
    }

    /// Removes registry `id`: a loaded one is marked (and can be kept
    /// again), an added one is dropped.
    pub fn remove(&mut self, id: usize) {
        if let Some(index) = self.rows.iter().position(|row| row.id == id) {
            if self.rows[index].original.is_some() {
                self.rows[index].removed = true;
            } else {
                self.rows.remove(index);
            }
        }
    }

    pub fn restore(&mut self, id: usize) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
            row.removed = false;
        }
    }

    fn edited(&self, cx: &App) -> Result<DockerConfig, ValidationError> {
        let edits: Vec<RegistryEdit> = self
            .rows
            .iter()
            .filter(|row| !row.removed)
            .map(|row| RegistryEdit {
                original: row.original.clone(),
                registry: row.registry(cx),
            })
            .collect();
        self.config.with_registries(&edits)
    }

    /// The value to save: `None` when the loaded config is unchanged.
    pub fn value(&self, cx: &App) -> Result<Option<SecretBytes>, ValidationError> {
        let edited = self.edited(cx)?;
        Ok((self.baseline.is_none() || edited != self.config).then(|| edited.to_bytes()))
    }

    /// The form as JSON text, to go on editing it as a plain value, with
    /// the loaded value it changes. An unchanged form gives the loaded text
    /// as it is; a form that can't be saved gives the loaded config.
    pub fn json(&self, cx: &App) -> (String, Option<SecretBytes>) {
        let edited = self.edited(cx).unwrap_or_else(|_| self.config.clone());
        let unchanged = self
            .baseline
            .as_ref()
            .filter(|_| edited == self.config)
            .and_then(|baseline| baseline.text().map(str::to_owned));
        let text = unchanged.unwrap_or_else(|| {
            let bytes = edited.to_bytes();
            let compact = String::from_utf8_lossy(bytes.expose());
            kubach_core::data_keys::format_json(&compact).unwrap_or_else(|_| compact.into_owned())
        });
        (text, self.baseline.clone())
    }

    /// The registries of key row `key_id`.
    pub fn render(
        &self,
        key_id: usize,
        editable: bool,
        cx: &mut Context<SecretEditor>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let mut column = div().flex().flex_col().gap_2();
        if self.rows.is_empty() {
            column = column.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("editor-registry-none")),
            );
        }
        for row in &self.rows {
            let id = row.id;
            let (status, tone) = match (&row.original, row.removed) {
                (None, _) => (Some(t!("editor-key-new")), Tone::Success),
                (Some(_), true) => (Some(t!("editor-key-removed")), Tone::Danger),
                (Some(_), false) => (None, Tone::Neutral),
            };
            let toggle = if row.removed {
                Button::new(("registry-keep", id))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Undo2)
                    .label(t!("editor-secret-keep"))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.restore_registry(key_id, id, cx)),
                    )
            } else {
                Button::new(("registry-remove", id))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Delete)
                    .tooltip(t!("editor-registry-remove"))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.remove_registry(key_id, id, cx)),
                    )
            };
            let header = div()
                .flex()
                .items_center()
                .gap_2()
                .child(if row.removed {
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .line_through()
                        .text_color(theme.muted_foreground)
                        .child(row.original.clone().unwrap_or_default())
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&row.server).small().disabled(!editable))
                        .into_any_element()
                })
                .children(status.map(|status| {
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(tone.color(cx))
                        .child(status)
                }))
                .children(editable.then_some(toggle));
            let mut card = div()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .child(header);
            if !row.removed {
                card = card.child(
                    KeyValueList::new()
                        .row(
                            t!("editor-field-user"),
                            Input::new(&row.username).small().disabled(!editable),
                        )
                        .row(
                            t!("editor-field-password"),
                            Input::new(&row.password)
                                .small()
                                .mask_toggle()
                                .disabled(!editable),
                        )
                        .row(
                            t!("editor-field-email"),
                            Input::new(&row.email).small().disabled(!editable),
                        ),
                );
            }
            column = column.child(card);
        }
        if editable {
            column = column.child(
                div().child(
                    Button::new(("registry-add", key_id))
                        .outline()
                        .xsmall()
                        .icon(IconName::Plus)
                        .label(t!("editor-registry-add"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.add_registry(key_id, window, cx)
                        })),
                ),
            );
        }
        column
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(t!("editor-registry-hint")),
            )
            .into_any_element()
    }
}
