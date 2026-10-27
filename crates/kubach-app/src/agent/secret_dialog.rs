//! Kubach's own form for a Secret an agent asked to create or change
//! (#230, ADR 0005 "Запись Secrets через пользователя"), and the parts of
//! the copy and delete confirmations that are about Secrets.
//!
//! The user types values, generates them or picks files; the answer goes
//! to Kubach, which writes the Secret, and never to the agent. The name,
//! keys and explanation come from the agent and are shown as unverified
//! text; the context and namespace are shown large, so a request for a
//! production token stands out. Generated values are never drawn, only
//! copied on request (cleared from the clipboard after 30 s). Nothing is
//! remembered: every form is one write.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::secrets::checks::{self, KeyMatch};
use kubach_core::secrets::generate::{self, Charset};
use kubach_core::secrets::{self, NewSecret, SecretBytes, typed};
use kubach_i18n::t;
use kubach_mcp::{
    ConfirmationDecision, ConfirmationRequest, SecretInput, SecretOp, SecretRequest, check_input,
};
use kubach_ui::{FilePickField, GeneratorControls, KeyValueList, SecretField, Tone, UntrustedText};

use super::{AgentState, countdown};

/// Largest file read for a value (a whole Secret is limited to 1 MiB).
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The dialog title for a Secret request.
pub(super) fn title(secret: &SecretRequest) -> String {
    match secret.op {
        SecretOp::Create => t!("agent-title-secret-create"),
        SecretOp::Update => t!("agent-title-secret-update"),
        SecretOp::Copy => t!("agent-title-secret-copy"),
        SecretOp::Delete => t!("agent-title-secret-delete"),
    }
}

/// Context and namespace, large: where the values will go.
pub(super) fn target(request: &ConfirmationRequest, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap_x_2()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t!("agent-writes-to")),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(format!("{} / {}", request.context, request.namespace)),
        )
}

/// What the agent said, marked as unverified: the Secret's name, the
/// source of a copy, the explanation.
pub(super) fn agent_says(secret: &SecretRequest, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let mut list = KeyValueList::new().row(t!("agent-secret"), secret.name.clone());
    if let Some(source) = &secret.source {
        list = list.row(
            t!("agent-copied-from"),
            format!("{}/{}", source.namespace, source.name),
        );
    }
    list = list.row(t!("agent-secret-type"), secret.secret_type.clone());
    if matches!(secret.op, SecretOp::Copy | SecretOp::Delete) && !secret.existing.is_empty() {
        list = list.row(
            t!("agent-secret-keys"),
            secret
                .existing
                .iter()
                .map(|k| format!("{} ({})", k.key, secrets::describe_size(k.size)))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(list)
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t!("agent-secret-unverified")),
        )
        .when(!secret.explanation.trim().is_empty(), |this| {
            this.child(UntrustedText::new(
                t!("agent-explanation"),
                secret.explanation.clone(),
            ))
        })
}

/// Pods and workloads that reference the Secret.
pub(super) fn users(secret: &SecretRequest, cx: &App) -> Option<AnyElement> {
    if secret.used_by.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let color = if secret.op == SecretOp::Delete {
        theme.danger
    } else {
        theme.muted_foreground
    };
    let title = match secret.op {
        SecretOp::Delete => t!("agent-still-referenced"),
        _ => t!("agent-used-by"),
    };
    Some(
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(color.opacity(0.5))
            .text_xs()
            .child(div().text_color(color).child(title))
            .children(secret.used_by.iter().map(|user| {
                div().text_color(theme.foreground).child(format!(
                    "{} {} — {}",
                    user.kind,
                    user.name,
                    user.via.join("; ")
                ))
            }))
            .into_any_element(),
    )
}

/// Opens the form for pending request `id` in `window`.
pub(super) fn open(agent: Entity<AgentState>, id: u64, window: &mut Window, cx: &mut App) {
    let Some(request) = agent
        .read(cx)
        .pending_by_id(id)
        .map(|pending| pending.request.clone())
    else {
        return;
    };
    let kubach_mcp::RequestKind::Secret { secret } = request.kind.clone() else {
        return;
    };
    let title = title(&secret);
    let view = cx.new(|cx| SecretInputView::new(agent, id, request, *secret, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(title.clone())
            .w(px(680.))
            .overlay_closable(false)
            .keyboard(false)
            .close_button(false)
            .child(view.clone())
    });
}

// ------------------------------------------------------------- the form

/// A value read from a file.
struct Picked {
    name: String,
    value: SecretBytes,
}

/// Where a row's value comes from.
enum Value {
    /// An existing key left as it is.
    Keep,
    Typed(Entity<InputState>),
    /// Drawn by the generator; never shown.
    Generated(SecretBytes),
    File(Picked),
    /// An existing key to remove.
    Remove,
}

struct Generator {
    length: usize,
    charset: Charset,
}

impl Default for Generator {
    fn default() -> Self {
        Self {
            length: generate::DEFAULT_LENGTH,
            charset: Charset::default(),
        }
    }
}

struct Row {
    id: usize,
    /// Name and size of an existing key; a new key has an input instead.
    existing: Option<(String, usize)>,
    name: Option<Entity<InputState>>,
    value: Value,
    /// The agent asked a value for this key.
    asked: bool,
    /// The agent suggests removing this key.
    suggested_remove: bool,
    generator: Option<Generator>,
}

impl Row {
    fn key(&self, cx: &App) -> String {
        match (&self.existing, &self.name) {
            (Some((key, _)), _) => key.clone(),
            (None, Some(name)) => name.read(cx).value().trim().to_owned(),
            (None, None) => String::new(),
        }
    }
}

enum Form {
    Keys(Vec<Row>),
    Tls {
        cert: Option<Picked>,
        key: Option<Picked>,
    },
    Docker {
        server: Entity<InputState>,
        username: Entity<InputState>,
        password: Entity<InputState>,
        email: Entity<InputState>,
    },
}

pub(super) struct SecretInputView {
    agent: Entity<AgentState>,
    id: u64,
    request: ConfirmationRequest,
    secret: SecretRequest,
    form: Form,
    next_row: usize,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

fn text_input(
    placeholder: &str,
    value: Option<&str>,
    masked: bool,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let placeholder = placeholder.to_owned();
    let value = value.map(str::to_owned);
    cx.new(|cx| {
        let state = InputState::new(window, cx)
            .placeholder(placeholder)
            .masked(masked);
        match value {
            Some(value) => state.default_value(value),
            None => state,
        }
    })
}

impl SecretInputView {
    fn new(
        agent: Entity<AgentState>,
        id: u64,
        request: ConfirmationRequest,
        secret: SecretRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&agent, |_, _, cx| cx.notify());
        let mut view = Self {
            agent,
            id,
            request,
            secret: secret.clone(),
            form: Form::Keys(Vec::new()),
            next_row: 0,
            error: None,
            _subscriptions: vec![observe],
            _tasks: Vec::new(),
        };
        view.form = match secret.secret_type.as_str() {
            typed::TYPE_TLS => Form::Tls {
                cert: None,
                key: None,
            },
            typed::TYPE_DOCKER_CONFIG_JSON => Form::Docker {
                server: text_input(secrets::edit::DEFAULT_REGISTRY, None, false, window, cx),
                username: text_input(&t!("agent-placeholder-user"), None, false, window, cx),
                password: text_input(&t!("agent-placeholder-password"), None, true, window, cx),
                email: text_input(&t!("agent-placeholder-optional"), None, false, window, cx),
            },
            _ => {
                let mut rows = Vec::new();
                for existing in &secret.existing {
                    let asked = secret.keys.contains(&existing.key);
                    let value = if asked {
                        Value::Typed(view.value_input(window, cx))
                    } else {
                        Value::Keep
                    };
                    rows.push(view.row(
                        Some((existing.key.clone(), existing.size)),
                        None,
                        value,
                        asked,
                        secret.remove.contains(&existing.key),
                    ));
                }
                let new_keys: Vec<String> = secret
                    .keys
                    .iter()
                    .filter(|key| !secret.existing.iter().any(|e| &e.key == *key))
                    .cloned()
                    .collect();
                for key in &new_keys {
                    let name = view.name_input(Some(key), window, cx);
                    let value = Value::Typed(view.value_input(window, cx));
                    rows.push(view.row(None, Some(name), value, true, false));
                }
                if rows.is_empty() {
                    let name = view.name_input(None, window, cx);
                    let value = Value::Typed(view.value_input(window, cx));
                    rows.push(view.row(None, Some(name), value, false, false));
                }
                Form::Keys(rows)
            }
        };
        if let Form::Docker {
            server,
            username,
            password,
            email,
        } = &view.form
        {
            let watched = [server, username, password, email].map(|input| watch(input, cx));
            view._subscriptions.extend(watched);
        }
        view
    }

    fn row(
        &mut self,
        existing: Option<(String, usize)>,
        name: Option<Entity<InputState>>,
        value: Value,
        asked: bool,
        suggested_remove: bool,
    ) -> Row {
        self.next_row += 1;
        Row {
            id: self.next_row,
            existing,
            name,
            value,
            asked,
            suggested_remove,
            generator: None,
        }
    }

    fn name_input(
        &mut self,
        key: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let input = text_input(&t!("agent-placeholder-key"), key, false, window, cx);
        self._subscriptions.push(watch(&input, cx));
        input
    }

    fn value_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let input = text_input(&t!("agent-placeholder-value"), None, true, window, cx);
        self._subscriptions.push(watch(&input, cx));
        input
    }

    fn rows_mut(&mut self) -> Option<&mut Vec<Row>> {
        match &mut self.form {
            Form::Keys(rows) => Some(rows),
            _ => None,
        }
    }

    fn row_mut(&mut self, id: usize) -> Option<&mut Row> {
        self.rows_mut()?.iter_mut().find(|row| row.id == id)
    }

    fn add_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name_input(None, window, cx);
        let value = Value::Typed(self.value_input(window, cx));
        let row = self.row(None, Some(name), value, false, false);
        if let Some(rows) = self.rows_mut() {
            rows.push(row);
        }
        cx.notify();
    }

    fn type_value(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.value_input(window, cx);
        input.update(cx, |input, cx| input.focus(window, cx));
        if let Some(row) = self.row_mut(id) {
            row.value = Value::Typed(input);
            row.generator = None;
        }
        cx.notify();
    }

    fn generate(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.row_mut(id) {
            let generator = row.generator.get_or_insert_with(Generator::default);
            row.value = Value::Generated(generate::generate(generator.length, generator.charset));
        }
        cx.notify();
    }

    fn set_generator(
        &mut self,
        id: usize,
        length: Option<usize>,
        charset: Option<Charset>,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.row_mut(id)
            && let Some(generator) = row.generator.as_mut()
        {
            if let Some(length) = length {
                generator.length = length;
            }
            if let Some(charset) = charset {
                generator.charset = charset;
            }
        }
        // A new setting draws a new value, so what is copied matches it.
        self.generate(id, cx);
    }

    fn copy_generated(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(Row {
            value: Value::Generated(value),
            ..
        }) = self.row_mut(id)
        else {
            return;
        };
        // Generated values are ASCII.
        let text = String::from_utf8_lossy(value.expose()).into_owned();
        crate::secret_clipboard::copy(&text, cx);
        let mut bytes = text.into_bytes();
        secrets::value::wipe(&mut bytes);
    }

    fn set_value(&mut self, id: usize, value: Value, cx: &mut Context<Self>) {
        if let Some(row) = self.row_mut(id) {
            row.value = value;
            row.generator = None;
        }
        cx.notify();
    }

    fn drop_row(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(rows) = self.rows_mut() {
            rows.retain(|row| row.id != id);
        }
        cx.notify();
    }

    /// Reads a file picked by the user into `apply`.
    fn pick_file(
        &mut self,
        cx: &mut Context<Self>,
        apply: fn(&mut Self, Picked, &mut Context<Self>),
    ) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = read_file(&path);
            this.update(cx, |this, cx| {
                match result {
                    Ok(picked) => {
                        this.error = None;
                        apply(this, picked, cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn pick_row_file(&mut self, id: usize, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = read_file(&path);
            this.update(cx, |this, cx| {
                match result {
                    Ok(picked) => this.set_value(id, Value::File(picked), cx),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The answer, checked as Kubach will check it.
    fn build(&self, cx: &App) -> Result<SecretInput, String> {
        let mut input = SecretInput::default();
        match &self.form {
            Form::Keys(rows) => {
                for row in rows {
                    let key = row.key(cx);
                    let value = match &row.value {
                        Value::Keep => continue,
                        Value::Remove => {
                            input.remove.insert(key);
                            continue;
                        }
                        Value::Typed(field) => {
                            let text = field.read(cx).value();
                            if text.is_empty() {
                                return Err(if key.is_empty() {
                                    t!("agent-enter-value-new")
                                } else {
                                    t!("agent-enter-value", key = key.as_str())
                                });
                            }
                            SecretBytes::from(text.as_ref())
                        }
                        Value::Generated(value) => value.clone(),
                        Value::File(picked) => picked.value.clone(),
                    };
                    secrets::validate_key(&key).map_err(|e| e.to_string())?;
                    if row.existing.is_none()
                        && rows.iter().any(|other| {
                            other.existing.as_ref().is_some_and(|(k, _)| *k == key)
                                && !matches!(other.value, Value::Remove)
                        })
                    {
                        return Err(t!("agent-key-exists", key = key.as_str()));
                    }
                    if input.set.insert(key.clone(), value).is_some() {
                        return Err(t!("agent-key-twice", key = key.as_str()));
                    }
                }
            }
            Form::Tls { cert, key } => {
                let (Some(cert), Some(key)) = (cert, key) else {
                    return Err(t!("agent-choose-tls"));
                };
                input
                    .set
                    .insert(typed::KEY_TLS_CERT.into(), cert.value.clone());
                input
                    .set
                    .insert(typed::KEY_TLS_KEY.into(), key.value.clone());
            }
            Form::Docker {
                server,
                username,
                password,
                email,
            } => {
                let server = server.read(cx).value().trim().to_owned();
                let secret = NewSecret::DockerRegistry {
                    server: if server.is_empty() {
                        secrets::edit::DEFAULT_REGISTRY.to_owned()
                    } else {
                        server
                    },
                    username: username.read(cx).value().trim().to_owned(),
                    password: SecretBytes::from(password.read(cx).value().as_ref()),
                    email: Some(email.read(cx).value().trim().to_owned()).filter(|e| !e.is_empty()),
                };
                let mut built = secret
                    .build(&self.request.namespace, "check")
                    .map_err(|e| e.to_string())?;
                let mut data = built.data.take().unwrap_or_default();
                if let Some(config) = data.get_mut(typed::KEY_DOCKER_CONFIG_JSON) {
                    input.set.insert(
                        typed::KEY_DOCKER_CONFIG_JSON.into(),
                        SecretBytes::new(std::mem::take(&mut config.0)),
                    );
                }
                for value in data.values_mut() {
                    secrets::value::wipe(&mut value.0);
                }
            }
        }
        check_input(&self.secret, &input).map_err(capitalize)?;
        Ok(input)
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.build(cx) {
            Ok(input) => {
                self.clear_inputs(window, cx);
                self.answer(ConfirmationDecision::Provided { input }, window, cx);
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_inputs(window, cx);
        self.answer(
            ConfirmationDecision::Rejected {
                reason: Some("cancelled by the user".into()),
            },
            window,
            cx,
        );
    }

    /// Empties every field and drops generated and picked values, so the
    /// form keeps nothing once answered.
    fn clear_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut inputs = Vec::new();
        match &mut self.form {
            Form::Keys(rows) => {
                for row in rows.iter_mut() {
                    if let Value::Typed(input) = &row.value {
                        inputs.push(input.clone());
                    }
                    row.value = Value::Keep;
                }
            }
            Form::Tls { cert, key } => {
                *cert = None;
                *key = None;
            }
            Form::Docker { password, .. } => inputs.push(password.clone()),
        }
        for input in inputs {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    fn answer(&self, decision: ConfirmationDecision, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id;
        self.agent
            .update(cx, |agent, cx| agent.resolve(id, decision, cx));
        window.close_dialog(cx);
    }

    // --------------------------------------------------------- rendering

    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let id = row.id;
        let existing = row.existing.is_some();
        let label: SharedString = match &row.existing {
            Some((key, _)) => key.clone().into(),
            None => t!("agent-new-key").into(),
        };
        let (status, tone) = match (&row.value, &row.existing) {
            (Value::Keep, Some((_, size))) => (
                t!(
                    "agent-value-unchanged",
                    size = secrets::describe_size(*size)
                ),
                Tone::Neutral,
            ),
            (Value::Keep, None) => (String::new(), Tone::Neutral),
            (Value::Typed(input), _) => {
                let chars = input.read(cx).value().chars().count();
                if chars == 0 {
                    (t!("agent-value-type"), Tone::Warning)
                } else {
                    (t!("agent-value-typed", count = chars), Tone::Info)
                }
            }
            (Value::Generated(value), _) => (
                t!("agent-value-generated", count = value.len()),
                Tone::Success,
            ),
            (Value::File(picked), _) => (
                t!(
                    "agent-value-file",
                    name = picked.name.as_str(),
                    size = secrets::describe_size(picked.value.len())
                ),
                Tone::Info,
            ),
            (Value::Remove, _) => (t!("agent-value-removed"), Tone::Danger),
        };
        let mut field = SecretField::new(("secret-row", id), label)
            .status(status, tone)
            .struck(matches!(row.value, Value::Remove));
        if row.asked {
            field = field.badge(t!("agent-badge-asked"), Tone::Info);
        } else if row.suggested_remove {
            field = field.badge(t!("agent-badge-remove"), Tone::Warning);
        }
        let button = |name: &'static str, icon: IconName, tooltip: String| {
            Button::new((name, id))
                .ghost()
                .xsmall()
                .icon(icon)
                .tooltip(tooltip)
        };
        match &row.value {
            Value::Remove => {
                field = field.action(
                    button("secret-keep", IconName::Undo2, t!("agent-keep-key"))
                        .label(t!("agent-keep"))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.set_value(id, Value::Keep, cx)),
                        ),
                );
            }
            value => {
                if !matches!(value, Value::Typed(_)) {
                    field = field.action(
                        button("secret-type", IconName::Replace, t!("agent-type-new"))
                            .label(t!("agent-type"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.type_value(id, window, cx)
                            })),
                    );
                }
                field = field.action(
                    button(
                        "secret-generate",
                        IconName::RefreshCw,
                        t!("agent-generate-tooltip"),
                    )
                    .label(t!("agent-generate"))
                    .on_click(cx.listener(move |this, _, _, cx| this.generate(id, cx))),
                );
                if matches!(value, Value::Generated(_)) {
                    field = field.action(
                        button("secret-copy", IconName::Copy, t!("agent-copy-generated")).on_click(
                            cx.listener(move |this, _, _, cx| this.copy_generated(id, cx)),
                        ),
                    );
                }
                field = field.action(
                    button("secret-file", IconName::File, t!("agent-from-file"))
                        .on_click(cx.listener(move |this, _, _, cx| this.pick_row_file(id, cx))),
                );
                field = if existing {
                    let undo = !matches!(value, Value::Keep);
                    field
                        .when(undo, |field| {
                            field.action(
                                button("secret-undo", IconName::Undo2, t!("agent-keep-current"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_value(id, Value::Keep, cx)
                                    })),
                            )
                        })
                        .action(
                            button("secret-remove", IconName::Delete, t!("agent-remove-key"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_value(id, Value::Remove, cx)
                                })),
                        )
                } else {
                    field.action(
                        button("secret-drop", IconName::Delete, t!("agent-leave-out"))
                            .on_click(cx.listener(move |this, _, _, cx| this.drop_row(id, cx))),
                    )
                };
            }
        }
        let name = row
            .name
            .as_ref()
            .map(|name| div().w(px(260.)).child(Input::new(name).small()));
        let value: Option<AnyElement> = match (&row.value, &row.generator) {
            (Value::Typed(input), _) => {
                Some(Input::new(input).small().mask_toggle().into_any_element())
            }
            (Value::Generated(_), Some(generator)) => {
                let charsets = Charset::ALL;
                let active = charsets
                    .iter()
                    .position(|c| *c == generator.charset)
                    .unwrap_or(0);
                Some(
                    GeneratorControls::new(
                        SharedString::from(format!("secret-gen-{id}")),
                        generator.length,
                        generate::MIN_LENGTH..=generate::MAX_LENGTH,
                    )
                    .charsets(charsets.iter().map(|c| c.label()), active)
                    .on_length(cx.listener(move |this, length: &usize, _, cx| {
                        this.set_generator(id, Some(*length), None, cx)
                    }))
                    .on_charset(cx.listener(move |this, ix: &usize, _, cx| {
                        this.set_generator(id, None, Charset::ALL.get(*ix).copied(), cx)
                    }))
                    .on_generate(cx.listener(move |this, _, _, cx| this.generate(id, cx)))
                    .into_any_element(),
                )
            }
            _ => None,
        };
        if name.is_none() && value.is_none() {
            return field.into_any_element();
        }
        field
            .body(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .children(name)
                    .children(value),
            )
            .into_any_element()
    }

    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.form {
            Form::Keys(rows) => div()
                .flex()
                .flex_col()
                .gap_2()
                .children(rows.iter().map(|row| self.render_row(row, cx)))
                .child(
                    div().child(
                        Button::new("secret-add-key")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label(t!("agent-add-key"))
                            .on_click(cx.listener(|this, _, window, cx| this.add_key(window, cx))),
                    ),
                )
                .into_any_element(),
            Form::Tls { cert, key } => {
                let mut cert_field = FilePickField::new("secret-tls-cert")
                    .chosen(cert.as_ref().map(|c| format!("tls.crt: {}", c.name)))
                    .on_pick(cx.listener(|this, _, _, cx| {
                        this.pick_file(cx, |this, picked, _| {
                            if let Form::Tls { cert, .. } = &mut this.form {
                                *cert = Some(picked);
                            }
                        })
                    }));
                if let Some(cert) = cert {
                    let (text, tone) = describe_cert(&cert.value);
                    cert_field = cert_field.detail(text, tone);
                }
                let mut key_field = FilePickField::new("secret-tls-key")
                    .chosen(key.as_ref().map(|k| format!("tls.key: {}", k.name)))
                    .on_pick(cx.listener(|this, _, _, cx| {
                        this.pick_file(cx, |this, picked, _| {
                            if let Form::Tls { key, .. } = &mut this.form {
                                *key = Some(picked);
                            }
                        })
                    }));
                if let Some(key) = key {
                    let (text, tone) = describe_key(cert.as_ref().map(|c| &c.value), &key.value);
                    key_field = key_field.detail(text, tone);
                }
                KeyValueList::new()
                    .row(t!("agent-tls-cert"), cert_field)
                    .row(t!("agent-tls-key"), key_field)
                    .into_any_element()
            }
            Form::Docker {
                server,
                username,
                password,
                email,
            } => KeyValueList::new()
                .row(t!("agent-registry"), Input::new(server).small())
                .row(t!("agent-user"), Input::new(username).small())
                .row(
                    t!("agent-password"),
                    Input::new(password).small().mask_toggle(),
                )
                .row(t!("agent-email"), Input::new(email).small())
                .into_any_element(),
        }
    }
}

impl Render for SecretInputView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let agent = self.agent.read(cx);
        let Some(pending) = agent.pending_by_id(self.id) else {
            return div()
                .text_sm()
                .child(t!("agent-no-longer-pending"))
                .into_any_element();
        };
        let left = countdown(pending.deadline);
        let degraded =
            agent.server_limits_warning(&self.request.session_id, &self.request.namespace);
        let request = self.request.clone();
        let hint = match self.secret.secret_type.as_str() {
            typed::TYPE_TLS => t!("agent-hint-tls"),
            typed::TYPE_DOCKER_CONFIG_JSON => t!("agent-hint-docker"),
            _ if self.secret.op == SecretOp::Update => t!("agent-hint-update"),
            _ => t!("agent-hint-keys"),
        };
        let submit = match self.secret.op {
            SecretOp::Create => t!("agent-create-secret"),
            _ => t!("agent-apply"),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(target(&request, cx))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(request.summary.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("agent-client", client = request.client.as_str()))
                    .child(t!("agent-client-unverified-note")),
            )
            .child(agent_says(&self.secret, cx))
            .child(
                div()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.info.opacity(0.5))
                    .text_xs()
                    .text_color(theme.foreground)
                    .child(t!("agent-secret-info")),
            )
            .when_some(degraded, |this, warning| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(t!("agent-secret-limits", warning = warning)),
                )
            })
            .children(users(&self.secret, cx))
            .child(
                div()
                    .id("secret-form")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .child(self.render_form(cx)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(hint),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(theme.danger).child(error))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t!("agent-auto-cancel", left = left)),
                    )
                    .child(
                        Button::new("secret-cancel")
                            .outline()
                            .small()
                            .label(t!("agent-cancel"))
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(
                        Button::new("secret-submit")
                            .primary()
                            .small()
                            .label(submit)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .into_any_element()
    }
}

fn watch(input: &Entity<InputState>, cx: &mut Context<SecretInputView>) -> Subscription {
    cx.subscribe(input, |this, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            this.error = None;
            cx.notify();
        }
    })
}

fn capitalize(message: String) -> String {
    let mut chars = message.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => message,
    }
}

fn read_file(path: &std::path::Path) -> Result<Picked, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let unreadable = |e: std::io::Error| {
        t!(
            "agent-file-unreadable",
            name = name.as_str(),
            error = e.to_string()
        )
    };
    let size = std::fs::metadata(path).map_err(unreadable)?.len();
    if size > MAX_FILE_BYTES {
        return Err(t!("agent-file-too-large", name = name.as_str()));
    }
    let bytes = std::fs::read(path).map_err(unreadable)?;
    Ok(Picked {
        name,
        value: SecretBytes::new(bytes),
    })
}

/// What the certificate file holds: subject and expiry, or why not.
fn describe_cert(pem: &SecretBytes) -> (String, Tone) {
    match typed::certificates(pem.expose()) {
        Ok(chain) => match chain.first() {
            Some(cert) => (
                t!(
                    "agent-cert-valid",
                    subject = cert.subject.as_str(),
                    time = secrets::x509::format_time(cert.not_after)
                ),
                Tone::Success,
            ),
            None => (t!("agent-cert-none"), Tone::Danger),
        },
        Err(error) => (capitalize(error.to_string()), Tone::Danger),
    }
}

/// Whether the key file holds a key, and whether it belongs to `cert`.
fn describe_key(cert: Option<&SecretBytes>, key: &SecretBytes) -> (String, Tone) {
    if !typed::has_private_key(key.expose()) {
        return (t!("agent-key-none"), Tone::Danger);
    }
    let Some(cert) = cert else {
        return (t!("agent-key-loaded"), Tone::Neutral);
    };
    match checks::key_matches_certificate(cert.expose(), key.expose()) {
        KeyMatch::Matches => (t!("agent-key-matches"), Tone::Success),
        KeyMatch::Differs => (t!("agent-key-differs"), Tone::Danger),
        KeyMatch::Unknown => (t!("agent-key-unknown"), Tone::Warning),
    }
}

#[cfg(test)]
mod tests {
    use super::capitalize;

    #[test]
    fn messages_start_with_a_capital() {
        assert_eq!(capitalize("enter a value".into()), "Enter a value");
        assert_eq!(capitalize(String::new()), "");
    }
}
