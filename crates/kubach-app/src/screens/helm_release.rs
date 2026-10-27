//! Pure pieces of the Helm screen: table rows, the texts of the detail tabs
//! and the revision pair a diff starts with.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString};
use gpui_kit::{StatefulInteractiveElement as _, Styled as _, div, px};
use kubach_core::helm::diagnose::missing_kinds;
use kubach_core::helm::updates::ChartUpdate;
use kubach_core::helm::{Hook, Release, ReleaseStatus, ReleaseSummary, values_to_yaml};
use kubach_core::names::NameError;
use kubach_core::validation::{Field, ValidationError};
use kubach_i18n::t;
use kubach_ui::{ResourceCell, ResourceColumn, ResourceRow, StatusBadge, Tone};

pub fn status_tone(status: &ReleaseStatus) -> Tone {
    match status {
        ReleaseStatus::Deployed => Tone::Success,
        ReleaseStatus::Failed => Tone::Danger,
        status if status.is_pending() => Tone::Warning,
        _ => Tone::Neutral,
    }
}

/// Row identity of a release: `namespace/name`.
pub fn release_key(namespace: &str, name: &str) -> String {
    format!("{namespace}/{name}")
}

pub fn release_columns(show_namespace: bool) -> Vec<ResourceColumn> {
    let mut columns = vec![ResourceColumn::new("name", t!("helm-column-name")).width(px(260.))];
    if show_namespace {
        columns.push(ResourceColumn::new("namespace", t!("helm-column-namespace")).width(px(160.)));
    }
    columns.extend([
        ResourceColumn::new("chart", t!("helm-field-chart")).width(px(220.)),
        ResourceColumn::new("app-version", t!("helm-field-app-version")).width(px(120.)),
        ResourceColumn::new("revision", t!("helm-field-revision"))
            .width(px(90.))
            .align_end(),
        ResourceColumn::new("status", t!("helm-field-status")).width(px(150.)),
        ResourceColumn::new("updated", t!("helm-field-updated"))
            .width(px(90.))
            .align_end(),
    ]);
    columns.into_iter().map(ResourceColumn::sortable).collect()
}

/// `↑ 1.2.3`, the badge of a newer chart version.
pub fn update_badge_label(update: &ChartUpdate) -> String {
    format!("↑ {}", update.version)
}

/// What a newer chart version is: `New version 1.2.3 (app 4.5) available
/// in repo X`.
pub fn update_text(update: &ChartUpdate) -> String {
    match update.app_version.as_deref() {
        Some(app) => t!(
            "helm-update-available-app",
            version = update.version.clone(),
            app = app,
            repository = update.repository.clone()
        ),
        None => t!(
            "helm-update-available",
            version = update.version.clone(),
            repository = update.repository.clone()
        ),
    }
}

/// A `helm list` row; the namespace cell only with `show_namespace`. The
/// chart cell carries a badge when `update` names a newer version.
pub fn release_row(
    release: &ReleaseSummary,
    show_namespace: bool,
    update: Option<&ChartUpdate>,
) -> ResourceRow {
    let mut cells = vec![ResourceCell::text(release.name.clone())];
    if show_namespace {
        cells.push(ResourceCell::text(release.namespace.clone()));
    }
    let mut chart = super::helm_charts::chart_cell(
        release.chart.reference(),
        release.chart.name.clone(),
        release.chart.icon.clone(),
    );
    if let Some(update) = update {
        let label = update_badge_label(update);
        let tooltip = SharedString::from(update_text(update));
        let id = SharedString::from(format!(
            "helm-update-{}",
            release_key(&release.namespace, &release.name)
        ));
        chart = chart.trailing(move |_, _| {
            let tooltip = tooltip.clone();
            div()
                .id(id.clone())
                .flex_none()
                .child(StatusBadge::new(label.clone(), Tone::Info))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .into_any_element()
        });
    }
    cells.extend([
        chart,
        text_or_dash(release.chart.app_version.as_deref()),
        ResourceCell::number(release.revision.into()),
        ResourceCell::status(
            release.status.as_str().to_owned(),
            status_tone(&release.status),
        ),
        age(release.updated.map(|at| at.as_second())),
    ]);
    ResourceRow::new(release_key(&release.namespace, &release.name), cells)
}

pub fn revision_columns() -> Vec<ResourceColumn> {
    vec![
        ResourceColumn::new("revision", t!("helm-field-revision"))
            .width(px(90.))
            .align_end()
            .sortable(),
        ResourceColumn::new("status", t!("helm-field-status")).width(px(150.)),
        ResourceColumn::new("chart", t!("helm-field-chart")).width(px(220.)),
        ResourceColumn::new("app-version", t!("helm-field-app-version")).width(px(120.)),
        ResourceColumn::new("updated", t!("helm-field-updated"))
            .width(px(90.))
            .align_end(),
        ResourceColumn::new("description", t!("helm-field-description")).width(px(360.)),
    ]
}

/// A `helm history` row, keyed by the revision number.
pub fn revision_row(release: &Release) -> ResourceRow {
    ResourceRow::new(
        release.revision.to_string(),
        [
            ResourceCell::number(release.revision.into()),
            ResourceCell::status(
                release.status.as_str().to_owned(),
                status_tone(&release.status),
            ),
            ResourceCell::text(release.chart.reference()),
            text_or_dash(release.chart.app_version.as_deref()),
            age(release.last_deployed.map(|at| at.as_second())),
            text_or_dash(Some(&release.description)),
        ],
    )
}

fn text_or_dash(text: Option<&str>) -> ResourceCell {
    match text {
        Some(text) if !text.is_empty() => ResourceCell::text(text.to_owned()),
        _ => ResourceCell::text("—").muted(),
    }
}

/// Live age cell of a time in Unix seconds.
fn age(at: Option<i64>) -> ResourceCell {
    match at {
        Some(at) => ResourceCell::age(at),
        None => ResourceCell::text("—").muted(),
    }
}

/// Tabs of the release details.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Values,
    ComputedValues,
    Manifest,
    Hooks,
    Notes,
    History,
    Diff,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::Values,
        Tab::ComputedValues,
        Tab::Manifest,
        Tab::Hooks,
        Tab::Notes,
        Tab::History,
        Tab::Diff,
    ];

    pub fn title(self) -> String {
        match self {
            Tab::Values => t!("helm-tab-values"),
            Tab::ComputedValues => t!("helm-tab-computed-values"),
            Tab::Manifest => t!("helm-tab-manifest"),
            Tab::Hooks => t!("helm-tab-hooks"),
            Tab::Notes => t!("helm-tab-notes"),
            Tab::History => t!("helm-tab-history"),
            Tab::Diff => t!("helm-tab-diff"),
        }
    }

    /// Shows data that may hold secrets, masked until revealed.
    pub fn sensitive(self) -> bool {
        matches!(
            self,
            Tab::Values | Tab::ComputedValues | Tab::Manifest | Tab::Hooks | Tab::Diff
        )
    }

    /// Shown as YAML in the read-only viewer.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            Tab::Values | Tab::ComputedValues | Tab::Manifest | Tab::Hooks
        )
    }
}

/// The viewer text of a text tab (empty for other tabs).
pub fn tab_text(release: &Release, tab: Tab) -> String {
    match tab {
        Tab::Values => values_to_yaml(release.user_values.expose()),
        Tab::ComputedValues => {
            let computed = release.computed_values();
            let warnings: String = computed
                .warnings
                .iter()
                .map(|warning| format!("# {}\n", t!("helm-values-warning", warning = warning)))
                .collect();
            warnings + &values_to_yaml(computed.values.expose())
        }
        Tab::Manifest => release.manifest.expose().trim_start().to_owned(),
        Tab::Hooks => hooks_text(&release.hooks),
        Tab::Notes => release.notes.clone(),
        Tab::History | Tab::Diff => String::new(),
    }
}

/// Hook manifests, one YAML document each, headed by when they run.
pub fn hooks_text(hooks: &[Hook]) -> String {
    if hooks.is_empty() {
        return format!("# {}\n", t!("helm-hooks-none"));
    }
    let mut hooks: Vec<&Hook> = hooks.iter().collect();
    hooks.sort_by(|a, b| (a.weight, &a.name).cmp(&(b.weight, &b.name)));
    hooks
        .into_iter()
        .map(|hook| {
            let mut header = format!(
                "---\n# {}\n",
                t!(
                    "helm-hook-header",
                    kind = hook.kind.clone(),
                    name = hook.name.clone(),
                    events = hook.events.join(", "),
                    weight = hook.weight.to_string()
                )
            );
            if !hook.delete_policies.is_empty() {
                header += &format!(
                    "# {}\n",
                    t!(
                        "helm-hook-delete-policy",
                        policies = hook.delete_policies.join(", ")
                    )
                );
            }
            if let Some(run) = &hook.last_run {
                header += &format!(
                    "# {}\n",
                    t!("helm-hook-last-run", phase = run.phase.clone())
                );
            }
            let manifest = hook.manifest.expose().trim();
            let manifest = manifest
                .strip_prefix("---")
                .unwrap_or(manifest)
                .trim_start();
            format!("{header}{manifest}\n")
        })
        .collect()
}

/// What a diff compares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DiffSource {
    #[default]
    Manifest,
    Values,
    ComputedValues,
}

impl DiffSource {
    pub const ALL: [DiffSource; 3] = [
        DiffSource::Manifest,
        DiffSource::Values,
        DiffSource::ComputedValues,
    ];

    pub fn title(self) -> String {
        match self {
            DiffSource::Manifest => t!("helm-tab-manifest"),
            DiffSource::Values => t!("helm-tab-values"),
            DiffSource::ComputedValues => t!("helm-tab-computed-values"),
        }
    }

    pub fn text(self, release: &Release) -> String {
        match self {
            DiffSource::Manifest => tab_text(release, Tab::Manifest),
            DiffSource::Values => values_to_yaml(release.user_values.expose()),
            DiffSource::ComputedValues => values_to_yaml(release.computed_values().values.expose()),
        }
    }
}

/// Revisions a diff starts with: the one before `to` against `to`, or the
/// latest two when `to` is the first. `None` with fewer than two revisions.
/// `history` is oldest first.
pub fn default_diff_pair(history: &[Release], to: Option<u32>) -> Option<(u32, u32)> {
    let revisions: Vec<u32> = history.iter().map(|release| release.revision).collect();
    let to_ix = match to {
        Some(to) => revisions.iter().position(|&r| r == to)?,
        None => revisions.len().checked_sub(1)?,
    };
    match to_ix {
        0 => (revisions.len() >= 2).then(|| (revisions[0], revisions[1])),
        ix => Some((revisions[ix - 1], revisions[ix])),
    }
}

/// Text of the uninstall confirmation: the delete hooks and an intro to the
/// resource list that follows it in the dialog (the command goes in its own
/// block).
pub fn uninstall_message(
    name: &str,
    namespace: &str,
    resources: usize,
    delete_hooks: usize,
) -> String {
    let mut message = String::new();
    if delete_hooks > 0 {
        message.push_str(&t!("helm-uninstall-delete-hooks", count = delete_hooks));
        message.push('\n');
    }
    message.push_str(&if resources == 0 {
        t!(
            "helm-uninstall-no-resources",
            name = name,
            namespace = namespace
        )
    } else {
        t!(
            "helm-uninstall-resources",
            name = name,
            namespace = namespace,
            count = resources
        )
    });
    message
}

/// Notice of the uninstall confirmation about the claims Helm leaves
/// behind (#273); `None` when there are none.
pub fn claims_notice(claims: &Result<Vec<String>, String>) -> Option<String> {
    match claims {
        Ok(claims) if claims.is_empty() => None,
        Ok(claims) => Some(t!("helm-claims-stay", count = claims.len())),
        Err(error) => Some(t!("helm-claims-check-failed", error = error.clone())),
    }
}

/// What a failed `helm` call means when the chart renders kinds the
/// cluster has no CRD for (#261); `None` for other failures.
pub fn missing_crds_text(output: &str) -> Option<String> {
    let kinds = missing_kinds(output);
    if kinds.is_empty() {
        return None;
    }
    let labels: Vec<String> = kinds.iter().map(|kind| kind.label()).collect();
    Some(t!(
        "helm-missing-crds",
        kinds = labels.join(", "),
        count = labels.len()
    ))
}

/// A validation error of the Helm forms (repository, release, namespace,
/// values, stored chart) in the interface language; other errors keep
/// their own text.
pub fn validation_text(error: &ValidationError) -> String {
    use ValidationError as V;
    match error {
        V::Empty(Field::RepositoryName) => t!("helm-invalid-repository-name-empty"),
        V::InvalidChars {
            field: Field::RepositoryName,
            ..
        } => t!("helm-invalid-repository-name-chars"),
        V::Empty(Field::ReleaseName)
        | V::Name {
            field: Field::ReleaseName,
            error: NameError::Empty,
        } => t!("helm-invalid-release-empty"),
        V::Name {
            field: Field::ReleaseName,
            error: NameError::TooLong { .. },
        } => t!(
            "helm-invalid-release-too-long",
            max = kubach_core::helm::cli::MAX_RELEASE_NAME
        ),
        V::Name {
            field: Field::ReleaseName,
            ..
        } => t!("helm-invalid-release-chars"),
        V::Empty(Field::Namespace)
        | V::Name {
            field: Field::Namespace,
            error: NameError::Empty,
        } => t!("helm-invalid-namespace-empty"),
        V::Name {
            field: Field::Namespace,
            ..
        } => t!("helm-invalid-namespace-chars"),
        V::OciRepository => t!("helm-invalid-oci-repository"),
        V::UrlScheme => t!("helm-invalid-url-scheme"),
        V::InvalidUrl => t!("helm-invalid-url"),
        V::InsecureCredentials => t!("helm-invalid-insecure-credentials"),
        V::Yaml {
            line: Some(line),
            message,
            ..
        } => t!(
            "helm-invalid-yaml-line",
            line = *line,
            message = message.clone()
        ),
        V::MultipleDocuments(_) => t!("helm-invalid-values-documents"),
        V::NotMapping { line, .. } => t!("helm-invalid-values-mapping", line = *line),
        V::NoChartMetadata => t!("helm-invalid-no-chart-metadata"),
        V::Subcharts => t!("helm-invalid-subcharts"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::helm::{HookRun, Sensitive, decode_release, encode_release};
    use serde_json::json;

    use super::{
        DiffSource, Tab, claims_notice, default_diff_pair, hooks_text, release_row, revision_row,
        status_tone, tab_text, uninstall_message, update_badge_label, update_text,
    };
    use kubach_core::helm::updates::ChartUpdate;

    use kubach_core::helm::{Hook, Release, ReleaseStatus};
    use kubach_ui::Tone;

    #[test]
    fn uninstall_message_counts_resources_and_hooks() {
        let message = uninstall_message("web", "apps", 3, 2);
        assert_eq!(
            message,
            "The chart's 2 delete hooks run first.\n\
             Helm deletes release web in apps and its 3 resources:"
        );
        let one = uninstall_message("web", "apps", 1, 1);
        assert!(one.contains("delete hook runs first"), "{one}");
        assert!(one.ends_with("and its resource:"), "{one}");
        assert!(uninstall_message("web", "apps", 0, 0).ends_with("no resources."));
    }

    #[test]
    fn claims_notice_names_what_stays() {
        assert_eq!(claims_notice(&Ok(Vec::new())), None);
        let one = claims_notice(&Ok(vec!["data-db-0".into()])).unwrap();
        assert!(one.starts_with("This PersistentVolumeClaim stays"), "{one}");
        let two = claims_notice(&Ok(vec!["a".into(), "b".into()])).unwrap();
        assert!(
            two.starts_with("These 2 PersistentVolumeClaims stay"),
            "{two}"
        );
        let failed = claims_notice(&Err("forbidden".into())).unwrap();
        assert!(failed.ends_with(": forbidden"), "{failed}");
    }

    fn release(revision: u32, replicas: u32) -> Release {
        let json = json!({
            "name": "web",
            "namespace": "apps",
            "version": revision,
            "info": {
                "status": "deployed",
                "last_deployed": "2026-10-01T00:00:00Z",
                "description": "Upgrade complete",
                "notes": "Visit http://web"
            },
            "chart": {
                "metadata": {"name": "nginx", "version": "1.2.0", "appVersion": "1.27"},
                "values": {"replicaCount": 1, "image": {"tag": "stable"}}
            },
            "config": {"replicaCount": replicas},
            "manifest": format!("---\napiVersion: apps/v1\nkind: Deployment\nspec:\n  replicas: {replicas}\n"),
            "hooks": []
        });
        decode_release(encode_release(&json).as_bytes()).unwrap()
    }

    fn hook(name: &str, weight: i64) -> Hook {
        Hook {
            name: name.into(),
            kind: "Job".into(),
            path: format!("nginx/templates/{name}.yaml"),
            events: vec!["pre-install".into(), "pre-upgrade".into()],
            weight,
            delete_policies: vec!["hook-succeeded".into()],
            last_run: Some(HookRun {
                started_at: None,
                completed_at: None,
                phase: "Succeeded".into(),
            }),
            manifest: Sensitive::new(format!("---\nkind: Job\nmetadata:\n  name: {name}\n")),
        }
    }

    #[test]
    fn statuses_have_tones() {
        assert_eq!(status_tone(&ReleaseStatus::Deployed), Tone::Success);
        assert_eq!(status_tone(&ReleaseStatus::Failed), Tone::Danger);
        assert_eq!(status_tone(&ReleaseStatus::PendingUpgrade), Tone::Warning);
        assert_eq!(status_tone(&ReleaseStatus::Superseded), Tone::Neutral);
    }

    #[test]
    fn rows_follow_helm_list_and_history() {
        let release = release(3, 2);
        let deployed = release.last_deployed.unwrap().as_second();
        let row = release_row(&release.summary(), true, None);
        assert_eq!(row.key.as_ref(), "apps/web");
        let labels: Vec<_> = row.cells.iter().map(|c| c.label().to_string()).collect();
        let age = kubach_ui::format_since(deployed, kubach_ui::unix_now());
        assert_eq!(
            labels,
            [
                "web",
                "apps",
                "nginx-1.2.0",
                "1.27",
                "3",
                "deployed",
                age.as_str()
            ]
        );
        assert_eq!(release_row(&release.summary(), false, None).cells.len(), 6);
        let update = ChartUpdate {
            repository: "stable".into(),
            chart: "nginx".into(),
            version: "1.3.0".into(),
            app_version: Some("1.28".into()),
        };
        // The badge does not change the cell's text (search, sort, copy).
        let row = release_row(&release.summary(), false, Some(&update));
        assert_eq!(row.cells[1].label().as_ref(), "nginx-1.2.0");
        assert_eq!(update_badge_label(&update), "↑ 1.3.0");
        assert_eq!(
            update_text(&update),
            "New version 1.3.0 (app 1.28) available in repo stable"
        );

        let row = revision_row(&release);
        assert_eq!(row.key.as_ref(), "3");
        assert_eq!(row.cells[5].label().as_ref(), "Upgrade complete");
    }

    #[test]
    fn tab_texts() {
        let release = release(1, 3);
        assert_eq!(tab_text(&release, Tab::Values), "replicaCount: 3\n");
        let computed = tab_text(&release, Tab::ComputedValues);
        assert_eq!(computed, "image:\n  tag: stable\nreplicaCount: 3\n");
        assert!(tab_text(&release, Tab::Manifest).starts_with("---\napiVersion"));
        assert_eq!(tab_text(&release, Tab::Notes), "Visit http://web");
        assert!(tab_text(&release, Tab::Hooks).contains("no hooks"));
        assert!(Tab::Values.sensitive() && !Tab::Notes.sensitive());
    }

    #[test]
    fn hooks_are_ordered_by_weight() {
        let text = hooks_text(&[hook("migrate", 5), hook("init", -1)]);
        let init = text.find("name: init").unwrap();
        let migrate = text.find("name: migrate").unwrap();
        assert!(init < migrate, "{text}");
        assert!(text.starts_with(
            "---\n# Job init · pre-install, pre-upgrade · weight -1\n\
             # delete policy: hook-succeeded\n# last run: Succeeded\nkind: Job\n"
        ));
    }

    #[test]
    fn diff_pair_defaults_to_previous_revision() {
        let history = [release(1, 1), release(2, 2), release(4, 3)];
        assert_eq!(default_diff_pair(&history, None), Some((2, 4)));
        assert_eq!(default_diff_pair(&history, Some(2)), Some((1, 2)));
        assert_eq!(default_diff_pair(&history, Some(1)), Some((1, 2)));
        assert_eq!(default_diff_pair(&history, Some(9)), None);
        assert_eq!(default_diff_pair(&history[..1], None), None);
        assert_eq!(default_diff_pair(&[], None), None);
    }

    #[test]
    fn diff_sources_render_each_side() {
        let (old, new) = (release(1, 1), release(2, 5));
        assert_ne!(DiffSource::Values.text(&old), DiffSource::Values.text(&new));
        assert!(DiffSource::Manifest.text(&new).contains("replicas: 5"));
        assert!(
            DiffSource::ComputedValues
                .text(&new)
                .contains("tag: stable")
        );
    }
}
