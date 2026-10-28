//! Helm tools (#418): chart sources, values masking, the objects a chart
//! may create, and the policy of Helm requests.

use serde_json::json;

use super::*;
use crate::grants::Grant;
use crate::policy::{Confirmation, Policy};
use crate::settings::{Profile, SecurityPolicy};
use crate::testutil::{NS, OTHER_NS, catalog, grants, permissive, scope_with};

fn repos() -> Vec<String> {
    vec!["stable".into(), "podinfo".into()]
}

#[test]
fn charts_come_from_added_repositories_or_oci() {
    assert_eq!(
        chart_source("podinfo/podinfo", &repos()).unwrap(),
        ChartSource::Repository {
            name: "podinfo".into()
        }
    );
    assert_eq!(
        chart_source("oci://ghcr.io/stefanprodan/charts/podinfo:6.7.0", &repos()).unwrap(),
        ChartSource::Oci {
            source: "oci://ghcr.io/stefanprodan/charts/podinfo".into()
        }
    );
    let unknown = chart_source("bitnami/redis", &repos()).unwrap_err();
    assert!(
        unknown.message.contains("not added in Kubach"),
        "{}",
        unknown.message
    );
    assert!(
        unknown.message.contains("stable, podinfo"),
        "{}",
        unknown.message
    );
    for refused in [
        "./chart",
        "/tmp/chart",
        "../x/y",
        "https://example.com/chart-1.0.0.tgz",
        "file:///etc/passwd",
        "chart.tgz",
        "~/charts/x",
        "stable/../../etc",
        "oci://",
        "",
    ] {
        assert!(chart_source(refused, &repos()).is_err(), "{refused}");
    }
}

#[test]
fn values_are_one_mapping_and_bounded() {
    assert_eq!(
        parse_values("replicaCount: 2\n").unwrap(),
        json!({"replicaCount": 2})
    );
    assert_eq!(parse_values("").unwrap(), json!({}));
    assert!(parse_values("- a\n- b\n").is_err());
    assert!(parse_values("a: 1\n---\nb: 2\n").is_err());
    let huge = format!("a: \"{}\"\n", "x".repeat(MAX_VALUES_BYTES));
    assert!(parse_values(&huge).is_err());
}

#[test]
fn values_are_masked_by_key_name_at_any_depth() {
    let text = "auth:\n  password: hunter22\n  enabled: true\nredis:\n  config:\n    \
                apiToken: abc\n  replicas: 3\nimage:\n  tag: \"7.2\"\n";
    let masked = masked_values_yaml(text);
    assert!(!masked.contains("hunter22"), "{masked}");
    assert!(!masked.contains("abc"), "{masked}");
    assert!(masked.contains("*** (8 bytes)"), "{masked}");
    assert!(masked.contains("replicas: 3"), "{masked}");
    assert!(masked.contains("7.2"), "{masked}");
    assert!(masked_values_yaml("- not\n- a mapping\n").starts_with("<values that do not parse"));
    // The audit log masks the values argument the same way.
    let mut args = json!({"release": "r", "values": text});
    crate::masking::mask_audit_args(&mut args);
    assert!(!args.to_string().contains("hunter22"));
}

fn resource(
    api_version: &str,
    kind: &str,
    name: &str,
    namespace: Option<&str>,
) -> ManifestResource {
    ManifestResource {
        api_version: api_version.into(),
        kind: kind.into(),
        name: name.into(),
        namespace: namespace.map(str::to_owned),
        keep: false,
    }
}

#[test]
fn objects_stay_in_the_namespace() {
    let objects = [
        resource("apps/v1", "Deployment", "web", None),
        resource("v1", "Service", "web", Some(NS)),
    ];
    let preview = check_objects(&objects, NS, &catalog(), SessionMode::ClientOnly).unwrap();
    assert_eq!(preview.objects, ["Deployment web", "Service web"]);
    assert!(preview.findings.is_empty() && preview.cluster.is_empty());
    assert!(
        preview
            .kinds
            .contains(&("apps".into(), "deployments".into()))
    );
    let elsewhere = [resource("v1", "ConfigMap", "c", Some(OTHER_NS))];
    let denial = check_objects(&elsewhere, NS, &catalog(), SessionMode::ClientOnly).unwrap_err();
    assert_eq!(denial.code, DenialCode::Namespace);
}

#[test]
fn cluster_objects_need_a_tick_and_never_run_as_the_service_account() {
    let objects = [
        resource("apps/v1", "Deployment", "web", None),
        resource(
            "apiextensions.k8s.io/v1",
            "CustomResourceDefinition",
            "widgets.example.com",
            None,
        ),
        resource(
            "rbac.authorization.k8s.io/v1",
            "ClusterRole",
            "reader",
            None,
        ),
        resource(
            "admissionregistration.k8s.io/v1",
            "ValidatingWebhookConfiguration",
            "hook",
            None,
        ),
    ];
    for mode in [SessionMode::ClientOnly, SessionMode::Impersonation] {
        let preview = check_objects(&objects, NS, &catalog(), mode).unwrap();
        assert_eq!(preview.cluster.len(), 3, "{mode:?}");
        assert_eq!(preview.findings.len(), 3, "{mode:?}");
        assert!(
            preview
                .findings
                .iter()
                .all(|finding| finding.message.contains("cluster-scoped"))
        );
        assert!(
            !preview
                .kinds
                .iter()
                .any(|(_, plural)| plural == "clusterroles")
        );
    }
    let denial = check_objects(
        &objects,
        NS,
        &catalog(),
        SessionMode::TemporaryServiceAccount,
    )
    .unwrap_err();
    assert_eq!(denial.code, DenialCode::ClusterScoped);
    // RBAC inside the namespace: a tick, or refused for the ServiceAccount.
    let rbac = [resource("rbac.authorization.k8s.io/v1", "Role", "r", None)];
    let preview = check_objects(&rbac, NS, &catalog(), SessionMode::ClientOnly).unwrap();
    assert_eq!(preview.findings.len(), 1);
    assert!(check_objects(&rbac, NS, &catalog(), SessionMode::TemporaryServiceAccount).is_err());
}

#[test]
fn helm_rules_never_grant_rbac_or_cluster_kinds() {
    let rules = helm_rules(&[
        ("rbac.authorization.k8s.io".into(), "roles".into()),
        ("monitoring.coreos.com".into(), "servicemonitors".into()),
    ]);
    for rule in &rules {
        let groups = rule.api_groups.clone().unwrap_or_default();
        assert!(
            !groups
                .iter()
                .any(|g| g == "rbac.authorization.k8s.io" || g == "*")
        );
        assert!(
            !rule
                .verbs
                .iter()
                .any(|v| v == "*" || v == "escalate" || v == "bind")
        );
        assert!(
            !rule
                .resources
                .clone()
                .unwrap_or_default()
                .iter()
                .any(|r| r == "*" || r == "namespaces")
        );
    }
    assert!(rules.iter().any(|rule| {
        rule.resources
            .as_ref()
            .is_some_and(|r| r.iter().any(|r| r == "servicemonitors"))
    }));
}

#[test]
fn helm_changes_need_the_ceiling_the_namespace_and_a_confirmation() {
    let request = Request::HelmChange {
        namespace: None,
        release: "web".into(),
    };
    let strict = SecurityPolicy {
        mcp_enabled: true,
        ..SecurityPolicy::default()
    };
    let scope = scope_with(Profile::ReadOnly, &strict, &[NS]);
    let catalog = catalog();
    let all = grants(&[Grant::read(NS), Grant::modify(NS)]);
    let denial = Policy::new(
        &scope,
        &catalog,
        false,
        all.clone(),
        SessionMode::TemporaryServiceAccount,
    )
    .check(&request)
    .unwrap_err();
    assert_eq!(denial.code, DenialCode::NeedsAccess);

    let scope = scope_with(Profile::Deploy, &permissive(), &[NS]);
    let allowed = Policy::new(
        &scope,
        &catalog,
        false,
        all,
        SessionMode::TemporaryServiceAccount,
    )
    .check(&request)
    .unwrap();
    assert_eq!(allowed.confirmation, Confirmation::Required);
    assert_eq!(allowed.namespace, NS);
    // The namespace's right comes first.
    let denial = Policy::new(
        &scope,
        &catalog,
        false,
        grants(&[Grant::read(NS)]),
        SessionMode::TemporaryServiceAccount,
    )
    .check(&request)
    .unwrap_err();
    assert_eq!(denial.grant, Some(Grant::modify(NS)));
    // Another namespace is outside a pinned session; bad names are refused.
    let other = Request::HelmChange {
        namespace: Some(OTHER_NS.into()),
        release: "web".into(),
    };
    let policy = Policy::new(
        &scope,
        &catalog,
        false,
        grants(&[Grant::read(NS), Grant::modify(NS)]),
        SessionMode::ClientOnly,
    );
    assert_eq!(
        policy.check(&other).unwrap_err().code,
        DenialCode::Namespace
    );
    for name in ["Web", "--kube-apiserver=x", "a_b", &"x".repeat(60)] {
        let bad = Request::HelmChange {
            namespace: None,
            release: name.to_owned(),
        };
        assert_eq!(
            policy.check(&bad).unwrap_err().code,
            DenialCode::InvalidName,
            "{name}"
        );
    }
    // Reading releases needs reading the namespace only.
    let read = Request::HelmRead { namespace: None };
    let policy = Policy::new(
        &scope,
        &catalog,
        false,
        grants(&[]),
        SessionMode::ClientOnly,
    );
    assert_eq!(
        policy.check(&read).unwrap_err().grant,
        Some(Grant::read(NS))
    );
    assert!(policy.check(&Request::HelmCharts).is_ok());
}

#[test]
fn a_cluster_only_kubeconfig_carries_no_credentials() {
    let mut config = kube::Config::new("https://10.0.0.1:6443/".parse().unwrap());
    config.root_cert = Some(vec![vec![1, 2, 3]]);
    config.tls_server_name = Some("kubernetes".into());
    config.auth_info.token = Some(secrecy::SecretString::from("user-token".to_owned()));
    let text = crate::identity::cluster_kubeconfig(&config);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value["clusters"][0]["cluster"]["server"],
        "https://10.0.0.1:6443"
    );
    assert_eq!(value["users"][0]["user"], json!({}));
    assert!(value["clusters"][0]["cluster"]["certificate-authority-data"].is_string());
    assert!(!text.contains("user-token"));
}
