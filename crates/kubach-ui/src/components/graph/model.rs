//! Data shown by a [`GraphView`](super::GraphView): neutral nodes, edges and
//! groups. The view knows nothing about what they stand for.

use std::collections::{HashMap, HashSet};

use gpui_kit::SharedString;
use gpui_kit::assets::IconNamed;

use crate::tokens::Tone;
use kubach_i18n::t;

/// A small tinted label on a node.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphBadge {
    pub label: SharedString,
    pub tone: Tone,
}

/// One box of the graph.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphNode {
    /// Unique among the nodes; reported in events.
    pub id: SharedString,
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
    /// Asset path of an SVG icon (see [`GraphNode::icon`]).
    pub icon: Option<SharedString>,
    /// Color of the status dot.
    pub tone: Tone,
    pub badges: Vec<GraphBadge>,
    /// Id of the [`GraphGroup`] holding the node. An unknown id makes a
    /// group titled with the id.
    pub group: Option<SharedString>,
    /// Column wanted for the node, 0 leftmost. Without it the column
    /// follows from the edges.
    pub layer: Option<usize>,
}

impl GraphNode {
    pub fn new(id: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: None,
            icon: None,
            tone: Tone::Neutral,
            badges: Vec::new(),
            group: None,
            layer: None,
        }
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn icon(mut self, icon: impl IconNamed) -> Self {
        self.icon = Some(icon.path());
        self
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    pub fn badge(mut self, label: impl Into<SharedString>, tone: Tone) -> Self {
        self.badges.push(GraphBadge {
            label: label.into(),
            tone,
        });
        self
    }

    pub fn group(mut self, group: impl Into<SharedString>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn layer(mut self, layer: usize) -> Self {
        self.layer = Some(layer);
        self
    }
}

/// Line of an edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EdgeStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
    /// Long dashes: measured traffic the structure doesn't explain (#326).
    LongDash,
}

/// How busy a traffic edge is, colored by the theme's
/// `graph.traffic.*` tokens (#326): three load levels, then errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrafficLevel {
    Low,
    Medium,
    High,
    /// Some requests fail (or flows are dropped).
    Degraded,
    /// Many requests fail.
    Failing,
}

impl TrafficLevel {
    pub const ALL: [TrafficLevel; 5] = [
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Degraded,
        Self::Failing,
    ];

    /// Index into [`crate::theme::traffic_palette`].
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> String {
        match self {
            Self::Low => t!("ui-traffic-low"),
            Self::Medium => t!("ui-traffic-medium"),
            Self::High => t!("ui-traffic-high"),
            Self::Degraded => t!("ui-traffic-degraded"),
            Self::Failing => t!("ui-traffic-failing"),
        }
    }

    /// Level of an edge with `load` (0..=1, relative to the busiest edge)
    /// and `error_ratio` (0..=1): 5% failures or more is failing, 1% or
    /// more degraded, otherwise by load in thirds.
    pub fn of(load: f32, error_ratio: Option<f32>) -> Self {
        match error_ratio {
            Some(ratio) if ratio >= 0.05 => Self::Failing,
            Some(ratio) if ratio >= 0.01 => Self::Degraded,
            _ if load >= 0.66 => Self::High,
            _ if load >= 0.33 => Self::Medium,
            _ => Self::Low,
        }
    }
}

/// A connection drawn from `from` to `to` with an arrowhead.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphEdge {
    pub id: SharedString,
    pub from: SharedString,
    pub to: SharedString,
    pub style: EdgeStyle,
    /// Text at the middle of the edge (a port, a host), shown when zoomed in.
    pub label: Option<SharedString>,
    /// 0..=1: thickness, e.g. for traffic. `None` draws a thin line.
    pub weight: Option<f32>,
    /// Neutral edges use the theme's line color.
    pub tone: Tone,
    /// Measured traffic: colored by level instead of tone.
    pub traffic: Option<TrafficLevel>,
}

impl GraphEdge {
    pub fn new(
        id: impl Into<SharedString>,
        from: impl Into<SharedString>,
        to: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            style: EdgeStyle::Solid,
            label: None,
            weight: None,
            tone: Tone::Neutral,
            traffic: None,
        }
    }

    pub fn style(mut self, style: EdgeStyle) -> Self {
        self.style = style;
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = Some(weight.clamp(0., 1.));
        self
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Colors the edge as measured traffic of `level`.
    pub fn traffic(mut self, level: TrafficLevel) -> Self {
        self.traffic = Some(level);
        self
    }
}

/// A box around nodes (e.g. a namespace) that folds into one node.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphGroup {
    pub id: SharedString,
    pub title: SharedString,
    /// Initial state; afterwards the view keeps what the user toggled.
    pub collapsed: bool,
}

impl GraphGroup {
    pub fn new(id: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            collapsed: false,
        }
    }

    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

/// Everything a [`GraphView`](super::GraphView) shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Bands are stacked in this order.
    pub groups: Vec<GraphGroup>,
}

/// What a box on the canvas stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Item {
    /// A node, by index in [`GraphData::nodes`].
    Node(usize),
    /// A collapsed group, by index in [`Visible::groups`].
    Group(usize),
}

/// A box of the graph by id: a node, or a collapsed group drawn as one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GraphTarget {
    Node(SharedString),
    Group(SharedString),
}

/// A group after resolving node references.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VisibleGroup {
    pub id: SharedString,
    pub title: SharedString,
    pub collapsed: bool,
    /// Member nodes, by index in [`GraphData::nodes`].
    pub members: Vec<usize>,
    /// Most severe member tone.
    pub tone: Tone,
}

/// An edge between items; folded groups merge several data edges.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VisibleEdge {
    pub from: usize,
    pub to: usize,
    /// Data edges drawn by this one, by index in [`GraphData::edges`].
    pub edges: Vec<usize>,
}

/// The graph as drawn: collapsed groups replaced by one item each.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Visible {
    pub items: Vec<Item>,
    pub keys: Vec<GraphTarget>,
    /// Group band of each item (expanded groups only).
    pub band: Vec<Option<usize>>,
    pub groups: Vec<VisibleGroup>,
    pub edges: Vec<VisibleEdge>,
    /// Item drawing each data node.
    pub item_of_node: Vec<Option<usize>>,
}

/// Severity order for summarizing tones.
fn severity(tone: Tone) -> u8 {
    match tone {
        Tone::Neutral => 0,
        Tone::Success => 1,
        Tone::Info => 2,
        Tone::Warning => 3,
        Tone::Danger => 4,
    }
}

impl Visible {
    /// Resolves `data` with the groups in `collapsed` folded. Duplicate node
    /// ids keep the first; edges to unknown nodes are dropped.
    pub fn build(data: &GraphData, collapsed: &HashSet<SharedString>) -> Self {
        let mut groups: Vec<VisibleGroup> = Vec::new();
        let mut group_ix: HashMap<SharedString, usize> = HashMap::new();
        for group in &data.groups {
            if group_ix.contains_key(&group.id) {
                continue;
            }
            group_ix.insert(group.id.clone(), groups.len());
            groups.push(VisibleGroup {
                id: group.id.clone(),
                title: group.title.clone(),
                collapsed: collapsed.contains(&group.id),
                members: Vec::new(),
                tone: Tone::Neutral,
            });
        }

        let mut node_ix: HashMap<&SharedString, usize> = HashMap::new();
        let mut node_group: Vec<Option<usize>> = vec![None; data.nodes.len()];
        for (ix, node) in data.nodes.iter().enumerate() {
            if node_ix.contains_key(&node.id) {
                continue;
            }
            node_ix.insert(&node.id, ix);
            if let Some(id) = &node.group {
                let g = *group_ix.entry(id.clone()).or_insert_with(|| {
                    groups.push(VisibleGroup {
                        id: id.clone(),
                        title: id.clone(),
                        collapsed: collapsed.contains(id),
                        members: Vec::new(),
                        tone: Tone::Neutral,
                    });
                    groups.len() - 1
                });
                let group = &mut groups[g];
                group.members.push(ix);
                if severity(node.tone) > severity(group.tone) {
                    group.tone = node.tone;
                }
                node_group[ix] = Some(g);
            }
        }

        let mut visible = Visible {
            item_of_node: vec![None; data.nodes.len()],
            ..Visible::default()
        };
        let mut group_item: Vec<Option<usize>> = vec![None; groups.len()];
        for (ix, node) in data.nodes.iter().enumerate() {
            if node_ix.get(&node.id) != Some(&ix) {
                continue;
            }
            match node_group[ix] {
                Some(g) if groups[g].collapsed => {
                    let item = *group_item[g].get_or_insert_with(|| {
                        visible.items.push(Item::Group(g));
                        visible.keys.push(GraphTarget::Group(groups[g].id.clone()));
                        visible.band.push(None);
                        visible.items.len() - 1
                    });
                    visible.item_of_node[ix] = Some(item);
                }
                band => {
                    visible.item_of_node[ix] = Some(visible.items.len());
                    visible.items.push(Item::Node(ix));
                    visible.keys.push(GraphTarget::Node(node.id.clone()));
                    visible.band.push(band);
                }
            }
        }

        let mut merged: HashMap<(usize, usize), usize> = HashMap::new();
        for (ix, edge) in data.edges.iter().enumerate() {
            let item =
                |id: &SharedString| node_ix.get(id).and_then(|&node| visible.item_of_node[node]);
            let (Some(from), Some(to)) = (item(&edge.from), item(&edge.to)) else {
                continue;
            };
            if from == to {
                continue;
            }
            match merged.get(&(from, to)) {
                Some(&e) => visible.edges[e].edges.push(ix),
                None => {
                    merged.insert((from, to), visible.edges.len());
                    visible.edges.push(VisibleEdge {
                        from,
                        to,
                        edges: vec![ix],
                    });
                }
            }
        }
        visible.groups = groups;
        visible
    }

    /// Items reachable from `item` downstream and upstream, with the edges
    /// on those paths.
    pub fn chain(&self, item: usize) -> (Vec<bool>, Vec<bool>) {
        let n = self.items.len();
        let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut inc: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (e, edge) in self.edges.iter().enumerate() {
            out[edge.from].push(e);
            inc[edge.to].push(e);
        }
        let walk = |adjacent: &[Vec<usize>], forward: bool| {
            let mut seen = vec![false; n];
            seen[item] = true;
            let mut stack = vec![item];
            while let Some(v) = stack.pop() {
                for &e in &adjacent[v] {
                    let edge = &self.edges[e];
                    let next = if forward { edge.to } else { edge.from };
                    if !seen[next] {
                        seen[next] = true;
                        stack.push(next);
                    }
                }
            }
            seen
        };
        let down = walk(&out, true);
        let up = walk(&inc, false);
        let nodes: Vec<bool> = (0..n).map(|v| down[v] || up[v]).collect();
        let edges = self
            .edges
            .iter()
            .map(|e| (down[e.from] && down[e.to]) || (up[e.from] && up[e.to]))
            .collect();
        (nodes, edges)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui_kit::SharedString;

    use super::{GraphData, GraphEdge, GraphGroup, GraphNode, Item, Visible};

    fn data() -> GraphData {
        GraphData {
            nodes: vec![
                GraphNode::new("in", "ingress"),
                GraphNode::new("svc", "svc").group("a"),
                GraphNode::new("pod", "pod").group("a"),
                GraphNode::new("db", "db").group("b"),
                GraphNode::new("svc", "duplicate"),
            ],
            edges: vec![
                GraphEdge::new("e1", "in", "svc"),
                GraphEdge::new("e2", "svc", "pod"),
                GraphEdge::new("e3", "pod", "db"),
                GraphEdge::new("e4", "in", "pod"),
                GraphEdge::new("e5", "pod", "missing"),
            ],
            groups: vec![GraphGroup::new("a", "Group A")],
        }
    }

    #[test]
    fn resolves_nodes_groups_and_edges() {
        let visible = Visible::build(&data(), &HashSet::new());
        assert_eq!(visible.items.len(), 4);
        assert_eq!(visible.item_of_node[4], None);
        // Group "b" is made from the node reference.
        assert_eq!(visible.groups.len(), 2);
        assert_eq!(visible.groups[1].title.as_ref(), "b");
        assert_eq!(visible.band, vec![None, Some(0), Some(0), Some(1)]);
        assert_eq!(visible.edges.len(), 4);
    }

    #[test]
    fn collapsing_merges_edges_into_one_item() {
        let collapsed: HashSet<SharedString> = ["a".into()].into();
        let visible = Visible::build(&data(), &collapsed);
        assert_eq!(visible.items.len(), 3);
        assert!(visible.items.contains(&Item::Group(0)));
        let group_item = visible.item_of_node[1].unwrap();
        assert_eq!(visible.item_of_node[2], Some(group_item));
        // in→svc and in→pod merge; svc→pod is inside the group.
        let into_group: Vec<_> = visible
            .edges
            .iter()
            .filter(|e| e.to == group_item)
            .collect();
        assert_eq!(into_group.len(), 1);
        assert_eq!(into_group[0].edges, vec![0, 3]);
        assert_eq!(visible.edges.len(), 2);
    }

    #[test]
    fn traffic_levels_follow_errors_then_load() {
        use super::TrafficLevel;
        assert_eq!(TrafficLevel::of(1., Some(0.2)), TrafficLevel::Failing);
        assert_eq!(TrafficLevel::of(0.1, Some(0.02)), TrafficLevel::Degraded);
        assert_eq!(TrafficLevel::of(0.9, Some(0.001)), TrafficLevel::High);
        assert_eq!(TrafficLevel::of(0.5, None), TrafficLevel::Medium);
        assert_eq!(TrafficLevel::of(0.1, None), TrafficLevel::Low);
        assert_eq!(TrafficLevel::Failing.index(), 4);
    }

    #[test]
    fn chain_follows_paths_through_an_item() {
        let visible = Visible::build(&data(), &HashSet::new());
        let svc = visible.item_of_node[1].unwrap();
        let (nodes, edges) = visible.chain(svc);
        // in → svc → pod → db; the in → pod shortcut bypasses svc.
        assert!(nodes.iter().all(|&n| n));
        assert_eq!(edges.iter().filter(|&&e| e).count(), 3);
    }
}
