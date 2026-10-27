//! Secret editor tab: a key → value form instead of YAML.
//!
//! Values are typed as plain text (or read from files); Kubach does the
//! base64. An existing Secret opens with its keys only: a value is fetched
//! when the user asks to edit it, and keys left alone are not part of the
//! patch at all, so values never loaded can't be overwritten. The review
//! names the changed keys with `<hidden, changed>`, never values.
//!
//! When the context's security settings hide Secret values (#96), "Load
//! value" is unavailable; values can still be replaced without loading,
//! and keys added or removed.
//!
//! Each key is a block with its own code editor (#275): JSON and YAML
//! values are highlighted and checked, JSON can be formatted. A key whose
//! value is in the form can be renamed: the patch removes the old key and
//! sets the new one.

use kubach_ui::ButtonLoading as _;
use std::path::Path;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::data_keys::ContentKind;
use kubach_core::secrets::{
    self, Access, KeyChange, NewSecret, ReviewLine, SecretBytes, SecretInfo, SecretPatch, typed,
};
use kubach_core::settings::SecretAction;
use kubach_core::{Connection, ConnectionState, runtime};
use kubach_i18n::t;
use kubach_ui::{DataKeyBlock, KeyValueList, Panel, Tone};
use kube::Client;

use super::value::{ValueEditor, binary_text, content_label, size_text, unreadable};
use super::{Banner, callout, context_notice};
use crate::cluster::Cluster;

/// Largest value read from a file (a whole Secret is limited to 1 MiB).
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const FORM_WIDTH: Pixels = px(720.);

/// A text in the current language, such as a placeholder.
type Text = fn() -> String;

/// What a Secret editor works on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretTarget {
    Existing { namespace: String, name: String },
    New { namespace: String },
}

impl SecretTarget {
    fn title(&self) -> String {
        match self {
            Self::Existing { namespace, name } => format!("Secret {namespace}/{name}"),
            Self::New { namespace } => t!("editor-secret-new-in", namespace = namespace),
        }
    }

    fn tab_title(&self) -> String {
        match self {
            Self::Existing { name, .. } => format!("Secret/{name}"),
            Self::New { .. } => t!("editor-new-secret"),
        }
    }

    fn namespace(&self) -> &str {
        match self {
            Self::Existing { namespace, .. } | Self::New { namespace } => namespace,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Busy {
    Loading,
    Applying,
}

/// The value side of a key row.
enum ValueState {
    /// An existing key, not loaded and not changed.
    Untouched,
    /// Typed text; `baseline` is the loaded value, `None` when the value is
    /// being replaced without loading it.
    Text {
        editor: ValueEditor,
        baseline: Option<SecretBytes>,
    },
    /// The value comes from a file.
    File {
        name: String,
        value: SecretBytes,
    },
    /// The loaded value is binary: it can only be replaced from a file.
    Binary {
        len: usize,
    },
    Removed,
}

struct KeyRow {
    id: usize,
    /// Name and size of an existing key; `None` for a new key.
    existing: Option<(String, usize)>,
    /// Name of a new key.
    name: Option<Entity<InputState>>,
    /// New name of an existing key being renamed.
    rename: Option<Entity<InputState>>,
    value: ValueState,
    loading: bool,
}

/// What a row contributes to the change.
enum RowChange {
    None,
    Set(String, SecretBytes),
    Remove(String),
    /// An existing key under a new name: remove `from`, set `to`.
    Rename {
        from: String,
        to: String,
        value: SecretBytes,
    },
}

impl KeyRow {
    fn key(&self, cx: &App) -> String {
        match (&self.existing, &self.name) {
            (Some((key, _)), _) => key.clone(),
            (None, Some(name)) => name.read(cx).value().trim().to_owned(),
            (None, None) => String::new(),
        }
    }

    /// The name the key ends up with, if an existing key is renamed.
    fn renamed_to(&self, cx: &App) -> Option<String> {
        let (existing, _) = self.existing.as_ref()?;
        let to = self.rename.as_ref()?.read(cx).value().trim().to_owned();
        (to != *existing).then_some(to)
    }

    /// The name the key has after the change.
    fn final_key(&self, cx: &App) -> String {
        self.renamed_to(cx).unwrap_or_else(|| self.key(cx))
    }

    /// The value in the form, if any.
    fn form_value(&self, cx: &App) -> Option<SecretBytes> {
        match &self.value {
            ValueState::Text { editor, .. } => Some(SecretBytes::from(editor.text(cx).as_str())),
            ValueState::File { value, .. } => Some(value.clone()),
            _ => None,
        }
    }

    /// Whether the key can be renamed: its value must be in the form, since
    /// the new key is set with it.
    fn can_rename(&self) -> bool {
        self.existing.is_some()
            && matches!(
                self.value,
                ValueState::Text { .. } | ValueState::File { .. }
            )
    }

    fn change(&self, cx: &App) -> RowChange {
        let key = self.key(cx);
        if matches!(self.value, ValueState::Removed) {
            return RowChange::Remove(key);
        }
        if let Some(to) = self.renamed_to(cx) {
            return match self.form_value(cx) {
                Some(value) => RowChange::Rename {
                    from: key,
                    to,
                    value,
                },
                None => RowChange::None,
            };
        }
        match &self.value {
            ValueState::Untouched | ValueState::Binary { .. } | ValueState::Removed => {
                RowChange::None
            }
            ValueState::File { value, .. } => RowChange::Set(key, value.clone()),
            ValueState::Text { editor, baseline } => {
                let text = editor.text(cx);
                match baseline {
                    Some(baseline)
                        if self.existing.is_some() && baseline.expose() == text.as_bytes() =>
                    {
                        RowChange::None
                    }
                    _ => RowChange::Set(key, SecretBytes::from(text.as_str())),
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NewKind {
    Opaque,
    DockerRegistry,
    Tls,
}

impl NewKind {
    const ALL: [NewKind; 3] = [NewKind::Opaque, NewKind::DockerRegistry, NewKind::Tls];

    fn label(self) -> String {
        match self {
            Self::Opaque => "Opaque".to_owned(),
            Self::DockerRegistry => t!("editor-secret-kind-docker"),
            Self::Tls => "TLS".to_owned(),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Opaque => "secret-kind-Opaque",
            Self::DockerRegistry => "secret-kind-Docker registry",
            Self::Tls => "secret-kind-TLS",
        }
    }
}

/// A file picked for a TLS Secret.
struct PickedFile {
    name: String,
    value: SecretBytes,
}

struct CreateForm {
    name: Entity<InputState>,
    kind: NewKind,
    server: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    email: Entity<InputState>,
    cert: Option<PickedFile>,
    key: Option<PickedFile>,
}

/// What "Apply" sends, fixed at review time.
enum Pending {
    Patch(SecretPatch),
    Create { name: String, secret: NewSecret },
}

pub struct SecretEditor {
    target: SecretTarget,
    context: String,
    client: Client,
    cluster: Entity<Cluster>,
    info: Option<SecretInfo>,
    rows: Vec<KeyRow>,
    next_row: usize,
    create: Option<CreateForm>,
    review: Option<(Vec<ReviewLine>, Pending)>,
    /// Whether the user may patch (existing) or create (new) Secrets.
    access: Option<Access>,
    busy: Option<Busy>,
    banner: Option<Banner>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
    _operation: Task<()>,
}

impl SecretEditor {
    pub fn new(
        connection: Connection,
        target: SecretTarget,
        cluster: Entity<Cluster>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let create =
            matches!(target, SecretTarget::New { .. }).then(|| CreateForm::new(window, cx));
        let mut subscriptions = vec![cx.observe(&cluster, |this, _, cx| {
            if !this.allows(SecretAction::LoadValue, cx) {
                this.unload_values(cx);
            }
            cx.notify();
        })];
        if let Some(form) = &create {
            for input in [
                &form.name,
                &form.server,
                &form.username,
                &form.password,
                &form.email,
            ] {
                subscriptions.push(watch_input(input, cx));
            }
            let placeholders: [(&Entity<InputState>, Text); 3] = [
                (&form.username, || t!("editor-secret-user-placeholder")),
                (&form.password, || t!("editor-secret-password-placeholder")),
                (&form.email, || t!("editor-secret-optional")),
            ];
            for (input, text) in placeholders {
                subscriptions.extend(crate::i18n::localized_placeholder(input, text, window, cx));
            }
        }
        let mut this = Self {
            target,
            context: connection.context,
            client: connection.client,
            cluster,
            info: None,
            rows: Vec::new(),
            next_row: 0,
            create,
            review: None,
            access: None,
            busy: None,
            banner: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
            _tasks: Vec::new(),
            _operation: Task::ready(()),
        };
        this.check_access(cx);
        match &this.target {
            SecretTarget::Existing { .. } => this.load(cx),
            SecretTarget::New { .. } => this.add_key(window, cx),
        }
        this
    }

    pub fn context(&self) -> &str {
        &self.context
    }

    pub fn tab_title(&self) -> String {
        self.target.tab_title()
    }

    pub fn same_target(&self, context: &str, target: &SecretTarget) -> bool {
        self.context == context
            && matches!(target, SecretTarget::Existing { .. })
            && &self.target == target
    }

    pub fn focus_editor(&self, window: &mut Window, cx: &mut App) {
        match &self.create {
            Some(form) => form.name.update(cx, |input, cx| input.focus(window, cx)),
            None => window.focus(&self.focus, cx),
        }
    }

    /// Edits that were not applied.
    pub fn is_dirty(&self, cx: &App) -> bool {
        if let Some(form) = &self.create {
            return !form.name.read(cx).value().is_empty()
                || form.cert.is_some()
                || form.key.is_some()
                || !form.password.read(cx).value().is_empty()
                || self.rows.iter().any(
                    |row| matches!(row.change(cx), RowChange::Set(_, ref v) if !v.is_empty()),
                );
        }
        self.rows
            .iter()
            .any(|row| !matches!(row.change(cx), RowChange::None))
    }

    fn session_closed(&self, cx: &App) -> bool {
        !matches!(
            self.cluster.read(cx).state_of(&self.context),
            ConnectionState::Connected { .. }
        )
    }

    fn foreign_context(&self, cx: &App) -> bool {
        let active = self.cluster.read(cx).active().map(|c| c.name.clone());
        active.as_deref() != Some(self.context.as_str())
    }

    /// Whether the context's security settings allow `action`.
    fn allows(&self, action: SecretAction, cx: &App) -> bool {
        self.cluster.read(cx).allows_secret(&self.context, action)
    }

    /// Drops loaded values that were not changed (settings now hide
    /// values). Edited and replaced values stay: they are the user's input.
    fn unload_values(&mut self, cx: &App) {
        for row in &mut self.rows {
            row.loading = false;
            let loaded = match &row.value {
                ValueState::Binary { .. } => true,
                ValueState::Text {
                    baseline: Some(_), ..
                } => row.existing.is_some() && matches!(row.change(cx), RowChange::None),
                _ => false,
            };
            if loaded {
                row.value = ValueState::Untouched;
            }
        }
    }

    fn immutable(&self) -> bool {
        self.info.as_ref().is_some_and(|info| info.immutable)
    }

    fn check_access(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let (verb, name) = match &self.target {
            SecretTarget::Existing { name, .. } => ("patch", Some(name.clone())),
            SecretTarget::New { .. } => ("create", None),
        };
        let namespace = self.target.namespace().to_owned();
        let check = runtime::spawn(async move {
            secrets::can_i(&client, verb, &namespace, name.as_deref()).await
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let access = check
                .await
                .unwrap_or_else(|e| Access::Unknown(e.to_string()));
            this.update(cx, |this, cx| {
                this.access = Some(access);
                cx.notify();
            })
            .ok();
        }));
    }

    /// (Re)loads keys, sizes and type; drops all edits.
    fn load(&mut self, cx: &mut Context<Self>) {
        let SecretTarget::Existing { namespace, name } = &self.target else {
            return;
        };
        let client = self.client.clone();
        let (namespace, name) = (namespace.clone(), name.clone());
        let task =
            runtime::spawn(async move { secrets::inspect(&client, &namespace, &name).await });
        self.busy = Some(Busy::Loading);
        self.review = None;
        self._operation = cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(Ok(info)) => this.set_info(info),
                    Ok(Err(error)) => {
                        this.banner = Some(Banner::new(
                            Tone::Danger,
                            t!("editor-secret-load-failed", error = error.to_string()),
                        ))
                    }
                    Err(error) => this.banner = Some(Banner::new(Tone::Danger, error.to_string())),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn set_info(&mut self, info: SecretInfo) {
        self.rows = info
            .keys
            .iter()
            .map(|(key, size)| {
                self.next_row += 1;
                KeyRow {
                    id: self.next_row,
                    existing: Some((key.clone(), *size)),
                    name: None,
                    rename: None,
                    value: ValueState::Untouched,
                    loading: false,
                }
            })
            .collect();
        self.info = Some(info);
    }

    fn row_mut(&mut self, id: usize) -> Option<&mut KeyRow> {
        self.rows.iter_mut().find(|row| row.id == id)
    }

    fn add_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.next_row += 1;
        let id = self.next_row;
        let name = cx.new(|cx| InputState::new(window, cx));
        self._subscriptions
            .extend(crate::i18n::localized_placeholder(
                &name,
                || t!("editor-key-placeholder"),
                window,
                cx,
            ));
        let editor = self.new_value_editor(id, "", String::new(), window, cx);
        self._subscriptions.push(watch_key(id, &name, cx));
        name.update(cx, |name, cx| name.focus(window, cx));
        self.rows.push(KeyRow {
            id,
            existing: None,
            name: Some(name),
            rename: None,
            value: ValueState::Text {
                editor,
                baseline: None,
            },
            loading: false,
        });
        cx.notify();
    }

    /// A value editor for row `id`; its edits recheck the value.
    fn new_value_editor(
        &mut self,
        id: usize,
        key: &str,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ValueEditor {
        let editor = ValueEditor::new(key, text, window, cx);
        self._subscriptions.push(watch_key(id, &editor.state, cx));
        editor
    }

    /// The key or value of row `id` changed: pick the value's kind again.
    fn row_edited(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
            let key = row.final_key(cx);
            if let ValueState::Text { editor, .. } = &mut row.value {
                editor.refresh(&key, cx);
            }
        }
        cx.notify();
    }

    /// Shows a field to rename existing key `id`.
    fn start_rename(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter().find(|row| row.id == id) else {
            return;
        };
        let Some((key, _)) = row.existing.clone() else {
            return;
        };
        if row.rename.is_some() || !row.can_rename() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(key));
        self._subscriptions
            .extend(crate::i18n::localized_placeholder(
                &input,
                || t!("editor-secret-new-key-placeholder"),
                window,
                cx,
            ));
        input.update(cx, |input, cx| input.focus(window, cx));
        self._subscriptions.push(watch_key(id, &input, cx));
        if let Some(row) = self.row_mut(id) {
            row.rename = Some(input);
        }
        cx.notify();
    }

    fn format_value(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id)
            && let ValueState::Text { editor, .. } = &mut row.value
        {
            editor.format(window, cx);
        }
    }

    /// Fetches the current value of an existing key into the form.
    fn load_value(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let SecretTarget::Existing { namespace, name } = &self.target else {
            return;
        };
        let Some(row) = self.rows.iter_mut().find(|row| row.id == id) else {
            return;
        };
        let Some((key, _)) = row.existing.clone() else {
            return;
        };
        if row.loading {
            return;
        }
        if !self
            .cluster
            .read(cx)
            .allows_secret(&self.context, SecretAction::LoadValue)
        {
            return;
        }
        let Some(row) = self.rows.iter_mut().find(|row| row.id == id) else {
            return;
        };
        row.loading = true;
        let client = self.client.clone();
        let (namespace, name) = (namespace.clone(), name.clone());
        let wanted = key.clone();
        let fetch = runtime::spawn(async move {
            secrets::reveal(&client, &namespace, &name, &[wanted.as_str()]).await
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = fetch.await;
            this.update_in(cx, |this, window, cx| {
                let allowed = this.allows(SecretAction::LoadValue, cx);
                let Some(row) = this.row_mut(id) else {
                    return;
                };
                // Unloaded meanwhile (settings now hide values): drop it.
                if !row.loading || !allowed {
                    row.loading = false;
                    return;
                }
                row.loading = false;
                match result {
                    Ok(Ok(mut revealed)) => match revealed.values.remove(&key) {
                        Some(value) => {
                            if value.text().is_some() {
                                this.set_text_value(id, Some(value), window, cx);
                            } else if let Some(row) = this.row_mut(id) {
                                row.value = ValueState::Binary { len: value.len() };
                            }
                        }
                        None => {
                            this.banner = Some(Banner::new(
                                Tone::Warning,
                                t!("editor-secret-key-gone-reload", key = &key),
                            ))
                        }
                    },
                    Ok(Err(error)) => {
                        this.banner = Some(Banner::new(Tone::Danger, error.to_string()))
                    }
                    Err(error) => this.banner = Some(Banner::new(Tone::Danger, error.to_string())),
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    /// Puts a text field on row `id`, holding `baseline` (or empty).
    fn set_text_value(
        &mut self,
        id: usize,
        baseline: Option<SecretBytes>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = baseline
            .as_ref()
            .and_then(|b| b.text().map(str::to_owned))
            .unwrap_or_default();
        let key = self
            .rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.final_key(cx))
            .unwrap_or_default();
        let editor = self.new_value_editor(id, &key, text, window, cx);
        editor.state.update(cx, |state, cx| state.focus(window, cx));
        if let Some(row) = self.row_mut(id) {
            row.value = ValueState::Text { editor, baseline };
        }
        cx.notify();
    }

    fn pick_file(&mut self, id: usize, cx: &mut Context<Self>) {
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
                    Ok(file) => {
                        if let Some(row) = this.row_mut(id) {
                            row.value = ValueState::File {
                                name: file.name,
                                value: file.value,
                            };
                        }
                    }
                    Err(error) => this.banner = Some(Banner::new(Tone::Danger, error)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn pick_tls_file(&mut self, cert: bool, cx: &mut Context<Self>) {
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
                match (result, this.create.as_mut()) {
                    (Ok(file), Some(form)) => {
                        if cert {
                            form.cert = Some(file);
                        } else {
                            form.key = Some(file);
                        }
                    }
                    (Err(error), _) => this.banner = Some(Banner::new(Tone::Danger, error)),
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn remove_row(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(index) = self.rows.iter().position(|row| row.id == id) {
            if self.rows[index].existing.is_some() {
                self.rows[index].value = ValueState::Removed;
            } else {
                self.rows.remove(index);
            }
        }
        cx.notify();
    }

    fn undo_row(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.row_mut(id) {
            row.value = ValueState::Untouched;
            row.rename = None;
        }
        cx.notify();
    }

    /// Builds what would be sent and shows its review.
    fn start_review(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.banner = None;
        let pending = match self.build(cx) {
            Ok(pending) => pending,
            Err(message) => {
                self.banner = Some(Banner::new(Tone::Warning, message));
                cx.notify();
                return;
            }
        };
        let lines = match &pending {
            Pending::Patch(patch) => patch.review(),
            Pending::Create { secret, .. } => secret.review(),
        };
        self.review = Some((lines, pending));
        cx.notify();
    }

    fn build(&self, cx: &App) -> Result<Pending, String> {
        let changes: Vec<RowChange> = self.rows.iter().map(|row| row.change(cx)).collect();
        let Some(form) = &self.create else {
            if self.immutable() {
                return Err(t!("editor-secret-immutable-error"));
            }
            let info = self
                .info
                .as_ref()
                .ok_or_else(|| t!("editor-secret-not-loaded"))?;
            let mut patch = SecretPatch::new(
                info.keys.iter().map(|(key, _)| key.clone()),
                info.resource_version.clone(),
            );
            // Every key that stays has its own name.
            let mut names = std::collections::BTreeSet::new();
            for row in &self.rows {
                if matches!(row.value, ValueState::Removed) {
                    continue;
                }
                let name = row.final_key(cx);
                if !names.insert(name.clone()) {
                    return Err(t!("editor-key-twice", key = &name));
                }
            }
            // Removals first: a renamed key may take a removed key's name.
            for change in &changes {
                match change {
                    RowChange::Remove(key) | RowChange::Rename { from: key, .. } => {
                        patch.remove(key)
                    }
                    _ => {}
                }
            }
            for change in changes {
                match change {
                    RowChange::Set(key, value) | RowChange::Rename { to: key, value, .. } => {
                        secrets::validate_key(&key)?;
                        patch.set(key, value);
                    }
                    RowChange::None | RowChange::Remove(_) => {}
                }
            }
            if patch.is_empty() {
                return Err(t!("editor-no-changes-to-apply"));
            }
            return Ok(Pending::Patch(patch));
        };
        let name = form.name.read(cx).value().trim().to_owned();
        secrets::validate_name(&name)?;
        let secret = match form.kind {
            NewKind::Opaque => NewSecret::Opaque {
                entries: changes
                    .into_iter()
                    .filter_map(|change| match change {
                        RowChange::Set(key, value) => Some((key, value)),
                        _ => None,
                    })
                    .collect(),
            },
            NewKind::DockerRegistry => {
                let server = form.server.read(cx).value().trim().to_owned();
                NewSecret::DockerRegistry {
                    server: if server.is_empty() {
                        secrets::edit::DEFAULT_REGISTRY.to_owned()
                    } else {
                        server
                    },
                    username: form.username.read(cx).value().trim().to_owned(),
                    password: SecretBytes::from(form.password.read(cx).value().as_ref()),
                    email: Some(form.email.read(cx).value().trim().to_owned())
                        .filter(|e| !e.is_empty()),
                }
            }
            NewKind::Tls => NewSecret::Tls {
                cert: form
                    .cert
                    .as_ref()
                    .ok_or_else(|| t!("editor-secret-choose-cert"))?
                    .value
                    .clone(),
                key: form
                    .key
                    .as_ref()
                    .ok_or_else(|| t!("editor-secret-choose-key"))?
                    .value
                    .clone(),
            },
        };
        secret.validate()?;
        Ok(Pending::Create { name, secret })
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.session_closed(cx) {
            return;
        }
        let Some((_, pending)) = self.review.take() else {
            return;
        };
        let client = self.client.clone();
        let namespace = self.target.namespace().to_owned();
        // Detached: a write that was confirmed finishes even if the editor
        // closes meanwhile (ADR 0011).
        let (task, created) = match pending {
            Pending::Patch(patch) => {
                let SecretTarget::Existing { name, .. } = &self.target else {
                    return;
                };
                let name = name.clone();
                (
                    runtime::spawn_detached(async move {
                        secrets::apply_patch(&client, &namespace, &name, &patch).await
                    }),
                    None,
                )
            }
            Pending::Create { name, secret } => {
                let created = name.clone();
                (
                    runtime::spawn_detached(async move {
                        secrets::create(&client, &namespace, &name, &secret).await
                    }),
                    Some(created),
                )
            }
        };
        self.busy = Some(Busy::Applying);
        self._operation = cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(Ok(())) => {
                        if let Some(name) = created {
                            this.target = SecretTarget::Existing {
                                namespace: this.target.namespace().to_owned(),
                                name,
                            };
                            this.create = None;
                            this.rows.clear();
                            this.access = None;
                            this.check_access(cx);
                            this.banner =
                                Some(Banner::new(Tone::Success, t!("editor-secret-created")));
                        } else {
                            this.banner =
                                Some(Banner::new(Tone::Success, t!("editor-changes-applied")));
                        }
                        this.load(cx);
                    }
                    Ok(Err(error)) => {
                        this.banner = Some(Banner::new(Tone::Danger, error.to_string()))
                    }
                    Err(error) => this.banner = Some(Banner::new(Tone::Danger, error.to_string())),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn set_kind(&mut self, kind: NewKind, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.create {
            form.kind = kind;
            self.review = None;
            cx.notify();
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let closed = self.session_closed(cx);
        let busy = self.busy.is_some() || closed;
        let denied = matches!(self.access, Some(Access::Denied(_)));
        let subtitle = match &self.target {
            SecretTarget::Existing { .. } => {
                t!("editor-subtitle-edit-values", context = &self.context)
            }
            SecretTarget::New { .. } => {
                t!("editor-subtitle-create-secret", context = &self.context)
            }
        };
        let title = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(self.target.title()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(subtitle),
            );
        let actions = div().flex().items_center().gap_2();
        let actions = match &self.review {
            None => actions
                .when(
                    matches!(self.target, SecretTarget::Existing { .. }),
                    |row| {
                        row.child(
                            Button::new("secret-reload")
                                .ghost()
                                .small()
                                .icon(IconName::RefreshCw)
                                .label(t!("editor-reload"))
                                .tooltip(t!("editor-secret-reload-tooltip"))
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.banner = None;
                                    this.load(cx)
                                })),
                        )
                    },
                )
                .child(
                    Button::new("secret-review")
                        .primary()
                        .small()
                        .label(t!("editor-review"))
                        .disabled(busy || denied || self.immutable())
                        .on_click(cx.listener(|this, _, _, cx| this.start_review(cx))),
                ),
            Some(_) => actions
                .child(
                    Button::new("secret-back")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowLeft)
                        .label(t!("editor-back-to-editing"))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.review = None;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("secret-apply")
                        .primary()
                        .small()
                        .label(if self.create.is_some() {
                            t!("editor-create")
                        } else {
                            t!("editor-apply")
                        })
                        .spinning(self.busy == Some(Busy::Applying))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
                ),
        };
        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_4()
            .px_4()
            .py_2p5()
            .border_b_1()
            .border_color(theme.border)
            .child(title)
            .child(actions)
    }

    fn render_notices(&self, cx: &App) -> Vec<AnyElement> {
        let mut notices = Vec::new();
        if let Some((tone, text)) = context_notice(
            self.foreign_context(cx),
            self.session_closed(cx),
            &self.context,
        ) {
            notices.push(callout(tone, text, cx).into_any_element());
        }
        if let Some(Access::Denied(message)) = &self.access {
            notices.push(callout(Tone::Warning, message.clone(), cx).into_any_element());
        }
        if matches!(self.target, SecretTarget::Existing { .. })
            && !self.allows(SecretAction::LoadValue, cx)
        {
            notices
                .push(callout(Tone::Info, t!("editor-secret-hidden-load"), cx).into_any_element());
        }
        if self.immutable() {
            notices.push(
                callout(Tone::Info, t!("editor-secret-immutable-notice"), cx).into_any_element(),
            );
        }
        if let Some(banner) = &self.banner {
            notices.push(callout(banner.tone, banner.text.clone(), cx).into_any_element());
        }
        notices
    }

    fn render_review(&self, lines: &[ReviewLine], cx: &App) -> AnyElement {
        let theme = cx.theme();
        let list = lines.iter().fold(KeyValueList::new(), |list, line| {
            list.row(line.key.clone(), change_placeholder(line.change))
        });
        let what = match &self.target {
            SecretTarget::Existing { .. } => t!("editor-secret-review-patch"),
            SecretTarget::New { .. } => {
                let kind = self
                    .create
                    .as_ref()
                    .map_or("Opaque", |form| match form.kind {
                        NewKind::Opaque => typed::TYPE_OPAQUE,
                        NewKind::DockerRegistry => typed::TYPE_DOCKER_CONFIG_JSON,
                        NewKind::Tls => typed::TYPE_TLS,
                    });
                t!("editor-secret-review-create", schema_type = kind)
            }
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                Panel::new()
                    .title(t!("editor-secret-changes-to", title = self.target.title()))
                    .child(list),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(what),
            )
            .into_any_element()
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let editable = !self.immutable() && self.busy.is_none();
        let mut column = div().flex().flex_col().gap_3();
        if self.rows.is_empty() && self.busy != Some(Busy::Loading) {
            column = column.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(t!("editor-no-keys")),
            );
        }
        let names: Vec<Option<String>> = self
            .rows
            .iter()
            .map(|row| (!matches!(row.value, ValueState::Removed)).then(|| row.final_key(cx)))
            .collect();
        for (ix, row) in self.rows.iter().enumerate() {
            let twice = names[ix].as_ref().is_some_and(|name| {
                names
                    .iter()
                    .enumerate()
                    .any(|(other, n)| other != ix && n.as_ref() == Some(name))
            });
            column = column.child(self.render_row(row, twice, editable, cx));
        }
        if editable {
            column = column.child(
                div().child(
                    Button::new("secret-add-key")
                        .outline()
                        .small()
                        .icon(IconName::Plus)
                        .label(t!("editor-add-key"))
                        .on_click(cx.listener(|this, _, window, cx| this.add_key(window, cx))),
                ),
            );
        }
        column.into_any_element()
    }

    fn render_row(
        &self,
        row: &KeyRow,
        twice: bool,
        editable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id;
        let change = row.change(cx);
        let (status, tone): (String, Tone) = match (&row.existing, &row.value, &change) {
            (None, _, _) => (t!("editor-key-new"), Tone::Success),
            (_, ValueState::Removed, _) => (t!("editor-key-removed"), Tone::Danger),
            (Some((key, _)), _, RowChange::Rename { .. }) => {
                (t!("editor-key-renamed", key = key), Tone::Info)
            }
            (_, _, RowChange::Set(..)) => (t!("editor-key-changed"), Tone::Info),
            (Some((_, size)), ValueState::Untouched, _) => (size_text(*size), Tone::Neutral),
            _ => (t!("editor-key-unchanged"), Tone::Neutral),
        };
        let removed = matches!(row.value, ValueState::Removed);
        let mut block = match (&row.existing, &row.name, &row.rename) {
            (Some(_), _, Some(rename)) => DataKeyBlock::new(
                ("secret-key", id),
                div()
                    .w(px(260.))
                    .child(Input::new(rename).small().disabled(!editable)),
            ),
            (Some((key, _)), _, None) => {
                DataKeyBlock::labeled(("secret-key", id), key.clone(), removed, cx)
            }
            (None, Some(input), _) => DataKeyBlock::new(
                ("secret-key", id),
                div()
                    .w(px(260.))
                    .child(Input::new(input).small().disabled(!editable)),
            ),
            (None, None, _) => DataKeyBlock::new(("secret-key", id), div()),
        }
        .status(status, tone);
        if let ValueState::Text { editor, .. } = &row.value {
            block = block.kind(content_label(editor.kind));
        }
        if editable {
            let existing = row.existing.is_some();
            match &row.value {
                ValueState::Untouched => {
                    let load_allowed = self.allows(SecretAction::LoadValue, cx);
                    block = block
                        .action(
                            Button::new(("secret-load", id))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Eye)
                                .label(t!("editor-secret-load-value"))
                                .tooltip(if load_allowed {
                                    t!("editor-secret-load-value-tooltip")
                                } else {
                                    t!("editor-secret-hidden")
                                })
                                .disabled(!load_allowed)
                                .spinning(row.loading)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.load_value(id, window, cx)
                                })),
                        )
                        .action(
                            Button::new(("secret-replace", id))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Replace)
                                .label(t!("editor-secret-replace"))
                                .tooltip(t!("editor-secret-replace-tooltip"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_text_value(id, None, window, cx)
                                })),
                        );
                }
                ValueState::Text { editor, .. } if editor.kind == ContentKind::Json => {
                    block = block.action(
                        Button::new(("secret-format", id))
                            .ghost()
                            .xsmall()
                            .icon(Lucide::Braces)
                            .label(t!("editor-format"))
                            .tooltip(t!("editor-format-tooltip"))
                            .disabled(!editor.can_format())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.format_value(id, window, cx)
                            })),
                    );
                }
                _ => {}
            }
            if existing && !removed && row.rename.is_none() {
                block = block.action(
                    Button::new(("secret-rename", id))
                        .ghost()
                        .xsmall()
                        .icon(Lucide::PencilLine)
                        .tooltip(if row.can_rename() {
                            t!("editor-secret-rename-tooltip")
                        } else {
                            t!("editor-secret-rename-disabled")
                        })
                        .disabled(!row.can_rename())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_rename(id, window, cx)
                        })),
                );
            }
            if existing && !matches!(row.value, ValueState::Untouched | ValueState::Removed) {
                block = block.action(
                    Button::new(("secret-undo", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Undo2)
                        .tooltip(t!("editor-secret-undo-tooltip"))
                        .on_click(cx.listener(move |this, _, _, cx| this.undo_row(id, cx))),
                );
            }
            if !removed {
                block = block.action(
                    Button::new(("secret-file", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::File)
                        .tooltip(t!("editor-secret-file-tooltip"))
                        .on_click(cx.listener(move |this, _, _, cx| this.pick_file(id, cx))),
                );
            }
            block = if removed {
                block.action(
                    Button::new(("secret-restore", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Undo2)
                        .label(t!("editor-secret-keep"))
                        .on_click(cx.listener(move |this, _, _, cx| this.undo_row(id, cx))),
                )
            } else {
                block.action(
                    Button::new(("secret-remove", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Delete)
                        .tooltip(t!("editor-remove-key"))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_row(id, cx))),
                )
            };
        }
        let muted = cx.theme().muted_foreground;
        let note = |text: String| div().text_xs().text_color(muted).child(text);
        block = match &row.value {
            ValueState::Untouched | ValueState::Removed => block,
            ValueState::Text { editor, .. } => block.body(editor.element(!editable, cx)),
            ValueState::File { name, value } => block.body(note(t!(
                "editor-secret-from-file",
                name = name,
                size = size_text(value.len())
            ))),
            ValueState::Binary { len } => {
                block.body(note(t!("editor-secret-binary", binary = binary_text(*len))))
            }
        };
        let key = row.final_key(cx);
        let name_problem = (!removed && (row.existing.is_none() || row.rename.is_some()))
            .then(|| secrets::validate_key(&key).err().map(|e| e.to_string()))
            .flatten()
            .or_else(|| twice.then(|| t!("editor-key-twice", key = &key)));
        match (&row.value, name_problem) {
            (_, Some(problem)) => block.problem(Some(problem), Tone::Danger),
            (ValueState::Text { editor, .. }, None) => {
                block.problem(editor.warning(), Tone::Warning)
            }
            _ => block,
        }
        .into_any_element()
    }

    fn render_create(&self, form: &CreateForm, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let kinds = NewKind::ALL.iter().map(|&kind| {
            Button::new(kind.id())
                .small()
                .label(kind.label())
                .map(|b| {
                    if form.kind == kind {
                        b.primary()
                    } else {
                        b.outline()
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_kind(kind, cx)))
        });
        let header = KeyValueList::new()
            .row(
                t!("editor-field-name"),
                div().w(px(320.)).child(Input::new(&form.name).small()),
            )
            .row("Namespace", self.target.namespace().to_owned())
            .row(
                t!("editor-field-type"),
                div().flex().gap_1().children(kinds),
            );
        let body: AnyElement = match form.kind {
            NewKind::Opaque => self.render_rows(cx),
            NewKind::DockerRegistry => KeyValueList::new()
                .row(t!("editor-field-server"), Input::new(&form.server).small())
                .row(t!("editor-field-user"), Input::new(&form.username).small())
                .row(
                    t!("editor-field-password"),
                    Input::new(&form.password).small(),
                )
                .row(t!("editor-field-email"), Input::new(&form.email).small())
                .into_any_element(),
            NewKind::Tls => {
                let cert = form.cert.as_ref().map(|file| {
                    typed::certificates(file.value.expose())
                        .ok()
                        .and_then(|chain| chain.into_iter().next())
                        .map(|cert| {
                            t!(
                                "editor-secret-cert-file",
                                file = &file.name,
                                subject = cert.subject,
                                until = secrets::x509::format_time(cert.not_after)
                            )
                        })
                        .unwrap_or_else(|| t!("editor-secret-cert-not-pem", file = &file.name))
                });
                let key = form
                    .key
                    .as_ref()
                    .map(|file| t!("editor-secret-key-file", file = &file.name));
                KeyValueList::new()
                    .row(
                        t!("editor-field-certificate"),
                        file_picker(
                            "secret-tls-cert",
                            cert,
                            cx.listener(|this, _, _, cx| this.pick_tls_file(true, cx)),
                        ),
                    )
                    .row(
                        t!("editor-field-private-key"),
                        file_picker(
                            "secret-tls-key",
                            key,
                            cx.listener(|this, _, _, cx| this.pick_tls_file(false, cx)),
                        ),
                    )
                    .into_any_element()
            }
        };
        let hint = match form.kind {
            NewKind::Opaque => t!("editor-secret-hint-opaque"),
            NewKind::DockerRegistry => t!("editor-secret-hint-docker"),
            NewKind::Tls => t!("editor-secret-hint-tls"),
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(Panel::new().child(header))
            .child(Panel::new().title(form.kind.label()).child(body))
            .child(div().text_xs().text_color(muted).child(hint))
            .into_any_element()
    }
}

impl CreateForm {
    fn new(window: &mut Window, cx: &mut Context<SecretEditor>) -> Self {
        let input =
            |placeholder: &'static str, window: &mut Window, cx: &mut Context<SecretEditor>| {
                cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
            };
        Self {
            name: input("my-secret", window, cx),
            kind: NewKind::Opaque,
            server: input(secrets::edit::DEFAULT_REGISTRY, window, cx),
            username: cx.new(|cx| InputState::new(window, cx)),
            password: cx.new(|cx| InputState::new(window, cx).masked(true)),
            email: cx.new(|cx| InputState::new(window, cx)),
            cert: None,
            key: None,
        }
    }
}

/// What the review shows in place of a value.
fn change_placeholder(change: KeyChange) -> String {
    match change {
        KeyChange::Added => t!("editor-secret-hidden-added"),
        KeyChange::Changed => t!("editor-secret-hidden-changed"),
        KeyChange::Removed => t!("editor-secret-removed"),
    }
}

fn file_picker(
    id: &'static str,
    chosen: Option<String>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .child(
            Button::new(id)
                .outline()
                .xsmall()
                .icon(IconName::FolderOpen)
                .label(t!("ui-secret-choose-file"))
                .on_click(on_click),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_xs()
                .child(chosen.unwrap_or_else(|| t!("ui-secret-no-file"))),
        )
}

fn watch_input(input: &Entity<InputState>, cx: &mut Context<SecretEditor>) -> Subscription {
    cx.subscribe(input, |_, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    })
}

/// Edits of row `id`'s key or value recheck the value.
fn watch_key<T: EventEmitter<InputEvent>>(
    id: usize,
    input: &Entity<T>,
    cx: &mut Context<SecretEditor>,
) -> Subscription {
    cx.subscribe(input, move |this, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            this.row_edited(id, cx);
        }
    })
}

fn read_file(path: &Path) -> Result<PickedFile, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let size = std::fs::metadata(path)
        .map_err(|e| unreadable(&name, &e))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(t!("editor-file-too-large-secret", name = &name));
    }
    let bytes = std::fs::read(path).map_err(|e| unreadable(&name, &e))?;
    Ok(PickedFile {
        name,
        value: SecretBytes::new(bytes),
    })
}

impl Focusable for SecretEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SecretEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let body = match (&self.review, &self.create) {
            (Some((lines, _)), _) => self.render_review(lines, cx),
            (None, Some(form)) => self.render_create(form, cx),
            (None, None) if self.busy == Some(Busy::Loading) && self.info.is_none() => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t!("editor-loading"))
                .into_any_element(),
            (None, None) => {
                let info = self.info.as_ref().map(|info| {
                    KeyValueList::new()
                        .row(t!("editor-field-type"), info.secret_type.clone())
                        .row(t!("editor-field-keys"), info.keys.len().to_string())
                });
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .children(info.map(|info| Panel::new().child(info)))
                    .child(self.render_rows(cx))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .whitespace_normal()
                            .child(t!("editor-secret-edit-hint")),
                    )
                    .into_any_element()
            }
        };
        div()
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .text_color(theme.foreground)
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .id("secret-editor-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .max_w(FORM_WIDTH)
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .children(self.render_notices(cx))
                            .child(body),
                    ),
            )
    }
}
