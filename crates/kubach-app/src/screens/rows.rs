//! Converts plugin data (columns, cells, health) into `kubach-ui` table rows.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::px;
use kubach_core::metrics::Usage;
use kubach_core::plugin::{
    Cell, Column, ColumnKind, ContainerBrief, ContainerHealth, ContainerRole, Health, HealthStatus,
    ResourcePlugin, not_ready_count,
};
use kubach_i18n::Locale;
use kubach_ui::{
    ContainerCubeState, ResourceCell, ResourceColumn, ResourceRow, StatusCube, Tone,
    container_tooltip, name_matches, unix_now,
};
use kube::api::DynamicObject;

use super::usage::{self, UsageView};

/// Longest health message shown in a table cell; longer ones (condition
/// messages) are only shown in the details panel.
const STATUS_LABEL_MAX: usize = 18;

pub fn columns(plugin: &dyn ResourcePlugin) -> Vec<ResourceColumn> {
    plugin.columns().iter().map(column).collect()
}

fn column(column: &Column) -> ResourceColumn {
    let base = ResourceColumn::new(column.id, super::plugin_text::column_title(column)).sortable();
    match (column.id, column.kind) {
        ("name", _) => base.width(px(320.)),
        ("namespace", _) => base.width(px(160.)),
        // "3/3".
        ("ready", _) => base.width(px(72.)),
        // "12 (3h ago)".
        ("restarts", _) => base.width(px(100.)).align_end(),
        (_, ColumnKind::Text) => base.width(px(160.)),
        // "Running", "Terminating"; longer ones ("CrashLoopBackOff") are
        // clipped, with the full label in a tooltip.
        (_, ColumnKind::Status) => base.width(px(100.)),
        (_, ColumnKind::Number) => base.width(px(90.)).align_end(),
        // "123d".
        (_, ColumnKind::Age) => base.width(px(64.)).align_end(),
        // A few 10 px cubes.
        (_, ColumnKind::Containers) => base.width(px(110.)),
    }
}

/// Index of the plugin's Namespace column, if it has one.
pub fn namespace_column(plugin: &dyn ResourcePlugin) -> Option<usize> {
    plugin
        .columns()
        .iter()
        .position(|column| column.id == "namespace")
}

/// Row identity: `namespace/name`, or just `name` for cluster-scoped objects.
pub fn object_key(object: &DynamicObject) -> String {
    let name = object.metadata.name.as_deref().unwrap_or_default();
    match object.metadata.namespace.as_deref() {
        Some(namespace) => format!("{namespace}/{name}"),
        None => name.to_owned(),
    }
}

pub fn row(plugin: &dyn ResourcePlugin, object: &DynamicObject) -> ResourceRow {
    let columns = plugin.columns();
    let cells = plugin.cells(object).into_iter().enumerate().map(|(ix, c)| {
        match (columns.get(ix).map(|column| column.id), c) {
            // The notes of CRD versions are ours, the versions names.
            (Some("versions"), Cell::Text(text)) => {
                cell(Cell::Text(super::plugin_text::crd_versions(&text)))
            }
            (_, c) => cell(c),
        }
    });
    ResourceRow::new(object_key(object), cells)
}

/// What the rows of a list are built with besides its objects.
#[derive(Clone)]
pub struct RowBuilder {
    plugin: Arc<dyn ResourcePlugin>,
    /// Plugin column left out of the rows (Namespace for one namespace).
    hidden_column: Option<usize>,
}

/// The row of one object without its usage cells, and what it was built
/// from: enough to tell later whether it still stands.
pub struct BuiltRow {
    object: Arc<DynamicObject>,
    row: ResourceRow,
    /// The language of the texts in the cells.
    locale: Locale,
    /// When the cells were built (Unix seconds).
    built_at: i64,
    /// [`ResourcePlugin::cells_turn_at`] of the object (Unix seconds).
    turns_at: Option<i64>,
}

impl BuiltRow {
    /// Whether the row is what would be built with `inputs`: in the same
    /// language, and not from before a moment its cells turned at.
    fn stands(&self, inputs: &RowInputs) -> bool {
        let turned = self
            .turns_at
            .is_some_and(|at| self.built_at < at && at <= inputs.now);
        self.locale == inputs.locale && !turned
    }
}

impl RowBuilder {
    pub fn new(plugin: Arc<dyn ResourcePlugin>, hidden_column: Option<usize>) -> Self {
        Self {
            plugin,
            hidden_column,
        }
    }

    /// Builds the row of `object` at `now`, in the language `locale` (the
    /// one of the process: it is recorded, not applied).
    pub fn build(&self, object: &Arc<DynamicObject>, now: i64, locale: Locale) -> BuiltRow {
        let plugin = self.plugin.as_ref();
        let mut row = row(plugin, object);
        if let Some(ix) = self.hidden_column.filter(|&ix| ix < row.cells.len()) {
            row.cells.remove(ix);
        }
        let turns_at = plugin.cells_turn_at(object).map(|at| at.as_second());
        BuiltRow {
            object: object.clone(),
            row,
            locale,
            built_at: now,
            turns_at,
        }
    }

    /// [`build`](Self::build) now, off the UI thread; `None` if the
    /// language was switched meanwhile (the row could mix two).
    fn build_detached(&self, object: &Arc<DynamicObject>) -> Option<BuiltRow> {
        let locale = kubach_i18n::locale();
        let built = self.build(object, unix_now(), locale);
        (kubach_i18n::locale() == locale).then_some(built)
    }
}

/// What rows depend on besides their objects and the [`RowBuilder`].
#[derive(Clone, Copy)]
pub struct RowInputs<'a> {
    /// What the usage cells show; `None` shows dashes.
    pub usage: Option<UsageView<'a>>,
    /// Unix seconds.
    pub now: i64,
    pub locale: Locale,
}

impl<'a> RowInputs<'a> {
    /// The clock and the language of the process.
    pub fn current(usage: Option<UsageView<'a>>) -> Self {
        Self {
            usage,
            now: unix_now(),
            locale: kubach_i18n::locale(),
        }
    }
}

/// An update builds at most this many rows where it runs, the UI thread:
/// about a frame of work for pods (`docs/perf.md`). More (a long list
/// shown for the first time, another language) are built on other threads.
pub const OFF_THREAD_ROWS: usize = 300;

/// Rows a [`RowCache`] is missing for a list, to build off the UI thread.
pub struct RowJob {
    builder: RowBuilder,
    /// The list the rows are for.
    list: Vec<Arc<DynamicObject>>,
    missing: Vec<Arc<DynamicObject>>,
}

impl RowJob {
    /// The work in parts of at most `size` rows, each for any thread.
    pub fn parts(&self, size: usize) -> Vec<impl FnOnce() -> Vec<BuiltRow> + Send + 'static> {
        let parts = self.missing.chunks(size.max(1)).map(|objects| {
            let (builder, objects) = (self.builder.clone(), objects.to_vec());
            move || {
                let rows = objects.iter().map(|object| builder.build_detached(object));
                rows.flatten().collect()
            }
        });
        parts.collect()
    }
}

struct Entry {
    object: Arc<DynamicObject>,
    row: Arc<ResourceRow>,
    /// What the usage cells of `row` show; `None` is dashes.
    usage: Option<Usage>,
    built_at: i64,
    turns_at: Option<i64>,
}

impl Entry {
    /// The moment the row has to be built again, if it was built before it.
    fn pending_turn(&self) -> Option<i64> {
        self.turns_at.filter(|&at| self.built_at < at)
    }

    /// Replaces the two usage cells, the last of the row.
    fn set_usage(&mut self, usage: Option<Usage>) {
        let mut cells = self.row.cells.clone();
        cells.truncate(cells.len().saturating_sub(2));
        cells.extend(usage::usage_cells(usage));
        self.row = Arc::new(ResourceRow {
            key: self.row.key.clone(),
            cells,
        });
        self.usage = usage;
    }
}

/// The table rows of a list, kept per object (#165): [`update`] builds
/// rows only for objects that are new or changed, and for the few whose
/// row no longer stands for another reason.
///
/// A row is built from
/// - its object: the cache keeps an unchanged object in the same `Arc`,
///   across updates and relists, so another `Arc` is a changed object;
/// - the plugin and the hidden column: fixed for the life of the cache;
/// - usage: only the two usage cells are replaced, and only when the
///   object's usage differs in a new revision of the feed;
/// - the clock, for cells that turn at a known moment
///   ([`ResourcePlugin::cells_turn_at`]). Ages are not among them: the
///   table formats those from their moment when it draws;
/// - the language of translated texts (statuses, container tooltips): a
///   switch builds every row again.
///
/// Rows do not depend on the search: [`rows`] filters the built ones.
///
/// Many rows to build at once (a long list shown for the first time) are
/// built off the UI thread: see [`update_or_job`](Self::update_or_job).
///
/// [`update`]: Self::update
/// [`rows`]: Self::rows
pub struct RowCache {
    builder: RowBuilder,
    /// Rows end with the CPU and Memory cells.
    usage_columns: bool,
    locale: Locale,
    usage_revision: Option<u64>,
    /// In the order of the objects of the last update.
    entries: Vec<Entry>,
    /// Rows built off the UI thread for the next update, by the address
    /// of their object (alive as long as the row holds it).
    prebuilt: HashMap<usize, BuiltRow>,
    /// The earliest moment a row has to be built again.
    next_turn: Option<i64>,
}

fn address(object: &Arc<DynamicObject>) -> usize {
    Arc::as_ptr(object) as usize
}

fn same_name(a: &DynamicObject, b: &DynamicObject) -> bool {
    a.metadata.name == b.metadata.name && a.metadata.namespace == b.metadata.namespace
}

impl RowCache {
    pub fn new(builder: RowBuilder, usage_columns: bool) -> Self {
        Self {
            builder,
            usage_columns,
            locale: kubach_i18n::locale(),
            usage_revision: None,
            entries: Vec::new(),
            prebuilt: HashMap::new(),
            next_turn: None,
        }
    }

    #[cfg(test)]
    pub fn builder(&self) -> &RowBuilder {
        &self.builder
    }

    /// Whether the clock reached a moment some row turns at: an
    /// [`update`](Self::update) is due though nothing else changed.
    pub fn due(&self, now: i64) -> bool {
        self.next_turn.is_some_and(|at| at <= now)
    }

    /// For each of `objects`, the entry that holds its row as it stands
    /// with `inputs`.
    ///
    /// Lists keep their order, so the entries are walked along with the
    /// objects: an object changed in place or removed is stepped over, and
    /// only an insertion or a reordering looks entries up by address.
    fn standing(&self, objects: &[Arc<DynamicObject>], inputs: &RowInputs) -> Vec<Option<usize>> {
        if inputs.locale != self.locale {
            return vec![None; objects.len()];
        }
        let entries = &self.entries;
        let is = |ix: usize, object: &Arc<DynamicObject>| {
            entries
                .get(ix)
                .is_some_and(|entry| Arc::ptr_eq(&entry.object, object))
        };
        let mut by_address: Option<HashMap<usize, usize>> = None;
        let mut cursor = 0;
        objects
            .iter()
            .map(|object| {
                let ix = if is(cursor, object) {
                    cursor
                } else if entries
                    .get(cursor)
                    .is_some_and(|entry| same_name(&entry.object, object))
                {
                    // A new version of the object in its place.
                    cursor += 1;
                    return None;
                } else if is(cursor + 1, object) {
                    cursor + 1
                } else {
                    let by_address = by_address.get_or_insert_with(|| {
                        let addresses = entries.iter().map(|entry| address(&entry.object));
                        addresses.zip(0..).collect()
                    });
                    *by_address.get(&address(object))?
                };
                cursor = ix + 1;
                let turned = entries[ix]
                    .pending_turn()
                    .is_some_and(|at| at <= inputs.now);
                (!turned).then_some(ix)
            })
            .collect()
    }

    /// The `objects` whose rows [`update`](Self::update) would have to
    /// build.
    #[cfg(test)]
    pub fn missing(
        &self,
        objects: &[Arc<DynamicObject>],
        inputs: &RowInputs,
    ) -> Vec<Arc<DynamicObject>> {
        self.missing_of(objects, inputs, &self.standing(objects, inputs))
    }

    fn missing_of(
        &self,
        objects: &[Arc<DynamicObject>],
        inputs: &RowInputs,
        standing: &[Option<usize>],
    ) -> Vec<Arc<DynamicObject>> {
        let missing = objects.iter().zip(standing).filter(|(object, entry)| {
            entry.is_none()
                && !self
                    .prebuilt
                    .get(&address(object))
                    .is_some_and(|built| built.stands(inputs))
        });
        missing.map(|(object, _)| object.clone()).collect()
    }

    /// Takes rows built ahead: the next [`update`](Self::update) uses
    /// those whose object is still listed and drops the rest.
    pub fn adopt(&mut self, rows: impl IntoIterator<Item = BuiltRow>) {
        let rows = rows.into_iter().map(|row| (address(&row.object), row));
        self.prebuilt.extend(rows);
    }

    /// Brings the rows to `objects` with `inputs`; returns how many rows
    /// it had to build ([adopted](Self::adopt) ones are not counted).
    pub fn update(&mut self, objects: &[Arc<DynamicObject>], inputs: &RowInputs) -> usize {
        let standing = self.standing(objects, inputs);
        self.apply(objects, inputs, standing)
    }

    /// [`update`](Self::update), unless it would build more than
    /// [`OFF_THREAD_ROWS`] rows: then the rows stay as they are, and the
    /// job returned builds the missing ones on other threads
    /// ([`finish`](Self::finish) takes them).
    pub fn update_or_job(
        &mut self,
        objects: &[Arc<DynamicObject>],
        inputs: &RowInputs,
    ) -> Option<RowJob> {
        let standing = self.standing(objects, inputs);
        let missing = self.missing_of(objects, inputs, &standing);
        if missing.len() <= OFF_THREAD_ROWS {
            self.apply(objects, inputs, standing);
            return None;
        }
        Some(RowJob {
            builder: self.builder.clone(),
            list: objects.to_vec(),
            missing,
        })
    }

    /// Takes the `rows` built for `job` and brings the rows to the list
    /// the job was made for. The list may have changed since: the next
    /// update is for the caller to run, and has little left to build
    /// unless most of the list changed meanwhile.
    pub fn finish(&mut self, job: RowJob, rows: Vec<BuiltRow>, inputs: &RowInputs) -> usize {
        self.adopt(rows);
        self.update(&job.list, inputs)
    }

    /// No rows yet: nothing was listed, or the first rows are being built.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn apply(
        &mut self,
        objects: &[Arc<DynamicObject>],
        inputs: &RowInputs,
        standing: Vec<Option<usize>>,
    ) -> usize {
        let revision = inputs.usage.map(|usage| usage.revision);
        let usage_changed = self.usage_revision != revision;
        self.usage_revision = revision;
        self.locale = inputs.locale;
        let usage_columns = self.usage_columns;
        let usage_of = |object: &DynamicObject| {
            let usage = inputs.usage.filter(|_| usage_columns)?;
            usage::usage_of(usage.state, object)
        };
        let mut previous: Vec<Option<Entry>> = std::mem::take(&mut self.entries)
            .into_iter()
            .map(Some)
            .collect();
        let mut prebuilt = std::mem::take(&mut self.prebuilt);
        let mut built = 0;
        let entries = objects.iter().zip(standing).map(|(object, ix)| {
            // `None` again for an object listed twice: its entry is taken.
            if let Some(mut entry) = ix.and_then(|ix| previous[ix].take()) {
                if usage_changed && usage_columns {
                    let usage = usage_of(object);
                    if usage != entry.usage {
                        entry.set_usage(usage);
                    }
                }
                return entry;
            }
            let ready = prebuilt
                .remove(&address(object))
                .filter(|row| row.stands(inputs));
            let BuiltRow {
                object,
                mut row,
                built_at,
                turns_at,
                ..
            } = ready.unwrap_or_else(|| {
                built += 1;
                self.builder.build(object, inputs.now, inputs.locale)
            });
            let usage = usage_of(&object);
            if usage_columns {
                row.cells.extend(usage::usage_cells(usage));
            }
            Entry {
                object,
                row: Arc::new(row),
                usage,
                built_at,
                turns_at,
            }
        });
        self.entries = entries.collect();
        self.next_turn = self.entries.iter().filter_map(Entry::pending_turn).min();
        built
    }

    /// The rows of the objects whose name matches the search `query`, in
    /// the order of the objects.
    pub fn rows(&self, query: &str) -> Vec<Arc<ResourceRow>> {
        let matching = self.entries.iter().filter(|entry| {
            name_matches(
                entry.object.metadata.name.as_deref().unwrap_or_default(),
                query,
            )
        });
        matching.map(|entry| entry.row.clone()).collect()
    }
}

/// Times stay absolute: the table formats them when it draws and keeps
/// them current.
fn cell(cell: Cell) -> ResourceCell {
    match cell {
        Cell::Text(text) if text.is_empty() => ResourceCell::text("—").muted(),
        Cell::Text(text) => ResourceCell::text(text),
        Cell::Number(value) => ResourceCell::number(value),
        Cell::Age(Some(since)) => ResourceCell::age(since.as_second()),
        Cell::Age(None) => ResourceCell::text("—").muted(),
        Cell::CountAge(count, since) => ResourceCell::count_age(count, since.as_second()),
        Cell::Status(health) => ResourceCell::status(status_label(&health), tone(health.status)),
        Cell::Containers(containers) => containers_cell(&containers),
    }
}

/// A cube per container, init containers first and dimmed; sorted by how
/// many are not ready (then by count), so troubled pods group together.
fn containers_cell(containers: &[ContainerBrief]) -> ResourceCell {
    let (init, rest): (Vec<_>, Vec<_>) = containers
        .iter()
        .partition(|c| c.role == ContainerRole::Init);
    let sort = not_ready_count(containers) as i64 * 1000 + containers.len() as i64;
    ResourceCell::cubes(
        init.into_iter().map(container_cube),
        rest.into_iter().map(container_cube),
    )
    .sort_by(sort)
}

/// The status cube of a container: the same colors in the pod table, the
/// details panel and related lists.
pub fn container_cube(container: &ContainerBrief) -> StatusCube {
    let role = match container.role {
        ContainerRole::Init => Some("init"),
        ContainerRole::Sidecar => Some("sidecar"),
        ContainerRole::App => None,
    };
    StatusCube::container(
        cube_state(container.health),
        container_tooltip(
            &container.name,
            role,
            &super::plugin_text::container_state(container),
            container.restarts,
        ),
    )
}

fn cube_state(health: ContainerHealth) -> ContainerCubeState {
    match health {
        ContainerHealth::Ready => ContainerCubeState::Ready,
        ContainerHealth::NotReady => ContainerCubeState::NotReady,
        ContainerHealth::Starting => ContainerCubeState::Starting,
        ContainerHealth::Failing => ContainerCubeState::Failing,
        ContainerHealth::Completed => ContainerCubeState::Completed,
        ContainerHealth::Failed => ContainerCubeState::Failed,
        ContainerHealth::Unknown => ContainerCubeState::Unknown,
    }
}

/// Short label for a health badge: the plugin's message when it is a short
/// word like `Running`, otherwise the status itself.
pub fn status_label(health: &Health) -> String {
    if !health.message.is_empty() && health.message.chars().count() <= STATUS_LABEL_MAX {
        super::plugin_text::health_message(health)
    } else {
        super::plugin_text::status_title(health.status)
    }
}

/// The English name of `status`, as plugins write it in health messages;
/// [`super::plugin_text::status_title`] is the one to show.
pub fn status_name(status: HealthStatus) -> &'static str {
    match status {
        HealthStatus::Healthy => "Healthy",
        HealthStatus::Progressing => "Progressing",
        HealthStatus::Degraded => "Degraded",
        HealthStatus::Failed => "Failed",
        HealthStatus::Unknown => "Unknown",
    }
}

pub fn tone(status: HealthStatus) -> Tone {
    match status {
        HealthStatus::Healthy => Tone::Success,
        HealthStatus::Progressing | HealthStatus::Degraded => Tone::Warning,
        HealthStatus::Failed => Tone::Danger,
        HealthStatus::Unknown => Tone::Neutral,
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::plugin::{ContainerPhase, PluginRegistry};
    use serde_json::json;

    use super::*;

    #[test]
    fn rows_follow_plugin_cells() {
        let object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": "app",
                "namespace": "web",
                "creationTimestamp": "2026-10-01T00:00:00Z"
            }
        }))
        .unwrap();
        let plugin = PluginRegistry::new().plugin_for("", "ConfigMap");
        let created = 1_790_812_800; // 2026-10-01T00:00:00Z
        let row = row(plugin.as_ref(), &object);
        assert_eq!(row.key.as_ref(), "web/app");
        let labels: Vec<_> = row.cells.iter().map(|c| c.label().to_string()).collect();
        // The age is counted from the creation time when drawn.
        let age = kubach_ui::format_since(created, kubach_ui::unix_now());
        assert_eq!(labels, ["app", "web", age.as_str()]);
        assert_eq!(columns(plugin.as_ref()).len(), row.cells.len());
    }

    #[test]
    fn containers_sort_by_trouble_and_init_cubes_come_first() {
        let brief = |name: &str, role, health| ContainerBrief {
            name: name.into(),
            role,
            health,
            state: "Running".into(),
            phase: ContainerPhase::Running { since: None },
            restarts: 3,
        };
        let healthy = cell(Cell::Containers(vec![
            brief("a", ContainerRole::App, ContainerHealth::Ready),
            brief("b", ContainerRole::App, ContainerHealth::Ready),
            brief("c", ContainerRole::App, ContainerHealth::Ready),
        ]));
        let troubled = cell(Cell::Containers(vec![
            brief("migrate", ContainerRole::Init, ContainerHealth::Completed),
            brief("app", ContainerRole::App, ContainerHealth::Failing),
        ]));
        assert_eq!(troubled.label().as_ref(), "migrate · init, app");
        assert_eq!(healthy.label().as_ref(), "a, b, c");
        let cube = container_cube(&brief("app", ContainerRole::App, ContainerHealth::Failing));
        assert_eq!(cube.tone, Tone::Danger);
        assert_eq!(cube.tooltip.as_ref(), "app\nRunning\nRestarts: 3");
    }

    #[test]
    fn long_health_messages_fall_back_to_the_status() {
        let short = Health::new(HealthStatus::Healthy, "Running");
        let long = Health::new(
            HealthStatus::Degraded,
            "Deployment does not have minimum availability.",
        );
        assert_eq!(status_label(&short), "Running");
        assert_eq!(status_label(&long), "Degraded");
        assert_eq!(status_label(&Health::unknown()), "Unknown");
    }

    // The rows kept per object (#165).

    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    use k8s_openapi::jiff::Timestamp;
    use kubach_core::metrics::{ContainerMetrics, MetricsIndex, NodeMetrics, PodMetrics};
    use kubach_core::plugin::{DetailSection, Relation};
    use kube::api::GroupVersionKind;

    use super::super::usage::MetricsState;

    /// What the cells of the [`Stub`] plugin read besides the object: the
    /// clock and the language, as real plugins read those of the process.
    #[derive(Default)]
    struct World {
        now: AtomicI64,
        language: AtomicUsize,
        /// How many times the plugin built cells.
        built: AtomicUsize,
    }

    impl World {
        fn now(&self) -> i64 {
            self.now.load(Ordering::Relaxed)
        }

        fn locale(&self) -> Locale {
            Locale::ALL[self.language.load(Ordering::Relaxed) % Locale::ALL.len()]
        }

        fn built(&self) -> usize {
            self.built.load(Ordering::Relaxed)
        }
    }

    /// Cells that show the object's version, whether its moment (the
    /// `turn` annotation) has come, and the language.
    struct Stub(Arc<World>);

    fn turn_of(object: &DynamicObject) -> Option<i64> {
        let annotations = object.metadata.annotations.as_ref()?;
        annotations.get("turn")?.parse().ok()
    }

    impl ResourcePlugin for Stub {
        fn name(&self) -> &'static str {
            "stub"
        }

        fn supported(&self) -> Vec<GroupVersionKind> {
            Vec::new()
        }

        fn columns(&self) -> Vec<Column> {
            let column = |id, kind| Column {
                id,
                title: id,
                kind,
            };
            vec![
                column("name", ColumnKind::Text),
                column("namespace", ColumnKind::Text),
                column("state", ColumnKind::Text),
                column("age", ColumnKind::Age),
            ]
        }

        fn cells(&self, object: &DynamicObject) -> Vec<Cell> {
            self.0.built.fetch_add(1, Ordering::Relaxed);
            let meta = &object.metadata;
            let turned = turn_of(object).is_some_and(|at| at <= self.0.now());
            let version = meta.resource_version.as_deref().unwrap_or_default();
            vec![
                Cell::Text(meta.name.clone().unwrap_or_default()),
                Cell::Text(meta.namespace.clone().unwrap_or_default()),
                Cell::Text(format!("v{version} {turned} {:?}", self.0.locale())),
                Cell::Age(meta.creation_timestamp.as_ref().map(|time| time.0)),
            ]
        }

        fn cells_turn_at(&self, object: &DynamicObject) -> Option<Timestamp> {
            Timestamp::from_second(turn_of(object)?).ok()
        }

        fn details(&self, _: &DynamicObject) -> Vec<DetailSection> {
            Vec::new()
        }

        fn relations(&self, _: &DynamicObject) -> Vec<Relation> {
            Vec::new()
        }

        fn health(&self, _: &DynamicObject) -> Health {
            Health::unknown()
        }
    }

    fn thing(
        name: &str,
        namespace: Option<&str>,
        version: u64,
        turn: Option<i64>,
    ) -> Arc<DynamicObject> {
        let mut value = json!({
            "apiVersion": "v1",
            "kind": "Thing",
            "metadata": {
                "name": name,
                "resourceVersion": version.to_string(),
                "creationTimestamp": "2026-10-01T00:00:00Z"
            }
        });
        if let Some(namespace) = namespace {
            value["metadata"]["namespace"] = json!(namespace);
        }
        if let Some(turn) = turn {
            value["metadata"]["annotations"] = json!({ "turn": turn.to_string() });
        }
        Arc::new(serde_json::from_value(value).unwrap())
    }

    fn usage(cpu: u64, memory: u64) -> Usage {
        Usage { cpu, memory }
    }

    fn pod_usage(samples: &[(&str, &str, Usage)]) -> MetricsState {
        let samples = samples.iter().map(|(namespace, name, usage)| PodMetrics {
            name: (*name).into(),
            namespace: (*namespace).into(),
            timestamp: None,
            window: None,
            containers: vec![ContainerMetrics {
                name: "app".into(),
                usage: *usage,
            }],
        });
        MetricsState::Ready(MetricsIndex::from_pods(samples))
    }

    /// A list as the browser shows it: its objects, usage and search, and
    /// the cache of its rows.
    struct List {
        world: Arc<World>,
        plugin: Arc<dyn ResourcePlugin>,
        hidden: Option<usize>,
        /// `None` for a kind without usage columns.
        usage: Option<MetricsState>,
        revision: u64,
        objects: Vec<Arc<DynamicObject>>,
        query: String,
        cache: RowCache,
    }

    impl List {
        fn new(hidden: Option<usize>, usage: Option<MetricsState>) -> Self {
            let world = Arc::new(World::default());
            world.now.store(1_000, Ordering::Relaxed);
            let plugin: Arc<dyn ResourcePlugin> = Arc::new(Stub(world.clone()));
            Self::of(world, plugin, hidden, usage)
        }

        fn of(
            world: Arc<World>,
            plugin: Arc<dyn ResourcePlugin>,
            hidden: Option<usize>,
            usage: Option<MetricsState>,
        ) -> Self {
            let builder = RowBuilder::new(plugin.clone(), hidden);
            Self {
                cache: RowCache::new(builder, usage.is_some()),
                world,
                plugin,
                hidden,
                usage,
                revision: 0,
                objects: Vec::new(),
                query: String::new(),
            }
        }

        fn set_usage(&mut self, usage: MetricsState) {
            self.usage = Some(usage);
            self.revision += 1;
        }

        fn advance(&self, seconds: i64) {
            self.world.now.fetch_add(seconds, Ordering::Relaxed);
        }

        fn inputs(&self) -> RowInputs<'_> {
            RowInputs {
                usage: self.usage.as_ref().map(|state| UsageView {
                    state,
                    revision: self.revision,
                }),
                now: self.world.now(),
                locale: self.world.locale(),
            }
        }

        /// Brings the cache up to date; returns how many times the plugin
        /// built cells for it.
        fn update(&mut self) -> usize {
            let before = self.world.built();
            let built = self.update_at(self.world.now(), self.world.locale());
            assert_eq!(built, self.world.built() - before);
            built
        }

        fn update_at(&mut self, now: i64, locale: Locale) -> usize {
            let inputs = RowInputs {
                usage: self.usage.as_ref().map(|state| UsageView {
                    state,
                    revision: self.revision,
                }),
                now,
                locale,
            };
            self.cache.update(&self.objects, &inputs)
        }

        fn shared(&self) -> Vec<Arc<ResourceRow>> {
            self.cache.rows(&self.query)
        }

        fn shown(&self) -> Vec<ResourceRow> {
            self.shared().iter().map(|row| (**row).clone()).collect()
        }

        /// Every row built from scratch, as the browser did before rows
        /// were kept.
        fn rebuilt(&self) -> Vec<ResourceRow> {
            let matching = self.objects.iter().filter(|object| {
                name_matches(
                    object.metadata.name.as_deref().unwrap_or_default(),
                    &self.query,
                )
            });
            matching
                .map(|object| {
                    let mut row = row(self.plugin.as_ref(), object);
                    if let Some(ix) = self.hidden.filter(|&ix| ix < row.cells.len()) {
                        row.cells.remove(ix);
                    }
                    if let Some(state) = &self.usage {
                        row.cells
                            .extend(usage::usage_cells(usage::usage_of(state, object)));
                    }
                    row
                })
                .collect()
        }

        fn check(&mut self) {
            self.update();
            assert_eq!(self.shown(), self.rebuilt());
            let inputs = self.inputs();
            assert!(self.cache.missing(&self.objects, &inputs).is_empty());
            assert!(!self.cache.due(inputs.now));
        }
    }

    fn same_allocations(a: &[Arc<ResourceRow>], b: &[Arc<ResourceRow>]) -> Vec<bool> {
        a.iter().zip(b).map(|(a, b)| Arc::ptr_eq(a, b)).collect()
    }

    #[test]
    fn only_rows_of_changed_objects_are_built() {
        let mut list = List::new(None, None);
        list.objects = vec![
            thing("a", Some("web"), 1, None),
            thing("b", Some("web"), 1, None),
            thing("c", Some("web"), 1, None),
        ];
        assert_eq!(list.update(), 3);
        let first = list.shared();
        assert_eq!(list.shown(), list.rebuilt());
        // The same list again, also as another vector of the same objects.
        assert_eq!(list.update(), 0);
        list.objects = list.objects.clone();
        assert_eq!(list.update(), 0);
        assert_eq!(same_allocations(&first, &list.shared()), [true; 3]);

        // A new version of one object.
        list.objects[1] = thing("b", Some("web"), 2, None);
        assert_eq!(list.update(), 1);
        assert_eq!(
            same_allocations(&first, &list.shared()),
            [true, false, true]
        );
        list.check();

        // An equal object in another allocation is another object.
        list.objects[0] = Arc::new((*list.objects[0]).clone());
        assert_eq!(list.update(), 1);
        assert_eq!(list.shown(), list.rebuilt());
    }

    #[test]
    fn added_removed_and_moved_objects_keep_the_other_rows() {
        let mut list = List::new(None, None);
        list.objects = (0..6)
            .map(|ix| thing(&format!("t{ix}"), Some("web"), 1, None))
            .collect();
        assert_eq!(list.update(), 6);
        // Removed from the middle and from the end.
        list.objects.remove(2);
        list.objects.pop();
        assert_eq!(list.update(), 0);
        list.check();
        // Added at the start, in the middle and at the end.
        list.objects.insert(0, thing("first", Some("web"), 1, None));
        list.objects
            .insert(3, thing("middle", Some("web"), 1, None));
        list.objects.push(thing("last", Some("web"), 1, None));
        assert_eq!(list.update(), 3);
        list.check();
        // Another order (a merge of namespaces that came in another one).
        list.objects.reverse();
        assert_eq!(list.update(), 0);
        list.check();
        // Listed twice: one row is kept, the other built.
        list.objects.push(list.objects[0].clone());
        assert_eq!(list.update(), 1);
        list.check();
        // Nothing left, then everything again.
        let objects = std::mem::take(&mut list.objects);
        assert_eq!(list.update(), 0);
        assert!(list.shown().is_empty());
        list.objects = objects;
        assert_eq!(list.update(), list.objects.len());
        list.check();
    }

    #[test]
    fn a_metrics_poll_replaces_only_usage_cells_that_changed() {
        let mut list = List::new(None, Some(MetricsState::Loading));
        list.objects = vec![
            thing("a", Some("web"), 1, None),
            thing("b", Some("web"), 1, None),
            thing("c", Some("web"), 1, None),
        ];
        assert_eq!(list.update(), 3);
        // Dashes while loading.
        assert_eq!(list.shown(), list.rebuilt());
        let loading = list.shared();

        list.set_usage(pod_usage(&[
            ("web", "a", usage(100_000_000, 1 << 20)),
            ("web", "b", usage(200_000_000, 2 << 20)),
        ]));
        assert_eq!(list.update(), 0);
        assert_eq!(list.shown(), list.rebuilt());
        let polled = list.shared();
        // `c` has no sample: still dashes, the same row.
        assert_eq!(same_allocations(&loading, &polled), [false, false, true]);
        let plugin_cells = |row: &ResourceRow| row.cells[..4].to_vec();
        for (before, after) in loading.iter().zip(&polled) {
            assert_eq!(plugin_cells(before), plugin_cells(after));
        }

        // Another poll: `a` uses the same, `b` more, `c` got a sample.
        list.set_usage(pod_usage(&[
            ("web", "a", usage(100_000_000, 1 << 20)),
            ("web", "b", usage(250_000_000, 2 << 20)),
            ("web", "c", usage(1, 1)),
        ]));
        assert_eq!(list.update(), 0);
        assert_eq!(list.shown(), list.rebuilt());
        assert_eq!(
            same_allocations(&polled, &list.shared()),
            [true, false, false]
        );
        // The same state again is not looked at.
        let shown = list.shared();
        assert_eq!(list.update(), 0);
        assert_eq!(same_allocations(&shown, &list.shared()), [true; 3]);

        // Metrics went away: dashes again.
        list.set_usage(MetricsState::Unavailable);
        assert_eq!(list.update(), 0);
        assert_eq!(list.shown(), list.rebuilt());
        // A changed object gets the usage of the current poll.
        list.set_usage(pod_usage(&[("web", "b", usage(5, 5))]));
        list.objects[1] = thing("b", Some("web"), 2, None);
        assert_eq!(list.update(), 1);
        assert_eq!(list.shown(), list.rebuilt());
    }

    #[test]
    fn nodes_get_usage_by_name_and_a_hidden_column_stays_hidden() {
        let nodes = MetricsIndex::from_nodes([NodeMetrics {
            name: "n1".into(),
            timestamp: None,
            window: None,
            usage: usage(3_000_000_000, 8 << 30),
        }]);
        let mut list = List::new(Some(1), Some(MetricsState::Ready(nodes)));
        list.objects = vec![thing("n1", None, 1, None), thing("n2", None, 1, None)];
        list.check();
        let rows = list.shown();
        // Name, state, age, CPU, Memory.
        assert_eq!(rows[0].cells.len(), 5);
        assert_eq!(rows[0].cells[3].label().as_ref(), "3.000");
        assert_eq!(rows[1].cells[3].label().as_ref(), "—");
        list.set_usage(MetricsState::Loading);
        list.check();
        assert_eq!(list.shown()[0].cells.len(), 5);
    }

    #[test]
    fn a_row_is_built_again_when_its_moment_comes() {
        let mut list = List::new(None, None);
        let now = list.world.now();
        list.objects = vec![
            thing("steady", Some("web"), 1, None),
            thing("soon", Some("web"), 1, Some(now + 30)),
            thing("later", Some("web"), 1, Some(now + 60)),
            thing("past", Some("web"), 1, Some(now - 5)),
        ];
        assert_eq!(list.update(), 4);
        // The clock alone builds nothing before a moment.
        assert!(!list.cache.due(list.world.now()));
        list.advance(29);
        assert!(!list.cache.due(list.world.now()));
        assert_eq!(list.update(), 0);
        assert_eq!(list.shown(), list.rebuilt());

        list.advance(1);
        assert!(list.cache.due(list.world.now()));
        let before = list.shared();
        assert_eq!(list.update(), 1);
        assert_eq!(
            same_allocations(&before, &list.shared()),
            [true, false, true, true]
        );
        list.check();

        // Far past both moments: the one left turns, the turned one stays.
        list.advance(1_000);
        assert!(list.cache.due(list.world.now()));
        assert_eq!(list.update(), 1);
        list.check();
        list.advance(1_000);
        assert_eq!(list.update(), 0);

        // A new version with a new moment is waited for again.
        let now = list.world.now();
        list.objects[0] = thing("steady", Some("web"), 2, Some(now + 10));
        assert_eq!(list.update(), 1);
        assert!(!list.cache.due(now + 9));
        assert!(list.cache.due(now + 10));
    }

    #[test]
    fn another_language_builds_every_row() {
        let mut list = List::new(None, Some(MetricsState::Loading));
        list.objects = vec![
            thing("a", Some("web"), 1, None),
            thing("b", Some("web"), 1, None),
        ];
        assert_eq!(list.update(), 2);
        list.world.language.store(1, Ordering::Relaxed);
        assert_eq!(list.update(), 2);
        assert_eq!(list.shown(), list.rebuilt());
        assert_eq!(list.update(), 0);
        list.world.language.store(0, Ordering::Relaxed);
        assert_eq!(list.update(), 2);
        list.check();
    }

    #[test]
    fn the_search_filters_built_rows() {
        let mut list = List::new(None, None);
        list.objects = vec![
            thing("api-1", Some("web"), 1, None),
            thing("worker", Some("web"), 1, None),
            thing("API-2", Some("jobs"), 1, None),
        ];
        assert_eq!(list.update(), 3);
        let all = list.shared();
        for query in ["api", " Api ", "worker", "web", "", "-"] {
            list.query = query.into();
            assert_eq!(list.update(), 0, "{query}");
            assert_eq!(list.shown(), list.rebuilt(), "{query}");
        }
        list.query = "api".into();
        let found = list.shared();
        assert_eq!(found.len(), 2);
        assert!(Arc::ptr_eq(&found[0], &all[0]) && Arc::ptr_eq(&found[1], &all[2]));
    }

    #[test]
    fn rows_built_ahead_are_used_while_they_stand() {
        let mut list = List::new(None, None);
        let now = list.world.now();
        list.objects = vec![
            thing("a", Some("web"), 1, None),
            thing("b", Some("web"), 1, Some(now + 10)),
            thing("c", Some("web"), 1, None),
        ];
        let missing = list.cache.missing(&list.objects, &list.inputs());
        assert_eq!(missing.len(), 3);
        let build = |list: &List, objects: &[Arc<DynamicObject>]| -> Vec<BuiltRow> {
            let builder = list.cache.builder();
            let (now, locale) = (list.world.now(), list.world.locale());
            objects
                .iter()
                .map(|object| builder.build(object, now, locale))
                .collect()
        };
        let ahead = build(&list, &missing);
        assert_eq!(list.world.built(), 3);
        list.cache.adopt(ahead);
        assert!(list.cache.missing(&list.objects, &list.inputs()).is_empty());
        // Meanwhile one object changed and one was removed.
        list.objects[0] = thing("a", Some("web"), 2, None);
        list.objects.pop();
        let missing = list.cache.missing(&list.objects, &list.inputs());
        assert_eq!(missing.len(), 1);
        assert!(Arc::ptr_eq(&missing[0], &list.objects[0]));
        assert_eq!(list.update(), 1);
        assert_eq!(list.shown(), list.rebuilt());
        assert_eq!(list.world.built(), 4 + 2);

        // Built ahead before the moment of `b`, used after it: built again.
        let fresh = thing("b", Some("web"), 2, Some(now + 10));
        list.objects[1] = fresh.clone();
        let ahead = build(&list, &[fresh]);
        list.cache.adopt(ahead);
        list.advance(10);
        assert_eq!(list.cache.missing(&list.objects, &list.inputs()).len(), 1);
        assert_eq!(list.update(), 1);
        list.check();

        // Built ahead in another language: built again.
        let fresh = thing("c", Some("web"), 1, None);
        list.objects.push(fresh.clone());
        let ahead = build(&list, &[fresh]);
        list.cache.adopt(ahead);
        list.world.language.store(2, Ordering::Relaxed);
        assert_eq!(list.update(), 3);
        list.check();
    }

    #[test]
    fn many_rows_are_left_to_a_job_and_few_built_at_once() {
        let mut list = List::new(None, Some(MetricsState::Loading));
        let many = OFF_THREAD_ROWS + 40;
        list.objects = (0..many)
            .map(|ix| thing(&format!("t{ix}"), Some("web"), 1, None))
            .collect();
        // The process speaks the language the job builds in.
        let inputs = |list: &List| RowInputs {
            usage: None,
            now: list.world.now(),
            locale: kubach_i18n::locale(),
        };
        let job = list
            .cache
            .update_or_job(&list.objects, &inputs(&list))
            .expect("too many rows for one update");
        // Nothing was built or shown yet.
        assert_eq!(list.world.built(), 0);
        assert!(list.cache.is_empty() && list.cache.rows("").is_empty());

        // Built in parts, on whatever threads.
        let parts = job.parts(100);
        assert_eq!(parts.len(), many.div_ceil(100));
        let threads: Vec<_> = parts.into_iter().map(std::thread::spawn).collect();
        let rows: Vec<BuiltRow> = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(rows.len(), many);
        assert_eq!(list.world.built(), many);

        // Meanwhile the list changed a little.
        let listed = std::mem::take(&mut list.objects);
        let mut now = listed.clone();
        now[3] = thing("t3", Some("web"), 2, None);
        now.remove(10);
        now.push(thing("new", Some("web"), 1, None));
        // The job's rows are shown for its list, without building any.
        assert_eq!(list.cache.finish(job, rows, &inputs(&list)), 0);
        assert_eq!(list.world.built(), many);
        list.objects = listed;
        list.world
            .language
            .store(kubach_i18n::locale() as usize, Ordering::Relaxed);
        assert_eq!(list.shown(), list.rebuilt());
        // The rest is little: built at once.
        list.objects = now;
        let before = list.world.built();
        assert!(
            list.cache
                .update_or_job(&list.objects, &inputs(&list))
                .is_none()
        );
        assert_eq!(list.world.built() - before, 2);
        assert_eq!(list.shown(), list.rebuilt());

        // Exactly the limit is still built at once.
        let mut list = List::new(None, None);
        list.objects = (0..OFF_THREAD_ROWS)
            .map(|ix| thing(&format!("t{ix}"), None, 1, None))
            .collect();
        assert!(
            list.cache
                .update_or_job(&list.objects, &inputs(&list))
                .is_none()
        );
        assert_eq!(list.world.built(), OFF_THREAD_ROWS);
    }

    #[test]
    fn the_rows_of_pods_are_those_built_from_scratch() {
        use kubach_core::perf::{self, PodShape};

        let plugin = PluginRegistry::with_builtins().plugin_for("", "Pod");
        let pods = perf::shared(perf::pods(60, 3, PodShape::SLIM));
        let sample = |pod: &Arc<DynamicObject>, cpu| PodMetrics {
            name: pod.metadata.name.clone().unwrap(),
            namespace: pod.metadata.namespace.clone().unwrap(),
            timestamp: None,
            window: None,
            containers: vec![ContainerMetrics {
                name: "app".into(),
                usage: usage(cpu, 64 << 20),
            }],
        };
        let namespace = namespace_column(plugin.as_ref());
        for hidden in [None, namespace] {
            let world = Arc::new(World::default());
            world.now.store(unix_now(), Ordering::Relaxed);
            let mut list = List::of(world, plugin.clone(), hidden, Some(MetricsState::Loading));
            list.objects = pods.clone();
            list.update_at(unix_now(), kubach_i18n::locale());
            assert_eq!(list.shown(), list.rebuilt());
            let samples = pods.iter().step_by(2).map(|pod| sample(pod, 250_000_000));
            list.set_usage(MetricsState::Ready(MetricsIndex::from_pods(samples)));
            let mut changed = (*pods[7]).clone();
            perf::touch(&mut changed, 77);
            list.objects[7] = Arc::new(changed);
            list.objects.remove(20);
            list.query = "app-000".into();
            assert_eq!(list.update_at(unix_now(), kubach_i18n::locale()), 1);
            assert_eq!(list.shown(), list.rebuilt());
            assert!(!list.shown().is_empty());
        }
    }

    /// A small generator of repeatable "random" numbers.
    struct Dice(u64);

    impl Dice {
        fn roll(&mut self, sides: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % sides.max(1) as u64) as usize
        }

        fn chance(&mut self, percent: usize) -> bool {
            self.roll(100) < percent
        }
    }

    /// Whatever happens to a list, in whatever order, the kept rows are
    /// those a build from scratch gives.
    #[test]
    fn kept_rows_equal_rebuilt_rows_after_random_changes() {
        const NAMES: [&str; 8] = ["api", "API-b", "worker", "db", "cache", "b1", "web", "job"];
        const QUERIES: [&str; 7] = ["", "a", "b1", "API", " w ", "zz", "-"];
        for seed in 1..=60u64 {
            let mut dice = Dice(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let namespaced = dice.chance(70);
            let hidden = [None, Some(1), Some(0), Some(9)][dice.roll(4)];
            let usage_columns = dice.chance(60).then_some(MetricsState::Loading);
            let mut list = List::new(hidden, usage_columns);
            let mut next_version = 1;
            let mut new_thing = |dice: &mut Dice, now: i64, name: Option<String>| {
                next_version += 1;
                let name = name.unwrap_or_else(|| {
                    format!("{}-{}", NAMES[dice.roll(NAMES.len())], dice.roll(6))
                });
                let namespace = namespaced.then(|| ["web", "jobs"][dice.roll(2)]);
                let turn = dice.chance(40).then(|| now + dice.roll(12) as i64 - 3);
                thing(&name, namespace, next_version, turn)
            };
            let mut ahead: Vec<BuiltRow> = Vec::new();
            for step in 0..400 {
                let now = list.world.now();
                let count = list.objects.len();
                let at = dice.roll(count);
                match dice.roll(14) {
                    0 | 1 if count < 24 => {
                        let object = new_thing(&mut dice, now, None);
                        list.objects.insert(dice.roll(count + 1), object);
                    }
                    2 | 3 if count > 0 => {
                        let name = list.objects[at].metadata.name.clone();
                        list.objects[at] = new_thing(&mut dice, now, name);
                    }
                    4 if count > 0 => {
                        list.objects.remove(at);
                    }
                    5 if count > 1 => list.objects.swap(at, dice.roll(count)),
                    // An equal object in another allocation.
                    6 if count > 0 => list.objects[at] = Arc::new((*list.objects[at]).clone()),
                    7 if count > 0 && dice.chance(20) => {
                        let again = list.objects[at].clone();
                        list.objects.push(again);
                    }
                    8 if list.usage.is_some() => {
                        let state = match dice.roll(6) {
                            0 => MetricsState::Loading,
                            1 => MetricsState::Unavailable,
                            _ => {
                                let mut sampled = Vec::new();
                                for object in &list.objects {
                                    if dice.chance(70) {
                                        let usage = usage(
                                            dice.roll(3) as u64 * 100_000_000,
                                            dice.roll(3) as u64 * (64 << 20),
                                        );
                                        let meta = &object.metadata;
                                        sampled.push((
                                            meta.namespace.clone(),
                                            meta.name.clone().unwrap(),
                                            usage,
                                        ));
                                    }
                                }
                                let pods = sampled.iter().filter_map(|(namespace, name, usage)| {
                                    Some(PodMetrics {
                                        name: name.clone(),
                                        namespace: namespace.clone()?,
                                        timestamp: None,
                                        window: None,
                                        containers: vec![ContainerMetrics {
                                            name: "app".into(),
                                            usage: *usage,
                                        }],
                                    })
                                });
                                let mut index = MetricsIndex::from_pods(pods);
                                if !namespaced {
                                    let nodes =
                                        sampled.iter().map(|(_, name, usage)| NodeMetrics {
                                            name: name.clone(),
                                            timestamp: None,
                                            window: None,
                                            usage: *usage,
                                        });
                                    index = MetricsIndex::from_nodes(nodes);
                                }
                                MetricsState::Ready(index)
                            }
                        };
                        list.set_usage(state);
                    }
                    9 => list.advance(dice.roll(5) as i64),
                    10 if dice.chance(15) => {
                        list.world.language.store(dice.roll(3), Ordering::Relaxed);
                    }
                    11 => list.query = QUERIES[dice.roll(QUERIES.len())].into(),
                    // Rows built ahead for some of the missing objects,
                    // taken now or after more changes.
                    12 => {
                        let inputs = list.inputs();
                        let missing = list.cache.missing(&list.objects, &inputs);
                        let builder = list.cache.builder();
                        for object in &missing {
                            if dice.chance(70) {
                                ahead.push(builder.build(object, inputs.now, inputs.locale));
                            }
                        }
                    }
                    13 => list.cache.adopt(ahead.drain(..)),
                    _ => {}
                }
                if dice.chance(60) {
                    list.update();
                    assert_eq!(list.shown(), list.rebuilt(), "seed {seed}, step {step}");
                    let inputs = list.inputs();
                    assert!(list.cache.missing(&list.objects, &inputs).is_empty());
                    assert!(!list.cache.due(inputs.now), "seed {seed}, step {step}");
                }
            }
        }
    }
}
