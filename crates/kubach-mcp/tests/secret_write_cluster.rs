//! Secrets written through the user against a real cluster (#230), under a
//! temporary ServiceAccount: the Role allows `create secrets` with
//! `modify(ns)` and `patch`/`delete` only on Secrets the user approved a
//! change of; values reach the API but not the agent's answer or the audit
//! log. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-230 \
//!     KUBACH_TEST_NAMESPACE_B=kubach-test-230-b \
//!     cargo test -p kubach-mcp --test secret_write_cluster -- --ignored
//! ```
//!
//! The namespaces must exist; the test's Secrets and the session's objects
//! are deleted at the end. Values are fake.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Secret, ServiceAccount};
use k8s_openapi::api::rbac::v1::Role;
use kubach_mcp::{
    AuditLog, McpServer, Profile, ScriptedConfirmer, SecretInput, SecurityPolicy, Session,
    SessionMode, SessionSpec,
};
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};

const TYPED: &str = "fake-Typed-Value-230";
const ROTATED: &str = "fake-Rotated-Value-230";

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

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

fn input(set: &[(&str, &str)], remove: &[&str]) -> SecretInput {
    SecretInput {
        set: set
            .iter()
            .map(|(k, v)| (k.to_string(), (*v).into()))
            .collect(),
        remove: remove.iter().map(|k| k.to_string()).collect(),
    }
}

fn b64(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(value)
}

fn assert_no_values(text: &str) {
    for value in [TYPED, ROTATED] {
        assert!(!text.contains(value), "{value} leaked: {text}");
        assert!(!text.contains(&b64(value)), "{value} leaked: {text}");
    }
}

/// Two fresh self-signed certificates with their keys, made by `openssl`
/// in a temporary folder; `None` without it.
fn tls_pairs() -> Option<[(String, String); 2]> {
    let dir = tempfile::tempdir().ok()?;
    let make = |n: usize| -> Option<(String, String)> {
        let cert = dir.path().join(format!("{n}.crt"));
        let key = dir.path().join(format!("{n}.key"));
        let status = std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
            ])
            .args(["-nodes", "-days", "1", "-subj", "/CN=kubach-test"])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        status.success().then_some(())?;
        Some((
            std::fs::read_to_string(cert).ok()?,
            std::fs::read_to_string(key).ok()?,
        ))
    };
    Some([make(1)?, make(2)?])
}

fn value_of(secret: &Secret, key: &str) -> Vec<u8> {
    secret.data.as_ref().unwrap()[key].0.clone()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn secrets_are_written_through_the_user() {
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
    // The namespace's ServiceAccount is set up when reading is allowed.
    session
        .decide(kubach_mcp::Grant::read(namespace.clone()), true)
        .await
        .unwrap();
    assert_eq!(
        session.mode_in(&namespace),
        SessionMode::TemporaryServiceAccount,
        "{:?}",
        session.boundaries()
    );
    let user = Client::try_from(base.clone()).unwrap();
    let secrets: Api<Secret> = Api::namespaced(user.clone(), &namespace);
    let roles: Api<Role> = Api::namespaced(user.clone(), &namespace);
    let role = format!("kubach-mcp-{}", session.id());
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
    let call = async |confirmer: ScriptedConfirmer, tool: &str, arguments: Value| {
        let server = McpServer::new(session.clone(), Arc::new(confirmer), Some(audit.clone()));
        server.call("test-agent", tool, args(arguments)).await
    };

    // A Secret the agent has no business with, made by the user.
    let other: Secret = serde_json::from_value(json!({
        "metadata": {"name": "kubach-test-other"},
        "stringData": {"k": "fake"},
    }))
    .unwrap();
    secrets.create(&Default::default(), &other).await.unwrap();

    // Create: the user enters the value.
    let outcome = call(
        ScriptedConfirmer::providing(input(&[("password", TYPED)], &[])),
        "request_secret_input",
        json!({"name": "kubach-test-db", "mode": "create", "keys": ["password"]}),
    )
    .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_no_values(&serde_json::to_string(&outcome).unwrap());
    let created = secrets.get("kubach-test-db").await.unwrap();
    assert_eq!(value_of(&created, "password"), TYPED.as_bytes());
    let managers: Vec<_> = created
        .metadata
        .managed_fields
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| f.manager)
        .collect();
    assert!(managers.contains(&"kubach-mcp".to_owned()), "{managers:?}");

    // The Role: create without names, nothing named yet.
    let rules = roles.get(&role).await.unwrap().rules.unwrap_or_default();
    let secret_rules: Vec<_> = rules
        .iter()
        .filter(|r| r.resources.as_deref() == Some(&["secrets".to_owned()][..]))
        .collect();
    assert!(
        secret_rules
            .iter()
            .any(|r| r.verbs == ["create"] && r.resource_names.is_none()),
        "{secret_rules:?}"
    );
    assert!(
        !secret_rules
            .iter()
            .any(|r| r.verbs.iter().any(|v| v == "patch" || v == "delete")),
        "{secret_rules:?}"
    );
    // The session identity cannot patch a Secret nobody approved.
    let as_session: Api<Secret> =
        Api::namespaced(session.client_in(&namespace).unwrap(), &namespace);
    let touch = json!({"metadata": {"labels": {"kubach-test": "x"}}});
    let error = as_session
        .patch(
            "kubach-test-other",
            &PatchParams::default(),
            &Patch::Merge(&touch),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&error, kube::Error::Api(s) if s.code == 403),
        "{error}"
    );

    // Update: rotate the value, add a key, nothing else is touched.
    let outcome = call(
        ScriptedConfirmer::providing(input(&[("password", ROTATED), ("user", "fake-app")], &[])),
        "request_secret_input",
        json!({"name": "kubach-test-db", "mode": "update", "keys": ["password"]}),
    )
    .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_no_values(&outcome.text);
    let updated = secrets.get("kubach-test-db").await.unwrap();
    assert_eq!(value_of(&updated, "password"), ROTATED.as_bytes());
    assert_eq!(value_of(&updated, "user"), b"fake-app");
    let named: Vec<_> = roles
        .get(&role)
        .await
        .unwrap()
        .rules
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| r.resource_names.map(|names| (names, r.verbs)))
        .collect();
    assert_eq!(
        named,
        [(
            vec!["kubach-test-db".to_owned()],
            vec!["patch".to_owned(), "delete".to_owned()]
        )]
    );
    // Still nothing on the other Secret.
    assert!(
        as_session
            .patch(
                "kubach-test-other",
                &PatchParams::default(),
                &Patch::Merge(&touch)
            )
            .await
            .is_err()
    );

    // Cancelled: a refusal, nothing changes.
    let outcome = call(
        ScriptedConfirmer::denying(),
        "request_secret_input",
        json!({"name": "kubach-test-db", "mode": "update"}),
    )
    .await;
    assert!(outcome.is_error && outcome.text.contains("declined"));

    // Copy inside Kubach.
    let outcome = call(
        ScriptedConfirmer::approving(),
        "copy_secret",
        json!({"name": "kubach-test-db", "toName": "kubach-test-db-copy"}),
    )
    .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_no_values(&outcome.text);
    let copied = secrets.get("kubach-test-db-copy").await.unwrap();
    assert_eq!(value_of(&copied, "password"), ROTATED.as_bytes());
    assert!(copied.metadata.owner_references.is_none());

    // Delete, confirmed.
    let outcome = call(
        ScriptedConfirmer::approving(),
        "delete_secret",
        json!({"name": "kubach-test-db"}),
    )
    .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(eventually(async || secrets.get_opt("kubach-test-db").await.unwrap().is_none()).await);

    // TLS: the key must belong to the certificate (needs `openssl`).
    if let Some([(cert, key), (_, other_key)]) = tls_pairs() {
        let outcome = call(
            ScriptedConfirmer::providing(input(
                &[("tls.crt", &cert), ("tls.key", &other_key)],
                &[],
            )),
            "request_secret_input",
            json!({"name": "kubach-test-tls", "mode": "create", "type": "kubernetes.io/tls"}),
        )
        .await;
        assert!(outcome.is_error, "{}", outcome.text);
        assert!(outcome.text.contains("does not belong"), "{}", outcome.text);
        assert!(secrets.get_opt("kubach-test-tls").await.unwrap().is_none());
        let outcome = call(
            ScriptedConfirmer::providing(input(&[("tls.crt", &cert), ("tls.key", &key)], &[])),
            "request_secret_input",
            json!({"name": "kubach-test-tls", "mode": "create", "type": "kubernetes.io/tls"}),
        )
        .await;
        assert!(!outcome.is_error, "{}", outcome.text);
        assert!(!outcome.text.contains("PRIVATE KEY"), "{}", outcome.text);
        let tls = secrets.get("kubach-test-tls").await.unwrap();
        assert_eq!(tls.type_.as_deref(), Some("kubernetes.io/tls"));
        let _ = secrets
            .delete("kubach-test-tls", &DeleteParams::default())
            .await;
    }

    // The audit log names keys, never values.
    let audit_text = std::fs::read_to_string(&audit_path).unwrap();
    assert_no_values(&audit_text);
    assert!(
        audit_text.contains(r#""keys":["password","user"]"#),
        "{audit_text}"
    );

    for name in ["kubach-test-db-copy", "kubach-test-other"] {
        let _ = secrets.delete(name, &DeleteParams::default()).await;
    }
    session.close().await.unwrap();
    let accounts: Api<ServiceAccount> = Api::namespaced(user, &namespace);
    assert!(
        eventually(async || accounts.get_opt(&role).await.unwrap().is_none()).await,
        "the session's ServiceAccount was not deleted"
    );
}

/// Copy between two namespaces of one session (client only): the copy has
/// the data and type, but no owner references, labels or last-applied
/// annotation of the source. Needs `KUBACH_TEST_NAMESPACE_B` as well.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE, KUBACH_TEST_NAMESPACE_B)"]
async fn copies_cross_namespaces_inside_kubach() {
    let (base, context, namespace) = config().await;
    let other = std::env::var("KUBACH_TEST_NAMESPACE_B").expect("KUBACH_TEST_NAMESPACE_B");
    let policy = SecurityPolicy {
        temporary_objects: false,
        ..SecurityPolicy::with_minimum(kubach_mcp::Access::ALL)
    };
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: vec![namespace.clone(), other.clone()],
        profile: Profile::Deploy,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let user = Client::try_from(base.clone()).unwrap();
    let source: Api<Secret> = Api::namespaced(user.clone(), &namespace);
    let target: Api<Secret> = Api::namespaced(user.clone(), &other);
    let pull: Secret = serde_json::from_value(json!({
        "metadata": {
            "name": "kubach-test-pull",
            "labels": {"app.kubernetes.io/managed-by": "someone"},
            "annotations": {"kubectl.kubernetes.io/last-applied-configuration": "{\"fake\":1}"},
        },
        "stringData": {"token": TYPED},
    }))
    .unwrap();
    source.create(&Default::default(), &pull).await.unwrap();

    let server = McpServer::new(
        session.clone(),
        Arc::new(ScriptedConfirmer::approving()),
        None,
    );
    let outcome = server
        .call(
            "test-agent",
            "copy_secret",
            args(json!({"namespace": namespace, "name": "kubach-test-pull", "toNamespace": other})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert_no_values(&outcome.text);
    let copy = target.get("kubach-test-pull").await.unwrap();
    assert_eq!(value_of(&copy, "token"), TYPED.as_bytes());
    assert!(copy.metadata.owner_references.is_none());
    assert!(copy.metadata.labels.is_none());
    let annotations = copy.metadata.annotations.clone().unwrap_or_default();
    assert_eq!(
        annotations.get("kubach.io/copied-from"),
        Some(&format!("{namespace}/kubach-test-pull"))
    );
    assert_eq!(annotations.len(), 1, "{annotations:?}");

    // Outside the session: refused before any request.
    let outcome = server
        .call(
            "test-agent",
            "copy_secret",
            args(json!({"namespace": namespace, "name": "kubach-test-pull", "toNamespace": "default"})),
        )
        .await;
    assert!(outcome.is_error && outcome.text.starts_with("denied by policy"));

    let _ = source
        .delete("kubach-test-pull", &DeleteParams::default())
        .await;
    let _ = target
        .delete("kubach-test-pull", &DeleteParams::default())
        .await;
    session.close().await.unwrap();
}
