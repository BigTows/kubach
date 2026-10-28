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
//!
//! `a_chart_with_crds_runs_as_the_user_only_with_consent` (#18) installs
//! cert-manager (oci://quay.io, CRDs, ClusterRoles, webhooks) under the
//! temporary ServiceAccount: refused without the user's tick, installed
//! and uninstalled with it. Its cluster-scoped objects are deleted at the
//! end whatever happens; run it with `KUBACH_TEST_NAMESPACE=kubach-test-18`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhookConfiguration, ValidatingWebhookConfiguration,
};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::api::core::v1::{Namespace, Secret, Service, ServiceAccount};
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding, Role, RoleBinding};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kubach_core::helm::repo::HelmPaths;
use kubach_core::plugin::BoxFuture;
use kubach_mcp::{
    ConfirmationDecision, ConfirmationRequest, Confirmer, HelmAction, HelmCredentials, HelmEnv,
    McpServer, Profile, RequestKind, ScriptedConfirmer, SecurityPolicy, Session, SessionSpec,
    USER_IDENTITY,
};
use kube::api::{Api, DeleteParams, ListParams};
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

const CERT_MANAGER: &str = "oci://quay.io/jetstack/charts/cert-manager";
const CM_RELEASE: &str = "kubach-cm18";

/// Approves everything except, until `allow_user` is set, running a helm
/// call with the user's credentials (#18).
struct UserTick {
    allow_user: AtomicBool,
    requests: Mutex<Vec<ConfirmationRequest>>,
}

impl UserTick {
    fn requests(&self) -> Vec<ConfirmationRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Confirmer for UserTick {
    fn confirm<'a>(
        &'a self,
        request: &'a ConfirmationRequest,
    ) -> BoxFuture<'a, ConfirmationDecision> {
        self.requests.lock().unwrap().push(request.clone());
        let asks_user = matches!(
            &request.kind,
            RequestKind::Helm { helm } if helm.credentials == HelmCredentials::AskUser
        );
        let decision = if asks_user && !self.allow_user.load(Ordering::SeqCst) {
            ConfirmationDecision::Rejected {
                reason: Some("not ticked".into()),
            }
        } else {
            ConfirmationDecision::Approved
        };
        Box::pin(async move { decision })
    }
}

fn helm_changes(requests: &[ConfirmationRequest]) -> Vec<(HelmAction, HelmCredentials)> {
    requests
        .iter()
        .filter_map(|request| match &request.kind {
            RequestKind::Helm { helm } => Some((helm.action, helm.credentials)),
            _ => None,
        })
        .collect()
}

/// Deletes what the cert-manager release may have left behind, through the
/// user's credentials: its cluster-scoped objects above all.
async fn clean_cert_manager(user: &Client, namespace: &str) {
    let labels = ListParams::default().labels(&format!("app.kubernetes.io/instance={CM_RELEASE}"));
    let dp = DeleteParams::default();
    macro_rules! wipe {
        ($api:expr) => {{
            let _ = $api.delete_collection(&dp, &labels).await;
        }};
    }
    wipe!(Api::<CustomResourceDefinition>::all(user.clone()));
    wipe!(Api::<ClusterRole>::all(user.clone()));
    wipe!(Api::<ClusterRoleBinding>::all(user.clone()));
    wipe!(Api::<MutatingWebhookConfiguration>::all(user.clone()));
    wipe!(Api::<ValidatingWebhookConfiguration>::all(user.clone()));
    wipe!(Api::<Deployment>::namespaced(user.clone(), namespace));
    wipe!(Api::<Service>::namespaced(user.clone(), namespace));
    wipe!(Api::<ServiceAccount>::namespaced(user.clone(), namespace));
    wipe!(Api::<Role>::namespaced(user.clone(), namespace));
    wipe!(Api::<RoleBinding>::namespaced(user.clone(), namespace));
    let secrets: Api<Secret> = Api::namespaced(user.clone(), namespace);
    let storage = ListParams::default().labels(&format!("owner=helm,name={CM_RELEASE}"));
    let _ = secrets.delete_collection(&dp, &storage).await;
    if let Ok(list) = secrets.list(&ListParams::default()).await {
        for secret in list {
            let name = secret.metadata.name.unwrap_or_default();
            if name.starts_with(CM_RELEASE) {
                let _ = secrets.delete(&name, &dp).await;
            }
        }
    }
    let leases: Api<Lease> = Api::namespaced(user.clone(), namespace);
    if let Ok(list) = leases.list(&ListParams::default()).await {
        for lease in list {
            let name = lease.metadata.name.unwrap_or_default();
            if name.starts_with("cert-manager") {
                let _ = leases.delete(&name, &dp).await;
            }
        }
    }
}

/// Waits (at most two minutes) until no CRD of the release is left.
async fn crds_gone(user: &Client) -> bool {
    let crds: Api<CustomResourceDefinition> = Api::all(user.clone());
    let labels = ListParams::default().labels(&format!("app.kubernetes.io/instance={CM_RELEASE}"));
    for _ in 0..60 {
        if crds
            .list(&labels)
            .await
            .is_ok_and(|list| list.items.is_empty())
        {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster, helm and quay.io (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn a_chart_with_crds_runs_as_the_user_only_with_consent() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    let crds: Api<CustomResourceDefinition> = Api::all(user.clone());
    assert!(
        crds.get_opt("certificates.cert-manager.io")
            .await
            .unwrap()
            .is_none(),
        "cert-manager is installed on this cluster already; use another one"
    );
    // The namespace is created when missing, and then deleted at the end.
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let created = if namespaces.get_opt(&namespace).await.unwrap().is_none() {
        let object: Namespace =
            serde_json::from_value(json!({"metadata": {"name": namespace}})).unwrap();
        namespaces
            .create(&Default::default(), &object)
            .await
            .unwrap();
        true
    } else {
        false
    };
    // On its own thread and runtime, so a failing assertion still reaches
    // the cleanup below.
    let flow_namespace = namespace.clone();
    let flow = std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(cert_manager_flow(base, context, flow_namespace))
    });
    let outcome = tokio::task::spawn_blocking(move || flow.join())
        .await
        .unwrap();
    clean_cert_manager(&user, &namespace).await;
    let gone = crds_gone(&user).await;
    if created {
        let _ = namespaces
            .delete(&namespace, &DeleteParams::default())
            .await;
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    assert!(gone, "the CRDs of the release are still there");
}

async fn cert_manager_flow(base: Config, context: String, namespace: String) {
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
    let confirmer = Arc::new(UserTick {
        allow_user: AtomicBool::new(false),
        requests: Mutex::default(),
    });
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    server.set_helm(HelmEnv {
        binary: kubach_core::settings::find_on_path("helm"),
        kubeconfig: Some(kubach_core::helm::cli::kubeconfig_files(&[])),
        paths: HelmPaths::from_env(),
    });
    let values = format!(
        "crds:\n  enabled: true\n  keep: false\nglobal:\n  leaderElection:\n    namespace: \
         {namespace}\nstartupapicheck:\n  enabled: false\n"
    );
    let install = json!({
        "release": CM_RELEASE,
        "chart": CERT_MANAGER,
        "version": "v1.18.2",
        "namespace": namespace,
        "values": values,
    });

    // Without the tick: a refusal naming the objects, and nothing ran.
    let refused = server
        .call("test", "helm_install", args(install.clone()))
        .await;
    assert!(refused.is_error, "{}", refused.text);
    assert!(refused.text.contains("did not allow"), "{}", refused.text);
    assert!(
        refused
            .text
            .contains("CustomResourceDefinition certificates.cert-manager.io"),
        "{}",
        refused.text
    );
    assert!(!refused.text.contains("forbidden"), "{}", refused.text);
    assert_eq!(
        helm_changes(&confirmer.requests()),
        [(HelmAction::Install, HelmCredentials::AskUser)]
    );
    let user = Client::try_from(base.clone()).unwrap();
    let storage: Api<Secret> = Api::namespaced(user.clone(), &namespace);
    let release_storage = ListParams::default().labels(&format!("owner=helm,name={CM_RELEASE}"));
    assert!(
        storage
            .list(&release_storage)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let crds: Api<CustomResourceDefinition> = Api::all(user);
    assert!(
        crds.get_opt("certificates.cert-manager.io")
            .await
            .unwrap()
            .is_none()
    );

    // With the tick: the dry run and the install run as the user, after a
    // second window with the dry run.
    confirmer.allow_user.store(true, Ordering::SeqCst);
    let installed = call(&server, "helm_install", install).await;
    // The release is read back under the ServiceAccount, which may not read
    // its storage: then helm's own output stands in for the status.
    assert!(
        installed["status"] == "deployed"
            || installed["output"]
                .as_str()
                .is_some_and(|out| out.contains("STATUS: deployed")),
        "{installed}"
    );
    assert_eq!(storage.list(&release_storage).await.unwrap().items.len(), 1);
    let changes = helm_changes(&confirmer.requests());
    assert_eq!(
        changes[1..],
        [
            (HelmAction::Install, HelmCredentials::AskUser),
            (HelmAction::Install, HelmCredentials::User),
        ]
    );
    let last = confirmer.requests().last().cloned().unwrap();
    let RequestKind::Helm { helm } = &last.kind else {
        unreachable!()
    };
    assert_eq!(helm.identity, USER_IDENTITY);
    assert!(
        helm.cluster_objects
            .iter()
            .any(|object| object.starts_with("CustomResourceDefinition")),
        "{:?}",
        helm.cluster_objects
    );
    assert!(
        crds.get_opt("certificates.cert-manager.io")
            .await
            .unwrap()
            .is_some()
    );

    // Uninstall deletes the CRDs (crds.keep false): one window, as the user.
    call(
        &server,
        "helm_uninstall",
        json!({"release": CM_RELEASE, "namespace": namespace}),
    )
    .await;
    assert_eq!(
        helm_changes(&confirmer.requests()).last(),
        Some(&(HelmAction::Uninstall, HelmCredentials::AskUser))
    );
    session.close().await.unwrap();
}
