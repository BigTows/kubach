//! "Connections" in the resource details (#324): a small graph of what the
//! selected object is connected to, up to two edges upstream and
//! downstream. A click on a neighbor opens it in the browser.

use std::collections::BTreeSet;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::*;
use kubach_core::ResourceKind;
use kubach_core::cache::ResourceCache;
use kubach_core::discovery::ApiCatalog;
use kubach_core::topology::{
    BuildOptions, Detail, NodeId, SourceKind, TopologyGraph, TopologySnapshot,
};
use kubach_i18n::t;
use kubach_ui::{GraphTarget, GraphView, GraphViewEvent, Panel, StatusDot};
use kube::api::DynamicObject;

use super::super::access::Reveal;
use super::map::{self, MapOptions};
use super::{Live, LiveOwner};

/// Edges followed from the object each way.
const DEPTH: usize = 2;
const HEIGHT: Pixels = px(240.);

/// The node standing for `object` in a topology: itself, or the
/// controller its pods are collapsed into (a Deployment's ReplicaSet).
fn seed_of(kind: &ResourceKind, object: &DynamicObject) -> NodeId {
    let namespace = object.metadata.namespace.as_deref();
    let collapsed = matches!(
        (kind.api.group.as_str(), kind.api.kind.as_str()),
        ("apps", "ReplicaSet") | ("batch", "Job")
    );
    let owner = object
        .metadata
        .owner_references
        .iter()
        .flatten()
        .find(|owner| owner.controller == Some(true))
        .filter(|_| collapsed);
    match owner {
        Some(owner) => {
            let group = owner
                .api_version
                .rsplit_once('/')
                .map_or("", |(group, _)| group);
            NodeId::object(&owner.kind, group, namespace, &owner.name)
        }
        None => NodeId::object(
            &kind.api.kind,
            &kind.api.group,
            namespace,
            object.metadata.name.as_deref().unwrap_or_default(),
        ),
    }
}

/// Neighbors of one object, shown in its details.
pub struct Connections {
    namespace: String,
    seed: NodeId,
    detail: Detail,
    live: Option<Live>,
    graph: Entity<GraphView>,
    /// The neighborhood drawn, for clicks.
    shown: Option<TopologyGraph>,
    loading: bool,
    /// Centers the object once the canvas has its size.
    _center: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<Reveal> for Connections {}

impl LiveOwner for Connections {
    fn live(&mut self) -> Option<&mut Live> {
        self.live.as_mut()
    }

    fn build_options(&self) -> BuildOptions {
        BuildOptions {
            detail: self.detail,
            ..BuildOptions::default()
        }
    }

    fn apply(&mut self, snapshot: TopologySnapshot, cx: &mut Context<Self>) {
        let near = map::near(&snapshot.graph, &self.seed, DEPTH);
        let before: BTreeSet<NodeId> = self
            .shown
            .iter()
            .flat_map(|graph| graph.nodes().iter().map(|node| node.id.clone()))
            .collect();
        let graph = snapshot.graph.retain(|node| near.contains(&node.id));
        let data = map::graph_data(&graph, MapOptions::default());
        let seed = GraphTarget::Node(self.seed.as_str().to_owned().into());
        let recenter = before != near;
        self.graph.update(cx, |view, cx| {
            view.set_data(data, cx);
            view.select(Some(seed.clone()), cx);
        });
        // The panel is too narrow to fit two steps each way at a readable
        // size: the object is centered and the rest is a pan away.
        if recenter {
            let graph = self.graph.downgrade();
            self._center = Some(cx.spawn(async move |_, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                graph.update(cx, |view, cx| view.center_on(&seed, cx)).ok();
            }));
        }
        self.loading = snapshot.loading;
        self.shown = Some(graph);
        cx.notify();
    }
}

impl Connections {
    /// Whether objects of this kind are drawn on a topology.
    pub fn applies(kind: &ResourceKind) -> bool {
        SourceKind::from_api(&kind.api.group, &kind.api.kind)
            .is_some_and(|source| source != SourceKind::EndpointSlice)
            && kind.namespaced
    }

    pub fn new(
        cache: &ResourceCache,
        catalog: &ApiCatalog,
        kind: &ResourceKind,
        object: &DynamicObject,
        cx: &mut Context<Self>,
    ) -> Self {
        let graph = cx.new(|cx| {
            let mut view = GraphView::new(cx);
            view.set_minimap(false, cx);
            // The details scroll past the graph until it is clicked (#460).
            view.set_wheel_needs_focus(true, cx);
            view
        });
        let subscription = cx.subscribe(&graph, |this, _, event: &GraphViewEvent, cx| {
            if let GraphViewEvent::Selected(Some(GraphTarget::Node(id)))
            | GraphViewEvent::Activated(GraphTarget::Node(id)) = event
            {
                this.open(id, cx);
            }
        });
        let namespace = object.metadata.namespace.clone().unwrap_or_default();
        let mut this = Self {
            namespace: namespace.clone(),
            seed: seed_of(kind, object),
            detail: Self::detail_for(kind),
            live: None,
            graph,
            shown: None,
            loading: true,
            _center: None,
            _subscription: subscription,
        };
        this.live = Some(Live::new(cache, catalog, Some(vec![namespace]), cx));
        this
    }

    fn detail_for(kind: &ResourceKind) -> Detail {
        if kind.api.group.is_empty() && kind.api.kind == "Pod" {
            Detail::Pods
        } else {
            Detail::Workloads
        }
    }

    /// Whether this panel can follow `object` without new watches.
    pub fn watches(&self, cache: &ResourceCache, object: &DynamicObject) -> bool {
        self.live.as_ref().is_some_and(|live| {
            live.same(
                cache,
                &Some(vec![object.metadata.namespace.clone().unwrap_or_default()]),
            )
        }) && object.metadata.namespace.as_deref() == Some(&self.namespace)
    }

    /// Shows the neighbors of another object of the same namespace.
    pub fn set_target(
        &mut self,
        kind: &ResourceKind,
        object: &DynamicObject,
        cx: &mut Context<Self>,
    ) {
        let seed = seed_of(kind, object);
        let detail = Self::detail_for(kind);
        if seed == self.seed && detail == self.detail {
            return;
        }
        self.seed = seed;
        self.detail = detail;
        self.shown = None;
        Live::schedule(self, cx);
    }

    /// Direct neighbors as links: `(id, label, relation)`.
    fn links(&self) -> Vec<(String, String, String, kubach_ui::Tone)> {
        let Some(graph) = &self.shown else {
            return Vec::new();
        };
        let relation = |edge: &kubach_core::topology::Edge, arrow: &str| {
            let kind = map::edge_kind_label(edge.kind);
            match map::edge_label(edge) {
                Some(label) => format!("{arrow} {kind} · {label}"),
                None => format!("{arrow} {kind}"),
            }
        };
        let incoming = graph
            .edges_to(&self.seed)
            .filter_map(|edge| Some((graph.node(&edge.from)?, relation(edge, "←"))));
        let outgoing = graph
            .edges_from(&self.seed)
            .filter_map(|edge| Some((graph.node(&edge.to)?, relation(edge, "→"))));
        incoming
            .chain(outgoing)
            .map(|(node, relation)| {
                (
                    node.id.as_str().to_owned(),
                    format!("{} {}", node.kind.label(), node.name),
                    relation,
                    map::tone(graph, node),
                )
            })
            .collect()
    }

    fn open(&mut self, id: &str, cx: &mut Context<Self>) {
        if id == self.seed.as_str() {
            return;
        }
        let reveal = self
            .shown
            .as_ref()
            .and_then(|graph| graph.nodes().iter().find(|node| node.id.as_str() == id))
            .and_then(map::reveal_of);
        if let Some(reveal) = reveal {
            cx.emit(reveal);
        }
    }
}

impl Render for Connections {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let count = self
            .shown
            .as_ref()
            .map_or(0, |graph| graph.nodes().len().saturating_sub(1));
        let body = if count > 0 {
            let muted = theme.muted_foreground;
            let links = self.links().into_iter().enumerate().map(|(ix, link)| {
                let (id, label, relation, tone) = link;
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(StatusDot::new(tone))
                    .child(
                        div().min_w_0().overflow_hidden().child(
                            Button::new(("connections-link", ix))
                                .link()
                                .small()
                                .label(label)
                                .on_click(cx.listener(move |this, _, _, cx| this.open(&id, cx))),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_right()
                            .text_xs()
                            .text_color(muted)
                            .child(relation),
                    )
            });
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .h(HEIGHT)
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .overflow_hidden()
                        .child(self.graph.clone()),
                )
                .child(div().flex().flex_col().gap_1().children(links))
                .into_any_element()
        } else {
            let text = if self.loading || self.shown.is_none() {
                t!("details-loading")
            } else {
                t!("connections-none")
            };
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let mut panel = Panel::new().title(t!("connections-title"));
        if count > 0 {
            panel = panel.note(count.to_string());
        }
        panel.child(body)
    }
}
