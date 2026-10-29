//! Rights on request against a real cluster (#227, variant A): the Role of
//! a temporary ServiceAccount holds exactly the granted rights. Ignored by
//! default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-227 \
//!     cargo test -p kubach-mcp --test jit_grants_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the session's objects are deleted at the end.

use std::time::Duration;

use k8s_openapi::api::core::v1::{Pod, ServiceAccount};
use k8s_openapi::api::rbac::v1::Role;
use kubach_mcp::{Grant, Profile, SecurityPolicy, Session, SessionMode, SessionSpec};
use kube::api::{Api, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

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

/// RBAC changes reach the authorizer within moments; retry briefly.
async fn eventually(mut check: impl AsyncFnMut() -> bool) -> bool {
    for _ in 0..20 {
        if check().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn role_follows_grants() {
    let (base, context, namespace) = config().await;
    let policy = SecurityPolicy {
        temporary_objects: true,
        ..SecurityPolicy::with_minimum(kubach_mcp::Access::ALL)
    };
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: vec![namespace.clone()],
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let user = Client::try_from(base.clone()).unwrap();
    let roles: Api<Role> = Api::namespaced(user.clone(), &namespace);
    let name = format!("kubach-mcp-{}", session.id());
    let rules = async || roles.get(&name).await.unwrap().rules.unwrap_or_default();

    // Nothing in the cluster before the user allows reading (#401), and
    // no client for the namespace either.
    assert!(roles.get_opt(&name).await.unwrap().is_none());
    assert!(session.client_in(&namespace).is_err());

    session
        .decide(Grant::read(namespace.clone()), true)
        .await
        .unwrap();
    assert_eq!(
        session.mode_in(&namespace),
        SessionMode::TemporaryServiceAccount,
        "{:?}",
        session.boundaries()
    );
    assert!(!rules().await.is_empty());
    let pods: Api<Pod> = Api::namespaced(session.client_in(&namespace).unwrap(), &namespace);
    assert!(eventually(async || pods.list(&ListParams::default()).await.is_ok()).await);

    session
        .decide(Grant::secret(namespace.clone(), "kubach-test-db"), true)
        .await
        .unwrap();
    let named: Vec<_> = rules()
        .await
        .into_iter()
        .filter_map(|rule| rule.resource_names)
        .collect();
    assert_eq!(named, [vec!["kubach-test-db".to_owned()]]);

    // Taking back reading takes everything on top of it, on the server too.
    session
        .revoke(&Grant::read(namespace.clone()))
        .await
        .unwrap();
    assert!(rules().await.is_empty());
    assert!(eventually(async || pods.list(&ListParams::default()).await.is_err()).await);

    session.close().await.unwrap();
    let accounts: Api<ServiceAccount> = Api::namespaced(user, &namespace);
    assert!(
        eventually(async || accounts.get_opt(&name).await.unwrap().is_none()).await,
        "the session's ServiceAccount was not deleted"
    );
}
