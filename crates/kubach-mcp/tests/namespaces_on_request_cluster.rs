//! Namespaces on request against a real cluster (#401): a session starts
//! without namespaces, and each namespace the user allows reading gets its
//! own temporary ServiceAccount, which the API server confines to it.
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-401 \
//!     KUBACH_TEST_OTHER_NAMESPACE=kubach-test-401-b \
//!     cargo test -p kubach-mcp --test namespaces_on_request_cluster -- --ignored
//! ```
//!
//! Both namespaces must exist; the session's objects are deleted at the end.

use std::time::Duration;

use k8s_openapi::api::core::v1::{Pod, ServiceAccount};
use kubach_mcp::{Grant, Profile, SecurityPolicy, Session, SessionMode, SessionSpec};
use kube::api::{Api, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

async fn config() -> (Config, String, String, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    let other = std::env::var("KUBACH_TEST_OTHER_NAMESPACE").expect("KUBACH_TEST_OTHER_NAMESPACE");
    let options = KubeConfigOptions {
        context: Some(context.clone()),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(Kubeconfig::read().unwrap(), &options)
        .await
        .unwrap();
    (config, context, namespace, other)
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
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE, KUBACH_TEST_OTHER_NAMESPACE)"]
async fn each_namespace_gets_its_own_service_account() {
    let (base, context, namespace, other) = config().await;
    let policy = SecurityPolicy {
        temporary_objects: true,
        ..SecurityPolicy::with_minimum(kubach_mcp::Access::ALL)
    };
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::ReadOnly,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let user = Client::try_from(base.clone()).unwrap();
    let name = format!("kubach-mcp-{}", session.id());
    let accounts = |ns: &str| Api::<ServiceAccount>::namespaced(user.clone(), ns);

    // Nothing anywhere before the user answers.
    assert!(session.boundaries().is_empty());
    assert!(accounts(&namespace).get_opt(&name).await.unwrap().is_none());

    for ns in [&namespace, &other] {
        session.decide(Grant::read(ns.clone()), true).await.unwrap();
        assert_eq!(
            session.mode_in(ns),
            SessionMode::TemporaryServiceAccount,
            "{:?}",
            session.boundaries()
        );
        assert!(accounts(ns).get_opt(&name).await.unwrap().is_some());
    }

    // Each identity reads its own namespace, and the API server refuses it
    // the other one even if Kubach asked.
    let first = session.client_in(&namespace).unwrap();
    let mine: Api<Pod> = Api::namespaced(first.clone(), &namespace);
    assert!(eventually(async || mine.list(&ListParams::default()).await.is_ok()).await);
    let theirs: Api<Pod> = Api::namespaced(first, &other);
    assert!(theirs.list(&ListParams::default()).await.is_err());
    let second: Api<Pod> = Api::namespaced(session.client_in(&other).unwrap(), &other);
    assert!(eventually(async || second.list(&ListParams::default()).await.is_ok()).await);

    session.close().await.unwrap();
    for ns in [&namespace, &other] {
        let accounts = accounts(ns);
        assert!(
            eventually(async || accounts.get_opt(&name).await.unwrap().is_none()).await,
            "the session's ServiceAccount in {ns} was not deleted"
        );
    }
}
