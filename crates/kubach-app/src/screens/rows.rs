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
    pub fn build_detached(&self, object: &Arc<DynamicObject>) -> Option<BuiltRow> {
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
    /// build: to build them off the UI thread first when they are many
    /// ([`adopt`](Self::adopt)).
    pub fn missing(
        &self,
        objects: &[Arc<DynamicObject>],
        inputs: &RowInputs,
    ) -> Vec<Arc<DynamicObject>> {
        let standing = self.standing(objects, inputs);
        let missing = objects.iter().zip(standing).filter(|(object, entry)| {
            entry.is_none()
                && !self
                    .prebuilt
                    .get(&address(object))
                    .is_some_and(|built| built.locale == inputs.locale)
        });
        missing.map(|(object, _)| object.clone()).collect()
    }

    /// Takes rows built off the UI thread: the next
    /// [`update`](Self::update) uses those whose object is still listed
    /// and drops the rest.
    pub fn adopt(&mut self, rows: impl IntoIterator<Item = BuiltRow>) {
        let rows = rows.into_iter().map(|row| (address(&row.object), row));
        self.prebuilt.extend(rows);
    }

    /// Brings the rows to `objects` with `inputs`; returns how many rows
    /// were built.
    pub fn update(&mut self, objects: &[Arc<DynamicObject>], inputs: &RowInputs) -> usize {
        let standing = self.standing(objects, inputs);
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
            built += 1;
            let ready = prebuilt
                .remove(&address(object))
                .filter(|row| row.locale == inputs.locale);
            let BuiltRow {
                object,
                mut row,
                built_at,
                turns_at,
                ..
            } = ready.unwrap_or_else(|| self.builder.build(object, inputs.now, inputs.locale));
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
}
