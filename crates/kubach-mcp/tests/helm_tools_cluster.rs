//! The agent's Helm tools against a real cluster (#418): install, upgrade,
//! rollback and uninstall of a small chart without CRDs, with every
//! confirmation answered by a scripted user. helm runs as the session's
//! temporary ServiceAccount. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-418 \
//! HELM_CONFIG_HOME=$(mktemp -d) HELM_CACHE_HOME=$(mktemp -d) HELM_DATA_HOME=$(mktemp -d) \
//!     cargo test -p kubach-mcp --test helm_tools_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the release and the session's objects are
//! deleted at the end. Needs `helm` on `PATH` and access to ghcr.io.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::rbac::v1::Role;
use kubach_core::helm::repo::HelmPaths;
use kubach_mcp::{
    HelmAction, HelmEnv, McpServer, Profile, RequestKind, ScriptedConfirmer, SecurityPolicy,
    Session, SessionSpec,
};
use kube::api::Api;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

const CHART: &str = "oci://ghcr.io/stefanprodan/charts/podinfo";

async fn config() -> (Config, String, String) {
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
    let config = Config::from_custom_kubeconfig(Kubeconfig::read().unwrap(), &options)
        .await
        .unwrap();
    (config, context, namespace)
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

async fn call(server: &McpServer, tool: &str, arguments: Value) -> Value {
    let outcome = server.call("test", tool, args(arguments)).await;
    assert!(!outcome.is_error, "{tool}: {}", outcome.text);
    serde_json::from_str(&outcome.text).unwrap_or(Value::String(outcome.text))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster, helm and ghcr.io (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn the_agent_installs_upgrades_rolls_back_and_uninstalls() {
    let (base, context, namespace) = config().await;
    let policy = SecurityPolicy {
        mcp_enabled: true,
        temporary_objects: true,
        ..SecurityPolicy::default()
    };
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(900)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    server.set_helm(HelmEnv {
        binary: kubach_core::settings::find_on_path("helm"),
        kubeconfig: Some(kubach_core::helm::cli::kubeconfig_files(&[])),
        paths: HelmPaths::from_env(),
    });
    let user = Client::try_from(base.clone()).unwrap();
    let roles: Api<Role> = Api::namespaced(user, &namespace);
    let role = format!("kubach-mcp-{}", session.id());

    // Reading: the registry is asked for once, and no namespace yet.
    let found = call(&server, "helm_search_charts", json!({"oci": CHART})).await;
    assert!(
        found["versions"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v == "6.15.0"))
    );
    call(
        &server,
        "helm_show_values",
        json!({"chart": CHART, "version": "6.15.0"}),
    )
    .await;

    // Install: the access (changes), the change itself; the namespace's
    // rights come with the access (#15).
    let installed = call(
        &server,
        "helm_install",
        json!({
            "release": "kubach-podinfo",
            "chart": CHART,
            "version": "6.15.0",
            "namespace": namespace,
            "values": "replicaCount: 1\nextra:\n  password: s3cret-value\n",
        }),
    )
    .await;
    assert_eq!(installed["status"], "deployed", "{installed}");
    assert_eq!(installed["revision"], 1);
    let objects = installed["objects"].to_string();
    assert!(objects.contains("Deployment kubach-podinfo"), "{objects}");
    let requests = confirmer.requests();
    let kinds: Vec<&str> = requests
        .iter()
        .map(|request| match &request.kind {
            RequestKind::ChartSource { .. } => "source",
            RequestKind::Access { .. } => "access",
            RequestKind::Grant { .. } => "grant",
            RequestKind::Helm { .. } => "helm",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["source", "access", "helm"]);
    let RequestKind::Helm { helm } = &requests[2].kind else {
        unreachable!()
    };
    assert_eq!(helm.action, HelmAction::Install);
    assert!(!helm.values.contains("s3cret-value"), "{}", helm.values);
    assert!(
        helm.identity.starts_with("serviceaccount:"),
        "{}",
        helm.identity
    );
    assert!(helm.cluster_objects.is_empty());

    // The ServiceAccount's Role is narrow again once helm is done: the
    // rights helm alone had (writing ServiceAccounts, say) are gone.
    let rules = roles.get(&role).await.unwrap().rules.unwrap_or_default();
    assert!(
        !rules.iter().any(|rule| rule
            .resources
            .as_ref()
            .is_some_and(|r| r.iter().any(|r| r == "serviceaccounts"))
            && rule.verbs.iter().any(|v| v == "create")),
        "{rules:?}"
    );

    let listed = call(
        &server,
        "helm_list_releases",
        json!({"namespace": namespace}),
    )
    .await;
    assert_eq!(listed["releases"][0]["release"], "kubach-podinfo");

    // Upgrade with the chart kept in the release and new values.
    let upgraded = call(
        &server,
        "helm_upgrade",
        json!({"release": "kubach-podinfo", "namespace": namespace, "values": "replicaCount: 2\n"}),
    )
    .await;
    assert_eq!(upgraded["revision"], 2, "{upgraded}");
    let RequestKind::Helm { .. } = &confirmer.requests().last().unwrap().kind else {
        panic!("the upgrade was not confirmed");
    };
    assert!(
        confirmer
            .requests()
            .last()
            .unwrap()
            .details
            .contains("replicas: 2"),
        "the diff shows the change"
    );

    let history = call(
        &server,
        "helm_release_history",
        json!({"release": "kubach-podinfo", "namespace": namespace}),
    )
    .await;
    assert_eq!(history["revisions"].as_array().unwrap().len(), 2);

    let rolled = call(
        &server,
        "helm_rollback",
        json!({"release": "kubach-podinfo", "revision": 1, "namespace": namespace}),
    )
    .await;
    assert_eq!(rolled["revision"], 3, "{rolled}");

    call(
        &server,
        "helm_uninstall",
        json!({"release": "kubach-podinfo", "namespace": namespace}),
    )
    .await;
    let listed = call(
        &server,
        "helm_list_releases",
        json!({"namespace": namespace}),
    )
    .await;
    assert_eq!(listed["releases"], json!([]), "{listed}");

    session.close().await.unwrap();
}
