//! Requests sent by `run_action` of the built-in plugins
//! (#220), checked against a fake API server: method, path, query,
//! content type and body. The UI and the MCP agent both go through these.

use std::sync::{Arc, Mutex};

use http::{Request, Response};
use http_body_util::BodyExt;
use kube::Client;
use kube::api::{DynamicObject, PropagationPolicy};
use kube::client::Body;
use serde_json::{Value, json};

use super::node::cordon_patch;
use super::workloads::restart_patch;
use super::*;
use crate::plugin::bulk;
use crate::plugin::{
    ActionId, ActionKind, ActionParams, Brief, GenericPlugin, Health, HealthStatus, PluginRegistry,
    ResourcePlugin,
};

/// One request seen by the fake API server.
#[derive(Debug)]
struct Sent {
    method: String,
    /// Path and query, as sent.
    uri: String,
    content_type: Option<String>,
    body: Value,
}

/// A client whose every request is recorded and answered with `{}` (200).
fn fake_client() -> (Client, Arc<Mutex<Vec<Sent>>>) {
    fake_client_with(|_| None)
}

/// Like [`fake_client`], but a `GET` of a path for which `answer` returns
/// a value (discovery) is answered with it.
fn fake_client_with(answer: fn(&str) -> Option<Value>) -> (Client, Arc<Mutex<Vec<Sent>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    let service = tower::service_fn(move |req: Request<Body>| {
        let log = log.clone();
        async move {
            let reply = (req.method() == http::Method::GET)
                .then(|| answer(req.uri().path()))
                .flatten()
                .unwrap_or_else(|| json!({}));
            let (parts, body) = req.into_parts();
            let bytes = body.collect().await?.to_bytes();
            let body = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap_or(Value::Null)
            };
            log.lock().unwrap().push(Sent {
                method: parts.method.to_string(),
                uri: parts.uri.to_string(),
                content_type: parts
                    .headers
                    .get(http::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
                body,
            });
            Ok::<_, kube::Error>(
                Response::builder()
                    .status(200)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&reply).unwrap()))
                    .unwrap(),
            )
        }
    });
    (Client::new(service, "default"), sent)
}

fn obj(v: Value) -> DynamicObject {
    serde_json::from_value(v).unwrap()
}

fn named(api_version: &str, kind: &str, ns: Option<&str>) -> DynamicObject {
    let mut meta = json!({"name": "web", "uid": "u-1"});
    if let Some(ns) = ns {
        meta["namespace"] = json!(ns);
    }
    obj(json!({"apiVersion": api_version, "kind": kind, "metadata": meta, "spec": {}}))
}

/// Runs one action and returns the single request it sent.
async fn run(
    plugin: &dyn ResourcePlugin,
    o: &DynamicObject,
    action_id: ActionId,
    params: &ActionParams,
) -> Sent {
    let (client, sent) = fake_client();
    plugin
        .run_action(&client, o, &action_id, params)
        .await
        .unwrap_or_else(|e| panic!("{action_id}: {e}"));
    let mut sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1, "{action_id}: {sent:?}");
    sent.remove(0)
}

const MERGE: &str = "application/merge-patch+json";
const STRATEGIC: &str = "application/strategic-merge-patch+json";

fn assert_delete(s: &Sent, path: &str) {
    assert_eq!(s.method, "DELETE");
    assert_eq!(s.uri, format!("{path}?"));
    assert_eq!(s.content_type.as_deref(), Some("application/json"));
    assert_eq!(s.body["propagationPolicy"], "Background");
}

#[test]
fn shared_params() {
    let mut obj: DynamicObject = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": {"name": "c", "namespace": "ns"}
    }))
    .unwrap();
    let d = delete_params(&obj);
    assert!(matches!(
        d.propagation_policy,
        Some(PropagationPolicy::Background)
    ));
    assert!(!d.dry_run);
    assert_eq!(d.grace_period_seconds, None);
    // Without a UID, no precondition; with one, the delete leaves alone an
    // object recreated under the same name (#345).
    assert!(d.preconditions.is_none());
    obj.metadata.uid = Some("u-1".into());
    let d = delete_params(&obj);
    assert_eq!(d.preconditions.unwrap().uid.as_deref(), Some("u-1"));

    let p = patch_params();
    assert_eq!(p.field_manager, None);
    assert!(!p.dry_run);
    assert!(!p.force);
}

#[tokio::test]
async fn workload_scale_restart_delete() {
    let mut replicas = ActionParams::new();
    replicas.insert(action::REPLICAS.into(), json!(3));
    let none = ActionParams::new();

    let cases: [(&dyn ResourcePlugin, &str, &str); 4] = [
        (&DeploymentPlugin, "Deployment", "deployments"),
        (&StatefulSetPlugin, "StatefulSet", "statefulsets"),
        (&DaemonSetPlugin, "DaemonSet", "daemonsets"),
        (&ReplicaSetPlugin, "ReplicaSet", "replicasets"),
    ];
    for (plugin, kind, plural) in cases {
        let o = named("apps/v1", kind, Some("ns1"));
        let path = format!("/apis/apps/v1/namespaces/ns1/{plural}/web");

        if kind != "DaemonSet" {
            let s = run(plugin, &o, action::SCALE, &replicas).await;
            assert_eq!(s.method, "PATCH");
            // No field manager, no dry run: an empty query.
            assert_eq!(s.uri, format!("{path}/scale?"));
            assert_eq!(s.content_type.as_deref(), Some(MERGE));
            assert_eq!(s.body, json!({"spec": {"replicas": 3}}));
        }

        if kind != "ReplicaSet" {
            let s = run(plugin, &o, action::RESTART, &none).await;
            assert_eq!(s.method, "PATCH");
            assert_eq!(s.uri, format!("{path}?"));
            assert_eq!(s.content_type.as_deref(), Some(STRATEGIC));
            let at = s
                .body
                .pointer("/spec/template/metadata/annotations/kubectl.kubernetes.io~1restartedAt")
                .and_then(Value::as_str)
                .unwrap();
            assert_eq!(s.body, restart_patch(at));
        }

        let s = run(plugin, &o, action::DELETE, &none).await;
        assert_delete(&s, &path);
    }
}

#[tokio::test]
async fn workload_scale_without_replicas_sends_nothing() {
    let (client, sent) = fake_client();
    let o = named("apps/v1", "Deployment", Some("ns1"));
    let r = DeploymentPlugin
        .run_action(&client, &o, &action::SCALE, &ActionParams::new())
        .await;
    assert!(matches!(r, Err(PluginError::InvalidParam { .. })));
    // Not allowed for the kind: rejected before any request.
    let r = DaemonSetPlugin
        .run_action(&client, &o, &action::SCALE, &ActionParams::new())
        .await;
    assert!(matches!(r, Err(PluginError::UnknownAction(_))));
    assert!(sent.lock().unwrap().is_empty());
}

/// The stepper's relative scale (#347): read `/scale`, then replace it
/// with the step from the read count and its resourceVersion.
#[tokio::test]
async fn workload_scale_by_delta_reads_then_replaces() {
    let (client, sent) = fake_client_with(|path| {
        path.ends_with("/scale").then(|| {
            json!({"apiVersion": "autoscaling/v1", "kind": "Scale",
                   "metadata": {"name": "web", "namespace": "ns1", "resourceVersion": "42"},
                   "spec": {"replicas": 5}})
        })
    });
    let mut params = ActionParams::new();
    params.insert(action::DELTA.into(), json!(-1));
    let o = named("apps/v1", "StatefulSet", Some("ns1"));
    StatefulSetPlugin
        .run_action(&client, &o, &action::SCALE, &params)
        .await
        .unwrap();
    {
        let sent = sent.lock().unwrap();
        let path = "/apis/apps/v1/namespaces/ns1/statefulsets/web/scale";
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(
            (sent[0].method.as_str(), sent[0].uri.as_str()),
            ("GET", path)
        );
        assert_eq!(sent[1].method, "PUT");
        assert_eq!(sent[1].uri, format!("{path}?"));
        assert_eq!(sent[1].body["spec"]["replicas"], 4);
        assert_eq!(sent[1].body["metadata"]["resourceVersion"], "42");
    }

    // A zero step is refused before any request.
    let (client, sent) = fake_client();
    params.insert(action::DELTA.into(), json!(0));
    let r = StatefulSetPlugin
        .run_action(&client, &o, &action::SCALE, &params)
        .await;
    assert!(matches!(r, Err(PluginError::InvalidParam { .. })));
    assert!(sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn node_cordon_uncordon() {
    let o = named("v1", "Node", None);
    let none = ActionParams::new();
    for (id, body) in [
        (action::CORDON, json!({"spec": {"unschedulable": true}})),
        (action::UNCORDON, json!({"spec": {"unschedulable": null}})),
    ] {
        let s = run(&NodePlugin, &o, id, &none).await;
        assert_eq!(s.method, "PATCH");
        assert_eq!(s.uri, "/api/v1/nodes/web?");
        assert_eq!(s.content_type.as_deref(), Some(STRATEGIC));
        assert_eq!(s.body, body);
        assert_eq!(s.body, cordon_patch(id == action::CORDON));
    }
}

/// Drain (#318) runs from the details panel through `crate::drain`, with
/// options, progress and cancel; it is no plugin action, so bulk runs and
/// the MCP agent never get it.
#[tokio::test]
async fn node_drain_is_not_a_plugin_action() {
    let drain = ActionId::new("drain");
    let o = named("v1", "Node", None);
    let mut cordoned = o.clone();
    cordoned.data["spec"] = json!({"unschedulable": true});
    for declared in [
        NodePlugin.actions(),
        NodePlugin.actions_for(&o),
        NodePlugin.actions_for(&cordoned),
    ] {
        assert!(declared.iter().all(|a| a.id != drain), "{declared:?}");
    }
    let (client, sent) = fake_client();
    let r = NodePlugin
        .run_action(&client, &o, &drain, &ActionParams::new())
        .await;
    assert!(matches!(r, Err(PluginError::UnknownAction(id)) if id == "drain"));
    assert!(sent.lock().unwrap().is_empty());
}

/// Every stream a built-in plugin declares (logs, exec, debug,
/// port-forward) is refused by `run_action` without a request: the UI and
/// the MCP agent open streams by their [`ActionKind`].
#[tokio::test]
async fn stream_actions_are_not_run() {
    let registry = PluginRegistry::with_builtins();
    let (client, sent) = fake_client();
    let o = named("v1", "Pod", Some("ns1"));
    let mut streams = 0;
    for plugin in registry.plugins() {
        for a in plugin.actions() {
            let ActionKind::Stream(_) = a.kind else {
                continue;
            };
            streams += 1;
            let r = plugin.run_action(&client, &o, &a.id, &NO_PARAMS).await;
            assert!(
                matches!(&r, Err(PluginError::Stream(id)) if *id == a.id.as_str()),
                "{} {}: {r:?}",
                plugin.name(),
                a.id
            );
        }
    }
    // Pod: logs, exec, debug, port-forward; workloads, Service, Job: port-forward.
    assert!(streams >= 8, "{streams}");
    assert!(sent.lock().unwrap().is_empty());
}

/// APIResourceList of `example.com/v1` with one namespaced `widgets` and
/// one cluster-scoped `gadgets` (irregular plural on purpose).
fn widget_discovery(path: &str) -> Option<Value> {
    (path == "/apis/example.com/v1").then(|| {
        json!({
            "kind": "APIResourceList", "apiVersion": "v1", "groupVersion": "example.com/v1",
            "resources": [
                {"name": "widgetries", "singularName": "widget", "namespaced": true,
                 "kind": "Widget", "verbs": ["delete", "get", "list"]},
                {"name": "widgetries/status", "singularName": "", "namespaced": true,
                 "kind": "Widget", "verbs": ["get"]},
                {"name": "gadgets", "singularName": "gadget", "namespaced": false,
                 "kind": "Gadget", "verbs": ["delete", "get", "list"]}
            ]
        })
    })
}

#[tokio::test]
async fn generic_delete_uses_discovery_for_crds() {
    assert_eq!(GenericPlugin.actions(), [DELETE_ACTION]);
    let none = ActionParams::new();
    for (kind, ns, path) in [
        (
            "Widget",
            Some("ns1"),
            "/apis/example.com/v1/namespaces/ns1/widgetries/web",
        ),
        ("Gadget", None, "/apis/example.com/v1/gadgets/web"),
    ] {
        let (client, sent) = fake_client_with(widget_discovery);
        let o = named("example.com/v1", kind, ns);
        GenericPlugin
            .run_action(&client, &o, &action::DELETE, &none)
            .await
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(sent[0].method, "GET");
        assert_eq!(sent[0].uri, "/apis/example.com/v1");
        assert_delete(&sent[1], path);
    }

    // A built-in kind without a plugin of its own (core group).
    let (client, sent) = fake_client_with(|path| {
        (path == "/api/v1").then(|| {
            json!({
                "kind": "APIResourceList", "groupVersion": "v1",
                "resources": [{"name": "serviceaccounts", "singularName": "",
                    "namespaced": true, "kind": "ServiceAccount", "verbs": ["delete"]}]
            })
        })
    });
    let o = named("v1", "ServiceAccount", Some("ns1"));
    GenericPlugin
        .run_action(&client, &o, &action::DELETE, &none)
        .await
        .unwrap();
    let sent = sent.lock().unwrap();
    assert_delete(&sent[1], "/api/v1/namespaces/ns1/serviceaccounts/web");
}

#[tokio::test]
async fn pv_and_pvc_delete() {
    let none = ActionParams::new();
    let cases: [(&dyn ResourcePlugin, &str, Option<&str>, &str); 2] = [
        (
            &PersistentVolumeClaimPlugin,
            "PersistentVolumeClaim",
            Some("ns1"),
            "/api/v1/namespaces/ns1/persistentvolumeclaims/web",
        ),
        (
            &PersistentVolumePlugin,
            "PersistentVolume",
            None,
            "/api/v1/persistentvolumes/web",
        ),
    ];
    let registry = PluginRegistry::with_builtins();
    for (plugin, kind, ns, path) in cases {
        assert_eq!(registry.plugin_for("", kind).name(), plugin.name());
        let o = named("v1", kind, ns);
        let declared = plugin.actions_for(&o);
        assert_eq!(declared, [DELETE_ACTION], "{kind}");
        // Confirmed like every other delete, no discovery request.
        assert_eq!(
            bulk::confirmation(&declared[0], 1, false),
            bulk::Confirmation::List
        );
        let s = run(plugin, &o, action::DELETE, &none).await;
        assert_delete(&s, path);
    }
}

#[tokio::test]
async fn generic_delete_of_namespaced_kind_without_namespace_sends_no_delete() {
    let (client, sent) = fake_client_with(widget_discovery);
    let o = named("example.com/v1", "Widget", None);
    let r = GenericPlugin
        .run_action(&client, &o, &action::DELETE, &ActionParams::new())
        .await;
    assert!(matches!(r, Err(PluginError::InvalidObject(_))), "{r:?}");
    assert!(sent.lock().unwrap().iter().all(|s| s.method != "DELETE"));
}

#[test]
fn crds_are_deleted_in_bulk_with_the_same_confirmation() {
    let crds: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|name| {
            let mut o = named("example.com/v1", "Widget", Some("ns1"));
            o.metadata.name = Some(name.into());
            std::sync::Arc::new(o)
        })
        .collect();
    let common = bulk::common_actions(&GenericPlugin, &crds);
    assert_eq!(common, [DELETE_ACTION]);
    let pods = [std::sync::Arc::new(named("v1", "Pod", Some("ns1")))];
    let pod_delete: Vec<_> = bulk::common_actions(&PodPlugin, &pods)
        .into_iter()
        .filter(|a| a.id == action::DELETE)
        .collect();
    assert_eq!(pod_delete, common);
    for count in [1, 6] {
        assert_eq!(
            bulk::confirmation(&common[0], count, false),
            bulk::confirmation(&pod_delete[0], count, false)
        );
    }
}

#[tokio::test]
async fn gitops_delete() {
    let none = ActionParams::new();
    let cases: [(&dyn ResourcePlugin, &str, &str, &str); 3] = [
        (
            &gitops::ApplicationPlugin,
            "argoproj.io/v1alpha1",
            "Application",
            "/apis/argoproj.io/v1alpha1/namespaces/ns1/applications/web",
        ),
        (
            &gitops::ApplicationSetPlugin,
            "argoproj.io/v1alpha1",
            "ApplicationSet",
            "/apis/argoproj.io/v1alpha1/namespaces/ns1/applicationsets/web",
        ),
        (
            &gitops::FluxKustomizationPlugin,
            "kustomize.toolkit.fluxcd.io/v1",
            "Kustomization",
            "/apis/kustomize.toolkit.fluxcd.io/v1/namespaces/ns1/kustomizations/web",
        ),
    ];
    for (plugin, api_version, kind, path) in cases {
        let o = named(api_version, kind, Some("ns1"));
        assert!(
            plugin.actions_for(&o).contains(&DELETE_ACTION),
            "{kind} declares delete"
        );
        let s = run(plugin, &o, action::DELETE, &none).await;
        assert_delete(&s, path);
    }
}

#[tokio::test]
async fn job_and_cronjob_suspend_delete() {
    let none = ActionParams::new();
    let cases: [(&dyn ResourcePlugin, &str, &str); 2] = [
        (&JobPlugin, "Job", "jobs"),
        (&CronJobPlugin, "CronJob", "cronjobs"),
    ];
    for (plugin, kind, plural) in cases {
        let o = named("batch/v1", kind, Some("ns1"));
        let path = format!("/apis/batch/v1/namespaces/ns1/{plural}/web");
        for (id, suspend) in [(action::SUSPEND, true), (action::RESUME, false)] {
            let s = run(plugin, &o, id, &none).await;
            assert_eq!(s.method, "PATCH");
            assert_eq!(s.uri, format!("{path}?"));
            assert_eq!(s.content_type.as_deref(), Some(MERGE));
            assert_eq!(s.body, json!({"spec": {"suspend": suspend}}));
        }
        let s = run(plugin, &o, action::DELETE, &none).await;
        assert_delete(&s, &path);
    }
}

#[tokio::test]
async fn cronjob_trigger_creates_job() {
    let o = obj(json!({
        "apiVersion": "batch/v1", "kind": "CronJob",
        "metadata": {"name": "nightly", "namespace": "ns1", "uid": "u-1"},
        "spec": {
            "schedule": "0 0 * * *",
            "jobTemplate": {"spec": {"template": {"spec": {
                "restartPolicy": "Never",
                "containers": [{"name": "c", "image": "busybox"}]
            }}}}
        }
    }));
    let s = run(&CronJobPlugin, &o, action::TRIGGER, &ActionParams::new()).await;
    assert_eq!(s.method, "POST");
    assert_eq!(s.uri, "/apis/batch/v1/namespaces/ns1/jobs?");
    assert_eq!(s.content_type.as_deref(), Some("application/json"));
    assert_eq!(s.body["kind"], "Job");
    assert_eq!(s.body["metadata"]["generateName"], "nightly-manual-");
    assert_eq!(
        s.body["metadata"]["annotations"]["cronjob.kubernetes.io/instantiate"],
        "manual"
    );
    assert_eq!(s.body["metadata"]["ownerReferences"][0]["uid"], "u-1");
    assert_eq!(
        s.body["spec"]["template"]["spec"]["containers"][0]["image"],
        "busybox"
    );
}

#[tokio::test]
async fn unknown_actions_send_nothing() {
    let (client, sent) = fake_client();
    let o = named("apps/v1", "Deployment", Some("ns1"));
    for plugin in [
        &DeploymentPlugin as &dyn ResourcePlugin,
        &JobPlugin,
        &CronJobPlugin,
        &NodePlugin,
        &GenericPlugin,
    ] {
        let r = plugin
            .run_action(&client, &o, &ActionId::new("nope"), &ActionParams::new())
            .await;
        assert!(matches!(r, Err(PluginError::UnknownAction(id)) if id == "nope"));
    }
    assert!(sent.lock().unwrap().is_empty());
}

// --------------------------------------------------------------------- brief

fn with_phase(phase: &str) -> DynamicObject {
    obj(json!({
        "apiVersion": "v1", "kind": "PersistentVolumeClaim",
        "metadata": {"name": "data"},
        "status": {"phase": phase}
    }))
}

#[test]
fn default_brief_from_phase() {
    for (phase, status) in [
        ("Bound", HealthStatus::Healthy),
        ("Active", HealthStatus::Healthy),
        ("Succeeded", HealthStatus::Healthy),
        ("Pending", HealthStatus::Progressing),
        ("Released", HealthStatus::Progressing),
        ("Lost", HealthStatus::Failed),
        ("Failed", HealthStatus::Failed),
        ("Weird", HealthStatus::Unknown),
    ] {
        let b = GenericPlugin.brief(&with_phase(phase));
        assert_eq!(
            b,
            Brief {
                health: Health::new(status, phase),
                note: phase.into(),
                phrase: None,
                containers: Vec::new(),
            },
            "{phase}"
        );
    }

    // No phase and no conditions: unknown, empty note.
    let b = GenericPlugin.brief(&named("example.com/v1", "Widget", None));
    assert_eq!(b.health.status, HealthStatus::Unknown);
    assert_eq!(b.note, "");
}

#[test]
fn default_brief_keeps_known_health() {
    // Health from conditions wins over the phase; the phase stays the note.
    let o = obj(json!({
        "apiVersion": "example.com/v1", "kind": "Widget",
        "metadata": {"name": "w"},
        "status": {
            "phase": "Pending",
            "conditions": [{"type": "Ready", "status": "False", "message": "no endpoints"}]
        }
    }));
    let b = GenericPlugin.brief(&o);
    assert_eq!(
        b.health,
        Health::new(HealthStatus::Degraded, "no endpoints")
    );
    assert_eq!(b.note, "Pending");
}

#[test]
fn overridden_brief() {
    // ReplicaSet: ready count, not the phase.
    let o = obj(json!({
        "apiVersion": "apps/v1", "kind": "ReplicaSet",
        "metadata": {"name": "web", "namespace": "ns1"},
        "spec": {"replicas": 3},
        "status": {"replicas": 3, "readyReplicas": 2, "phase": "Running"}
    }));
    let b = ReplicaSetPlugin.brief(&o);
    assert_eq!(b.note, "2/3 ready");
    assert!(b.containers.is_empty());
    assert_eq!(b.health, ReplicaSetPlugin.health(&o));
}

/// A plugin, the object's apiVersion and kind, its namespace and the
/// path its delete is sent to.
type DeleteCase = (
    &'static dyn ResourcePlugin,
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
);

/// Kinds covered by #279: typed delete, no discovery request.
#[tokio::test]
async fn coverage_plugins_delete() {
    let none = ActionParams::new();
    let cases: [DeleteCase; 8] = [
        (
            &HorizontalPodAutoscalerPlugin,
            "autoscaling/v2",
            "HorizontalPodAutoscaler",
            Some("ns1"),
            "/apis/autoscaling/v2/namespaces/ns1/horizontalpodautoscalers/web",
        ),
        (
            &ResourceQuotaPlugin,
            "v1",
            "ResourceQuota",
            Some("ns1"),
            "/api/v1/namespaces/ns1/resourcequotas/web",
        ),
        (
            &LimitRangePlugin,
            "v1",
            "LimitRange",
            Some("ns1"),
            "/api/v1/namespaces/ns1/limitranges/web",
        ),
        (
            &PodDisruptionBudgetPlugin,
            "policy/v1",
            "PodDisruptionBudget",
            Some("ns1"),
            "/apis/policy/v1/namespaces/ns1/poddisruptionbudgets/web",
        ),
        (
            &NetworkPolicyPlugin,
            "networking.k8s.io/v1",
            "NetworkPolicy",
            Some("ns1"),
            "/apis/networking.k8s.io/v1/namespaces/ns1/networkpolicies/web",
        ),
        (
            &IngressClassPlugin,
            "networking.k8s.io/v1",
            "IngressClass",
            None,
            "/apis/networking.k8s.io/v1/ingressclasses/web",
        ),
        (
            &StorageClassPlugin,
            "storage.k8s.io/v1",
            "StorageClass",
            None,
            "/apis/storage.k8s.io/v1/storageclasses/web",
        ),
        (
            &PriorityClassPlugin,
            "scheduling.k8s.io/v1",
            "PriorityClass",
            None,
            "/apis/scheduling.k8s.io/v1/priorityclasses/web",
        ),
    ];
    let registry = PluginRegistry::with_builtins();
    for (plugin, api_version, kind, ns, path) in cases {
        let group = api_version.rsplit_once('/').map_or("", |(g, _)| g);
        assert_eq!(registry.plugin_for(group, kind).name(), plugin.name());
        let o = named(api_version, kind, ns);
        assert_eq!(plugin.actions_for(&o), [DELETE_ACTION], "{kind}");
        let s = run(plugin, &o, action::DELETE, &none).await;
        assert_delete(&s, path);
    }
}

/// Gateway API kinds are CRDs: delete goes through discovery.
#[tokio::test]
async fn gateway_api_delete_uses_discovery() {
    fn discovery(path: &str) -> Option<Value> {
        (path == "/apis/gateway.networking.k8s.io/v1").then(|| {
            json!({
                "kind": "APIResourceList", "groupVersion": "gateway.networking.k8s.io/v1",
                "resources": [
                    {"name": "gateways", "singularName": "gateway", "namespaced": true,
                     "kind": "Gateway", "verbs": ["delete"]},
                    {"name": "gatewayclasses", "singularName": "gatewayclass", "namespaced": false,
                     "kind": "GatewayClass", "verbs": ["delete"]},
                    {"name": "httproutes", "singularName": "httproute", "namespaced": true,
                     "kind": "HTTPRoute", "verbs": ["delete"]}
                ]
            })
        })
    }
    let none = ActionParams::new();
    let cases: [(&dyn ResourcePlugin, &str, Option<&str>, &str); 3] = [
        (
            &GatewayPlugin,
            "Gateway",
            Some("ns1"),
            "/apis/gateway.networking.k8s.io/v1/namespaces/ns1/gateways/web",
        ),
        (
            &GatewayClassPlugin,
            "GatewayClass",
            None,
            "/apis/gateway.networking.k8s.io/v1/gatewayclasses/web",
        ),
        (
            &RoutePlugin,
            "HTTPRoute",
            Some("ns1"),
            "/apis/gateway.networking.k8s.io/v1/namespaces/ns1/httproutes/web",
        ),
    ];
    for (plugin, kind, ns, path) in cases {
        let (client, sent) = fake_client_with(discovery);
        let o = named("gateway.networking.k8s.io/v1", kind, ns);
        assert_eq!(plugin.actions_for(&o), [DELETE_ACTION], "{kind}");
        plugin
            .run_action(&client, &o, &action::DELETE, &none)
            .await
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_delete(&sent[1], path);
    }
}

/// Kinds covered by #294 with typed delete: no discovery request.
#[tokio::test]
async fn more_plugins_delete() {
    let none = ActionParams::new();
    let cases: [DeleteCase; 7] = [
        (
            &CustomResourceDefinitionPlugin,
            "apiextensions.k8s.io/v1",
            "CustomResourceDefinition",
            None,
            "/apis/apiextensions.k8s.io/v1/customresourcedefinitions/web",
        ),
        (
            &WebhookConfigurationPlugin::MUTATING,
            "admissionregistration.k8s.io/v1",
            "MutatingWebhookConfiguration",
            None,
            "/apis/admissionregistration.k8s.io/v1/mutatingwebhookconfigurations/web",
        ),
        (
            &WebhookConfigurationPlugin::VALIDATING,
            "admissionregistration.k8s.io/v1",
            "ValidatingWebhookConfiguration",
            None,
            "/apis/admissionregistration.k8s.io/v1/validatingwebhookconfigurations/web",
        ),
        (
            &RolePlugin::ROLE,
            "rbac.authorization.k8s.io/v1",
            "Role",
            Some("ns1"),
            "/apis/rbac.authorization.k8s.io/v1/namespaces/ns1/roles/web",
        ),
        (
            &RolePlugin::CLUSTER_ROLE,
            "rbac.authorization.k8s.io/v1",
            "ClusterRole",
            None,
            "/apis/rbac.authorization.k8s.io/v1/clusterroles/web",
        ),
        (
            &RoleBindingPlugin::ROLE_BINDING,
            "rbac.authorization.k8s.io/v1",
            "RoleBinding",
            Some("ns1"),
            "/apis/rbac.authorization.k8s.io/v1/namespaces/ns1/rolebindings/web",
        ),
        (
            &RoleBindingPlugin::CLUSTER_ROLE_BINDING,
            "rbac.authorization.k8s.io/v1",
            "ClusterRoleBinding",
            None,
            "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings/web",
        ),
    ];
    let registry = PluginRegistry::with_builtins();
    for (plugin, api_version, kind, ns, path) in cases {
        let group = api_version.rsplit_once('/').map_or("", |(g, _)| g);
        assert_eq!(registry.plugin_for(group, kind).name(), plugin.name());
        let o = named(api_version, kind, ns);
        assert_eq!(plugin.actions_for(&o), [DELETE_ACTION], "{kind}");
        let s = run(plugin, &o, action::DELETE, &none).await;
        assert_delete(&s, path);
    }
}

/// VPA and ReferenceGrant are CRDs: delete goes through discovery.
#[tokio::test]
async fn vpa_and_reference_grant_delete_use_discovery() {
    fn discovery(path: &str) -> Option<Value> {
        let (group_version, kind, plural) = match path {
            "/apis/autoscaling.k8s.io/v1" => (
                "autoscaling.k8s.io/v1",
                "VerticalPodAutoscaler",
                "verticalpodautoscalers",
            ),
            "/apis/gateway.networking.k8s.io/v1beta1" => (
                "gateway.networking.k8s.io/v1beta1",
                "ReferenceGrant",
                "referencegrants",
            ),
            _ => return None,
        };
        Some(json!({
            "kind": "APIResourceList", "groupVersion": group_version,
            "resources": [{"name": plural, "singularName": "", "namespaced": true,
                           "kind": kind, "verbs": ["delete"]}]
        }))
    }
    let none = ActionParams::new();
    let cases: [(&dyn ResourcePlugin, &str, &str, &str); 2] = [
        (
            &VerticalPodAutoscalerPlugin,
            "autoscaling.k8s.io/v1",
            "VerticalPodAutoscaler",
            "/apis/autoscaling.k8s.io/v1/namespaces/ns1/verticalpodautoscalers/web",
        ),
        (
            &ReferenceGrantPlugin,
            "gateway.networking.k8s.io/v1beta1",
            "ReferenceGrant",
            "/apis/gateway.networking.k8s.io/v1beta1/namespaces/ns1/referencegrants/web",
        ),
    ];
    let registry = PluginRegistry::with_builtins();
    for (plugin, api_version, kind, path) in cases {
        let group = api_version.rsplit_once('/').map_or("", |(g, _)| g);
        assert_eq!(registry.plugin_for(group, kind).name(), plugin.name());
        let (client, sent) = fake_client_with(discovery);
        let o = named(api_version, kind, Some("ns1"));
        assert_eq!(plugin.actions_for(&o), [DELETE_ACTION], "{kind}");
        plugin
            .run_action(&client, &o, &action::DELETE, &none)
            .await
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_delete(&sent[1], path);
    }
}

/// Evict is a Pod action only: no other built-in plugin declares it, and
/// the Pod plugin sends a `policy/v1` Eviction for it.
#[tokio::test]
async fn evict_is_a_pod_action() {
    let registry = PluginRegistry::with_builtins();
    for plugin in registry.plugins() {
        let declares = plugin.actions().iter().any(|a| a.id == action::EVICT);
        assert_eq!(
            declares,
            plugin.name() == PodPlugin.name(),
            "{}",
            plugin.name()
        );
    }
    assert!(
        !GenericPlugin
            .actions()
            .iter()
            .any(|a| a.id == action::EVICT)
    );
    let (client, sent) = fake_client();
    let deploy = named("apps/v1", "Deployment", Some("ns1"));
    let r = DeploymentPlugin
        .run_action(&client, &deploy, &action::EVICT, &NO_PARAMS)
        .await;
    assert!(matches!(r, Err(PluginError::UnknownAction(_))), "{r:?}");
    assert!(sent.lock().unwrap().is_empty());

    let pod = named("v1", "Pod", Some("ns1"));
    let s = run(&PodPlugin, &pod, action::EVICT, &NO_PARAMS).await;
    assert_eq!(s.method, "POST");
    assert_eq!(s.uri, "/api/v1/namespaces/ns1/pods/web/eviction");
    assert_eq!(s.content_type.as_deref(), Some("application/json"));
    assert_eq!(
        s.body,
        json!({"apiVersion": "policy/v1", "kind": "Eviction",
               "metadata": {"name": "web", "namespace": "ns1"}})
    );
}

/// A client answering every request with `code` and `body`.
fn answering_client(code: u16, body: Value) -> Client {
    let service = tower::service_fn(move |_req: Request<Body>| {
        let body = body.clone();
        async move {
            Ok::<_, kube::Error>(
                Response::builder()
                    .status(code)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
        }
    });
    Client::new(service, "default")
}

/// In a bulk run, a budget refusal reads plainly, a pod already gone
/// counts as evicted and an RBAC refusal is flagged.
#[tokio::test]
async fn bulk_evict_explains_budgets_and_ignores_gone_pods() {
    let pods = vec![std::sync::Arc::new(named("v1", "Pod", Some("ns1")))];
    let plugin: std::sync::Arc<dyn ResourcePlugin> = std::sync::Arc::new(PodPlugin);
    let refused = answering_client(
        429,
        json!({"kind": "Status", "status": "Failure", "code": 429, "reason": "TooManyRequests",
               "message": "Cannot evict pod as it would violate the pod's disruption budget.",
               "details": {"causes": [{"reason": "DisruptionBudget",
                   "message": "The disruption budget web needs 2 healthy pods and has 2 currently"}]}}),
    );
    let summary = bulk::run_bulk(
        plugin.clone(),
        refused,
        pods.clone(),
        action::EVICT,
        |_, _| {},
    )
    .await;
    assert_eq!(summary.succeeded, 0);
    assert_eq!(summary.failures[0].object, "ns1/web");
    assert_eq!(
        summary.failures[0].reason.english(),
        "PodDisruptionBudget web does not allow evicting the pod now \
         (needs 2 healthy pods and has 2 currently)"
    );
    assert!(!summary.failures[0].forbidden);

    let gone = answering_client(
        404,
        json!({"kind": "Status", "status": "Failure", "code": 404, "reason": "NotFound"}),
    );
    let summary =
        bulk::run_bulk(plugin.clone(), gone, pods.clone(), action::EVICT, |_, _| {}).await;
    assert_eq!((summary.succeeded, summary.failures.len()), (1, 0));

    let forbidden = answering_client(
        403,
        json!({"kind": "Status", "status": "Failure", "code": 403, "reason": "Forbidden",
               "message": "pods \"web\" is forbidden"}),
    );
    let summary = bulk::run_bulk(plugin, forbidden, pods, action::EVICT, |_, _| {}).await;
    assert!(summary.any_forbidden());
}

/// Delete carries the UID of the object it was loaded with; already gone
/// counts as deleted, a recreated object is refused plainly (#345).
#[tokio::test]
async fn delete_guards_against_recreated_objects() {
    let pod = named("v1", "Pod", Some("ns1"));
    let s = run(&PodPlugin, &pod, action::DELETE, &ActionParams::new()).await;
    assert_delete(&s, "/api/v1/namespaces/ns1/pods/web");
    assert_eq!(s.body["preconditions"], json!({"uid": "u-1"}));

    let pods = vec![std::sync::Arc::new(pod)];
    let plugin: std::sync::Arc<dyn ResourcePlugin> = std::sync::Arc::new(PodPlugin);
    let gone = answering_client(
        404,
        json!({"kind": "Status", "status": "Failure", "code": 404, "reason": "NotFound"}),
    );
    let summary = bulk::run_bulk(
        plugin.clone(),
        gone,
        pods.clone(),
        action::DELETE,
        |_, _| {},
    )
    .await;
    assert_eq!((summary.succeeded, summary.failures.len()), (1, 0));

    let recreated = answering_client(
        409,
        json!({"kind": "Status", "status": "Failure", "code": 409, "reason": "Conflict",
               "message": "Operation cannot be fulfilled on pods \"web\": Precondition failed: \
                           UID in precondition: u-1, UID in object meta: u-2"}),
    );
    let summary = bulk::run_bulk(plugin, recreated, pods, action::DELETE, |_, _| {}).await;
    assert_eq!(summary.succeeded, 0);
    assert!(
        summary.failures[0].reason.english().contains("recreated"),
        "{}",
        summary.failures[0].reason
    );
}
