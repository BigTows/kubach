//! The Prometheus of every connected context (ADR 0009), shared app-wide:
//! the tables (#179) and the details panel of a context use one connection.
//!
//! Which server a context uses is decided by [`Plan`] from the source picked
//! in the settings and the search remembered in `state.json`. The cluster
//! is searched only for Auto without a remembered search (or when the user
//! asks: "Detect again"); the result is remembered, so later starts use it
//! without searching. A server that does not answer is reported, not
//! replaced by a new search.

use std::collections::{HashMap, HashSet};

use gpui_kit::*;
use kubach_core::metrics::prometheus::source::{self, Plan};
use kubach_core::metrics::prometheus::{Endpoint, Prometheus, PrometheusError};
use kubach_core::runtime;
use kubach_core::settings::PrometheusSource;
use kubach_core::state::PrometheusDiscovery;
use kubach_i18n::t;
use kubach_ui::unix_now;

use crate::cluster::Cluster;
use crate::settings::{AppSettings, SettingsTarget, open_settings};
use crate::state::StateStore;

/// The Prometheus of one context, as the screens see it.
#[derive(Clone, Debug)]
pub enum PrometheusStatus {
    /// Off in the settings: no search, no requests.
    Off,
    /// The cluster is being searched.
    Detecting,
    /// The search could not run (cluster unreachable, no permission). The reason is
    /// [`PrometheusRegistry::detect_error`].
    DetectFailed,
    /// Auto: the search found no Prometheus.
    NotFound,
    /// Manual, without an address.
    NoAddress,
    /// The chosen address cannot be parsed.
    Invalid(String),
    /// Probing the chosen server (or waiting for the context to connect).
    Connecting(Endpoint),
    Ready(Prometheus),
    /// The chosen server did not answer.
    Unreachable(Endpoint),
    /// Auto found Prometheus only outside the well-known namespaces: not
    /// connected until the user confirms it ([`confirm`], #200).
    Unconfirmed(Endpoint),
}

impl PrometheusStatus {
    pub fn prometheus(&self) -> Option<&Prometheus> {
        match self {
            Self::Ready(prometheus) => Some(prometheus),
            _ => None,
        }
    }

    /// The server waiting for the user's confirmation, if any.
    pub fn unconfirmed(&self) -> Option<&Endpoint> {
        match self {
            Self::Unconfirmed(endpoint) => Some(endpoint),
            _ => None,
        }
    }

    /// Still deciding: searching or connecting.
    pub fn pending(&self) -> bool {
        matches!(self, Self::Detecting | Self::Connecting(_))
    }

    /// What is wrong with the chosen server, for the screens that use it.
    /// A chosen server that fails is reported, never searched for again
    /// behind the user's back (ADR 0009).
    pub fn problem(&self) -> Option<String> {
        match self {
            Self::Unreachable(endpoint) => Some(doesnt_answer(endpoint)),
            Self::Invalid(address) => {
                Some(t!("prometheus-invalid-address", address = address.clone()))
            }
            Self::Unconfirmed(endpoint) => Some(unusual_namespace(endpoint)),
            _ => None,
        }
    }
}

/// "Prometheus at … doesn't answer": the probe failed, or polling did.
pub fn doesnt_answer(endpoint: &Endpoint) -> String {
    t!("prometheus-doesnt-answer", endpoint = endpoint.describe())
}

/// The warning for a server found only in an unusual namespace.
pub fn unusual_namespace(endpoint: &Endpoint) -> String {
    t!(
        "prometheus-unusual-namespace",
        endpoint = endpoint.describe()
    )
}

/// Uses `endpoint`, found by the search, for `context` ("Use it"): picks it
/// in the settings, so it is connected from now on.
pub fn confirm(context: &str, endpoint: &Endpoint, cx: &mut App) {
    let Some(settings) = AppSettings::try_global(cx) else {
        return;
    };
    let source = PrometheusSource::Found(endpoint.describe());
    settings.update(cx, |settings, cx| {
        settings.edit(cx, |settings| {
            settings.clusters.set_prometheus_source(context, source)
        })
    });
}

/// A problem with the Prometheus of `context` (it doesn't answer, it is in
/// an unusual namespace) with its actions: "Use it" for a server waiting
/// for confirmation, "Detect again" and "Settings". Button ids start with
/// `id_prefix`.
pub fn problem_bar(id_prefix: &str, context: &str, problem: String, cx: &App) -> AnyElement {
    use gpui_kit::component::button::{Button, ButtonVariants as _};
    use gpui_kit::component::{ActiveTheme as _, Sizable as _};
    use gpui_kit::prelude::FluentBuilder as _;
    use kubach_ui::ButtonLoading as _;

    let theme = cx.theme();
    let registry = PrometheusRegistry::try_global(cx);
    let detecting = registry
        .as_ref()
        .is_some_and(|registry| registry.read(cx).detecting(context));
    // Found only in an unusual namespace: "Use it" confirms it (#200).
    let unconfirmed =
        registry.and_then(|registry| registry.read(cx).status(context, cx).unconfirmed().cloned());
    let id = |name: &str| SharedString::from(format!("{id_prefix}-{name}"));
    let (detect_context, settings_context) = (context.to_owned(), context.to_owned());
    let confirm_context = context.to_owned();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .min_w_0()
                .text_xs()
                .text_color(theme.warning)
                .child(problem),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .gap_1()
                .when_some(unconfirmed, |row, endpoint| {
                    row.child(
                        Button::new(id("confirm-prometheus"))
                            .ghost()
                            .xsmall()
                            .label(t!("prometheus-use-it"))
                            .tooltip(t!("prometheus-use-it-tooltip"))
                            .on_click(move |_, _, cx| confirm(&confirm_context, &endpoint, cx)),
                    )
                })
                .child(
                    Button::new(id("detect-prometheus"))
                        .ghost()
                        .xsmall()
                        .label(t!("prometheus-detect-again"))
                        .spinning(detecting)
                        .on_click(move |_, _, cx| {
                            if let Some(registry) = PrometheusRegistry::try_global(cx) {
                                registry.update(cx, |registry, cx| {
                                    registry.detect(&detect_context, cx)
                                });
                            }
                        }),
                )
                .child(
                    Button::new(id("prometheus-settings"))
                        .ghost()
                        .xsmall()
                        .label(t!("prometheus-settings"))
                        .on_click(move |_, _, cx| {
                            open_settings(SettingsTarget::prometheus(settings_context.clone()), cx)
                        }),
                ),
        )
        .into_any_element()
}

/// Connection to the chosen server of a context.
struct Link {
    endpoint: Endpoint,
    /// `None` while probing; `Err` with the reason when it did not answer.
    state: Option<Result<Prometheus, String>>,
    _task: Task<()>,
}

/// A search of a context's cluster.
enum Detection {
    Running { _task: Task<()> },
    Failed(String),
}

/// Prometheus servers of all connected contexts. Observe it to follow
/// searches and connections.
pub struct PrometheusRegistry {
    cluster: WeakEntity<Cluster>,
    links: HashMap<String, Link>,
    detections: HashMap<String, Detection>,
    /// "Detect again" for contexts still connecting.
    pending: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

struct GlobalRegistry(Entity<PrometheusRegistry>);

impl Global for GlobalRegistry {}

impl PrometheusRegistry {
    /// Makes the registry of the contexts of `cluster` global.
    pub fn install(cluster: &Entity<Cluster>, cx: &mut App) {
        let registry = cx.new(|cx| Self::new(cluster, cx));
        cx.set_global(GlobalRegistry(registry));
    }

    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalRegistry>()
            .map(|global| global.0.clone())
    }

    fn new(cluster: &Entity<Cluster>, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![cx.observe(cluster, |this, cluster, cx| {
            this.on_cluster_change(&cluster, cx)
        })];
        // A new source or address changes the plan.
        if let Some(settings) = AppSettings::try_global(cx) {
            subscriptions.push(cx.observe(&settings, |_, _, cx| cx.notify()));
        }
        Self {
            cluster: cluster.downgrade(),
            links: HashMap::new(),
            detections: HashMap::new(),
            pending: HashSet::new(),
            _subscriptions: subscriptions,
        }
    }

    /// What to do for `context`, from the settings and the remembered
    /// search.
    fn plan(context: &str, cx: &App) -> Plan {
        let settings = AppSettings::try_global(cx);
        let clusters = settings.as_ref().map(|s| &s.read(cx).get().clusters);
        let source = clusters
            .map(|c| c.prometheus_source(context))
            .unwrap_or_default();
        let manual = clusters.and_then(|c| c.prometheus(context));
        let store = StateStore::try_global(cx);
        let discovery = store
            .as_ref()
            .and_then(|store| store.read(cx).get().prometheus(context));
        Plan::of(&source, manual, discovery)
    }

    /// The remembered search of `context`.
    pub fn discovery(context: &str, cx: &App) -> Option<PrometheusDiscovery> {
        StateStore::try_global(cx)?
            .read(cx)
            .get()
            .prometheus(context)
            .cloned()
    }

    /// Servers the user can pick for `context`.
    pub fn choices(context: &str, source: &PrometheusSource, cx: &App) -> Vec<String> {
        source::choices(source, Self::discovery(context, cx).as_ref())
    }

    /// Whether `context`'s cluster is being searched.
    pub fn detecting(&self, context: &str) -> bool {
        self.pending.contains(context)
            || matches!(
                self.detections.get(context),
                Some(Detection::Running { .. })
            )
    }

    /// Why the last search of `context` could not run.
    pub fn detect_error(&self, context: &str) -> Option<&str> {
        match self.detections.get(context) {
            Some(Detection::Failed(error)) => Some(error),
            _ => None,
        }
    }

    /// The Prometheus of `context` as it is now, starting nothing.
    pub fn status(&self, context: &str, cx: &App) -> PrometheusStatus {
        match Self::plan(context, cx) {
            Plan::Off => PrometheusStatus::Off,
            Plan::Discover => match self.detections.get(context) {
                Some(Detection::Failed(_)) => PrometheusStatus::DetectFailed,
                _ => PrometheusStatus::Detecting,
            },
            Plan::NotFound => PrometheusStatus::NotFound,
            Plan::NoAddress => PrometheusStatus::NoAddress,
            Plan::Invalid(address) => PrometheusStatus::Invalid(address),
            Plan::Confirm(endpoint) => PrometheusStatus::Unconfirmed(endpoint),
            Plan::Use(endpoint) => match self.links.get(context) {
                Some(link) if link.endpoint == endpoint => match &link.state {
                    None => PrometheusStatus::Connecting(endpoint),
                    Some(Ok(prometheus)) => PrometheusStatus::Ready(prometheus.clone()),
                    Some(Err(_)) => PrometheusStatus::Unreachable(endpoint),
                },
                _ => PrometheusStatus::Connecting(endpoint),
            },
        }
    }

    /// The Prometheus of `context`, searching (only when there is no
    /// remembered search) or connecting as the plan says.
    pub fn ensure(&mut self, context: &str, cx: &mut Context<Self>) -> PrometheusStatus {
        match Self::plan(context, cx) {
            Plan::Discover if !self.detections.contains_key(context) => {
                self.start_detection(context, cx);
            }
            Plan::Use(endpoint)
                if self
                    .links
                    .get(context)
                    .is_none_or(|link| link.endpoint != endpoint) =>
            {
                self.connect(context, endpoint, cx);
            }
            _ => {}
        }
        self.status(context, cx)
    }

    /// Searches the cluster of `context` again ("Detect again"), connecting
    /// the context first when needed.
    pub fn detect(&mut self, context: &str, cx: &mut Context<Self>) {
        if self.detecting(context) {
            return;
        }
        self.detections.remove(context);
        if !self.start_detection(context, cx) {
            let Some(cluster) = self.cluster.upgrade() else {
                return;
            };
            self.pending.insert(context.to_owned());
            let name = context.to_owned();
            cluster.update(cx, |cluster, cx| cluster.connect_in_background(name, cx));
            cx.notify();
        }
    }

    fn client(&self, context: &str, cx: &App) -> Option<kube::Client> {
        let cluster = self.cluster.upgrade()?;
        let connection = cluster.read(cx).connection_of(context)?;
        Some(connection.client.clone())
    }

    /// Starts searching `context`'s cluster; `false` when it is not
    /// connected.
    fn start_detection(&mut self, context: &str, cx: &mut Context<Self>) -> bool {
        let Some(client) = self.client(context, cx) else {
            return false;
        };
        self.pending.remove(context);
        let handle = runtime::spawn(async move {
            Prometheus::discover(&client)
                .await
                .map(|found| found.iter().map(|p| p.endpoint().clone()).collect())
        });
        let name = context.to_owned();
        let task = cx.spawn(async move |this, cx| {
            let found: Result<Vec<Endpoint>, PrometheusError> = handle
                .await
                .unwrap_or_else(|error| Err(PrometheusError::Request(error.to_string())));
            this.update(cx, |this, cx| this.finish_detection(&name, found, cx))
                .ok();
        });
        self.detections
            .insert(context.to_owned(), Detection::Running { _task: task });
        cx.notify();
        true
    }

    fn finish_detection(
        &mut self,
        context: &str,
        found: Result<Vec<Endpoint>, PrometheusError>,
        cx: &mut Context<Self>,
    ) {
        match source::remembered(found.as_deref(), unix_now().max(0) as u64) {
            Some(discovery) => {
                self.detections.remove(context);
                if let Some(store) = StateStore::try_global(cx) {
                    store.update(cx, |store, cx| {
                        store.edit(cx, |state| state.remember_prometheus(context, discovery))
                    });
                }
                // Probe the chosen server again, even if it is the same.
                self.links.remove(context);
            }
            None => {
                let error = found.err().map(|e| e.to_string()).unwrap_or_default();
                self.detections
                    .insert(context.to_owned(), Detection::Failed(error));
            }
        }
        cx.notify();
    }

    /// Probes `endpoint` for `context`; waits when it is not connected.
    fn connect(&mut self, context: &str, endpoint: Endpoint, cx: &mut Context<Self>) {
        let Some(client) = self.client(context, cx) else {
            self.links.remove(context);
            return;
        };
        let target = endpoint.clone();
        let handle = runtime::spawn(async move { Prometheus::connect(&client, target).await });
        let name = context.to_owned();
        let probed = endpoint.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = handle
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
            this.update(cx, |this, cx| {
                if let Some(link) = this.links.get_mut(&name)
                    && link.endpoint == probed
                {
                    link.state = Some(result);
                    cx.notify();
                }
            })
            .ok();
        });
        self.links.insert(
            context.to_owned(),
            Link {
                endpoint,
                state: None,
                _task: task,
            },
        );
        cx.notify();
    }

    /// Drops the links of contexts that disconnected (or are reconnecting,
    /// with a new client); starts "Detect again" for contexts that just
    /// connected.
    fn on_cluster_change(&mut self, cluster: &Entity<Cluster>, cx: &mut Context<Self>) {
        let connected = |name: &str| cluster.read(cx).connection_of(name).is_some();
        let before = self.links.len();
        self.links.retain(|name, _| connected(name));
        let mut changed = self.links.len() != before;
        let ready: Vec<String> = self
            .pending
            .iter()
            .filter(|name| connected(name))
            .cloned()
            .collect();
        for name in ready {
            self.start_detection(&name, cx);
            changed = true;
        }
        let failed: Vec<(String, String)> = self
            .pending
            .iter()
            .filter_map(|name| match cluster.read(cx).state_of(name) {
                kubach_core::ConnectionState::Error(error) => Some((name.clone(), error.clone())),
                _ => None,
            })
            .collect();
        for (name, error) in failed {
            self.pending.remove(&name);
            self.detections.insert(
                name,
                Detection::Failed(t!("prometheus-cannot-connect", error = error)),
            );
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }
}
