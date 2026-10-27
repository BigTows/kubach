//! Sidebar: sections, kind ids and the per-context navigation state.

use std::collections::HashMap;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::*;
use kubach_core::gitops::overview as gitops_overview;
use kubach_i18n::t;
use kubach_ui::{NavNode, NavTreeState};

use crate::cluster::CatalogState;
use crate::screens::{AccessPage, BrowseSection, HelmPage, KindEntry, custom_groups};

use super::Workspace;

/// Swaps per-context state on a context switch: stores `current` for
/// `leaving` and takes out what was stored for `entering`.
pub(super) fn swap_context_state<T>(
    memory: &mut HashMap<String, T>,
    leaving: Option<String>,
    current: T,
    entering: Option<&str>,
) -> Option<T> {
    if let Some(leaving) = leaving {
        memory.insert(leaving, current);
    }
    memory.remove(entering?)
}

/// Sidebar id prefix of a resource kind: `kind:{group}/{kind}`.
pub(super) const KIND_PREFIX: &str = "kind:";

pub(super) fn kind_nav_id(entry: &KindEntry) -> SharedString {
    format!(
        "{KIND_PREFIX}{}/{}",
        entry.kind.api.group, entry.kind.api.kind
    )
    .into()
}

/// `(group, kind)` of a sidebar kind id.
pub(super) fn parse_kind_nav_id(id: &str) -> Option<(&str, &str)> {
    id.strip_prefix(KIND_PREFIX)?.rsplit_once('/')
}

/// Sidebar state of a context seen for the first time.
pub(super) fn default_nav_state() -> NavTreeState {
    let mut state = NavTreeState::default();
    state.set_expanded(Section::Workloads.nav_id(), true);
    state.selected = Some(Section::Overview.nav_id().into());
    state
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Section {
    Overview,
    Events,
    /// Graph of how the application's objects are connected (#324).
    Topology,
    Cluster,
    Workloads,
    Network,
    /// Gateway API group, only when the cluster serves its CRDs.
    GatewayApi,
    Storage,
    Config,
    AccessControl,
    CustomResources,
    /// Helm group: Releases and Charts.
    Helm,
    /// Kustomize builds (a leaf).
    Kustomize,
    /// Argo CD / Flux overview (a leaf, only when the cluster serves them).
    GitOps,
    /// Leaf inside the Network group.
    PortForwards,
    /// Leaves inside the Access control group.
    Permissions,
    SecurityChecks,
    /// Leaves inside the Helm group.
    HelmReleases,
    HelmCharts,
}

impl Section {
    pub(super) const ALL: [Section; 19] = [
        Section::Overview,
        Section::Events,
        Section::Topology,
        Section::Cluster,
        Section::Workloads,
        Section::Network,
        Section::GatewayApi,
        Section::Storage,
        Section::Config,
        Section::AccessControl,
        Section::CustomResources,
        Section::Helm,
        Section::Kustomize,
        Section::GitOps,
        Section::PortForwards,
        Section::Permissions,
        Section::SecurityChecks,
        Section::HelmReleases,
        Section::HelmCharts,
    ];

    pub(super) fn label(self) -> String {
        match self {
            Section::Overview => t!("nav-overview"),
            Section::Events => t!("nav-events"),
            Section::Topology => t!("nav-topology"),
            Section::Cluster => t!("nav-cluster"),
            Section::Workloads => t!("nav-workloads"),
            Section::Network => t!("nav-network"),
            Section::GatewayApi => t!("nav-gateway-api"),
            Section::Storage => t!("nav-storage"),
            Section::Config => t!("nav-config"),
            Section::AccessControl => t!("nav-access-control"),
            Section::CustomResources => t!("nav-custom-resources"),
            Section::Helm => t!("nav-helm"),
            Section::Kustomize => t!("nav-kustomize"),
            Section::GitOps => t!("nav-gitops"),
            Section::PortForwards => t!("nav-port-forwards"),
            Section::Permissions => t!("nav-permissions"),
            Section::SecurityChecks => t!("nav-security-checks"),
            Section::HelmReleases => t!("nav-helm-releases"),
            Section::HelmCharts => t!("nav-helm-charts"),
        }
    }

    /// The name of the section on "← Back" (#395).
    pub(super) fn history_label(self) -> String {
        match self {
            Section::HelmReleases => t!("nav-back-helm-releases"),
            Section::HelmCharts => t!("nav-back-helm-charts"),
            section => section.label(),
        }
    }

    /// Sections that list resources through the [`ResourceBrowser`].
    pub(super) fn browse(self) -> Option<BrowseSection> {
        match self {
            Section::Cluster => Some(BrowseSection::Cluster),
            Section::Workloads => Some(BrowseSection::Workloads),
            Section::Network => Some(BrowseSection::Network),
            Section::GatewayApi => Some(BrowseSection::GatewayApi),
            Section::Storage => Some(BrowseSection::Storage),
            Section::Config => Some(BrowseSection::Config),
            Section::AccessControl => Some(BrowseSection::AccessControl),
            Section::CustomResources => Some(BrowseSection::Custom),
            Section::Overview
            | Section::Events
            | Section::Topology
            | Section::Helm
            | Section::Kustomize
            | Section::GitOps
            | Section::PortForwards
            | Section::Permissions
            | Section::SecurityChecks
            | Section::HelmReleases
            | Section::HelmCharts => None,
        }
    }

    pub(super) fn from_browse(section: BrowseSection) -> Self {
        match section {
            BrowseSection::Cluster => Section::Cluster,
            BrowseSection::Workloads => Section::Workloads,
            BrowseSection::Network => Section::Network,
            BrowseSection::GatewayApi => Section::GatewayApi,
            BrowseSection::Storage => Section::Storage,
            BrowseSection::Config => Section::Config,
            BrowseSection::AccessControl => Section::AccessControl,
            BrowseSection::Custom => Section::CustomResources,
        }
    }

    /// Sidebar row id (a leaf for Overview / Port forwards, a group otherwise).
    pub(super) fn nav_id(self) -> &'static str {
        match self {
            Section::Overview => "overview",
            Section::Events => "events",
            Section::Topology => "topology",
            Section::Cluster => "cluster",
            Section::Workloads => "workloads",
            Section::Network => "network",
            Section::GatewayApi => "gateway-api",
            Section::Storage => "storage",
            Section::Config => "config",
            Section::AccessControl => "access-control",
            Section::CustomResources => "custom-resources",
            Section::Helm => "helm",
            Section::Kustomize => "kustomize",
            Section::GitOps => "gitops",
            Section::PortForwards => "port-forwards",
            Section::Permissions => "permissions",
            Section::SecurityChecks => "security-checks",
            Section::HelmReleases => "helm-releases",
            Section::HelmCharts => "helm-charts",
        }
    }

    pub(super) fn icon(self) -> Lucide {
        match self {
            Section::Overview => Lucide::LayoutDashboard,
            Section::Events => Lucide::Bell,
            Section::Topology => Lucide::Workflow,
            Section::Cluster => Lucide::Globe,
            Section::Workloads => Lucide::Cpu,
            Section::Network => Lucide::Network,
            Section::GatewayApi => Lucide::Waypoints,
            Section::Storage => Lucide::HardDrive,
            Section::Config => Lucide::Settings2,
            Section::AccessControl => Lucide::User,
            Section::CustomResources => Lucide::Frame,
            Section::Helm => Lucide::GalleryVerticalEnd,
            Section::Kustomize => Lucide::FolderClosed,
            Section::GitOps => Lucide::RotateCw,
            Section::PortForwards => Lucide::ArrowRightLeft,
            Section::Permissions => Lucide::CircleUser,
            Section::SecurityChecks => Lucide::TriangleAlert,
            Section::HelmReleases => Lucide::GalleryVerticalEnd,
            Section::HelmCharts => Lucide::Search,
        }
    }

    /// The group a leaf screen is listed in: Network for Port forwards.
    pub(super) fn parent(self) -> Option<Section> {
        Section::ALL
            .into_iter()
            .find(|group| group.screens().contains(&self))
    }

    /// Screens listed as leaves inside a group, before or after its kinds.
    pub(super) fn screens(self) -> &'static [Section] {
        match self {
            Section::Network => &[Section::PortForwards],
            Section::AccessControl => &[Section::Permissions, Section::SecurityChecks],
            Section::Helm => &[Section::HelmReleases, Section::HelmCharts],
            _ => &[],
        }
    }

    /// The page of the Helm screen this section shows.
    pub(super) fn helm_page(self) -> Option<HelmPage> {
        match self {
            Section::HelmReleases => Some(HelmPage::Releases),
            Section::HelmCharts => Some(HelmPage::Charts),
            _ => None,
        }
    }

    /// The page of the access screen this section shows.
    pub(super) fn access_page(self) -> Option<AccessPage> {
        match self {
            Section::Permissions => Some(AccessPage::Permissions),
            Section::SecurityChecks => Some(AccessPage::SecurityChecks),
            _ => None,
        }
    }
}

impl Workspace {
    /// Saves the sidebar of the context left and restores the one entered.
    pub(super) fn on_cluster_change(&mut self, cx: &mut Context<Self>) {
        let active = self.cluster.read(cx).active().map(|c| c.name.clone());
        if active != self.context {
            let state = self.nav.read(cx).state().clone();
            let restored = swap_context_state(
                &mut self.nav_memory,
                self.context.take(),
                state,
                active.as_deref(),
            )
            .unwrap_or_else(default_nav_state);
            let selected = restored
                .selected
                .clone()
                .unwrap_or_else(|| Section::Overview.nav_id().into());
            self.nav.update(cx, |nav, cx| nav.set_state(restored, cx));
            self.context = active;
            // "Back" never leads into another context's places.
            self.history.clear();
            self.open(&selected, cx);
        }
        self.refresh_nav(cx);
        cx.notify();
    }

    /// Rebuilds the sidebar tree from the catalog.
    pub(super) fn refresh_nav(&mut self, cx: &mut Context<Self>) {
        let catalog = match self.cluster.read(cx).catalog() {
            CatalogState::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        };
        let leaf = |entry: KindEntry| NavNode::leaf(kind_nav_id(&entry), entry.title.clone());
        let screens = |section: Section| -> Vec<NavNode> {
            section
                .screens()
                .iter()
                .map(|screen| NavNode::leaf(screen.nav_id(), screen.label()).icon(screen.icon()))
                .collect()
        };
        let nested: Vec<Section> = Section::ALL
            .into_iter()
            .flat_map(|section| section.screens().iter().copied())
            .collect();
        // GitOps only shows up when the cluster serves Argo CD or Flux.
        let gitops = catalog
            .as_ref()
            .is_some_and(|catalog| !gitops_overview::served(catalog).is_empty());
        // Gateway API only shows up when the cluster serves its CRDs.
        let gateway_api = catalog
            .as_ref()
            .is_some_and(|catalog| !BrowseSection::GatewayApi.kinds(catalog).is_empty());
        let nodes: Vec<NavNode> = Section::ALL
            .into_iter()
            .filter(|section| !nested.contains(section))
            .filter(|section| *section != Section::GitOps || gitops)
            .filter(|section| *section != Section::GatewayApi || gateway_api)
            .map(|section| {
                let node = match (section.browse(), &catalog) {
                    (None, _) if section.screens().is_empty() => {
                        NavNode::leaf(section.nav_id(), section.label())
                    }
                    (None, _) => {
                        NavNode::group(section.nav_id(), section.label(), screens(section))
                    }
                    (Some(BrowseSection::Custom), Some(catalog)) => NavNode::group(
                        section.nav_id(),
                        section.label(),
                        custom_groups(catalog)
                            .into_iter()
                            .map(|(group, entries)| {
                                let children = entries.into_iter().map(leaf).collect();
                                NavNode::group(format!("crd:{group}"), group, children)
                            })
                            .collect(),
                    ),
                    (Some(browse), Some(catalog)) => {
                        let kinds = browse.kinds(catalog).into_iter().map(leaf);
                        // Port forwards follow Network's kinds; the access
                        // screens come before the RBAC kinds.
                        let children: Vec<NavNode> = if section == Section::Network {
                            kinds.chain(screens(section)).collect()
                        } else {
                            screens(section).into_iter().chain(kinds).collect()
                        };
                        NavNode::group(section.nav_id(), section.label(), children)
                    }
                    (Some(_), None) => {
                        NavNode::group(section.nav_id(), section.label(), screens(section))
                    }
                };
                node.icon(section.icon())
            })
            .collect();
        self.nav.update(cx, |nav, cx| nav.set_nodes(nodes, cx));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{HelmPage, Section, swap_context_state};

    #[test]
    fn helm_pages_are_sidebar_leaves() {
        let screens = Section::Helm.screens();
        let pages: Vec<_> = screens.iter().map(|s| s.helm_page()).collect();
        assert_eq!(pages, [Some(HelmPage::Releases), Some(HelmPage::Charts)]);
        assert!(screens.iter().all(|s| s.browse().is_none()));
        assert_eq!(Section::Helm.helm_page(), None);
        // Leaf ids are unique across the sidebar.
        let mut ids: Vec<_> = Section::ALL.iter().map(|s| s.nav_id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), Section::ALL.len());
    }

    #[test]
    fn context_state_is_restored_on_return() {
        let mut memory = HashMap::new();
        // First visit to "a": nothing stored yet.
        assert_eq!(swap_context_state(&mut memory, None, 0, Some("a")), None);
        // a → b: a's state is kept, b is new.
        assert_eq!(
            swap_context_state(&mut memory, Some("a".into()), 1, Some("b")),
            None
        );
        // b → a: a comes back, b's state is kept.
        assert_eq!(
            swap_context_state(&mut memory, Some("b".into()), 2, Some("a")),
            Some(1)
        );
        // a → no context → b.
        assert_eq!(
            swap_context_state(&mut memory, Some("a".into()), 3, None),
            None
        );
        assert_eq!(swap_context_state(&mut memory, None, 9, Some("b")), Some(2));
        assert_eq!(memory.get("a"), Some(&3));
    }
}
