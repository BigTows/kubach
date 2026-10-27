//! The pods of workloads (#393) against a real cluster: the owner chains
//! the controllers build (Deployment → ReplicaSet → Pod, CronJob → Job →
//! Pod) and direct owners. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-393 \
//!     cargo test -p kubach-core --test workload_pods_cluster -- --ignored --test-threads=1
//! ```
//!
//! The namespace must exist; the test creates and deletes objects in it
//! only. It waits for the CronJob's first run (scheduled every minute).

use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::jiff::Timestamp;
use kubach_core::workload_pods::{PodRow, Workload, pods_of};
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, ListParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::GroupVersionKind;
use kube::{Client, Config};
use serde_json::json;

const WAIT: Duration = Duration::from_secs(110);

async fn client() -> (Client, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    let kubeconfig = Kubeconfig::read().unwrap();
    let options = KubeConfigOptions {
        context: Some(context),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .unwrap();
    (kubach_core::redact::client(config).unwrap(), namespace)
}

fn api(client: &Client, ns: &str, group: &str, version: &str, kind: &str) -> Api<DynamicObject> {
    let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind));
    Api::namespaced_with(client.clone(), ns, &resource)
}

async fn list(api: &Api<DynamicObject>) -> Vec<Arc<DynamicObject>> {
    api.list(&ListParams::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .map(Arc::new)
        .collect()
}

fn template(app: &str, restart: &str) -> serde_json::Value {
    json!({
        "metadata": {"labels": {"app": app}},
        "spec": {"restartPolicy": restart, "terminationGracePeriodSeconds": 0,
                 "containers": [{"name": "c", "image": "busybox:1.36",
                                 "command": ["sh", "-c", "sleep 3600"]}]}
    })
}

fn job_template(app: &str) -> serde_json::Value {
    json!({
        "metadata": {"labels": {"app": app}},
        "spec": {"restartPolicy": "Never", "terminationGracePeriodSeconds": 0,
                 "containers": [{"name": "c", "image": "busybox:1.36",
                                 "command": ["sh", "-c", "sleep 5"]}]}
    })
}

struct Fixture {
    client: Client,
    ns: String,
}

impl Fixture {
    fn api(&self, group: &str, version: &str, kind: &str) -> Api<DynamicObject> {
        api(&self.client, &self.ns, group, version, kind)
    }

    async fn create(&self, group: &str, version: &str, kind: &str, body: serde_json::Value) {
        let object: DynamicObject = serde_json::from_value(body).unwrap();
        self.api(group, version, kind)
            .create(&PostParams::default(), &object)
            .await
            .unwrap();
    }

    async fn get(&self, group: &str, version: &str, kind: &str, name: &str) -> DynamicObject {
        self.api(group, version, kind).get(name).await.unwrap()
    }

    /// Names of the pods of `workload` `name` as the details pick them.
    async fn pods(&self, workload: Workload, group: &str, kind: &str, name: &str) -> Vec<PodRow> {
        let object = self.get(group, "v1", kind, name).await;
        let intermediates = match workload.intermediate() {
            Some((group, kind)) => Some(list(&self.api(group, "v1", kind)).await),
            None => None,
        };
        let pods = list(&self.api("", "v1", "Pod")).await;
        pods_of(workload, &object, intermediates.as_deref(), &pods)
            .iter()
            .map(|pod| PodRow::of(pod, Timestamp::now()))
            .collect()
    }

    async fn delete(&self, group: &str, kind: &str, name: &str) {
        let params = DeleteParams {
            propagation_policy: Some(kube::api::PropagationPolicy::Foreground),
            grace_period_seconds: Some(0),
            ..DeleteParams::default()
        };
        let _ = self.api(group, "v1", kind).delete(name, &params).await;
    }
}

/// Polls `check` until it holds or [`WAIT`] runs out.
async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    while !check().await {
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[tokio::test]
#[ignore = "needs a cluster: KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE"]
async fn pods_of_each_workload_kind() {
    let (client, ns) = client().await;
    let fx = Fixture { client, ns };
    fx.create(
        "apps",
        "v1",
        "Deployment",
        json!({"apiVersion": "apps/v1", "kind": "Deployment",
               "metadata": {"name": "pods-web"},
               "spec": {"replicas": 2, "selector": {"matchLabels": {"app": "pods-web"}},
                        "template": template("pods-web", "Always")}}),
    )
    .await;
    // Same labels, another owner: never a pod of pods-web.
    fx.create(
        "apps",
        "v1",
        "Deployment",
        json!({"apiVersion": "apps/v1", "kind": "Deployment",
               "metadata": {"name": "pods-twin"},
               "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "pods-web"}},
                        "template": template("pods-web", "Always")}}),
    )
    .await;
    fx.create(
        "apps",
        "v1",
        "StatefulSet",
        json!({"apiVersion": "apps/v1", "kind": "StatefulSet",
               "metadata": {"name": "pods-db"},
               "spec": {"replicas": 1, "serviceName": "pods-db",
                        "selector": {"matchLabels": {"app": "pods-db"}},
                        "template": template("pods-db", "Always")}}),
    )
    .await;
    fx.create(
        "batch",
        "v1",
        "Job",
        json!({"apiVersion": "batch/v1", "kind": "Job",
               "metadata": {"name": "pods-report"},
               "spec": {"template": job_template("pods-report")}}),
    )
    .await;
    fx.create(
        "batch",
        "v1",
        "CronJob",
        json!({"apiVersion": "batch/v1", "kind": "CronJob",
               "metadata": {"name": "pods-tick"},
               "spec": {"schedule": "* * * * *",
                        "jobTemplate": {"spec": {"template": job_template("pods-tick")}}}}),
    )
    .await;

    let result = async {
        eventually("2 pods of the Deployment", || async {
            fx.pods(Workload::Deployment, "apps", "Deployment", "pods-web")
                .await
                .len()
                == 2
        })
        .await;
        let web = fx
            .pods(Workload::Deployment, "apps", "Deployment", "pods-web")
            .await;
        assert!(web.iter().all(|row| row.name.starts_with("pods-web-")));
        let twin = fx
            .pods(Workload::Deployment, "apps", "Deployment", "pods-twin")
            .await;
        assert!(twin.iter().all(|row| row.name.starts_with("pods-twin-")));

        // The ReplicaSet itself owns the same pods.
        let sets = list(&fx.api("apps", "v1", "ReplicaSet")).await;
        let set = sets
            .iter()
            .find(|s| {
                s.metadata
                    .name
                    .as_deref()
                    .is_some_and(|n| n.starts_with("pods-web-"))
            })
            .unwrap();
        let of_set = fx
            .pods(
                Workload::ReplicaSet,
                "apps",
                "ReplicaSet",
                set.metadata.name.as_deref().unwrap(),
            )
            .await;
        assert_eq!(
            of_set.iter().map(|r| &r.name).collect::<Vec<_>>(),
            web.iter().map(|r| &r.name).collect::<Vec<_>>()
        );

        eventually("the StatefulSet's pod", || async {
            fx.pods(Workload::StatefulSet, "apps", "StatefulSet", "pods-db")
                .await
                .len()
                == 1
        })
        .await;
        let db = fx
            .pods(Workload::StatefulSet, "apps", "StatefulSet", "pods-db")
            .await;
        assert_eq!(db[0].name, "pods-db-0");

        eventually("the Job's pod", || async {
            fx.pods(Workload::Job, "batch", "Job", "pods-report")
                .await
                .len()
                == 1
        })
        .await;

        eventually("a pod of the CronJob's first run", || async {
            !fx.pods(Workload::CronJob, "batch", "CronJob", "pods-tick")
                .await
                .is_empty()
        })
        .await;
        let tick = fx
            .pods(Workload::CronJob, "batch", "CronJob", "pods-tick")
            .await;
        assert!(tick.iter().all(|row| row.name.starts_with("pods-tick-")));
        // The Job's pod is not the CronJob's.
        assert!(tick.iter().all(|row| !row.name.starts_with("pods-report")));

        eventually("the Deployment's pods running", || async {
            fx.pods(Workload::Deployment, "apps", "Deployment", "pods-web")
                .await
                .iter()
                .all(|row| {
                    row.status == "Running" && row.ready_text() == "1/1" && row.node.is_some()
                })
        })
        .await;
    };
    // Clean up even when an assertion fails.
    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(result)).await;
    fx.delete("batch", "CronJob", "pods-tick").await;
    fx.delete("batch", "Job", "pods-report").await;
    fx.delete("apps", "StatefulSet", "pods-db").await;
    fx.delete("apps", "Deployment", "pods-twin").await;
    fx.delete("apps", "Deployment", "pods-web").await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
