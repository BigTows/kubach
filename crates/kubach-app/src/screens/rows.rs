//! Converts plugin data (columns, cells, health) into `kubach-ui` table rows.

use gpui_kit::px;
use kubach_core::plugin::{
    Cell, Column, ColumnKind, ContainerBrief, ContainerHealth, ContainerRole, Health, HealthStatus,
    ResourcePlugin, not_ready_count,
};
use kubach_ui::{
    ContainerCubeState, ResourceCell, ResourceColumn, ResourceRow, StatusCube, Tone,
    container_tooltip,
};
use kube::api::DynamicObject;

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
