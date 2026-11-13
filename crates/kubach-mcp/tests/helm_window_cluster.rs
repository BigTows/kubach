//! What the Helm confirmation window and the checks of a chart's objects
//! see (#179), against a real cluster and a real `helm`. Ignored by default;
//! run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-179 \
//! HELM_CONFIG_HOME=$(mktemp -d) HELM_CACHE_HOME=$(mktemp -d) HELM_DATA_HOME=$(mktemp -d) \
//!     cargo test -p kubach-mcp --test helm_window_cluster -- --ignored
//! ```
//!
//! The test creates and deletes the namespace and a CustomResourceDefinition
//! itself and needs no registry: the chart comes from a repository served on
//! 127.0.0.1 for the test.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Namespace, Pod};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kubach_core::discovery::ApiCatalog;
use kubach_core::helm::repo::HelmPaths;
use kubach_mcp::{
    ConfirmationRequest, HelmAction, HelmChange, HelmCredentials, HelmEnv, McpServer, Profile,
    RequestKind, ScriptedConfirmer, SecurityPolicy, Session, SessionSpec,
};
use kube::api::{Api, DeleteParams, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

const REPOSITORY: &str = "kubach-test-179";
const CHART: &str = "kubach-test-179/window";
const RELEASE: &str = "window";
/// A cluster-scoped kind that appears after the session has started.
const GROUP: &str = "kubach-test-179.example.com";
const KIND: &str = "Probe179";
const CRD: &str = "probe179s.kubach-test-179.example.com";

const OBJECTS: &str = r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ .Release.Name }}-settings
data:
  note: {{ .Values.note | quote }}
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ .Release.Name }}-web
spec:
  replicas: 1
  selector:
    matchLabels:
      app: {{ .Release.Name }}-web
  template:
    metadata:
      labels:
        app: {{ .Release.Name }}-web
    spec:
      containers:
        - name: web
          image: registry.k8s.io/pause:3.9
{{- if .Values.list }}
---
apiVersion: v1
kind: List
items:
  - apiVersion: rbac.authorization.k8s.io/v1
    kind: Role
    metadata:
      name: {{ .Release.Name }}-in-a-list
    rules: []
  - apiVersion: v1
    kind: ConfigMap
    metadata:
      name: {{ .Release.Name }}-in-a-list
{{- end }}
{{- if .Values.generated }}
---
apiVersion: v1
kind: ConfigMap
metadata:
  generateName: {{ .Release.Name }}-generated-
{{- end }}
{{- if .Values.repeated }}
---
apiVersion: v1
kind: ConfigMap
kind: Secret
metadata:
  name: {{ .Release.Name }}-repeated
{{- end }}
{{- if .Values.custom }}
---
apiVersion: kubach-test-179.example.com/v1
kind: Probe179
metadata:
  name: {{ .Release.Name }}-probe
{{- end }}
"#;

const HOOK: &str = r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ .Release.Name }}-hook
  annotations:
    helm.sh/hook: pre-install,pre-upgrade,pre-rollback
data:
  note: {{ .Values.hookNote | quote }}
"#;

const VALUES: &str = "note: hello\nhookNote: first\nlist: false\ngenerated: false\n\
                      repeated: false\ncustom: false\n";

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

fn helm(arguments: &[&str]) {
    let binary = kubach_core::settings::find_on_path("helm").expect("helm on PATH");
    let output = std::process::Command::new(binary)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "helm {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Packages the chart, serves it on 127.0.0.1 and adds the repository to
/// the test's helm home.
fn repository() {
    let dir = tempfile::tempdir().unwrap();
    let chart = dir.path().join("window");
    std::fs::create_dir_all(chart.join("templates")).unwrap();
    std::fs::write(
        chart.join("Chart.yaml"),
        "apiVersion: v2\nname: window\nversion: 0.1.0\n",
    )
    .unwrap();
    std::fs::write(chart.join("values.yaml"), VALUES).unwrap();
    std::fs::write(chart.join("templates/objects.yaml"), OBJECTS).unwrap();
    std::fs::write(chart.join("templates/hook.yaml"), HOOK).unwrap();
    helm(&[
        "package",
        chart.to_str().unwrap(),
        "--destination",
        dir.path().to_str().unwrap(),
    ]);
    let archive = std::fs::read(dir.path().join("window-0.1.0.tgz")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let index = format!(
        "apiVersion: v1\nentries:\n  window:\n  - apiVersion: v2\n    name: window\n    \
         version: 0.1.0\n    urls:\n    - {url}/window-0.1.0.tgz\n\
         generated: \"2026-01-01T00:00:00Z\"\n"
    );
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            let path = request.split(' ').nth(1).unwrap_or_default();
            let body: Vec<u8> = if path.ends_with("index.yaml") {
                index.clone().into_bytes()
            } else if path.ends_with(".tgz") {
                archive.clone()
            } else {
                Vec::new()
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    helm(&["repo", "add", "--force-update", REPOSITORY, &url]);
}

async fn namespace_gone(namespaces: &Api<Namespace>, name: &str) -> bool {
    let _ = namespaces.delete(name, &DeleteParams::default()).await;
    for _ in 0..90 {
        if matches!(namespaces.get_opt(name).await, Ok(None)) {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    false
}

async fn crd_gone(crds: &Api<CustomResourceDefinition>) -> bool {
    let _ = crds.delete(CRD, &DeleteParams::default()).await;
    for _ in 0..60 {
        if matches!(crds.get_opt(CRD).await, Ok(None)) {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    false
}

/// The Helm windows so far, in order.
fn helm_windows(confirmer: &ScriptedConfirmer) -> Vec<(HelmChange, ConfirmationRequest)> {
    confirmer
        .requests()
        .into_iter()
        .filter_map(|request| match &request.kind {
            RequestKind::Helm { helm } => Some(((**helm).clone(), request.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster and helm (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn the_window_and_the_checks_see_what_helm_does() {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    for home in ["HELM_CONFIG_HOME", "HELM_CACHE_HOME", "HELM_DATA_HOME"] {
        assert!(
            std::env::var_os(home).is_some(),
            "{home} must point to a temporary directory: the user's helm stays untouched"
        );
    }
    let options = KubeConfigOptions {
        context: Some(context.clone()),
        ..Default::default()
    };
    let base = Config::from_custom_kubeconfig(Kubeconfig::read().unwrap(), &options)
        .await
        .unwrap();
    let user = Client::try_from(base.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let crds: Api<CustomResourceDefinition> = Api::all(user.clone());
    assert!(namespace_gone(&namespaces, &namespace).await);
    assert!(crd_gone(&crds).await);
    let object: Namespace =
        serde_json::from_value(json!({"metadata": {"name": namespace}})).unwrap();
    namespaces
        .create(&Default::default(), &object)
        .await
        .unwrap();
    let flow_namespace = namespace.clone();
    let flow = std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(flow(base, context, flow_namespace))
    });
    let outcome = tokio::task::spawn_blocking(move || flow.join())
        .await
        .unwrap();
    let crd_dropped = crd_gone(&crds).await;
    let dropped = namespace_gone(&namespaces, &namespace).await;
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(problems) => assert!(problems.is_empty(), "{problems:#?}"),
    }
    assert!(
        crd_dropped,
        "the test's CustomResourceDefinition is still there"
    );
    assert!(dropped, "the test's namespace is still there");
}

async fn flow(base: Config, context: String, namespace: String) -> Vec<String> {
    let mut problems = Vec::new();
    let user = Client::try_from(base.clone()).unwrap();
    repository();
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(900)),
        hints: Vec::new(),
    };
    let policy = SecurityPolicy {
        mcp_enabled: true,
        temporary_objects: true,
        ..SecurityPolicy::default()
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    server.set_helm(HelmEnv {
        binary: kubach_core::settings::find_on_path("helm"),
        kubeconfig: Some(kubach_core::helm::cli::kubeconfig_files(&[])),
        vault: None,
        paths: HelmPaths::from_env(),
    });
    let install = |values: &str, preview: bool| {
        json!({"release": RELEASE, "chart": CHART, "namespace": namespace,
               "values": values, "preview": preview})
    };

    // ---- A document without a name, and one with a repeated key: helm
    // renders both; the call ends before anybody is asked. The number is
    // the document's place in helm's render, which sorts objects by kind.
    for (values, reason) in [
        ("generated: true\n", "generateName is not a name"),
        ("repeated: true\n", "repeated key"),
    ] {
        for preview in [true, false] {
            let refused = server
                .call("test", "helm_install", args(install(values, preview)))
                .await;
            if !refused.is_error
                || !refused.text.contains(reason)
                || !refused.text.contains("(document ")
            {
                problems.push(format!(
                    "{values:?} preview={preview} was not refused by its document: {}",
                    refused.text
                ));
            }
        }
    }
    if !helm_windows(&confirmer).is_empty() {
        problems.push("a refused render asked the user".into());
    }

    // ---- A kind that the cluster serves only since the session started:
    // its scope is read from the cluster, not guessed.
    if session.catalog().find(GROUP, KIND).is_some() {
        problems.push("the session already knows the test's kind".into());
    }
    let crds: Api<CustomResourceDefinition> = Api::all(user.clone());
    let crd: CustomResourceDefinition = serde_json::from_value(json!({
        "metadata": {"name": CRD},
        "spec": {
            "group": GROUP, "scope": "Cluster",
            "names": {"plural": "probe179s", "singular": "probe179", "kind": KIND},
            "versions": [{"name": "v1", "served": true, "storage": true,
                          "schema": {"openAPIV3Schema": {"type": "object",
                                     "x-kubernetes-preserve-unknown-fields": true}}}],
        },
    }))
    .unwrap();
    crds.create(&Default::default(), &crd).await.unwrap();
    let mut served = false;
    for _ in 0..60 {
        if ApiCatalog::discover(&user)
            .await
            .is_ok_and(|catalog| catalog.find(GROUP, KIND).is_some())
        {
            served = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if !served {
        problems.push("the cluster did not serve the test's kind in a minute".into());
    }
    let preview = server
        .call(
            "test",
            "helm_install",
            args(install("custom: true\n", true)),
        )
        .await;
    let probe = format!("{KIND} {RELEASE}-probe (cluster-scoped)");
    match serde_json::from_str::<Value>(&preview.text) {
        Ok(answer) if answer["clusterObjects"] == json!([probe]) => {}
        _ => problems.push(format!(
            "the new cluster-scoped kind was not found as such: {}",
            preview.text
        )),
    }

    // ---- A list: its objects are in both windows, and the values the
    // agent sends are in the window as they are.
    let values = "list: true\nauth:\n  password: sent-by-the-agent-179\n";
    let installed = server
        .call("test", "helm_install", args(install(values, false)))
        .await;
    assert!(!installed.is_error, "{}", installed.text);
    let windows = helm_windows(&confirmer);
    let role = format!("Role {RELEASE}-in-a-list");
    match windows.as_slice() {
        [(asked, _), (change, _)] => {
            if asked.credentials != HelmCredentials::AskUser || asked.user_objects != [role.clone()]
            {
                problems.push(format!("the first window: {asked:?}"));
            }
            for object in [
                role.clone(),
                format!("ConfigMap {RELEASE}-in-a-list"),
                format!("ConfigMap {RELEASE}-hook"),
            ] {
                if !change.objects.contains(&object) {
                    problems.push(format!("{object} is not in {:?}", change.objects));
                }
            }
            if change
                .objects
                .iter()
                .any(|object| object.starts_with("List"))
            {
                problems.push(format!(
                    "the list itself is an object: {:?}",
                    change.objects
                ));
            }
            for (name, window) in [("first", asked), ("second", change)] {
                if !window.values.contains("password: sent-by-the-agent-179") {
                    problems.push(format!("the {name} window's values: {}", window.values));
                }
            }
        }
        other => problems.push(format!(
            "windows of the install: {:?}",
            other
                .iter()
                .map(|(change, _)| (change.action, change.credentials))
                .collect::<Vec<_>>()
        )),
    }
    if installed.text.contains("sent-by-the-agent-179") {
        problems.push(format!("the answer has the value: {}", installed.text));
    }

    // ---- What the chart created is the session's own: its pod's logs are
    // confirmed.
    let pods: Api<Pod> = Api::namespaced(user.clone(), &namespace);
    let selector = ListParams::default().labels(&format!("app={RELEASE}-web"));
    let mut pod = None;
    for _ in 0..60 {
        pod = pods
            .list(&selector)
            .await
            .ok()
            .and_then(|list| list.items.into_iter().next())
            .and_then(|pod| pod.metadata.name);
        if pod.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    match pod {
        None => problems.push("the release's pod did not appear in a minute".into()),
        Some(pod) => {
            let before = confirmer.requests().len();
            let _ = server
                .call(
                    "test",
                    "get_logs",
                    args(json!({"pod": pod, "namespace": namespace})),
                )
                .await;
            let asked = confirmer.requests();
            let reason = format!("created or changed Deployment {RELEASE}-web");
            if !asked[before..]
                .iter()
                .any(|request| format!("{} {}", request.summary, request.details).contains(&reason))
            {
                problems.push(format!(
                    "the logs of the release's pod were not confirmed: {:?}",
                    asked[before..]
                        .iter()
                        .map(|request| request.details.clone())
                        .collect::<Vec<_>>()
                ));
            }
        }
    }

    // ---- An upgrade that changes only the hook: the window shows it.
    let upgraded = server
        .call(
            "test",
            "helm_upgrade",
            args(
                json!({"release": RELEASE, "chart": CHART, "namespace": namespace,
                        "valuesPatch": "hookNote: second\n"}),
            ),
        )
        .await;
    assert!(!upgraded.is_error, "{}", upgraded.text);
    hook_window(
        "the upgrade",
        HelmAction::Upgrade,
        &confirmer,
        ["-  note: first", "+  note: second"],
        &mut problems,
    );

    // ---- A rollback to the first revision: the hook goes back.
    let rolled_back = server
        .call(
            "test",
            "helm_rollback",
            args(json!({"release": RELEASE, "revision": 1, "namespace": namespace})),
        )
        .await;
    assert!(!rolled_back.is_error, "{}", rolled_back.text);
    hook_window(
        "the rollback",
        HelmAction::Rollback,
        &confirmer,
        ["-  note: second", "+  note: first"],
        &mut problems,
    );

    let uninstalled = server
        .call(
            "test",
            "helm_uninstall",
            args(json!({"release": RELEASE, "namespace": namespace})),
        )
        .await;
    if uninstalled.is_error {
        problems.push(format!("uninstall: {}", uninstalled.text));
    }
    session.close().await.unwrap();
    problems
}

/// The last Helm window is `action`'s and has the hook's change under the
/// heading of hooks, with no change of the release's objects.
fn hook_window(
    what: &str,
    action: HelmAction,
    confirmer: &ScriptedConfirmer,
    lines: [&str; 2],
    problems: &mut Vec<String>,
) {
    let windows = helm_windows(confirmer);
    let Some((change, request)) = windows.last() else {
        problems.push(format!("{what} asked nobody"));
        return;
    };
    let details = &request.details;
    let heading = details.find("# Hooks:");
    let shown = heading.is_some_and(|at| lines.iter().all(|line| details[at..].contains(line)));
    if change.action != action
        || !shown
        || !details.starts_with("# No object of the release changes")
        || details.contains("changes no object")
    {
        problems.push(format!(
            "the window of {what} ({:?}): {details}",
            change.action
        ));
    }
}
