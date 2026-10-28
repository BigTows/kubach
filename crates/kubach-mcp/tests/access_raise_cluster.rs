//! Raising the agent's access from a session against a real cluster (#417,
//! #15): the default minimum (reading), the user allows changes for the
//! session, and the temporary ServiceAccount — created automatically where
//! the user's RBAC allows it — grows with it. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-15 \
//!     cargo test -p kubach-mcp --test access_raise_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the session's objects are deleted at the end.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::{ConfigMap, ServiceAccount};
use k8s_openapi::api::rbac::v1::Role;
use kubach_mcp::{
    Access, McpServer, Profile, RequestKind, ScriptedConfirmer, SecurityPolicy, Session,
    SessionSpec,
};
use kube::api::{Api, DeleteParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

async fn config() -> (Config, String, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    let options = KubeConfigOptions {
        context: Some(context.clone()),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(Kubeconfig::read().unwrap(), &options)
        .await
        .unwrap();
    (config, context, namespace)
}

async fn eventually(mut check: impl AsyncFnMut() -> bool) -> bool {
    for _ in 0..20 {
        if check().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn raised_access_widens_the_role_of_the_namespace() {
    let (base, context, namespace) = config().await;
    // The defaults: reading without asking, the identity chosen
    // automatically.
    let policy = SecurityPolicy::default();
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    let user = Client::try_from(base.clone()).unwrap();
    let roles: Api<Role> = Api::namespaced(user.clone(), &namespace);
    let role = format!("kubach-mcp-{}", session.id());
    let write_verbs = async || {
        roles
            .get(&role)
            .await
            .map(|role| {
                role.rules
                    .unwrap_or_default()
                    .iter()
                    .any(|rule| rule.verbs.iter().any(|verb| verb == "create"))
            })
            .unwrap_or(false)
    };

    // Reading first, without a question: the Role holds read verbs only.
    let outcome = server
        .call(
            "test",
            "list_resources",
            args(json!({"kind": "ConfigMap", "namespace": namespace})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(confirmer.requests().is_empty());
    assert!(roles.get_opt(&role).await.unwrap().is_some(), "SA created");
    assert!(!write_verbs().await);

    // A real apply beyond the minimum: the access is asked first, then the
    // apply itself with its diff.
    let yaml =
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: kubach-test-ceiling\ndata:\n  a: b\n";
    let outcome = server
        .call(
            "test",
            "apply_manifest",
            args(json!({"yaml": yaml, "namespace": namespace, "dryRun": false})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let kinds: Vec<&str> = confirmer
        .requests()
        .iter()
        .map(|request| match &request.kind {
            RequestKind::Access { .. } => "access",
            RequestKind::Grant { .. } => "grant",
            RequestKind::Call => "call",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["access", "call"]);
    assert_eq!(session.access(), Access::profile(Profile::Deploy));

    // The API server allows the change too: the SA's Role grew.
    assert!(eventually(async || write_verbs().await).await);
    let maps: Api<ConfigMap> = Api::namespaced(user.clone(), &namespace);
    let created = maps.get("kubach-test-ceiling").await.unwrap();
    let managers: Vec<String> = created
        .metadata
        .managed_fields
        .unwrap_or_default()
        .into_iter()
        .filter_map(|field| field.manager)
        .collect();
    assert!(managers.iter().any(|m| m == "kubach-mcp"), "{managers:?}");

    session.close().await.unwrap();
    let _ = maps
        .delete("kubach-test-ceiling", &DeleteParams::default())
        .await;
    let accounts: Api<ServiceAccount> = Api::namespaced(user, &namespace);
    assert!(
        eventually(async || accounts.get_opt(&role).await.unwrap().is_none()).await,
        "the session's ServiceAccount was not deleted"
    );
}
