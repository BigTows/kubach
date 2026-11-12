//! What the agent gets from a chart before the user approved anything
//! (#173), and one fetch of the chart for every step of a call (#174),
//! against a real cluster and a real `helm`. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-173 \
//! HELM_CONFIG_HOME=$(mktemp -d) HELM_CACHE_HOME=$(mktemp -d) HELM_DATA_HOME=$(mktemp -d) \
//!     cargo test -p kubach-mcp --test helm_dry_run_cluster -- --ignored
//! ```
//!
//! The test creates and deletes the namespace itself and needs no registry:
//! the chart comes from a repository served on 127.0.0.1 for the test, which
//! can give another archive after a number of downloads.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Secret};
use kubach_core::helm::repo::HelmPaths;
use kubach_mcp::{
    HelmAction, HelmCredentials, HelmEnv, McpServer, Profile, RequestKind, ScriptedConfirmer,
    SecurityPolicy, Session, SessionSpec,
};
use kube::api::{Api, DeleteParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

const REPOSITORY: &str = "kubach-test-173";
const CHART: &str = "kubach-test-173/computed";
/// A Secret of the namespace, and its values as the API server gives them.
const SECRET: &str = "db-creds";
const PLAIN: [&str; 2] = ["S3cretValue-173", "kubach-license-173"];
const ENCODED: [&str; 2] = ["UzNjcmV0VmFsdWUtMTcz", "a3ViYWNoLWxpY2Vuc2UtMTcz"];

/// Values that make the chart fail with the namespace's Secrets as its
/// message, and ones that put them into a field of a ConfigMap.
const FAILS: &str =
    "note: '{{ fail (toJson (lookup \"v1\" \"Secret\" .Release.Namespace \"\")) }}'\n";
const COPIES: &str = "note: '{{ range (lookup \"v1\" \"Secret\" .Release.Namespace \"\").items }}\
                      {{ .metadata.name }}={{ .data | toJson }};{{ end }}'\n";

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

/// A chart that computes a field from a value (`tpl`), as many charts do;
/// `extra` adds an object the first archive does not have.
fn write_chart(dir: &std::path::Path, extra: bool) {
    std::fs::create_dir_all(dir.join("templates")).unwrap();
    std::fs::write(
        dir.join("Chart.yaml"),
        "apiVersion: v2\nname: computed\nversion: 0.1.0\n",
    )
    .unwrap();
    std::fs::write(dir.join("values.yaml"), "note: hello\n").unwrap();
    let mut template = String::from(
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {{ .Release.Name }}-note\n\
         data:\n  note: {{ tpl .Values.note . | quote }}\n",
    );
    if extra {
        template.push_str(
            "---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {{ .Release.Name }}-later\n",
        );
    }
    std::fs::write(dir.join("templates/note.yaml"), template).unwrap();
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

/// A chart repository on 127.0.0.1: its index has no digest, and the
/// archive is `first` for the first `first_downloads` downloads and `later`
/// after them.
struct Repository {
    url: String,
    downloads: Arc<AtomicUsize>,
    first_downloads: Arc<AtomicUsize>,
}

impl Repository {
    fn serve(first: Vec<u8>, later: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let index = format!(
            "apiVersion: v1\nentries:\n  computed:\n  - apiVersion: v2\n    name: computed\n    \
             version: 0.1.0\n    urls:\n    - {url}/computed-0.1.0.tgz\n\
             generated: \"2026-01-01T00:00:00Z\"\n"
        );
        let downloads = Arc::new(AtomicUsize::new(0));
        let counted = downloads.clone();
        let first_downloads = Arc::new(AtomicUsize::new(usize::MAX));
        let limit = first_downloads.clone();
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
                    if counted.fetch_add(1, Ordering::SeqCst) < limit.load(Ordering::SeqCst) {
                        first.clone()
                    } else {
                        later.clone()
                    }
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
        Self {
            url,
            downloads,
            first_downloads,
        }
    }
}

/// Packages both charts and adds the repository to the test's helm home.
fn repository() -> Repository {
    let dir = tempfile::tempdir().unwrap();
    let mut archives = Vec::new();
    for (name, extra) in [("first", false), ("later", true)] {
        let chart = dir.path().join(name).join("computed");
        write_chart(&chart, extra);
        let out = dir.path().join(name);
        helm(&[
            "package",
            chart.to_str().unwrap(),
            "--destination",
            out.to_str().unwrap(),
        ]);
        archives.push(std::fs::read(out.join("computed-0.1.0.tgz")).unwrap());
    }
    let later = archives.pop().unwrap();
    let repository = Repository::serve(archives.pop().unwrap(), later);
    helm(&["repo", "add", "--force-update", REPOSITORY, &repository.url]);
    repository
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

fn helm_windows(confirmer: &ScriptedConfirmer) -> Vec<(HelmAction, HelmCredentials)> {
    confirmer
        .requests()
        .iter()
        .filter_map(|request| match &request.kind {
            RequestKind::Helm { helm } => Some((helm.action, helm.credentials)),
            _ => None,
        })
        .collect()
}

/// Whatever the agent got must hold nothing of the Secret.
fn leaks(what: &str, text: &str, problems: &mut Vec<String>) {
    for secret in PLAIN.iter().chain(&ENCODED) {
        if text.contains(secret) {
            problems.push(format!("{what} holds {secret}"));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster and helm (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn a_chart_shows_the_agent_no_secret_and_is_fetched_once() {
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
    assert!(namespace_gone(&namespaces, &namespace).await);
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
    let dropped = namespace_gone(&namespaces, &namespace).await;
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(problems) => assert!(problems.is_empty(), "{problems:#?}"),
    }
    assert!(dropped, "the test's namespace is still there");
}

async fn flow(base: Config, context: String, namespace: String) -> Vec<String> {
    let mut problems = Vec::new();
    let user = Client::try_from(base.clone()).unwrap();
    let secrets: Api<Secret> = Api::namespaced(user.clone(), &namespace);
    let secret: Secret = serde_json::from_value(json!({
        "metadata": {"name": SECRET},
        "stringData": {"password": PLAIN[0], "license": PLAIN[1]},
    }))
    .unwrap();
    secrets.create(&Default::default(), &secret).await.unwrap();
    let repository = repository();
    let helm_env = || HelmEnv {
        binary: kubach_core::settings::find_on_path("helm"),
        kubeconfig: Some(kubach_core::helm::cli::kubeconfig_files(&[])),
        vault: None,
        paths: HelmPaths::from_env(),
    };
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(900)),
        hints: Vec::new(),
    };

    // ---- A temporary ServiceAccount: the default.
    let policy = SecurityPolicy {
        mcp_enabled: true,
        temporary_objects: true,
        ..SecurityPolicy::default()
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    server.set_helm(helm_env());
    let install = |release: &str, values: &str, preview: bool| {
        json!({"release": release, "chart": CHART, "namespace": namespace,
               "values": values, "preview": preview})
    };

    // The chart fails with the Secrets as its message: as a preview and as
    // a call, with nobody asked.
    for preview in [true, false] {
        let failed = server
            .call(
                "test",
                "helm_install",
                args(install("fails", FAILS, preview)),
            )
            .await;
        if !failed.is_error {
            problems.push(format!("preview={preview}: the failing chart did not fail"));
        }
        leaks(
            &format!("the error of preview={preview}"),
            &failed.text,
            &mut problems,
        );
    }
    if !helm_windows(&confirmer).is_empty() {
        problems.push("a failing dry run asked the user".into());
    }

    // The chart copies the Secrets into a ConfigMap: the preview and its
    // diff do not have them.
    let preview = server
        .call(
            "test",
            "helm_install",
            args(install("copies", COPIES, true)),
        )
        .await;
    leaks("the preview", &preview.text, &mut problems);
    if preview.is_error {
        problems.push(format!("the preview failed: {}", preview.text));
    } else {
        let answer: Value = serde_json::from_str(&preview.text).unwrap();
        let diff = server
            .call(
                "test",
                "helm_preview_diff",
                args(json!({"previewId": answer["fullDiff"]["previewId"]})),
            )
            .await;
        leaks("the preview's diff", &diff.text, &mut problems);
    }
    if !helm_windows(&confirmer).is_empty() {
        problems.push("a preview asked the user".into());
    }

    // ---- One fetch: the repository gives another chart from the second
    // download of this call on, and the release has only what the first
    // one renders.
    let before = repository.downloads.load(Ordering::SeqCst);
    repository
        .first_downloads
        .store(before + 1, Ordering::SeqCst);
    let installed = server
        .call(
            "test",
            "helm_install",
            args(install("once", "note: plain\n", false)),
        )
        .await;
    assert!(!installed.is_error, "{}", installed.text);
    let fetched = repository.downloads.load(Ordering::SeqCst) - before;
    if fetched != 1 {
        problems.push(format!(
            "the chart was downloaded {fetched} times for one call"
        ));
    }
    let maps: Api<ConfigMap> = Api::namespaced(user.clone(), &namespace);
    if maps.get_opt("once-later").await.unwrap().is_some() {
        problems.push("the release has an object of the later archive".into());
    }
    match serde_json::from_str::<Value>(&installed.text) {
        Ok(answer) => {
            if answer["comparedWithDryRun"]["sameObjects"] != true {
                problems.push(format!("not compared with the dry run: {answer}"));
            }
            if answer["chartArchive"]["sha256"].as_str().map(str::len) != Some(64) {
                problems.push(format!("no digest of the archive: {answer}"));
            }
        }
        Err(_) => problems.push(format!("the answer is not JSON: {}", installed.text)),
    }
    if helm_windows(&confirmer) != [(HelmAction::Install, HelmCredentials::Session)] {
        problems.push(format!(
            "windows under the ServiceAccount: {:?}",
            helm_windows(&confirmer)
        ));
    }
    // An upgrade from the repository fetches once as well.
    let before = repository.downloads.load(Ordering::SeqCst);
    let upgraded = server
        .call(
            "test",
            "helm_upgrade",
            args(
                json!({"release": "once", "chart": CHART, "namespace": namespace,
                        "valuesPatch": "note: again\n"}),
            ),
        )
        .await;
    assert!(!upgraded.is_error, "{}", upgraded.text);
    let fetched = repository.downloads.load(Ordering::SeqCst) - before;
    if fetched != 1 {
        problems.push(format!(
            "the chart was downloaded {fetched} times for an upgrade"
        ));
    }
    session.close().await.unwrap();

    // ---- A client-only namespace: helm has the user's own credentials.
    let policy = SecurityPolicy {
        mcp_enabled: true,
        temporary_objects: false,
        ..SecurityPolicy::default()
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    server.set_helm(helm_env());
    let preview = server
        .call(
            "test",
            "helm_install",
            args(install("copies", COPIES, true)),
        )
        .await;
    leaks("the client-only preview", &preview.text, &mut problems);
    let failed = server
        .call("test", "helm_install", args(install("fails", FAILS, true)))
        .await;
    leaks(
        "the client-only preview's error",
        &failed.text,
        &mut problems,
    );
    if !helm_windows(&confirmer).is_empty() {
        problems.push("a client-only preview asked the user".into());
    }
    // The call itself: the dry run comes after the first window, and what
    // it fails on stays with helm.
    let failed = server
        .call("test", "helm_install", args(install("fails", FAILS, false)))
        .await;
    if !failed.is_error {
        problems.push("client-only: the failing chart did not fail".into());
    }
    leaks("the client-only error", &failed.text, &mut problems);
    if helm_windows(&confirmer) != [(HelmAction::Install, HelmCredentials::AskUser)] {
        problems.push(format!(
            "client-only windows before a dry run: {:?}",
            helm_windows(&confirmer)
        ));
    }
    session.close().await.unwrap();
    problems
}
