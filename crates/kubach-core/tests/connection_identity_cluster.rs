//! Connection identity against a real cluster (#344): a change captured in
//! one connection must tell a switch to another context, even one pointing
//! at the same cluster, and a reconnect from the connection it was asked in.
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube cargo test -p kubach-core \
//!     --test connection_identity_cluster -- --ignored
//! ```

use kubach_core::Contexts;
use kube::config::{Kubeconfig, NamedContext};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT)"]
async fn connections_tell_contexts_and_reconnects_apart() {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let mut kubeconfig = Kubeconfig::read().unwrap();
    // A second name for the same cluster and user, like a staging/prod
    // pair that only differs by name.
    let alias = format!("{context}-alias-344");
    let original = kubeconfig
        .contexts
        .iter()
        .find(|named| named.name == context)
        .cloned()
        .expect("the test context is in the kubeconfig");
    kubeconfig.contexts.push(NamedContext {
        name: alias.clone(),
        ..original
    });
    let contexts = Contexts::from_kubeconfig(kubeconfig);

    let first = contexts.connect(&context).await.unwrap();
    let again = contexts.connect(&context).await.unwrap();
    let other = contexts.connect(&alias).await.unwrap();

    assert!(first.same_as(&first.clone()), "clones are one connection");
    assert!(!first.same_as(&again), "a reconnect is a new connection");
    assert!(
        !first.same_as(&other),
        "another context is another connection"
    );
    assert_eq!(other.context, alias);
    // Both reach the same API server: only the identity tells them apart.
    assert_eq!(first.server_version, other.server_version);
}
