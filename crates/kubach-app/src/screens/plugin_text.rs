//! Texts of the built-in resource plugins in the language of the interface
//! (ADR 0018). `kubach-core` keeps column headers, action names, detail
//! section titles, field labels and health states in English, the language
//! the MCP agent reads; the UI translates them here by id (section titles
//! and field labels, which have no id, by their English text). Health
//! reasons and notes of our own come as [`Message`]s (`Health::phrase`,
//! `Brief::phrase`), as do values with data in them (`FieldValue::Phrase`,
//! the `phrase` of a link) and the notes of the Diagnostics and Security
//! sections (`SectionBody::Notes`). Anything unknown, such as the printer
//! columns of a CRD or data of the cluster, keeps the core's text.

use kubach_core::plugin::{
    Action, Brief, Column, ContainerBrief, ContainerHealth, ContainerPhase, Health, HealthStatus,
    ReplicaCounts,
};
use kubach_i18n::{Message, t};

/// The header of a plugin column.
pub fn column_title(column: &Column) -> String {
    column_message(column.id).unwrap_or_else(|| column.title.to_owned())
}

/// The header of a column by its id, `fallback` when unknown.
pub fn column_title_of(id: &str, fallback: &str) -> String {
    column_message(id).unwrap_or_else(|| fallback.to_owned())
}

/// The name of a plugin action (button label, tooltip, run title).
pub fn action_title(action: &Action) -> String {
    action_message(action.id.as_str()).unwrap_or_else(|| action.title.to_owned())
}

/// The title of a detail section a plugin made. Titles that are kind names
/// (Pods, Jobs) or carry data (`Webhook web`) stay as they are.
pub fn section_title(title: &str) -> String {
    if let Some(message) = section_message(title) {
        return message;
    }
    if let Some(parent) = title.strip_prefix("Parent ") {
        return t!("section-parent", parent = parent);
    }
    if let Some(item) = title.strip_suffix(" limits") {
        return t!("section-limits", item = item);
    }
    title.to_owned()
}

/// The name of a health state (status badge, table cell).
pub fn status_title(status: HealthStatus) -> String {
    match status {
        HealthStatus::Healthy => t!("health-healthy"),
        HealthStatus::Progressing => t!("health-progressing"),
        HealthStatus::Degraded => t!("health-degraded"),
        HealthStatus::Failed => t!("health-failed"),
        HealthStatus::Unknown => t!("health-unknown"),
    }
}

/// The label of a detail field, a key/value row or a table column. Labels
/// are looked up by their English text (`field-<slug>` in `fields.ftl`);
/// keys that are data (container, volume or key names) stay as they are.
pub fn label(text: &str) -> String {
    known_label(text).unwrap_or_else(|| text.to_owned())
}

/// [`label`] when `text` is a label of ours, `None` for data.
pub fn known_label(text: &str) -> Option<String> {
    if let Some(label) = by_english("field-", text) {
        return Some(label);
    }
    // `Rule 2` of a list of rules.
    if let Some(n) = text
        .strip_prefix("Rule ")
        .filter(|n| n.parse::<u32>().is_ok())
    {
        return Some(t!("field-rule-n", n = n));
    }
    // `Revision 3` of a Flux history.
    if let Some(revision) = text.strip_prefix("Revision ") {
        return Some(t!("field-revision-of", revision = revision));
    }
    // `#2 Repo`: a field of one of several sources (Argo CD).
    let (number, rest) = text.strip_prefix('#')?.split_once(' ')?;
    number.parse::<u32>().ok()?;
    known_label(rest).map(|label| format!("#{number} {label}"))
}

/// The value of a field labelled `key` when it is a text of ours (`Yes`,
/// `No rule: all ingress is denied`); values that are data stay.
pub fn field_value(key: &str, value: &str) -> String {
    if known_label(key).is_some()
        && let Some(text) = by_english("value-", value)
    {
        return text;
    }
    value.to_owned()
}

/// The message `<prefix><slug of text>` whose English text is exactly
/// `text`, in the language of the interface.
fn by_english(prefix: &str, text: &str) -> Option<String> {
    let id = format!("{prefix}{}", slug(text));
    if !kubach_i18n::has_message(&id) {
        return None;
    }
    let message = Message::new(id);
    (message.english() == text).then(|| message.to_string())
}

/// `Restart policy` → `restart-policy`, `History (failed)` → `history-failed`.
fn slug(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// The reason of a health: ours translated, the cluster's as it is.
pub fn health_message(health: &Health) -> String {
    match &health.phrase {
        Some(phrase) => phrase.to_string(),
        None => health.message.clone(),
    }
}

/// The note of a related object: ours translated, a phase as it is.
pub fn brief_note(brief: &Brief) -> String {
    match &brief.phrase {
        Some(phrase) => phrase.to_string(),
        None => brief.note.clone(),
    }
}

/// The Versions cell of the CRD table, `v1 (storage), v1alpha1 (not
/// served)`, with the notes in the language of the interface; version
/// names (which have no commas or brackets) stay.
pub fn crd_versions(text: &str) -> String {
    let mut items: Vec<String> = Vec::new();
    let mut open = false;
    for part in text.split(", ") {
        match items.last_mut() {
            Some(last) if open => {
                last.push_str(", ");
                last.push_str(part);
            }
            _ => items.push(part.to_owned()),
        }
        open = items
            .last()
            .is_some_and(|i| i.contains('(') && !i.ends_with(')'));
    }
    items
        .iter()
        .map(|item| {
            let Some((version, notes)) = item
                .strip_suffix(')')
                .and_then(|rest| rest.split_once(" ("))
            else {
                return item.clone();
            };
            let notes: Vec<String> = notes
                .split(", ")
                .map(|note| match note {
                    "storage" => t!("crd-version-storage"),
                    "not served" => t!("crd-version-not-served"),
                    "deprecated" => t!("crd-version-deprecated"),
                    other => other.to_owned(),
                })
                .collect();
            t!(
                "crd-version-notes",
                version = version,
                notes = notes.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `Running`, `Waiting: CrashLoopBackOff`, `Terminated: Completed (exit
/// code 0)`; reasons and messages are the kubelet's.
pub fn container_phase(phase: &ContainerPhase) -> String {
    match phase {
        ContainerPhase::Running { .. } => t!("container-state-running"),
        ContainerPhase::Waiting {
            reason,
            message: Some(message),
        } => t!(
            "container-state-waiting-message",
            reason = reason,
            message = message
        ),
        ContainerPhase::Waiting {
            reason,
            message: None,
        } => t!("container-state-waiting", reason = reason),
        ContainerPhase::Terminated { reason, exit_code } => t!(
            "container-state-terminated",
            reason = reason,
            code = *exit_code
        ),
        ContainerPhase::Unknown => t!("container-state-unknown"),
    }
}

/// The state line of a container's status cube: its phase and, when it
/// runs without being ready, that.
pub fn container_state(container: &ContainerBrief) -> String {
    let phase = container_phase(&container.phase);
    if container.health == ContainerHealth::NotReady {
        t!("container-state-not-ready", state = phase)
    } else {
        phase
    }
}

/// `3 desired · 3 current · 2 ready · 3 updated · 2 available`.
pub fn replicas(counts: &ReplicaCounts) -> String {
    [
        Some(t!("replicas-desired", count = counts.desired)),
        Some(t!("replicas-current", count = counts.current)),
        Some(t!("replicas-ready", count = counts.ready)),
        counts
            .updated
            .map(|count| t!("replicas-updated", count = count)),
        counts
            .available
            .map(|count| t!("replicas-available", count = count)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

fn column_message(id: &str) -> Option<String> {
    Some(match id {
        "access_modes" => t!("column-access-modes"),
        "active" => t!("column-active"),
        "address" => t!("column-address"),
        "addresses" => t!("column-addresses"),
        "age" => t!("column-age"),
        "allowed" => t!("column-allowed"),
        "applications" => t!("column-applications"),
        "available" => t!("column-available"),
        "backends" => t!("column-backends"),
        "binding_mode" => t!("column-binding-mode"),
        "capacity" => t!("column-capacity"),
        "chart" => t!("column-chart"),
        "claim" => t!("column-claim"),
        "class" => t!("column-class"),
        "cluster_ip" => t!("column-cluster-ip"),
        "completions" => t!("column-completions"),
        "containers" => t!("column-containers"),
        "controller" => t!("column-controller"),
        "cpu" => t!("column-cpu"),
        "current" => t!("column-current"),
        "data" => t!("column-data"),
        "default" => t!("column-default"),
        "desired" => t!("column-desired"),
        "destination" => t!("column-destination"),
        "duration" => t!("column-duration"),
        "egress" => t!("column-egress"),
        "expansion" => t!("column-expansion"),
        "external_ip" => t!("column-external-ip"),
        "failure_policy" => t!("column-failure-policy"),
        "from" => t!("column-from"),
        "generators" => t!("column-generators"),
        "global_default" => t!("column-global-default"),
        "group" => t!("column-group"),
        "health" => t!("column-health"),
        "healthy" => t!("column-healthy"),
        "hostnames" => t!("column-hostnames"),
        "hosts" => t!("column-hosts"),
        "images" => t!("column-images"),
        "ingress" => t!("column-ingress"),
        "internal_ip" => t!("column-internal-ip"),
        "ip" => t!("column-ip"),
        "kind" => t!("column-kind"),
        "labels" => t!("column-labels"),
        "last_schedule" => t!("column-last-schedule"),
        "listeners" => t!("column-listeners"),
        "max" => t!("column-max"),
        "max_unavailable" => t!("column-max-unavailable"),
        "memory" => t!("column-memory"),
        "min" => t!("column-min"),
        "min_available" => t!("column-min-available"),
        "mode" => t!("column-mode"),
        "most_used" => t!("column-most-used"),
        "name" => t!("column-name"),
        "namespace" => t!("column-namespace"),
        "node" => t!("column-node"),
        "node_selector" => t!("column-node-selector"),
        "parameters" => t!("column-parameters"),
        "parents" => t!("column-parents"),
        "path" => t!("column-path"),
        "pod_selector" => t!("column-pod-selector"),
        "policy_types" => t!("column-policy-types"),
        "ports" => t!("column-ports"),
        "preemption" => t!("column-preemption"),
        "project" => t!("column-project"),
        "provisioner" => t!("column-provisioner"),
        "ready" => t!("column-ready"),
        "reclaim_policy" => t!("column-reclaim-policy"),
        "ref" => t!("column-ref"),
        "reference" => t!("column-reference"),
        "replicas" => t!("column-replicas"),
        "repo" => t!("column-repo"),
        "request" => t!("column-request"),
        "resources" => t!("column-resources"),
        "restarts" => t!("column-restarts"),
        "revision" => t!("column-revision"),
        "role" => t!("column-role"),
        "roles" => t!("column-roles"),
        "rules" => t!("column-rules"),
        "runtime" => t!("column-runtime"),
        "schedule" => t!("column-schedule"),
        "scope" => t!("column-scope"),
        "selector" => t!("column-selector"),
        "source" => t!("column-source"),
        "status" => t!("column-status"),
        "storage_class" => t!("column-storage-class"),
        "subjects" => t!("column-subjects"),
        "suspend" => t!("column-suspend"),
        "suspended" => t!("column-suspended"),
        "sync" => t!("column-sync"),
        "target" => t!("column-target"),
        "targets" => t!("column-targets"),
        "timezone" => t!("column-timezone"),
        "to" => t!("column-to"),
        "type" => t!("column-type"),
        "types" => t!("column-types"),
        "up_to_date" => t!("column-up-to-date"),
        "url" => t!("column-url"),
        "value" => t!("column-value"),
        "version" => t!("column-version"),
        "versions" => t!("column-versions"),
        "volume" => t!("column-volume"),
        "webhooks" => t!("column-webhooks"),
        _ => return None,
    })
}

fn action_message(id: &str) -> Option<String> {
    Some(match id {
        "delete" => t!("action-delete"),
        "scale" => t!("action-scale"),
        "restart" => t!("action-restart"),
        "trigger" => t!("action-trigger"),
        "suspend" => t!("action-suspend"),
        "resume" => t!("action-resume"),
        "cordon" => t!("action-cordon"),
        "uncordon" => t!("action-uncordon"),
        "evict" => t!("action-evict"),
        "logs" => t!("action-logs"),
        "exec" => t!("action-exec"),
        "port-forward" => t!("action-port-forward"),
        "debug" => t!("action-debug"),
        "refresh" => t!("action-refresh"),
        "hard-refresh" => t!("action-hard-refresh"),
        "sync" => t!("action-sync"),
        "reconcile" => t!("action-reconcile"),
        _ => return None,
    })
}

fn section_message(title: &str) -> Option<String> {
    Some(match title {
        "Active jobs" => t!("section-active-jobs"),
        "Addresses" => t!("section-addresses"),
        "Allocatable" => t!("section-allocatable"),
        "Allowed topologies" => t!("section-allowed-topologies"),
        "Annotations" => t!("section-annotations"),
        "Applications" => t!("section-applications"),
        "Backends" => t!("section-backends"),
        "Behavior" => t!("section-behavior"),
        "Binary data" => t!("section-binary-data"),
        "Capacity" => t!("section-capacity"),
        "Conditions" => t!("section-conditions"),
        "Container policies" => t!("section-container-policies"),
        "Containers" => t!("section-containers"),
        "Data" => t!("section-data"),
        "Deletion blocked by" => t!("section-deletion-blocked-by"),
        "Diagnostics" => t!("section-diagnostics"),
        "Disrupted pods" => t!("section-disrupted-pods"),
        "Egress rules" => t!("section-egress-rules"),
        "Finalizers" => t!("section-finalizers"),
        "From" => t!("section-from"),
        "History" => t!("section-history"),
        "Images" => t!("section-images"),
        "Ingress rules" => t!("section-ingress-rules"),
        "Init containers" => t!("section-init-containers"),
        "Inventory" => t!("section-inventory"),
        "Labels" => t!("section-labels"),
        "Listeners" => t!("section-listeners"),
        "Metrics" => t!("section-metrics"),
        "Operation" => t!("section-operation"),
        "Owner references" => t!("section-owner-references"),
        "Parameters" => t!("section-parameters"),
        "Ports" => t!("section-ports"),
        "Probes" => t!("section-probes"),
        "Recommendations" => t!("section-recommendations"),
        "Resources" => t!("section-resources"),
        "Resources summary" => t!("section-resources-summary"),
        "Rules" => t!("section-rules"),
        "Rules (aggregated)" => t!("section-rules-aggregated"),
        "Scheduling" => t!("section-scheduling"),
        "Security" => t!("section-security"),
        "Selector" => t!("section-selector"),
        "Source" => t!("section-source"),
        "Sources" => t!("section-sources"),
        "Subjects" => t!("section-subjects"),
        "Sync history" => t!("section-sync-history"),
        "Sync result" => t!("section-sync-result"),
        "System info" => t!("section-system-info"),
        "Taints" => t!("section-taints"),
        "Template sources" => t!("section-template-sources"),
        "To" => t!("section-to"),
        "Usage" => t!("section-usage"),
        "Used by pods" => t!("section-used-by-pods"),
        "Versions" => t!("section-versions"),
        "Volume claim templates" => t!("section-volume-claim-templates"),
        "Volumes" => t!("section-volumes"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kubach_core::plugin::{
        ContainerBrief, ContainerHealth, ContainerPhase, ContainerRole, PluginRegistry,
        ReplicaCounts,
    };
    use kubach_i18n::{Locale, Message};

    use super::{
        action_message, column_message, container_phase, container_state, crd_versions,
        field_value, known_label, label, replicas, section_title, slug,
    };

    /// Calls in `kubach-core` plugins whose first argument is a label.
    const LABEL_CALLS: [&str; 9] = [
        "Field::text(",
        "Field::new(",
        "Field::link(",
        "Field::object_link(",
        "Field::phrase(",
        "Field::phrase_link(",
        "pair(",
        "selector_field(",
        "key(",
    ];

    /// The string literal at the start of `s` (after blanks).
    fn literal(s: &str) -> Option<&str> {
        let s = s.trim_start().strip_prefix('"')?;
        s.find('"').map(|end| &s[..end])
    }

    /// Labels the non-test code of a plugin source writes as literals:
    /// first arguments of [`LABEL_CALLS`], `("Rules".into(), …)` pairs and
    /// the column headers of `Table::new(&[…])`.
    fn literal_labels(source: &str) -> Vec<&str> {
        let code = source.split("#[cfg(test)]").next().unwrap_or_default();
        let mut labels = Vec::new();
        for call in LABEL_CALLS {
            for (at, _) in code.match_indices(call) {
                labels.extend(literal(&code[at + call.len()..]));
            }
        }
        for (at, _) in code.match_indices("(\"") {
            let rest = &code[at + 1..];
            if let Some(text) = literal(rest)
                && text.starts_with(|c: char| c.is_ascii_uppercase())
                && rest[text.len() + 2..].starts_with(".into(),")
            {
                labels.push(text);
            }
        }
        for (at, _) in code.match_indices("Table::new(") {
            let rest = code[at..].split_once("&[").map_or("", |(_, r)| r);
            let headers = rest.split(']').next().unwrap_or_default();
            labels.extend(headers.split(',').filter_map(literal));
        }
        labels
    }

    fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// A new field label of a built-in plugin needs its message in
    /// `fields.ftl`, or it shows in English whatever the language. Labels
    /// have no id, so they are read from the plugins' sources.
    #[test]
    fn every_builtin_label_is_translated() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../kubach-core/src/plugin");
        let mut files = Vec::new();
        rust_files(&dir, &mut files);
        let mut seen = 0;
        for file in files {
            let source = std::fs::read_to_string(&file).unwrap();
            for text in literal_labels(&source) {
                seen += 1;
                assert!(
                    known_label(text).is_some(),
                    "{}: label {text:?} has no field-{} in fields.ftl",
                    file.display(),
                    slug(text)
                );
            }
        }
        // The scan itself still finds the labels.
        assert!(seen > 150, "only {seen} labels found");
    }

    #[test]
    fn labels_are_found_by_their_english_text() {
        assert_eq!(slug("History (failed)"), "history-failed");
        assert_eq!(slug("Auto-sync"), "auto-sync");
        assert_eq!(label("Restart policy"), "Restart policy");
        let ru = Message::new(format!("field-{}", slug("Restart policy"))).translate(Locale::Ru);
        assert_eq!(ru, "Политика перезапуска");
        assert!(known_label("Rule 2").is_some());
        assert!(known_label("#2 Repo").is_some());
        // Data keys stay: names of containers, volumes, ConfigMap keys.
        for data in ["app", "restart policy", "#2 2026-10-01", "Rule two"] {
            assert_eq!(known_label(data), None, "{data}");
        }
        assert_eq!(field_value("Immutable", "Yes"), "Yes");
        assert!(super::by_english("value-", "Yes").is_some());
        // A value is only ours under a label of ours.
        assert_eq!(field_value("app.yaml", "Yes"), "Yes");
    }

    #[test]
    fn states_read_like_the_core_ones() {
        for phase in [
            ContainerPhase::Running { since: None },
            ContainerPhase::Waiting {
                reason: "CrashLoopBackOff".into(),
                message: Some("back-off 5m".into()),
            },
            ContainerPhase::Waiting {
                reason: "ContainerCreating".into(),
                message: None,
            },
            ContainerPhase::Terminated {
                reason: "Error".into(),
                exit_code: 1,
            },
            ContainerPhase::Unknown,
        ] {
            assert_eq!(container_phase(&phase), phase.text());
        }
        let brief = ContainerBrief::new(
            "app",
            ContainerRole::App,
            &ContainerPhase::Running { since: None },
            Some(false),
            0,
        );
        assert_eq!(brief.health, ContainerHealth::NotReady);
        assert_eq!(container_state(&brief), brief.state);
        let counts = ReplicaCounts {
            desired: 3,
            current: 3,
            ready: 2,
            updated: Some(3),
            available: None,
        };
        assert_eq!(replicas(&counts), counts.describe());
    }

    #[test]
    fn notes_of_pods_count_restarts_in_russian() {
        let note = |restarts: i32| {
            Message::new("brief-pod-on-node")
                .arg("ready", 1)
                .arg("total", 1)
                .arg("restarts", restarts)
                .arg("node", "node-1")
                .translate(Locale::Ru)
        };
        assert_eq!(note(0), "готово 1/1 · node-1");
        assert_eq!(note(1), "готово 1/1 · 1 перезапуск · node-1");
        assert_eq!(note(3), "готово 1/1 · 3 перезапуска · node-1");
        assert_eq!(note(5), "готово 1/1 · 5 перезапусков · node-1");
    }

    /// A new column or action of a built-in plugin needs its message in
    /// `columns.ftl`, or it shows in English whatever the language.
    #[test]
    fn every_builtin_column_and_action_is_translated() {
        let registry = PluginRegistry::with_builtins();
        for plugin in registry.plugins() {
            for column in plugin.columns() {
                assert!(column_message(column.id).is_some(), "column {}", column.id);
            }
            for action in plugin.actions() {
                assert!(
                    action_message(action.id.as_str()).is_some(),
                    "action {}",
                    action.id
                );
            }
        }
    }

    #[test]
    fn crd_versions_keep_names_and_read_like_the_core() {
        for text in [
            "v1",
            "v1 (storage), v1beta1, v1alpha1 (not served, deprecated)",
            "v2 (storage, deprecated), v1 (not served)",
        ] {
            assert_eq!(crd_versions(text), text);
        }
        let ru = Message::new("crd-version-notes")
            .arg("version", "v1alpha1")
            .arg(
                "notes",
                Message::new("crd-version-not-served").translate(Locale::Ru),
            )
            .translate(Locale::Ru);
        assert_eq!(ru, "v1alpha1 (не обслуживается)");
    }

    /// Diagnostics of a pod read in Russian without English left over;
    /// text output (MCP) keeps English.
    #[test]
    fn pod_notes_are_translated_and_english_for_text() {
        let pod: k8s_openapi::api::core::v1::Pod = serde_json::from_value(serde_json::json!({
            "metadata": { "name": "web", "namespace": "default" },
            "spec": { "containers": [{ "name": "api", "image": "nginx:1.27" }] },
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "api", "image": "nginx:1.27", "imageID": "",
                    "ready": false, "restartCount": 12,
                    "state": { "waiting": { "reason": "CrashLoopBackOff" } },
                    "lastState": { "terminated": { "exitCode": 1, "reason": "Error" } }
                }]
            }
        }))
        .unwrap();
        let findings = kubach_core::diagnostics::diagnose(&pod, &[]);
        let note = findings[0].note();
        assert_eq!(
            note.key_in(Locale::En),
            "Error: Container `api` is crash looping (exit code 1, 12 restarts)"
        );
        assert_eq!(
            note.key_in(Locale::Ru),
            "Ошибка: Контейнер `api` падает в цикле (код выхода 1, 12 перезапусков)"
        );
        assert_eq!(
            note.value_in(Locale::Ru),
            "Последний запуск завершился с кодом 1, причина: Error.\n\
             Подсказка: Посмотрите логи предыдущего запуска"
        );
        assert_eq!(
            note.value_in(Locale::En),
            "Last run ended with exit code 1, reason Error.\nHint: Check logs of the previous run"
        );
        // The pod's health reason is the title, translated the same way.
        let health = kubach_core::plugin::builtin::pod_health_with_events(
            &pod,
            &[],
            k8s_openapi::jiff::Timestamp::now(),
        );
        assert_eq!(health.message, findings[0].title.english());
        assert_eq!(
            health.phrase.unwrap().translate(Locale::Ru),
            "Контейнер `api` падает в цикле (код выхода 1, 12 перезапусков)"
        );
    }

    #[test]
    fn sections_with_data_keep_the_data() {
        assert_eq!(section_title("Volumes"), "Volumes");
        assert_eq!(
            section_title("Parent Gateway web/main"),
            "Parent Gateway web/main"
        );
        assert_eq!(section_title("Container limits"), "Container limits");
        assert_eq!(
            section_title("Webhook pods.example.com"),
            "Webhook pods.example.com"
        );
    }
}
