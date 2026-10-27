//! Shell targets in a pod with several containers against a real cluster:
//! a running container, a completed init container, a crashed container,
//! one without any shell and one with `/bin/sh` but no bash (#462). Ignored
//! by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-141 \
//!     cargo test -p kubach-core --test exec_target_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the test creates and deletes its own pod
//! (images `busybox:1.37`, `registry.k8s.io/pause:3.10.2`).

use std::time::Duration;

use futures::StreamExt as _;
use k8s_openapi::api::core::v1::Pod;
use kubach_core::streams::{
    ExecEvent, SessionOptions, StreamError, check_exec_target, exec_capture, start_shell,
};
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::json;

const POD: &str = "kubach-exec-targets";

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

fn container_running(pod: &Pod, name: &str) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.container_statuses.as_ref())
        .into_iter()
        .flatten()
        .any(|s| s.name == name && s.state.as_ref().is_some_and(|s| s.running.is_some()))
}

fn container_stopped(pod: &Pod, name: &str) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.container_statuses.as_ref())
        .into_iter()
        .flatten()
        .any(|s| s.name == name && s.state.as_ref().is_some_and(|s| s.running.is_none()))
}

#[tokio::test]
#[ignore = "needs a cluster: KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE"]
async fn explains_unreachable_shell_targets() {
    let (client, ns) = client().await;
    let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
    pods.delete(POD, &DeleteParams::default().grace_period(0))
        .await
        .ok();
    let pod: Pod = serde_json::from_value(json!({
        "metadata": {"name": POD},
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "initContainers": [{
                "name": "migrate", "image": "busybox:1.37",
                "imagePullPolicy": "IfNotPresent", "command": ["true"]
            }],
            "containers": [
                {"name": "app", "image": "busybox:1.37", "imagePullPolicy": "IfNotPresent",
                 "command": ["sleep", "3600"]},
                {"name": "pause", "image": "registry.k8s.io/pause:3.10.2",
                 "imagePullPolicy": "IfNotPresent"},
                {"name": "crasher", "image": "busybox:1.37", "imagePullPolicy": "IfNotPresent",
                 "command": ["sh", "-c", "exit 3"]}
            ]
        }
    }))
    .unwrap();
    // Recreating right after a delete may hit the terminating pod.
    let mut created = false;
    for _ in 0..30 {
        if pods.create(&PostParams::default(), &pod).await.is_ok() {
            created = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(created, "pod not created");

    let mut ready = false;
    for _ in 0..60 {
        let pod = pods.get(POD).await.unwrap();
        if container_running(&pod, "app")
            && container_running(&pod, "pause")
            && container_stopped(&pod, "crasher")
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(ready, "pod did not start");

    // The chosen container is the one exec reaches.
    check_exec_target(&client, &ns, POD, "app").await.unwrap();
    let capture = exec_capture(
        &client,
        &ns,
        POD,
        Some("app"),
        ["sh", "-c", "echo $HOSTNAME"],
        64 * 1024,
    )
    .await
    .unwrap();
    assert!(capture.status.success());

    let error = check_exec_target(&client, &ns, POD, "migrate")
        .await
        .unwrap_err();
    assert!(
        matches!(&error, StreamError::ContainerNotRunning { container, .. } if container == "migrate"),
        "{error}"
    );
    assert!(error.to_string().contains("Completed"), "{error}");

    let error = check_exec_target(&client, &ns, POD, "crasher")
        .await
        .unwrap_err();
    assert!(
        matches!(&error, StreamError::ContainerNotRunning { .. }),
        "{error}"
    );

    // busybox has /bin/sh but no bash: the shell falls back to it and runs.
    let options = SessionOptions {
        container: Some("app".into()),
        ..SessionOptions::default()
    };
    let started = start_shell(&client, &ns, POD, &options).await.unwrap();
    assert_eq!(started.shell, "/bin/sh");
    assert_eq!(started.missing, ["/bin/bash"]);
    let mut session = started.session;
    session
        .input()
        .write(b"echo kubach-$((40+2))\nexit\n".to_vec());
    let mut output = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(20), session.next())
        .await
        .unwrap()
    {
        match event {
            ExecEvent::Stdout(bytes) | ExecEvent::Stderr(bytes) => output.extend(bytes),
            ExecEvent::Exited(status) => {
                assert!(status.success(), "{status:?}");
                break;
            }
        }
    }
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("kubach-42"), "{output}");

    // pause is running but has no shell at all: every candidate is tried
    // and the error says so.
    check_exec_target(&client, &ns, POD, "pause").await.unwrap();
    let options = SessionOptions {
        container: Some("pause".into()),
        ..SessionOptions::default()
    };
    let error = start_shell(&client, &ns, POD, &options)
        .await
        .err()
        .expect("pause has no shell");
    assert!(
        matches!(&error, StreamError::NoShell { container } if container == "pause"),
        "{error}"
    );

    pods.delete(POD, &DeleteParams::default().grace_period(0))
        .await
        .ok();
}
