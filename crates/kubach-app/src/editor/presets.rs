//! Templates and presets for new resources (#320): what "+" opens on the
//! page of a kind, the menu of every template and preset beside it, and
//! "Save as preset" in the editor.

use std::ops::Range;

use gpui_kit::component::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::*;
use kubach_core::settings::{Presets, ResourcePreset};
use kubach_core::templates::{self, Template};
use kubach_core::{ApiCatalog, ResourceKind};
use kubach_i18n::t;
use kubach_ui::{SplitButton, Tone};
use kube::api::GroupVersionKind;

use super::{Banner, Target, YamlEditor, open};
use crate::cluster::{CatalogState, Cluster};
use crate::settings::AppSettings;

/// Text a new-resources editor starts with.
#[derive(Clone, Debug, PartialEq)]
pub struct NewText {
    pub text: String,
    /// Kind of the first document, for the tab title.
    pub kind: Option<String>,
    /// Bytes to select: the name, to type over it.
    pub select: Option<Range<usize>>,
    /// Made without a schema: rebuilt once the schema arrives, if the text
    /// is still untouched.
    pub skeleton: Option<Skeleton>,
}

/// What a schema skeleton is built from.
#[derive(Clone, Debug, PartialEq)]
pub struct Skeleton {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
}

impl Skeleton {
    pub fn gvk(&self) -> GroupVersionKind {
        let (group, version) = self
            .api_version
            .rsplit_once('/')
            .unwrap_or(("", &self.api_version));
        GroupVersionKind::gvk(group, version, &self.kind)
    }
}

impl NewText {
    /// `text` (with placeholders) for a new object of `kind`.
    fn render(text: &str, kind: &str, namespace: Option<&str>) -> Self {
        let name = templates::default_name(kind);
        let text = templates::render(text, &name, namespace);
        Self {
            select: templates::name_range(&text, &name),
            text,
            kind: Some(kind.to_owned()),
            skeleton: None,
        }
    }

    fn builtin(template: &Template, namespace: &str) -> Self {
        let namespace = template.namespaced.then_some(namespace);
        Self::render(template.text, template.kind, namespace)
    }

    fn preset(preset: &ResourcePreset, namespaced: bool, namespace: &str) -> Self {
        Self::render(&preset.text, &preset.kind, namespaced.then_some(namespace))
    }

    /// Skeleton of `kind`; without a schema yet, rebuilt when it comes.
    pub fn skeleton(kind: &ResourceKind, namespace: &str) -> Self {
        let skeleton = Skeleton {
            api_version: templates::api_version(&kind.api.group, &kind.api.version),
            kind: kind.api.kind.clone(),
            name: templates::default_name(&kind.api.kind),
            namespace: kind.namespaced.then(|| namespace.to_owned()),
        };
        let mut this = Self::from_skeleton(&skeleton, None);
        this.skeleton = Some(skeleton);
        this
    }

    pub fn from_skeleton(
        skeleton: &Skeleton,
        schema: Option<&kubach_core::apply::SchemaDocument>,
    ) -> Self {
        let text = templates::skeleton(
            &skeleton.api_version,
            &skeleton.kind,
            schema,
            &skeleton.name,
            skeleton.namespace.as_deref(),
        );
        Self {
            select: templates::name_range(&text, &skeleton.name),
            text,
            kind: Some(skeleton.kind.clone()),
            skeleton: None,
        }
    }
}

/// What "+" opens on the page of `kind`: the user's default preset of the
/// kind, else its built-in template, else a skeleton from the schema.
pub fn default_text(kind: &ResourceKind, namespace: &str, presets: &Presets) -> NewText {
    let (group, name) = (&kind.api.group, &kind.api.kind);
    if let Some(preset) = presets.default_for(group, name) {
        return NewText::preset(preset, kind.namespaced, namespace);
    }
    if let Some(template) = templates::builtin_for(group, name) {
        return NewText::builtin(template, namespace);
    }
    NewText::skeleton(kind, namespace)
}

fn presets(cx: &App) -> Presets {
    AppSettings::try_global(cx)
        .map(|settings| settings.read(cx).get().presets.clone())
        .unwrap_or_default()
}

fn catalog(cluster: &Entity<Cluster>, cx: &App) -> Option<ApiCatalog> {
    match cluster.read(cx).catalog() {
        CatalogState::Ready(catalog) => Some(catalog.clone()),
        _ => None,
    }
}

/// "+" for new resources. On the page of a known kind it opens that
/// kind's template at once; the chevron lists every template and preset,
/// grouped by kind, and "Empty".
pub fn new_button(
    cluster: &Entity<Cluster>,
    namespace: String,
    kind: Option<(String, String)>,
    cx: &App,
) -> AnyElement {
    let connected = cluster.read(cx).connection().is_some();
    if !connected {
        return Button::new("new-resource")
            .outline()
            .small()
            .icon(IconName::Plus)
            .tooltip(t!("editor-new-tooltip"))
            .disabled(true)
            .into_any_element();
    }
    let kind = kind.and_then(|(group, kind)| catalog(cluster, cx)?.find(&group, &kind).cloned());
    let tooltip = match &kind {
        Some(kind) => t!("editor-new-kind-tooltip", kind = &kind.api.kind),
        None => t!("editor-new-tooltip"),
    };
    let main = (cluster.clone(), namespace.clone(), kind.clone());
    let menu = (cluster.clone(), namespace, kind);
    SplitButton::new("new-resource", t!("editor-new"))
        .icon(IconName::Plus)
        .tooltip(tooltip)
        .menu_tooltip(t!("editor-templates-tooltip"))
        .on_click(move |_, _, cx| {
            let (cluster, namespace, kind) = &main;
            let template = kind
                .as_ref()
                .map(|kind| default_text(kind, namespace, &presets(cx)));
            open_new(cluster, namespace, template, cx);
        })
        .menu(move |popup, _, cx| {
            let (cluster, namespace, kind) = &menu;
            fill_menu(popup, cluster, namespace, kind.as_ref(), cx)
        })
        .into_any_element()
}

fn open_new(cluster: &Entity<Cluster>, namespace: &str, template: Option<NewText>, cx: &mut App) {
    open(
        cluster,
        Target::New {
            namespace: namespace.to_owned(),
            template,
        },
        cx,
    );
}

/// One kind in the menu: its built-in template and the user's presets.
struct MenuKind<'a> {
    group: &'a str,
    kind: &'a str,
    builtin: Option<&'static Template>,
    presets: Vec<(usize, &'a ResourcePreset)>,
}

/// Kinds of the menu: `current` first, then the built-in templates, then
/// kinds that only have presets.
fn menu_kinds<'a>(current: Option<&'a ResourceKind>, presets: &'a Presets) -> Vec<MenuKind<'a>> {
    let mut kinds: Vec<MenuKind<'a>> = Vec::new();
    let mut add = |group: &'a str, kind: &'a str| {
        if !kinds.iter().any(|k| k.group == group && k.kind == kind) {
            kinds.push(MenuKind {
                group,
                kind,
                builtin: templates::builtin_for(group, kind),
                presets: presets
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.is_kind(group, kind))
                    .collect(),
            });
        }
    };
    if let Some(current) = current {
        add(&current.api.group, &current.api.kind);
    }
    for template in templates::builtin() {
        add(template.group, template.kind);
    }
    for preset in &presets.items {
        add(preset.group(), &preset.kind);
    }
    kinds
}

fn fill_menu(
    popup: PopupMenu,
    cluster: &Entity<Cluster>,
    namespace: &str,
    current: Option<&ResourceKind>,
    cx: &App,
) -> PopupMenu {
    let presets = presets(cx);
    let catalog = catalog(cluster, cx);
    let namespaced = |group: &str, kind: &str| {
        catalog
            .as_ref()
            .and_then(|c| c.find(group, kind))
            .is_none_or(|k| k.namespaced)
    };
    let item = |label: String, template: Option<NewText>| {
        let (cluster, namespace) = (cluster.clone(), namespace.to_owned());
        PopupMenuItem::new(label)
            .on_click(move |_, _, cx| open_new(&cluster, &namespace, template.clone(), cx))
    };
    let mut popup = popup
        .max_h(px(480.))
        .scrollable(true)
        .item(item(t!("editor-menu-empty"), None));
    for (ix, entry) in menu_kinds(current, &presets).into_iter().enumerate() {
        popup = popup.separator().label(entry.kind.to_owned());
        let is_current = ix == 0
            && current.is_some_and(|c| c.api.group == entry.group && c.api.kind == entry.kind);
        match (entry.builtin, current.filter(|_| is_current)) {
            (Some(template), _) => {
                popup = popup.item(item(
                    t!("editor-menu-builtin", label = template.label),
                    Some(NewText::builtin(template, namespace)),
                ));
            }
            (None, Some(kind)) => {
                popup = popup.item(item(
                    t!("editor-menu-skeleton"),
                    Some(NewText::skeleton(kind, namespace)),
                ));
            }
            (None, None) => {}
        }
        let namespaced = namespaced(entry.group, entry.kind);
        for (_, preset) in entry.presets {
            let label = if preset.default {
                t!("editor-menu-default", name = &preset.name)
            } else {
                preset.name.clone()
            };
            popup = popup.item(item(
                label,
                Some(NewText::preset(preset, namespaced, namespace)),
            ));
        }
    }
    popup
}

/// The skeleton waiting for its schema, and the text it was opened with.
pub(super) struct PendingSkeleton {
    pub skeleton: Skeleton,
    pub text: String,
}

impl YamlEditor {
    /// Rebuilds a skeleton with the required fields once the schema of its
    /// kind is known, unless the text was edited meanwhile.
    pub(super) fn sync_skeleton(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = &self.skeleton else {
            return;
        };
        let lookup = self.schemas.borrow().lookup(&pending.skeleton.gvk());
        let document = match lookup {
            super::analysis::SchemaLookup::Pending => return,
            super::analysis::SchemaLookup::Ready(document) => document,
            super::analysis::SchemaLookup::Unavailable => {
                self.skeleton = None;
                return;
            }
        };
        let Some(pending) = self.skeleton.take() else {
            return;
        };
        if self.text(cx) != pending.text {
            return;
        }
        let next = NewText::from_skeleton(&pending.skeleton, Some(&document));
        if next.text == pending.text {
            return;
        }
        self.baseline = next.text.clone();
        self.set_text(&next, window, cx);
    }

    /// Replaces the text and selects the name.
    pub(super) fn set_text(&mut self, text: &NewText, window: &mut Window, cx: &mut Context<Self>) {
        let select = text.select.clone();
        let value = text.text.clone();
        self.editor.update(cx, |state, cx| {
            state.set_value(value, window, cx);
            if let Some(range) = select {
                state.set_selected_range(range, cx);
            }
        });
    }

    /// Shows the "Save as preset" bar with a suggested name.
    pub(super) fn ask_preset_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let suggested = templates::kind_of(&text)
            .map(|(_, kind)| t!("editor-preset-suggested", kind = kind))
            .unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(suggested));
        self.preset_subscriptions =
            vec![
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.save_preset(window, cx);
                    }
                }),
            ];
        self.preset_subscriptions
            .extend(crate::i18n::localized_placeholder(
                &input,
                || t!("editor-preset-placeholder"),
                window,
                cx,
            ));
        input.update(cx, |input, cx| input.focus(window, cx));
        self.preset_name = Some(input);
        cx.notify();
    }

    pub(super) fn cancel_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preset_name = None;
        self.preset_subscriptions.clear();
        self.focus_editor(window, cx);
        cx.notify();
    }

    /// Saves the text as a preset: the first document's name and namespace
    /// become placeholders.
    pub(super) fn save_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = &self.preset_name else {
            return;
        };
        let name = input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.banner = Some(Banner::new(Tone::Warning, t!("editor-preset-name-first")));
            cx.notify();
            return;
        }
        if let Err(error) = self.sync_keys(window, cx) {
            self.banner = Some(Banner::new(Tone::Danger, error));
            cx.notify();
            return;
        }
        let text = self.text(cx);
        let preset = match preset_from(&name, &text) {
            Ok(preset) => preset,
            Err(error) => {
                self.banner = Some(Banner::new(Tone::Danger, error));
                cx.notify();
                return;
            }
        };
        let kind = preset.kind.clone();
        let Some(settings) = AppSettings::try_global(cx) else {
            return;
        };
        settings.update(cx, |settings, cx| {
            settings.edit(cx, |s| {
                s.presets.save(preset);
            })
        });
        self.banner = Some(Banner::new(
            Tone::Success,
            t!("editor-preset-saved", name = name, kind = kind),
        ));
        self.cancel_preset(window, cx);
    }

    pub(super) fn render_preset_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        use gpui_kit::component::ActiveTheme as _;
        use gpui_kit::component::input::Input;
        let input = self.preset_name.as_ref()?;
        let theme = cx.theme();
        Some(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_2()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(t!("editor-save-preset")),
                )
                .child(div().w(px(260.)).child(Input::new(input).small()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t!(
                            "editor-preset-hint",
                            name = "${name}",
                            namespace = "${namespace}"
                        )),
                )
                .child(
                    Button::new("preset-save")
                        .primary()
                        .small()
                        .label(t!("editor-save"))
                        .on_click(cx.listener(|this, _, window, cx| this.save_preset(window, cx))),
                )
                .child(
                    Button::new("preset-cancel")
                        .ghost()
                        .small()
                        .label(t!("ui-cancel"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.cancel_preset(window, cx)),
                        ),
                ),
        )
    }
}

/// A preset named `name` from the editor text.
fn preset_from(name: &str, text: &str) -> Result<ResourcePreset, String> {
    if templates::has_secret(text) {
        return Err(t!("editor-preset-secret"));
    }
    let Some((api_version, kind)) = templates::kind_of(text) else {
        return Err(t!("editor-preset-no-kind"));
    };
    Ok(ResourcePreset {
        name: name.to_owned(),
        api_version,
        kind,
        text: templates::generalize(text),
        default: false,
    })
}

#[cfg(test)]
mod tests {
    use super::{NewText, default_text, menu_kinds, preset_from};
    use kubach_core::ResourceKind;
    use kubach_core::settings::{Presets, ResourcePreset};
    use kube::discovery::ApiResource;

    fn kind(group: &str, version: &str, name: &str, namespaced: bool) -> ResourceKind {
        let api_version = if group.is_empty() {
            version.to_owned()
        } else {
            format!("{group}/{version}")
        };
        ResourceKind {
            api: ApiResource {
                group: group.into(),
                version: version.into(),
                api_version,
                kind: name.into(),
                plural: format!("{}s", name.to_lowercase()),
            },
            namespaced,
            verbs: vec!["create".into()],
        }
    }

    fn preset(name: &str, default: bool) -> ResourcePreset {
        ResourcePreset {
            name: name.into(),
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            text: "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: ${name}\n  namespace: ${namespace}\n".into(),
            default,
        }
    }

    #[test]
    fn plus_prefers_default_preset_then_builtin_then_skeleton() {
        let deployment = kind("apps", "v1", "Deployment", true);
        let mut presets = Presets::default();
        let builtin = default_text(&deployment, "team", &presets);
        assert!(builtin.text.contains("image: nginx"));
        assert!(builtin.text.contains("namespace: team"));
        assert_eq!(
            &builtin.text[builtin.select.clone().unwrap()],
            "my-deployment"
        );
        assert!(builtin.skeleton.is_none());

        presets.save(preset("mine", false));
        assert_eq!(default_text(&deployment, "team", &presets), builtin);
        presets.set_default(0, true);
        let mine = default_text(&deployment, "team", &presets);
        assert_eq!(
            mine.text,
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: my-deployment\n  namespace: team\n"
        );

        let widget = kind("example.com", "v1", "Widget", false);
        let skeleton = default_text(&widget, "team", &presets);
        assert!(skeleton.skeleton.is_some());
        assert!(!skeleton.text.contains("namespace"));
        assert_eq!(
            &skeleton.text[skeleton.select.clone().unwrap()],
            "my-widget"
        );
        assert_eq!(
            NewText::from_skeleton(skeleton.skeleton.as_ref().unwrap(), None).text,
            skeleton.text
        );
    }

    #[test]
    fn menu_puts_the_current_kind_first() {
        let mut presets = Presets::default();
        presets.save(preset("mine", false));
        let mut other = preset("thing", false);
        other.api_version = "example.com/v1".into();
        other.kind = "Thing".into();
        presets.save(other);
        let widget = kind("example.com", "v1", "Widget", true);
        let kinds = menu_kinds(Some(&widget), &presets);
        assert_eq!(kinds[0].kind, "Widget");
        assert!(kinds[0].builtin.is_none());
        let deployment = kinds.iter().find(|k| k.kind == "Deployment").unwrap();
        assert!(deployment.builtin.is_some());
        assert_eq!(deployment.presets.len(), 1);
        assert_eq!(kinds.last().unwrap().kind, "Thing");
        assert_eq!(kinds.len(), 1 + 14 + 1);
    }

    #[test]
    fn presets_need_a_kind_and_no_secrets() {
        let saved = preset_from(
            "web",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: app\n  namespace: prod\n",
        )
        .unwrap();
        assert_eq!(saved.kind, "ConfigMap");
        assert!(saved.text.contains("name: ${name}"));
        assert!(saved.text.contains("namespace: ${namespace}"));
        assert!(preset_from("x", "data: {}\n").is_err());
        assert!(
            preset_from(
                "x",
                "apiVersion: v1\nkind: ConfigMap\n---\napiVersion: v1\nkind: Secret\n"
            )
            .is_err()
        );
    }
}
