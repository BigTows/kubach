//! The two "Clusters" pages (#402). "Common": kubeconfig files, the
//! startup context, background connections and imported configs. "Per
//! cluster": a list of the contexts and the settings of the picked one
//! (#278): hotbar tile, default namespace, Prometheus (ADR 0009), the Helm
//! storage driver, security and how to remove it from Kubach.

use std::path::PathBuf;

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::helm::Driver;
use kubach_core::kubeconfig::{FileState, Source};
use kubach_core::metrics::prometheus::Endpoint;
use kubach_core::settings::{DEFAULT_KEEP_CONNECTED, PrometheusSource, Settings};
use kubach_i18n::t;
use kubach_ui::ButtonLoading as _;
use kubach_ui::{
    NumberRow, SelectRow, SettingRow, SideList, SideListItem, ToggleRow, format_age, unix_now,
};

use super::{
    Anchor, Choice, Entry, Group, Section, SettingsScreen, index_of, searchable, setter, translated,
};
use crate::hotbar::tile_color;
use crate::kubeconfig;
use crate::screens::{PrometheusRegistry, PrometheusStatus, confirm_prometheus};
use crate::settings::AppSettings;
use crate::vault;

/// Per-context inputs of the page, rebuilt when the contexts change.
#[derive(Default)]
pub(super) struct ClustersForm {
    pub(super) namespace_inputs: Vec<(String, Entity<InputState>)>,
    _namespace_subscriptions: Vec<Subscription>,
    /// Prometheus address per context (ADR 0009).
    pub(super) prometheus_inputs: Vec<(String, Entity<InputState>)>,
    _prometheus_subscriptions: Vec<Subscription>,
}

/// Helm storage driver of a context; `None` detects it.
const HELM_DRIVERS: [Choice<Option<Driver>>; 3] = [
    (None, || t!("settings-helm-driver-detect")),
    (Some(Driver::Secret), || "Secret".to_owned()),
    (Some(Driver::ConfigMap), || "ConfigMap".to_owned()),
];

/// From this many contexts on, the list on the per-cluster page gets a
/// filter.
const FILTER_FROM: usize = 8;

/// How a context leaves Kubach, by where it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Removal {
    /// Imported into the vault: the copy is deleted ("Remove from Kubach…").
    Vault { id: String },
    /// From a linked file: the file is unlinked, with every context in it.
    Unlink(PathBuf),
    /// From KUBECONFIG or `~/.kube/config`, the user's own files: only
    /// hidden from the hotbar.
    Hide,
}

/// The way `source` (`None` when unknown) is removed from Kubach.
pub(super) fn removal(source: Option<&Source>) -> Removal {
    match source {
        Some(Source::Imported { id, .. }) => Removal::Vault { id: id.clone() },
        Some(Source::Linked(path)) => Removal::Unlink(path.clone()),
        Some(Source::Env(_) | Source::Home(_)) | None => Removal::Hide,
    }
}

/// Short name of where a context comes from, for the context list.
fn source_badge(source: Option<&Source>) -> String {
    match source {
        Some(Source::Env(_) | Source::Home(_)) => "kubeconfig".to_owned(),
        Some(Source::Linked(_)) => t!("settings-badge-linked"),
        Some(Source::Imported { .. }) => t!("settings-badge-vault"),
        None => String::new(),
    }
}

/// [`Source::origin`] in the interface language: `KUBECONFIG` and
/// `~/.kube/config` stay as they are.
fn origin_label(source: &Source) -> String {
    match source {
        Source::Linked(_) => t!("settings-origin-linked"),
        Source::Imported { .. } => t!("settings-origin-imported"),
        Source::Env(_) | Source::Home(_) => source.origin().to_owned(),
    }
}

/// Whether `query` (lowercase words) matches every word in `text`.
fn matches_filter(text: &str, query: &str) -> bool {
    let text = text.to_lowercase();
    query
        .split_whitespace()
        .all(|word| text.contains(&word.to_lowercase()))
}

impl SettingsScreen {
    /// Per-context state of the page: the selected context and the
    /// namespace and Prometheus inputs.
    pub(super) fn rebuild_context_inputs(
        &mut self,
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let contexts: Vec<_> = self
            .contexts
            .as_ref()
            .map(|contexts| contexts.list().to_vec())
            .unwrap_or_default();
        let keep = self
            .cluster_context
            .as_ref()
            .is_some_and(|name| contexts.iter().any(|c| &c.name == name));
        if !keep {
            self.cluster_context = self.contexts.as_ref().ok().and_then(|all| {
                settings
                    .clusters
                    .startup_context
                    .as_deref()
                    .and_then(|name| all.get(name))
                    .or_else(|| all.default_context())
                    .map(|context| context.name.clone())
            });
        }

        self.clusters._namespace_subscriptions.clear();
        self.clusters.namespace_inputs = contexts
            .iter()
            .map(|context| {
                let name = context.name.clone();
                let stored = settings
                    .clusters
                    .default_namespace(&name)
                    .unwrap_or("")
                    .to_owned();
                let placeholder = context.namespace.clone().unwrap_or("default".into());
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(stored)
                });
                let target = name.clone();
                let subscription =
                    cx.subscribe_in(&input, window, move |this, input, event, _, cx| {
                        if let InputEvent::Change = event {
                            let value = input.read(cx).value().to_string();
                            let target = target.clone();
                            this.settings.update(cx, |settings, cx| {
                                settings.edit(cx, |settings| {
                                    settings
                                        .clusters
                                        .set_default_namespace(&target, Some(value))
                                })
                            });
                        }
                    });
                self.clusters._namespace_subscriptions.push(subscription);
                (name, input)
            })
            .collect();

        self.clusters._prometheus_subscriptions.clear();
        self.clusters.prometheus_inputs = contexts
            .iter()
            .map(|context| {
                let name = context.name.clone();
                let stored = settings.clusters.prometheus(&name).unwrap_or("").to_owned();
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(t!("settings-prometheus-placeholder"))
                        .default_value(stored)
                });
                let target = name.clone();
                let subscription =
                    cx.subscribe_in(&input, window, move |this, input, event, _, cx| {
                        if let InputEvent::Change = event {
                            let value = input.read(cx).value().to_string();
                            let target = target.clone();
                            this.settings.update(cx, |settings, cx| {
                                settings.edit(cx, |settings| {
                                    settings.clusters.set_prometheus(&target, Some(value))
                                })
                            });
                        }
                    });
                self.clusters._prometheus_subscriptions.push(subscription);
                (name, input)
            })
            .collect();
    }

    pub(super) fn clusters(&self, settings: &Settings, cx: &mut Context<Self>) -> Vec<Group> {
        let mut kubeconfig = Group::new("Kubeconfig");
        let loading = self.contexts_loading(cx);
        if loading {
            kubeconfig = kubeconfig.notice(
                Alert::info("kubeconfig-loading", t!("settings-kubeconfig-loading")).small(),
            );
        } else if let Err(error) = &self.contexts {
            kubeconfig = kubeconfig.notice(
                Alert::warning(
                    "kubeconfig-error",
                    t!("settings-kubeconfig-not-loaded", error = error.to_string()),
                )
                .small(),
            );
        }

        // Imported configs are listed with the vault below.
        let files: Vec<FileState> = self
            .contexts
            .as_ref()
            .map(|contexts| contexts.files().to_vec())
            .unwrap_or_default()
            .into_iter()
            .filter(|file| !file.source.is_imported())
            .collect();
        let linked = !settings.clusters.kubeconfig_paths.is_empty();
        let list = div().flex().flex_col().gap_1().children(
            files
                .iter()
                .enumerate()
                .map(|(ix, file)| file_row(ix, file, cx)),
        );
        let clear = setter(&self.settings, |s, ()| s.clusters.kubeconfig_paths.clear());
        let (files_title, files_description) = (
            t!("settings-kubeconfig-files"),
            t!("settings-kubeconfig-files-description"),
        );
        kubeconfig = kubeconfig.entry(Entry::new(
            searchable(
                "kubeconfig files paths additional extra linked link add missing permissions \
                 find again remove",
                &[&files_title, &files_description],
            ),
            SettingRow::new("kubeconfig-paths", files_title)
                .description(files_description)
                .reset(linked, move |window, cx| clear((), window, cx))
                .control(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            Button::new("add-kubeconfig")
                                .outline()
                                .small()
                                .icon(IconName::Plus)
                                .label(t!("settings-link-file"))
                                .tooltip(t!("settings-link-file-tooltip"))
                                .on_click(|_, window, cx| kubeconfig::link_file(window, cx)),
                        )
                        .child(
                            Button::new("import-kubeconfig")
                                .outline()
                                .small()
                                .label(t!("settings-import"))
                                .tooltip(t!("settings-import-tooltip"))
                                .on_click(|_, window, cx| vault::import_file(window, cx)),
                        ),
                )
                .when(!files.is_empty(), |row| row.below(list)),
        ));

        // Startup context: "current-context" first, then every context.
        let names: Vec<String> = self
            .contexts
            .as_ref()
            .map(|contexts| contexts.list().iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default();
        let current = self
            .contexts
            .as_ref()
            .ok()
            .and_then(|contexts| contexts.default_context())
            .map(|context| context.name.clone());
        let mut options = vec![match &current {
            Some(name) => format!("current-context ({name})"),
            None => "current-context".to_owned(),
        }];
        options.extend(names.iter().cloned());
        let startup = settings.clusters.startup_context.clone();
        let selected = match &startup {
            None => 0,
            Some(name) => match names.iter().position(|n| n == name) {
                Some(ix) => ix + 1,
                None => {
                    options.push(t!("settings-startup-context-missing", name = name));
                    options.len() - 1
                }
            },
        };
        let choices = names.clone();
        let (startup_title, startup_description) = (
            t!("settings-startup-context"),
            t!("settings-startup-context-description"),
        );
        kubeconfig = kubeconfig.entry(Entry::new(
            searchable(
                "startup context open at launch current-context",
                &[&startup_title, &startup_description],
            ),
            SelectRow::new("startup-context", startup_title, options)
                .description(startup_description)
                .selected(selected)
                .default_index(0)
                .on_change(setter(&self.settings, move |s, ix: usize| {
                    if ix == 0 {
                        s.clusters.startup_context = None;
                    } else if let Some(name) = choices.get(ix - 1) {
                        s.clusters.startup_context = Some(name.clone());
                    }
                })),
        ));

        let (keep_title, keep_description) = (
            t!("settings-keep-connected"),
            t!("settings-keep-connected-description"),
        );
        kubeconfig = kubeconfig.entry(Entry::new(
            searchable(
                "background connections keep connected recent contexts hotbar",
                &[&keep_title, &keep_description],
            ),
            NumberRow::new("keep-connected", keep_title)
                .description(keep_description)
                .value(settings.clusters.keep_connected.max(1))
                .range(1..=10)
                .step(1)
                .default_value(DEFAULT_KEEP_CONNECTED)
                .on_change(setter(&self.settings, |s, count: u32| {
                    s.clusters.keep_connected = count
                })),
        ));

        vec![kubeconfig, self.vault_group(settings, cx)]
    }

    /// Source of `name`, if the contexts are loaded.
    fn source_of(&self, name: &str) -> Option<&Source> {
        self.contexts
            .as_ref()
            .ok()
            .and_then(|contexts| contexts.source(name))
    }

    /// API server of `name`, empty when unknown.
    fn server_of(&self, name: &str) -> String {
        self.contexts
            .as_ref()
            .ok()
            .and_then(|contexts| contexts.server(name))
            .unwrap_or_default()
            .to_owned()
    }

    /// The per-cluster page: everything set for the selected context
    /// (#278). The list to pick it from is beside the page
    /// ([`Self::render_context_list`]); while searching, the contexts are a
    /// group of their own.
    pub(super) fn cluster_page(&self, settings: &Settings, cx: &mut Context<Self>) -> Vec<Group> {
        let Some(name) = self.selected_context() else {
            let notice = if self.contexts_loading(cx) {
                t!("settings-kubeconfig-loading")
            } else {
                t!("settings-no-contexts-notice")
            };
            return vec![
                Group::new(t!("settings-group-cluster"))
                    .notice(Alert::info("no-contexts", notice).small()),
            ];
        };
        let server = self.server_of(&name);
        let searching = !self.query.trim().is_empty();
        let mut groups = Vec::new();
        if searching {
            groups.push(self.contexts_group(cx));
        }
        groups.extend([
            self.overview_group(&name, &server, settings, cx),
            self.tile_color_group(&name, settings),
            self.namespace_group(&name, &server, settings),
            self.prometheus_group(&name, settings, cx),
            self.helm_driver_group(&name, &server, settings),
        ]);
        groups.extend(self.security_groups(&name, &server, settings, cx));
        groups.push(self.remove_group(&name, settings));
        groups
    }

    /// While searching: every context, to jump to its settings.
    fn contexts_group(&self, cx: &mut Context<Self>) -> Group {
        let selected = self.selected_context();
        let mut group = Group::new(t!("settings-group-contexts")).keywords("context cluster");
        for (ix, name) in self.context_names().into_iter().enumerate() {
            let server = self.server_of(&name);
            let source = self.source_of(&name);
            let text = format!(
                "context cluster {name} {server} {} {} {}",
                source_badge(source),
                source.map(Source::origin).unwrap_or_default(),
                source.map(origin_label).unwrap_or_default()
            );
            let target = name.clone();
            let open = cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.section = Section::Cluster;
                this.query.clear();
                this.search
                    .update(cx, |search, cx| search.set_value("", window, cx));
                this.select_context(Some(target.clone()), window, cx);
            });
            group = group.entry(Entry::new(
                text,
                SettingRow::new(("cluster-context", ix), name.clone())
                    .description(server)
                    .control(
                        Button::new(("open-cluster-context", ix))
                            .outline()
                            .small()
                            .label(if selected.as_ref() == Some(&name) {
                                t!("settings-context-shown-below")
                            } else {
                                t!("settings-context-open")
                            })
                            .on_click(open),
                    ),
            ));
        }
        group
    }

    /// The list of contexts beside the per-cluster page, with a filter
    /// once there are many.
    pub(super) fn render_context_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.settings.read(cx).get().clone();
        let prefs = &settings.clusters.hotbar;
        let names = self.context_names();
        let filter = self.context_filter.read(cx).value().to_string();
        let with_filter = names.len() >= FILTER_FROM || !filter.is_empty();
        let items: Vec<SideListItem> = names
            .iter()
            .filter_map(|name| {
                let server = self.server_of(name);
                let source = self.source_of(name);
                let badge = source_badge(source);
                let alias = prefs.alias(name).unwrap_or_default();
                if !matches_filter(&format!("{name} {alias} {server} {badge}"), &filter) {
                    return None;
                }
                let hidden = prefs.hidden_of(std::slice::from_ref(name)).len() == 1;
                let item = SideListItem::new(name.clone(), name.clone())
                    .accent(tile_color(prefs, name).color(cx))
                    .badge(badge)
                    .muted(hidden);
                Some(match (hidden, server.is_empty()) {
                    (true, _) => item.detail(t!("settings-context-hidden")),
                    (false, false) => item.detail(server),
                    (false, true) => item,
                })
            })
            .collect();
        let entity = cx.entity().downgrade();
        let mut list = SideList::new("settings-contexts", items)
            .selected(self.selected_context())
            .empty(if names.is_empty() {
                t!("settings-no-contexts")
            } else {
                t!("settings-no-contexts-match")
            })
            .on_select(move |name, window, cx| {
                let name = name.to_string();
                entity
                    .update(cx, |this, cx| this.select_context(Some(name), window, cx))
                    .ok();
            });
        if with_filter {
            list = list.header(Input::new(&self.context_filter).small().cleanable(true));
        }
        list
    }

    /// Where `name` comes from and how it shows in the hotbar.
    fn overview_group(&self, name: &str, server: &str, settings: &Settings, cx: &App) -> Group {
        let source = self.source_of(name);
        let (origin, location) = match source {
            Some(Source::Imported { name: entry, .. }) => (
                t!("settings-origin-imported"),
                t!("settings-location-imported", entry = entry),
            ),
            Some(Source::Linked(path)) => (
                t!("settings-origin-linked"),
                t!(
                    "settings-location-linked",
                    path = path.display().to_string()
                ),
            ),
            Some(source @ (Source::Env(path) | Source::Home(path))) => (
                source.origin().to_owned(),
                t!("settings-location-own", path = path.display().to_string()),
            ),
            None => (t!("settings-origin-unknown"), String::new()),
        };
        let hidden = settings
            .clusters
            .hotbar
            .hidden_of(&[name.to_owned()])
            .contains(&name.to_owned());
        let target = name.to_owned();
        let (show_title, show_description) = (
            t!("settings-show-in-hotbar"),
            t!("settings-show-in-hotbar-description"),
        );
        Group::new(name.to_owned())
            .description(if server.is_empty() {
                t!("settings-no-api-server")
            } else {
                server.to_owned()
            })
            .keywords("context cluster source")
            .entry(Entry::new(
                format!("source kubeconfig linked file imported vault origin {origin} {location}"),
                SettingRow::new("cluster-source", t!("settings-source"))
                    .description(location)
                    .control(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(origin.clone()),
                    ),
            ))
            .entry(Entry::new(
                searchable(
                    "show in hotbar hide hidden tile",
                    &[&show_title, &show_description],
                ),
                ToggleRow::new("cluster-show-in-hotbar", show_title)
                    .description(show_description)
                    .checked(!hidden)
                    .default_value(true)
                    .on_change(setter(&self.settings, move |s, show: bool| {
                        s.clusters.hotbar.set_hidden(&target, !show)
                    })),
            ))
    }

    /// How `name` leaves Kubach, by its source; every way asks first.
    fn remove_group(&self, name: &str, settings: &Settings) -> Group {
        let group = Group::new(t!("settings-group-remove"))
            .keywords(format!("delete remove unlink hide forget {name}"));
        match removal(self.source_of(name)) {
            Removal::Vault { id } => {
                let contexts = self.vault_contexts(&id);
                let title = t!("settings-remove-from-kubach");
                group.entry(Entry::new(
                    searchable("remove from kubach imported vault delete", &[&title]),
                    SettingRow::new("cluster-remove", title)
                        .description(t!(
                            "settings-remove-from-kubach-description",
                            contexts = contexts.join(", ")
                        ))
                        .control(
                            Button::new("cluster-remove-button")
                                .danger()
                                .small()
                                .label(t!("settings-remove-from-kubach-button"))
                                .on_click(move |_, window, cx| {
                                    vault::confirm_remove(id.clone(), window, cx)
                                }),
                        ),
                ))
            }
            Removal::Unlink(path) => {
                let contexts = self.file_contexts(&path);
                let others = contexts.iter().filter(|c| *c != name).count();
                let description = if others == 0 {
                    t!(
                        "settings-unlink-description-single",
                        path = path.display().to_string()
                    )
                } else {
                    t!(
                        "settings-unlink-description",
                        path = path.display().to_string(),
                        count = contexts.len(),
                        contexts = contexts.join(", ")
                    )
                };
                let title = t!("settings-unlink-file");
                group.entry(Entry::new(
                    searchable("unlink linked file remove kubeconfig", &[&title]),
                    SettingRow::new("cluster-remove", title)
                        .description(description)
                        .control(
                            Button::new("cluster-remove-button")
                                .danger()
                                .small()
                                .label(t!("settings-unlink-file-button"))
                                .on_click(move |_, window, cx| {
                                    kubeconfig::confirm_unlink(
                                        path.clone(),
                                        contexts.clone(),
                                        window,
                                        cx,
                                    )
                                }),
                        ),
                ))
            }
            Removal::Hide => {
                let origin = self
                    .source_of(name)
                    .map(origin_label)
                    .unwrap_or_else(|| t!("settings-origin-your-kubeconfig"));
                let hidden = settings
                    .clusters
                    .hotbar
                    .hidden_of(&[name.to_owned()])
                    .contains(&name.to_owned());
                let (settings, target) = (self.settings.clone(), name.to_owned());
                let title = t!("settings-hide");
                group.entry(Entry::new(
                    searchable("hide kubeconfig home kube config", &[&title]),
                    SettingRow::new("cluster-remove", title)
                        .description(t!(
                            "settings-hide-description",
                            name = name,
                            origin = origin
                        ))
                        .control(
                            Button::new("cluster-remove-button")
                                .outline()
                                .small()
                                .label(if hidden {
                                    t!("settings-hidden")
                                } else {
                                    t!("settings-hide-button")
                                })
                                .disabled(hidden)
                                .on_click(move |_, window, cx| {
                                    confirm_hide(settings.clone(), target.clone(), window, cx)
                                }),
                        ),
                ))
            }
        }
    }

    /// Contexts of the vault entry `id`.
    fn vault_contexts(&self, id: &str) -> Vec<String> {
        self.contexts
            .as_ref()
            .map(|contexts| {
                contexts
                    .list()
                    .iter()
                    .filter(|context| {
                        matches!(
                            contexts.source(&context.name),
                            Some(Source::Imported { id: entry, .. }) if entry == id
                        )
                    })
                    .map(|context| context.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Contexts of the kubeconfig file at `path`.
    fn file_contexts(&self, path: &std::path::Path) -> Vec<String> {
        self.contexts
            .as_ref()
            .ok()
            .and_then(|contexts| contexts.files().iter().find(|file| file.path() == path))
            .map(|file| file.contexts.clone())
            .unwrap_or_default()
    }

    /// Default namespace of `name`.
    fn namespace_group(&self, name: &str, server: &str, settings: &Settings) -> Group {
        let mut group = Group::new(t!("settings-group-default-namespace"))
            .description(t!("settings-default-namespace-description"))
            .keywords(name);
        for (ix, (context, input)) in self.clusters.namespace_inputs.iter().enumerate() {
            if context != name {
                continue;
            }
            let target = context.clone();
            let reset = setter(&self.settings, move |s, ()| {
                s.clusters.set_default_namespace(&target, None)
            });
            group = group.entry(Entry::new(
                format!("default namespace {name} {server}"),
                SettingRow::new(("namespace", ix), context.clone())
                    .description(server.to_owned())
                    .reset(
                        settings.clusters.default_namespace(name).is_some(),
                        move |window, cx| reset((), window, cx),
                    )
                    .control(div().w(px(200.)).child(Input::new(input).small())),
            ));
        }
        group
    }

    /// Prometheus of `name` (ADR 0009): the source, the servers the last
    /// search found, the typed address and "Detect again".
    fn prometheus_group(&self, name: &str, settings: &Settings, cx: &mut Context<Self>) -> Group {
        let mut group = Group::new("Prometheus")
            .description(t!("settings-prometheus-description"))
            .keywords(name)
            .anchor(Anchor::Prometheus);
        let registry = PrometheusRegistry::try_global(cx);
        let selected = name;
        for (ix, (name, input)) in self.clusters.prometheus_inputs.iter().enumerate() {
            if name != selected {
                continue;
            }
            let source = settings.clusters.prometheus_source(name);
            let discovery = PrometheusRegistry::discovery(name, cx);
            let (detecting, detect_error, status) = match &registry {
                Some(registry) => {
                    let registry = registry.read(cx);
                    (
                        registry.detecting(name),
                        registry.detect_error(name).map(ToOwned::to_owned),
                        Some(registry.status(name, cx)),
                    )
                }
                None => (false, None, None),
            };

            let mut options = vec![(PrometheusSource::Auto, t!("settings-prometheus-auto"))];
            options.extend(
                PrometheusRegistry::choices(name, &source, cx)
                    .into_iter()
                    .map(|address| {
                        // Outside the well-known namespaces (#200).
                        let unusual = Endpoint::parse(&address)
                            .is_ok_and(|endpoint| endpoint.unusual_namespace().is_some());
                        let label = if unusual {
                            t!("settings-prometheus-unusual", address = address.clone())
                        } else {
                            address.clone()
                        };
                        (PrometheusSource::Found(address), label)
                    }),
            );
            options.push((PrometheusSource::Manual, t!("settings-prometheus-address")));
            options.push((PrometheusSource::Off, t!("settings-prometheus-off")));
            let selected = options
                .iter()
                .position(|(option, _)| *option == source)
                .unwrap_or(0);
            let label = options[selected].1.clone();
            // The choices in the interface language find the entry too.
            let option_words = options
                .iter()
                .map(|(_, label)| label.as_str())
                .collect::<Vec<_>>()
                .join(" ");

            let mut lines = Vec::new();
            if detecting {
                lines.push(t!("settings-prometheus-searching"));
            } else if let Some(error) = detect_error {
                lines.push(t!("settings-prometheus-search-failed", error = error));
            } else {
                lines.push(match &discovery {
                    None => t!("settings-prometheus-not-searched"),
                    Some(found) => {
                        let age = format_age((unix_now().max(0) as u64).saturating_sub(found.at));
                        if found.found.is_empty() {
                            t!("settings-prometheus-searched-none", age = age)
                        } else {
                            t!(
                                "settings-prometheus-searched-found",
                                age = age,
                                found = found.found.join(", ")
                            )
                        }
                    }
                });
            }
            let unconfirmed = status
                .as_ref()
                .and_then(PrometheusStatus::unconfirmed)
                .cloned();
            match status {
                Some(PrometheusStatus::Off) => {
                    lines = vec![t!("settings-prometheus-status-off")];
                }
                Some(PrometheusStatus::Ready(prometheus)) => {
                    lines.push(t!(
                        "settings-prometheus-status-ready",
                        endpoint = prometheus.endpoint().describe()
                    ));
                }
                Some(PrometheusStatus::Unreachable(endpoint)) => {
                    lines.push(t!(
                        "settings-prometheus-status-unreachable",
                        endpoint = endpoint.describe()
                    ));
                }
                Some(PrometheusStatus::Connecting(endpoint)) => {
                    lines.push(t!(
                        "settings-prometheus-status-connecting",
                        endpoint = endpoint.describe()
                    ));
                }
                Some(PrometheusStatus::NoAddress) => {
                    lines.push(t!("settings-prometheus-status-no-address"))
                }
                Some(PrometheusStatus::Invalid(address)) => {
                    lines.push(t!("settings-prometheus-status-invalid", address = address));
                }
                Some(PrometheusStatus::Unconfirmed(endpoint)) => {
                    lines.push(t!(
                        "settings-prometheus-status-unconfirmed",
                        endpoint = endpoint.describe()
                    ));
                }
                _ => {}
            }
            // Found only outside the well-known namespaces: a warning and
            // "Use it" instead of connecting (#200).
            let confirm_row = unconfirmed.map(|endpoint| {
                let namespace = endpoint.unusual_namespace().unwrap_or_default().to_owned();
                let target = name.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child(t!(
                                "settings-prometheus-unusual-warning",
                                namespace = namespace
                            )),
                    )
                    .child(
                        Button::new(("prometheus-confirm", ix))
                            .outline()
                            .xsmall()
                            .label(t!("settings-prometheus-use-it"))
                            .tooltip(t!(
                                "settings-prometheus-use-it-tooltip",
                                endpoint = endpoint.describe(),
                                name = name
                            ))
                            .on_click(move |_, _, cx| confirm_prometheus(&target, &endpoint, cx)),
                    )
            });

            let menu_settings = self.settings.clone();
            let target = name.clone();
            let picker = Button::new(("prometheus-source", ix))
                .outline()
                .small()
                .label(label.clone())
                .accessibility_label(t!(
                    "settings-prometheus-source-label",
                    name = name,
                    label = label.clone()
                ))
                .dropdown_caret(true)
                .dropdown_menu(move |menu: PopupMenu, _, _| {
                    options
                        .iter()
                        .enumerate()
                        .fold(menu, |menu, (i, (option, label))| {
                            let (settings, target, option) =
                                (menu_settings.clone(), target.clone(), option.clone());
                            menu.item(
                                PopupMenuItem::new(label.clone())
                                    .checked(i == selected)
                                    .on_click(move |_, _, cx| {
                                        settings.update(cx, |settings, cx| {
                                            settings.edit(cx, |settings| {
                                                settings
                                                    .clusters
                                                    .set_prometheus_source(&target, option.clone())
                                            })
                                        });
                                    }),
                            )
                        })
                });
            let detect_target = name.clone();
            let detect = Button::new(("prometheus-detect", ix))
                .outline()
                .small()
                .label(t!("settings-prometheus-detect-again"))
                .spinning(detecting)
                .tooltip(t!("settings-prometheus-detect-tooltip"))
                .on_click(move |_, _, cx| {
                    if let Some(registry) = PrometheusRegistry::try_global(cx) {
                        registry.update(cx, |registry, cx| registry.detect(&detect_target, cx));
                    }
                });
            let target = name.clone();
            let reset = setter(&self.settings, move |s, ()| {
                s.clusters.reset_prometheus(&target)
            });
            let changed =
                source != PrometheusSource::Auto || settings.clusters.prometheus(name).is_some();
            group = group.entry(Entry::new(
                format!(
                    "prometheus metrics history monitoring url address detect auto off {name} \
                     {option_words}"
                ),
                SettingRow::new(("prometheus", ix), name.clone())
                    .description(lines.join(" · "))
                    .reset(changed, move |window, cx| reset((), window, cx))
                    .control(div().flex().gap_1().child(picker).child(detect))
                    .when(source == PrometheusSource::Manual, |row| {
                        row.below(div().w(px(320.)).child(Input::new(input).small()))
                    })
                    .when_some(confirm_row, |row, confirm| row.below(confirm)),
            ));
        }
        group
    }

    /// Helm storage driver of `name` (ADR 0006).
    fn helm_driver_group(&self, name: &str, server: &str, settings: &Settings) -> Group {
        let target = name.to_owned();
        Group::new(t!("settings-group-helm-driver"))
            .description(t!("settings-helm-driver-description"))
            .keywords(name)
            .entry(Entry::new(
                format!(
                    "helm storage driver secret configmap helm_driver {name} {server} {}",
                    t!("settings-helm-driver-detect")
                ),
                SelectRow::new("helm-driver", name.to_owned(), translated(&HELM_DRIVERS))
                    .description(server.to_owned())
                    .selected(index_of(&HELM_DRIVERS, settings.clusters.helm_driver(name)))
                    .default_index(0)
                    .on_change(setter(&self.settings, move |s, ix: usize| {
                        s.clusters.set_helm_driver(&target, HELM_DRIVERS[ix].0)
                    })),
            ))
    }
}

/// One kubeconfig file in the Clusters page: where it comes from, its
/// contexts or why it can't be read, and what can be done about it.
fn file_row(ix: usize, file: &FileState, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let path = file.path().to_owned();
    let mut actions = div().flex().items_center().gap_1().flex_shrink_0();
    if file.source.is_linked() {
        if !file.is_ok() {
            let find = path.clone();
            actions = actions.child(
                Button::new(("find-kubeconfig", ix))
                    .outline()
                    .xsmall()
                    .label(t!("settings-find-again"))
                    .on_click(move |_, window, cx| {
                        kubeconfig::find_again(find.clone(), window, cx)
                    }),
            );
        }
        let (remove, contexts) = (path.clone(), file.contexts.clone());
        actions = actions.child(
            Button::new(("remove-kubeconfig", ix))
                .ghost()
                .xsmall()
                .when(file.is_ok(), |button| {
                    button.icon(IconName::Close).tooltip(t!("settings-unlink"))
                })
                .when(!file.is_ok(), |button| button.label(t!("settings-unlink")))
                .on_click(move |_, window, cx| {
                    kubeconfig::confirm_unlink(remove.clone(), contexts.clone(), window, cx)
                }),
        );
    }

    let status = match &file.problem {
        Some(problem) => div()
            .text_color(theme.danger)
            .child(problem.message().to_string()),
        None if file.contexts.is_empty() => div()
            .text_color(theme.muted_foreground)
            .child(t!("settings-no-contexts")),
        None => div()
            .text_color(theme.muted_foreground)
            .truncate()
            .child(t!(
                "settings-file-contexts",
                count = file.contexts.len(),
                contexts = file.contexts.join(", ")
            )),
    };
    let permissions = file.loose_mode.map(|mode| {
        let restrict = path.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(theme.warning)
                    .child(kubeconfig::permissions_warning(&path, mode)),
            )
            .child(
                Button::new(("restrict-kubeconfig", ix))
                    .outline()
                    .xsmall()
                    .label(t!("settings-restrict-permissions"))
                    .on_click(move |_, window, cx| kubeconfig::restrict(&restrict, window, cx)),
            )
    });

    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_2()
        .py_1p5()
        .rounded_md()
        .bg(theme.muted)
        .text_xs()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground)
                        .child(origin_label(&file.source)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.mono_font_family.clone())
                        .child(path.display().to_string()),
                )
                .child(actions),
        )
        .child(status)
        .children(permissions)
}

/// Asks before hiding `name` from the hotbar.
fn confirm_hide(settings: Entity<AppSettings>, name: String, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::button::ButtonVariant;
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let (settings, target) = (settings.clone(), name.clone());
        dialog
            .confirm()
            .title(t!("settings-hide-title", name = name.as_str()))
            .width(px(440.))
            .description(t!("settings-hide-message"))
            .ok_text(t!("settings-hide"))
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, _, cx| {
                let target = target.clone();
                settings.update(cx, |settings, cx| {
                    settings.edit(cx, |s| s.clusters.hotbar.set_hidden(&target, true))
                });
                true
            })
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kubach_core::kubeconfig::Source;

    use super::{Removal, matches_filter, removal};

    #[test]
    fn removal_follows_the_source() {
        let imported = Source::Imported {
            vault: PathBuf::from("/cfg/vault.kubach"),
            id: "abc".into(),
            name: "prod".into(),
        };
        assert_eq!(
            removal(Some(&imported)),
            Removal::Vault { id: "abc".into() }
        );
        let linked = Source::Linked(PathBuf::from("/home/me/team.yaml"));
        assert_eq!(
            removal(Some(&linked)),
            Removal::Unlink(PathBuf::from("/home/me/team.yaml"))
        );
        // The user's own kubeconfig is never changed: hide only.
        for own in [
            Source::Home(PathBuf::from("/home/me/.kube/config")),
            Source::Env(PathBuf::from("/tmp/kubeconfig")),
        ] {
            assert_eq!(removal(Some(&own)), Removal::Hide);
        }
        assert_eq!(removal(None), Removal::Hide);
    }

    #[test]
    fn filter_matches_every_word() {
        assert!(matches_filter(
            "minikube https://192.168.49.2 kubeconfig",
            ""
        ));
        assert!(matches_filter(
            "minikube https://192.168.49.2 kubeconfig",
            "MINI 192"
        ));
        assert!(!matches_filter(
            "minikube https://192.168.49.2 kubeconfig",
            "mini vault"
        ));
    }
}
