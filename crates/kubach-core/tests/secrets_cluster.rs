//! Secret operations against a real cluster. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-56 \
//!     cargo test -p kubach-core --test secrets_cluster -- --ignored
//! ```
//!
//! The namespace must exist; the test creates and deletes its own Secrets.

use k8s_openapi::api::core::v1::Secret;
use kubach_core::secrets::{
    self, Access, NewSecret, SecretBytes, SecretError, SecretPatch, Summary, TypedKind,
};
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

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

async fn cleanup(client: &Client, namespace: &str, names: &[&str]) {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    for name in names {
        api.delete(name, &DeleteParams::default()).await.ok();
    }
}

const CERT: &str = "-----BEGIN CERTIFICATE-----
MIICDDCCAbKgAwIBAgIUSTr3bXduGiNjqQJyhm1vb4ZW/tYwCgYIKoZIzj0EAwIw
OjELMAkGA1UEBhMCREUxFDASBgNVBAoMC0t1YmFjaCBUZXN0MRUwEwYDVQQDDAxl
eGFtcGxlLnRlc3QwHhcNMjYxMDIyMTcxOTE1WhcNMzYxMDE5MTcxOTE1WjA6MQsw
CQYDVQQGEwJERTEUMBIGA1UECgwLS3ViYWNoIFRlc3QxFTATBgNVBAMMDGV4YW1w
bGUudGVzdDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABFwNRo99vnYmA7HukZxH
2EsAzLumZE3MkUR9T1TfpARDJylOvw832lb5NamHjjC0UmivZvT4MV2Qyh/SASrb
kHWjgZUwgZIwHQYDVR0OBBYEFC7vkyNHEediAT6nHnagy6An7pD3MB8GA1UdIwQY
MBaAFC7vkyNHEediAT6nHnagy6An7pD3MA8GA1UdEwEB/wQFMAMBAf8wPwYDVR0R
BDgwNoIMZXhhbXBsZS50ZXN0gg4qLmV4YW1wbGUudGVzdIcECgAAAYcQAAAAAAAA
AAAAAAAAAAAAATAKBggqhkjOPQQDAgNIADBFAiAg2KdVCj06snV1t0/7ZGKbfplo
DxORXmsBBu+7ZZQiCgIhALEk5isBpBGwZOdD4+yToi3Xz+QPokJdE+NgLdqQjCfY
-----END CERTIFICATE-----
";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn secrets_round_trip() {
    let (client, ns) = client().await;
    let names = [
        "kubach-opaque",
        "kubach-pull",
        "kubach-tls",
        "kubach-frozen",
    ];
    cleanup(&client, &ns, &names).await;

    assert_eq!(
        secrets::can_i(&client, "get", &ns, Some("kubach-opaque")).await,
        Access::Allowed
    );

    // Create an Opaque Secret and read one value back.
    let opaque = NewSecret::Opaque {
        entries: vec![
            ("user".into(), SecretBytes::from("admin")),
            ("pass".into(), SecretBytes::from("old")),
            ("bin".into(), SecretBytes::new(vec![0, 159, 146, 150])),
        ],
    };
    secrets::create(&client, &ns, "kubach-opaque", &opaque)
        .await
        .unwrap();
    assert_eq!(
        secrets::create(&client, &ns, "kubach-opaque", &opaque).await,
        Err(SecretError::AlreadyExists)
    );
    let revealed = secrets::reveal(&client, &ns, "kubach-opaque", &["pass"])
        .await
        .unwrap();
    assert_eq!(revealed.values.len(), 1);
    assert_eq!(revealed.values["pass"].text(), Some("old"));
    let info = secrets::inspect(&client, &ns, "kubach-opaque")
        .await
        .unwrap();
    assert_eq!(
        info.keys,
        [("bin".into(), 4), ("pass".into(), 3), ("user".into(), 5)]
    );

    // Patch: change one key, add one, remove one; `bin` is not sent.
    let mut patch = SecretPatch::new(
        info.keys.iter().map(|(k, _)| k.clone()),
        info.resource_version.clone(),
    );
    patch.set("pass", SecretBytes::from("new"));
    patch.set("token", SecretBytes::from("t0k"));
    patch.remove("user");
    secrets::apply_patch(&client, &ns, "kubach-opaque", &patch)
        .await
        .unwrap();
    let all = secrets::reveal(
        &client,
        &ns,
        "kubach-opaque",
        &["bin", "pass", "token", "user"],
    )
    .await
    .unwrap();
    assert_eq!(all.values["pass"].text(), Some("new"));
    assert_eq!(all.values["token"].text(), Some("t0k"));
    assert_eq!(all.values["bin"].expose(), [0, 159, 146, 150]);
    assert!(!all.values.contains_key("user"));

    // The same patch again carries a stale resourceVersion.
    assert_eq!(
        secrets::apply_patch(&client, &ns, "kubach-opaque", &patch).await,
        Err(SecretError::Conflict)
    );

    // Docker registry: the summary has the user, not the password.
    let pull = NewSecret::DockerRegistry {
        server: "ghcr.io".into(),
        username: "bot".into(),
        password: SecretBytes::from("pw"),
        email: None,
    };
    secrets::create(&client, &ns, "kubach-pull", &pull)
        .await
        .unwrap();
    let kind = TypedKind::DockerConfigJson;
    let revealed = secrets::reveal(&client, &ns, "kubach-pull", kind.source_keys())
        .await
        .unwrap();
    assert_eq!(revealed.info.typed_kind(), Some(kind));
    let Summary::Registries(logins) = secrets::typed::summarize(kind, &revealed.values).unwrap()
    else {
        panic!()
    };
    assert_eq!(logins[0].registry, "ghcr.io");
    assert_eq!(logins[0].username.as_deref(), Some("bot"));

    // TLS from PEM; the summary never fetches tls.key.
    let tls = NewSecret::Tls {
        cert: SecretBytes::from(CERT),
        key: SecretBytes::from("-----BEGIN PRIVATE KEY-----\nAAEC\n-----END PRIVATE KEY-----\n"),
    };
    secrets::create(&client, &ns, "kubach-tls", &tls)
        .await
        .unwrap();
    let revealed = secrets::reveal(&client, &ns, "kubach-tls", TypedKind::Tls.source_keys())
        .await
        .unwrap();
    assert!(!revealed.values.contains_key("tls.key"));
    let Summary::Certificates(chain) =
        secrets::typed::summarize(TypedKind::Tls, &revealed.values).unwrap()
    else {
        panic!()
    };
    assert_eq!(chain[0].subject, "C=DE, O=Kubach Test, CN=example.test");

    // Immutable Secrets refuse data changes.
    secrets::create(&client, &ns, "kubach-frozen", &opaque)
        .await
        .unwrap();
    let api: Api<Secret> = Api::namespaced(client.clone(), &ns);
    api.patch(
        "kubach-frozen",
        &PatchParams::default(),
        &Patch::Merge(serde_json::json!({"immutable": true})),
    )
    .await
    .unwrap();
    let info = secrets::inspect(&client, &ns, "kubach-frozen")
        .await
        .unwrap();
    assert!(info.immutable);
    let mut patch = SecretPatch::new(["pass".to_owned()], None);
    patch.set("pass", SecretBytes::from("x"));
    let error = secrets::apply_patch(&client, &ns, "kubach-frozen", &patch)
        .await
        .unwrap_err();
    assert!(matches!(error, SecretError::Invalid(_)), "{error:?}");

    cleanup(&client, &ns, &names).await;
}
