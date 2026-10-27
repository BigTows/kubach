//! Admission webhooks: MutatingWebhookConfiguration and
//! ValidatingWebhookConfiguration. Both list webhooks with the same
//! fields (the mutating one adds `reinvocationPolicy`), so one plugin
//! reads the JSON of either.

use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhookConfiguration, ValidatingWebhookConfiguration,
};
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};
use serde_json::Value;

use super::gateway::{arr_at, str_at};
use super::{
    DELETE_ACTION, OVERVIEW, col, creation, kv, object_ref, or_none, pair, references,
    relations_with, run_delete, section, selector_string, text,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Cell, Column, ColumnKind, DetailSection, Field,
    FieldValue, GenericPlugin, Health, HealthStatus, ObjectRef, PluginError, Relation,
    ResourcePlugin, SectionBody,
};

const GROUP: &str = "admissionregistration.k8s.io";

/// MutatingWebhookConfiguration or ValidatingWebhookConfiguration.
pub struct WebhookConfigurationPlugin {
    mutating: bool,
}

impl WebhookConfigurationPlugin {
    pub const MUTATING: Self = Self { mutating: true };
    pub const VALIDATING: Self = Self { mutating: false };

    fn kind(&self) -> &'static str {
        if self.mutating {
            "MutatingWebhookConfiguration"
        } else {
            "ValidatingWebhookConfiguration"
        }
    }
}

fn webhooks(obj: &DynamicObject) -> &[Value] {
    arr_at(&obj.data, "/webhooks")
}

/// `failurePolicy` of a webhook; the API defaults it to `Fail`.
fn failure_policy(hook: &Value) -> &str {
    str_at(hook, "/failurePolicy").unwrap_or("Fail")
}

/// `Fail`, `Ignore`, or `Fail 2 · Ignore 1` when the webhooks differ.
fn failure_summary(obj: &DynamicObject) -> String {
    let hooks = webhooks(obj);
    let fail = hooks.iter().filter(|h| failure_policy(h) == "Fail").count();
    let ignore = hooks.len() - fail;
    match (fail, ignore) {
        (0, 0) => String::new(),
        (_, 0) => "Fail".into(),
        (0, _) => "Ignore".into(),
        _ => format!("Fail {fail} · Ignore {ignore}"),
    }
}

/// The webhook's Service as an object and a label (`ns/name:443/path`).
fn hook_service(hook: &Value) -> Option<(ObjectRef, String)> {
    let service = hook.pointer("/clientConfig/service")?;
    let name = str_at(service, "/name")?;
    let ns = str_at(service, "/namespace").unwrap_or("default");
    let port = service.get("port").and_then(Value::as_i64).unwrap_or(443);
    let path = str_at(service, "/path").unwrap_or_default();
    Some((
        object_ref("v1", "Service", name, Some(ns)),
        format!("{ns}/{name}:{port}{path}"),
    ))
}

fn strings(v: &Value, pointer: &str) -> Vec<String> {
    arr_at(v, pointer)
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// `CREATE, UPDATE · apps/v1 deployments, deployments/scale · Namespaced`.
fn rule_text(rule: &Value) -> String {
    let ops = strings(rule, "/operations").join(", ");
    let groups = strings(rule, "/apiGroups")
        .into_iter()
        .map(|g| if g.is_empty() { "core".into() } else { g })
        .collect::<Vec<_>>()
        .join(",");
    let versions = strings(rule, "/apiVersions").join(",");
    let resources = strings(rule, "/resources").join(", ");
    let mut parts = vec![or_none(ops), format!("{groups}/{versions} {resources}")];
    if let Some(scope) = str_at(rule, "/scope").filter(|s| *s != "*") {
        parts.push(scope.to_owned());
    }
    parts.join(" · ")
}

/// A selector as chips when it only has labels, as text otherwise; an
/// empty or missing one matches everything.
fn selector_field(key: &str, selector: Option<&Value>) -> Field {
    let parsed = selector
        .cloned()
        .and_then(|s| {
            serde_json::from_value::<k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector>(
                s,
            )
            .ok()
        })
        .unwrap_or_default();
    let only_labels = parsed.match_expressions.as_ref().is_none_or(Vec::is_empty);
    match (&parsed.match_labels, only_labels) {
        (Some(labels), true) if !labels.is_empty() => Field::new(
            key,
            FieldValue::Labels(labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
        ),
        _ => Field::text(
            key,
            selector_string(&parsed).unwrap_or_else(|| "<all>".into()),
        ),
    }
}

fn hook_fields(hook: &Value, mutating: bool) -> Vec<Field> {
    let mut fields = Vec::new();
    match hook_service(hook) {
        Some((service, label)) => fields.push(Field::object_link("Service", label, service)),
        None => fields.push(Field::text(
            "URL",
            or_none(str_at(hook, "/clientConfig/url").unwrap_or_default()),
        )),
    }
    let rules = arr_at(hook, "/rules");
    if rules.is_empty() {
        fields.push(Field::text("Rules", "<none>"));
    }
    for (i, rule) in rules.iter().enumerate() {
        let key = if rules.len() == 1 {
            "Rule".to_owned()
        } else {
            format!("Rule {}", i + 1)
        };
        fields.push(Field::text(key, rule_text(rule)));
    }
    fields.push(Field::text("Failure policy", failure_policy(hook)));
    fields.push(Field::text(
        "Side effects",
        str_at(hook, "/sideEffects").unwrap_or("Unknown"),
    ));
    let timeout = hook
        .get("timeoutSeconds")
        .and_then(Value::as_i64)
        .unwrap_or(10);
    fields.push(Field::text("Timeout", format!("{timeout}s")));
    fields.push(Field::text(
        "Match policy",
        str_at(hook, "/matchPolicy").unwrap_or("Equivalent"),
    ));
    if mutating {
        fields.push(Field::text(
            "Reinvocation",
            str_at(hook, "/reinvocationPolicy").unwrap_or("Never"),
        ));
    }
    fields.push(selector_field(
        "Namespace selector",
        hook.get("namespaceSelector"),
    ));
    fields.push(selector_field(
        "Object selector",
        hook.get("objectSelector"),
    ));
    let conditions: Vec<String> = arr_at(hook, "/matchConditions")
        .iter()
        .filter_map(|c| str_at(c, "/name").map(str::to_owned))
        .collect();
    if !conditions.is_empty() {
        fields.push(Field::text("Match conditions", conditions.join(", ")));
    }
    let versions = strings(hook, "/admissionReviewVersions").join(", ");
    if !versions.is_empty() {
        fields.push(Field::text("Review versions", versions));
    }
    fields
}

impl ResourcePlugin for WebhookConfigurationPlugin {
    fn name(&self) -> &'static str {
        if self.mutating {
            "mutatingwebhookconfiguration"
        } else {
            "validatingwebhookconfiguration"
        }
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk(GROUP, "v1", self.kind())]
    }

    fn columns(&self) -> Vec<Column> {
        vec![
            col("name", "Name", ColumnKind::Text),
            col("webhooks", "Webhooks", ColumnKind::Number),
            col("failure_policy", "Failure policy", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        vec![
            text(obj.metadata.name.clone().unwrap_or_default()),
            Cell::Number(webhooks(obj).len() as i64),
            text(failure_summary(obj)),
            Cell::Age(creation(obj)),
        ]
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let hooks = webhooks(obj);
        let mut sections = vec![kv(
            OVERVIEW,
            vec![
                pair("Webhooks", hooks.len().to_string()),
                pair("Failure policy", or_none(failure_summary(obj))),
            ],
        )];
        for hook in hooks {
            let name = str_at(hook, "/name").unwrap_or_default();
            sections.push(section(
                format!("Webhook {name}"),
                SectionBody::Fields(hook_fields(hook, self.mutating)),
            ));
        }
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![DELETE_ACTION]
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        if self.mutating {
            run_delete::<MutatingWebhookConfiguration>(client, obj, action_id)
        } else {
            run_delete::<ValidatingWebhookConfiguration>(client, obj, action_id)
        }
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let extra = webhooks(obj)
            .iter()
            .filter_map(hook_service)
            .map(|(service, _)| references(service))
            .collect();
        relations_with(obj, extra)
    }

    /// A configuration has no status: present means in effect.
    fn health(&self, _obj: &DynamicObject) -> Health {
        Health::phrase(HealthStatus::Healthy, msg!("state-active"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::test_util::{obj, pairs, section as body};

    fn config(kind: &str) -> DynamicObject {
        obj(json!({
            "apiVersion": "admissionregistration.k8s.io/v1", "kind": kind,
            "metadata": {"name": "policy-admission"},
            "webhooks": [
                {"name": "pods.policy.example.com",
                 "clientConfig": {"service": {"namespace": "policy", "name": "webhook", "path": "/mutate", "port": 8443}},
                 "rules": [
                    {"operations": ["CREATE", "UPDATE"], "apiGroups": [""], "apiVersions": ["v1"],
                     "resources": ["pods"], "scope": "Namespaced"},
                    {"operations": ["*"], "apiGroups": ["apps"], "apiVersions": ["v1"],
                     "resources": ["deployments", "deployments/scale"], "scope": "*"}
                 ],
                 "failurePolicy": "Ignore", "sideEffects": "None", "timeoutSeconds": 5,
                 "reinvocationPolicy": "IfNeeded",
                 "namespaceSelector": {"matchLabels": {"policy": "enabled"}},
                 "objectSelector": {"matchExpressions": [{"key": "skip", "operator": "DoesNotExist"}]},
                 "matchConditions": [{"name": "not-system", "expression": "true"}],
                 "admissionReviewVersions": ["v1"]},
                {"name": "external.example.com",
                 "clientConfig": {"url": "https://hooks.example.com/validate"},
                 "sideEffects": "NoneOnDryRun"}
            ]
        }))
    }

    #[test]
    fn webhook_cells() {
        for (p, kind) in [
            (
                WebhookConfigurationPlugin::MUTATING,
                "MutatingWebhookConfiguration",
            ),
            (
                WebhookConfigurationPlugin::VALIDATING,
                "ValidatingWebhookConfiguration",
            ),
        ] {
            let o = config(kind);
            let cells = p.cells(&o);
            assert_eq!(cells.len(), p.columns().len());
            assert_eq!(cells[1], Cell::Number(2));
            assert_eq!(cells[2], text("Fail 1 · Ignore 1"));
            assert_eq!(p.supported()[0].kind, kind);
            assert_eq!(p.health(&o).status, HealthStatus::Healthy);
        }
    }

    #[test]
    fn webhook_details_per_hook() {
        let p = WebhookConfigurationPlugin::MUTATING;
        let o = config("MutatingWebhookConfiguration");
        let sections = p.details(&o);
        assert_eq!(
            pairs(&sections, OVERVIEW),
            [
                pair("Webhooks", "2"),
                pair("Failure policy", "Fail 1 · Ignore 1")
            ]
        );
        let SectionBody::Fields(fields) = body(&sections, "Webhook pods.policy.example.com") else {
            panic!("not fields");
        };
        let lines: Vec<(String, String)> = fields
            .iter()
            .map(|f| (f.key.clone(), f.value.text()))
            .collect();
        assert_eq!(
            lines,
            [
                pair("Service", "policy/webhook:8443/mutate"),
                pair("Rule 1", "CREATE, UPDATE · core/v1 pods · Namespaced"),
                pair("Rule 2", "* · apps/v1 deployments, deployments/scale"),
                pair("Failure policy", "Ignore"),
                pair("Side effects", "None"),
                pair("Timeout", "5s"),
                pair("Match policy", "Equivalent"),
                pair("Reinvocation", "IfNeeded"),
                pair("Namespace selector", "policy=enabled"),
                pair("Object selector", "!skip"),
                pair("Match conditions", "not-system"),
                pair("Review versions", "v1"),
            ]
        );
        assert_eq!(
            fields[0].value,
            FieldValue::Link {
                phrase: None,
                label: "policy/webhook:8443/mutate".into(),
                target: object_ref("v1", "Service", "webhook", Some("policy")),
            }
        );
        assert!(matches!(fields[8].value, FieldValue::Labels(_)));

        let SectionBody::Fields(external) = body(&sections, "Webhook external.example.com") else {
            panic!("not fields");
        };
        let lines: Vec<(String, String)> = external
            .iter()
            .map(|f| (f.key.clone(), f.value.text()))
            .collect();
        assert_eq!(lines[0], pair("URL", "https://hooks.example.com/validate"));
        assert_eq!(lines[1], pair("Rules", "<none>"));
        assert_eq!(lines[2], pair("Failure policy", "Fail"));
        assert_eq!(lines[4], pair("Timeout", "10s"));
        assert!(lines.contains(&pair("Namespace selector", "<all>")));

        // Validating webhooks have no reinvocation policy.
        let v = WebhookConfigurationPlugin::VALIDATING
            .details(&config("ValidatingWebhookConfiguration"));
        let SectionBody::Fields(fields) = body(&v, "Webhook pods.policy.example.com") else {
            panic!("not fields");
        };
        assert!(fields.iter().all(|f| f.key != "Reinvocation"));

        let targets: Vec<_> = p.relations(&o).into_iter().map(|r| r.target.name).collect();
        assert_eq!(targets, ["webhook"]);
    }
}
