//! Visual catalog of the Kubach design system.
//!
//! `cargo run -p kubach-ui --example gallery`

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, IconName, Selectable as _, Sizable as _, ThemeMode};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_ui::tokens::{SIDEBAR_WIDTH, TITLEBAR_HEIGHT};
use kubach_ui::{
    BulkProgress, BulkResult, ChartLine, CheckResult, CheckStatus, ConfirmDialog, ConfirmLive,
    DEFAULT_LOG_CAPACITY, DiffLineKind, DiffView, DiffViewLine, DownloadProgress, EmptyState,
    EventEntry, EventList, EventTimeline, FilterBar, FlowGraph, FlowNode, Hotbar, HotbarTile,
    KeyValueList, LabelChip, LineChart, LogEntry, LogView, MultiSelect, MultiSelectEvent,
    MultiSelection, NavItem, NavNode, NavTree, NavTreeEvent, NumberRow, PageHeader, Panel, PathRow,
    ResourceCell, ResourceColumn, ResourceRow, ResourceTable, ResourceTableEvent, SelectRow,
    SettingGroup, SettingRow, SideList, SideListItem, SplitButton, StatusBadge, StatusBar,
    StatusBarItem, StatusCounts, StatusDot, TabItem, TabStrip, TerminalEvent, TerminalView,
    TileColor, TilePreview, ToggleRow, Tone, TruncatedText, UsageMeter, WindowControls,
    WindowTitlebar, unix_now,
};
use kubach_ui::{
    LineSelector, SelectableLines, SpanColor, SpanStyle, StyledSpan, layer_highlights, selectable,
};
// Cluster overview cards (#414).
use kubach_ui::{Donut, DonutColor, DonutSegment, StatCard, UsageBar};
// Compact table for details (#294).
use kubach_ui::CompactTable;
use kubach_ui::LoadingBar;
// Chart icons (#167).
use kubach_ui::ChartIcon;
// App logo (#253).
use kubach_ui::Logo;
// Back to where a jump came from (#395).
use kubach_ui::BackButton;
use kubach_ui::{ButtonLoading as _, spinner};
// Entering Secret values for an agent (#230).
use kubach_ui::{FilePickField, GeneratorControls, SecretField, UntrustedText};
// ConfigMap / Secret keys edited one by one (#275).
use gpui_kit::component::input::{Editor, EditorState};
use kubach_ui::{DataKeyBlock, data_key_editor_height};
// Details panel pieces (#146).
use kubach_ui::{ChipList, PortChip, ReplicaBar, SegmentTab, SegmentTabs, StatusCube, StatusCubes};
// Action toasts (#156).
use kubach_ui::{ActionToast, OperationNotice, OperationsList};
// Right panel resized by its left edge (#309).
use kubach_ui::{SIDE_PANEL_CONTENT_MIN, SidePanel, SidePanelState};
// Container cubes in the pod table (#140).
use kubach_ui::{ContainerCubeState, container_tooltip};
// Earlier log lines on scrolling up (#160).
use kubach_ui::{LogTopNotice, LogViewEvent};
// Topology graph canvas (#323).
use kubach_ui::{
    EdgeStyle, GraphData, GraphEdge, GraphGroup, GraphNode, GraphTarget, GraphView, GraphViewEvent,
};
// Traffic layer of the topology (#326).
use kubach_ui::{TrafficLegend, TrafficLevel};

actions!(gallery, [Quit]);

struct Gallery {
    nav: usize,
    pods: Entity<ResourceTable>,
    selected_pod: Option<SharedString>,
    checked_pods: Vec<SharedString>,
    /// Pods "deleted" through the bulk demo.
    deleted_pods: usize,
    form: FormDemo,
    /// Details panel demo: folds and the picked metric tab.
    details_demo: DetailsDemo,
    tree: Entity<NavTree>,
    tree_pick: Option<SharedString>,
    graph_hover: Option<usize>,
    graph_pick: Option<SharedString>,
    /// Item picked in the side list demo (#402).
    side_pick: SharedString,
    /// Virtualized events demo (#368).
    timeline: EventTimeline,
    namespaces: Entity<MultiSelect>,
    /// Filter bar demo: its namespace picker and search.
    filter_namespaces: Entity<MultiSelect>,
    filter_search: Entity<gpui_kit::component::input::InputState>,
    /// Hotbar demo: tile ids in order and the active one.
    clusters: Vec<SharedString>,
    cluster: SharedString,
    logs: Entity<LogView>,
    snippet: Entity<SelectableSnippet>,
    terminal: Entity<TerminalView>,
    shell: FakeShell,
    secret_demo: SecretDemo,
    /// Value editors of the data key demo: valid JSON and broken JSON.
    data_key_editors: [Entity<EditorState>; 2],
    /// Width of the side panel demo.
    side_panel: Entity<SidePanelState>,
    graph_demo: GraphDemo,
    _log_feed: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// A tile color picked by the user in the demo: data, not a theme token,
/// so it is drawn as given in both themes.
const SAMPLE_CUSTOM_TILE: TileColor = TileColor::Custom(0x2f_b5_a0);

/// Sample hotbar tile for `id`.
fn sample_tile(id: &SharedString) -> HotbarTile {
    let (label, color, status, local, relaxed) = match id.as_ref() {
        "lab" => ("lb", SAMPLE_CUSTOM_TILE, Tone::Success, false, false),
        "minikube" => ("mk", TileColor::Teal, Tone::Success, true, false),
        "prod" => ("pr", TileColor::Pink, Tone::Neutral, false, true),
        "staging" => ("st", TileColor::Orange, Tone::Warning, false, false),
        "kind-dev" => ("kd", TileColor::Purple, Tone::Danger, true, false),
        "eks-old" => ("eo", TileColor::Gray, Tone::Danger, false, false),
        "team-prod" => ("tp", TileColor::Purple, Tone::Neutral, false, false),
        _ => ("api", TileColor::Blue, Tone::Neutral, false, false),
    };
    // Kubeconfig file of this context is missing.
    let unavailable = id.as_ref() == "eks-old";
    // Imported into the encrypted vault.
    let imported = id.as_ref() == "team-prod";
    HotbarTile::new(id.clone(), label)
        .unavailable(unavailable)
        .color(color)
        .status(status)
        .icon(
            local
                .then_some(IconName::HardDrive.into())
                .or(imported.then_some(Lucide::Lock)),
        )
        .badge(relaxed.then_some(IconName::TriangleAlert))
        .tooltip(
            id.clone(),
            vec![
                ("Cluster".into(), id.clone()),
                ("Server".into(), "https://127.0.0.1:6443".into()),
                ("User".into(), "admin".into()),
                ("Namespace".into(), "default".into()),
            ],
        )
}

/// Sample sidebar for the tree demo.
fn sample_tree() -> Vec<NavNode> {
    let leaves = |items: &[(&str, Option<usize>)]| {
        items
            .iter()
            .map(|&(label, count)| NavNode::leaf(label.to_lowercase(), label).count(count))
            .collect()
    };
    vec![
        NavNode::leaf("overview", "Overview").icon(IconName::LayoutDashboard),
        NavNode::group(
            "workloads",
            "Workloads",
            leaves(&[
                ("Pods", Some(12)),
                ("Deployments", Some(4)),
                ("StatefulSets", None),
                ("Jobs", None),
            ]),
        )
        .icon(IconName::Cpu),
        NavNode::group(
            "config",
            "Config",
            leaves(&[("ConfigMaps", None), ("Secrets", None)]),
        )
        .icon(IconName::Settings2),
        NavNode::group(
            "custom",
            "Custom resources",
            vec![NavNode::group(
                "cert-manager.io",
                "cert-manager.io",
                leaves(&[("Certificate", None), ("Issuer", None)]),
            )],
        )
        .icon(IconName::Frame),
    ]
}

/// Values behind the settings form demo.
struct FormDemo {
    enabled: bool,
    profile: usize,
    ttl: u32,
    path: Option<String>,
}

impl Default for FormDemo {
    fn default() -> Self {
        Self {
            enabled: false,
            profile: 0,
            ttl: 30,
            path: None,
        }
    }
}

/// State of the details panel demo.
#[derive(Default)]
struct DetailsDemo {
    labels_open: bool,
    annotations_open: bool,
    scheduling_open: bool,
    metric: usize,
}

/// Rows in the resource table demo.
const POD_COUNT: usize = 1000;
/// Events in the virtualized timeline demo, before "1000 events".
const TIMELINE_EVENTS: usize = 20;

/// `count` events, newest first, like a busy namespace: warnings, repeats
/// and a long message that wraps.
fn demo_events(count: usize) -> Vec<EventEntry> {
    let now = unix_now();
    (0..count)
        .map(|i| {
            let pod = format!("Pod/api-{i}");
            let since = now - (i as i64) * 7;
            let entry = match i % 4 {
                0 => EventEntry::new(
                    "BackOff",
                    format!("Back-off restarting failed container in {pod}"),
                )
                .warning(true),
                1 => EventEntry::new("Pulled", "Successfully pulled image \"nginx:1.27\""),
                2 => EventEntry::new(
                    "FailedScheduling",
                    "0/3 nodes are available: 3 Insufficient memory. preemption: 0/3 nodes \
                     are available: 3 No preemption victims found for incoming pod.",
                )
                .warning(true),
                _ => EventEntry::new("Created", "Created container api"),
            };
            entry
                .object(pod)
                .count(1 + (i % 5) as u32)
                .since(since)
                .key(i)
        })
        .collect()
}

/// Lines generated up front for the log list demo.
const LOG_LINES: usize = 200_000;
/// Fake lines before the first shown one, loaded in pages on scrolling to
/// the top.
const EARLIER_LOG_LINES: usize = 3_000;
const EARLIER_LOG_PAGE: usize = 1_000;

impl Gallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let gallery = cx.entity().downgrade();
        let pods = cx.new(|cx| {
            let mut table = ResourceTable::new(
                vec![
                    ResourceColumn::new("name", "Name")
                        .width(px(300.))
                        .sortable(),
                    ResourceColumn::new("namespace", "Namespace")
                        .width(px(140.))
                        .sortable(),
                    ResourceColumn::new("containers", "Containers")
                        .width(px(110.))
                        .sortable(),
                    // The app's default Status width: long labels are
                    // clipped, with a tooltip.
                    ResourceColumn::new("status", "Status")
                        .width(px(100.))
                        .sortable(),
                    ResourceColumn::new("restarts", "Restarts")
                        .width(px(110.))
                        .align_end()
                        .sortable(),
                    ResourceColumn::new("age", "Age")
                        .width(px(80.))
                        .align_end()
                        .sortable(),
                ],
                window,
                cx,
            )
            .checkable(cx)
            .movable_columns(cx)
            .on_select(move |row, _, cx| {
                let key = row.key.clone();
                gallery
                    .update(cx, |gallery, cx| {
                        gallery.selected_pod = Some(key);
                        cx.notify();
                    })
                    .ok();
            });
            table.set_rows(fake_pods(POD_COUNT), cx);
            table
        });
        let tree = cx.new(|cx| {
            let mut tree = NavTree::new(cx);
            tree.set_nodes(sample_tree(), cx);
            tree.select("pods", cx);
            tree
        });
        let namespaces = cx.new(|cx| {
            let mut select = MultiSelect::new("gallery-namespaces", window, cx)
                .all_label("All namespaces")
                .allow_custom(true);
            select.set_items(
                ["default", "web", "api", "worker", "queue", "kube-system"]
                    .map(SharedString::from)
                    .to_vec(),
                cx,
            );
            select.set_recent(vec!["web".into(), "api".into()], cx);
            select.set_selection(
                MultiSelection::Items(vec!["web".into(), "api".into(), "worker".into()]),
                cx,
            );
            select
        });
        let filter_namespaces = cx.new(|cx| {
            let mut select = MultiSelect::new("gallery-filter-namespaces", window, cx)
                .all_label("All namespaces")
                .allow_custom(true);
            select.set_items(
                ["default", "web", "api", "kube-system"]
                    .map(SharedString::from)
                    .to_vec(),
                cx,
            );
            select.set_selection(MultiSelection::Items(vec!["web".into()]), cx);
            select
        });
        let filter_search = FilterBar::search_state("Search by name…", window, cx);
        let logs = cx.new(|cx| {
            let mut view = LogView::new(DEFAULT_LOG_CAPACITY, cx);
            let mut lines = FakeLog {
                counter: EARLIER_LOG_LINES,
            };
            view.push((0..LOG_LINES).map(|_| lines.next()).collect(), cx);
            view
        });
        // New lines keep arriving, like a followed stream.
        let log_feed = cx.spawn({
            let logs = logs.downgrade();
            async move |_, cx| {
                let mut lines = FakeLog {
                    counter: EARLIER_LOG_LINES + LOG_LINES,
                };
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                    let batch: Vec<LogEntry> = (0..5).map(|_| lines.next()).collect();
                    if logs.update(cx, |view, cx| view.push(batch, cx)).is_err() {
                        break;
                    }
                }
            }
        });
        let terminal = cx.new(|cx| TerminalView::new(window, cx));
        let shell = FakeShell::default();
        let banner = shell.banner();
        terminal.update(cx, |view, cx| view.feed(banner.as_bytes(), cx));
        let subscriptions = vec![
            cx.subscribe(&tree, |this, _, event: &NavTreeEvent, cx| {
                let NavTreeEvent::Select(id) = event;
                this.tree_pick = Some(id.clone());
                cx.notify();
            }),
            cx.subscribe(&namespaces, |_, _, _: &MultiSelectEvent, cx| cx.notify()),
            cx.subscribe(&filter_namespaces, |_, _, _: &MultiSelectEvent, cx| {
                cx.notify()
            }),
            cx.subscribe(
                &filter_search,
                |_, _, _: &gpui_kit::component::input::InputEvent, cx| cx.notify(),
            ),
            cx.subscribe(&pods, |this, _, event: &ResourceTableEvent, cx| {
                let ResourceTableEvent::CheckedChanged(keys) = event;
                this.checked_pods = keys.clone();
                cx.notify();
            }),
            cx.observe(&logs, |_, _, cx| cx.notify()),
            // Earlier lines, as the logs tab loads them from the cluster.
            {
                let mut earliest = EARLIER_LOG_LINES;
                cx.subscribe(&logs, move |_, logs, event: &LogViewEvent, cx| {
                    let LogViewEvent::ReachedTop = event;
                    if earliest == 0 {
                        return;
                    }
                    let start = earliest.saturating_sub(EARLIER_LOG_PAGE);
                    let mut lines = FakeLog { counter: start };
                    let page: Vec<LogEntry> = (start..earliest).map(|_| lines.next()).collect();
                    earliest = start;
                    logs.update(cx, |view, cx| {
                        view.prepend(page, cx);
                        if earliest == 0 {
                            view.set_top_notice(
                                Some(LogTopNotice::Info("Beginning of log".into())),
                                cx,
                            );
                        }
                    });
                })
            },
            cx.subscribe(&terminal, |this, terminal, event: &TerminalEvent, cx| {
                if let TerminalEvent::Input(bytes) = event {
                    let echo = this.shell.input(bytes);
                    terminal.update(cx, |view, cx| view.feed(echo.as_bytes(), cx));
                }
            }),
        ];
        let mut timeline = EventTimeline::new();
        timeline.set_entries(demo_events(TIMELINE_EVENTS), true);
        Self {
            timeline,
            nav: 0,
            pods,
            selected_pod: None,
            checked_pods: Vec::new(),
            deleted_pods: 0,
            details_demo: DetailsDemo::default(),
            tree,
            tree_pick: None,
            graph_hover: None,
            graph_pick: None,
            side_pick: "minikube".into(),
            namespaces,
            filter_namespaces,
            filter_search,
            clusters: [
                "minikube",
                "prod",
                "staging",
                "kind-dev",
                "eu-api",
                "eks-old",
                "team-prod",
                "lab",
            ]
            .map(SharedString::from)
            .to_vec(),
            cluster: "minikube".into(),
            form: FormDemo::default(),
            logs,
            snippet: cx.new(SelectableSnippet::new),
            terminal,
            shell,
            secret_demo: SecretDemo::default(),
            data_key_editors: [
                "{\n  \"replicas\": 3,\n  \"features\": [\"search\", \"export\"]\n}",
                "{\n  \"level\": \"debug\",\n}",
            ]
            .map(|text| {
                cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language("json")
                        .default_value(text)
                })
            }),
            side_panel: cx.new(|_| SidePanelState::new(px(320.), None)),
            graph_demo: GraphDemo::new(window, cx),
            _log_feed: log_feed,
            _subscriptions: subscriptions,
        }
    }

    /// Content beside a side panel: drag the panel's left edge, double
    /// click it for the default width.
    fn side_panel_demo(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let width = self.side_panel.read(cx).width();
        div()
            .h(px(180.))
            .flex()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w(SIDE_PANEL_CONTENT_MIN)
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("List: stays at least 360 px wide"),
            )
            .child(
                SidePanel::new("gallery-side-panel", &self.side_panel).child(
                    Panel::new()
                        .title("Details")
                        .note(format!("{:.0} px", f32::from(width)))
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .whitespace_normal()
                                .child("Drag the left edge; double click it for 320 px."),
                        ),
                ),
            )
    }

    /// Namespace picker, search by name and an action over a short list;
    /// a search without matches shows the "Clear search" placeholder.
    fn render_filter_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        const NAMES: [&str; 6] = [
            "api-7f9c6d-x2k4q",
            "api-7f9c6d-p9wz1",
            "web-5d8b7-lq2m8",
            "worker-0",
            "worker-1",
            "redis-master-0",
        ];
        let query = self.filter_search.read(cx).value().to_string();
        let shown: Vec<&str> = NAMES
            .into_iter()
            .filter(|name| kubach_ui::name_matches(name, &query))
            .collect();
        let theme = cx.theme();
        let bar = FilterBar::new()
            .filter(self.filter_namespaces.clone())
            .search(&self.filter_search)
            .action(
                Button::new("gallery-filter-new")
                    .primary()
                    .small()
                    .icon(IconName::Plus)
                    .label("New"),
            );
        let body = if shown.is_empty() {
            let search = self.filter_search.clone();
            FilterBar::no_matches("Pods", &query, move |_, window, cx| {
                search.update(cx, |search, cx| search.set_value("", window, cx));
            })
            .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_sm()
                .font_family(theme.mono_font_family.clone())
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{} of {} Pods", shown.len(), NAMES.len())),
                )
                .children(shown.into_iter().map(|name| div().child(name)))
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(bar)
            .child(body)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Substring, ignoring case. Esc clears the search."),
            )
    }

    /// "N checked" with a bulk delete that confirms like the app does.
    fn render_bulk_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.checked_pods.len();
        let theme = cx.theme();
        div()
            .flex()
            .items_center()
            .gap_3()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(if count == 0 {
                format!(
                    "Check rows: checkbox, shift-click for a range, cmd/ctrl-click, cmd-a. \
                     Deleted so far: {}.",
                    self.deleted_pods
                )
            } else {
                format!("Selected: {count}")
            })
            .when(count > 0, |bar| {
                bar.child(
                    Button::new("bulk-delete")
                        .danger()
                        .small()
                        .label("Delete…")
                        .on_click(cx.listener(|this, _, window, cx| {
                            let keys = this.checked_pods.clone();
                            let gallery = cx.entity().downgrade();
                            ConfirmDialog::open(window, cx, move |window, cx| {
                                let count = keys.len();
                                let dialog = ConfirmDialog::new(format!("Delete {count} pods?"))
                                    .context("kind-demo")
                                    .message("This only pretends to delete them.")
                                    .command(format!(
                                        "kubectl delete pod --context kind-demo --namespace \
                                         apps --wait=false -- {}",
                                        keys.join(" ")
                                    ))
                                    .items(keys.clone())
                                    .confirm_label(format!("Delete {count}"))
                                    .danger()
                                    .notice(
                                        "Their volume claims stay (demo):",
                                        ["data-web-0", "data-web-1"],
                                    )
                                    .option("Skip graceful termination (demo)", false)
                                    .option("Also delete the claims (demo)", false)
                                    .on_confirm_with_options(move |_, _, cx| {
                                        gallery
                                            .update(cx, |this, cx| {
                                                this.deleted_pods += count;
                                                this.pods.update(cx, |t, cx| t.clear_checked(cx));
                                            })
                                            .ok();
                                    });
                                if count > 5 {
                                    dialog.require_typed(count.to_string(), window, cx)
                                } else {
                                    dialog
                                }
                            });
                        })),
                )
                .child(
                    Button::new("bulk-clear")
                        .ghost()
                        .small()
                        .label("Clear selection")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pods.update(cx, |table, cx| table.clear_checked(cx));
                        })),
                )
            })
    }

    fn render_log_demo(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let view = self.logs.read(cx);
        let searching = view.search_query() == "error";
        let (current, total) = view.match_position();
        let following = view.follow_state().is_following();
        let wrap = view.wrap();
        let timestamps = view.show_timestamps();
        let logs = self.logs.clone();
        let toolbar = div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new("log-search-error")
                    .ghost()
                    .xsmall()
                    .label("Search \"error\"")
                    .selected(searching)
                    .on_click({
                        let logs = logs.clone();
                        move |_, _, cx| {
                            let query = if searching { "" } else { "error" };
                            logs.update(cx, |view, cx| view.set_search(query, cx));
                        }
                    }),
            )
            .child(
                Button::new("log-next")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronDown)
                    .on_click({
                        let logs = logs.clone();
                        move |_, _, cx| logs.update(cx, |view, cx| view.next_match(cx))
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{current}/{total}")),
            )
            .child(
                Button::new("log-wrap")
                    .ghost()
                    .xsmall()
                    .label("Wrap")
                    .selected(wrap)
                    .on_click({
                        let logs = logs.clone();
                        move |_, _, cx| logs.update(cx, |view, cx| view.set_wrap(!wrap, cx))
                    }),
            )
            .child(
                Button::new("log-timestamps")
                    .ghost()
                    .xsmall()
                    .label("Timestamps")
                    .selected(timestamps)
                    .on_click({
                        let logs = logs.clone();
                        move |_, _, cx| {
                            logs.update(cx, |view, cx| view.set_show_timestamps(!timestamps, cx))
                        }
                    }),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} lines", view.len())),
            )
            .child(
                Button::new("log-follow")
                    .ghost()
                    .xsmall()
                    .label(if following { "Pause" } else { "Follow" })
                    .on_click(move |_, _, cx| {
                        logs.update(cx, |view, cx| {
                            if following {
                                view.pause(cx)
                            } else {
                                view.follow(cx)
                            }
                        })
                    }),
            );
        div().flex().flex_col().gap_2().child(toolbar).child(
            div()
                .h(px(360.))
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(self.logs.clone()),
        )
    }

    fn flow_graph_demo(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (nodes, edges) = demo_graph();
        let titles: Vec<SharedString> = nodes.iter().map(|n| n.title.clone()).collect();
        let highlight = self.graph_hover.map(|ix| demo_chain(&edges, ix));
        let theme = cx.theme();
        let picked = match &self.graph_pick {
            Some(name) => format!("Clicked: {name}"),
            None => "Hover a node to trace its chain; click a role or binding to open it.".into(),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div().id("flow-graph-scroll").overflow_x_scroll().child(
                    FlowGraph::new("flow-graph")
                        .columns(["Subjects", "Bindings", "Roles", "Rules"])
                        .nodes(nodes)
                        .edges(edges)
                        .highlight(highlight)
                        .on_hover(cx.listener(|this, (ix, hovered): &(usize, bool), _, cx| {
                            if *hovered {
                                this.graph_hover = Some(*ix);
                            } else if this.graph_hover == Some(*ix) {
                                this.graph_hover = None;
                            }
                            cx.notify();
                        }))
                        .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                            this.graph_pick = titles.get(*ix).cloned();
                            cx.notify();
                        })),
                ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(picked),
            )
    }

    /// Pieces of the resource details: metric tabs with request and limit
    /// lines, chips that fold and copy, a replica bar, status cubes and a
    /// folding section.
    fn details_demo(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let demo = &self.details_demo;
        let theme = cx.theme().clone();
        let tabs = ["CPU", "Memory", "Network", "Filesystem"]
            .iter()
            .enumerate()
            .map(|(ix, label)| {
                let tab = SegmentTab::new(*label);
                if ix >= 2 {
                    tab.muted(true)
                        .tooltip("Needs Prometheus: metrics-server reports only CPU and memory")
                } else {
                    tab
                }
            })
            .collect();
        let cpu: Vec<(f64, f64)> = (0..60)
            .map(|ix| {
                let i = ix as f64;
                (i * 15.0, 0.06 + 0.03 * (i / 6.0).sin().abs())
            })
            .collect();
        let memory: Vec<(f64, f64)> = (0..60)
            .map(|ix| (ix as f64 * 15.0, (40.0 + ix as f64 * 0.3) * 1024.0 * 1024.0))
            .collect();
        let chart = if demo.metric == 1 {
            LineChart::new("details-memory", "")
                .line(ChartLine::new("Working set", memory).slot(1))
                .threshold("Request", 32.0 * 1024.0 * 1024.0, Tone::Info)
                .threshold("Limit", 64.0 * 1024.0 * 1024.0, Tone::Warning)
                .format(|v| SharedString::from(format!("{:.0}Mi", v / 1024.0 / 1024.0)))
        } else {
            LineChart::new("details-cpu", "")
                .line(ChartLine::new("CPU", cpu).slot(0))
                .threshold("Request", 0.05, Tone::Info)
                .threshold("Limit", 4.0, Tone::Warning)
                .format(|v| SharedString::from(format!("{:.0}m", v * 1000.0)))
        }
        .x_range(0.0, 885.0)
        .x_labels("-15m", "now");
        let labels = [
            ("app.kubernetes.io/name", "web"),
            ("app.kubernetes.io/instance", "web-prod"),
            ("app.kubernetes.io/version", "1.27.3"),
            ("app.kubernetes.io/managed-by", "Helm"),
            ("team", "platform"),
            ("tier", "frontend"),
        ];
        let cubes = [
            StatusCube::new(Tone::Success, "app: Running"),
            StatusCube::new(Tone::Success, "proxy: Running"),
            StatusCube::new(Tone::Warning, "migrate: ContainerCreating"),
            StatusCube::new(Tone::Danger, "worker: CrashLoopBackOff"),
            StatusCube::new(Tone::Neutral, "metrics: Unknown"),
        ];
        div()
            .w(px(380.))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                Panel::new()
                    .title("Metrics")
                    .note("metrics-server · while the panel is open")
                    .child(
                        SegmentTabs::new("demo-metric-tabs", tabs, demo.metric).on_select(
                            cx.listener(|this, ix: &usize, _, cx| {
                                this.details_demo.metric = *ix;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        // Ranges with nothing more to show are disabled (#376).
                        div().flex().justify_end().child(SegmentTabs::new(
                            "demo-range-tabs",
                            ["5m", "15m", "All"]
                                .iter()
                                .enumerate()
                                .map(|(ix, label)| {
                                    let tab = SegmentTab::new(*label);
                                    if ix == 1 {
                                        tab.disabled(true).tooltip(
                                            "Collected since the panel opened: 14m45s so far",
                                        )
                                    } else {
                                        tab
                                    }
                                })
                                .collect(),
                            2,
                        )),
                    )
                    .child(chart),
            )
            .child(
                // Text cut where it doesn't fit gets the full text as a
                // tooltip; a key column takes its widest key (#444).
                Panel::new().child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .text_sm()
                        .child(
                            div()
                                .flex()
                                .gap_4()
                                .child(div().w(px(160.)).child(TruncatedText::new(
                                    "demo-truncated-long",
                                    "Переадресация портов и проверки безопасности",
                                )))
                                .child(div().w(px(160.)).child(TruncatedText::new(
                                    "demo-truncated-short",
                                    "Fits: no tooltip",
                                ))),
                        )
                        .child(
                            KeyValueList::new()
                                .row("Последний обработанный reconcile", "2m ago")
                                .row("Name", "web"),
                        ),
                ),
            )
            .child(
                Panel::new().child(
                    KeyValueList::new()
                        .row("Name", "web")
                        .row("Replicas", ReplicaBar::new(5, 4, 3))
                        .row("Surge", ReplicaBar::new(3, 4, 3))
                        .row(
                            "Quota pods",
                            UsageMeter::new("", "8 / 10 · 80%").fill(Some(0.8)),
                        )
                        .row("Containers", StatusCubes::new("demo-cubes", cubes))
                        .row(
                            "Ports",
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .child(
                                    PortChip::new("demo-port-http", "http 80/TCP")
                                        .tooltip("Port forward 80 → 127.0.0.1:80 (or nearest free)")
                                        .on_click(|_, _, _| {}),
                                )
                                .child(
                                    PortChip::new("demo-port-https", "https 10250/TCP")
                                        .forwarded(Some("127.0.0.1:10250"))
                                        .tooltip("Forwarded to 127.0.0.1:10250")
                                        .on_click(|_, _, _| {}),
                                )
                                .child(
                                    PortChip::new("demo-port-dns", "dns 53/UDP")
                                        .disabled(true)
                                        .tooltip("Port forward supports TCP only"),
                                ),
                        )
                        .row(
                            "Labels",
                            ChipList::new("demo-labels", labels)
                                .expanded(demo.labels_open)
                                .on_toggle(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.details_demo.labels_open = !this.details_demo.labels_open;
                                    cx.notify();
                                })),
                        )
                        .row(
                            "Annotations",
                            ChipList::new("demo-annotations", labels)
                                .folded(0)
                                .expanded(demo.annotations_open)
                                .on_toggle(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.details_demo.annotations_open =
                                        !this.details_demo.annotations_open;
                                    cx.notify();
                                })),
                        ),
                ),
            )
            .child(
                Panel::new().title("Rules").child(
                    CompactTable::new(["API groups", "Resources", "Verbs", "Resource names"])
                        .row(["core", "pods, pods/log", "get, list, watch", ""])
                        .row(["apps", "deployments", "get, patch", "web, api"]),
                ),
            )
            .child(
                Panel::new()
                    .title("Scheduling")
                    .collapsible(
                        "demo-scheduling",
                        demo.scheduling_open,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.details_demo.scheduling_open = !this.details_demo.scheduling_open;
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Tolerations: node.kubernetes.io/not-ready exists :NoExecute"),
                    ),
            )
    }

    /// A confirmation whose command and list follow its options
    /// ([`ConfirmDialog::live`]), like Drain in the app.
    fn demo_drain_dialog(window: &mut Window, cx: &mut App) {
        ConfirmDialog::open(window, cx, |_, _| {
            ConfirmDialog::new("Drain Node node-1?")
                .message("Cordons the node, then evicts its pods (demo).")
                .confirm_label("Drain")
                .danger()
                .option("Ignore DaemonSets", true)
                .option("Delete emptyDir data", false)
                .option("Force (pods without a controller)", false)
                .live(|checked| {
                    let flag = |ix: usize| checked.get(ix).copied().unwrap_or(false);
                    let mut command = "kubectl drain node-1".to_owned();
                    for (ix, name) in ["--ignore-daemonsets", "--delete-emptydir-data", "--force"]
                        .into_iter()
                        .enumerate()
                    {
                        if flag(ix) {
                            command.push(' ');
                            command.push_str(name);
                        }
                    }
                    command.push_str(" --timeout=5m");
                    let cache = if flag(1) {
                        "apps/cache-0 — evict"
                    } else {
                        "apps/cache-0 — blocked: uses emptyDir"
                    };
                    ConfirmLive {
                        command: Some(command.into()),
                        items: vec!["apps/web-5c9b — evict".into(), cache.into()],
                        notice: None,
                        blocked: !flag(1),
                    }
                })
                .on_confirm(|_, _| {})
        });
    }

    /// A fake `helm install` that waits for a hook: live phases and output,
    /// elapsed time, Cancel behind a confirmation.
    fn demo_helm_toast(window: &mut Window, cx: &mut Context<Self>) {
        const PHASES: [&str; 6] = [
            "Creating 12 resource(s)",
            "Waiting for 1 resource (timeout 5m0s)",
            "Waiting for Job cert-manager-startupapicheck: InProgress",
            "Waiting for Job cert-manager-startupapicheck: InProgress",
            "All resources achieved desired status",
            "Updating status for installed release",
        ];
        let toast = ActionToast::show("Install of cert-manager", 1, window, cx);
        toast.set_detail(
            "helm install --values - --create-namespace -- cert-manager jetstack/cert-manager",
            cx,
        );
        let cancelled = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = cancelled.clone();
        toast.set_cancel_confirmed(
            "Stop the install of cert-manager?",
            "Helm is asked to stop and marks the release failed; resources it already \
             created stay. If it does not exit in 10 s it is killed and the release may \
             stay pending-install.",
            move |_| flag.set(true),
            cx,
        );
        cx.spawn(async move |this, cx| {
            for phase in PHASES {
                cx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
                if cancelled.get() {
                    let result = BulkResult {
                        succeeded: 0,
                        failures: vec![(
                            "".into(),
                            "Stopped: Error: INSTALLATION FAILED: context canceled".into(),
                        )],
                        hint: None,
                    };
                    this.update(cx, |_, cx| toast.finish(result, cx)).ok();
                    return;
                }
                let alive = this.update(cx, |_, cx| toast.push_output([phase.into()], cx));
                if alive.is_err() {
                    return;
                }
            }
            this.update(cx, |_, cx| {
                toast.set_detail("NAME: cert-manager\nSTATUS: deployed\nREVISION: 1", cx);
                toast.finish(
                    BulkResult {
                        succeeded: 1,
                        ..Default::default()
                    },
                    cx,
                )
            })
            .ok();
        })
        .detach();
    }

    /// The operations of the session, as the status bar opens them.
    fn open_operations(window: &mut Window, cx: &mut App) {
        OperationsList::open(window, cx);
    }

    /// Runs a fake action over five objects, shown as an [`ActionToast`].
    fn demo_toast(fail: bool, window: &mut Window, cx: &mut Context<Self>) {
        const TOTAL: usize = 5;
        let title = if fail {
            "Delete 5 Pods"
        } else {
            "Restart 5 Deployments"
        };
        let toast = ActionToast::show(title, TOTAL, window, cx);
        // The successful run can be cancelled from the toast.
        let cancelled = std::rc::Rc::new(std::cell::Cell::new(false));
        if !fail {
            let flag = cancelled.clone();
            toast.set_cancel(move |_| flag.set(true), cx);
        }
        cx.spawn(async move |this, cx| {
            for done in 1..=TOTAL {
                cx.background_executor()
                    .timer(Duration::from_millis(400))
                    .await;
                if cancelled.get() {
                    let result = BulkResult {
                        succeeded: done - 1,
                        failures: vec![("".into(), "Cancelled".into())],
                        hint: None,
                    };
                    this.update(cx, |_, cx| toast.finish(result, cx)).ok();
                    return;
                }
                if this.update(cx, |_, cx| toast.set_done(done, cx)).is_err() {
                    return;
                }
            }
            let result = if fail {
                BulkResult {
                    succeeded: 3,
                    failures: vec![
                        (
                            "kube-system/coredns-7d8f".into(),
                            "No permission (RBAC): pods \"coredns-7d8f\" is forbidden".into(),
                        ),
                        ("web/api-5c9b".into(), "Timed out".into()),
                    ],
                    hint: Some("Ask a cluster admin for the delete permission.".into()),
                }
            } else {
                BulkResult {
                    succeeded: TOTAL,
                    ..Default::default()
                }
            };
            this.update(cx, |_, cx| toast.finish(result, cx)).ok();
        })
        .detach();
    }

    fn section(title: &'static str, cx: &App) -> Div {
        div().flex().flex_col().gap_3().child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().muted_foreground)
                .child(title.to_uppercase()),
        )
    }

    /// A row handler that stores its value in the form demo.
    fn form_setter<T: 'static>(
        cx: &Context<Self>,
        apply: fn(&mut FormDemo, T),
    ) -> impl Fn(T, &mut Window, &mut App) + 'static {
        let entity = cx.entity().downgrade();
        move |value, _, cx| {
            entity
                .update(cx, |gallery, cx| {
                    apply(&mut gallery.form, value);
                    cx.notify();
                })
                .ok();
        }
    }

    /// Settings rows; each is controlled, the gallery stores the values.
    fn settings_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let form = &self.form;
        let clear = Self::form_setter(cx, |f, ()| f.path = None);
        SettingGroup::new()
            .title("AI agent (MCP)")
            .description("Toggle, select, number with unit, path picker and reset-to-default.")
            .child(
                ToggleRow::new("demo-toggle", "AI agent access")
                    .description("Allow an agent connected over MCP.")
                    .checked(form.enabled)
                    .default_value(false)
                    .on_change(Self::form_setter(cx, |f, on| f.enabled = on)),
            )
            .child(
                SelectRow::new(
                    "demo-select",
                    "Maximum profile",
                    ["Read-only", "Debug", "Deploy"],
                )
                .selected(form.profile)
                .default_index(0)
                .on_change(Self::form_setter(cx, |f, ix| f.profile = ix)),
            )
            .child(
                NumberRow::new("demo-number", "Session TTL")
                    .value(form.ttl)
                    .range(15..=480)
                    .step(15)
                    .unit("min")
                    .default_value(30)
                    .on_change(Self::form_setter(cx, |f, ttl| f.ttl = ttl)),
            )
            .child(
                PathRow::new("demo-path", "helm binary")
                    .path(form.path.clone())
                    .placeholder("Auto: /usr/local/bin/helm")
                    .on_pick(Self::form_setter(cx, |f, path: std::path::PathBuf| {
                        f.path = Some(path.display().to_string())
                    }))
                    .on_clear(move |window, cx| clear((), window, cx)),
            )
            .child(
                SettingRow::new("demo-fixed", "Secret values are masked")
                    .description("A plain row with any control.")
                    .control(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Always"),
                    ),
            )
    }

    /// Nested navigation items and a list of items beside a page (#402).
    fn side_list_demo(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let items = vec![
            SideListItem::new("minikube", "minikube")
                .detail("https://192.168.49.2:8443")
                .accent(TileColor::Blue.color(cx))
                .badge("kubeconfig"),
            SideListItem::new("prod-eu", "prod-eu")
                .detail("https://prod.example.com")
                .accent(TileColor::Orange.color(cx))
                .badge("vault"),
            SideListItem::new("staging", "staging")
                .detail("Hidden from the hotbar")
                .accent(TileColor::Gray.color(cx))
                .badge("linked")
                .muted(true),
        ];
        let picked = self.side_pick.clone();
        div()
            .flex()
            .h(px(220.))
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .w(px(180.))
                    .flex_shrink_0()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .border_r_1()
                    .border_color(theme.border)
                    .child(NavItem::new("demo-sub-parent", Lucide::Server, "Clusters"))
                    .child(
                        NavItem::new("demo-sub-common", Lucide::SlidersHorizontal, "Common")
                            .nested(true),
                    )
                    .child(
                        NavItem::new("demo-sub-each", Lucide::Layers, "Per cluster")
                            .nested(true)
                            .selected(true),
                    ),
            )
            .child(
                SideList::new("demo-side-list", items)
                    .selected(Some(picked.clone()))
                    .on_select(cx.listener(|this, key: &SharedString, _, cx| {
                        this.side_pick = key.clone();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .p_4()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!("Settings of {picked}")),
            )
    }

    fn swatches(cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = [
            ("background", theme.background),
            ("foreground", theme.foreground),
            ("muted", theme.muted),
            ("muted fg", theme.muted_foreground),
            ("border", theme.border),
            ("primary", theme.primary),
            ("success", theme.success),
            ("warning", theme.warning),
            ("danger", theme.danger),
            ("info", theme.info),
        ];
        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .children(colors.into_iter().map(|(name, color)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .w(px(96.))
                    .child(
                        div()
                            .h(px(56.))
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .bg(color),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(name),
                    )
            }))
    }

    /// Status bars of the main window: steady, and syncing with an MCP
    /// call waiting for confirmation.
    fn status_bars(cx: &App) -> impl IntoElement {
        let frame = |bar: StatusBar| {
            div()
                .w_full()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .overflow_hidden()
                .child(bar)
        };
        let contexts = |menu: PopupMenu| {
            ["minikube", "prod-eu", "staging"]
                .into_iter()
                .fold(menu.label("Switch context"), |menu, name| {
                    menu.item(PopupMenuItem::new(name).checked(name == "minikube"))
                })
        };
        let steady = StatusBar::new("status-steady")
            .left(
                StatusBarItem::new("sb-cluster", "minikube · v1.31.0")
                    .dot(Tone::Success)
                    .tooltip("Switch context")
                    .menu(move |menu, _, _| contexts(menu)),
            )
            .left(StatusBarItem::new("sb-ns", "kube-system").icon(Lucide::Layers))
            .right(
                StatusBarItem::new("sb-forwards", "2 port forwards")
                    .icon(Lucide::ArrowRightLeft)
                    .on_click(|_, _, _| {}),
            )
            .right(
                StatusBarItem::new("sb-mcp", "MCP off")
                    .icon(Lucide::Bot)
                    .on_click(|_, _, _| {}),
            )
            .right(
                StatusBarItem::new("sb-version", "Kubach 2026.10.1")
                    .tooltip("Copy version, OS and cluster version")
                    .on_click(|_, _, _| {}),
            );
        let busy = StatusBar::new("status-busy")
            .loading(true)
            .left(
                StatusBarItem::new("sb-cluster-2", "prod-eu · connecting…")
                    .dot(Tone::Warning)
                    .on_click(|_, _, _| {}),
            )
            .left(StatusBarItem::new("sb-ns-2", "3 namespaces").icon(Lucide::Layers))
            .left(
                StatusBarItem::new("sb-sync", "Syncing…")
                    .icon(IconName::LoaderCircle)
                    .tooltip("4 watches loading"),
            )
            .left(
                StatusBarItem::new("sb-operations", "1 operation running")
                    .icon(IconName::LoaderCircle)
                    .tooltip("Show operations")
                    .on_click(|_, window, cx| Self::open_operations(window, cx)),
            )
            .left(
                OperationNotice {
                    key: 0,
                    text: "cert-manager installed".into(),
                    tone: Tone::Success,
                }
                .item("sb-operation-notice"),
            )
            .right(
                StatusBarItem::new("sb-mcp-2", "MCP · 1 to confirm")
                    .icon(Lucide::Bot)
                    .highlight(true)
                    .on_click(|_, _, _| {}),
            )
            .right(StatusBarItem::new("sb-version-2", "Kubach 2026.10.1").on_click(|_, _, _| {}));
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(frame(steady))
            .child(frame(busy))
    }

    fn render_hotbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tiles = self
            .clusters
            .iter()
            .map(|id| sample_tile(id).active(*id == self.cluster))
            .collect();
        let gallery = cx.entity().downgrade();
        let (select, reorder) = (gallery.clone(), gallery);
        Hotbar::new("gallery-hotbar", tiles)
            .on_select(move |id, _, cx| {
                select
                    .update(cx, |this, cx| {
                        this.cluster = id.clone();
                        cx.notify();
                    })
                    .ok();
            })
            .on_move(move |dragged, target, _, cx| {
                reorder
                    .update(cx, |this, cx| {
                        let from = this.clusters.iter().position(|id| id == dragged);
                        let to = this.clusters.iter().position(|id| id == target);
                        if let (Some(from), Some(to)) = (from, to) {
                            let id = this.clusters.remove(from);
                            this.clusters.insert(to, id);
                            cx.notify();
                        }
                    })
                    .ok();
            })
            .tile_menu(|_, menu, _, _| {
                menu.item(PopupMenuItem::new("Disconnect"))
                    .item(PopupMenuItem::new("Rename…"))
                    .item(PopupMenuItem::new("Hide"))
            })
            .more_menu(|menu, _, _| menu.label("Hidden").item(PopupMenuItem::new("old-cluster")))
            .on_add(|_, _| {})
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = [
            (IconName::LayoutDashboard, "Overview", None),
            (IconName::Cpu, "Workloads", Some(42)),
            (IconName::Network, "Network", Some(7)),
            (IconName::HardDrive, "Storage", Some(3)),
        ];
        let theme = cx.theme();
        div()
            .w(SIDEBAR_WIDTH)
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .p_2()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .children(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (icon, label, count))| {
                        let item = NavItem::new(("nav", ix), icon, label)
                            .selected(ix == self.nav)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.nav = ix;
                                cx.notify();
                            }));
                        match count {
                            Some(count) => item.count(count),
                            None => item,
                        }
                    }),
            )
    }

    fn render_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = unix_now();
        let theme_label = if cx.theme().mode.is_dark() {
            "Light theme"
        } else {
            "Dark theme"
        };
        div()
            .id("content")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_10()
            .p_8()
            .child(
                PageHeader::new("Kubach UI")
                    .description(
                        "Design tokens and components. Vercel-inspired, translucent, GPU-rendered.",
                    )
                    .action(
                        Button::new("toggle-theme")
                            .outline()
                            .small()
                            .label(theme_label)
                            .on_click(|_, window, cx| kubach_ui::theme::toggle(window, cx)),
                    ),
            )
            .child(Self::section("Colors", cx).child(Self::swatches(cx)))
            .child(
                Self::section("Typography", cx).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_3xl()
                                .font_weight(FontWeight::BOLD)
                                .child("Clusters at a glance"),
                        )
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Workloads"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .child("Body text for descriptions and table cells."),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Muted text for secondary information."),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_family(cx.theme().mono_font_family.clone())
                                .child("kubectl get pods -n kube-system"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{} / {}",
                                    cx.theme().font_family,
                                    cx.theme().mono_font_family
                                )),
                        ),
                ),
            )
            .child(
                Self::section("Buttons", cx).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(Button::new("b-primary").primary().label("Deploy"))
                        .child(Button::new("b-secondary").label("Scale"))
                        .child(Button::new("b-outline").outline().label("Restart"))
                        .child(Button::new("b-ghost").ghost().label("Cancel"))
                        .child(Button::new("b-danger").danger().label("Delete"))
                        .child(
                            Button::new("b-icon")
                                .outline()
                                .icon(IconName::RefreshCw)
                                .label("Refresh"),
                        )
                        .child(BackButton::new("b-back", "Topology").tooltip("Back to Topology (⌘[)"))
                        .child(
                            BackButton::new("b-back-long", "Deployment checkout-payments-gateway-v2")
                                .tooltip("Back to Deployment checkout-payments-gateway-v2 (⌘[)"),
                        )
                        .child(
                            SplitButton::new("b-split", "Shell")
                                .icon(IconName::SquareTerminal)
                                .tooltip("Open a shell in app (default container)")
                                .menu_tooltip("Choose a container")
                                .menu(|menu, _, _| {
                                    menu.label("Containers")
                                        .item(PopupMenuItem::new("app"))
                                        .item(PopupMenuItem::new("sidecar · CrashLoopBackOff"))
                                        .separator()
                                        .label("Init containers")
                                        .item(PopupMenuItem::new("migrate · Completed"))
                                }),
                        ),
                ),
            )
            .child(
                Self::section("Status", cx)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children(DemoStatus::ALL.map(DemoStatus::badge)),
                    )
                    .child(
                        div().flex().items_center().gap_4().text_sm().children(
                            [
                                (Tone::Success, "Connected"),
                                (Tone::Warning, "Reconnecting"),
                                (Tone::Danger, "Unreachable"),
                                (Tone::Neutral, "Idle"),
                            ]
                            .map(|(tone, label)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(StatusDot::new(tone))
                                    .child(label)
                            }),
                        ),
                    )
                    // Dock tabs: the dot takes the context's hotbar color.
                    .child(
                        div().flex().items_center().gap_4().text_sm().children(
                            [
                                (TileColor::Teal, "minikube"),
                                (TileColor::Pink, "prod"),
                                (TileColor::Orange, "staging"),
                            ]
                            .map(|(color, label)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(StatusDot::color(color.color(cx)))
                                    .child(label)
                            }),
                        ),
                    )
                    .child(
                        StatusCounts::new()
                            .item(2, "errors", Tone::Danger)
                            .item(1, "out of sync", Tone::Warning)
                            .item(3, "in progress", Tone::Info)
                            .item(0, "suspended", Tone::Neutral)
                            .item(14, "healthy", Tone::Success),
                    )
                    .child(
                        StatusCounts::new()
                            .keep_zero()
                            .item(0, "problems", Tone::Danger)
                            .item(5, "synced", Tone::Success),
                    ),
            )
            .child(Self::section("Status bar", cx).child(Self::status_bars(cx)))
            .children(debug_hud_demo(cx))
            .child(
                Self::section("Resource table", cx)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(match &self.selected_pod {
                                Some(pod) => format!("{POD_COUNT} pods, selected {pod}"),
                                None => format!(
                                    "{POD_COUNT} pods. Click a header to sort, a row to select."
                                ),
                            })
                            .when(self.selected_pod.is_some(), |row| {
                                row.child(
                                    Button::new("clear-pod-selection")
                                        .ghost()
                                        .small()
                                        .label("Clear selection")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.selected_pod = None;
                                            this.pods
                                                .update(cx, |table, cx| table.clear_selection(cx));
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .child(self.render_bulk_bar(cx))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Drag a header to move its column; right-click a header \
                                 to reset the order.",
                            ),
                    )
                    // A fixed height keeps the virtualized table bounded inside
                    // the scrolling page.
                    .child(div().h(px(420.)).child(self.pods.clone())),
            )
            .child(Self::section("Settings form", cx).child(self.settings_form(cx)))
            .child(Self::section("Side list and sub-pages", cx).child(self.side_list_demo(cx)))
            .child(
                Self::section("Download progress", cx)
                    .child(
                        DownloadProgress::new("gallery-download", "Downloading Helm 3.22.1")
                            .bytes(11_200_000, Some(17_058_141))
                            .on_cancel(|_, _, _| {}),
                    )
                    .child(
                        DownloadProgress::new("gallery-download-unknown", "Downloading kustomize")
                            .bytes(640 * 1024, None),
                    ),
            )
            .child(
                Self::section("Bulk progress", cx)
                    .child(BulkProgress::new("bulk-restart", "Restarting 12 Deployments", 7, 12))
                    .child(
                        BulkProgress::new("bulk-drain", "Drain node-1", 2, 6)
                            .elapsed("41s")
                            .phase("4 pods left")
                            .on_cancel(|_, _, _| {}),
                    )
                    .child(
                        BulkProgress::new("bulk-helm", "Install of cert-manager", 0, 1)
                            .elapsed("1m12s")
                            .phase("Waiting for Job cert-manager-startupapicheck: InProgress")
                            .on_hide(|_, _, _| {})
                            .on_cancel(|_, _, _| {})
                            .log(
                                vec![
                                    "Creating 12 resource(s)".into(),
                                    "Waiting for 1 resource (timeout 5m0s)".into(),
                                    "Waiting for Job cert-manager-startupapicheck: InProgress".into(),
                                ],
                                true,
                                |_, _, _| {},
                            ),
                    )
                    .child(
                        BulkProgress::new("bulk-delete-cm", "Deleting 4 ConfigMaps", 4, 4).finished(BulkResult {
                            succeeded: 4,
                            ..Default::default()
                        }),
                    )
                    .child(
                        BulkProgress::new("bulk-delete-pods", "Deleting 5 Pods", 5, 5)
                            .finished(BulkResult {
                                succeeded: 3,
                                failures: vec![
                                    (
                                        "kube-system/coredns-7d8f".into(),
                                        "No permission (RBAC): pods \"coredns-7d8f\" is forbidden"
                                            .into(),
                                    ),
                                    (
                                        "ingress-nginx/ingress-nginx-controller-6f4d8b7c9-x2kqz"
                                            .into(),
                                        "ServiceError: connection closed before message completed"
                                            .into(),
                                    ),
                                ],
                                hint: Some(
                                    "Ask a cluster admin for the delete permission.".into(),
                                ),
                            })
                            .on_dismiss(|_, _, _| {}),
                    ),
            )
            .child(
                Self::section("Action toast", cx)
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Progress, then the outcome, over the window: a success \
                                 hides itself, failures stay until closed.",
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("toast-success")
                                    .outline()
                                    .small()
                                    .label("Restart 5 Deployments")
                                    .on_click(cx.listener(|_, _, window, cx| {
                                        Self::demo_toast(false, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("toast-failure")
                                    .outline()
                                    .small()
                                    .label("Delete 5 Pods (2 fail)")
                                    .on_click(cx.listener(|_, _, window, cx| {
                                        Self::demo_toast(true, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("toast-helm")
                                    .outline()
                                    .small()
                                    .label("helm install (phases)")
                                    .on_click(cx.listener(|_, _, window, cx| {
                                        Self::demo_helm_toast(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("toast-operations")
                                    .outline()
                                    .small()
                                    .label("Operations…")
                                    .on_click(|_, window, cx| Self::open_operations(window, cx)),
                            )
                            .child(
                                Button::new("confirm-live")
                                    .outline()
                                    .small()
                                    .label("Drain node-1… (options)")
                                    .on_click(|_, window, cx| Self::demo_drain_dialog(window, cx)),
                            ),
                    ),
            )
            .child(
                Self::section("Navigation tree", cx)
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(match &self.tree_pick {
                                Some(id) => format!("Opened {id}"),
                                None => "Click or use ↑ ↓ ← → Enter after clicking a row.".into(),
                            }),
                    )
                    .child(
                        div()
                            .w(SIDEBAR_WIDTH)
                            .h(px(260.))
                            .p_2()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().sidebar)
                            .child(self.tree.clone()),
                    ),
            )
            .child(
                Self::section("Hotbar", cx)
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Active: {}. Drag to reorder, right-click for the menu.",
                                self.cluster
                            )),
                    )
                    .child(
                        div()
                            .h(px(360.))
                            .flex()
                            .rounded_lg()
                            .overflow_hidden()
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(self.render_hotbar(cx)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Tile previews: theme presets, then a custom color \
                                 (\"lab\" above), plain and active.",
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_3()
                            .children(
                                TileColor::ALL
                                    .into_iter()
                                    .chain([SAMPLE_CUSTOM_TILE])
                                    .flat_map(|color| {
                                        let label = &color.name()[..2];
                                        [
                                            TilePreview::new(label.to_owned(), color),
                                            TilePreview::new(label.to_owned(), color).active(true),
                                        ]
                                    }),
                            ),
                    ),
            )
            .child(
                Self::section("Multi-select", cx)
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Applied: {}. Toggle keeps the list open; Enter or click outside applies; ⌥-click picks one.",
                                self.namespaces.read(cx).selection().label("All namespaces")
                            )),
                    )
                    .child(div().flex().child(self.namespaces.clone())),
            )
            .child(Self::section("Filter bar", cx).child(self.render_filter_bar(cx)))
            .child(Self::section("Log list", cx).child(self.render_log_demo(cx)))
            .child(
                Self::section("Text selection", cx)
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Drag, Shift+click, double-click a word, triple-click a line; \
                                 Cmd/Ctrl+A, Cmd/Ctrl+C. The log list above uses the same selector.",
                            ),
                    )
                    .child(
                        div()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .overflow_hidden()
                            .child(self.snippet.clone()),
                    ),
            )
            .child(
                Self::section("Terminal", cx).child(
                    div()
                        .h(px(380.))
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .overflow_hidden()
                        .child(self.terminal.clone()),
                ),
            )
            .child(
                Self::section("Labels", cx).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(LabelChip::new("app", "nginx"))
                        .child(LabelChip::new("tier", "frontend"))
                        .child(LabelChip::new("app.kubernetes.io/managed-by", "Helm")),
                ),
            )
            .child(
                Self::section("Panel", cx).child(
                    Panel::new().title("Pod nginx-7c5ddbdf54-x2k9p").child(
                        KeyValueList::new()
                            .row("Namespace", "default")
                            .row("Status", DemoStatus::Running.badge())
                            .row("Node", "ip-10-0-1-23.eu-central-1.compute.internal")
                            .row("Pod IP", "10.244.1.17")
                            .row("Restarts", "0"),
                    ),
                ),
            )
            .child(
                Self::section("Events", cx).child(
                    // Times relative to this render; tables keep theirs live.
                    Panel::new().title("Events").child(EventList::new([
                        EventEntry::new("BackOff", "Back-off restarting failed container api")
                            .warning(true)
                            .object("Pod/api-7c5ddbdf54-x2k9p")
                            .count(14)
                            .since(now - 2 * 60),
                        EventEntry::new(
                            "FailedScheduling",
                            "0/3 nodes are available: 3 Insufficient memory.",
                        )
                        .warning(true)
                        .object("Pod/worker-0")
                        .since(now - 6 * 60),
                        EventEntry::new("Pulled", "Successfully pulled image \"nginx:1.27\"")
                            .object("Pod/api-7c5ddbdf54-x2k9p")
                            .since(now - 12 * 60),
                        EventEntry::new("ScalingReplicaSet", "Scaled up replica set api to 3")
                            .object("Deployment/api")
                            .since(now - 3600),
                    ])),
                ),
            )
            .child(self.render_timeline_demo(cx))
            .child(
                Self::section("Usage meters", cx).child(
                    Panel::new().title("Resources").child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                UsageMeter::new("CPU", "120m · 48% of 250m request · no limit")
                                    .fill(Some(0.48)),
                            )
                            .child(
                                UsageMeter::new(
                                    "Memory",
                                    "430Mi · 168% of 256Mi request · 84% of 512Mi limit",
                                )
                                .fill(Some(0.84)),
                            )
                            .child(
                                UsageMeter::new("Disk", "3.7Gi · 95% of 3.9Gi allocatable")
                                    .fill(Some(0.95)),
                            )
                            .child(UsageMeter::new("Network", "5m · no request · no limit")),
                    ),
                ),
            )
            .child(Self::section("Line chart", cx).child(line_charts()))
            .child(Self::section("Cluster overview", cx).child(overview_cards()))
            .child(Self::section("Details panel", cx).child(self.details_demo(cx)))
            .child(Self::section("Side panel", cx).child(self.side_panel_demo(cx)))
            .child(Self::section("Flow graph", cx).child(self.flow_graph_demo(cx)))
            .child(Self::section("Graph canvas", cx).child(self.graph_demo.render(cx)))
            .child(
                Self::section("Keyboard", cx).child(
                    div().flex().items_center().gap_4().text_sm().children(
                        [
                            ("cmd-k", "Command palette"),
                            ("cmd-shift-l", "Logs"),
                            ("cmd-q", "Quit"),
                        ]
                        .map(|(keys, label)| {
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(Kbd::new(Keystroke::parse(keys).expect("valid keystroke")))
                                .child(label)
                        }),
                    ),
                ),
            )
            .child(
                // Live buttons: they act on this window. In the app they show
                // only under client-side decorations (GNOME on Wayland).
                Self::section("Window controls", cx).child(
                    div()
                        .h(TITLEBAR_HEIGHT)
                        .flex()
                        .justify_end()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(WindowControls::new().always()),
                ),
            )
            .child(
                Self::section("Tabs", cx).child(
                    div()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .overflow_hidden()
                        .child(TabStrip::new(
                            "gallery-tabs",
                            vec![
                                // The home tab, in the current cluster's color.
                                TabItem::new("Deployments")
                                    .icon(IconName::Cpu)
                                    .accent(TileColor::Teal.color(cx)),
                                TabItem::new("ConfigMap/app-config").closable(true).dirty(true),
                                TabItem::new("Deployment/web").badge("prod").closable(true),
                                TabItem::new("New resource").closable(true),
                            ],
                            1,
                        )),
                ),
            )
            .child(
                Self::section("Diff", cx).child(
                    div()
                        .h(px(260.))
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .overflow_hidden()
                        .child(DiffView::new("gallery-diff", sample_diff())),
                ),
            )
            .child(
                Self::section("Check result", cx).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(CheckResult::new(
                            "gallery-check-running",
                            CheckStatus::Running("Connecting to https://10.0.0.1:6443…".into()),
                        ))
                        .child(CheckResult::new(
                            "gallery-check-passed",
                            CheckStatus::Passed {
                                title: "Connected: Kubernetes v1.31.2".into(),
                                detail: Some("https://10.0.0.1:6443".into()),
                            },
                        ))
                        .child(CheckResult::new(
                            "gallery-check-failed",
                            CheckStatus::Failed {
                                title: "TLS: the server's certificate is not signed by the given CA"
                                    .into(),
                                detail: Some("invalid peer certificate: UnknownIssuer".into()),
                                hint: Some(
                                    "Use the cluster's CA certificate (certificate-authority-data \
                                     in its kubeconfig)."
                                        .into(),
                                ),
                            },
                        )),
                ),
            )
            .child(Self::section("Secret input", cx).child(self.secret_input_demo(cx)))
            .child(Self::section("Data keys", cx).child(self.data_key_demo(cx)))
            .child(Self::section("Chart icon", cx).child(chart_icons()))
            .child(
                Self::section("Logo", cx).child(
                    div()
                        .flex()
                        .items_end()
                        .gap_6()
                        .child(Logo::new())
                        .child(Logo::new().size(px(32.)))
                        .child(Logo::new().size(px(64.))),
                ),
            )
            .child(
                Self::section("Loader", cx).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(spinner().with_size(px(12.)))
                        .child(spinner().color(cx.theme().muted_foreground))
                        .child(spinner().large().color(cx.theme().primary))
                        .child(
                            Button::new("gallery-loader-refresh")
                                .outline()
                                .small()
                                .icon(IconName::RefreshCw)
                                .spinning(true)
                                .tooltip("Reloading releases…"),
                        )
                        .child(
                            Button::new("gallery-loader-apply")
                                .primary()
                                .small()
                                .label("Apply")
                                .spinning(true),
                        ),
                ),
            )
            .child(
                Self::section("Loading bar", cx).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Indeterminate: a list from the cache refreshing"),
                        )
                        .child(
                            div()
                                .w_full()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .overflow_hidden()
                                .child(LoadingBar::new("gallery-loading-bar")),
                        ),
                ),
            )
            .child(
                Self::section("Empty state", cx).child(
                    EmptyState::new(IconName::Globe, "No cluster connected")
                        .description("Pick a context from your kubeconfig to browse resources.")
                        .action(
                            Button::new("connect")
                                .primary()
                                .small()
                                .label("Connect cluster"),
                        ),
                ),
            )
    }
}

/// State of the Secret input demo: generator settings and whether a value
/// was "generated" (the demo holds no value at all).
struct SecretDemo {
    length: usize,
    charset: usize,
    generated: bool,
}

impl Default for SecretDemo {
    fn default() -> Self {
        Self {
            length: 32,
            charset: 0,
            generated: false,
        }
    }
}

impl Gallery {
    fn secret_input_demo(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let demo = &self.secret_demo;
        let status = if demo.generated {
            format!("generated, {} characters, hidden", demo.length)
        } else {
            "no value yet".to_owned()
        };
        let tone = if demo.generated {
            Tone::Success
        } else {
            Tone::Neutral
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .max_w(px(640.))
            .child(UntrustedText::new(
                "From the agent, not verified",
                "The app needs a database password for the new staging environment.",
            ))
            .child(
                SecretField::new("gallery-secret-password", "password")
                    .badge("asked by the agent", Tone::Info)
                    .status(status, tone)
                    .action(
                        Button::new("gallery-secret-copy")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .tooltip("Copy (cleared from the clipboard in 30 s)"),
                    )
                    .body(
                        GeneratorControls::new("gallery-secret-gen", demo.length, 8..=256)
                            .charsets(["a-z A-Z 0-9", "+ symbols", "hex"], demo.charset)
                            .on_length(cx.listener(|this, length: &usize, _, cx| {
                                this.secret_demo.length = *length;
                                cx.notify();
                            }))
                            .on_charset(cx.listener(|this, ix: &usize, _, cx| {
                                this.secret_demo.charset = *ix;
                                cx.notify();
                            }))
                            .on_generate(cx.listener(|this, _, _, cx| {
                                this.secret_demo.generated = true;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                SecretField::new("gallery-secret-old", "legacy-token")
                    .status("will be removed", Tone::Danger)
                    .struck(true),
            )
            .child(
                FilePickField::new("gallery-secret-cert")
                    .chosen(Some("server.crt"))
                    .detail("CN=example.com, valid until 2027-01-01", Tone::Success),
            )
            .child(
                FilePickField::new("gallery-secret-key")
                    .chosen(Some("other.key"))
                    .detail("does not belong to the certificate", Tone::Danger),
            )
    }
}

impl Gallery {
    fn data_key_demo(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let editor = |state: &Entity<EditorState>, cx: &App| {
            let lines = state.read(cx).value().lines().count();
            Editor::new(state).h(data_key_editor_height(lines))
        };
        let [valid, broken] = &self.data_key_editors;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .max_w(px(640.))
            .child(
                DataKeyBlock::labeled("gallery-key-settings", "settings.json", false, cx)
                    .kind("JSON")
                    .status("changed", Tone::Info)
                    .action(
                        Button::new("gallery-key-format")
                            .ghost()
                            .xsmall()
                            .icon(Lucide::Braces)
                            .label("Format"),
                    )
                    .action(
                        Button::new("gallery-key-remove")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Delete),
                    )
                    .body(editor(valid, cx)),
            )
            .child(
                DataKeyBlock::labeled("gallery-key-logging", "logging", false, cx)
                    .kind("JSON")
                    .status("new", Tone::Success)
                    .body(editor(broken, cx))
                    .problem(
                        Some("line 3: Invalid JSON: trailing comma. Can still be saved as text."),
                        Tone::Warning,
                    ),
            )
            .child(
                DataKeyBlock::labeled("gallery-key-logo", "logo.png", false, cx)
                    .kind("Binary")
                    .status("2.4 KiB", Tone::Neutral)
                    .action(
                        Button::new("gallery-key-file")
                            .ghost()
                            .xsmall()
                            .icon(IconName::File)
                            .label("Replace from file"),
                    ),
            )
            .child(
                DataKeyBlock::labeled("gallery-key-old", "legacy.conf", true, cx)
                    .status("removed", Tone::Danger),
            )
    }
}

/// Chart icons: an image, a missing file (the letter), no image at all.
/// The debug overlay of debug builds (#291), with made-up metrics: a busy
/// window and an idle one.
#[cfg(debug_assertions)]
fn debug_hud_demo(cx: &App) -> Option<Div> {
    use kubach_ui::DebugHud;
    let busy = DebugHud::new()
        .section("Frames")
        .row("FPS", "58")
        .row("Frame", "4.1 ms")
        .toned_row("Avg / max 5 s", "6.3 / 21.7 ms", Tone::Warning)
        .section("Process")
        .row("CPU", "12.4 %")
        .row("Memory", "312.6 MB")
        .row("Threads", "41")
        .section("Kubach")
        .row("Clusters", "2")
        .row("Watches", "14")
        .row("Cached objects", "3 482")
        .row("Port forwards", "1")
        .row("Runtime tasks", "27")
        .footer("⌘⇧D to hide");
    let idle = DebugHud::new()
        .section("Frames")
        .toned_row("FPS", "idle", Tone::Neutral)
        .row("Frame", "—")
        .row("Avg / max 5 s", "— / —")
        .section("Process")
        .row("CPU", "0.4 %")
        .row("Memory", "286.0 MB")
        .row("Threads", "38");
    Some(
        Gallery::section("Debug overlay", cx).child(
            div()
                .flex()
                .items_start()
                .gap_4()
                .p_4()
                .rounded_lg()
                .bg(cx.theme().muted)
                .child(busy)
                .child(idle),
        ),
    )
}

/// Debug overlay exists only in debug builds.
#[cfg(not(debug_assertions))]
fn debug_hud_demo(_: &App) -> Option<Div> {
    None
}

fn chart_icons() -> impl IntoElement {
    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#326ce5"/><circle cx="16" cy="16" r="8" fill="none" stroke="#fff" stroke-width="3"/></svg>"##;
    let image = std::sync::Arc::new(Image::from_bytes(ImageFormat::Svg, SVG.to_vec()));
    let labeled = |icon: ChartIcon, label: &'static str| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(icon)
            .child(label)
    };
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_6()
        .child(labeled(
            ChartIcon::new("kubernetes").image(image.clone()),
            "image",
        ))
        .child(labeled(
            ChartIcon::new("bitnami/nginx").path(Some("/nonexistent/icon.png".into())),
            "failed to load",
        ))
        .child(labeled(ChartIcon::new("cert-manager"), "no icon"))
        .child(labeled(
            ChartIcon::new("argo-cd").image(image).size(px(32.)),
            "32 px",
        ))
}

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(kubach_ui::theme::window_background(cx))
            .text_color(theme.foreground)
            .child(
                WindowTitlebar::new("titlebar")
                    .h(TITLEBAR_HEIGHT)
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_b_1()
                    .border_color(theme.title_bar_border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("kubach ui gallery"),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .min_h_0()
                    .child(self.render_sidebar(cx))
                    .child(self.render_content(cx)),
            )
    }
}

/// A Deployment edit: replicas and image changed, a label added.
fn sample_diff() -> Vec<DiffViewLine> {
    let before = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n  namespace: default\nspec:\n  replicas: 1\n  selector:\n    matchLabels:\n      app: web\n  template:\n    metadata:\n      labels:\n        app: web\n    spec:\n      containers:\n      - name: app\n        image: nginx:1.27\n        ports:\n        - containerPort: 80";
    let mut lines = Vec::new();
    let (mut old, mut new) = (0, 0);
    let mut push = |kind, text: &str, old: &mut usize, new: &mut usize| {
        let (o, n) = match kind {
            DiffLineKind::Unchanged => (Some(*old), Some(*new)),
            DiffLineKind::Removed => (Some(*old), None),
            DiffLineKind::Added => (None, Some(*new)),
        };
        if o.is_some() {
            *old += 1;
        }
        if n.is_some() {
            *new += 1;
        }
        lines.push(DiffViewLine {
            kind,
            old: o,
            new: n,
            text: text.to_string().into(),
        });
    };
    for line in before.lines() {
        match line {
            "  replicas: 1" => {
                push(DiffLineKind::Removed, line, &mut old, &mut new);
                push(DiffLineKind::Added, "  replicas: 3", &mut old, &mut new);
            }
            "        image: nginx:1.27" => {
                push(DiffLineKind::Removed, line, &mut old, &mut new);
                push(
                    DiffLineKind::Added,
                    "        image: nginx:1.29",
                    &mut old,
                    &mut new,
                );
            }
            "        app: web" => {
                push(DiffLineKind::Unchanged, line, &mut old, &mut new);
                push(
                    DiffLineKind::Added,
                    "        tier: frontend",
                    &mut old,
                    &mut new,
                );
            }
            _ => push(DiffLineKind::Unchanged, line, &mut old, &mut new),
        }
    }
    lines
}

/// Deterministic multi-pod log lines for the log list demo.
#[derive(Default)]
struct FakeLog {
    counter: usize,
}

impl FakeLog {
    fn next(&mut self) -> LogEntry {
        const PODS: [&str; 3] = ["api-7d9f/app", "api-7d9f/proxy", "worker-5c2a/app"];
        const MESSAGES: [&str; 6] = [
            "GET /healthz 200 0.4ms",
            "POST /v1/orders 201 12.7ms",
            "cache miss for key user:4821, fetching from postgres",
            "error: upstream connect error or disconnect/reset before headers",
            "processed batch of 128 events in 35ms",
            "retrying request (attempt 2/5) after timeout; a long line that keeps going to show how wrapping behaves when the toggle is on and the panel is narrow",
        ];
        let n = self.counter;
        self.counter += 1;
        let pod = n % PODS.len();
        let seconds = n / 10;
        let message = (n * 7 + n / 3) % MESSAGES.len();
        let text = format!("#{n} {}", MESSAGES[message]);
        // Program output colors, as `kubach_core::ansi` would parse them.
        let start = text.find(' ').map_or(0, |ix| ix + 1);
        let style = |foreground, bold, dim| SpanStyle {
            foreground: Some(foreground),
            bold,
            dim,
            ..SpanStyle::default()
        };
        let spans = match message {
            0 => vec![(0..3, style(SpanColor::Named(2), false, false))],
            1 => vec![(0..4, style(SpanColor::Rgb(0x7c, 0x9c, 0xff), false, false))],
            2 => vec![(
                0..text.len() - start,
                style(SpanColor::Named(7), false, true),
            )],
            3 => vec![(0..5, style(SpanColor::Named(1), true, false))],
            4 => vec![(0..9, style(SpanColor::Indexed(39), false, false))],
            _ => vec![(0..8, style(SpanColor::Named(11), false, false))],
        };
        let spans = spans
            .into_iter()
            .map(|(range, style)| StyledSpan {
                range: range.start + start..range.end + start,
                style,
            })
            .collect();
        LogEntry::new(text)
            .spans(spans)
            .tag(PODS[pod], pod)
            .timestamp(format!(
                "2026-10-22T{:02}:{:02}:{:02}.{:03}Z",
                (seconds / 3600) % 24,
                (seconds / 60) % 60,
                seconds % 60,
                (n * 37) % 1000
            ))
    }
}

/// A short YAML document drawn with [`LineSelector`]: the selection
/// mechanism without a virtualized list.
struct SelectableSnippet {
    lines: Vec<&'static str>,
    selector: LineSelector,
}

impl SelectableSnippet {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            lines: vec![
                "apiVersion: apps/v1",
                "kind: Deployment",
                "metadata:",
                "  name: api-server",
                "  namespace: web",
                "spec:",
                "  replicas: 3",
                "  template:",
                "    spec:",
                "      containers:",
                "        - name: app",
                "          image: ghcr.io/kubach/api:2026.10.1",
            ],
            selector: LineSelector::new(cx),
        }
    }
}

impl SelectableLines for SelectableSnippet {
    fn line_selector(&self) -> &LineSelector {
        &self.selector
    }

    fn line_selector_mut(&mut self) -> &mut LineSelector {
        &mut self.selector
    }

    fn line_ids(&self) -> std::ops::Range<u64> {
        0..self.lines.len() as u64
    }

    fn line_text(&self, line: u64) -> Option<String> {
        self.lines.get(line as usize).map(|text| text.to_string())
    }

    fn scroll_by(&mut self, _: Pixels, _: &mut Context<Self>) {}
}

impl Render for SelectableSnippet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.selector.begin_frame();
        let theme = cx.theme();
        let rows: Vec<Div> = self
            .lines
            .iter()
            .enumerate()
            .map(|(ix, text)| {
                let key = text.find(':').unwrap_or(0);
                let mut highlights = vec![(
                    0..key,
                    HighlightStyle {
                        color: Some(theme.muted_foreground),
                        ..HighlightStyle::default()
                    },
                )];
                highlights.extend(self.selector.highlight(ix as u64, text.len(), cx));
                let styled =
                    StyledText::new(text.to_string()).with_highlights(layer_highlights(highlights));
                self.selector.row(ix as u64, styled).px_3()
            })
            .collect();
        let root = div()
            .relative()
            .py_2()
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .line_height(px(18.))
            .text_color(theme.foreground);
        selectable(root, &self.selector, cx).children(rows)
    }
}

/// A pretend shell for the terminal demo: echoes typed text and knows a
/// couple of commands. Replies to terminal queries are ignored.
#[derive(Default)]
struct FakeShell {
    line: String,
}

impl FakeShell {
    const PROMPT: &str = "\x1b[32mdemo\x1b[0m:\x1b[34m~\x1b[0m$ ";

    fn banner(&self) -> String {
        let mut out =
            String::from("Kubach terminal demo. Try `colors`, `glyphs`, `clear` or any text.\r\n");
        out.push_str(&Self::colors());
        out.push_str(&Self::glyphs());
        out.push_str(Self::PROMPT);
        out
    }

    fn colors() -> String {
        let mut out = String::new();
        for base in [30, 90] {
            for color in 0..8 {
                out.push_str(&format!("\x1b[{}m ██ \x1b[0m", base + color));
            }
            out.push_str("\r\n");
        }
        out.push_str("\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \x1b[7minverse\x1b[0m \x1b[38;5;208m256-color\x1b[0m \x1b[38;2;80;227;194mtruecolor\x1b[0m\r\n");
        out
    }

    /// Symbols the terminal draws over the whole cell: lines, blocks,
    /// braille and powerline.
    fn glyphs() -> String {
        let lines = [
            "╭─────┬─────╮ ┏━━━┳━━━┓ ╔═══╦═══╗ ┌┄┄┬╌╌┐ ╱╲╳",
            "│ pod │ 3/3 │ ┃ a ┃ b ┃ ║ x ║ y ║ ┆  ╎  ┆ ╲╱╳",
            "├─────┼─────┤ ┣━━━╋━━━┫ ╠═══╬═══╣ ├──┼──┤ ▁▂▃▄▅▆▇█",
            "╰─────┴─────╯ ┗━━━┻━━━┛ ╚═══╩═══╝ └──┴──┘ █▉▊▋▌▍▎▏",
            "CPU [\x1b[32m████████\x1b[33m▓▓▒▒░░\x1b[0m      ] ▖▗▘▙▚▛▜▝▞▟ ▀▄▌▐",
            "\x1b[36m⣀⣀⣠⣤⣴⣶⣾⣿⣷⣶⣦⣤⣄⣀⡀⠀⢀⣀⣤⣶⣿⣿⣿⣶⣤⣀\x1b[0m ⠁⠂⠄⡀⠈⠐⠠⢀ ⣿",
            "\x1b[30;44m kubach \x1b[34;42m\u{E0B0}\x1b[30m ~/k8s \x1b[32;45m\u{E0B0}\x1b[30m main \x1b[0;35m\u{E0B0}\x1b[0m \u{E0B1} \u{E0B3} \x1b[33m\u{E0B6}\x1b[30;43mprod\x1b[0;33m\u{E0B4}\x1b[0m \u{E0B5}\u{E0B7}",
        ];
        lines.iter().map(|line| format!("{line}\r\n")).collect()
    }

    fn input(&mut self, bytes: &[u8]) -> String {
        if bytes.first() == Some(&0x1b) {
            return String::new();
        }
        let mut out = String::new();
        for c in String::from_utf8_lossy(bytes).chars() {
            match c {
                '\r' => {
                    out.push_str("\r\n");
                    match self.line.trim() {
                        "" => {}
                        "colors" => out.push_str(&Self::colors()),
                        "glyphs" => out.push_str(&Self::glyphs()),
                        "clear" => out.push_str("\x1b[2J\x1b[H"),
                        line => out.push_str(&format!("{line}: command not found\r\n")),
                    }
                    self.line.clear();
                    out.push_str(Self::PROMPT);
                }
                '\x7f' => {
                    if self.line.pop().is_some() {
                        out.push_str("\x08 \x08");
                    }
                }
                '\x03' => {
                    self.line.clear();
                    out.push_str("^C\r\n");
                    out.push_str(Self::PROMPT);
                }
                c if !c.is_control() => {
                    self.line.push(c);
                    out.push(c);
                }
                _ => {}
            }
        }
        out
    }
}

/// Container cubes matching a fake pod's status: some pods have an init
/// container (dimmed, before a divider); sorted by containers not ready.
fn fake_containers(
    status: DemoStatus,
    containers: u64,
    ready: u64,
    restarts: u64,
    next: &mut impl FnMut() -> u64,
) -> ResourceCell {
    use ContainerCubeState::*;
    const NAMES: [&str; 3] = ["app", "proxy", "exporter"];
    let cube = |name: &str, role: Option<&str>, state: ContainerCubeState, text: &str, n: u64| {
        StatusCube::container(state, container_tooltip(name, role, text, n as i32))
    };
    let mut init = Vec::new();
    if next().is_multiple_of(3) {
        let (state, text) = match status {
            DemoStatus::Pending if next().is_multiple_of(2) => {
                (Failed, "Terminated: Error (exit code 1)")
            }
            DemoStatus::Pending => (Ready, "Running"),
            _ => (Completed, "Terminated: Completed (exit code 0)"),
        };
        init.push(cube("migrate", Some("init"), state, text, 0));
    }
    let init_blocks = init.iter().any(|c| c.tone != Tone::Neutral);
    let cubes: Vec<_> = (0..containers)
        .map(|ix| {
            let name = NAMES[ix as usize];
            let (state, text, n) = match status {
                DemoStatus::Succeeded => (Completed, "Terminated: Completed (exit code 0)", 0),
                DemoStatus::Pending if init_blocks => (Starting, "Waiting: PodInitializing", 0),
                DemoStatus::Pending if ix < ready => (Ready, "Running", 0),
                DemoStatus::Pending => (Starting, "Waiting: ContainerCreating", 0),
                DemoStatus::CrashLoopBackOff if ix == 0 => (
                    Failing,
                    "Waiting: CrashLoopBackOff (back-off 5m0s restarting failed container)",
                    restarts,
                ),
                DemoStatus::CrashLoopBackOff | DemoStatus::Terminating if ix >= ready => {
                    (NotReady, "Running, not ready", 0)
                }
                _ => (Ready, "Running", restarts),
            };
            cube(name, None, state, text, n)
        })
        .collect();
    let not_ready = init
        .iter()
        .chain(&cubes)
        .filter(|c| c.tone != Tone::Success && (c.hollow || c.tone != Tone::Neutral))
        .count() as i64;
    let total = (init.len() + cubes.len()) as i64;
    ResourceCell::cubes(init, cubes).sort_by(not_ready * 1000 + total)
}

/// Deterministic pod-like rows for the resource table demo.
fn fake_pods(count: usize) -> Vec<ResourceRow> {
    const APPS: [&str; 8] = [
        "nginx", "api", "worker", "redis", "postgres", "ingress", "metrics", "auth",
    ];
    const NAMESPACES: [&str; 5] = [
        "default",
        "kube-system",
        "monitoring",
        "payments",
        "staging",
    ];
    // Mostly healthy, like a real cluster.
    const STATUSES: [DemoStatus; 10] = [
        DemoStatus::Running,
        DemoStatus::Running,
        DemoStatus::Running,
        DemoStatus::Running,
        DemoStatus::Running,
        DemoStatus::Running,
        DemoStatus::Pending,
        DemoStatus::Succeeded,
        DemoStatus::CrashLoopBackOff,
        DemoStatus::Terminating,
    ];

    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        // xorshift64: cheap and reproducible.
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let now = unix_now();
    (0..count)
        .map(|ix| {
            let app = APPS[next() as usize % APPS.len()];
            let namespace = NAMESPACES[next() as usize % NAMESPACES.len()];
            let status = STATUSES[next() as usize % STATUSES.len()];
            let name = format!(
                "{app}-{:08x}-{:05x}",
                next() as u32,
                next() as u32 & 0xfffff
            );
            let containers = 1 + next() % 3;
            let ready = if status == DemoStatus::Running {
                containers
            } else {
                next() % containers
            };
            let restarts = if status == DemoStatus::CrashLoopBackOff {
                5 + next() % 200
            } else {
                next() % 3
            };
            // A few fresh pods show the ages ticking.
            let age = if ix < 3 {
                next() % 60
            } else {
                next() % (90 * 24 * 3600)
            } as i64;
            ResourceRow::new(
                format!("{namespace}/{name}"),
                [
                    // A trailing badge, like a chart's newer version.
                    if ix % 7 == 0 {
                        ResourceCell::text(name).trailing(|_, _| {
                            StatusBadge::new("↑ new", Tone::Info).into_any_element()
                        })
                    } else {
                        ResourceCell::text(name)
                    },
                    ResourceCell::text(namespace).muted(),
                    fake_containers(status, containers, ready, restarts, &mut next),
                    ResourceCell::status(status.label(), status.tone()),
                    if restarts > 0 {
                        ResourceCell::count_age(restarts as i64, now - age / 2)
                    } else {
                        ResourceCell::number(0)
                    },
                    ResourceCell::age(now - age).muted(),
                ],
            )
        })
        .collect()
}

/// Counter cards of the Overview screen (#414): nodes, pods by phase, CPU
/// and memory against allocatable with request and limit marks.
fn overview_cards() -> impl IntoElement {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let cores = |v: f64| SharedString::from(format!("{v:.2}"));
    let gibibytes = |v: f64| SharedString::from(format!("{:.1}Gi", v / GIB));
    div()
        .flex()
        .flex_wrap()
        .gap_3()
        .child(
            StatCard::new("Nodes", "2/3")
                .caption("ready")
                .tone(Tone::Warning)
                .scope("whole cluster")
                .child(StatusCounts::new().item(2, "ready", Tone::Success).item(
                    1,
                    "not ready",
                    Tone::Danger,
                )),
        )
        .child(
            StatCard::new("Pods", "42")
                .caption("in 2 namespaces")
                .child(
                    StatusCounts::new()
                        .keep_zero()
                        .item(38, "running", Tone::Success)
                        .item(2, "pending", Tone::Warning)
                        .item(1, "failed", Tone::Danger)
                        .item(1, "succeeded", Tone::Info),
                ),
        )
        .child(
            StatCard::new("CPU", "34%").caption("of allocatable").child(
                UsageBar::new(8.0)
                    .used(Some(2.7))
                    .requests(3.5)
                    .limits(9.0)
                    .limits_note("2 pods unbounded")
                    .format(cores),
            ),
        )
        .child(
            StatCard::new("CPU", "34%")
                .caption("used of allocatable")
                .child(
                    Donut::new(8.0)
                        .center("34%")
                        .caption("used")
                        .footer("2.7 / 8 cores")
                        .ring([DonutSegment::new(
                            "Used",
                            Some(2.7),
                            DonutColor::Tone(Tone::Success),
                        )])
                        .ring([DonutSegment::new(
                            "Requests",
                            Some(3.5),
                            DonutColor::Request,
                        )])
                        .ring([DonutSegment::new("Limits", Some(9.0), DonutColor::Limit)
                            .note("2 pods without a limit")])
                        .format(cores),
                ),
        )
        .child(
            StatCard::new("Pods", "42").caption("of 110 slots").child(
                Donut::new(110.0)
                    .center("38%")
                    .caption("of capacity")
                    .footer("42 / 110 pods")
                    .ring([
                        DonutSegment::new("Running", Some(40.0), DonutColor::Tone(Tone::Success)),
                        DonutSegment::new("Pending", Some(2.0), DonutColor::Tone(Tone::Warning)),
                    ])
                    .format(|v| format!("{v:.0}").into()),
            ),
        )
        .child(
            StatCard::new("Memory", "—")
                .caption("usage unavailable")
                .child(
                    UsageBar::new(32.0 * GIB)
                        .requests(28.0 * GIB)
                        .limits(30.0 * GIB)
                        .format(gibibytes),
                ),
        )
}

/// Usage history as the details show it: one line, two lines with a gap
/// (missed scrapes), and no data.
fn line_charts() -> impl IntoElement {
    let series = |count: usize, f: &dyn Fn(f64) -> f64| -> Vec<(f64, f64)> {
        (0..count)
            .map(|ix| {
                let x = ix as f64 * 30.0;
                (x, f(ix as f64))
            })
            .collect()
    };
    let cpu = series(120, &|i| {
        0.18 + 0.08 * (i / 9.0).sin() + 0.03 * (i / 2.3).cos()
    });
    let mut receive = series(120, &|i| 40_000.0 + 25_000.0 * (i / 14.0).sin().abs());
    receive.drain(70..80);
    let mut transmit = series(120, &|i| 12_000.0 + 6_000.0 * (i / 6.0).cos().abs());
    transmit.drain(70..80);
    let cores = |v: f64| SharedString::from(format!("{:.0}m", v * 1000.0));
    let rate = |v: f64| SharedString::from(format!("{:.1}Ki/s", v / 1024.0));
    const MIB: f64 = 1024.0 * 1024.0;
    // Real times, so the readout shows a sensible clock.
    let now = kubach_ui::unix_now() as f64;
    let memory_app: Vec<(f64, f64)> = series(120, &|i| (120.0 + 30.0 * (i / 20.0).sin()) * MIB)
        .into_iter()
        .map(|(x, y)| (now - 3570.0 + x, y))
        .collect();
    let mut memory_proxy: Vec<(f64, f64)> = series(120, &|i| (40.0 + 0.2 * i) * MIB)
        .into_iter()
        .map(|(x, y)| (now - 3570.0 + x, y))
        .collect();
    // Started later: no value in the readout before that.
    memory_proxy.drain(..30);
    let mebibytes = |v: f64| SharedString::from(format!("{:.0}Mi", v / MIB));
    Panel::new().title("Usage history").child(
        div()
            .w(px(380.))
            .flex()
            .flex_col()
            .gap_4()
            .child(
                LineChart::new("chart-cpu", "CPU")
                    .line(ChartLine::new("CPU", cpu).slot(0))
                    .x_range(0.0, 3570.0)
                    .x_labels("-1h", "now")
                    .format(cores),
            )
            .child(
                LineChart::new("chart-network", "Network")
                    .line(ChartLine::new("Receive", receive).slot(2))
                    .line(ChartLine::new("Transmit", transmit).slot(3))
                    .x_range(0.0, 3570.0)
                    .x_labels("-1h", "now")
                    .format(rate),
            )
            .child(
                // Hover: the line snaps to the nearest sample, the readout
                // shows the time and every value, request and limit (#376).
                LineChart::new("chart-memory-split", "Memory by container")
                    .line(ChartLine::new("app", memory_app).slot(0))
                    .line(ChartLine::new("istio-proxy", memory_proxy).slot(1))
                    .threshold("Request", 160.0 * MIB, Tone::Info)
                    .threshold("Limit", 256.0 * MIB, Tone::Warning)
                    .x_range(now - 3570.0, now)
                    .x_labels("-1h", "now")
                    .format(mebibytes)
                    .binary(),
            )
            .child(LineChart::new("chart-empty", "Memory").empty("No data for this range")),
    )
}

/// "Who can get pods": subject → binding → role → rule.
fn demo_graph() -> (Vec<FlowNode>, Vec<(usize, usize)>) {
    let nodes = vec![
        FlowNode::new(0, 0, "system:masters")
            .subtitle("Group")
            .tone(Tone::Warning),
        FlowNode::new(0, 1, "alice")
            .subtitle("User")
            .tone(Tone::Info),
        FlowNode::new(0, 2, "web/ci")
            .subtitle("ServiceAccount")
            .tone(Tone::Success)
            .clickable(true),
        FlowNode::new(1, 0, "cluster-admin")
            .subtitle("ClusterRoleBinding")
            .clickable(true),
        FlowNode::new(1, 1, "web/read-pods")
            .subtitle("RoleBinding")
            .clickable(true),
        FlowNode::new(2, 0, "cluster-admin")
            .subtitle("ClusterRole")
            .clickable(true),
        FlowNode::new(2, 1, "web/pod-reader")
            .subtitle("Role")
            .clickable(true),
        FlowNode::new(3, 0, "* on *").subtitle("groups: *"),
        FlowNode::new(3, 1, "get, list on pods").subtitle("groups: core"),
    ];
    let edges = vec![(0, 3), (1, 4), (2, 4), (3, 5), (4, 6), (5, 7), (6, 8)];
    (nodes, edges)
}

/// Nodes reachable from `ix` to the right and those reaching it from the left.
fn demo_chain(edges: &[(usize, usize)], ix: usize) -> Vec<usize> {
    let mut found = vec![ix];
    for forward in [true, false] {
        let mut stack = vec![ix];
        while let Some(current) = stack.pop() {
            for &(from, to) in edges {
                let (here, next) = if forward { (from, to) } else { (to, from) };
                if here == current && !found.contains(&next) {
                    found.push(next);
                    stack.push(next);
                }
            }
        }
    }
    found
}

impl Gallery {
    /// Virtualized events timeline with a button for a thousand events (#368).
    fn render_timeline_demo(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        Self::section("Events timeline (virtualized)", cx).child(
            Panel::new()
                .title(format!("Events · {}", self.timeline.len()))
                .child(
                    div().flex().child(
                        Button::new("gallery-timeline-fill")
                            .xsmall()
                            .outline()
                            .label(if self.timeline.len() > TIMELINE_EVENTS {
                                "20 events"
                            } else {
                                "1000 events"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                let count = if this.timeline.len() > TIMELINE_EVENTS {
                                    TIMELINE_EVENTS
                                } else {
                                    1000
                                };
                                this.timeline.set_entries(demo_events(count), true);
                                cx.notify();
                            })),
                    ),
                )
                .child(
                    div()
                        .mt_2()
                        .h(px(360.))
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .overflow_hidden()
                        .child(self.timeline.render()),
                ),
        )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(kubach_ui::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            kubach_ui::theme::init(ThemeMode::Dark, cx);
            kubach_ui::init(cx);
            kubach_ui::terminal::init(cx);
            set_language_from_env(cx);

            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("ctrl-q", Quit, None),
            ]);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let options = kubach_ui::window::main_window_options("Kubach UI", cx);
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Gallery::new(window, cx))
            })
            .expect("failed to open gallery window");
            cx.activate(true);
            kubach_ui::a11y_check::start(cx);
        });
}

/// Built-in text of the components in another language:
/// `KUBACH_UI_LANG=ru` or `zh-Hans` (the gallery's own text stays English).
fn set_language_from_env(cx: &mut App) {
    let locale = match std::env::var("KUBACH_UI_LANG").as_deref() {
        Ok("ru") => kubach_i18n::Locale::Ru,
        Ok("zh-Hans" | "zh-hans" | "zh") => kubach_i18n::Locale::ZhHans,
        _ => return,
    };
    kubach_i18n::set_locale(locale);
    if locale == kubach_i18n::Locale::ZhHans {
        kubach_ui::theme::set_text_fallbacks(&kubach_ui::theme::SIMPLIFIED_CHINESE_FALLBACKS, cx);
    }
}

/// Demo Kubernetes states for the gallery: the design system itself knows
/// only labels and [`Tone`]s, the app maps real statuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DemoStatus {
    Running,
    Ready,
    Succeeded,
    Pending,
    ContainerCreating,
    Terminating,
    Failed,
    CrashLoopBackOff,
    ImagePullBackOff,
    Unknown,
}

impl DemoStatus {
    const ALL: [DemoStatus; 10] = [
        DemoStatus::Running,
        DemoStatus::Ready,
        DemoStatus::Succeeded,
        DemoStatus::Pending,
        DemoStatus::ContainerCreating,
        DemoStatus::Terminating,
        DemoStatus::Failed,
        DemoStatus::CrashLoopBackOff,
        DemoStatus::ImagePullBackOff,
        DemoStatus::Unknown,
    ];

    fn label(self) -> &'static str {
        match self {
            DemoStatus::Running => "Running",
            DemoStatus::Ready => "Ready",
            DemoStatus::Succeeded => "Succeeded",
            DemoStatus::Pending => "Pending",
            DemoStatus::ContainerCreating => "ContainerCreating",
            DemoStatus::Terminating => "Terminating",
            DemoStatus::Failed => "Failed",
            DemoStatus::CrashLoopBackOff => "CrashLoopBackOff",
            DemoStatus::ImagePullBackOff => "ImagePullBackOff",
            DemoStatus::Unknown => "Unknown",
        }
    }

    fn tone(self) -> Tone {
        match self {
            DemoStatus::Running | DemoStatus::Ready => Tone::Success,
            DemoStatus::Succeeded => Tone::Info,
            DemoStatus::Pending | DemoStatus::ContainerCreating | DemoStatus::Terminating => {
                Tone::Warning
            }
            DemoStatus::Failed | DemoStatus::CrashLoopBackOff | DemoStatus::ImagePullBackOff => {
                Tone::Danger
            }
            DemoStatus::Unknown => Tone::Neutral,
        }
    }

    fn badge(self) -> StatusBadge {
        StatusBadge::new(self.label(), self.tone())
    }
}

/// Graph canvas demo: a small service map or a generated large one, with
/// search and the last event shown below.
struct GraphDemo {
    view: Entity<GraphView>,
    search: Entity<gpui_kit::component::input::InputState>,
    large: bool,
    event: Option<String>,
    matches: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

impl GraphDemo {
    fn new(window: &mut Window, cx: &mut Context<Gallery>) -> Self {
        let view = cx.new(|cx| {
            let mut view = GraphView::new(cx);
            view.set_data(small_graph(), cx);
            // The gallery page scrolls past the graph until it is clicked.
            view.set_wheel_needs_focus(true, cx);
            view
        });
        let search = FilterBar::search_state("Find a node…", window, cx);
        let subscriptions = vec![
            cx.subscribe(&view, |this, _, event: &GraphViewEvent, cx| {
                let target = |t: &GraphTarget| match t {
                    GraphTarget::Node(id) => format!("node {id}"),
                    GraphTarget::Group(id) => format!("group {id}"),
                };
                let text = match event {
                    GraphViewEvent::Hovered(_) => return,
                    GraphViewEvent::Selected(Some(t)) => format!("Selected {}", target(t)),
                    GraphViewEvent::Selected(None) => "Selection cleared".into(),
                    GraphViewEvent::Activated(t) => format!("Opened {}", target(t)),
                    GraphViewEvent::GroupToggled { group, collapsed } => {
                        let state = if *collapsed { "collapsed" } else { "expanded" };
                        format!("Group {group} {state}")
                    }
                };
                this.graph_demo.event = Some(text);
                cx.notify();
            }),
            cx.subscribe(
                &search,
                |this, search, event: &gpui_kit::component::input::InputEvent, cx| {
                    use gpui_kit::component::input::InputEvent;
                    let view = this.graph_demo.view.clone();
                    match event {
                        InputEvent::Change => {
                            let query = search.read(cx).value().to_string();
                            let count = view.update(cx, |view, cx| view.search(&query, cx));
                            this.graph_demo.matches = (!query.trim().is_empty()).then_some(count);
                        }
                        InputEvent::PressEnter { .. } => {
                            view.update(cx, |view, cx| view.search_next(cx));
                        }
                        _ => return,
                    }
                    cx.notify();
                },
            ),
        ];
        Self {
            view,
            search,
            large: false,
            event: None,
            matches: None,
            _subscriptions: subscriptions,
        }
    }

    fn render(&self, cx: &mut Context<Gallery>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let laying_out = self.view.read(cx).is_laying_out();
        let mut status = vec![
            "Drag or scroll to pan, ⌘-scroll or pinch to zoom, click a node to trace its chain (again or Esc to clear), double click a namespace title to fold it. Shortened edge labels show in full on hover."
                .to_string(),
        ];
        if let Some(count) = self.matches {
            status.push(format!("{count} matches (Enter for next)"));
        }
        if let Some(event) = &self.event {
            status.push(event.clone());
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("graph-small")
                            .small()
                            .label("Service map")
                            .selected(!self.large)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.graph_demo.large = false;
                                this.graph_demo
                                    .view
                                    .update(cx, |view, cx| view.set_data(small_graph(), cx));
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("graph-large")
                            .small()
                            .label(format!("Large graph ({LARGE_GRAPH_NODES} nodes)"))
                            .selected(self.large)
                            .loading(laying_out)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.graph_demo.large = true;
                                this.graph_demo.view.update(cx, |view, cx| {
                                    view.set_data(large_graph(LARGE_GRAPH_NODES), cx)
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        div().w(px(240.)).child(
                            gpui_kit::component::input::Input::new(&self.search)
                                .small()
                                .cleanable(true),
                        ),
                    ),
            )
            .child(
                div()
                    .h(px(520.))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(self.view.clone()),
            )
            .child(
                TrafficLegend::new()
                    .source("Traffic: Istio · 5m")
                    .unmapped(true),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(status.join("  ·  ")),
            )
    }
}

const LARGE_GRAPH_NODES: usize = 1500;

/// Entry → services → workloads → dependencies over two namespaces.
fn small_graph() -> GraphData {
    let node = |id: &str, title: &str, kind: &str, layer: usize, icon: Lucide| {
        GraphNode::new(id, title)
            .subtitle(kind)
            .layer(layer)
            .icon(icon)
            .tone(Tone::Success)
    };
    let nodes = vec![
        node("host", "shop.example.com", "Host", 0, Lucide::Globe).tone(Tone::Neutral),
        node("ing", "shop", "Ingress", 0, Lucide::Waypoints).group("shop"),
        node("svc-web", "web", "Service", 1, Lucide::Network).group("shop"),
        node("svc-api", "api", "Service", 1, Lucide::Network).group("shop"),
        node("dep-web", "web", "Deployment", 2, Lucide::Cpu)
            .group("shop")
            .badge("3/3", Tone::Success),
        node("dep-api", "api", "Deployment", 2, Lucide::Cpu)
            .group("shop")
            .tone(Tone::Warning)
            .badge("2/3", Tone::Warning),
        node("cm-web", "web-config", "ConfigMap", 3, Lucide::FileText).group("shop"),
        node("sec-api", "api-token", "Secret", 3, Lucide::EyeOff).group("shop"),
        node("svc-pg", "postgres", "Service", 1, Lucide::Network).group("data"),
        node("svc-redis", "redis", "Service", 1, Lucide::Network).group("data"),
        node("sts-pg", "postgres", "StatefulSet", 2, Lucide::Frame)
            .group("data")
            .badge("1/1", Tone::Success),
        node("dep-redis", "redis", "Deployment", 2, Lucide::Cpu)
            .group("data")
            .tone(Tone::Danger)
            .badge("CrashLoop", Tone::Danger),
        node("pvc-pg", "pg-data", "PVC", 3, Lucide::HardDrive).group("data"),
        node("ext", "api.stripe.com", "External", 3, Lucide::ExternalLink).tone(Tone::Neutral),
    ];
    let edge = |from: &str, to: &str| GraphEdge::new(format!("{from}>{to}"), from, to);
    let edges = vec![
        edge("host", "ing").label("https"),
        edge("ing", "svc-web").label("/ :80"),
        edge("ing", "svc-api").label("/api :8080"),
        edge("svc-web", "dep-web"),
        edge("svc-api", "dep-api"),
        edge("svc-pg", "sts-pg"),
        edge("svc-redis", "dep-redis"),
        edge("dep-web", "cm-web").style(EdgeStyle::Dotted),
        edge("dep-api", "sec-api").style(EdgeStyle::Dotted),
        edge("sts-pg", "pvc-pg").style(EdgeStyle::Dotted),
        // Measured traffic (#326): thickness by load, color by level.
        edge("dep-web", "svc-api")
            .label("42 req/s · p95 31 ms")
            .weight(0.55)
            .traffic(TrafficLevel::Medium),
        edge("dep-api", "svc-pg")
            .label("120 req/s · p95 8 ms")
            .weight(0.95)
            .traffic(TrafficLevel::High),
        edge("dep-api", "svc-redis")
            .label("9 req/s · 12% err")
            .weight(0.3)
            .traffic(TrafficLevel::Failing),
        edge("dep-web", "dep-redis")
            .style(EdgeStyle::LongDash)
            .label("0.4 req/s · no Service")
            .weight(0.1)
            .traffic(TrafficLevel::Low),
        edge("dep-api", "ext")
            .style(EdgeStyle::Dashed)
            .label("egress")
            .tone(Tone::Warning),
    ];
    GraphData {
        nodes,
        edges,
        groups: vec![
            GraphGroup::new("shop", "shop"),
            GraphGroup::new("data", "data"),
        ],
    }
}

/// A generated cluster: namespaces of services, workloads and their
/// config, with calls between workloads and services.
fn large_graph(target: usize) -> GraphData {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let tones = [
        Tone::Success,
        Tone::Success,
        Tone::Success,
        Tone::Warning,
        Tone::Danger,
    ];
    let mut data = GraphData::default();
    let namespaces = 24;
    let per_namespace = target / namespaces;
    let mut services: Vec<String> = Vec::new();
    let edge = |from: &String, to: &String| {
        GraphEdge::new(format!("{from}>{to}"), from.clone(), to.clone())
    };
    for ns in 0..namespaces {
        let group = format!("team-{ns:02}");
        data.groups
            .push(GraphGroup::new(group.clone(), group.clone()).collapsed(ns >= 20));
        let ingress = format!("{group}/ing");
        data.nodes.push(
            GraphNode::new(ingress.clone(), format!("{group}-ingress"))
                .subtitle("Ingress")
                .icon(Lucide::Waypoints)
                .layer(0)
                .group(group.clone()),
        );
        for app in 0..(per_namespace - 1) / 4 {
            let name = format!("app-{app:02}");
            let id = |kind: &str| format!("{group}/{kind}/{name}");
            let (svc, dep, cm, sec) = (id("svc"), id("dep"), id("cm"), id("sec"));
            let kinds = [
                (&svc, name.clone(), "Service", Lucide::Network, 1),
                (&dep, name.clone(), "Deployment", Lucide::Cpu, 2),
                (
                    &cm,
                    format!("{name}-config"),
                    "ConfigMap",
                    Lucide::FileText,
                    3,
                ),
                (&sec, format!("{name}-token"), "Secret", Lucide::EyeOff, 3),
            ];
            let tone = tones[next(tones.len())];
            for (id, title, kind, icon, layer) in kinds {
                let node = GraphNode::new(id.clone(), title)
                    .subtitle(kind)
                    .icon(icon)
                    .layer(layer)
                    .group(group.clone());
                data.nodes
                    .push(if layer == 2 { node.tone(tone) } else { node });
            }
            if app % 3 == 0 {
                data.edges.push(edge(&ingress, &svc));
            }
            data.edges.push(edge(&svc, &dep));
            data.edges.push(edge(&dep, &cm).style(EdgeStyle::Dotted));
            data.edges.push(edge(&dep, &sec).style(EdgeStyle::Dotted));
            services.push(svc);
        }
    }
    // Calls from workloads to services, mostly within the namespace.
    let deployments: Vec<String> = data
        .nodes
        .iter()
        .filter(|n| n.id.contains("/dep/"))
        .map(|n| n.id.to_string())
        .collect();
    let target_edges = data.nodes.len() * 8 / 3;
    while data.edges.len() < target_edges {
        let dep = &deployments[next(deployments.len())];
        let ns = &dep[..dep.find('/').unwrap_or(0)];
        let svc = if next(12) == 0 {
            services[next(services.len())].clone()
        } else {
            let local: Vec<&String> = services.iter().filter(|s| s.starts_with(ns)).collect();
            local[next(local.len())].clone()
        };
        if svc.replace("/svc/", "/dep/") == *dep {
            continue;
        }
        data.edges.push(
            GraphEdge::new(
                format!("{dep}>{svc}#{}", data.edges.len()),
                dep.clone(),
                svc,
            )
            .style(EdgeStyle::Dashed)
            .weight(next(100) as f32 / 100.)
            .tone(if next(12) == 0 {
                Tone::Danger
            } else {
                Tone::Neutral
            }),
        );
    }
    data
}
