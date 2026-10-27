//! Node drain and node shell (#318) against a real single-node cluster.
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_CONFIG_DIR=$(mktemp -d) \
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-318 \
//!     cargo test -p kubach-core --test node_ops_cluster -- --ignored --test-threads=1
//! ```
//!
//! The namespace must exist. The drain test evicts only pods of that
//! namespace and uncordons the node right after, whatever happens; the
//! system pods are only planned, never evicted.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use futures::StreamExt as _;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Namespace, Node, Pod};
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use k8s_openapi::jiff::{SignedDuration, Timestamp};
use kubach_core::drain::{self, DrainEvent, DrainOptions, Skip, Verdict};
use kubach_core::streams::node_shell;
use kubach_core::streams::node_shell::owner::{self, Owner};
use kubach_core::streams::{ExecEvent, SessionCommand, SessionOptions, start_session};
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config, ResourceExt as _};
use serde_json::json;

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
    (Client::try_from(config).unwrap(), namespace)
}

async fn only_node(client: &Client) -> String {
    let nodes: Api<Node> = Api::all(client.clone());
    let list = nodes.list(&ListParams::default()).await.unwrap();
    assert_eq!(list.items.len(), 1, "expects a single-node cluster");
    list.items[0].name_any()
}

async fn uncordon(client: &Client, node: &str) {
    let nodes: Api<Node> = Api::all(client.clone());
    nodes
        .patch(
            node,
            &PatchParams::default(),
            &Patch::Strategic(json!({"spec": {"unschedulable": null}})),
        )
        .await
        .unwrap();
}

async fn wait_running(pods: &Api<Pod>, selector: &str, count: usize) {
    for _ in 0..240 {
        let list = pods
            .list(&ListParams::default().labels(selector))
            .await
            .unwrap();
        let running = list
            .items
            .iter()
            .filter(|p| p.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running"))
            .count();
        if running >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("pods {selector} did not run");
}

/// The system pods of the node are only planned: mirror pods and
/// DaemonSet pods stay, nothing of kube-system is evicted.
#[tokio::test]
#[ignore = "needs a cluster: KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE"]
async fn plans_system_pods_without_touching_them() {
    let (client, _) = client().await;
    let node = only_node(&client).await;
    let pods = drain::node_pods(&client, &node, Some("kube-system"))
        .await
        .unwrap();
    let plan = drain::plan(&pods, &DrainOptions::default());
    let verdict = |prefix: &str| {
        plan.pods
            .iter()
            .find(|(key, _)| key.name.starts_with(prefix))
            .map(|(_, v)| *v)
    };
    assert_eq!(verdict("etcd-"), Some(Verdict::Skip(Skip::Mirror)));
    assert_eq!(
        verdict("kube-apiserver-"),
        Some(Verdict::Skip(Skip::Mirror))
    );
    assert_eq!(verdict("kube-proxy-"), Some(Verdict::Skip(Skip::DaemonSet)));
    // CoreDNS has a ReplicaSet: it would be evicted.
    assert_eq!(verdict("coredns-"), Some(Verdict::Evict));
    // storage-provisioner on minikube has no controller: it blocks.
    if let Some(v) = verdict("storage-provisioner") {
        assert_eq!(v, Verdict::Block(drain::Block::NoController));
    }
}

#[tokio::test]
#[ignore = "needs a cluster: KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE"]
async fn drains_pods_of_one_namespace() {
    let (client, ns) = client().await;
    let node = only_node(&client).await;
    let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
    let deploys: Api<Deployment> = Api::namespaced(client.clone(), &ns);
    let pdbs: Api<PodDisruptionBudget> = Api::namespaced(client.clone(), &ns);
    let deployment = |name: &str, replicas: i32, empty_dir: bool| -> Deployment {
        let mut spec = json!({
            "terminationGracePeriodSeconds": 0,
            "containers": [{"name": "c", "image": "busybox:1.37",
                            "imagePullPolicy": "IfNotPresent", "command": ["sleep", "3600"]}]
        });
        if empty_dir {
            spec["volumes"] = json!([{"name": "scratch", "emptyDir": {}}]);
        }
        serde_json::from_value(json!({
            "metadata": {"name": name},
            "spec": {
                "replicas": replicas,
                "selector": {"matchLabels": {"app": name}},
                "template": {"metadata": {"labels": {"app": name}}, "spec": spec}
            }
        }))
        .unwrap()
    };
    let create = PostParams::default();
    deploys
        .create(&create, &deployment("web", 1, false))
        .await
        .unwrap();
    deploys
        .create(&create, &deployment("cache", 1, true))
        .await
        .unwrap();
    // Blocks every eviction of guarded: minAvailable equals its replicas.
    deploys
        .create(&create, &deployment("guarded", 1, false))
        .await
        .unwrap();
    let pdb: PodDisruptionBudget = serde_json::from_value(json!({
        "metadata": {"name": "guarded"},
        "spec": {"minAvailable": 1, "selector": {"matchLabels": {"app": "guarded"}}}
    }))
    .unwrap();
    pdbs.create(&create, &pdb).await.unwrap();
    let bare: Pod = serde_json::from_value(json!({
        "metadata": {"name": "bare", "labels": {"app": "bare"}},
        "spec": {"terminationGracePeriodSeconds": 0,
                 "containers": [{"name": "c", "image": "busybox:1.37",
                                 "imagePullPolicy": "IfNotPresent", "command": ["sleep", "3600"]}]}
    }))
    .unwrap();
    pods.create(&create, &bare).await.unwrap();
    let done: Pod = serde_json::from_value(json!({
        "metadata": {"name": "done", "labels": {"app": "done"}},
        "spec": {"restartPolicy": "Never",
                 "containers": [{"name": "c", "image": "busybox:1.37",
                                 "imagePullPolicy": "IfNotPresent", "command": ["true"]}]}
    }))
    .unwrap();
    pods.create(&create, &done).await.unwrap();
    for app in ["web", "cache", "guarded", "bare"] {
        wait_running(&pods, &format!("app={app}"), 1).await;
    }
    for _ in 0..120 {
        let phase = pods.get("done").await.unwrap().status.and_then(|s| s.phase);
        if phase.as_deref() == Some("Succeeded") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Without the options the plan is blocked and nothing is evicted.
    let mut options = DrainOptions {
        namespace: Some(ns.clone()),
        timeout: Duration::from_secs(15),
        ..DrainOptions::default()
    };
    let listed = drain::node_pods(&client, &node, Some(&ns)).await.unwrap();
    let plan = drain::plan(&listed, &options);
    assert!(plan.is_blocked());
    let blocked: Vec<String> = plan.blocked().map(|(k, _)| k.name.clone()).collect();
    assert!(blocked.contains(&"bare".to_owned()), "{blocked:?}");
    assert!(
        blocked.iter().any(|n| n.starts_with("cache-")),
        "{blocked:?}"
    );

    options.force = true;
    options.delete_emptydir_data = true;
    let mut events = Vec::new();
    let report = drain::drain(
        client.clone(),
        node.clone(),
        options,
        Arc::new(AtomicBool::new(false)),
        |event| events.push(event),
    )
    .await;
    // Uncordon before any assertion can fail.
    uncordon(&client, &node).await;

    assert_eq!(events.first(), Some(&DrainEvent::Planned { total: 5 }));
    assert_eq!(events.len(), 6, "{events:?}");
    // web, cache, bare evicted; done deleted; guarded refused until the
    // timeout.
    assert_eq!(report.summary.succeeded, 4, "{report:?}");
    assert_eq!(report.summary.failures.len(), 1, "{report:?}");
    let failure = &report.summary.failures[0];
    assert!(failure.object.starts_with(&format!("{ns}/guarded-")));
    let reason = failure.reason.english();
    assert!(reason.contains("PodDisruptionBudget guarded"), "{reason}");
    assert!(reason.contains("timeout"), "{reason}");
    assert!(pods.get_opt("bare").await.unwrap().is_none());
    assert!(pods.get_opt("done").await.unwrap().is_none());

    for name in ["web", "cache", "guarded"] {
        deploys.delete(name, &DeleteParams::default()).await.ok();
    }
    pdbs.delete("guarded", &DeleteParams::default()).await.ok();
    let nodes: Api<Node> = Api::all(client.clone());
    let unschedulable = nodes
        .get(&node)
        .await
        .unwrap()
        .spec
        .and_then(|s| s.unschedulable);
    assert_ne!(unschedulable, Some(true));
}

#[tokio::test]
#[ignore = "needs a cluster: KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE"]
async fn node_shell_enters_the_host_and_cleans_up() {
    let (client, ns) = client().await;
    let node = only_node(&client).await;
    let image = "busybox:1.37";
    let node_shell::CreatedPod { name, uid } =
        node_shell::create_shell_pod(&client, &node, &ns, image)
            .await
            .unwrap();
    node_shell::wait_running(&client, &ns, &name, node_shell::START_TIMEOUT)
        .await
        .unwrap();

    // The same TTY session the dock tab opens.
    let options = SessionOptions {
        container: Some("shell".into()),
        size: Some((120, 40)),
        ..SessionOptions::default()
    };
    let session = start_session(
        &client,
        &ns,
        &name,
        SessionCommand::Exec(node_shell::shell_command()),
        &options,
    )
    .await
    .unwrap();
    let input = session.input();
    input.write(
        b"hostname; grep ^ID= /etc/os-release; echo PID1=$(cat /proc/1/comm)\nexit\n".to_vec(),
    );
    let mut output = Vec::new();
    let mut events = session;
    let collect = async {
        while let Some(event) = events.next().await {
            match event {
                ExecEvent::Stdout(bytes) | ExecEvent::Stderr(bytes) => {
                    output.extend_from_slice(&bytes)
                }
                ExecEvent::Exited(_) => break,
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(30), collect)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains(&node), "{text}");
    // The host's os-release, not busybox's (which has none).
    assert!(text.contains("\nID="), "{text}");
    assert!(
        text.contains("PID1=systemd") || text.contains("PID1=init"),
        "{text}"
    );

    // An orphan (of this installation, its process gone) is swept; the
    // live one stays, and pods without both labels are never touched.
    let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
    let orphan = pods
        .create(
            &PostParams::default(),
            &node_shell::shell_pod_of(&dead_owner(), Timestamp::now(), &node, &ns, image),
        )
        .await
        .unwrap()
        .name_any();
    let foreign: Pod = serde_json::from_value(json!({
        "metadata": {"name": "foreign", "labels": {node_shell::MANAGED_BY_LABEL: "kubach"}},
        "spec": {"terminationGracePeriodSeconds": 0,
                 "containers": [{"name": "c", "image": image,
                                 "imagePullPolicy": "IfNotPresent", "command": ["sleep", "3600"]}]}
    }))
    .unwrap();
    pods.create(&PostParams::default(), &foreign).await.unwrap();
    let swept = node_shell::sweep_orphans(&client, &ns).await.unwrap();
    assert_eq!(swept, 1);
    let gone = |name: String| {
        let pods = pods.clone();
        async move {
            for _ in 0..60 {
                if pods.get_opt(&name).await.unwrap().is_none() {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            false
        }
    };
    assert!(gone(orphan).await);
    assert!(pods.get_opt(&name).await.unwrap().is_some());
    assert!(pods.get_opt("foreign").await.unwrap().is_some());
    pods.delete("foreign", &DeleteParams::default().grace_period(0))
        .await
        .unwrap();

    // Closing the tab deletes the pod at once.
    node_shell::delete_shell_pod(&client, &ns, &name, uid.as_deref())
        .await
        .unwrap();
    assert!(gone(name).await);
}

/// This installation with the pid of a process that has exited.
fn dead_owner() -> Owner {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    Owner {
        pid,
        ..Owner::current().clone()
    }
}

async fn gone(pods: &Api<Pod>, name: &str) -> bool {
    for _ in 0..60 {
        if pods.get_opt(name).await.unwrap().is_none() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

/// A sweep keeps this process's live pod and live pods of other
/// installations, and deletes its own orphans and long-stale pods of
/// others (#351). The pods need not run: the sweep goes by metadata.
#[tokio::test]
#[ignore = "needs a cluster: KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE"]
async fn node_shell_sweep_respects_other_installations() {
    let (client, ns) = client().await;
    let node = only_node(&client).await;
    let image = node_shell::DEFAULT_IMAGE;
    let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
    let other = Owner {
        install_id: uuid_like("2"),
        pid: 4242,
        host: "another-machine".into(),
    };
    let create = |owner: Owner, heartbeat: Timestamp| {
        let pods = pods.clone();
        let pod = node_shell::shell_pod_of(&owner, heartbeat, &node, &ns, image);
        async move {
            let pod = pods.create(&PostParams::default(), &pod).await.unwrap();
            (pod.name_any(), pod.uid())
        }
    };
    let mins = |m: i64| Timestamp::now() - SignedDuration::from_mins(m);

    // Own live pod: registered by create_shell_pod, heartbeat refreshed.
    let live = node_shell::create_shell_pod(&client, &node, &ns, image)
        .await
        .unwrap();
    let before =
        pods.get(&live.name).await.unwrap().annotations()[owner::HEARTBEAT_ANNOTATION].clone();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    node_shell::heartbeat(&client, &ns, &live.name, live.uid.as_deref())
        .await
        .unwrap();
    let after =
        pods.get(&live.name).await.unwrap().annotations()[owner::HEARTBEAT_ANNOTATION].clone();
    assert_ne!(before, after);
    // A heartbeat with a stale UID does not touch the pod.
    assert!(
        node_shell::heartbeat(
            &client,
            &ns,
            &live.name,
            Some("00000000-0000-0000-0000-000000000000")
        )
        .await
        .is_err()
    );

    let (fresh, _) = create(other.clone(), Timestamp::now()).await;
    let (recent, _) = create(other.clone(), mins(10)).await;
    // Stale "after the fact": created fresh, then its heartbeat backdated.
    let (stale, _) = create(other.clone(), Timestamp::now()).await;
    pods.patch(
        &stale,
        &PatchParams::default(),
        &Patch::Merge(json!({"metadata": {"annotations": {
            owner::HEARTBEAT_ANNOTATION: mins(20).to_string()
        }}})),
    )
    .await
    .unwrap();
    let (orphan, _) = create(dead_owner(), Timestamp::now()).await;
    // This pid, not registered (a tab whose delete failed), past the grace.
    let (lost, _) = create(Owner::current().clone(), mins(3)).await;

    let swept = node_shell::sweep_orphans(&client, &ns).await.unwrap();
    assert_eq!(swept, 3);
    assert!(gone(&pods, &stale).await);
    assert!(gone(&pods, &orphan).await);
    assert!(gone(&pods, &lost).await);
    for kept in [&live.name, &fresh, &recent] {
        assert!(
            pods.get_opt(kept).await.unwrap().is_some(),
            "{kept} was swept"
        );
    }
    // Own pods by the owner label.
    let own = pods
        .list(&ListParams::default().labels(&Owner::current().selector()))
        .await
        .unwrap();
    let names: Vec<_> = own.items.iter().map(|p| p.name_any()).collect();
    assert_eq!(names, std::slice::from_ref(&live.name));

    for name in [&fresh, &recent] {
        pods.delete(name, &DeleteParams::default().grace_period(0))
            .await
            .unwrap();
    }
    node_shell::delete_shell_pod(&client, &ns, &live.name, live.uid.as_deref())
        .await
        .unwrap();
    assert!(gone(&pods, &live.name).await);
}

fn uuid_like(digit: &str) -> String {
    format!(
        "{0}{0}{0}{0}{0}{0}{0}{0}-{0}{0}{0}{0}-4{0}{0}{0}-8{0}{0}{0}-{0}{0}{0}{0}{0}{0}{0}{0}{0}{0}{0}{0}",
        digit
    )
}

/// Pod Security admission refusing privileged pods reads as such.
#[tokio::test]
#[ignore = "needs a cluster: KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE"]
async fn explains_pod_security_refusal() {
    let (client, ns) = client().await;
    let node = only_node(&client).await;
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let label = |value: Option<&str>| {
        Patch::Merge(json!({"metadata": {"labels": {
            "pod-security.kubernetes.io/enforce": value
        }}}))
    };
    namespaces
        .patch(&ns, &PatchParams::default(), &label(Some("baseline")))
        .await
        .unwrap();
    let result = node_shell::create_shell_pod(&client, &node, &ns, "busybox:1.37").await;
    namespaces
        .patch(&ns, &PatchParams::default(), &label(None))
        .await
        .unwrap();
    let error = result.unwrap_err().to_string();
    assert!(
        error.starts_with(&format!("Pod Security admission in namespace {ns}")),
        "{error}"
    );
}
