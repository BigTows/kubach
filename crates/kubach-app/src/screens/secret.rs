//! Secret values in the details panel: keys from the cache (without
//! values), and a "Show" button per key that fetches that one value with a
//! fresh `GET` at the moment of the click. Shown values hide themselves
//! after [`REVEAL_TTL`], and when the user selects another object or leaves
//! the screen. A copied value has its own clock: it is cleared from the
//! clipboard after [`CLIPBOARD_TTL`], when the vault locks or the app quits,
//! if it is still there (see [`crate::secret_clipboard`]).
//!
//! When the context's security settings hide Secret values (#96), Show and
//! Copy are unavailable and shown values are hidden at once; keys, sizes
//! and the typed summary (never passwords or keys) stay.
//!
//! Values live only in this entity (as [`SecretBytes`]) and in the text
//! being rendered; they never go to the cache, a log or an error message.

use kubach_ui::ButtonLoading as _;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::runtime;
use kubach_core::secrets::{
    self, Access, CLIPBOARD_TTL, CertificateInfo, Decoded, RegistryLogin, SecretBytes, Summary,
    TypedKind, Validity, x509,
};
use kubach_core::settings::SecretAction;
use kubach_i18n::t;
use kubach_ui::{KeyValueList, Panel, Tone};
use kube::Client;
use kube::api::DynamicObject;

use crate::cluster::Cluster;
use crate::editor::{binary_text, size_text};
use crate::secret_clipboard;
use crate::settings::{SettingsTarget, open_settings};

/// How long a shown value stays visible.
pub const REVEAL_TTL: Duration = Duration::from_secs(30);
/// Longest text shown inline; the rest is still copied.
const SHOWN_MAX_CHARS: usize = 4000;

/// Which Secret a [`SecretValues`] shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretId {
    pub context: String,
    pub namespace: String,
    pub name: String,
}

impl SecretId {
    pub fn of(context: &str, object: &DynamicObject) -> Option<Self> {
        Some(Self {
            context: context.to_owned(),
            namespace: object.metadata.namespace.clone()?,
            name: object.metadata.name.clone()?,
        })
    }
}

/// A value on screen until its timer hides it.
struct Shown {
    value: SecretBytes,
    _timer: Task<()>,
}

enum SummaryState {
    Loading,
    Ready(Summary),
    Failed(String),
}

pub struct SecretValues {
    id: SecretId,
    client: Client,
    /// Source of the context's security settings.
    cluster: Entity<Cluster>,
    /// The cached object (keys, type; no values).
    object: Arc<DynamicObject>,
    /// `get` permission on this Secret; `None` while being checked.
    access: Option<Access>,
    shown: BTreeMap<String, Shown>,
    loading: BTreeSet<String>,
    /// The key whose value was last copied (while it is shown).
    copied: Option<String>,
    summary: Option<SummaryState>,
    error: Option<String>,
    _tasks: Vec<Task<()>>,
    _cluster: Subscription,
}

impl SecretValues {
    pub fn new(
        id: SecretId,
        client: Client,
        cluster: Entity<Cluster>,
        object: Arc<DynamicObject>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Settings changed to hide values: hide what is shown now.
        let observe = cx.observe(&cluster, |this, _, cx| {
            if !this.allows(SecretAction::Show, cx) {
                this.hide_values(cx);
            }
        });
        let check = {
            let client = client.clone();
            let (namespace, name) = (id.namespace.clone(), id.name.clone());
            runtime::spawn(
                async move { secrets::can_i(&client, "get", &namespace, Some(&name)).await },
            )
        };
        let task = cx.spawn(async move |this, cx| {
            let access = check
                .await
                .unwrap_or_else(|error| Access::Unknown(error.to_string()));
            this.update(cx, |this, cx| {
                this.access = Some(access);
                cx.notify();
            })
            .ok();
        });
        Self {
            id,
            client,
            cluster,
            object,
            access: None,
            shown: BTreeMap::new(),
            loading: BTreeSet::new(),
            copied: None,
            summary: None,
            error: None,
            _tasks: vec![task],
            _cluster: observe,
        }
    }

    pub fn id(&self) -> &SecretId {
        &self.id
    }

    /// The cached object changed (keys added or removed).
    pub fn set_object(&mut self, object: Arc<DynamicObject>) {
        self.object = object;
    }

    fn may_reveal(&self) -> bool {
        self.access.as_ref().is_some_and(Access::allowed)
    }

    /// Whether the context's security settings allow `action`.
    fn allows(&self, action: SecretAction, cx: &App) -> bool {
        self.cluster
            .read(cx)
            .allows_secret(&self.id.context, action)
    }

    /// Fetches `key` now and shows it for [`REVEAL_TTL`].
    fn reveal(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.may_reveal()
            || !self.allows(SecretAction::Show, cx)
            || !self.loading.insert(key.clone())
        {
            return;
        }
        self.error = None;
        let client = self.client.clone();
        let (namespace, name) = (self.id.namespace.clone(), self.id.name.clone());
        let wanted = key.clone();
        let fetch = runtime::spawn(async move {
            secrets::reveal(&client, &namespace, &name, &[wanted.as_str()]).await
        });
        let task = cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| {
                // Hidden (or left) meanwhile: drop the value unseen.
                if !this.loading.remove(&key) || !this.allows(SecretAction::Show, cx) {
                    return;
                }
                match result {
                    Ok(Ok(mut revealed)) => match revealed.values.remove(&key) {
                        Some(value) => this.show(key, value, cx),
                        None => this.error = Some(t!("editor-secret-key-gone", key = &key)),
                    },
                    Ok(Err(error)) => this.error = Some(error.to_string()),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn show(&mut self, key: String, value: SecretBytes, cx: &mut Context<Self>) {
        let hide_key = key.clone();
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REVEAL_TTL).await;
            this.update(cx, |this, cx| this.hide(&hide_key, cx)).ok();
        });
        self.shown.insert(
            key,
            Shown {
                value,
                _timer: timer,
            },
        );
    }

    /// Hides `key`. A copied value stays in the clipboard until its own
    /// timer ([`CLIPBOARD_TTL`]).
    fn hide(&mut self, key: &str, cx: &mut Context<Self>) {
        self.loading.remove(key);
        self.shown.remove(key);
        if self.copied.as_deref() == Some(key) {
            self.copied = None;
        }
        cx.notify();
    }

    /// Hides every value, also those still loading.
    fn hide_values(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<String> = self.shown.keys().cloned().collect();
        for key in keys {
            self.hide(&key, cx);
        }
        self.loading.clear();
        cx.notify();
    }

    /// Hides every value and the summary (selection changed, screen left).
    pub fn conceal(&mut self, cx: &mut Context<Self>) {
        self.hide_values(cx);
        self.summary = None;
        cx.notify();
    }

    fn copy(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.allows(SecretAction::Copy, cx) {
            return;
        }
        let Some(text) = self.shown.get(key).and_then(|s| s.value.text()) else {
            return;
        };
        secret_clipboard::copy(text, cx);
        self.copied = Some(key.to_owned());
        cx.notify();
    }

    /// Fetches only the keys a summary needs (never passwords or private
    /// keys) and keeps the summary, not the values.
    fn load_summary(&mut self, kind: TypedKind, cx: &mut Context<Self>) {
        if !self.may_reveal() || matches!(self.summary, Some(SummaryState::Loading)) {
            return;
        }
        let client = self.client.clone();
        let (namespace, name) = (self.id.namespace.clone(), self.id.name.clone());
        let fetch = runtime::spawn(async move {
            let revealed = secrets::reveal(&client, &namespace, &name, kind.source_keys())
                .await
                .map_err(|e| e.to_string())?;
            secrets::typed::summarize(kind, &revealed.values).map_err(String::from)
        });
        self.summary = Some(SummaryState::Loading);
        let task = cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| {
                if this.summary.is_none() {
                    return;
                }
                this.summary = Some(match result {
                    Ok(Ok(summary)) => SummaryState::Ready(summary),
                    Ok(Err(error)) => SummaryState::Failed(error),
                    Err(error) => SummaryState::Failed(error.to_string()),
                });
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    /// The panels for the cached Secret (without values).
    fn render_panels(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let object = self.object.clone();
        let secret_type = object
            .data
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or(secrets::typed::TYPE_OPAQUE)
            .to_owned();
        let immutable = object
            .data
            .get("immutable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let keys = data_keys(&object);
        let mut panels = Vec::new();
        if let Some(kind) = TypedKind::of(&secret_type) {
            panels.push(self.render_summary(kind, cx).into_any_element());
        }
        panels.push(
            self.render_values(&secret_type, immutable, keys, cx)
                .into_any_element(),
        );
        panels
    }

    fn render_values(
        &self,
        secret_type: &str,
        immutable: bool,
        keys: Vec<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let note = match &self.access {
            None => Some((Tone::Neutral, t!("editor-secret-checking-access"))),
            Some(Access::Denied(message)) => {
                Some((Tone::Warning, t!("editor-secret-denied", message = message)))
            }
            Some(Access::Unknown(_)) | Some(Access::Allowed) => None,
        };
        let mut panel = Panel::new().title(t!("editor-secret-data"));
        let mut info = KeyValueList::new().row(t!("editor-field-type"), secret_type.to_owned());
        if immutable {
            info = info.row(
                t!("editor-secret-immutable"),
                t!("editor-secret-immutable-yes"),
            );
        }
        panel = panel.child(info);
        if let Some((tone, text)) = note {
            panel = panel.child(note_line(tone, text, cx));
        }
        if !self.allows(SecretAction::Show, cx) {
            panel = panel.child(self.render_hidden_note(cx));
        }
        if let Some(error) = &self.error {
            panel = panel.child(note_line(Tone::Danger, error.clone(), cx));
        }
        if keys.is_empty() {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(t!("editor-no-keys")),
            );
        }
        for (ix, key) in keys.into_iter().enumerate() {
            panel = panel.child(self.render_key(ix, key, cx));
        }
        panel.child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(t!(
                    "editor-secret-ttl-note",
                    reveal = REVEAL_TTL.as_secs(),
                    clipboard = CLIPBOARD_TTL.as_secs()
                )),
        )
    }

    /// Why Show is unavailable, with a way to the setting.
    fn render_hidden_note(&self, cx: &App) -> impl IntoElement + use<> {
        let context = self.id.context.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().min_w_0().child(note_line(
                Tone::Info,
                t!("editor-secret-hidden-keys"),
                cx,
            )))
            .child(
                Button::new("secret-security-settings")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Settings2)
                    .label(t!("editor-secret-security-settings"))
                    .on_click(move |_, _, cx| {
                        open_settings(SettingsTarget::security(context.clone()), cx)
                    }),
            )
    }

    fn render_key(&self, ix: usize, key: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let shown = self.shown.get(&key);
        let loading = self.loading.contains(&key);
        let hidden_by_policy = !self.allows(SecretAction::Show, cx);
        let allowed = self.may_reveal() && !hidden_by_policy;
        let toggle_key = key.clone();
        let toggle = Button::new(("secret-toggle", ix))
            .ghost()
            .xsmall()
            .icon(if shown.is_some() {
                IconName::EyeOff
            } else {
                IconName::Eye
            })
            .label(if shown.is_some() {
                t!("editor-secret-hide")
            } else {
                t!("editor-secret-show")
            })
            .spinning(loading)
            .disabled(!allowed && shown.is_none())
            .when(hidden_by_policy, |button| {
                button.tooltip(t!("editor-secret-hidden"))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.shown.contains_key(&toggle_key) {
                    this.hide(&toggle_key, cx);
                } else {
                    this.reveal(toggle_key.clone(), cx);
                }
            }));
        let copy_allowed = self.allows(SecretAction::Copy, cx);
        let copy = shown
            .and_then(|s| s.value.text())
            .filter(|_| copy_allowed)
            .map(|_| {
                let copy_key = key.clone();
                let copied = self.copied.as_deref() == Some(key.as_str());
                Button::new(("secret-copy", ix))
                    .ghost()
                    .xsmall()
                    .icon(if copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .tooltip(t!(
                        "editor-secret-copy-tooltip",
                        seconds = CLIPBOARD_TTL.as_secs()
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| this.copy(&copy_key, cx)))
            });
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(key.clone()),
            )
            .when_some(shown, |row, shown| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(size_text(shown.value.len())),
                )
            })
            .children(copy)
            .child(toggle);
        let body = match shown.map(|s| s.value.decode()) {
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("••••••"),
            Some(Decoded::Binary { len }) => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(binary_text(len)),
            Some(Decoded::Text("")) => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t!("editor-secret-empty")),
            Some(Decoded::Text(text)) => div()
                .px_2()
                .py_1p5()
                .rounded_md()
                .bg(theme.muted)
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .text_color(theme.foreground)
                .whitespace_normal()
                .child(clip_text(text)),
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn render_summary(&self, kind: TypedKind, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let title = match kind {
            TypedKind::Tls => t!("editor-secret-summary-tls"),
            TypedKind::BasicAuth => t!("editor-secret-summary-basic-auth"),
            TypedKind::DockerConfigJson | TypedKind::DockerCfg => {
                t!("editor-secret-summary-registry")
            }
        };
        let mut panel = Panel::new().title(title);
        match &self.summary {
            None => {
                let what = match kind {
                    TypedKind::Tls => t!("editor-secret-summary-what-tls"),
                    TypedKind::BasicAuth => t!("editor-secret-summary-what-basic-auth"),
                    _ => t!("editor-secret-summary-what-registry"),
                };
                panel = panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .whitespace_normal()
                                .child(what),
                        )
                        .child(
                            Button::new("secret-summary")
                                .outline()
                                .xsmall()
                                .label(t!("editor-secret-show-summary"))
                                .disabled(!self.may_reveal())
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.load_summary(kind, cx)),
                                ),
                        ),
                );
            }
            Some(SummaryState::Loading) => {
                panel = panel.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(t!("editor-loading")),
                );
            }
            Some(SummaryState::Failed(error)) => {
                panel = panel.child(note_line(Tone::Danger, error.clone(), cx));
            }
            Some(SummaryState::Ready(summary)) => {
                panel = match summary {
                    Summary::Registries(logins) => panel.children(
                        logins
                            .iter()
                            .map(|login| registry_row(login, cx).into_any_element())
                            .collect::<Vec<_>>(),
                    ),
                    Summary::Certificates(chain) => {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map_or(0, |d| d.as_secs() as i64);
                        let mut panel = panel;
                        for (ix, cert) in chain.iter().enumerate() {
                            panel = panel.child(certificate_rows(ix, chain.len(), cert, now, cx));
                        }
                        panel.child(KeyValueList::new().row(
                            t!("editor-field-private-key"),
                            t!("editor-secret-tls-key-hidden"),
                        ))
                    }
                    Summary::BasicAuth { username } => panel.child(
                        KeyValueList::new()
                            .row(
                                t!("editor-field-user"),
                                username.clone().unwrap_or_else(|| "—".into()),
                            )
                            .row(
                                t!("editor-field-password"),
                                t!("editor-secret-value-hidden"),
                            ),
                    ),
                };
            }
        }
        panel
    }
}

fn registry_row(login: &RegistryLogin, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let mut list = KeyValueList::new().row(
        t!("editor-field-user"),
        login.username.clone().unwrap_or_else(|| "—".into()),
    );
    if let Some(email) = &login.email {
        list = list.row(t!("editor-field-email"), email.clone());
    }
    list = list.row(
        t!("editor-field-password"),
        if login.has_password {
            t!("editor-secret-password-set")
        } else {
            t!("editor-secret-password-none")
        },
    );
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(login.registry.clone()),
        )
        .child(list)
}

fn certificate_rows(
    ix: usize,
    total: usize,
    cert: &CertificateInfo,
    now: i64,
    cx: &App,
) -> impl IntoElement + use<> {
    let (tone, validity) = match cert.validity(now) {
        Validity::NotYetValid => (Tone::Warning, t!("editor-cert-not-yet-valid")),
        Validity::Expired { days_ago } => {
            (Tone::Danger, t!("editor-cert-expired", days = days_ago))
        }
        Validity::Valid { days_left } if days_left < 30 => {
            (Tone::Warning, t!("editor-cert-expires", days = days_left))
        }
        Validity::Valid { days_left } => (Tone::Success, t!("editor-cert-valid", days = days_left)),
    };
    let title = match (ix, total) {
        (_, 1) => None,
        (0, _) => Some(t!("editor-cert-leaf")),
        (ix, _) => Some(t!("editor-cert-chain", index = ix)),
    };
    let mut list = KeyValueList::new().row(t!("editor-cert-subject"), wrap(cert.subject.clone()));
    if !cert.self_signed() {
        list = list.row(t!("editor-cert-issuer"), wrap(cert.issuer.clone()));
    } else {
        list = list.row(t!("editor-cert-issuer"), t!("editor-cert-self-signed"));
    }
    if !cert.san.is_empty() {
        list = list.row("SAN", wrap(cert.san.join(", ")));
    }
    list = list
        .row(
            t!("editor-cert-valid-from"),
            x509::format_time(cert.not_before),
        )
        .row(
            t!("editor-cert-valid-until"),
            x509::format_time(cert.not_after),
        )
        .row(
            t!("editor-cert-status"),
            div().text_color(tone.color(cx)).child(validity),
        );
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .when_some(title, |el, title| {
            el.child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(title),
            )
        })
        .child(list)
}

fn wrap(text: String) -> Div {
    div().whitespace_normal().child(text)
}

fn note_line(tone: Tone, text: String, cx: &App) -> impl IntoElement + use<> {
    div()
        .text_xs()
        .whitespace_normal()
        .text_color(tone.color(cx))
        .child(text)
}

/// Names of a Secret's data keys (values are never in the cache).
pub fn data_keys(object: &DynamicObject) -> Vec<String> {
    object
        .data
        .get("data")
        .and_then(|data| data.as_object())
        .map(|data| data.keys().cloned().collect())
        .unwrap_or_default()
}

fn clip_text(text: &str) -> String {
    if text.chars().count() <= SHOWN_MAX_CHARS {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(SHOWN_MAX_CHARS).collect();
    clipped.push('\n');
    clipped.push_str(&t!("editor-secret-truncated"));
    clipped
}

impl Render for SecretValues {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.render_panels(cx))
    }
}

impl Drop for SecretValues {
    fn drop(&mut self) {
        // Values are wiped by `SecretBytes`; a copied value is cleared by
        // `secret_clipboard`, which outlives this entity.
        self.shown.clear();
    }
}
