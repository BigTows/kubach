//! "Access control" screens: what the current user may do in a namespace,
//! who can access a resource, and security findings of pods. Objects found
//! here open in the resource browser through [`Reveal`] events.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::cache::{ResourceCache, SubscriptionSet, WatchFilter, WatchStatus};
use kubach_core::rbac::{self, AccessGraph, Grant, RbacSnapshot, ResourceAccess, RulesReview};
use kubach_core::runtime;
use kubach_core::security::{Counts, Severity};
use kubach_core::{ApiCatalog, ConnectionState};
use kubach_i18n::t;
use kubach_ui::{
    EmptyState, FilterBar, FlowGraph, KeyValueList, MultiSelection, Panel, ResourceTable,
    SIDE_PANEL_CONTENT_MIN, SidePanel, SidePanelState, StatusBadge, name_matches,
};
use kube::api::DynamicObject;

use super::activity::{ScreenActivity, watch_set};
use super::namespace_picker::{NamespacePicker, Namespaces};
use super::namespaces::selected_names;
use super::panel_width;
use super::rbac as rbac_rows;
use super::security::{self as security_rows, PodFinding};
use super::table_sort;
use crate::cluster::{CatalogState, Cluster};

const DETAILS_WIDTH: Pixels = px(380.);

/// Which access screen is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessPage {
    Permissions,
    SecurityChecks,
}

/// Asks the workspace to show an object in the resource browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reveal {
    pub group: String,
    pub kind: String,
    /// `None` for cluster-scoped objects.
    pub namespace: Option<String>,
    pub name: String,
}

/// How the "who can" grants are shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GrantsView {
    Table,
    Graph,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PermissionsTab {
    Mine,
    WhoCan,
}

/// A request to the API server and its result.
enum Load<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

/// Pods watched for the security checks.
struct PodWatch {
    selection: MultiSelection,
    watches: SubscriptionSet,
    _listener: Task<()>,
}

/// The one namespace the permission checks run in: the first selected, or
/// `None` (cluster-wide) for all namespaces.
fn first_namespace(selection: &MultiSelection) -> Option<String> {
    selection.items().first().map(ToString::to_string)
}

/// The `scope`, `namespace` and `namespaces` arguments of messages about
/// the selected namespaces: all, one or how many.
fn scope_of(selection: &MultiSelection) -> (&'static str, String, usize) {
    match selection.items() {
        [] => ("any", String::new(), 0),
        [one] => ("one", one.to_string(), 1),
        many => ("many", String::new(), many.len()),
    }
}

/// "Who can" result: the question and the grants that answer it.
struct WhoCan {
    access: ResourceAccess,
    grants: Vec<Grant>,
    graph: AccessGraph,
}

pub struct AccessScreen {
    cluster: Entity<Cluster>,
    /// Page shown while active.
    page: AccessPage,
    /// On screen; otherwise no watches, no requests.
    active: bool,
    tab: PermissionsTab,
    /// Connection the state below belongs to.
    cache: Option<ResourceCache>,
    /// Namespace selection shared with the other list screens.
    namespaces: Entity<Namespaces>,
    /// The selection the state below was computed for.
    selection: Option<MultiSelection>,
    /// Namespace of the permission checks (see [`first_namespace`]).
    namespace: Option<String>,
    /// Created on first render, which has the window they need.
    picker: Option<Entity<NamespacePicker>>,
    search: Option<Entity<InputState>>,
    /// Text of `search`: filters rules by resource, grants by subject and
    /// findings by pod.
    query: String,

    rules: Load<RulesReview>,
    rules_table: Option<Entity<ResourceTable>>,
    _rules_task: Option<Task<()>>,

    verb: Option<Entity<InputState>>,
    resource: Option<Entity<InputState>>,
    name: Option<Entity<InputState>>,
    grants: Load<WhoCan>,
    grants_table: Option<Entity<ResourceTable>>,
    selected_grant: Option<usize>,
    grants_view: GrantsView,
    /// Graph node under the pointer; its chain is highlighted.
    graph_hover: Option<usize>,
    _grants_task: Option<Task<()>>,

    /// Pods watched for the security checks.
    pods: Option<PodWatch>,
    pods_status: WatchStatus,
    /// Every finding, low severity included.
    findings: Vec<PodFinding>,
    include_low: bool,
    /// The table and whether it has a Namespace column.
    findings_table: Option<(bool, Entity<ResourceTable>)>,
    selected_finding: Option<usize>,
    /// Width of the grant and finding details, dragged by its left edge.
    details_panel: Entity<SidePanelState>,

    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Reveal> for AccessScreen {}

impl AccessScreen {
    pub fn new(
        cluster: Entity<Cluster>,
        namespaces: Entity<Namespaces>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&cluster, |this, _, cx| this.sync(cx)),
            cx.observe(&namespaces, |this, _, cx| this.sync(cx)),
        ];
        let details_panel = panel_width::remembered("access.details", DETAILS_WIDTH, cx);
        let mut this = Self {
            cluster,
            page: AccessPage::Permissions,
            active: false,
            tab: PermissionsTab::Mine,
            cache: None,
            namespaces,
            selection: None,
            namespace: None,
            picker: None,
            search: None,
            query: String::new(),
            rules: Load::Idle,
            rules_table: None,
            _rules_task: None,
            verb: None,
            resource: None,
            name: None,
            grants: Load::Idle,
            grants_table: None,
            selected_grant: None,
            grants_view: GrantsView::Table,
            graph_hover: None,
            _grants_task: None,
            pods: None,
            pods_status: WatchStatus::Loading,
            findings: Vec::new(),
            include_low: false,
            findings_table: None,
            selected_finding: None,
            details_panel,
            _subscriptions: subscriptions,
        };
        this.sync(cx);
        this
    }

    /// Picks the page shown while the screen is active.
    pub fn show(&mut self, page: AccessPage, cx: &mut Context<Self>) {
        if self.page != page {
            self.page = page;
            if self.active {
                self.sync(cx);
            }
        }
    }

    /// The page on screen; `None` while inactive.
    fn shown_page(&self) -> Option<AccessPage> {
        self.active.then_some(self.page)
    }

    fn catalog(&self, cx: &App) -> Option<ApiCatalog> {
        match self.cluster.read(cx).catalog() {
            CatalogState::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        }
    }

    /// Brings watches and requests in line with the connection, the
    /// namespace selection and the page.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let cache = self.cluster.read(cx).cache().cloned();
        let selection = self
            .namespaces
            .read(cx)
            .scope()
            .map(|scope| scope.selection.clone());
        let same_cache = match (&self.cache, &cache) {
            (Some(old), Some(new)) => old.same_as(new),
            (None, None) => true,
            _ => false,
        };
        if !same_cache || self.selection != selection {
            // New connection (context switch or reconnect) or namespaces:
            // start over.
            self.cache = cache;
            self.namespace = selection.as_ref().and_then(first_namespace);
            self.selection = selection;
            self.reset_results();
            self.update_grant_rows(cx);
            self.update_findings_rows(cx);
        }

        let catalog = self.catalog(cx);
        let (Some(page), Some(cache), Some(catalog), Some(selection)) = (
            self.shown_page(),
            self.cache.clone(),
            catalog,
            self.selection.clone(),
        ) else {
            self.pods = None;
            cx.notify();
            return;
        };
        match page {
            AccessPage::Permissions => {
                self.pods = None;
                match self.tab {
                    PermissionsTab::Mine if matches!(self.rules, Load::Idle) => self.load_rules(cx),
                    PermissionsTab::WhoCan if matches!(self.grants, Load::Idle) => {
                        self.check_who_can(cx)
                    }
                    _ => {}
                }
            }
            AccessPage::SecurityChecks => {
                if self.pods.as_ref().map(|pods| &pods.selection) != Some(&selection) {
                    self.pods = self.watch_pods(&cache, &catalog, selection, cx);
                    self.refresh_pods(cx);
                }
            }
        }
        cx.notify();
    }

    /// Drops results that belong to another connection or namespace.
    fn reset_results(&mut self) {
        self.rules = Load::Idle;
        self._rules_task = None;
        self.grants = Load::Idle;
        self._grants_task = None;
        self.selected_grant = None;
        self.pods = None;
        self.pods_status = WatchStatus::Loading;
        self.findings.clear();
        self.selected_finding = None;
    }

    /// Watches pods of the selected namespaces; `None` if not served.
    fn watch_pods(
        &self,
        cache: &ResourceCache,
        catalog: &ApiCatalog,
        selection: MultiSelection,
        cx: &mut Context<Self>,
    ) -> Option<PodWatch> {
        let kind = catalog.find("", "Pod")?;
        let watches = cache.subscribe_many(
            kind,
            selected_names(&selection).as_deref(),
            WatchFilter::default(),
        );
        let listener = watch_set(&watches, cx, Self::refresh_pods);
        Some(PodWatch {
            selection,
            watches,
            _listener: listener,
        })
    }

    fn refresh_pods(&mut self, cx: &mut Context<Self>) {
        let Some(pods) = &self.pods else {
            return;
        };
        let snapshot = pods.watches.snapshot();
        let objects: Vec<Arc<DynamicObject>> = snapshot.objects.iter().cloned().collect();
        self.findings = security_rows::findings(&objects, Severity::Low);
        self.pods_status = snapshot.status;
        self.update_findings_rows(cx);
    }

    fn set_tab(&mut self, tab: PermissionsTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.sync(cx);
    }

    fn client(&self, cx: &App) -> Option<kube::Client> {
        self.cluster
            .read(cx)
            .connection()
            .map(|connection| connection.client.clone())
    }

    /// Asks the API server for the current user's rules in the namespace.
    fn load_rules(&mut self, cx: &mut Context<Self>) {
        let (Some(namespace), Some(client)) = (self.namespace.clone(), self.client(cx)) else {
            self.rules = Load::Idle;
            cx.notify();
            return;
        };
        self.rules = Load::Loading;
        let handle =
            runtime::spawn(async move { rbac::rules_in_namespace(&client, &namespace).await });
        self._rules_task = Some(cx.spawn(async move |this, cx| {
            let result = match handle.await {
                Ok(Ok(review)) => Load::Ready(review),
                Ok(Err(error)) => Load::Failed(error.to_string()),
                Err(error) => Load::Failed(error.to_string()),
            };
            this.update(cx, |this, cx| {
                this.rules = result;
                this.update_rule_rows(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    fn input_value(input: &Option<Entity<InputState>>, fallback: &str, cx: &App) -> String {
        input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_else(|| fallback.to_owned())
    }

    /// Lists roles and bindings and finds who may do what the form asks.
    fn check_who_can(&mut self, cx: &mut Context<Self>) {
        let verb = Self::input_value(&self.verb, "get", cx);
        let resource = Self::input_value(&self.resource, "pods", cx);
        let name = Self::input_value(&self.name, "", cx);
        let Some(mut access) = ResourceAccess::parse(&verb, &resource) else {
            self.grants = Load::Failed(t!("access-who-can-invalid"));
            self.update_grant_rows(cx);
            return;
        };
        if let Some(namespace) = &self.namespace {
            access = access.in_namespace(namespace.clone());
        }
        if !name.trim().is_empty() {
            access = access.named(name.trim().to_owned());
        }
        let Some(client) = self.client(cx) else {
            return;
        };
        self.grants = Load::Loading;
        self.selected_grant = None;
        self.graph_hover = None;
        let question = access.clone();
        let handle = runtime::spawn(async move {
            let snapshot = RbacSnapshot::fetch(&client).await?;
            Ok::<_, rbac::RbacError>(rbac::who_can(&snapshot, &question))
        });
        self._grants_task = Some(cx.spawn(async move |this, cx| {
            let result = match handle.await {
                Ok(Ok(grants)) => {
                    let graph = AccessGraph::from_grants(&grants);
                    Load::Ready(WhoCan {
                        access,
                        grants,
                        graph,
                    })
                }
                Ok(Err(error)) => Load::Failed(error.to_string()),
                Err(error) => Load::Failed(error.to_string()),
            };
            this.update(cx, |this, cx| {
                this.grants = result;
                this.update_grant_rows(cx);
            })
            .ok();
        }));
        self.update_grant_rows(cx);
    }

    /// Findings shown in the list: low severity only when asked for.
    fn visible_findings(&self) -> Vec<PodFinding> {
        let min = if self.include_low {
            Severity::Low
        } else {
            Severity::Medium
        };
        self.findings
            .iter()
            .filter(|item| item.finding.severity >= min)
            .filter(|item| name_matches(&item.pod, &self.query))
            .cloned()
            .collect()
    }

    /// Rules whose resources match the search.
    fn visible_rules(&self, review: &RulesReview) -> Vec<rbac::Rule> {
        review
            .rules
            .iter()
            .filter(|rule| {
                self.query.trim().is_empty()
                    || rule
                        .resources
                        .iter()
                        .any(|resource| name_matches(resource, &self.query))
            })
            .cloned()
            .collect()
    }

    /// Grants whose subject matches the search.
    fn visible_grants(&self, result: &WhoCan) -> Vec<Grant> {
        result
            .grants
            .iter()
            .filter(|grant| name_matches(&grant.subject.display_name(), &self.query))
            .cloned()
            .collect()
    }

    fn update_rule_rows(&mut self, cx: &mut Context<Self>) {
        if let (Some(table), Load::Ready(review)) = (&self.rules_table, &self.rules) {
            let rows = rbac_rows::rule_rows(&self.visible_rules(review));
            table.update(cx, |table, cx| table.set_rows(rows, cx));
        }
        cx.notify();
    }

    fn update_grant_rows(&mut self, cx: &mut Context<Self>) {
        if let Some(table) = &self.grants_table {
            let rows = match &self.grants {
                Load::Ready(result) => rbac_rows::grant_rows(&self.visible_grants(result)),
                _ => Vec::new(),
            };
            table.update(cx, |table, cx| {
                table.clear_selection(cx);
                table.set_rows(rows, cx);
            });
        }
        cx.notify();
    }

    fn update_findings_rows(&mut self, cx: &mut Context<Self>) {
        if let Some((with_namespace, table)) = &self.findings_table {
            let rows = security_rows::rows(&self.visible_findings(), *with_namespace);
            // Rows are keyed by position; with the details closed, a stale
            // selection would also swallow the click that reopens them.
            let closed = self.selected_finding.is_none();
            table.update(cx, |table, cx| {
                if closed {
                    table.clear_selection(cx);
                }
                table.set_rows(rows, cx);
            });
        }
        cx.notify();
    }

    /// Creates tables and inputs, which need the window.
    fn ensure_widgets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        if self.rules_table.is_none() {
            let table = cx.new(|cx| ResourceTable::new(rbac_rows::rule_columns(), window, cx));
            table_sort::remember(&table, table_sort::screen("access.rules"), cx);
            self.rules_table = Some(table);
            self.update_rule_rows(cx);
        }
        if self.grants_table.is_none() {
            let this = this.clone();
            let table = cx.new(|cx| {
                ResourceTable::new(rbac_rows::grant_columns(), window, cx).on_select(
                    move |row, _, cx| {
                        let ix = row.key.parse().ok();
                        this.update(cx, |this, cx| {
                            this.selected_grant = ix;
                            cx.notify();
                        })
                        .ok();
                    },
                )
            });
            table_sort::remember(&table, table_sort::screen("access.grants"), cx);
            self.grants_table = Some(table);
            self.update_grant_rows(cx);
        }
        let with_namespace = !matches!(
            self.selection.as_ref().map(MultiSelection::items),
            Some([_])
        );
        if self
            .findings_table
            .as_ref()
            .is_none_or(|(shown, _)| *shown != with_namespace)
        {
            let table = cx.new(|cx| {
                ResourceTable::new(security_rows::columns(with_namespace), window, cx).on_select(
                    move |row, _, cx| {
                        let ix = row.key.parse().ok();
                        this.update(cx, |this, cx| {
                            this.selected_finding = ix;
                            cx.notify();
                        })
                        .ok();
                    },
                )
            });
            table_sort::remember(&table, table_sort::screen("access.findings"), cx);
            self.findings_table = Some((with_namespace, table));
            self.selected_finding = None;
            self.update_findings_rows(cx);
        }
        if self.picker.is_none() {
            let namespaces = self.namespaces.clone();
            self.picker =
                Some(cx.new(|cx| {
                    NamespacePicker::new("access-namespace-picker", namespaces, window, cx)
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
                        this.on_query_change(cx);
                    }
                },
            ));
            self.search = Some(search);
        }
        if self.verb.is_none() {
            let mut input = |value: &str, placeholder: fn() -> String| {
                let input =
                    cx.new(|cx| InputState::new(window, cx).default_value(value.to_owned()));
                self._subscriptions
                    .extend(crate::i18n::localized_placeholder(
                        &input,
                        placeholder,
                        window,
                        cx,
                    ));
                let subscription = cx.subscribe_in(&input, window, |this, _, event, _, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.check_who_can(cx);
                    }
                });
                self._subscriptions.push(subscription);
                input
            };
            let verb = input("get", || t!("access-verb-placeholder"));
            let resource = input("pods", || t!("access-resource-placeholder"));
            let name = input("", || t!("access-name-placeholder"));
            self.verb = Some(verb);
            self.resource = Some(resource);
            self.name = Some(name);
        }
    }

    /// Re-filters every list after the search changed.
    fn on_query_change(&mut self, cx: &mut Context<Self>) {
        self.selected_grant = None;
        self.selected_finding = None;
        self.update_rule_rows(cx);
        self.update_grant_rows(cx);
        self.update_findings_rows(cx);
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
        self.on_query_change(cx);
    }

    /// "No rules match …" with a button that clears the search.
    fn render_no_matches(&self, noun: &str, cx: &mut Context<Self>) -> AnyElement {
        FilterBar::no_matches(
            noun,
            &self.query,
            cx.listener(|this, _, window, cx| this.clear_search(window, cx)),
        )
        .into_any_element()
    }

    /// Placeholder while there is no connection or API catalog.
    fn render_unavailable(&self, cx: &App) -> Option<AnyElement> {
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
                CatalogState::Ready(_) => return None,
                CatalogState::Pending => {
                    EmptyState::new(IconName::LoaderCircle, t!("browser-discovering"))
                }
                CatalogState::Error(error) => {
                    EmptyState::new(IconName::CircleX, t!("browser-discovery-failed"))
                        .description(error.clone())
                }
            },
        };
        Some(state.into_any_element())
    }

    fn render_note(&self, text: impl Into<SharedString>, cx: &App) -> Div {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
    }

    fn render_banner(&self, text: impl Into<SharedString>, color: Hsla) -> Div {
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(color)
            .text_sm()
            .text_color(color)
            .child(text.into())
    }

    fn render_table(&self, table: &Entity<ResourceTable>, cx: &App) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(table.clone())
    }

    fn render_mine(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(namespace) = self.namespace.clone() else {
            return EmptyState::new(IconName::Inbox, t!("access-pick-namespace"))
                .description(t!("access-pick-namespace-description"))
                .into_any_element();
        };
        let review = match &self.rules {
            Load::Idle | Load::Loading => {
                return EmptyState::new(IconName::LoaderCircle, t!("access-reviewing"))
                    .into_any_element();
            }
            Load::Failed(error) => {
                return EmptyState::new(IconName::CircleX, t!("access-cant-review"))
                    .description(error.clone())
                    .into_any_element();
            }
            Load::Ready(review) => review,
        };
        let Some(table) = &self.rules_table else {
            return div().into_any_element();
        };
        if !review.rules.is_empty() && self.visible_rules(review).is_empty() {
            return self.render_no_matches(&t!("access-noun-rules"), cx);
        }
        let theme = cx.theme();
        let incomplete = review.incomplete.then(|| {
            let text = match &review.evaluation_error {
                Some(error) => t!("access-incomplete-reason", error = error.clone()),
                None => t!("access-incomplete"),
            };
            self.render_banner(text, theme.warning)
        });
        let non_resource = (!review.non_resource_rules.is_empty()).then(|| {
            let list = review
                .non_resource_rules
                .iter()
                .map(|rule| format!("{} ({})", rule.urls.join(", "), rule.verbs.join(", ")))
                .collect::<Vec<_>>()
                .join(" · ");
            self.render_note(t!("access-non-resource", urls = list), cx)
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(incomplete)
            .child(self.render_note(
                t!(
                    "access-rules-note",
                    count = review.rules.len(),
                    namespace = namespace,
                    selected = self.selected_count()
                ),
                cx,
            ))
            .child(self.render_table(table, cx))
            .children(non_resource)
            .into_any_element()
    }

    /// How many namespaces are selected, for "(the first of 3 selected)"
    /// when a check runs in one of several.
    fn selected_count(&self) -> usize {
        self.selection
            .as_ref()
            .map_or(0, |selection| selection.items().len())
    }

    fn render_who_can_form(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let field = |input: &Option<Entity<InputState>>, width: f32| {
            div()
                .w(px(width))
                .children(input.as_ref().map(|input| Input::new(input).small()))
        };
        let running = matches!(self.grants, Load::Loading);
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(field(&self.verb, 110.))
            .child(field(&self.resource, 260.))
            .child(field(&self.name, 180.))
            .child(
                Button::new("who-can-check")
                    .primary()
                    .small()
                    .label(t!("access-check"))
                    .disabled(running || self.cache.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.check_who_can(cx))),
            )
    }

    fn render_who_can(&self, cx: &mut Context<Self>) -> AnyElement {
        let form = self.render_who_can_form(cx).into_any_element();
        let body = match &self.grants {
            Load::Idle | Load::Loading => {
                EmptyState::new(IconName::LoaderCircle, t!("access-checking"))
                    .description(t!("access-checking-description"))
                    .into_any_element()
            }
            Load::Failed(error) => EmptyState::new(IconName::CircleX, t!("access-cant-check"))
                .description(t!("access-cant-check-description", error = error.clone()))
                .into_any_element(),
            Load::Ready(result) => self.render_grants(result, cx),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(form)
            .child(body)
            .into_any_element()
    }

    fn render_grants(&self, result: &WhoCan, cx: &mut Context<Self>) -> AnyElement {
        let access = &result.access;
        let mut resource = access.resource.clone();
        if !access.group.is_empty() {
            resource = format!("{resource}.{}", access.group);
        }
        if let Some(sub) = &access.subresource {
            resource = format!("{resource}/{sub}");
        }
        let question = match &access.name {
            Some(name) => t!(
                "access-question-named",
                verb = access.verb.clone(),
                resource = resource,
                name = name.clone()
            ),
            None => format!("{} {resource}", access.verb),
        };
        let count = result.grants.len();
        let note = match &access.namespace {
            Some(namespace) => t!(
                "access-grants-note",
                count = count,
                question = question,
                namespace = namespace.clone(),
                selected = self.selected_count()
            ),
            None => t!(
                "access-grants-note-cluster",
                count = count,
                question = question
            ),
        };
        if result.grants.is_empty() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_2()
                .child(self.render_note(note, cx))
                .child(EmptyState::new(IconName::Inbox, t!("access-nobody")))
                .into_any_element();
        }
        let Some(table) = &self.grants_table else {
            return div().into_any_element();
        };
        let visible = self.visible_grants(result);
        if visible.is_empty() {
            return self.render_no_matches(&t!("access-noun-subjects"), cx);
        }
        let header = div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().flex_1().min_w_0().child(self.render_note(note, cx)))
            .child(self.render_grants_view_toggle(cx));
        if self.grants_view == GrantsView::Graph {
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(header)
                .child(self.render_graph(&result.graph, cx))
                .into_any_element();
        }
        let details = self
            .selected_grant
            .and_then(|ix| visible.get(ix))
            .map(|grant| self.render_grant_details(grant, cx).into_any_element());
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(details.is_some(), |this| this.min_w(SIDE_PANEL_CONTENT_MIN))
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(header)
                    .child(self.render_table(table, cx)),
            )
            .children(details)
            .into_any_element()
    }

    fn render_grants_view_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        TabBar::new("grants-view")
            .segmented()
            .small()
            .selected_index(match self.grants_view {
                GrantsView::Table => 0,
                GrantsView::Graph => 1,
            })
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.grants_view = if *ix == 0 {
                    GrantsView::Table
                } else {
                    GrantsView::Graph
                };
                this.graph_hover = None;
                cx.notify();
            }))
            .child(Tab::new().label(t!("access-view-table")))
            .child(Tab::new().label(t!("access-view-graph")))
    }

    /// Subject → binding → role → rule; hovering traces a node's chain,
    /// clicking opens the object in the resource browser.
    fn render_graph(&self, graph: &AccessGraph, cx: &mut Context<Self>) -> impl IntoElement {
        let reveals: Vec<Option<Reveal>> = graph
            .nodes
            .iter()
            .map(|node| rbac_rows::node_reveal(&node.kind))
            .collect();
        let highlight = self
            .graph_hover
            .filter(|ix| *ix < graph.nodes.len())
            .map(|ix| graph.chain(ix));
        let flow = FlowGraph::new("who-can-graph")
            .columns(rbac_rows::graph_columns())
            .nodes(rbac_rows::graph_nodes(graph))
            .edges(graph.edges.iter().map(|edge| (edge.from, edge.to)))
            .highlight(highlight)
            .on_hover(cx.listener(|this, (ix, hovered): &(usize, bool), _, cx| {
                if *hovered {
                    this.graph_hover = Some(*ix);
                } else if this.graph_hover == Some(*ix) {
                    this.graph_hover = None;
                }
                cx.notify();
            }))
            .on_click(cx.listener(move |_, ix: &usize, _, cx| {
                if let Some(Some(reveal)) = reveals.get(*ix) {
                    cx.emit(reveal.clone());
                }
            }));
        div()
            .id("who-can-graph-scroll")
            .flex_1()
            .min_h_0()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .p_4()
            .overflow_scroll()
            .child(flow)
    }

    fn render_reveal_buttons(
        &self,
        id: &'static str,
        objects: Vec<(String, Reveal)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(
                objects
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (label, reveal))| {
                        Button::new((id, ix))
                            .outline()
                            .small()
                            .label(label)
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(reveal.clone())))
                    }),
            )
    }

    fn render_details_header(
        &self,
        title: String,
        on_close: fn(&mut Self),
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(title),
            )
            .child(
                Button::new("access-close-details")
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        on_close(this);
                        // So clicking the same row again reopens the details.
                        let tables = [
                            this.grants_table.clone(),
                            this.findings_table.as_ref().map(|(_, table)| table.clone()),
                        ];
                        for table in tables.into_iter().flatten() {
                            table.update(cx, |table, cx| table.clear_selection(cx));
                        }
                        cx.notify();
                    })),
            )
    }

    fn render_grant_details(&self, grant: &Grant, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = KeyValueList::new()
            .row(t!("access-subject"), grant.subject.kind())
            .row(t!("access-binding"), rbac_rows::binding_label(grant))
            .row(t!("access-role"), rbac_rows::role_label(grant))
            .row(
                t!("access-rule"),
                div()
                    .whitespace_normal()
                    .child(rbac_rows::rule_label(&grant.rule)),
            );
        let header = self.render_details_header(
            grant.subject.display_name(),
            |this| this.selected_grant = None,
            cx,
        );
        let details = div()
            .id("grant-details")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(self.render_reveal_buttons("grant-open", rbac_rows::grant_objects(grant), cx))
            .child(Panel::new().child(summary));
        SidePanel::new("grant-details-panel", &self.details_panel).child(details)
    }

    fn render_permissions(&self, cx: &mut Context<Self>) -> AnyElement {
        let tabs = TabBar::new("permissions-tabs")
            .underline()
            .small()
            .selected_index(match self.tab {
                PermissionsTab::Mine => 0,
                PermissionsTab::WhoCan => 1,
            })
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                let tab = if *ix == 0 {
                    PermissionsTab::Mine
                } else {
                    PermissionsTab::WhoCan
                };
                this.set_tab(tab, cx);
            }))
            .child(Tab::new().label(t!("access-tab-mine")))
            .child(Tab::new().label(t!("access-tab-who-can")));
        let body = match self.tab {
            PermissionsTab::Mine => self.render_mine(cx),
            PermissionsTab::WhoCan => self.render_who_can(cx),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(tabs)
            .child(body)
            .into_any_element()
    }

    fn render_security(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(pods) = &self.pods else {
            return EmptyState::new(IconName::Inbox, t!("access-pods-not-served"))
                .into_any_element();
        };
        let (scope, namespace, namespaces) = scope_of(&pods.selection);
        let visible = self.visible_findings();
        let searching = !self.query.trim().is_empty();
        let no_matches = searching
            && visible.is_empty()
            && !self.findings.is_empty()
            && !matches!(self.pods_status, WatchStatus::Loading);
        if no_matches {
            return self.render_no_matches(&t!("access-noun-findings"), cx);
        }
        let placeholder = match &self.pods_status {
            WatchStatus::Loading if self.findings.is_empty() => Some(EmptyState::new(
                IconName::LoaderCircle,
                t!("access-loading-pods"),
            )),
            WatchStatus::Error(error) if self.findings.is_empty() && error.forbidden => Some(
                EmptyState::new(IconName::Ban, t!("access-no-access-pods")).description(t!(
                    "access-no-access-pods-description",
                    scope = scope,
                    namespace = namespace.clone(),
                    namespaces = namespaces
                )),
            ),
            WatchStatus::Error(error) if self.findings.is_empty() => Some(
                EmptyState::new(IconName::CircleX, t!("access-cant-load-pods"))
                    .description(t!("browser-error-retrying", error = error.message.clone())),
            ),
            _ if visible.is_empty() => Some(
                EmptyState::new(IconName::CircleCheck, t!("access-no-findings")).description(t!(
                    "access-no-findings-description",
                    scope = scope,
                    namespace = namespace.clone(),
                    namespaces = namespaces
                )),
            ),
            _ => None,
        };
        let counts = Counts::of(
            &self
                .findings
                .iter()
                .map(|item| item.finding.clone())
                .collect::<Vec<_>>(),
        );
        let toolbar = div()
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!(
                        "access-security-summary",
                        summary = security_rows::summary(counts),
                        scope = scope,
                        namespace = namespace,
                        namespaces = namespaces
                    )),
            )
            .child(
                Switch::new("security-include-low")
                    .checked(self.include_low)
                    .label(t!("access-show-low"))
                    .small()
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.include_low = *checked;
                        this.selected_finding = None;
                        this.update_findings_rows(cx);
                    })),
            );
        let body = match (placeholder, &self.findings_table) {
            (Some(placeholder), _) => placeholder.into_any_element(),
            (None, Some((_, table))) => {
                let details = self
                    .selected_finding
                    .and_then(|ix| visible.get(ix))
                    .map(|item| self.render_finding_details(item, cx).into_any_element());
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(details.is_some(), |this| this.min_w(SIDE_PANEL_CONTENT_MIN))
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(self.render_table(table, cx)),
                    )
                    .children(details)
                    .into_any_element()
            }
            (None, None) => div().into_any_element(),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }

    fn render_finding_details(
        &self,
        item: &PodFinding,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let finding = &item.finding;
        let mut summary = KeyValueList::new()
            .row(
                t!("security-column-severity"),
                StatusBadge::new(
                    security_rows::severity_title(finding.severity),
                    security_rows::tone(finding.severity),
                ),
            )
            .row(t!("security-column-pod"), item.pod.clone());
        if let Some(namespace) = &item.namespace {
            summary = summary.row(t!("column-namespace"), namespace.clone());
        }
        summary = summary
            .row(
                t!("security-column-container"),
                finding.container.clone().unwrap_or_else(|| "—".into()),
            )
            .row(
                t!("security-fix"),
                div().whitespace_normal().child(finding.hint.to_string()),
            );
        let header = self.render_details_header(
            finding.title.to_string(),
            |this| this.selected_finding = None,
            cx,
        );
        let pod = Reveal {
            group: String::new(),
            kind: "Pod".into(),
            namespace: item.namespace.clone(),
            name: item.pod.clone(),
        };
        let details = div()
            .id("finding-details")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(self.render_reveal_buttons(
                "finding-open",
                vec![(t!("security-open-pod"), pod)],
                cx,
            ))
            .child(Panel::new().child(summary));
        SidePanel::new("finding-details-panel", &self.details_panel).child(details)
    }
}

impl ScreenActivity for AccessScreen {
    /// Hidden, the screen stops its watches and sends no requests.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            self.sync(cx);
        }
    }
}

impl Render for AccessScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_widgets(window, cx);
        let refresh = match (self.shown_page(), self.tab) {
            (Some(AccessPage::Permissions), tab) => Some(
                Button::new("access-refresh")
                    .outline()
                    .small()
                    .icon(IconName::RefreshCw)
                    .tooltip(t!("access-check-again"))
                    .disabled(self.cache.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| match tab {
                        PermissionsTab::Mine => this.load_rules(cx),
                        PermissionsTab::WhoCan => this.check_who_can(cx),
                    })),
            ),
            _ => None,
        };
        let mut header = FilterBar::new().actions(refresh);
        if let Some(picker) = &self.picker {
            header = header.filter(picker.clone());
        }
        if let Some(search) = &self.search {
            header = header.search(search);
        }
        let body = match (self.render_unavailable(cx), self.shown_page()) {
            (Some(unavailable), _) => unavailable,
            (None, Some(AccessPage::SecurityChecks)) => self.render_security(cx),
            (None, _) => self.render_permissions(cx),
        };
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_4()
            .p_8()
            .child(header)
            .child(body)
    }
}
