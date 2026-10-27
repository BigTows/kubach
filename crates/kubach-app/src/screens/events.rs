//! Event timelines: the "Events" panel in the details of a resource and the
//! Events screen of the selected namespaces. Warnings are highlighted.
//!
//! Both follow the cluster through watches. On the Events screen, rows do
//! not move while the user is scrolled into the list: they update in place
//! and new events wait behind a "new events" button (`merge_live`). The
//! screen's timeline is virtualized ([`EventTimeline`]): only the rows in
//! view are laid out.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, IconName, Selectable as _, Sizable as _};
use gpui_kit::*;
use kubach_core::cache::{ResourceCache, SubscriptionSet, WatchFilter, WatchStatus};
use kubach_core::events::{
    self, EventFeed, EventFilter, EventGroup, EventIndex, ObjectKey, TimelineView, merge_live,
};
use kubach_core::{ApiCatalog, ConnectionState, ResourceKind};
use kubach_i18n::t;
use kubach_ui::{
    AgeTicker, EmptyState, EventEntry, EventList, EventTimeline, FilterBar, MultiSelection, Panel,
    name_matches,
};
use kube::api::DynamicObject;

use super::activity::{ScreenActivity, watch_changes, watch_set};
use super::namespace_picker::{NamespacePicker, Namespaces};
use super::namespaces::selected_names;
use crate::cluster::{CatalogState, Cluster};

/// Groups shown in the details panel.
const OBJECT_EVENTS_MAX: usize = 50;
/// Groups shown on the Events screen.
const SCREEN_EVENTS_MAX: usize = 5000;
/// Scrolled further than this from the top, the timeline is held.
const HOLD_OFFSET: Pixels = px(8.);

/// A timeline row: reason, message, age and, when asked, the object.
fn entry(group: &EventGroup, object: bool, namespace: bool) -> EventEntry {
    let event = &group.latest;
    let mut entry = EventEntry::new(event.reason.clone(), event.message.clone())
        .warning(event.is_warning())
        .count(group.count)
        .key(group.key_hash());
    if let Some(time) = group.last_time {
        entry = entry.since(time.as_second());
    }
    if object {
        let regarding = &event.regarding;
        let mut label = format!("{}/{}", regarding.kind, regarding.name);
        if namespace && let Some(ns) = regarding.namespace.as_deref().filter(|ns| !ns.is_empty()) {
            label = format!("{ns} · {label}");
        }
        entry = entry.object(label);
    }
    entry
}

/// Background recomputation of a timeline: one at a time, and changes that
/// come in meanwhile are folded into a single rerun with the latest objects
/// (the intermediate states are never computed).
#[derive(Default)]
struct Recompute {
    running: bool,
    /// Something changed while running: run again when done.
    stale: bool,
    /// The job; dropping it (with its owner) cancels it.
    task: Option<Task<()>>,
}

impl Recompute {
    /// Whether a job may start now; if one is running, it reruns when done.
    fn begin(&mut self) -> bool {
        if self.running {
            self.stale = true;
            return false;
        }
        self.running = true;
        true
    }

    /// The job is done; returns whether to run again.
    fn finish(&mut self) -> bool {
        self.running = false;
        std::mem::take(&mut self.stale)
    }
}

/// Time of the newest shown event, which sets how often ages tick.
fn youngest(groups: &[EventGroup]) -> Option<i64> {
    groups
        .iter()
        .filter_map(|group| group.last_time)
        .map(|time| time.as_second())
        .max()
}

fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// Events of one object, shown in its details.
pub struct ObjectEvents {
    /// Identity of the object, to keep the panel across updates.
    id: String,
    /// `None` when the cluster serves no Event API.
    feed: Option<EventFeed>,
    filter: EventFilter,
    groups: Vec<EventGroup>,
    status: WatchStatus,
    /// Keeps the ages ("5m") current.
    ages: AgeTicker,
    /// Decoding and grouping run in the background.
    recompute: Recompute,
    _listener: Option<Task<()>>,
}

impl ObjectEvents {
    pub fn new(
        cache: &ResourceCache,
        catalog: &ApiCatalog,
        kind: &ResourceKind,
        object: &DynamicObject,
        cx: &mut Context<Self>,
    ) -> Self {
        let feed = EventFeed::subscribe(cache, catalog, object.metadata.namespace.as_deref()).ok();
        let listener = feed
            .as_ref()
            .map(|feed| watch_changes(feed.changes(), cx, Self::refresh));
        let status = match feed {
            Some(_) => WatchStatus::Loading,
            None => WatchStatus::Error(kubach_core::cache::WatchError {
                forbidden: false,
                connection: false,
                message: t!("events-not-served-error"),
            }),
        };
        Self {
            id: Self::id_of(kind, object),
            feed,
            filter: EventFilter::object(ObjectKey::of_kind(object, kind)),
            groups: Vec::new(),
            status,
            ages: AgeTicker::default(),
            recompute: Recompute::default(),
            _listener: listener,
        }
    }

    pub fn id_of(kind: &ResourceKind, object: &DynamicObject) -> String {
        format!(
            "{}/{}/{}/{}/{}",
            kind.api.group,
            kind.api.kind,
            object.metadata.namespace.as_deref().unwrap_or_default(),
            object.metadata.name.as_deref().unwrap_or_default(),
            // A recreated object (same name, new uid) gets a new filter.
            object.metadata.uid.as_deref().unwrap_or_default()
        )
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(feed) = self.feed.clone() else {
            return;
        };
        if !self.recompute.begin() {
            return;
        }
        let filter = self.filter.clone();
        self.recompute.task = Some(cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_spawn(async move { feed.recent_groups(&filter, OBJECT_EVENTS_MAX) })
                .await;
            this.update(cx, |this, cx| {
                this.groups = snapshot.groups;
                this.status = snapshot.status;
                this.ages.track(youngest(&this.groups), cx);
                if this.recompute.finish() {
                    this.refresh(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }
}

impl Render for ObjectEvents {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let warnings = self.groups.iter().filter(|g| g.latest.is_warning()).count();
        let title = match warnings {
            0 => t!("events-title"),
            n => t!("events-title-warnings", count = n),
        };
        let body = if !self.groups.is_empty() {
            EventList::new(self.groups.iter().map(|g| entry(g, false, false))).into_any_element()
        } else {
            let text = match &self.status {
                WatchStatus::Loading => t!("events-loading"),
                WatchStatus::Error(_) => t!("events-unavailable"),
                WatchStatus::Ready => t!("events-none"),
            };
            note(text, cx).into_any_element()
        };
        Panel::new().title(title).child(body)
    }
}

/// Live events of the selected namespaces.
struct NamespaceEvents {
    /// The connection's cache; a reconnect starts over.
    cache: ResourceCache,
    namespaces: MultiSelection,
    kind: ResourceKind,
    watches: SubscriptionSet,
    /// Decoded events, updated incrementally in the background; `None`
    /// while an update runs.
    index: Option<EventIndex>,
    /// The timeline as of the last finished update.
    view: TimelineView,
    recompute: Recompute,
    /// Groups on screen, newest first.
    shown: Vec<EventGroup>,
    /// New groups held back while the user is scrolled into the list.
    pending: usize,
    status: WatchStatus,
    _listener: Task<()>,
}

/// The Events screen: a namespace timeline with a warnings-only filter and
/// a search by the name of the object an event is about. The namespace
/// selection is shared with the other list screens.
pub struct EventsScreen {
    cluster: Entity<Cluster>,
    namespaces: Entity<Namespaces>,
    /// On screen: watches run only then.
    active: bool,
    warnings_only: bool,
    /// Created on first render, which has the window they need.
    picker: Option<Entity<NamespacePicker>>,
    search: Option<Entity<InputState>>,
    /// Text of `search`, applied to the timeline.
    query: String,
    feed: Option<NamespaceEvents>,
    /// Rows of `feed.shown`, drawn virtualized.
    timeline: EventTimeline,
    _subscriptions: Vec<Subscription>,
    /// Keeps the ages ("5m") of the shown events current.
    ages: AgeTicker,
}

impl EventsScreen {
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
            warnings_only: false,
            picker: None,
            search: None,
            query: String::new(),
            feed: None,
            timeline: EventTimeline::new(),
            _subscriptions: subscriptions,
            ages: AgeTicker::default(),
        }
    }

    /// The user scrolled into the timeline: keep rows where they are.
    fn held(&self) -> bool {
        self.timeline.is_scrolled(HOLD_OFFSET)
    }

    /// Recomputes the groups on screen from the feed's events; `hold` keeps
    /// shown rows in place (see `merge_live`).
    fn apply(&mut self, hold: bool) {
        let filter = EventFilter {
            warnings_only: self.warnings_only,
            ..EventFilter::default()
        };
        let Some(feed) = &mut self.feed else {
            return;
        };
        let mut fresh = feed.view.recent_groups(&filter, SCREEN_EVENTS_MAX);
        fresh.retain(|group| name_matches(&group.latest.regarding.name, &self.query));
        let live = merge_live(&feed.shown, fresh, hold);
        feed.shown = live.groups;
        feed.pending = live.pending;
        let single = matches!(&feed.namespaces, MultiSelection::Items(items) if items.len() == 1);
        let entries = feed.shown.iter().map(|g| entry(g, true, !single)).collect();
        // Held, the rows on screen stay put; otherwise the newest is on top.
        self.timeline.set_entries(entries, !hold);
    }

    /// Shows the held-back events and scrolls to them.
    fn release(&mut self, cx: &mut Context<Self>) {
        self.apply(false);
        cx.notify();
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
            self.timeline.set_entries(Vec::new(), true);
            self.ages.stop();
            cx.notify();
            return;
        };
        let kind = events::events_kind(&catalog).cloned();
        let same = self.feed.as_ref().is_some_and(|feed| {
            feed.namespaces == scope.selection
                && kind.as_ref().is_some_and(|k| k.api == feed.kind.api)
                && feed.cache.same_as(&cache)
        });
        if !same {
            // Also covers a reconnect, whose new cache needs new watches.
            self.feed = kind.map(|kind| self.watch(&cache, kind, scope.selection.clone(), cx));
            self.timeline.set_entries(Vec::new(), true);
        }
        cx.notify();
    }

    fn watch(
        &self,
        cache: &ResourceCache,
        kind: ResourceKind,
        namespaces: MultiSelection,
        cx: &mut Context<Self>,
    ) -> NamespaceEvents {
        let watches = cache.subscribe_many(
            &kind,
            selected_names(&namespaces).as_deref(),
            WatchFilter::default(),
        );
        let listener = watch_set(&watches, cx, Self::refresh);
        NamespaceEvents {
            cache: cache.clone(),
            namespaces,
            kind,
            watches,
            index: Some(EventIndex::new()),
            view: TimelineView::default(),
            recompute: Recompute::default(),
            shown: Vec::new(),
            pending: 0,
            status: WatchStatus::Loading,
            _listener: listener,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(feed) = &mut self.feed else {
            return;
        };
        if !feed.recompute.begin() {
            return;
        }
        let Some(mut index) = feed.index.take() else {
            // Taken only while running, which `begin` rules out.
            feed.recompute.finish();
            return;
        };
        let snapshot = feed.watches.snapshot();
        let (objects, status) = (snapshot.objects, snapshot.status);
        let kind = feed.kind.clone();
        // Only new and changed events are decoded, off the UI thread; the
        // UI gets the sorted groups.
        feed.recompute.task = Some(cx.spawn(async move |this, cx| {
            let (index, changed) = cx
                .background_spawn(async move {
                    let changed = index.update(&objects, &kind);
                    (index, changed)
                })
                .await;
            this.update(cx, |this, cx| this.refreshed(index, changed, status, cx))
                .ok();
        }));
    }

    /// A background update finished; nothing changed, the rows stay as
    /// they are.
    fn refreshed(
        &mut self,
        index: EventIndex,
        changed: bool,
        status: WatchStatus,
        cx: &mut Context<Self>,
    ) {
        let Some(feed) = &mut self.feed else {
            return;
        };
        if changed {
            feed.view = index.view().clone();
        }
        feed.index = Some(index);
        feed.status = status;
        let again = feed.recompute.finish();
        if changed {
            let hold = self.held();
            self.apply(hold);
        }
        if again {
            self.refresh(cx);
        }
        cx.notify();
    }

    /// Creates the namespace picker and the search, which need the window.
    fn ensure_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_none() {
            let namespaces = self.namespaces.clone();
            self.picker =
                Some(cx.new(|cx| {
                    NamespacePicker::new("events-namespace-picker", namespaces, window, cx)
                }));
        }
        if self.search.is_none() {
            let search = FilterBar::search_state("", window, cx);
            self._subscriptions
                .extend(crate::i18n::localized_placeholder(
                    &search,
                    || t!("events-search-placeholder"),
                    window,
                    cx,
                ));
            self._subscriptions.push(cx.subscribe(
                &search,
                |this, search, event: &InputEvent, cx| {
                    if let InputEvent::Change = event {
                        this.query = search.read(cx).value().to_string();
                        this.release(cx);
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
        self.release(cx);
    }

    fn render_placeholder(&self, cx: &App) -> Option<EmptyState> {
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
                    let empty = feed.view.is_empty();
                    match &feed.status {
                        WatchStatus::Loading if empty => {
                            EmptyState::new(IconName::LoaderCircle, t!("events-loading"))
                        }
                        WatchStatus::Error(error) if empty && error.forbidden => {
                            EmptyState::new(IconName::Ban, t!("events-no-access"))
                                .description(error.message.clone())
                        }
                        WatchStatus::Error(error) if empty => {
                            EmptyState::new(IconName::CircleX, t!("events-cant-load")).description(
                                t!("browser-error-retrying", error = error.message.clone()),
                            )
                        }
                        WatchStatus::Ready if empty => {
                            EmptyState::new(IconName::Inbox, t!("events-none"))
                                .description(t!("events-none-description"))
                        }
                        _ => return None,
                    }
                }
            },
        };
        Some(state)
    }
}

impl ScreenActivity for EventsScreen {
    /// Shown, the screen starts its watches; hidden, it stops them.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            self.sync(cx);
        }
    }
}

impl Render for EventsScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_filters(window, cx);
        let warnings_only = self.warnings_only;
        let mut header = FilterBar::new().action(
            Button::new("events-warnings-only")
                .small()
                .outline()
                .icon(IconName::TriangleAlert)
                .label(t!("events-warnings-only"))
                .selected(warnings_only)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.warnings_only = !this.warnings_only;
                    this.release(cx);
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
        if self.feed.as_ref().is_some_and(|feed| feed.pending > 0) && !self.held() {
            // Scrolled back to the top: show what was held back.
            self.apply(false);
        }
        let Some(feed) = &self.feed else {
            return root.child(
                div().flex_1().child(
                    EmptyState::new(IconName::Inbox, t!("browser-not-served"))
                        .description(t!("events-not-served")),
                ),
            );
        };
        let groups = &feed.shown;
        let warnings = feed.view.warnings();
        // Restarts only when the newest shown event changes.
        self.ages.track(youngest(groups), cx);
        let summary = if groups.len() + feed.pending >= SCREEN_EVENTS_MAX {
            t!(
                "events-summary-capped",
                count = feed.view.len(),
                warnings = warnings,
                shown = SCREEN_EVENTS_MAX
            )
        } else {
            t!(
                "events-summary",
                count = feed.view.len(),
                warnings = warnings
            )
        };
        let searching = !self.query.trim().is_empty();
        if groups.is_empty() && searching && !feed.view.is_empty() {
            let placeholder = FilterBar::no_matches(
                &t!("events-noun"),
                &self.query,
                cx.listener(|this, _, window, cx| this.clear_search(window, cx)),
            );
            return root.child(div().flex_1().child(placeholder));
        }
        let theme = cx.theme();
        let list: AnyElement = if groups.is_empty() {
            note(t!("events-no-warnings"), cx).p_2().into_any_element()
        } else {
            self.timeline.render().into_any_element()
        };
        let pending = feed.pending;
        let new_events = (pending > 0).then(|| {
            let label = t!("events-new", count = pending);
            Button::new("events-show-new")
                .xsmall()
                .ghost()
                .icon(IconName::ArrowUp)
                .label(label)
                .tooltip(t!("events-new-tooltip"))
                .on_click(cx.listener(|this, _, _, cx| this.release(cx)))
        });
        root.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(summary),
                )
                .children(new_events),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .child(list),
        )
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::events::{EventGroup, TimelineEvent, group};
    use kube::api::DynamicObject;
    use serde_json::json;

    use super::{entry, youngest};

    fn event() -> TimelineEvent {
        let object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Event",
            "metadata": { "name": "e1", "namespace": "web", "uid": "u1" },
            "involvedObject": { "kind": "Pod", "namespace": "web", "name": "api-1" },
            "type": "Warning",
            "reason": "BackOff",
            "message": "Back-off restarting failed container",
            "lastTimestamp": "2026-10-22T10:00:00Z",
            "count": 4
        }))
        .unwrap();
        TimelineEvent::from_dynamic(&object).unwrap()
    }

    #[test]
    fn entries_carry_age_object_and_count() {
        let groups: Vec<EventGroup> = group(&[event()]);
        let entry = entry(&groups[0], true, true);
        let text = format!("{entry:?}");
        assert!(text.contains("warning: true"), "{text}");
        assert!(text.contains("web · Pod/api-1"), "{text}");
        // 2026-10-22T10:00:00Z, shown as its age when drawn.
        assert!(text.contains("since: Some(1792663200)"), "{text}");
        assert_eq!(youngest(&groups), Some(1_792_663_200));
        assert!(text.contains("count: 4"), "{text}");
    }
}
