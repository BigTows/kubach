//! Topology screen (#324): how ingress, services, workloads and their
//! dependencies are connected in the selected namespaces, drawn by
//! [`GraphView`] from the model of `kubach_core::topology` (ADR 0016).
//!
//! The graph follows the cluster: the watches of [`TopologySource`] wake
//! a debounced rebuild that runs off the UI thread; the canvas keeps its
//! layout across rebuilds (stable ids). A node opens a compact details
//! panel (its summary); "Open details" or a double click shows the full
//! details of its object there instead (#394), the same view and actions
//! as in the resource browser, with "← <node>" back to the summary.
//! Problems (dangling references, Services without ready pods) are tinted
//! and listed under "Issues". The "Network policies" layer (#325)
//! marks pods isolated by NetworkPolicies and Service traffic they block,
//! and answers "can this reach that" in the details. The "Traffic" layer
//! (#326) draws measured traffic from the Prometheus metrics of Istio,
//! Linkerd or Cilium Hubble. "Export" (#327) copies the graph as Mermaid or
//! DOT, or saves it as SVG or PNG.

mod connections;
mod export;
mod map;
mod policy;
mod traffic;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::ConnectionState;
use kubach_core::cache::ResourceCache;
use kubach_core::discovery::ApiCatalog;
use kubach_core::topology::netpol::{self, Direction, PodGroup};
use kubach_core::topology::{
    BuildOptions, Detail, Filter, LabelSelector, Node, NodeClass, NodeId, Severity,
    TopologySnapshot, TopologySource,
};
use kubach_i18n::t;
use kubach_ui::{
    ChipList, EmptyState, FilterBar, GraphData, GraphTarget, GraphView, GraphViewEvent,
    KeyValueList, MultiSelection, Panel, SIDE_PANEL_CONTENT_MIN, SegmentTab, SegmentTabs,
    SidePanel, SidePanelState, StatusBadge, StatusDot, Tone,
};

use super::access::Reveal;
use super::activity::{ScreenActivity, watch_changes};
use super::browser::{DetailsEvent, ObjectActions, ResourceDetails, Subject, Where};
use super::namespace_picker::{NamespacePicker, Namespaces};
use super::namespaces::selected_names;
use super::panel_width;
use super::prometheus_link::PrometheusRegistry;
use crate::cluster::{CatalogState, Cluster};

pub use connections::Connections;
use map::{CLASSES, MapOptions};
use traffic::TrafficLayer;

/// Changes that come in a burst are built once.
const DEBOUNCE: Duration = Duration::from_millis(150);
const DETAILS_WIDTH: Pixels = px(360.);

/// Watches of a topology with a debounced background rebuild.
pub(super) struct Live {
    cache: ResourceCache,
    /// Namespaces watched (`None`: all).
    namespaces: Option<Vec<String>>,
    source: Arc<TopologySource>,
    building: bool,
    dirty: bool,
    _listener: Task<()>,
    _build: Option<Task<()>>,
}

/// How an owner of a [`Live`] takes part in rebuilds.
pub(super) trait LiveOwner: Sized + 'static {
    fn live(&mut self) -> Option<&mut Live>;
    fn build_options(&self) -> BuildOptions;
    fn apply(&mut self, snapshot: TopologySnapshot, cx: &mut Context<Self>);
}

impl Live {
    /// Subscribes to every kind of the topology in `namespaces`; the
    /// first build starts at once.
    pub(super) fn new<T: LiveOwner>(
        cache: &ResourceCache,
        catalog: &ApiCatalog,
        namespaces: Option<Vec<String>>,
        cx: &mut Context<T>,
    ) -> Self {
        let source = Arc::new(TopologySource::subscribe(
            cache,
            catalog,
            namespaces.as_deref(),
        ));
        let listener = watch_changes(source.changes(), cx, Self::schedule);
        Self {
            cache: cache.clone(),
            namespaces,
            source,
            building: false,
            dirty: false,
            _listener: listener,
            _build: None,
        }
    }

    pub(super) fn same(&self, cache: &ResourceCache, namespaces: &Option<Vec<String>>) -> bool {
        self.cache.same_as(cache) && &self.namespaces == namespaces
    }

    /// Asks for a rebuild: one runs [`DEBOUNCE`] later, off the UI thread,
    /// and again while changes keep coming in during a build.
    pub(super) fn schedule<T: LiveOwner>(owner: &mut T, cx: &mut Context<T>) {
        let Some(live) = owner.live() else {
            return;
        };
        live.dirty = true;
        if live.building {
            return;
        }
        live.building = true;
        let source = live.source.clone();
        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DEBOUNCE).await;
                let options = this.update(cx, |this, _| {
                    let options = this.build_options();
                    let live = this.live()?;
                    live.dirty = false;
                    Some(options)
                });
                let Ok(Some(options)) = options else {
                    return;
                };
                let built = source.clone();
                let snapshot = cx
                    .background_spawn(async move { built.snapshot(&options) })
                    .await;
                let again = this.update(cx, |this, cx| {
                    // A newer source (namespaces, context) took over.
                    if !this
                        .live()
                        .is_some_and(|live| Arc::ptr_eq(&live.source, &source))
                    {
                        return false;
                    }
                    this.apply(snapshot, cx);
                    let Some(live) = this.live() else {
                        return false;
                    };
                    if !live.dirty {
                        live.building = false;
                    }
                    live.dirty
                });
                if !matches!(again, Ok(true)) {
                    return;
                }
            }
        });
        if let Some(live) = owner.live() {
            live._build = Some(task);
        }
    }
}

/// The "Can reach…" form in the details of a pod or workload.
struct ReachForm {
    target: Entity<SelectState<SearchableVec<String>>>,
    port: Entity<InputState>,
    /// What the select offers: `(label, node)`.
    items: Vec<(String, NodeId)>,
}

/// Which side panel shows next to the graph.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    None,
    Issues,
}

pub struct TopologyScreen {
    cluster: Entity<Cluster>,
    namespaces: Entity<Namespaces>,
    /// On screen: watches run only then.
    active: bool,
    picker: Option<Entity<NamespacePicker>>,
    search: Option<Entity<InputState>>,
    labels: Option<Entity<InputState>>,
    /// Matches of the search: `(position, count)`.
    matches: Option<(usize, usize)>,
    /// Text of the label selector and whether it parses.
    selector: Result<Option<LabelSelector>, String>,
    detail: Detail,
    /// Classes switched off by the chips.
    hidden: BTreeSet<NodeClass>,
    /// The network policy layer is shown.
    network_policies: bool,
    /// The traffic layer: on or off, its source and last window.
    traffic: TrafficLayer,
    reach: Option<ReachForm>,
    live: Option<Live>,
    /// Context the shown graph belongs to.
    context: Option<String>,
    /// The graph as built from the watches.
    base: Option<Arc<TopologySnapshot>>,
    /// What is shown: `base` with the traffic layer laid over it.
    snapshot: Option<Arc<TopologySnapshot>>,
    graph: Entity<GraphView>,
    /// Id of the node whose details are open.
    selected: Option<String>,
    /// The next graph is fitted to the view (detail or filters changed).
    refit: bool,
    /// Centers a node once the side panel has resized the canvas.
    _center: Option<Task<()>>,
    side: Side,
    details_panel: Entity<SidePanelState>,
    /// Full details of the selected node's object (#394): a view of its
    /// own, embedded cached, so its updates do not redraw the graph.
    details: Entity<ResourceDetails>,
    /// Confirms and runs what the details ask for, as in the browser.
    actions: Entity<ObjectActions>,
    /// The selected node whose details (not summary) are wanted.
    opened: Option<String>,
    /// The details show `opened`, holding the watch of its object.
    details_shown: bool,
    /// Requests of the details' buttons; subscribed on first render, which
    /// has the window their confirmations need.
    details_events: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Reveal> for TopologyScreen {}

impl LiveOwner for TopologyScreen {
    fn live(&mut self) -> Option<&mut Live> {
        self.live.as_mut()
    }

    fn build_options(&self) -> BuildOptions {
        let kinds = (!self.hidden.is_empty()).then(|| {
            CLASSES
                .into_iter()
                .filter(|class| !self.hidden.contains(class))
                .collect()
        });
        BuildOptions {
            detail: self.detail,
            filter: Filter {
                namespaces: None,
                kinds,
                labels: self.selector.clone().ok().flatten(),
            },
            include_unreferenced: false,
            network_policies: self.network_policies,
        }
    }

    fn apply(&mut self, snapshot: TopologySnapshot, cx: &mut Context<Self>) {
        self.base = Some(Arc::new(snapshot));
        self.show(cx);
    }
}

impl TopologyScreen {
    /// Draws the built graph, with the traffic layer when it has data.
    fn show(&mut self, cx: &mut Context<Self>) {
        let Some(base) = self.base.clone() else {
            return;
        };
        let snapshot = match (&self.traffic.data, self.traffic.on) {
            (Some(data), true) => traffic::overlay(&base, data),
            _ => (*base).clone(),
        };
        let data = map::graph_data(&snapshot.graph, MapOptions { groups: true });
        if let Some(selected) = &self.selected
            && !snapshot.loading
            && !snapshot
                .graph
                .nodes()
                .iter()
                .any(|node| node.id.as_str() == selected)
        {
            self.selected = None;
        }
        self.snapshot = Some(Arc::new(snapshot));
        self.set_graph(data, cx);
        if std::mem::take(&mut self.refit) {
            self.graph.update(cx, |graph, cx| graph.fit_to_view(cx));
        }
        cx.notify();
    }
}

impl TopologyScreen {
    pub fn new(
        cluster: Entity<Cluster>,
        namespaces: Entity<Namespaces>,
        cx: &mut Context<Self>,
    ) -> Self {
        let graph = cx.new(GraphView::new);
        let details = cx.new(|cx| ResourceDetails::new(cluster.clone(), cx));
        let actions = cx.new(|cx| ObjectActions::new(cluster.clone(), &details, cx));
        let mut subscriptions = vec![
            // Links of the details (owners, "Managed by") go to the workspace.
            cx.subscribe(&details, |_, _, reveal: &Reveal, cx| {
                cx.emit(reveal.clone())
            }),
            // A run turns the action buttons of the details off, then on;
            // the graph is not redrawn for it.
            cx.observe(&actions, |this, actions, cx| {
                let running = actions.read(cx).running();
                this.details
                    .update(cx, |details, cx| details.set_running(running, cx));
            }),
            cx.observe(&cluster, |this, _, cx| this.sync(cx)),
            cx.observe(&namespaces, |this, _, cx| this.sync(cx)),
            cx.subscribe(&graph, |this, _, event: &GraphViewEvent, cx| {
                this.on_graph_event(event, cx)
            }),
        ];
        // The traffic layer follows the context's Prometheus.
        if let Some(registry) = PrometheusRegistry::try_global(cx) {
            subscriptions.push(cx.observe(&registry, |this, _, cx| {
                if this.traffic.on {
                    this.sync_traffic(cx);
                    cx.notify();
                }
            }));
        }
        Self {
            cluster,
            namespaces,
            active: false,
            picker: None,
            search: None,
            labels: None,
            matches: None,
            selector: Ok(None),
            detail: Detail::Workloads,
            hidden: BTreeSet::new(),
            network_policies: false,
            traffic: TrafficLayer::default(),
            reach: None,
            live: None,
            context: None,
            base: None,
            snapshot: None,
            graph,
            selected: None,
            refit: false,
            _center: None,
            side: Side::None,
            details_panel: panel_width::remembered("topology.details", DETAILS_WIDTH, cx),
            details,
            actions,
            opened: None,
            details_shown: false,
            details_events: None,
            _subscriptions: subscriptions,
        }
    }

    /// Brings the watches in line with the connection and namespaces.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let cluster = self.cluster.read(cx);
        let cache = cluster.cache().cloned();
        let context = cluster.active().map(|context| context.name.clone());
        let catalog = match cluster.catalog() {
            CatalogState::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        };
        let scope = self.namespaces.read(cx).scope().cloned();
        // The graph of another context is never shown, not even until the
        // first build of this one.
        if context != self.context {
            self.context = context;
            self.base = None;
            self.snapshot = None;
            self.selected = None;
            self.set_graph(GraphData::default(), cx);
        }
        let (Some(cache), Some(catalog), Some(scope), true) = (cache, catalog, scope, self.active)
        else {
            self.live = None;
            self.sync_traffic(cx);
            self.sync_details(cx);
            cx.notify();
            return;
        };
        let names = selected_names(&scope.selection);
        if !self
            .live
            .as_ref()
            .is_some_and(|live| live.same(&cache, &names))
        {
            self.live = Some(Live::new(&cache, &catalog, names, cx));
        }
        self.sync_traffic(cx);
        self.sync_details(cx);
        cx.notify();
    }

    /// Keeps the full details in line with the node they were opened for:
    /// shown while it is selected and the screen is on screen; another
    /// node, a closed panel or a hidden screen drops them and their watch.
    fn sync_details(&mut self, cx: &mut Context<Self>) {
        if self.opened.is_some() && self.opened != self.selected {
            self.opened = None;
        }
        let wanted = self.opened.clone().filter(|_| self.active);
        match (wanted, self.details_shown) {
            (Some(id), false) => {
                let Some(node) = self.node(&id) else {
                    return;
                };
                let Some(object) = map::open_object(node) else {
                    self.opened = None;
                    return;
                };
                let home = format!("{} {}", node.kind.label(), node.name);
                let running = self.actions.read(cx).running();
                let opened = self
                    .details
                    .update(cx, |details, cx| details.open(&object, home, running, cx));
                if opened {
                    self.details_shown = true;
                } else {
                    self.opened = None;
                }
            }
            (None, true) => {
                self.details
                    .update(cx, |details, cx| details.set_target(None, cx));
                self.details_shown = false;
            }
            _ => {}
        }
    }

    /// Shows the full details of node `id` in the side panel, selecting it.
    fn open_details(&mut self, id: String, cx: &mut Context<Self>) {
        if self.node(&id).and_then(map::open_object).is_none() {
            return;
        }
        if self.selected.as_deref() != Some(id.as_str()) {
            let target = GraphTarget::Node(id.clone().into());
            self.graph
                .update(cx, |graph, cx| graph.select(Some(target), cx));
        }
        self.selected = Some(id.clone());
        self.opened = Some(id);
        self.sync_details(cx);
        cx.notify();
    }

    /// Back and Close of the details; their actions run as in the browser.
    fn on_details_event(
        &mut self,
        _: &Entity<ResourceDetails>,
        event: &DetailsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DetailsEvent::Close => self.close_details(cx),
            DetailsEvent::Back => {
                self.opened = None;
                self.sync_details(cx);
                cx.notify();
            }
            _ => {
                let where_ = match event {
                    DetailsEvent::Action { object, .. } | DetailsEvent::Scale { object, .. } => {
                        Where::namespace(object.metadata.namespace.as_deref())
                    }
                    _ => Where::cluster(),
                };
                self.actions.update(cx, |actions, cx| {
                    actions.on_details_event(event, Subject::of, where_, window, cx)
                });
            }
        }
    }

    fn set_graph(&mut self, data: GraphData, cx: &mut Context<Self>) {
        let selected = self.selected.clone();
        self.graph.update(cx, |graph, cx| {
            graph.set_data(data, cx);
            graph.select(selected.map(|id| GraphTarget::Node(id.into())), cx);
        });
    }

    /// Rebuilds with new options and fits the result to the view.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.refit = true;
        Live::schedule(self, cx);
        cx.notify();
    }

    fn on_graph_event(&mut self, event: &GraphViewEvent, cx: &mut Context<Self>) {
        match event {
            GraphViewEvent::Selected(Some(GraphTarget::Node(id))) => {
                self.selected = Some(id.to_string());
            }
            GraphViewEvent::Selected(_) => self.selected = None,
            GraphViewEvent::Activated(GraphTarget::Node(id)) => {
                self.open_details(id.to_string(), cx);
            }
            _ => return,
        }
        cx.notify();
    }

    fn node(&self, id: &str) -> Option<&Node> {
        self.snapshot
            .as_ref()?
            .graph
            .nodes()
            .iter()
            .find(|node| node.id.as_str() == id)
    }
    /// Selects a node and brings it into view (from the issues and links).
    fn focus_node(&mut self, id: String, cx: &mut Context<Self>) {
        let target = GraphTarget::Node(id.clone().into());
        self.selected = Some(id);
        self.graph
            .update(cx, |graph, cx| graph.select(Some(target.clone()), cx));
        // The details panel opening narrows the canvas: center in the
        // narrowed one, a couple of frames later.
        self._center = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            this.update(cx, |this, cx| {
                this.graph
                    .update(cx, |graph, cx| graph.center_on(&target, cx))
            })
            .ok();
        }));
        cx.notify();
    }

    fn close_details(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.graph.update(cx, |graph, cx| graph.select(None, cx));
        cx.notify();
    }

    fn select_all_namespaces(&mut self, cx: &mut Context<Self>) {
        self.namespaces.update(cx, |namespaces, cx| {
            namespaces.apply(MultiSelection::All, cx);
        });
    }

    /// Creates the namespace picker and the inputs, which need the window.
    fn ensure_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_none() {
            let namespaces = self.namespaces.clone();
            self.picker = Some(cx.new(|cx| {
                NamespacePicker::new("topology-namespace-picker", namespaces, window, cx)
            }));
        }
        if self.search.is_none() {
            let search = FilterBar::search_state("", window, cx);
            self._subscriptions
                .extend(crate::i18n::localized_placeholder(
                    &search,
                    || t!("topology-search-placeholder"),
                    window,
                    cx,
                ));
            self._subscriptions.push(cx.subscribe(
                &search,
                |this, search, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        let query = search.read(cx).value().to_string();
                        let count = this.graph.update(cx, |graph, cx| graph.search(&query, cx));
                        this.matches = (!query.trim().is_empty()).then_some((0, count));
                        cx.notify();
                    }
                    InputEvent::PressEnter { secondary, .. } => {
                        let step = this.graph.update(cx, |graph, cx| {
                            if *secondary {
                                graph.search_previous(cx)
                            } else {
                                graph.search_next(cx)
                            }
                        });
                        if step.is_some() {
                            this.matches = step;
                        }
                        cx.notify();
                    }
                    _ => {}
                },
            ));
            self.search = Some(search);
        }
        if self.labels.is_none() {
            let labels = cx.new(|cx| InputState::new(window, cx));
            self._subscriptions
                .extend(crate::i18n::localized_placeholder(
                    &labels,
                    || t!("topology-labels-placeholder"),
                    window,
                    cx,
                ));
            self._subscriptions.push(cx.subscribe(
                &labels,
                |this, labels, event: &InputEvent, cx| {
                    if let InputEvent::Change = event {
                        let text = labels.read(cx).value().trim().to_owned();
                        this.selector = if text.is_empty() {
                            Ok(None)
                        } else {
                            LabelSelector::parse(&text).map(Some)
                        };
                        if this.selector.is_ok() {
                            this.rebuild(cx);
                        }
                        cx.notify();
                    }
                },
            ));
            self.labels = Some(labels);
        }
    }

    /// Creates the "Can reach…" form for the selected pod or workload and
    /// keeps its targets in line with the graph.
    fn ensure_reach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        if !self.network_policies || snapshot.policies.is_none() {
            return;
        }
        let Some(node) = self.selected.as_deref().and_then(|id| self.node(id)) else {
            return;
        };
        if !netpol::applies_to(node) {
            return;
        }
        let items = policy::targets(&snapshot.graph, &node.id);
        let labels = || {
            SearchableVec::new(
                items
                    .iter()
                    .map(|(label, _)| label.clone())
                    .collect::<Vec<_>>(),
            )
        };
        match &mut self.reach {
            Some(form) => {
                if form.items != items {
                    let labels = labels();
                    form.target
                        .update(cx, |state, cx| state.set_items(labels, window, cx));
                    form.items = items;
                }
            }
            None => {
                let labels = labels();
                let target =
                    cx.new(|cx| SelectState::new(labels, None, window, cx).searchable(true));
                let port = cx.new(|cx| InputState::new(window, cx));
                self._subscriptions
                    .extend(crate::i18n::localized_placeholder(
                        &port,
                        || t!("topology-port-placeholder"),
                        window,
                        cx,
                    ));
                self._subscriptions.push(cx.subscribe(
                    &target,
                    |_, _, _: &SelectEvent<SearchableVec<String>>, cx| cx.notify(),
                ));
                self._subscriptions
                    .push(cx.subscribe(&port, |_, _, event: &InputEvent, cx| {
                        if let InputEvent::Change = event {
                            cx.notify();
                        }
                    }));
                self.reach = Some(ReachForm {
                    target,
                    port,
                    items,
                });
            }
        }
    }

    /// Focuses the search field (`⌘F`).
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.search {
            search.update(cx, |search, cx| search.focus(window, cx));
        }
    }

    /// Opens the namespace list (`⌘⇧N`, the status bar).
    pub fn open_namespaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = &self.picker {
            picker.update(cx, |picker, cx| picker.open(window, cx));
        }
    }

    fn render_placeholder(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let cluster = self.cluster.read(cx);
        let state = match cluster.state() {
            ConnectionState::Disconnected => {
                EmptyState::new(IconName::Globe, t!("browser-no-cluster"))
            }
            ConnectionState::Connecting => {
                EmptyState::new(IconName::LoaderCircle, t!("browser-connecting"))
            }
            ConnectionState::Error(error) => {
                EmptyState::new(IconName::CircleX, t!("browser-not-connected"))
                    .description(error.clone())
            }
            ConnectionState::Connected { .. } => match cluster.catalog() {
                CatalogState::Pending => {
                    EmptyState::new(IconName::LoaderCircle, t!("browser-discovering"))
                }
                CatalogState::Error(error) => {
                    EmptyState::new(IconName::CircleX, t!("browser-discovery-failed"))
                        .description(error.clone())
                }
                CatalogState::Ready(_) => match &self.snapshot {
                    None => EmptyState::new(IconName::LoaderCircle, t!("screen-loading")),
                    Some(snapshot) if snapshot.graph.nodes().is_empty() => {
                        if snapshot.loading {
                            EmptyState::new(IconName::LoaderCircle, t!("screen-loading"))
                        } else {
                            let filtered =
                                !self.hidden.is_empty() || matches!(self.selector, Ok(Some(_)));
                            let empty = EmptyState::new(IconName::Inbox, t!("topology-nothing"));
                            if filtered {
                                empty.description(t!("topology-nothing-filtered"))
                            } else if self
                                .live
                                .as_ref()
                                .is_some_and(|live| live.namespaces.is_some())
                            {
                                empty
                                    .description(t!("topology-nothing-selected-namespaces"))
                                    .action(
                                        Button::new("topology-all-namespaces")
                                            .small()
                                            .outline()
                                            .label(t!("screen-show-all-namespaces"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.select_all_namespaces(cx)
                                            })),
                                    )
                            } else {
                                empty.description(t!("topology-nothing-any"))
                            }
                        }
                    }
                    Some(_) => return None,
                },
            },
        };
        Some(state.into_any_element())
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let detail = SegmentTabs::new(
            "topology-detail",
            vec![
                SegmentTab::new(t!("topology-detail-workloads"))
                    .tooltip(t!("topology-detail-workloads-tooltip")),
                SegmentTab::new(t!("topology-detail-pods"))
                    .tooltip(t!("topology-detail-pods-tooltip")),
            ],
            match self.detail {
                Detail::Workloads => 0,
                Detail::Pods => 1,
            },
        )
        .on_select(cx.listener(|this, ix: &usize, _, cx| {
            this.detail = if *ix == 0 {
                Detail::Workloads
            } else {
                Detail::Pods
            };
            this.rebuild(cx);
        }));
        let chips = CLASSES.into_iter().enumerate().map(|(ix, class)| {
            let shown = !self.hidden.contains(&class);
            Button::new(("topology-class", ix))
                .xsmall()
                .outline()
                .label(map::class_label(class))
                .selected(shown)
                .tooltip(if shown {
                    t!("topology-class-hide")
                } else {
                    t!("topology-class-show")
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.hidden.remove(&class) {
                        this.hidden.insert(class);
                    }
                    this.rebuild(cx);
                }))
        });
        let snapshot = self.snapshot.as_ref();
        let mut summary = Vec::new();
        if let Some(snapshot) = snapshot {
            summary.push(t!(
                "topology-size",
                nodes = snapshot.graph.nodes().len(),
                edges = snapshot.graph.edges().len()
            ));
            if snapshot.loading {
                summary.push(t!("screen-loading"));
            }
        }
        if let Some((position, count)) = self.matches {
            summary.push(match count {
                0 => t!("topology-no-matches"),
                count => t!(
                    "topology-match",
                    position = position.min(count - 1) + 1,
                    count = count
                ),
            });
        }
        let issues = snapshot.map_or(0, |s| s.graph.diagnostics().len());
        let errors = snapshot.map_or(0, |s| {
            s.graph
                .diagnostics()
                .iter()
                .filter(|d| d.severity() == Severity::Error)
                .count()
        });
        let policies_button = Button::new("topology-network-policies")
            .small()
            .outline()
            .icon(Lucide::Shield)
            .label(t!("topology-policies"))
            .selected(self.network_policies)
            .tooltip(t!("topology-policies-tooltip", note = policy::scope_note()))
            .on_click(cx.listener(|this, _, _, cx| {
                this.network_policies = !this.network_policies;
                Live::schedule(this, cx);
                cx.notify();
            }));
        let issues_button = Button::new("topology-issues")
            .small()
            .outline()
            .icon(IconName::TriangleAlert)
            .label(t!("topology-issues", count = issues))
            .selected(self.side == Side::Issues)
            .disabled(issues == 0 && self.side != Side::Issues)
            .tooltip(if errors > 0 {
                t!("topology-broken-references", count = errors)
            } else {
                t!("topology-issues-tooltip")
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.side = match this.side {
                    Side::Issues => Side::None,
                    Side::None => Side::Issues,
                };
                if this.side == Side::Issues {
                    this.selected = None;
                    this.graph.update(cx, |graph, cx| graph.select(None, cx));
                }
                cx.notify();
            }));
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(detail)
            .child(div().flex().flex_wrap().gap_1().children(chips))
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(summary.join(" · ")),
            )
            .child(policies_button)
            .child(self.traffic_button(cx))
            .child(issues_button)
            .child(export::button(self, cx))
            .into_any_element()
    }

    /// What the network policy layer covers, while it is on.
    fn render_policy_note(&self, cx: &App) -> Option<AnyElement> {
        if !self.network_policies {
            return None;
        }
        let theme = cx.theme();
        let count = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.policies.as_ref())
            .map(|policies| policies.policies().len());
        let lead = match count {
            None => t!("topology-policies-loading"),
            Some(0) => t!("topology-policies-none"),
            Some(count) => t!("topology-policies-count", count = count),
        };
        Some(
            div()
                .id("topology-policy-note")
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(
                    gpui_kit::component::Icon::new(Lucide::Shield)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(div().flex_1().min_w_0().child(t!(
                    "topology-policies-note",
                    lead = lead,
                    note = policy::scope_note()
                )))
                .into_any_element(),
        )
    }

    /// The "Network policy" section of a pod's or workload's details.
    fn render_policy_section(
        &self,
        snapshot: &TopologySnapshot,
        node: &Node,
        cx: &mut Context<Self>,
    ) -> Option<Panel> {
        let policies = snapshot.policies.as_ref()?;
        let pods = PodGroup::of(node).filter(|_| netpol::applies_to(node))?;
        let theme = cx.theme().clone();
        let muted = |text: String| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(text)
        };
        let summary = KeyValueList::new()
            .row(
                "Ingress",
                div().whitespace_normal().child(policy::isolation_summary(
                    policies,
                    &pods,
                    Direction::Ingress,
                )),
            )
            .row(
                "Egress",
                div().whitespace_normal().child(policy::isolation_summary(
                    policies,
                    &pods,
                    Direction::Egress,
                )),
            );
        let selecting = policy::policy_lines(policies, &pods);
        let listed = (!selecting.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .children(selecting.into_iter().map(|(name, lines)| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(name))
                        .children(lines.into_iter().map(&muted))
                }))
        });

        let mut reach = div().flex().flex_col().gap_2().child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(t!("topology-reach")),
        );
        if let Some(form) = &self.reach {
            reach = reach.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            Select::new(&form.target)
                                .id("topology-reach-target")
                                .small()
                                .placeholder(t!("topology-reach-target"))
                                .accessibility_label(t!("topology-reach-target-label"))
                                .search_placeholder(t!("topology-reach-search")),
                        ),
                    )
                    .child(
                        div()
                            .w(px(110.))
                            .child(Input::new(&form.port).small().cleanable(true)),
                    ),
            );
            let target = form.target.read(cx).selected_value().and_then(|label| {
                form.items
                    .iter()
                    .find(|(item, _)| item == label)
                    .and_then(|(_, id)| snapshot.graph.node(id))
            });
            let port_text = form.port.read(cx).value().trim().to_owned();
            match (target, policy::parse_port(&port_text)) {
                (None, _) => {}
                (Some(_), Err(error)) => {
                    if !port_text.is_empty() {
                        reach = reach.child(muted(error));
                    }
                }
                (Some(target), Ok((port, protocol))) => {
                    let answers =
                        policy::reach(&snapshot.graph, policies, node, target, &port, protocol);
                    if answers.is_empty() {
                        reach = reach.child(muted(t!("topology-reach-no-pods")));
                    }
                    for answer in answers {
                        let verdict = &answer.verdict;
                        let (label, tone) = if verdict.allowed {
                            (t!("topology-reach-allowed"), Tone::Success)
                        } else {
                            (t!("topology-reach-blocked"), Tone::Danger)
                        };
                        let port = verdict
                            .port
                            .map(|port| format!("{} {port}", verdict.protocol))
                            .unwrap_or_default();
                        let head = div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(StatusBadge::new(label, tone))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .whitespace_normal()
                                    .child(match &answer.via {
                                        Some(via) => {
                                            t!("topology-reach-via", port = port, via = via.clone())
                                        }
                                        None => port,
                                    }),
                            );
                        let side = |title: String, explanation: &netpol::Explanation| {
                            div()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(StatusDot::new(if explanation.allowed {
                                            Tone::Success
                                        } else {
                                            Tone::Danger
                                        }))
                                        .child(div().text_xs().child(title)),
                                )
                                .children(explanation.notes.iter().cloned().map(&muted))
                        };
                        reach = reach.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(head)
                                .child(side(t!("topology-reach-egress"), &verdict.egress_side))
                                .child(side(t!("topology-reach-ingress"), &verdict.ingress_side)),
                        );
                    }
                }
            }
        }

        Some(
            Panel::new().title(t!("topology-policy-section")).child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(summary)
                    .child(reach)
                    .children(listed)
                    .child(muted(policy::scope_note())),
            ),
        )
    }

    /// Watches that keep failing (no access, lasting outage): the cache
    /// reports them only past its threshold (#316). Kinds the cluster
    /// doesn't serve (Gateway API without CRDs) are not errors.
    fn render_errors(&self, cx: &App) -> Option<AnyElement> {
        let snapshot = self.snapshot.as_ref()?;
        if snapshot.errors.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let mut kinds: Vec<&str> = snapshot
            .errors
            .iter()
            .map(|(kind, _, _)| kind.kind())
            .collect();
        kinds.dedup();
        let (_, _, first) = &snapshot.errors[0];
        let text = if snapshot.errors.iter().all(|(_, _, e)| e.forbidden) {
            t!("topology-error-no-access", kinds = kinds.join(", "))
        } else if first.connection {
            t!("browser-banner-connection-lost")
        } else {
            t!("topology-error-cant-refresh", kinds = kinds.join(", "))
        };
        Some(
            div()
                .id("topology-errors")
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.warning)
                .text_sm()
                .text_color(theme.warning)
                .whitespace_normal()
                .tooltip({
                    let message = first.message.clone();
                    move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(message.clone())
                            .build(window, cx)
                    }
                })
                .child(text)
                .into_any_element(),
        )
    }

    fn render_issues(&self, snapshot: &TopologySnapshot, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let graph = &snapshot.graph;
        let mut diagnostics: Vec<_> = graph.diagnostics().iter().collect();
        diagnostics.sort_by_key(|d| std::cmp::Reverse(d.severity()));
        let rows = diagnostics.into_iter().enumerate().map(|(ix, diagnostic)| {
            let id = diagnostic.node.as_str().to_owned();
            div()
                .id(("topology-issue", ix))
                .flex()
                .items_start()
                .gap_2()
                .px_2()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|row| row.bg(theme.list_hover))
                .on_click(cx.listener(move |this, _, _, cx| this.focus_node(id.clone(), cx)))
                .child(
                    div()
                        .pt_1()
                        .child(StatusDot::new(map::severity_tone(diagnostic.severity()))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .whitespace_normal()
                        .child(map::diagnostic_text(diagnostic, graph)),
                )
        });
        let body = div()
            .id("topology-issues")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_0p5()
            .children(rows)
            .when(graph.diagnostics().is_empty(), |list| {
                list.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(t!("topology-no-problems")),
                )
            });
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t!("topology-issues", count = graph.diagnostics().len())),
            )
            .child(
                Button::new("topology-issues-close")
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .tooltip(t!("screen-close"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.side = Side::None;
                        cx.notify();
                    })),
            );
        SidePanel::new("topology-side-panel", &self.details_panel)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(header)
                    .child(body),
            )
            .into_any_element()
    }

    fn render_details(
        &self,
        snapshot: &TopologySnapshot,
        node: &Node,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let graph = &snapshot.graph;
        // Only a node with an object in the cluster has details to open.
        let open = map::open_object(node).map(|_| {
            let id = node.id.as_str().to_owned();
            Button::new("topology-open")
                .small()
                .outline()
                .icon(IconName::ExternalLink)
                .tooltip(t!("topology-open-details"))
                .on_click(cx.listener(move |this, _, _, cx| this.open_details(id.clone(), cx)))
        });
        let header = div()
            .flex()
            .items_start()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(node.name.clone()),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        match &node.namespace {
                            Some(namespace) => format!("{} · {namespace}", map::subtitle(node)),
                            None => map::subtitle(node),
                        },
                    )),
            )
            .children(open)
            .child(
                Button::new("topology-close")
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .tooltip(t!("screen-close"))
                    .on_click(cx.listener(|this, _, _, cx| this.close_details(cx))),
            );

        let problems: Vec<_> = graph.diagnostics_for(&node.id).collect();
        let problems =
            (!problems.is_empty()).then(|| {
                Panel::new().title(t!("topology-problems")).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(problems.into_iter().map(|diagnostic| {
                            div()
                                .flex()
                                .items_start()
                                .gap_2()
                                .child(div().pt_1().child(StatusDot::new(map::severity_tone(
                                    diagnostic.severity(),
                                ))))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .whitespace_normal()
                                        .child(map::diagnostic_text(diagnostic, graph)),
                                )
                        })),
                )
            });

        let mut facts = KeyValueList::new();
        let mut any_fact = false;
        let mut fact = |key: String, value: String| {
            any_fact = true;
            facts = std::mem::take(&mut facts).row(key, div().whitespace_normal().child(value));
        };
        if let Some(pods) = node.attrs.pods {
            fact(
                t!("topology-fact-pods-ready"),
                t!("topology-of", count = pods.ready, total = pods.total),
            );
        }
        if let Some(desired) = node.attrs.desired_replicas {
            fact(t!("topology-fact-replicas"), desired.to_string());
        }
        if let Some(phase) = &node.attrs.phase {
            fact(t!("topology-fact-phase"), phase.clone());
        }
        if let Some(service) = &node.attrs.service {
            if let Some(ip) = &service.cluster_ip {
                fact(t!("topology-fact-cluster-ip"), ip.clone());
            }
            if let Some(name) = &service.external_name {
                fact(t!("topology-fact-external-name"), name.clone());
            }
            if !service.ports.is_empty() {
                let ports = service
                    .ports
                    .iter()
                    .map(|port| {
                        let target = port
                            .target_port
                            .as_ref()
                            .map(|t| format!("→{t}"))
                            .unwrap_or_default();
                        format!("{}{target}", port.port)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                fact(t!("topology-fact-ports"), ports);
            }
        }
        if !node.attrs.hosts.is_empty() {
            fact(t!("topology-fact-hosts"), node.attrs.hosts.join(", "));
        }
        if let Some(class) = &node.attrs.class_name {
            fact(t!("topology-fact-class"), class.clone());
        }
        let facts = any_fact.then(|| Panel::new().title(t!("topology-overview")).child(facts));

        let network = self.render_policy_section(snapshot, node, cx);
        let mut link_ix: usize = 0;
        let mut links = |title: String, edges: Vec<(String, &Node, Option<String>)>| {
            if edges.is_empty() {
                return None;
            }
            let count = edges.len();
            let rows = edges.into_iter().map(|(kind, other, label)| {
                link_ix += 1;
                let id = other.id.as_str().to_owned();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(StatusDot::new(map::tone(graph, other)))
                    .child(
                        div().min_w_0().overflow_hidden().child(
                            Button::new(("topology-link", link_ix))
                                .link()
                                .small()
                                .label(format!("{} {}", other.kind.label(), other.name))
                                .tooltip(t!("topology-show-on-graph"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.focus_node(id.clone(), cx)
                                })),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_right()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(match label {
                                Some(label) => format!("{kind} · {label}"),
                                None => kind,
                            }),
                    )
            });
            Some(
                Panel::new()
                    .title(format!("{title} · {count}"))
                    .child(div().flex().flex_col().gap_1().children(rows)),
            )
        };
        let incoming: Vec<_> = graph
            .edges_to(&node.id)
            .filter_map(|edge| {
                let other = graph.node(&edge.from)?;
                Some((
                    map::edge_kind_label(edge.kind),
                    other,
                    map::edge_label(edge),
                ))
            })
            .collect();
        let outgoing: Vec<_> = graph
            .edges_from(&node.id)
            .filter_map(|edge| {
                let other = graph.node(&edge.to)?;
                Some((
                    map::edge_kind_label(edge.kind),
                    other,
                    map::edge_label(edge),
                ))
            })
            .collect();
        let incoming = links(t!("topology-incoming"), incoming);
        let outgoing = links(t!("topology-outgoing"), outgoing);

        let labels = (!node.labels.is_empty()).then(|| {
            Panel::new()
                .title(t!("section-labels"))
                .child(ChipList::new(
                    "topology-labels",
                    node.labels.iter().map(|(k, v)| (k.clone(), v.clone())),
                ))
        });
        let badge = match node.presence {
            kubach_core::topology::Presence::Missing => {
                Some(StatusBadge::new(t!("topology-missing"), Tone::Danger))
            }
            kubach_core::topology::Presence::Unknown => {
                Some(StatusBadge::new(t!("topology-not-loaded"), Tone::Neutral))
            }
            kubach_core::topology::Presence::Present => None,
        };
        let body = div()
            .id("topology-details")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .children(badge.map(|badge| div().flex().child(badge)))
            .children(problems)
            .children(network)
            .children(facts)
            .children(incoming)
            .children(outgoing)
            .children(labels);
        SidePanel::new("topology-side-panel", &self.details_panel)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(header)
                    .child(body),
            )
            .into_any_element()
    }
}

impl ScreenActivity for TopologyScreen {
    /// Shown, the screen starts its watches; hidden, it stops them.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            self.sync(cx);
        }
    }
}

impl Render for TopologyScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_filters(window, cx);
        self.ensure_reach(window, cx);
        self.sync_details(cx);
        if self.details_events.is_none() {
            let details = self.details.clone();
            self.details_events = Some(cx.subscribe_in(&details, window, Self::on_details_event));
        }
        let theme = cx.theme().clone();
        let mut header = FilterBar::new();
        if let Some(picker) = &self.picker {
            header = header.filter(picker.clone());
        }
        if let Some(search) = &self.search {
            header = header.search(search);
        }
        if let Some(labels) = &self.labels {
            let invalid = self.selector.as_ref().err().cloned();
            header = header.action(
                div()
                    .id("topology-labels-filter")
                    .w(px(200.))
                    .when_some(invalid, |this, error| {
                        this.rounded_md()
                            .border_1()
                            .border_color(theme.danger)
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(error.clone())
                                    .build(window, cx)
                            })
                    })
                    .child(Input::new(labels).small().cleanable(true)),
            );
        }
        let root = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_4()
            .p_8()
            .child(header)
            .child(self.render_toolbar(cx))
            .children(self.render_policy_note(cx))
            .children(self.render_traffic_note(cx))
            .children(self.render_errors(cx));
        if let Some(placeholder) = self.render_placeholder(cx) {
            return root.child(div().flex_1().child(placeholder));
        }
        let Some(snapshot) = self.snapshot.clone() else {
            return root;
        };
        let side = match (&self.selected, self.side) {
            // Cached: the details redraw on their own (ages, events,
            // metrics, a new version of the object), not with the graph.
            (Some(_), _) if self.details_shown => Some(
                SidePanel::new("topology-side-panel", &self.details_panel)
                    .child(super::cached(&self.details, cx))
                    .into_any_element(),
            ),
            (Some(id), _) => self
                .node(id)
                .cloned()
                .map(|node| self.render_details(&snapshot, &node, cx)),
            (None, Side::Issues) => Some(self.render_issues(&snapshot, cx)),
            (None, Side::None) => None,
        };
        let canvas = div()
            .flex_1()
            .min_w_0()
            .when(side.is_some(), |this| this.min_w(SIDE_PANEL_CONTENT_MIN))
            .min_h_0()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .child(super::cached(&self.graph, cx));
        root.child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap_4()
                .child(canvas)
                .children(side),
        )
    }
}
