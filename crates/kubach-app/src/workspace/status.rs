//! Status bar and the cluster status shown on the Overview.

use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{Disableable as _, IconName, Sizable as _, WindowExt as _};
use gpui_kit::*;
use kubach_core::ConnectionState;
use kubach_core::status_bar::{self, McpStatus};
use kubach_core::tools::cli::{self, CliTool};
use kubach_i18n::t;
use kubach_ui::{EmptyState, MultiSelection, OperationsList, StatusBar, StatusBarItem, Tone};

use crate::agent::{self, ServerStatus};
use crate::cluster::CatalogState;
use crate::settings::{SettingsSection, SettingsTarget, open_settings};
use crate::vault;

use super::Workspace;
use super::history::Transition;
use super::nav::Section;

pub(super) fn state_tone(state: &ConnectionState) -> Tone {
    match state {
        ConnectionState::Disconnected => Tone::Neutral,
        ConnectionState::Connecting => Tone::Warning,
        ConnectionState::Connected { .. } => Tone::Success,
        ConnectionState::Error(_) => Tone::Danger,
    }
}

/// How often the status bar checks whether the cache is still syncing.
pub(super) const SYNC_POLL: Duration = Duration::from_millis(500);

/// An empty state without clusters, taking dropped kubeconfig files.
pub(super) fn empty_drop_zone(empty: EmptyState, cx: &App) -> AnyElement {
    vault::drop_zone(
        "empty-drop",
        div().flex_1().flex().flex_col().child(empty),
        cx,
    )
    .into_any_element()
}

impl Workspace {
    /// Re-renders when the cache starts or stops syncing.
    pub(super) fn poll_sync(&mut self, cx: &mut Context<Self>) {
        let sync = self
            .cluster
            .read(cx)
            .cache()
            .map(|cache| cache.sync_summary())
            .unwrap_or_default();
        if sync != self.sync {
            self.sync = sync;
            cx.notify();
        }
    }

    /// Bottom bar: connection, namespaces and sync on the left; port
    /// forwards, MCP and the version on the right.
    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> StatusBar {
        let this = cx.entity().downgrade();
        let cluster = self.cluster.read(cx);
        let context = cluster.active().map(|context| context.name.clone());
        let state = cluster.state().clone();
        let label = if cluster.is_loading() {
            t!("status-loading-kubeconfig")
        } else {
            status_bar::cluster_label(context.as_deref(), &state).to_string()
        };
        let names = self.context_names(cx);

        let menu_this = this.clone();
        let active = context.clone();
        let mut cluster_item = StatusBarItem::new("status-cluster", label)
            .dot(state_tone(&state))
            .tooltip(match &state {
                ConnectionState::Error(error) => t!("status-error", error = error.to_string()),
                _ => t!("status-switch-context"),
            });
        if !names.is_empty() {
            cluster_item = cluster_item.menu(move |menu, _, _| {
                names
                    .iter()
                    .fold(menu.label(t!("status-switch-context")), |menu, name| {
                        let (this, name) = (menu_this.clone(), name.clone());
                        menu.item(
                            PopupMenuItem::new(name.clone())
                                .checked(active.as_ref() == Some(&name))
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |this, cx| this.switch_to(name.clone(), cx))
                                        .ok();
                                }),
                        )
                    })
            });
        }

        let connected = matches!(state, ConnectionState::Connected { .. });
        let pickable = self.list_screen().is_some();
        let namespace = connected
            .then(|| {
                self.namespaces
                    .read(cx)
                    .scope()
                    .map(|scope| match &scope.selection {
                        MultiSelection::All => status_bar::namespace_label::<SharedString>(None),
                        MultiSelection::Items(items) => {
                            status_bar::namespace_label(Some(items.as_slice()))
                        }
                    })
            })
            .flatten()
            .map(|label| {
                let item =
                    StatusBarItem::new("status-namespace", label.to_string()).icon(Lucide::Layers);
                if !pickable {
                    return item.tooltip(t!("status-namespaces-tooltip"));
                }
                let this = this.clone();
                item.tooltip(t!("status-namespaces-tooltip-change"))
                    .on_click(move |_, window, cx| {
                        this.update(cx, |this, cx| this.pick_namespaces(window, cx))
                            .ok();
                    })
            });

        // Retrying watches get a still icon and no loading bar (#316).
        let sync = self.sync.label().filter(|_| connected).map(|label| {
            let icon = if self.sync.is_loading() {
                Lucide::LoaderCircle
            } else {
                Lucide::RefreshCw
            };
            StatusBarItem::new("status-sync", label.to_string())
                .icon(icon)
                .tooltip(
                    self.sync
                        .details()
                        .map(|details| details.to_string())
                        .unwrap_or_default(),
                )
        });

        // Long operations keep going after their toast is hidden (#382).
        let running = self.operations_running;
        let operations = (running > 0).then(|| {
            StatusBarItem::new(
                "status-operations",
                t!("status-operations", count = running),
            )
            .icon(Lucide::LoaderCircle)
            .tooltip(t!("status-operations-tooltip"))
            .on_click(|_, window, cx| OperationsList::open(window, cx))
        });
        // Started, done or failed, for a few seconds (#459).
        let notice = self
            .operation_notice
            .as_ref()
            .map(|notice| notice.item("status-operation-notice"));

        let forwards =
            status_bar::forwards_label(self.forwards.read(cx).list().len()).map(|label| {
                let this = this.clone();
                StatusBarItem::new("status-forwards", label.to_string())
                    .icon(Lucide::ArrowRightLeft)
                    .tooltip(t!("status-forwards-tooltip"))
                    .on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| {
                            this.navigated(Transition::StatusBar, cx);
                            this.show_section(Section::PortForwards, cx)
                        })
                        .ok();
                    })
            });

        let terminal = StatusBarItem::new("status-terminal", t!("status-terminal"))
            .icon(IconName::SquareTerminal)
            .tooltip(t!("status-terminal-tooltip"))
            .on_click({
                let this = this.clone();
                move |_, window, cx| {
                    this.update(cx, |this, cx| this.open_terminal(window, cx))
                        .ok();
                }
            });

        let mcp = agent::global(cx).map(|agent| {
            let agent = agent.read(cx);
            let status = McpStatus::new(
                matches!(agent.status(), ServerStatus::Running { .. }),
                matches!(agent.status(), ServerStatus::Failed(_)),
                agent.sessions().len(),
                agent.pending_count(),
            );
            let item = StatusBarItem::new("status-mcp", status.label().to_string())
                .icon(Lucide::Bot)
                .highlight(status.needs_attention())
                .tooltip(t!("status-mcp-tooltip"))
                .on_click(|_, _, cx| {
                    open_settings(SettingsTarget::section(SettingsSection::Agent), cx)
                });
            match status {
                McpStatus::Failed => item.tone(Tone::Danger),
                _ => item,
            }
        });

        let summary = status_bar::version_summary(
            kubach_core::VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH,
            context.as_deref(),
            &state,
        );
        let version = StatusBarItem::new(
            "status-version",
            status_bar::version_label(kubach_core::VERSION),
        )
        .tooltip(t!("status-version-tooltip"))
        .on_click(move |_, window, cx| {
            use gpui_kit::component::notification::Notification;
            cx.write_to_clipboard(ClipboardItem::new_string(summary.clone()));
            window.push_notification(Notification::info(t!("status-version-copied")), cx);
        });

        // A thin bar while lists refresh (possibly shown from the cache).
        let refreshing = match &state {
            ConnectionState::Connecting => true,
            ConnectionState::Connected { .. } => {
                matches!(cluster.catalog(), CatalogState::Pending) || self.sync.is_loading()
            }
            _ => false,
        };
        StatusBar::new("status-bar")
            .loading(refreshing)
            .left(cluster_item)
            .left_some(namespace)
            .left_some(sync)
            .left_some(operations)
            .left_some(notice)
            .right_some(forwards)
            .right(terminal)
            .right_some(mcp)
            .right(version)
    }

    pub(super) fn render_cluster_status(&self, cx: &App) -> AnyElement {
        let cluster = self.cluster.read(cx);
        if cluster.is_loading() {
            return EmptyState::new(IconName::LoaderCircle, t!("workspace-loading-kubeconfig"))
                .into_any_element();
        }
        let contexts = match cluster.contexts() {
            Ok(contexts) => contexts,
            Err(error) => {
                return empty_drop_zone(
                    EmptyState::new(
                        IconName::TriangleAlert,
                        t!("workspace-kubeconfig-not-found"),
                    )
                    .description(error.clone()),
                    cx,
                );
            }
        };
        if contexts.is_empty() {
            if contexts.problems().any(|file| file.source.is_linked()) {
                return EmptyState::new(
                    IconName::TriangleAlert,
                    t!("workspace-kubeconfig-unavailable"),
                )
                .description(t!("workspace-kubeconfig-unavailable-description"))
                .action(
                    Button::new("kubeconfig-settings")
                        .outline()
                        .small()
                        .label(t!("workspace-open-settings"))
                        .on_click(|_, _, cx| {
                            open_settings(SettingsTarget::section(SettingsSection::Clusters), cx)
                        }),
                )
                .into_any_element();
            }
            return empty_drop_zone(
                EmptyState::new(IconName::Globe, t!("workspace-no-contexts"))
                    .description(t!("workspace-no-contexts-description"))
                    .action(
                        Button::new("link-kubeconfig")
                            .outline()
                            .small()
                            .label(t!("workspace-add-cluster"))
                            .on_click(|_, window, cx| vault::add_cluster(window, cx)),
                    ),
                cx,
            );
        }
        let Some(context) = cluster.active() else {
            return EmptyState::new(IconName::Globe, t!("workspace-no-cluster"))
                .description(t!("workspace-no-cluster-description"))
                .into_any_element();
        };

        match cluster.state() {
            ConnectionState::Disconnected => {
                let (entity, name) = (self.cluster.clone(), context.name.clone());
                EmptyState::new(
                    IconName::Globe,
                    t!("workspace-disconnected", context = context.name.as_str()),
                )
                .action(
                    Button::new("connect")
                        .outline()
                        .small()
                        .label(t!("workspace-connect"))
                        .on_click(move |_, _, cx| {
                            let name = name.clone();
                            entity.update(cx, |cluster, cx| cluster.connect(name, cx));
                        }),
                )
                .into_any_element()
            }
            ConnectionState::Connecting => EmptyState::new(
                IconName::LoaderCircle,
                t!("workspace-connecting", context = context.name.as_str()),
            )
            .into_any_element(),
            ConnectionState::Error(error) => {
                let entity = self.cluster.clone();
                let retry = Button::new("reconnect")
                    .outline()
                    .small()
                    .icon(IconName::RefreshCw)
                    .label(t!("workspace-retry"))
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |cluster, cx| cluster.reconnect(cx));
                    });
                let (fix, fix_error) = contexts
                    .exec_command(&context.name)
                    .is_some_and(cli::is_gke_plugin)
                    .then(|| self.render_gke_plugin_fix(cx))
                    .flatten()
                    .map_or((None, None), |(button, error)| (Some(button), error));
                EmptyState::new(
                    IconName::CircleX,
                    t!("workspace-connect-failed", context = context.name.as_str()),
                )
                .description(error.clone())
                .action(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .children(fix)
                                .child(retry),
                        )
                        .children(fix_error),
                )
                .into_any_element()
            }
            // Shown by the Overview screen (#414).
            ConnectionState::Connected { .. } => div().into_any_element(),
        }
    }

    /// A context whose `gke-gcloud-auth-plugin` is missing (#38): a button
    /// that installs it with gcloud and reconnects, or opens Settings →
    /// Tools to download gcloud first; plus why the last install failed.
    fn render_gke_plugin_fix(&self, cx: &App) -> Option<(Button, Option<Div>)> {
        if cli::gke_plugin_dir().is_some() {
            return None;
        }
        let Some(gcloud) = cli::locate(CliTool::Gcloud) else {
            let button = Button::new("gke-open-tools")
                .outline()
                .small()
                .label(t!("workspace-gke-open-tools"))
                .on_click(|_, _, cx| {
                    open_settings(SettingsTarget::section(SettingsSection::Tools), cx)
                });
            return Some((button, None));
        };
        let downloads = crate::tools::global(cx)?;
        let state = downloads.read(cx);
        let installing = state.plugin_installing();
        let failed = state.plugin_error().map(|error| {
            div()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(t!("cloud-gke-plugin-failed", error = error.to_string()))
        });
        let cluster = self.cluster.clone();
        let button = Button::new("gke-install-plugin")
            .primary()
            .small()
            .label(if installing {
                t!("cloud-gke-plugin-installing")
            } else {
                t!("cloud-gke-plugin-install")
            })
            .disabled(installing)
            .on_click(move |_, _, cx| {
                let (gcloud, cluster) = (gcloud.path.clone(), cluster.clone());
                downloads.update(cx, |downloads, cx| {
                    downloads.install_gke_plugin(
                        gcloud,
                        move |cx| cluster.update(cx, |cluster, cx| cluster.reconnect(cx)),
                        cx,
                    );
                });
            });
        Some((button, failed))
    }

    /// Whether the active context is connected: the Overview screen
    /// shows it.
    pub(super) fn connected(&self, cx: &App) -> bool {
        let cluster = self.cluster.read(cx);
        !cluster.is_loading()
            && cluster.active().is_some()
            && matches!(cluster.state(), ConnectionState::Connected { .. })
    }
}
