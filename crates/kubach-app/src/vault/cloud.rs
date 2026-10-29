//! "Import from cloud…" (#38): pick a provider (Google GKE, Yandex Cloud,
//! AWS EKS), find or download its CLI, list the clusters it can see and
//! store the checked ones in the vault through the usual import preview.
//!
//! Listing, parsing and building the kubeconfig live in
//! `kubach_core::cloud`; downloads in [`crate::tools`]. The CLI runs only
//! when this dialog asks it to (opening a tab, "Try again", "Find
//! clusters"); Kubach keeps no cloud token.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::cloud::{
    self, CloudCluster, CloudError, Discovery, EndpointKind, ExecCommands, Provider, Scope,
    Selection,
};
use kubach_core::runtime;
use kubach_core::tools::Platform;
use kubach_core::tools::cli::{self, CliBinary, CliSource};
use kubach_core::vault::ImportPlan;
use kubach_i18n::t;

use super::dialogs::{Reopen, error_line, label, open_manual_import, taken_names};
use crate::tools::{self as downloads_ui, ToolDownloads};

const DIALOG_WIDTH: Pixels = px(680.);
const SCOPES_MAX_HEIGHT: Pixels = px(150.);
const CLUSTERS_MAX_HEIGHT: Pixels = px(260.);

/// Opens the dialog in the main window.
pub fn open(window: &mut Window, cx: &mut App) {
    let form = cx.new(CloudImport::new);
    show(form, window, cx);
}

fn show(form: Entity<CloudImport>, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t!("cloud-title"))
            .w(DIALOG_WIDTH)
            .child(form.clone())
    });
}

/// Something asked of the CLI.
enum Load<T> {
    Idle,
    Loading(#[allow(dead_code)] Task<()>),
    Ready(T),
    Failed(CloudError),
}

impl<T> Load<T> {
    fn is_loading(&self) -> bool {
        matches!(self, Load::Loading(_))
    }
}

/// Clusters found and the scopes that failed.
type Found = (Vec<CloudCluster>, Vec<(Scope, CloudError)>);

/// What one provider's tab holds; kept when switching tabs.
struct ProviderTab {
    binary: Option<CliBinary>,
    discovery: Load<Discovery>,
    /// The profile chosen (yc, aws); `None` uses the CLI's active one.
    profile: Option<String>,
    scopes: HashSet<String>,
    found: Load<Found>,
    /// Checked clusters by key, with the endpoint chosen.
    checked: HashMap<String, EndpointKind>,
}

impl ProviderTab {
    fn new(provider: Provider) -> Self {
        Self {
            binary: cli::locate(provider.tool()),
            discovery: Load::Idle,
            profile: None,
            scopes: HashSet::new(),
            found: Load::Idle,
            checked: HashMap::new(),
        }
    }
}

struct CloudImport {
    provider: Provider,
    tabs: HashMap<Provider, ProviderTab>,
    downloads: Option<Entity<ToolDownloads>>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl CloudImport {
    fn new(cx: &mut Context<Self>) -> Self {
        let downloads = downloads_ui::global(cx);
        let subscriptions = downloads
            .iter()
            .map(|downloads| {
                // A finished download or plugin install changes what is found.
                cx.observe(downloads, |this: &mut Self, _, cx| {
                    this.relocate(cx);
                })
            })
            .collect();
        let mut this = Self {
            provider: Provider::Gke,
            tabs: HashMap::new(),
            downloads,
            error: None,
            _subscriptions: subscriptions,
        };
        this.select(Provider::Gke, cx);
        this
    }

    fn tab(&mut self) -> &mut ProviderTab {
        let provider = self.provider;
        self.tabs
            .entry(provider)
            .or_insert_with(|| ProviderTab::new(provider))
    }

    fn select(&mut self, provider: Provider, cx: &mut Context<Self>) {
        self.provider = provider;
        self.error = None;
        let tab = self.tab();
        if tab.binary.is_some() && matches!(tab.discovery, Load::Idle) {
            self.discover(cx);
        }
        cx.notify();
    }

    /// Looks for the CLI again (after a download, removal or refresh) and
    /// asks it what it sees when it just appeared.
    fn relocate(&mut self, cx: &mut Context<Self>) {
        let provider = self.provider;
        let tab = self.tab();
        let found = cli::locate(provider.tool());
        let appeared = tab.binary.is_none() && found.is_some();
        tab.binary = found;
        if appeared {
            self.discover(cx);
        }
        cx.notify();
    }

    fn discover(&mut self, cx: &mut Context<Self>) {
        let provider = self.provider;
        let tab = self.tab();
        let Some(binary) = tab.binary.clone() else {
            return;
        };
        let profile = tab.profile.clone();
        let handle = runtime::spawn(async move {
            cloud::discover(provider, &binary.path, profile.as_deref()).await
        });
        let task = cx.spawn(async move |this, cx| {
            let result = handle.await;
            this.update(cx, |this, cx| {
                let tab = this
                    .tabs
                    .entry(provider)
                    .or_insert_with(|| ProviderTab::new(provider));
                tab.discovery = match result {
                    Ok(Ok(discovery)) => {
                        tab.profile = discovery.profile.clone();
                        tab.scopes = discovery.preselected.iter().cloned().collect();
                        // One scope: nothing to choose.
                        if let [only] = discovery.scopes.as_slice() {
                            tab.scopes.insert(only.id.clone());
                        }
                        Load::Ready(discovery)
                    }
                    Ok(Err(error)) => Load::Failed(error),
                    Err(error) => Load::Failed(CloudError::Failed {
                        message: error.to_string(),
                    }),
                };
                cx.notify();
            })
            .ok();
        });
        let tab = self.tab();
        tab.discovery = Load::Loading(task);
        tab.found = Load::Idle;
        tab.checked.clear();
        cx.notify();
    }

    fn set_profile(&mut self, profile: String, cx: &mut Context<Self>) {
        let tab = self.tab();
        if tab.profile.as_deref() == Some(profile.as_str()) {
            return;
        }
        tab.profile = Some(profile);
        self.discover(cx);
    }

    fn find_clusters(&mut self, cx: &mut Context<Self>) {
        let provider = self.provider;
        let tab = self.tab();
        let (Some(binary), Load::Ready(discovery)) = (tab.binary.clone(), &tab.discovery) else {
            return;
        };
        let scopes: Vec<Scope> = discovery
            .scopes
            .iter()
            .filter(|scope| tab.scopes.contains(&scope.id))
            .cloned()
            .collect();
        if scopes.is_empty() {
            return;
        }
        let profile = tab.profile.clone();
        let handle = runtime::spawn(async move {
            cloud::clusters(provider, &binary.path, profile.as_deref(), &scopes).await
        });
        let task = cx.spawn(async move |this, cx| {
            let result = handle.await;
            this.update(cx, |this, cx| {
                let tab = this
                    .tabs
                    .entry(provider)
                    .or_insert_with(|| ProviderTab::new(provider));
                tab.found = match result {
                    Ok(found) => Load::Ready(found),
                    Err(error) => Load::Failed(CloudError::Failed {
                        message: error.to_string(),
                    }),
                };
                cx.notify();
            })
            .ok();
        });
        let tab = self.tab();
        tab.found = Load::Loading(task);
        tab.checked.clear();
        cx.notify();
    }

    fn toggle_cluster(&mut self, cluster: &CloudCluster, on: bool, cx: &mut Context<Self>) {
        let key = cluster.key();
        let tab = self.tab();
        if on {
            let kind = cluster
                .endpoints
                .first()
                .map_or(EndpointKind::External, |e| e.kind);
            tab.checked.insert(key, kind);
        } else {
            tab.checked.remove(&key);
        }
        cx.notify();
    }

    /// The checked clusters of every tab.
    fn selections(&self) -> Vec<Selection> {
        let mut selections = Vec::new();
        for provider in Provider::ALL {
            let Some(ProviderTab {
                found: Load::Ready((clusters, _)),
                checked,
                ..
            }) = self.tabs.get(&provider)
            else {
                continue;
            };
            for cluster in clusters {
                if let Some(endpoint) = checked.get(&cluster.key()) {
                    selections.push(Selection {
                        cluster: cluster.clone(),
                        endpoint: *endpoint,
                    });
                }
            }
        }
        selections
    }

    /// GKE clusters are checked but `gke-gcloud-auth-plugin` is missing:
    /// their contexts could not sign in, so the import waits for it.
    fn gke_plugin_missing(&self) -> bool {
        let gke = self
            .selections()
            .iter()
            .any(|selection| selection.cluster.provider() == Provider::Gke);
        gke && self
            .tabs
            .get(&Provider::Gke)
            .and_then(|tab| tab.binary.clone())
            .or_else(|| cli::locate(Provider::Gke.tool()))
            .is_none_or(|gcloud| cli::gke_plugin(&gcloud.path).is_none())
    }

    fn exec_commands(&self) -> ExecCommands {
        let binary = |provider: Provider| {
            self.tabs
                .get(&provider)
                .and_then(|tab| tab.binary.clone())
                .or_else(|| cli::locate(provider.tool()))
        };
        let gcloud = binary(Provider::Gke);
        let plugin = gcloud.as_ref().and_then(|g| cli::gke_plugin(&g.path));
        let plugin_in_path =
            kubach_core::which::which(&cli::gke_plugin_name(), &kubach_core::env::path()).is_some();
        ExecCommands::for_binaries(
            gcloud.as_ref(),
            plugin.as_deref(),
            plugin_in_path,
            binary(Provider::Yandex).as_ref(),
            binary(Provider::Eks).as_ref(),
        )
    }

    /// Opens the import preview; "Back" there returns here.
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selections = self.selections();
        if selections.is_empty() {
            self.error = Some(t!("cloud-none-selected").into());
            cx.notify();
            return;
        }
        if self.gke_plugin_missing() {
            self.error = Some(t!("cloud-gke-plugin-required").into());
            cx.notify();
            return;
        }
        let config = cloud::build_kubeconfig(&selections, &self.exec_commands());
        let plan = ImportPlan::from_kubeconfig(config, &taken_names(None, cx));
        let this = cx.entity();
        let back: Reopen = Rc::new(move |window, cx| show(this.clone(), window, cx));
        window.close_dialog(cx);
        open_manual_import(plan, back, window, cx);
    }

    fn render_cli(&self, tab: &ProviderTab, cx: &mut Context<Self>) -> Div {
        let tool = self.provider.tool();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let mono = theme.mono_font_family.clone();
        let line = div().flex().flex_wrap().items_center().gap_2().text_xs();
        let refresh = Button::new("cloud-refresh")
            .ghost()
            .xsmall()
            .label(t!("cloud-refresh"))
            .on_click(cx.listener(|this, _, _, cx| {
                let provider = this.provider;
                this.tabs.remove(&provider);
                this.select(provider, cx);
            }));
        let Some(binary) = &tab.binary else {
            let downloadable = Platform::current().is_some_and(|p| tool.downloadable(p));
            let state = self.downloads.as_ref().map(|d| d.read(cx));
            let running = state.and_then(|d| d.cli_running(tool));
            let error = state.and_then(|d| d.cli_error(tool));
            let body = if !downloadable {
                line.child(div().text_color(muted).child(t!(
                    "cloud-cli-cannot-download",
                    tool = tool.title(),
                    url = tool.install_page()
                )))
                .child(refresh)
            } else if let (Some(progress), Some(downloads)) = (running, &self.downloads) {
                line.child(downloads_ui::cli_progress(
                    "cloud-cli-progress",
                    downloads,
                    tool,
                    progress,
                ))
            } else {
                let downloads = self.downloads.clone();
                line.child(
                    div()
                        .text_color(muted)
                        .child(t!("cloud-cli-missing", tool = tool.title())),
                )
                .child(
                    Button::new("cloud-cli-download")
                        .outline()
                        .xsmall()
                        .label(t!("cloud-cli-download", tool = tool.title()))
                        .tooltip(t!("cloud-cli-download-tooltip"))
                        .disabled(downloads.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(downloads) = &downloads {
                                downloads.update(cx, |d, cx| d.start_cli(tool, cx));
                            }
                        }),
                )
                .child(refresh)
                .children(error.map(|error| div().w_full().text_color(theme.danger).child(error)))
            };
            return div().flex().flex_col().gap_1().child(body);
        };
        let where_ = match binary.source {
            CliSource::Path => t!(
                "cloud-cli-in-path",
                path = binary.path.display().to_string()
            ),
            CliSource::Downloaded => {
                t!(
                    "cloud-cli-downloaded",
                    path = binary.path.display().to_string()
                )
            }
        };
        let mut column = div().flex().flex_col().gap_1().child(
            line.child(
                div()
                    .min_w_0()
                    .text_color(muted)
                    .font_family(mono.clone())
                    .child(where_),
            )
            .child(refresh),
        );
        if self.provider == Provider::Gke {
            column = column.children(self.render_gke_plugin(binary, cx));
        }
        column
    }

    /// Missing `gke-gcloud-auth-plugin`: what to run, and a button that
    /// runs it.
    fn render_gke_plugin(&self, gcloud: &CliBinary, cx: &mut Context<Self>) -> Option<Div> {
        let state = self.downloads.as_ref().map(|d| d.read(cx));
        let installing = state.is_some_and(ToolDownloads::plugin_installing);
        let error = state.and_then(ToolDownloads::plugin_error);
        if cli::gke_plugin(&gcloud.path).is_some() && !installing {
            return None;
        }
        let theme = cx.theme();
        let downloads = self.downloads.clone();
        let path = gcloud.path.clone();
        let python_hint = cfg!(target_os = "macos") && error.is_some();
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Alert::warning("cloud-gke-plugin", t!("cloud-gke-plugin-missing")).small())
                .child(
                    div().flex().items_center().gap_2().child(
                        Button::new("cloud-gke-plugin-install")
                            .outline()
                            .xsmall()
                            .label(if installing {
                                t!("cloud-gke-plugin-installing")
                            } else {
                                t!("cloud-gke-plugin-install")
                            })
                            .disabled(installing || downloads.is_none())
                            .on_click(move |_, _, cx| {
                                if let Some(downloads) = &downloads {
                                    let path = path.clone();
                                    downloads
                                        .update(cx, |d, cx| d.install_gke_plugin(path, |_| {}, cx));
                                }
                            }),
                    ),
                )
                .children(error.map(|error| {
                    div()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(t!("cloud-gke-plugin-failed", error = error.to_string()))
                }))
                .when(python_hint, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t!("cloud-gke-python-hint")),
                    )
                }),
        )
    }

    fn render_failure(
        &self,
        error: &CloudError,
        profile: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let tool = self.provider.tool();
        let theme = cx.theme();
        let (alert, commands) = match error {
            CloudError::NotLoggedIn { message } => (
                Alert::warning(
                    "cloud-login",
                    t!("cloud-not-logged-in", tool = tool.command()),
                )
                .title(message.clone())
                .small(),
                self.provider.login_commands(profile),
            ),
            other => (
                Alert::error(
                    "cloud-failed",
                    t!(
                        "cloud-error",
                        tool = tool.command(),
                        error = other.to_string()
                    ),
                )
                .small(),
                Vec::new(),
            ),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(alert)
            .children(commands.into_iter().map(|command| {
                div()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .child(command)
            }))
            .child(
                div().child(
                    Button::new("cloud-try-again")
                        .outline()
                        .xsmall()
                        .label(t!("cloud-try-again"))
                        .on_click(cx.listener(|this, _, _, cx| this.discover(cx))),
                ),
            )
    }

    fn render_discovery(&self, tab: &ProviderTab, cx: &mut Context<Self>) -> Option<Div> {
        let tool = self.provider.tool();
        let muted = cx.theme().muted_foreground;
        match &tab.discovery {
            Load::Idle => None,
            Load::Loading(_) => Some(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!("cloud-loading", tool = tool.command())),
            ),
            Load::Failed(error) => Some(self.render_failure(error, tab.profile.as_deref(), cx)),
            Load::Ready(discovery) => Some(self.render_scopes(tab, discovery, cx)),
        }
    }

    fn render_scopes(
        &self,
        tab: &ProviderTab,
        discovery: &Discovery,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let scopes_label = match self.provider {
            Provider::Gke => t!("cloud-scopes-gke"),
            Provider::Yandex => t!("cloud-scopes-yandex"),
            Provider::Eks => t!("cloud-scopes-eks"),
        };
        let profiles = (self.provider.has_profiles() && discovery.profiles.len() > 1).then(|| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(label(t!("cloud-profile"), cx))
                .child(div().flex().flex_wrap().gap_1().children(
                    discovery.profiles.iter().enumerate().map(|(ix, profile)| {
                        let chosen = tab.profile.as_deref() == Some(profile.as_str());
                        let name = profile.clone();
                        let button = Button::new(("cloud-profile", ix))
                            .xsmall()
                            .label(profile.clone());
                        let button = if chosen {
                            button.primary()
                        } else {
                            button.outline()
                        };
                        button.on_click(
                            cx.listener(move |this, _, _, cx| this.set_profile(name.clone(), cx)),
                        )
                    }),
                ))
        });
        let list = discovery.scopes.iter().enumerate().map(|(ix, scope)| {
            let id = scope.id.clone();
            Checkbox::new(("cloud-scope", ix))
                .label(scope.label.clone())
                .checked(tab.scopes.contains(&scope.id))
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    let tab = this.tab();
                    if *checked {
                        tab.scopes.insert(id.clone());
                    } else {
                        tab.scopes.remove(&id);
                    }
                    cx.notify();
                }))
        });
        let loading = tab.found.is_loading();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(discovery.account.clone().map(|account| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("cloud-account", account = account))
            }))
            .children(profiles)
            .child(label(scopes_label, cx))
            .child(if discovery.scopes.is_empty() {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("cloud-no-scopes"))
                    .into_any_element()
            } else {
                div()
                    .id("cloud-scopes")
                    .max_h(SCOPES_MAX_HEIGHT)
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .children(list)
                    .into_any_element()
            })
            .child(
                div().child(
                    Button::new("cloud-find")
                        .outline()
                        .small()
                        .label(if loading {
                            t!("cloud-finding")
                        } else {
                            t!("cloud-find-clusters")
                        })
                        .disabled(loading || tab.scopes.is_empty())
                        .on_click(cx.listener(|this, _, _, cx| this.find_clusters(cx))),
                ),
            )
    }

    fn render_clusters(&self, tab: &ProviderTab, cx: &mut Context<Self>) -> Option<Div> {
        let Load::Ready((clusters, failed)) = &tab.found else {
            return match &tab.found {
                Load::Failed(error) => Some(self.render_failure(error, tab.profile.as_deref(), cx)),
                _ => None,
            };
        };
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let rows = clusters.iter().enumerate().map(|(ix, cluster)| {
            let chosen = tab.checked.get(&cluster.key()).copied();
            let details = [
                cluster.scope.as_str(),
                cluster.location.as_str(),
                cluster.status.as_str(),
                cluster.version.as_deref().unwrap_or_default(),
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
            let toggle = cluster.clone();
            let endpoints = (cluster.endpoints.len() > 1).then(|| {
                div()
                    .flex()
                    .gap_1()
                    .children(cluster.endpoints.iter().map(|endpoint| {
                        let kind = endpoint.kind;
                        let key = cluster.key();
                        let label = match kind {
                            EndpointKind::External => t!("cloud-endpoint-external"),
                            EndpointKind::Internal => t!("cloud-endpoint-internal"),
                            EndpointKind::Dns => t!("cloud-endpoint-dns"),
                        };
                        let id = SharedString::from(format!("cloud-endpoint-{ix}-{kind:?}"));
                        let button = Button::new(id)
                            .xsmall()
                            .label(label)
                            .tooltip(endpoint.server.clone());
                        let button = if chosen == Some(kind) {
                            button.primary()
                        } else {
                            button.ghost()
                        };
                        button.on_click(cx.listener(move |this, _, _, cx| {
                            this.tab().checked.insert(key.clone(), kind);
                            cx.notify();
                        }))
                    }))
            });
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            Checkbox::new(("cloud-cluster", ix))
                                .label(cluster.name.clone())
                                .checked(chosen.is_some())
                                .disabled(cluster.endpoints.is_empty())
                                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                    this.toggle_cluster(&toggle, *checked, cx)
                                })),
                        )
                        .child(
                            div()
                                .pl_6()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(details),
                        ),
                )
                .children(endpoints)
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(label(t!("cloud-clusters", count = clusters.len()), cx))
                .child(if clusters.is_empty() {
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(t!("cloud-no-clusters"))
                        .into_any_element()
                } else {
                    div()
                        .id("cloud-clusters")
                        .max_h(CLUSTERS_MAX_HEIGHT)
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(rows)
                        .into_any_element()
                })
                .children(failed.iter().map(|(scope, error)| {
                    div().text_xs().text_color(theme.danger).child(t!(
                        "cloud-scope-failed",
                        scope = scope.label.clone(),
                        error = error.to_string()
                    ))
                })),
        )
    }
}

impl Render for CloudImport {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let provider = self.provider;
        let tabs = TabBar::new("cloud-provider")
            .segmented()
            .small()
            .selected_index(
                Provider::ALL
                    .iter()
                    .position(|p| *p == provider)
                    .unwrap_or(0),
            )
            .children(Provider::ALL.iter().map(|p| Tab::new().label(p.label())))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                if let Some(provider) = Provider::ALL.get(*ix).copied() {
                    this.select(provider, cx);
                }
            }));
        // Taken out while rendering so its parts can borrow `self`.
        let tab = self
            .tabs
            .remove(&provider)
            .unwrap_or_else(|| ProviderTab::new(provider));
        let cli = self.render_cli(&tab, cx);
        let discovery = tab
            .binary
            .as_ref()
            .and_then(|_| self.render_discovery(&tab, cx));
        let clusters = self.render_clusters(&tab, cx);
        self.tabs.insert(provider, tab);
        let selected = self.selections().len();
        let gke_plugin_missing = self.gke_plugin_missing();
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(tabs)
            .child(cli)
            .children(discovery)
            .children(clusters)
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!("cloud-note", tool = provider.tool().command())),
            )
            .children(error_line(&self.error, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pt_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .when(!gke_plugin_missing, |this| {
                                this.text_color(muted)
                                    .child(t!("cloud-selected", count = selected))
                            })
                            .when(gke_plugin_missing, |this| {
                                this.text_color(danger)
                                    .child(t!("cloud-gke-plugin-required"))
                            }),
                    )
                    .child(
                        Button::new("cloud-cancel")
                            .ghost()
                            .small()
                            .label(t!("vault-cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("cloud-next")
                            .primary()
                            .small()
                            .label(t!("cloud-next"))
                            .disabled(selected == 0 || gke_plugin_missing)
                            .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::cloud::Provider;
    use kubach_core::tools::cli::CliTool;

    #[test]
    fn each_provider_has_its_cli() {
        assert_eq!(Provider::Gke.tool(), CliTool::Gcloud);
        assert_eq!(Provider::Yandex.tool(), CliTool::Yc);
        assert_eq!(Provider::Eks.tool(), CliTool::Aws);
    }
}
