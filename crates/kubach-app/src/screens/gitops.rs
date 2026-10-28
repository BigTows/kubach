//! GitOps screen: every Argo CD Application, Flux Kustomization, HelmRelease
//! and Flux source of the selected namespaces with sync and health, the
//! problems first, and the details of one: revision, errors, conditions and
//! the resources it manages (links into the browser).
//!
//! The objects are watched through the cache, so the list follows the
//! cluster. Classification is `kubach_core::gitops::overview`. The
//! namespace selection is shared with the other list screens.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, IconName, Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::ConnectionState;
use kubach_core::cache::{ResourceCache, SubscriptionSet, WatchFilter, WatchStatus};
use kubach_core::gitops::overview::{
    self, Attention, GitOpsItem, GitOpsKind, ResourceLink, Summary, short_revision,
};
use kubach_i18n::t;
use kubach_ui::{
    EmptyState, FilterBar, KeyValueList, MultiSelection, Panel, ResourceCell, ResourceColumn,
    ResourceRow, ResourceTable, SIDE_PANEL_CONTENT_MIN, SidePanel, SidePanelState, StatusBadge,
    StatusCounts, Tone, name_matches,
};

use super::access::Reveal;
use super::activity::{ScreenActivity, watch_set};
use super::namespace_picker::{NamespacePicker, Namespaces};
use super::namespaces::selected_names;
use super::panel_width;
use super::plugin_text::column_title_of;
use super::rows;
use super::table_sort;
use crate::cluster::{CatalogState, Cluster};

/// Managed resources listed in the details; the rest is counted.
const MANAGED_SHOWN: usize = 300;
/// Width of the details column next to the list.
const DETAILS_WIDTH: Pixels = px(440.);

/// The name of an attention level in the language of the interface.
pub fn attention_title(attention: Attention) -> String {
    match attention {
        Attention::Ok => t!("gitops-attention-ok"),
        Attention::Unknown => t!("gitops-attention-unknown"),
        Attention::Suspended => t!("gitops-attention-suspended"),
        Attention::Pending => t!("gitops-attention-pending"),
        Attention::Warning => t!("gitops-attention-warning"),
        Attention::Error => t!("gitops-attention-error"),
    }
}

pub fn attention_tone(attention: Attention) -> Tone {
    match attention {
        Attention::Ok => Tone::Success,
        Attention::Unknown | Attention::Suspended => Tone::Neutral,
        Attention::Pending => Tone::Info,
        Attention::Warning => Tone::Warning,
        Attention::Error => Tone::Danger,
    }
}

fn columns(show_namespace: bool) -> Vec<ResourceColumn> {
    let mut columns = vec![
        ResourceColumn::new("name", column_title_of("name", "Name"))
            .width(px(240.))
            .sortable(),
    ];
    if show_namespace {
        columns.push(
            ResourceColumn::new("namespace", column_title_of("namespace", "Namespace"))
                .width(px(140.))
                .sortable(),
        );
    }
    columns.extend([
        ResourceColumn::new("kind", column_title_of("kind", "Kind"))
            .width(px(170.))
            .sortable(),
        ResourceColumn::new("status", column_title_of("status", "Status"))
            .width(px(110.))
            .sortable(),
        ResourceColumn::new("sync", column_title_of("sync", "Sync"))
            .width(px(130.))
            .sortable(),
        ResourceColumn::new("health", column_title_of("health", "Health"))
            .width(px(120.))
            .sortable(),
        ResourceColumn::new("revision", column_title_of("revision", "Revision"))
            .width(px(170.))
            .sortable(),
        ResourceColumn::new("source", column_title_of("source", "Source"))
            .width(px(320.))
            .sortable(),
    ]);
    columns
}

fn kind_label(kind: GitOpsKind) -> String {
    format!("{} {}", kind.tool().title(), kind.kind())
}

fn row(item: &GitOpsItem, show_namespace: bool) -> ResourceRow {
    let text = |s: &str| {
        if s.is_empty() {
            ResourceCell::text("—").muted()
        } else {
            ResourceCell::text(s.to_owned())
        }
    };
    let mut cells = vec![ResourceCell::text(item.name.clone())];
    if show_namespace {
        cells.push(text(item.namespace.as_deref().unwrap_or_default()));
    }
    let health = match &item.health {
        Some(health) => ResourceCell::status(rows::status_label(health), rows::tone(health.status)),
        None => ResourceCell::text("—").muted(),
    };
    cells.extend([
        ResourceCell::text(kind_label(item.kind)),
        ResourceCell::status(
            attention_title(item.attention),
            attention_tone(item.attention),
        )
        .sort_by(item.attention as i64),
        ResourceCell::status(item.sync_label().to_owned(), rows::tone(item.sync.status)),
        health,
        text(&short_revision(&item.revision)),
        text(&item.source),
    ]);
    ResourceRow::new(item.key(), cells)
}

fn reveal_of(link: &ResourceLink) -> Reveal {
    Reveal {
        group: link.group.clone(),
        kind: link.kind.clone(),
        namespace: link.namespace.clone(),
        name: link.name.clone(),
    }
}

/// Watches of the GitOps kinds in the selected namespaces.
struct Feed {
    /// The connection's cache; a reconnect starts over.
    cache: ResourceCache,
    namespaces: MultiSelection,
    watches: Vec<(GitOpsKind, SubscriptionSet)>,
    items: Vec<GitOpsItem>,
    status: WatchStatus,
    /// Kinds that can't be listed, with the reason.
    errors: Vec<String>,
    _listeners: Vec<Task<()>>,
}

pub struct GitOpsScreen {
    cluster: Entity<Cluster>,
    namespaces: Entity<Namespaces>,
    /// On screen: watches run only then.
    active: bool,
    problems_only: bool,
    /// Created on first render, which has the window they need.
    picker: Option<Entity<NamespacePicker>>,
    search: Option<Entity<InputState>>,
    /// Text of `search`: objects whose name contains it are listed.
    query: String,
    feed: Option<Feed>,
    /// The table and whether it has a Namespace column.
    table: Option<(Entity<ResourceTable>, bool)>,
    /// Key of the object whose details are open.
    selected: Option<String>,
    /// Width of the details column, dragged by its left edge.
    details_panel: Entity<SidePanelState>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Reveal> for GitOpsScreen {}

impl GitOpsScreen {
    pub fn new(
        cluster: Entity<Cluster>,
        namespaces: Entity<Namespaces>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&cluster, |this, _, cx| this.sync(cx)),
            cx.observe(&namespaces, |this, _, cx| this.sync(cx)),
        ];
        Self {
            cluster,
            namespaces,
            active: false,
            problems_only: false,
            picker: None,
            search: None,
            query: String::new(),
            feed: None,
            table: None,
            selected: None,
            details_panel: panel_width::remembered("gitops.details", DETAILS_WIDTH, cx),
            _subscriptions: subscriptions,
        }
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let cluster = self.cluster.read(cx);
        let cache = cluster.cache().cloned();
        let catalog = match cluster.catalog() {
            CatalogState::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        };
        let scope = self.namespaces.read(cx).scope().cloned();
        let (Some(cache), Some(catalog), Some(scope), true) = (cache, catalog, scope, self.active)
        else {
            self.feed = None;
            cx.notify();
            return;
        };
        let kinds = overview::served(&catalog);
        let same = self.feed.as_ref().is_some_and(|feed| {
            feed.namespaces == scope.selection
                && feed.cache.same_as(&cache)
                && feed.watches.len() == kinds.len()
                && feed
                    .watches
                    .iter()
                    .zip(&kinds)
                    .all(|((a, _), (b, _))| a == b)
        });
        if !same {
            self.feed = Some(self.watch(&cache, kinds, scope.selection.clone(), cx));
            self.refresh(cx);
        }
        cx.notify();
    }

    fn watch(
        &self,
        cache: &ResourceCache,
        kinds: Vec<(GitOpsKind, kubach_core::ResourceKind)>,
        namespaces: MultiSelection,
        cx: &mut Context<Self>,
    ) -> Feed {
        let names = selected_names(&namespaces);
        let mut watches = Vec::new();
        let mut listeners = Vec::new();
        for (kind, served) in kinds {
            let set = cache.subscribe_many(&served, names.as_deref(), WatchFilter::default());
            listeners.push(watch_set(&set, cx, Self::refresh));
            watches.push((kind, set));
        }
        Feed {
            cache: cache.clone(),
            namespaces,
            watches,
            items: Vec::new(),
            status: WatchStatus::Loading,
            errors: Vec::new(),
            _listeners: listeners,
        }
    }

    /// Rebuilds the items from the watches' snapshots.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(feed) = &mut self.feed else {
            return;
        };
        let snapshots: Vec<_> = feed
            .watches
            .iter()
            .map(|(kind, set)| (*kind, set.snapshot()))
            .collect();
        feed.items = overview::collect(
            snapshots
                .iter()
                .flat_map(|(kind, snapshot)| snapshot.objects.iter().map(|o| (*kind, o.as_ref()))),
        );
        feed.errors = snapshots
            .iter()
            .flat_map(|(kind, snapshot)| {
                snapshot
                    .errors
                    .iter()
                    .map(move |(namespace, error)| match namespace.as_deref() {
                        Some(namespace) => t!(
                            "gitops-list-error-in",
                            kind = kind.kind(),
                            namespace = namespace,
                            error = error.message.clone()
                        ),
                        None => t!(
                            "gitops-list-error",
                            kind = kind.kind(),
                            error = error.message.clone()
                        ),
                    })
            })
            .collect();
        let loading = snapshots
            .iter()
            .any(|(_, s)| s.status == WatchStatus::Loading);
        let failing = snapshots.iter().find_map(|(_, s)| match &s.status {
            WatchStatus::Error(error) => Some(error.clone()),
            _ => None,
        });
        feed.status = if loading {
            WatchStatus::Loading
        } else if let Some(error) = failing.filter(|_| feed.items.is_empty()) {
            WatchStatus::Error(error)
        } else {
            WatchStatus::Ready
        };
        if let Some(selected) = &self.selected
            && !feed.items.iter().any(|item| &item.key() == selected)
            && feed.status != WatchStatus::Loading
        {
            self.selected = None;
        }
        self.update_rows(cx);
        cx.notify();
    }

    fn show_namespace(&self) -> bool {
        !matches!(
            self.feed.as_ref().map(|feed| &feed.namespaces),
            Some(MultiSelection::Items(items)) if items.len() == 1
        )
    }

    fn shown_items(&self) -> Vec<&GitOpsItem> {
        self.feed
            .iter()
            .flat_map(|feed| feed.items.iter())
            .filter(|item| !self.problems_only || item.is_problem())
            .filter(|item| name_matches(&item.name, &self.query))
            .collect()
    }

    fn update_rows(&mut self, cx: &mut Context<Self>) {
        let Some((table, show_namespace)) = self.table.clone() else {
            return;
        };
        let rows = self
            .shown_items()
            .into_iter()
            .map(|item| row(item, show_namespace))
            .collect();
        table.update(cx, |table, cx| table.set_rows(rows, cx));
    }

    fn ensure_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let show_namespace = self.show_namespace();
        if self
            .table
            .as_ref()
            .is_some_and(|(_, shown)| *shown == show_namespace)
        {
            return;
        }
        let screen = cx.entity().downgrade();
        let table = cx.new(|cx| {
            ResourceTable::new(columns(show_namespace), window, cx).on_select(move |row, _, cx| {
                let key = row.key.to_string();
                screen
                    .update(cx, |this, cx| {
                        this.selected = Some(key);
                        cx.notify();
                    })
                    .ok();
            })
        });
        table_sort::remember(&table, table_sort::screen("gitops"), cx);
        self.table = Some((table, show_namespace));
        self.update_rows(cx);
    }

    fn close_details(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        if let Some((table, _)) = &self.table {
            table.update(cx, |table, cx| table.clear_selection(cx));
        }
        cx.notify();
    }

    fn select_all_namespaces(&mut self, cx: &mut Context<Self>) {
        self.namespaces.update(cx, |namespaces, cx| {
            namespaces.apply(MultiSelection::All, cx);
        });
    }

    /// Creates the namespace picker and the search, which need the window.
    fn ensure_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_none() {
            let namespaces = self.namespaces.clone();
            self.picker =
                Some(cx.new(|cx| {
                    NamespacePicker::new("gitops-namespace-picker", namespaces, window, cx)
                }));
        }
        if self.search.is_none() {
            let search = FilterBar::search_state("", window, cx);
            self._subscriptions
                .extend(crate::i18n::localized_placeholder(
                    &search,
                    || t!("browser-search-placeholder"),
                    window,
                    cx,
                ));
            self._subscriptions.push(cx.subscribe(
                &search,
                |this, search, event: &InputEvent, cx| {
                    if let InputEvent::Change = event {
                        this.query = search.read(cx).value().to_string();
                        this.update_rows(cx);
                        cx.notify();
                    }
                },
            ));
            self.search = Some(search);
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

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.search {
            search.update(cx, |search, cx| search.set_value("", window, cx));
        }
        self.query.clear();
        self.update_rows(cx);
        cx.notify();
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
                CatalogState::Ready(_) => {
                    let feed = self.feed.as_ref()?;
                    if feed.watches.is_empty() {
                        EmptyState::new(IconName::Inbox, t!("gitops-no-tools"))
                            .description(t!("gitops-no-tools-description"))
                    } else if !feed.items.is_empty() {
                        return None;
                    } else {
                        match &feed.status {
                            WatchStatus::Loading => {
                                EmptyState::new(IconName::LoaderCircle, t!("screen-loading"))
                            }
                            WatchStatus::Error(error) if error.forbidden => {
                                EmptyState::new(IconName::Ban, t!("screen-no-access"))
                                    .description(error.message.clone())
                            }
                            WatchStatus::Error(error) => {
                                EmptyState::new(IconName::CircleX, t!("gitops-cant-list"))
                                    .description(t!(
                                        "browser-error-retrying",
                                        error = error.message.clone()
                                    ))
                            }
                            WatchStatus::Ready => {
                                let empty = EmptyState::new(IconName::Inbox, t!("gitops-empty"));
                                if feed.namespaces == MultiSelection::All {
                                    empty.description(t!("gitops-empty-any-namespace"))
                                } else {
                                    empty
                                        .description(t!("gitops-empty-selected-namespaces"))
                                        .action(
                                            Button::new("gitops-all-namespaces")
                                                .small()
                                                .outline()
                                                .label(t!("screen-show-all-namespaces"))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.select_all_namespaces(cx)
                                                })),
                                        )
                                }
                            }
                        }
                    }
                }
            },
        };
        Some(state.into_any_element())
    }

    fn render_summary(&self, summary: &Summary, cx: &App) -> AnyElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("gitops-objects", count = summary.total)),
            )
            .child(
                StatusCounts::new()
                    .item(summary.errors, t!("gitops-count-failing"), Tone::Danger)
                    .item(
                        summary.warnings,
                        t!("gitops-count-out-of-sync"),
                        Tone::Warning,
                    )
                    .item(summary.pending, t!("gitops-count-in-progress"), Tone::Info)
                    .item(
                        summary.suspended,
                        t!("gitops-count-suspended"),
                        Tone::Neutral,
                    )
                    .item(summary.ok, t!("gitops-count-healthy"), Tone::Success),
            )
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some((table, _)) = &self.table else {
            return div().flex_1().into_any_element();
        };
        let errors = self
            .feed
            .as_ref()
            .map(|f| f.errors.clone())
            .unwrap_or_default();
        let failed = (!errors.is_empty()).then(|| {
            div()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.warning)
                .text_sm()
                .text_color(theme.warning)
                .whitespace_normal()
                .child(t!("gitops-cant-list-kinds", errors = errors.join("; ")))
        });
        let searching = !self.query.trim().is_empty();
        let body = if self.shown_items().is_empty() && searching {
            let noun = if self.problems_only {
                t!("gitops-noun-problems")
            } else {
                t!("gitops-noun-objects")
            };
            FilterBar::no_matches(
                &noun,
                &self.query,
                cx.listener(|this, _, window, cx| this.clear_search(window, cx)),
            )
            .into_any_element()
        } else if self.shown_items().is_empty() {
            EmptyState::new(IconName::CircleCheck, t!("gitops-no-problems"))
                .description(t!("gitops-no-problems-description"))
                .into_any_element()
        } else {
            div()
                .size_full()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(table.clone())
                .into_any_element()
        };
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(failed)
            .child(div().flex_1().min_h_0().flex().child(body))
            .into_any_element()
    }

    fn render_details(&self, item: &GitOpsItem, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let this_reveal = Reveal {
            group: item.kind.group().to_owned(),
            kind: item.kind.kind().to_owned(),
            namespace: item.namespace.clone(),
            name: item.name.clone(),
        };
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
                            .text_color(theme.foreground)
                            .truncate()
                            .child(item.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(match &item.namespace {
                                Some(namespace) => {
                                    format!("{} · {namespace}", kind_label(item.kind))
                                }
                                None => t!("gitops-cluster-scoped", kind = kind_label(item.kind)),
                            }),
                    ),
            )
            .child(
                Button::new("gitops-open")
                    .small()
                    .outline()
                    .icon(IconName::ExternalLink)
                    .tooltip(t!("screen-open-in-browser"))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(this_reveal.clone()))),
            )
            .child(
                Button::new("gitops-close")
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .tooltip(t!("screen-close"))
                    .on_click(cx.listener(|this, _, _, cx| this.close_details(cx))),
            );

        let mut badges = vec![StatusBadge::new(
            attention_title(item.attention),
            attention_tone(item.attention),
        )];
        let sync = item.sync_label().to_owned();
        badges.push(StatusBadge::new(
            if item.kind.tool() == overview::Tool::ArgoCd {
                t!("gitops-badge-sync", state = sync)
            } else {
                t!("gitops-badge-ready", state = sync)
            },
            rows::tone(item.sync.status),
        ));
        if let Some(health) = &item.health {
            badges.push(StatusBadge::new(
                t!("gitops-badge-health", state = rows::status_label(health)),
                rows::tone(health.status),
            ));
        }

        let errors =
            (!item.errors.is_empty()).then(|| {
                Panel::new().title(t!("gitops-errors")).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(item.errors.iter().map(|error| {
                            div()
                                .text_sm()
                                .text_color(danger)
                                .whitespace_normal()
                                .child(error.clone())
                        })),
                )
            });

        let or_dash = |s: &str| {
            if s.is_empty() {
                "—".to_owned()
            } else {
                s.to_owned()
            }
        };
        let mut overview_list = KeyValueList::new().row(
            t!("gitops-revision"),
            div()
                .whitespace_normal()
                .font_family(theme.mono_font_family.clone())
                .child(or_dash(&item.revision)),
        );
        if let Some(attempted) = &item.attempted_revision {
            overview_list = overview_list.row(
                t!("gitops-attempted-revision"),
                div()
                    .whitespace_normal()
                    .font_family(theme.mono_font_family.clone())
                    .child(attempted.clone()),
            );
        }
        let target_title = match item.kind {
            GitOpsKind::ArgoApplication => t!("gitops-destination"),
            k if k.is_source() => t!("gitops-ref"),
            _ => t!("gitops-target-namespace"),
        };
        overview_list = overview_list
            .row(
                if item.kind.is_source() {
                    t!("gitops-url")
                } else {
                    t!("gitops-source")
                },
                div().whitespace_normal().child(or_dash(&item.source)),
            )
            .row(
                target_title,
                div().whitespace_normal().child(or_dash(&item.target)),
            );
        if let Some(link) = &item.source_ref {
            let reveal = reveal_of(link);
            overview_list = overview_list.row(
                t!("gitops-source-object"),
                Button::new("gitops-source-ref")
                    .link()
                    .small()
                    .label(link.label())
                    .tooltip(t!("screen-open-in-browser"))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(reveal.clone()))),
            );
        }
        overview_list = overview_list
            .row(
                t!("gitops-status-changed"),
                div().child(or_dash(item.updated.as_deref().unwrap_or_default())),
            )
            .row(
                t!("gitops-suspended"),
                div().child(if item.suspended {
                    t!("screen-yes")
                } else {
                    t!("screen-no")
                }),
            );

        let conditions = (!item.conditions.is_empty()).then(|| {
            Panel::new()
                .title(t!("section-conditions"))
                .child(item.conditions.iter().fold(KeyValueList::new(), |list, c| {
                    let detail = [c.reason.as_deref(), c.message.as_deref()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(": ");
                    let value = if detail.is_empty() {
                        c.status.clone()
                    } else {
                        format!("{} · {detail}", c.status)
                    };
                    list.row(c.type_.clone(), div().whitespace_normal().child(value))
                }))
        });

        let managed = if item.managed.is_empty() {
            None
        } else {
            let rest = item.managed.len().saturating_sub(MANAGED_SHOWN);
            let rows =
                item.managed
                    .iter()
                    .take(MANAGED_SHOWN)
                    .enumerate()
                    .map(|(ix, link)| {
                        let reveal = reveal_of(link);
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div().min_w_0().overflow_hidden().child(
                                    Button::new(("gitops-managed", ix))
                                        .link()
                                        .small()
                                        .label(link.label())
                                        .tooltip(t!("screen-open-in-browser"))
                                        .on_click(
                                            cx.listener(move |_, _, _, cx| cx.emit(reveal.clone())),
                                        ),
                                ),
                            )
                            .children(link.status.clone().map(|status| {
                                StatusBadge::new(status, attention_tone(link.attention))
                            }))
                    });
            Some(
                Panel::new()
                    .title(t!("gitops-managed", count = item.managed.len()))
                    .child(div().flex().flex_col().gap_1().children(rows).children(
                        (rest > 0).then(|| {
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(t!("ui-and-more", count = rest))
                        }),
                    )),
            )
        };

        let details = div()
            .id("gitops-details")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(div().flex().flex_wrap().gap_2().children(badges))
            .children(errors)
            .child(
                Panel::new()
                    .title(t!("gitops-overview"))
                    .child(overview_list),
            )
            .children(managed)
            .children(conditions);
        SidePanel::new("gitops-details-panel", &self.details_panel)
            .child(details)
            .into_any_element()
    }
}

impl ScreenActivity for GitOpsScreen {
    /// Shown, the screen starts its watches; hidden, it stops them.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            self.sync(cx);
        }
    }
}

impl Render for GitOpsScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_filters(window, cx);
        self.ensure_table(window, cx);
        let problems_only = self.problems_only;
        let mut header = FilterBar::new().action(
            Button::new("gitops-problems-only")
                .small()
                .outline()
                .icon(IconName::TriangleAlert)
                .label(t!("gitops-problems-only"))
                .selected(problems_only)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.problems_only = !this.problems_only;
                    this.update_rows(cx);
                    cx.notify();
                })),
        );
        if let Some(picker) = &self.picker {
            header = header.filter(picker.clone());
        }
        if let Some(search) = &self.search {
            header = header.search(search);
        }
        let root = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_4()
            .p_8()
            .child(header);
        if let Some(placeholder) = self.render_placeholder(cx) {
            return root.child(div().flex_1().child(placeholder));
        }
        let Some(feed) = &self.feed else {
            return root;
        };
        let summary = overview::summarize(&feed.items);
        let selected = self
            .selected
            .as_ref()
            .and_then(|key| feed.items.iter().find(|item| &item.key() == key))
            .cloned();
        let summary = self.render_summary(&summary, cx);
        let list = self.render_list(cx);
        let details = selected.map(|item| self.render_details(&item, cx));
        let list = div()
            .flex_1()
            .min_w_0()
            .when(details.is_some(), |this| this.min_w(SIDE_PANEL_CONTENT_MIN))
            .min_h_0()
            .flex()
            .child(list);
        root.child(summary).child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap_4()
                .child(list)
                .children(details),
        )
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::gitops::overview::{Attention, GitOpsItem, GitOpsKind};
    use kube::api::DynamicObject;
    use serde_json::json;

    use super::{attention_tone, columns, row};
    use kubach_ui::Tone;

    #[test]
    fn rows_match_columns() {
        let object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "Application",
            "metadata": {"name": "guestbook", "namespace": "argocd"},
            "spec": {"source": {"repoURL": "https://example.com/repo.git", "path": "app"}},
            "status": {
                "sync": {"status": "OutOfSync", "revision": "53e28ff20cc530b9ada2173fbbd64d48338583ba"},
                "health": {"status": "Healthy"}
            }
        }))
        .unwrap();
        let item = GitOpsItem::from_object(GitOpsKind::ArgoApplication, &object);
        for show_namespace in [true, false] {
            let row = row(&item, show_namespace);
            assert_eq!(row.cells.len(), columns(show_namespace).len());
        }
        let row = row(&item, true);
        let labels: Vec<_> = row.cells.iter().map(|c| c.label().to_string()).collect();
        assert_eq!(
            labels,
            [
                "guestbook",
                "argocd",
                "Argo CD Application",
                "Warning",
                "OutOfSync",
                "Healthy",
                "53e28ff",
                "https://example.com/repo.git · app · HEAD",
            ]
        );
        assert_eq!(row.key.as_ref(), "argoproj.io/Application/argocd/guestbook");
        assert_eq!(attention_tone(Attention::Error), Tone::Danger);
        assert_eq!(attention_tone(Attention::Pending), Tone::Info);
    }
}
