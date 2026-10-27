//! Namespace plugin: status, labels, what holds a namespace in
//! Terminating, its quotas and limit ranges, and delete (refused for the
//! namespaces Kubernetes keeps).

use k8s_openapi::api::core::v1::{Namespace, NamespaceCondition};
use kubach_i18n::{Message, msg};
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};

use super::{
    DELETE_ACTION, OVERVIEW, action, col, creation, decode, delete, fallback_cells, name_of,
    section, text, unknown,
};
use crate::namespace::{NAME_LABEL, is_immortal, is_system};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Field,
    FieldValue, GenericPlugin, Health, HealthStatus, PluginError, RelatedFilter, RelatedObjects,
    RelatedOrder, Relation, ResourcePlugin, SectionBody,
};

pub struct NamespacePlugin;

/// Conditions reported while a namespace cannot be fully deleted.
const DELETION_PROBLEMS: [&str; 4] = [
    "NamespaceDeletionDiscoveryFailure",
    "NamespaceDeletionGroupVersionParsingFailure",
    "NamespaceDeletionContentFailure",
    "NamespaceFinalizersRemaining",
];

/// Namespace conditions that report something being waited for when
/// `True` (the failure ones are negative by their names).
pub(crate) const WARNING_CONDITIONS: [&str; 2] =
    ["NamespaceContentRemaining", "NamespaceFinalizersRemaining"];

/// Title of the section that says what keeps a namespace in Terminating.
pub const BLOCKING_DELETION: &str = "Deletion blocked by";

/// Labels shown in the table before `+N`.
const LABELS_SHOWN: usize = 2;

/// What a deletion condition means, for people.
fn deletion_condition_title(type_: &str) -> &str {
    match type_ {
        "NamespaceContentRemaining" => "Objects remaining",
        "NamespaceFinalizersRemaining" => "Finalizers remaining",
        "NamespaceDeletionContentFailure" => "Deleting objects failed",
        "NamespaceDeletionDiscoveryFailure" => "API discovery failed",
        "NamespaceDeletionGroupVersionParsingFailure" => "API group version unreadable",
        other => other,
    }
}

/// Hint after the message of a deletion condition: what to do about it.
fn deletion_condition_hint(type_: &str) -> Option<Message> {
    match type_ {
        "NamespaceFinalizersRemaining" => Some(msg!("namespace-finalizers-hint")),
        "NamespaceDeletionDiscoveryFailure" => Some(msg!("namespace-discovery-hint")),
        _ => None,
    }
}

fn phase(ns: &Namespace) -> String {
    ns.status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| "Active".into())
}

fn conditions(ns: &Namespace) -> impl Iterator<Item = &NamespaceCondition> {
    ns.status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .into_iter()
        .flatten()
}

pub(crate) fn namespace_health(ns: &Namespace) -> Health {
    if phase(ns) != "Terminating" {
        return Health::new(HealthStatus::Healthy, "Active");
    }
    let true_of = |types: &[&str]| {
        conditions(ns).find(|c| types.contains(&c.type_.as_str()) && c.status == "True")
    };
    let message = |c: &NamespaceCondition| c.message.clone().unwrap_or_else(|| c.type_.clone());
    if let Some(c) = true_of(&DELETION_PROBLEMS) {
        return Health::new(HealthStatus::Degraded, message(c));
    }
    match true_of(&["NamespaceContentRemaining"]) {
        Some(c) => Health::new(HealthStatus::Progressing, message(c)),
        None => Health::new(HealthStatus::Progressing, "Terminating"),
    }
}

/// `team=web, env=prod +2`, without the label every namespace has.
pub(crate) fn short_labels(ns: &Namespace) -> String {
    let labels: Vec<String> = ns
        .metadata
        .labels
        .iter()
        .flatten()
        .filter(|(k, _)| k.as_str() != NAME_LABEL)
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    let mut shown = labels
        .iter()
        .take(LABELS_SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if labels.len() > LABELS_SHOWN {
        shown.push_str(&format!(" +{}", labels.len() - LABELS_SHOWN));
    }
    shown
}

/// What keeps a Terminating namespace: each deletion condition that is
/// `True`, with its message and what to do about it.
fn blocking(ns: &Namespace) -> Vec<Field> {
    let mut fields: Vec<Field> = conditions(ns)
        .filter(|c| c.status == "True")
        .filter(|c| {
            DELETION_PROBLEMS.contains(&c.type_.as_str()) || c.type_ == "NamespaceContentRemaining"
        })
        .map(|c| {
            let text = c
                .message
                .clone()
                .or_else(|| c.reason.clone())
                .unwrap_or_default();
            let title = deletion_condition_title(&c.type_);
            match deletion_condition_hint(&c.type_) {
                Some(hint) if text.is_empty() => Field::phrase(title, hint),
                Some(hint) => Field::phrase(
                    title,
                    msg!("namespace-condition-hint", message = text, hint = hint),
                ),
                None => Field::text(title, text),
            }
        })
        .collect();
    if fields.is_empty() {
        fields.push(Field::text(
            "Status",
            "Deleting the objects inside; nothing reported as stuck yet",
        ));
    }
    fields
}

/// Finalizers of the namespace: `spec.finalizers` (`kubernetes`, removed
/// by the namespace controller once it is empty) and `metadata.finalizers`.
fn finalizers(ns: &Namespace, obj: &DynamicObject) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let spec: Vec<String> = ns
        .spec
        .as_ref()
        .and_then(|s| s.finalizers.clone())
        .unwrap_or_default();
    if !spec.is_empty() {
        pairs.push(("spec.finalizers".to_owned(), spec.join(", ")));
    }
    let meta: Vec<String> = obj.metadata.finalizers.clone().unwrap_or_default();
    if !meta.is_empty() {
        pairs.push(("metadata.finalizers".to_owned(), meta.join(", ")));
    }
    pairs
}

/// Every object of a core kind in the namespace (quotas, limit ranges).
fn inside(namespace: &str, title: &str, kind: &str) -> DetailSection {
    section(
        title,
        SectionBody::Related(RelatedObjects {
            api_version: "v1".into(),
            kind: kind.into(),
            namespace: Some(namespace.to_owned()),
            filter: RelatedFilter::Selector {
                labels: None,
                owner_uid: None,
            },
            order: RelatedOrder::Name,
        }),
    )
}

impl ResourcePlugin for NamespacePlugin {
    fn name(&self) -> &'static str {
        "namespace"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("", "v1", "Namespace")]
    }

    fn columns(&self) -> Vec<Column> {
        vec![
            col("name", "Name", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("labels", "Labels", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(ns) = decode::<Namespace>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        vec![
            text(obj.metadata.name.clone().unwrap_or_default()),
            Cell::Status(Health::new(namespace_health(&ns).status, phase(&ns))),
            text(short_labels(&ns)),
            Cell::Age(creation(obj)),
        ]
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(ns) = decode::<Namespace>(obj) else {
            return GenericPlugin.details(obj);
        };
        let name = obj.metadata.name.clone().unwrap_or_default();
        let mut overview = vec![Field::text("Status", phase(&ns))];
        if let Some(deleted) = &obj.metadata.deletion_timestamp {
            overview.push(Field::new(
                "Deletion requested",
                FieldValue::Time(deleted.0),
            ));
        }
        if is_immortal(&name) {
            overview.push(Field::text(
                "Delete",
                "Not allowed: Kubernetes keeps this namespace",
            ));
        } else if is_system(&name) {
            overview.push(Field::text(
                "Delete",
                "System namespace: deleting it asks to type its name",
            ));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(overview))];
        if phase(&ns) == "Terminating" || obj.metadata.deletion_timestamp.is_some() {
            sections.push(section(
                BLOCKING_DELETION,
                SectionBody::Fields(blocking(&ns)),
            ));
        }
        let finalizers = finalizers(&ns, obj);
        if !finalizers.is_empty() {
            sections.push(section("Finalizers", SectionBody::KeyValues(finalizers)));
        }
        if !name.is_empty() {
            sections.push(inside(&name, "Resource quotas", "ResourceQuota"));
            sections.push(inside(&name, "Limit ranges", "LimitRange"));
        }
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![DELETE_ACTION]
    }

    /// No Delete for the namespaces the API server keeps anyway.
    fn actions_for(&self, obj: &DynamicObject) -> Vec<Action> {
        if obj.metadata.name.as_deref().is_some_and(is_immortal) {
            Vec::new()
        } else {
            self.actions()
        }
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(async move {
            match *action_id {
                action::DELETE => {
                    let name = name_of(obj)?;
                    if is_immortal(name) {
                        return Err(PluginError::InvalidObject(format!(
                            "namespace {name} is kept by Kubernetes and cannot be deleted"
                        )));
                    }
                    delete::<Namespace>(client, obj).await
                }
                _ => Err(unknown(action_id)),
            }
        })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        GenericPlugin.relations(obj)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<Namespace>(obj) {
            Some(ns) => namespace_health(&ns),
            None => GenericPlugin.health(obj),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs, section as section_body};

    fn stuck() -> DynamicObject {
        obj(json!({
            "apiVersion": "v1", "kind": "Namespace",
            "metadata": {
                "name": "old",
                "deletionTimestamp": "2026-10-20T10:00:00Z",
                "labels": {"kubernetes.io/metadata.name": "old"}
            },
            "spec": {"finalizers": ["kubernetes"]},
            "status": {"phase": "Terminating", "conditions": [
                {"type": "NamespaceDeletionDiscoveryFailure", "status": "False",
                 "reason": "ResourcesDiscovered", "message": "All resources successfully discovered"},
                {"type": "NamespaceDeletionContentFailure", "status": "False",
                 "reason": "ContentDeleted", "message": "All content successfully deleted"},
                {"type": "NamespaceContentRemaining", "status": "True",
                 "reason": "SomeResourcesRemain",
                 "message": "Some resources are remaining: widgets.example.com has 1 resource instances"},
                {"type": "NamespaceFinalizersRemaining", "status": "True",
                 "reason": "SomeFinalizersRemain",
                 "message": "Some content in the namespace has finalizers remaining: example.com/cleanup in 1 resource instances"}
            ]}
        }))
    }

    #[test]
    fn namespace_columns() {
        let p = NamespacePlugin;
        let ids: Vec<_> = p.columns().iter().map(|c| c.id).collect();
        assert_eq!(ids, ["name", "status", "labels", "age"]);
        let active = obj(json!({
            "apiVersion": "v1", "kind": "Namespace",
            "metadata": {"name": "dev", "creationTimestamp": "2026-10-01T00:00:00Z",
                "labels": {"kubernetes.io/metadata.name": "dev", "team": "web",
                           "env": "prod", "tier": "a"}},
            "status": {"phase": "Active"}
        }));
        let cells = p.cells(&active);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[0], Cell::Text("dev".into()));
        assert_eq!(
            cells[1],
            Cell::Status(Health::new(HealthStatus::Healthy, "Active"))
        );
        assert_eq!(cells[2], Cell::Text("env=prod, team=web +1".into()));
        assert!(matches!(cells[3], Cell::Age(Some(_))));

        // Only the automatic label: nothing to show.
        assert_eq!(p.cells(&stuck())[2], Cell::Text(String::new()));
        assert_eq!(
            p.cells(&stuck())[1],
            Cell::Status(Health::new(HealthStatus::Degraded, "Terminating"))
        );
    }

    #[test]
    fn terminating_details_explain_what_blocks_deletion() {
        let p = NamespacePlugin;
        let ns = stuck();
        assert_eq!(p.health(&ns).status, HealthStatus::Degraded);
        assert!(p.health(&ns).message.contains("finalizers remaining"));

        let sections = p.details(&ns);
        let SectionBody::Fields(overview) = section_body(&sections, OVERVIEW) else {
            panic!("overview is not fields");
        };
        assert_eq!(overview[0], Field::text("Status", "Terminating"));
        assert_eq!(overview[1].key, "Deletion requested");

        let SectionBody::Fields(blocking) = section_body(&sections, BLOCKING_DELETION) else {
            panic!("blocking is not fields");
        };
        let keys: Vec<_> = blocking.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["Objects remaining", "Finalizers remaining"]);
        assert!(blocking[1].value.text().contains("example.com/cleanup"));
        assert!(blocking[1].value.text().contains("must remove them"));
        assert_eq!(
            pairs(&sections, "Finalizers"),
            [("spec.finalizers".to_owned(), "kubernetes".to_owned())]
        );

        // Conditions: the "remaining" ones are warnings, failures negative.
        let SectionBody::Conditions(conditions) = section_body(&sections, "Conditions") else {
            panic!("conditions missing");
        };
        let of = |t: &str| conditions.iter().find(|c| c.type_ == t).unwrap();
        assert!(of("NamespaceFinalizersRemaining").warning);
        assert!(of("NamespaceDeletionContentFailure").negative);
        assert_eq!(of("NamespaceDeletionContentFailure").is_good(), Some(true));
        assert_eq!(of("NamespaceContentRemaining").is_good(), Some(false));

        // Quotas and limit ranges of the namespace are listed.
        let SectionBody::Related(quotas) = section_body(&sections, "Resource quotas") else {
            panic!("quotas missing");
        };
        assert_eq!(quotas.kind, "ResourceQuota");
        assert_eq!(quotas.namespace.as_deref(), Some("old"));
        assert!(matches!(
            section_body(&sections, "Limit ranges"),
            SectionBody::Related(r) if r.kind == "LimitRange"
        ));
    }

    #[test]
    fn terminating_without_problems_is_progressing() {
        let p = NamespacePlugin;
        let ns = obj(json!({
            "apiVersion": "v1", "kind": "Namespace",
            "metadata": {"name": "going", "deletionTimestamp": "2026-10-20T10:00:00Z"},
            "status": {"phase": "Terminating"}
        }));
        assert_eq!(
            p.health(&ns),
            Health::new(HealthStatus::Progressing, "Terminating")
        );
        let sections = p.details(&ns);
        let SectionBody::Fields(blocking) = section_body(&sections, BLOCKING_DELETION) else {
            panic!("blocking is not fields");
        };
        assert_eq!(blocking[0].key, "Status");
        // An active namespace has no deletion section.
        let active = obj(json!({
            "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "dev"}
        }));
        assert!(
            !p.details(&active)
                .iter()
                .any(|s| s.title == BLOCKING_DELETION)
        );
    }

    #[test]
    fn protected_namespaces_cannot_be_deleted() {
        let p = NamespacePlugin;
        let named = |name: &str| {
            obj(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": name}}))
        };
        for name in ["default", "kube-system", "kube-public"] {
            let ns = named(name);
            assert!(p.actions_for(&ns).is_empty(), "{name}");
            let SectionBody::Fields(overview) = section_body(&p.details(&ns), OVERVIEW).clone()
            else {
                panic!("overview is not fields");
            };
            assert!(overview.iter().any(|f| f.key == "Delete"), "{name}");
        }
        assert_eq!(p.actions_for(&named("kube-node-lease")), [DELETE_ACTION]);
        assert_eq!(p.actions_for(&named("shop")), [DELETE_ACTION]);
    }

    #[tokio::test]
    async fn deleting_an_immortal_namespace_sends_nothing() {
        // A client whose every request fails: the refusal comes first.
        let client =
            Client::try_from(kube::Config::new("http://127.0.0.1:9".parse().unwrap())).unwrap();
        let ns = obj(json!({
            "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "kube-system"}
        }));
        let error = NamespacePlugin
            .run_action(&client, &ns, &action::DELETE, &super::super::NO_PARAMS)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, PluginError::InvalidObject(m) if m.contains("kube-system")),
            "{error}"
        );
    }
}
