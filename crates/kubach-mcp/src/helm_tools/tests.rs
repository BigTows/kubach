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

#[test]
fn a_patch_merges_mappings_and_replaces_lists_and_scalars() {
    let mut values = json!({
        "crds": {"enabled": false, "keep": true},
        "cainjector": {"replicaCount": 1, "image": {"tag": "v1"}},
        "args": ["--a", "--b"],
        "extra": {"drop": 1, "stay": 2},
        "name": "web",
    });
    let patch = json!({
        "crds": {"enabled": true},
        "cainjector": {"replicaCount": 2},
        "args": ["--c"],
        "extra": {"drop": null},
        "name": {"first": "w"},
        "removed": null,
        "added": {"deep": {"value": 1, "gone": null}},
    });
    merge_values(&mut values, &patch);
    assert_eq!(
        values,
        json!({
            "crds": {"enabled": true, "keep": true},
            "cainjector": {"replicaCount": 2, "image": {"tag": "v1"}},
            "args": ["--c"],
            "extra": {"stay": 2},
            "name": {"first": "w"},
            "added": {"deep": {"value": 1}},
        })
    );
    // An empty patch changes nothing.
    let before = values.clone();
    merge_values(&mut values, &json!({}));
    assert_eq!(values, before);
    // A scalar where a mapping was replaces it whole.
    merge_values(&mut values, &json!({"crds": false}));
    assert_eq!(values["crds"], json!(false));
}

#[test]
fn values_and_a_patch_together_are_an_input_error() {
    let both = values_edit(Some("a: 1\n"), Some("b: 2\n")).unwrap_err();
    assert_eq!(both.code, DenialCode::InvalidInput);
    assert!(both.message.contains("not both"), "{}", both.message);
    assert!(matches!(values_edit(None, None).unwrap(), ValuesEdit::Keep));
    assert!(matches!(
        values_edit(Some("a: 1\n"), None).unwrap(),
        ValuesEdit::Replace(_)
    ));
    match values_edit(None, Some("a:\n  b: null\n")).unwrap() {
        ValuesEdit::Patch(patch) => assert_eq!(patch, json!({"a": {"b": null}})),
        other => panic!("not a patch: {other:?}"),
    }
    assert!(matches!(
        values_edit(None, Some("")).unwrap(),
        ValuesEdit::Patch(patch) if patch == json!({})
    ));
    let bad = values_edit(None, Some("- a\n")).unwrap_err();
    assert!(bad.message.starts_with("valuesPatch"), "{}", bad.message);
}

#[test]
fn strings_that_look_like_keys_or_tokens_are_secrets() {
    for secret in [
        "-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----\n",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJrdWJhY2gifQ.c2lnbmF0dXJl",
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        "dGhpcyBpcyBhIGxvbmcgc2VjcmV0IHZhbHVlIDEyMw==",
        "AKIAIOSFODNN7EXAMPLEwJalrXUtnFEMI_K7MDENG-bPxRfiCY",
    ] {
        assert!(looks_like_secret(secret), "missed a secret-like value");
    }
    for plain in [
        "",
        "nginx",
        "registry.example.com/team/image:1.2.3",
        "my-very-long-release-name-for-the-cluster",
        "https://charts.example.com/stable/index.yaml",
        "ShortB64Abc123==",
        "cert-manager-webhook-ca-secret-name-in-namespace",
        "1234567890123456789012345678901234567890",
        "--enable-certificate-owner-ref=true",
    ] {
        assert!(!looks_like_secret(plain), "{plain}");
    }
}

#[test]
fn get_values_masks_by_name_lists_and_secret_looking_strings() {
    let mut values = json!({
        "replicaCount": 2,
        "auth": {"password": "hunter2", "enabled": true, "user": "app"},
        "apiKeys": ["first", "second"],
        "webhook": {"caBundle": "LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCk1JSUM="},
        "image": {"repository": "registry.example.com/app", "tag": "1.2.3"},
        "url": "postgres://app:hunter3@db:5432/shop",
        "secretPort": 5432,
        "redisPassword": 123456,
    });
    let mut masked = mask_values_listed(&mut values);
    masked.sort();
    assert_eq!(
        masked,
        [
            "apiKeys[0]",
            "apiKeys[1]",
            "auth.password",
            "redisPassword",
            "webhook.caBundle"
        ]
    );
    let text = values.to_string();
    for secret in ["hunter2", "first", "LS0tLS1CRUdJTi", "hunter3"] {
        assert!(!text.contains(secret), "a value is not masked");
    }
    assert_eq!(values["auth"]["password"], "*** (7 bytes)");
    assert_eq!(values["auth"]["enabled"], true);
    assert_eq!(values["image"]["tag"], "1.2.3");
    assert_eq!(values["replicaCount"], 2);
    // A port of a secret store is a number under a setting, not a credential.
    assert_eq!(values["secretPort"], 5432);
}

/// Booleans, null and settings of credentials are not credentials (#59);
/// strings under credential names still are.
#[test]
fn get_values_keeps_switches_and_settings_under_sensitive_names() {
    let mut values = json!({
        "automountServiceAccountToken": true,
        "serviceAccount": {"automountServiceAccountToken": false, "name": "app"},
        "auth": {"enabled": true, "existingSecret": null, "tokenTTL": 3600},
        "passwordMinLength": 12,
        "serviceAccountToken": "eyJhbGciOi.plain-but-named",
        "apiToken": "abc",
        "password": "hunter2",
        "tokens": [1, "t1"],
    });
    let mut masked = mask_values_listed(&mut values);
    masked.sort();
    assert_eq!(
        masked,
        [
            "apiToken",
            "password",
            "serviceAccountToken",
            "tokens[0]",
            "tokens[1]"
        ]
    );
    assert_eq!(values["automountServiceAccountToken"], true);
    assert_eq!(
        values["serviceAccount"]["automountServiceAccountToken"],
        false
    );
    assert_eq!(values["auth"]["enabled"], true);
    assert_eq!(values["auth"]["existingSecret"], Value::Null);
    assert_eq!(values["auth"]["tokenTTL"], 3600);
    assert_eq!(values["passwordMinLength"], 12);
    let text = values.to_string();
    for (i, hidden) in ["plain-but-named", "\"abc\"", "hunter2", "t1"]
        .into_iter()
        .enumerate()
    {
        assert!(!text.contains(hidden), "value #{i} is not masked");
    }
}

#[test]
fn the_values_diff_shows_only_changes_and_masks_secrets() {
    let current = json!({
        "crds": {"enabled": false},
        "cainjector": {"replicaCount": 1},
        "auth": {"password": "old-pass"},
        "token": "same-size-1",
        "untouched": "stays",
    });
    let mut new = current.clone();
    merge_values(
        &mut new,
        &json!({"crds": {"enabled": true}, "cainjector": {"replicaCount": 2},
                "token": "same-size-2", "auth": {"password": "a-new-password"}}),
    );
    let diff = masked_values_diff(&current, &new, "values of revision 3", "after");
    for secret in ["old-pass", "a-new-password", "same-size"] {
        assert!(!diff.contains(secret), "a value is not masked");
    }
    assert!(diff.contains("--- values of revision 3"), "{diff}");
    assert!(diff.contains("-  enabled: false"), "{diff}");
    assert!(diff.contains("+  enabled: true"), "{diff}");
    assert!(diff.contains("+  replicaCount: 2"), "{diff}");
    assert!(diff.contains("+  password: \"*** (14 bytes)\""), "{diff}");
    // A secret of the same size still shows as changed.
    assert!(
        diff.contains("+token: \"*** (11 bytes, changed)\""),
        "{diff}"
    );
    assert!(!diff.contains("+untouched"), "{diff}");
    // Nothing changes: no diff.
    assert!(masked_values_diff(&current, &current, "a", "b").is_empty());
}

#[test]
fn an_upgrade_with_a_patch_sends_the_merged_values_to_helm() {
    let current = json!({"replicaCount": 1, "auth": {"password": "kept-secret"}, "args": ["--a"]});
    let patch = json!({"replicaCount": 3, "args": ["--b"]});
    let values = upgrade_values_now(ValuesEdit::Patch(patch), &current, 4);
    assert_eq!(
        parse_values(values.yaml.expose()).unwrap(),
        json!({"replicaCount": 3, "auth": {"password": "kept-secret"}, "args": ["--b"]})
    );
    assert!(!values.masked.contains("kept-secret"), "masked values leak");
    assert!(!values.diff.contains("kept-secret"), "the diff leaks");
    assert!(values.diff.contains("+replicaCount: 3"), "{}", values.diff);
    // Without values the release keeps its own, unchanged.
    let kept = upgrade_values_now(ValuesEdit::Keep, &current, 4);
    assert_eq!(parse_values(kept.yaml.expose()).unwrap(), current);
    assert!(kept.diff.is_empty());
    // values replace: the agent's text goes to helm as it is.
    let replaced = upgrade_values_now(ValuesEdit::Replace("replicaCount: 5\n".into()), &current, 4);
    assert_eq!(replaced.yaml.expose(), "replicaCount: 5\n");
    assert!(
        replaced.diff.contains("-  password: \"*** (11 bytes)\""),
        "{}",
        replaced.diff
    );
    // The audit log masks a patch like values.
    let mut args = json!({"release": "r", "valuesPatch": "auth:\n  password: from-the-agent\n"});
    crate::masking::mask_audit_args(&mut args);
    assert!(!args.to_string().contains("from-the-agent"));
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
    assert!(preview.elsewhere.is_empty());
}

#[test]
fn objects_in_other_namespaces_need_a_tick_and_are_named_with_their_namespace() {
    let objects = [
        resource("apps/v1", "Deployment", "web", None),
        resource("v1", "ConfigMap", "c", Some(OTHER_NS)),
        resource(
            "rbac.authorization.k8s.io/v1",
            "Role",
            "web:leaderelection",
            Some("kube-system"),
        ),
    ];
    for mode in [SessionMode::ClientOnly, SessionMode::Impersonation] {
        let preview = check_objects(&objects, NS, &catalog(), mode).unwrap();
        assert_eq!(
            preview.objects,
            [
                "Deployment web".to_owned(),
                format!("ConfigMap c (namespace {OTHER_NS})"),
                "Role web:leaderelection (namespace kube-system)".to_owned(),
            ]
        );
        assert_eq!(
            paths(&preview.findings),
            [
                format!("ConfigMap c (namespace {OTHER_NS})").as_str(),
                "Role web:leaderelection (namespace kube-system)",
            ]
        );
        assert!(
            preview.findings[1].message.contains("grants rights"),
            "{:?}",
            preview.findings
        );
        assert_eq!(
            other_namespaces(&preview.elsewhere),
            [OTHER_NS.to_owned(), "kube-system".to_owned()]
        );
        // The ServiceAccount's Role never gets the other namespace's kinds.
        assert!(!preview.kinds.contains(&("".into(), "configmaps".into())));
        assert!(preview.cluster.is_empty());
    }
    // A temporary ServiceAccount of NS cannot change another namespace.
    let denial = check_objects(
        &objects,
        NS,
        &catalog(),
        SessionMode::TemporaryServiceAccount,
    )
    .unwrap_err();
    assert_eq!(denial.code, DenialCode::Namespace);
    assert!(denial.message.contains(OTHER_NS), "{}", denial.message);
    // A name that is no namespace is refused without asking.
    for bad in ["*", "../kube-system", "Kube-System", "a b"] {
        let objects = [resource("v1", "ConfigMap", "c", Some(bad))];
        let denial = check_objects(&objects, NS, &catalog(), SessionMode::ClientOnly).unwrap_err();
        assert_eq!(denial.code, DenialCode::Namespace, "{bad}");
    }
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
    // Helm keeps releases in Secrets: without access to them the user is
    // asked for it, as for list_secrets (#53).
    let strict = SecurityPolicy {
        mcp_enabled: true,
        ..SecurityPolicy::default()
    };
    let scope = scope_with(Profile::ReadOnly, &strict, &[NS]);
    let denial = Policy::new(
        &scope,
        &catalog,
        false,
        grants(&[Grant::read(NS)]),
        SessionMode::TemporaryServiceAccount,
    )
    .check(&read)
    .unwrap_err();
    assert_eq!(denial.code, DenialCode::NeedsAccess);
    assert!(denial.access.is_some_and(|access| access.secrets));
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

/// What `helm template` prints for a chart with CRDs, RBAC and a webhook.
const RENDERED: &str = "\
---
# Source: demo/crds/widgets.yaml
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: widgets.example.com
---
# Source: demo/templates/rbac.yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: demo-reader
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: demo
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: demo
---
apiVersion: admissionregistration.k8s.io/v1
kind: ValidatingWebhookConfiguration
metadata:
  name: demo-hook
---
apiVersion: v1
kind: Namespace
metadata:
  name: demo-extra
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: demo
---
apiVersion: v1
kind: Service
metadata:
  name: demo
";

fn paths(findings: &[Finding]) -> Vec<&str> {
    findings.iter().map(|f| f.path.as_str()).collect()
}

#[test]
fn template_finds_cluster_objects_and_rbac_before_the_dry_run() {
    let findings = beyond_service_account(&[RENDERED], NS, &catalog())
        .unwrap()
        .findings;
    assert_eq!(
        paths(&findings),
        [
            "CustomResourceDefinition widgets.example.com",
            "ClusterRole demo-reader",
            "Role demo",
            "RoleBinding demo",
            "ValidatingWebhookConfiguration demo-hook",
            "Namespace demo-extra",
        ]
    );
    // A plain chart has nothing a ServiceAccount cannot do.
    let plain = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n";
    assert_eq!(
        beyond_service_account(&[plain, ""], NS, &catalog()).unwrap(),
        Scan::default()
    );
    // An object in another namespace is asked about (#21), not refused.
    let elsewhere = format!(
        "{plain}---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: c\n  namespace: {OTHER_NS}\n"
    );
    let scan = beyond_service_account(&[&elsewhere], NS, &catalog()).unwrap();
    assert_eq!(
        paths(&scan.findings),
        [format!("ConfigMap c (namespace {OTHER_NS})").as_str()]
    );
    assert_eq!(other_namespaces(&scan.elsewhere), [OTHER_NS.to_owned()]);
    assert!(needs_user_credentials(
        SessionMode::TemporaryServiceAccount,
        &scan.findings
    ));
}

/// cert-manager's leader election Role, which it puts into kube-system.
const LEADER_ELECTION: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: cm-cainjector
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: cm-cainjector:leaderelection
  namespace: kube-system
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: cm-cainjector:leaderelection
  namespace: kube-system
";

#[test]
fn an_upgrade_finds_other_namespaces_in_the_current_revision_too() {
    // The new values move leader election home; helm still deletes the
    // current revision's Role in kube-system.
    let new = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: cm-cainjector\n";
    let scan = beyond_service_account(&[new, LEADER_ELECTION], NS, &catalog()).unwrap();
    assert_eq!(
        paths(&scan.findings),
        [
            "Role cm-cainjector:leaderelection (namespace kube-system)",
            "RoleBinding cm-cainjector:leaderelection (namespace kube-system)",
        ]
    );
    assert_eq!(
        other_namespaces(&scan.elsewhere),
        ["kube-system".to_owned()]
    );
    // Both revisions hold it: listed once.
    let twice =
        beyond_service_account(&[LEADER_ELECTION, LEADER_ELECTION], NS, &catalog()).unwrap();
    assert_eq!(twice.elsewhere.len(), 2);
    assert_eq!(twice.findings.len(), 2);
    // The audit log names the release namespace first, then the others.
    assert_eq!(
        touched(NS, &other_namespaces(&scan.elsewhere)),
        [NS.to_owned(), "kube-system".to_owned()]
    );
    assert_eq!(touched(NS, &[]), [NS.to_owned()]);
}

#[test]
fn another_namespace_needs_the_sessions_rights_there() {
    let catalog = catalog();
    let other = Request::HelmChange {
        namespace: Some(OTHER_NS.into()),
        release: "web".into(),
    };
    // A session on request: reading, then changing the other namespace,
    // as for any tool there; the release namespace's rights do not count.
    let scope = scope_with(Profile::Deploy, &permissive(), &[]);
    let mut have = vec![Grant::read(NS), Grant::modify(NS)];
    for expected in [Grant::read(OTHER_NS), Grant::modify(OTHER_NS)] {
        let denial = Policy::new(
            &scope,
            &catalog,
            false,
            grants(&have),
            SessionMode::TemporaryServiceAccount,
        )
        .check(&other)
        .unwrap_err();
        assert_eq!(denial.code, DenialCode::NeedsGrant);
        assert_eq!(denial.grant, Some(expected.clone()));
        have.push(expected);
    }
    let allowed = Policy::new(
        &scope,
        &catalog,
        false,
        grants(&have),
        SessionMode::TemporaryServiceAccount,
    )
    .check(&other)
    .unwrap();
    assert_eq!(allowed.namespace, OTHER_NS);
    // Client only: reading it is the right; each change is confirmed.
    let client = Policy::new(
        &scope,
        &catalog,
        false,
        grants(&[Grant::read(OTHER_NS)]),
        SessionMode::ClientOnly,
    );
    assert!(client.check(&other).is_ok());
    // A pinned session refuses it without asking.
    let pinned = scope_with(Profile::Deploy, &permissive(), &[NS]);
    let denial = Policy::new(
        &pinned,
        &catalog,
        false,
        grants(&have),
        SessionMode::TemporaryServiceAccount,
    )
    .check(&other)
    .unwrap_err();
    assert_eq!(denial.code, DenialCode::Namespace);
}

#[test]
fn a_refused_namespace_names_its_objects() {
    let text = elsewhere_refusal(
        "kube-system",
        NS,
        &["Role cm-cainjector:leaderelection (namespace kube-system)".into()],
        "namespace \"kube-system\" is outside this session",
    );
    assert!(text.contains("Nothing ran"), "{text}");
    assert!(
        text.contains("Role cm-cainjector:leaderelection (namespace kube-system)"),
        "{text}"
    );
    assert!(text.contains("outside the release namespace"), "{text}");
    assert!(text.contains("outside this session"), "{text}");
}

#[test]
fn an_upgrade_counts_the_objects_of_the_current_revision() {
    // The new chart dropped its ClusterRole: helm deletes it on upgrade.
    let new = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n";
    let current = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n---\n\
                   apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\nmetadata:\n  \
                   name: old-reader\n";
    let findings = beyond_service_account(&[new, current], NS, &catalog())
        .unwrap()
        .findings;
    assert_eq!(paths(&findings), ["ClusterRole old-reader"]);
    // An object in both is listed once.
    let findings = beyond_service_account(&[current, current], NS, &catalog())
        .unwrap()
        .findings;
    assert_eq!(findings.len(), 1);
}

#[test]
fn only_a_service_account_asks_for_the_users_credentials() {
    let findings = beyond_service_account(&[RENDERED, LEADER_ELECTION], NS, &catalog())
        .unwrap()
        .findings;
    assert!(needs_user_credentials(
        SessionMode::TemporaryServiceAccount,
        &findings
    ));
    assert!(!needs_user_credentials(
        SessionMode::TemporaryServiceAccount,
        &[]
    ));
    // Client-only and impersonation keep their identity and the usual tick.
    for mode in [SessionMode::ClientOnly, SessionMode::Impersonation] {
        assert!(!needs_user_credentials(mode, &findings), "{mode:?}");
    }
}

#[test]
fn without_the_tick_the_agent_learns_which_objects() {
    let text = user_refusal(
        &[
            "CustomResourceDefinition widgets.example.com".into(),
            "Role demo".into(),
        ],
        Some("rejected by the user"),
    );
    assert!(text.contains("did not allow"), "{text}");
    assert!(text.contains("Nothing ran"), "{text}");
    assert!(
        text.contains("CustomResourceDefinition widgets.example.com, Role demo"),
        "{text}"
    );
    assert!(text.contains("(rejected by the user)"), "{text}");
}

#[test]
fn helm_changes_of_older_peers_run_as_the_session() {
    let change = HelmChange {
        action: HelmAction::Install,
        release: "web".into(),
        chart: "podinfo/podinfo".into(),
        version: None,
        revision: None,
        values: String::new(),
        objects: vec!["Deployment web".into()],
        cluster_objects: Vec::new(),
        create_namespace: false,
        identity: "serviceaccount:ns/sa".into(),
        credentials: HelmCredentials::AskUser,
        user_objects: vec!["ClusterRole demo".into()],
        namespaces: vec!["kube-system".into()],
        values_diff: "-a: 1\n+a: 2\n".into(),
    };
    let mut json = serde_json::to_value(&change).unwrap();
    assert_eq!(json["credentials"], "ask-user");
    let back: HelmChange = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(back, change);
    let object = json.as_object_mut().unwrap();
    object.remove("credentials");
    object.remove("userObjects");
    object.remove("namespaces");
    object.remove("valuesDiff");
    let old: HelmChange = serde_json::from_value(json).unwrap();
    assert!(old.values_diff.is_empty());
    assert_eq!(old.credentials, HelmCredentials::Session);
    assert!(old.user_objects.is_empty());
    assert!(old.namespaces.is_empty());
}

#[test]
fn a_vault_kubeconfig_shows_nothing_and_goes_to_a_private_file() {
    let config = Kubeconfig::from_yaml(
        "apiVersion: v1\nkind: Config\ncurrent-context: vault\n\
         clusters: [{name: c, cluster: {server: 'https://vault.example.com'}}]\n\
         users: [{name: u, user: {token: vault-token}}]\n\
         contexts: [{name: vault, context: {cluster: c, user: u}}]\n",
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("kubeconfig");
    let vault = VaultKubeconfig::new(config, dir.clone());
    let shown = format!(
        "{:?}",
        HelmEnv {
            vault: Some(vault.clone()),
            ..HelmEnv::default()
        }
    );
    assert!(!shown.contains("vault-token"), "{shown}");
    assert!(!shown.contains("vault.example.com"), "{shown}");

    // Nothing is on disk until a call asks for it.
    assert!(!dir.exists());
    let file = vault.write().unwrap();
    let path = file.path().to_owned();
    assert!(path.starts_with(&dir));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("vault-token")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir), 0o700);
    }
    drop(file);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

/// The render for `lookup` adds rights that only read, and none outside
/// the namespace (#114).
#[test]
fn the_rules_for_lookup_only_read() {
    let rules = helm_read_rules();
    assert!(!rules.is_empty());
    for rule in &rules {
        assert!(!rule.verbs.is_empty());
        assert!(
            rule.verbs
                .iter()
                .all(|verb| ["get", "list", "watch"].contains(&verb.as_str())),
            "{rule:?}"
        );
        assert!(
            !rule
                .api_groups
                .iter()
                .flatten()
                .any(|group| group == "rbac.authorization.k8s.io"),
            "{rule:?}"
        );
    }
}

// ------------------------------------------------------ preview summary (#115)

fn preview_input(old: &str, new: &str) -> crate::helm_preview::PreviewInput {
    crate::helm_preview::PreviewInput {
        old: old.to_owned(),
        new: new.to_owned(),
        old_label: "revision 1".into(),
        new_label: "after the upgrade".into(),
        namespace: NS.to_owned(),
        values_diff: String::new(),
        values: Vec::new(),
        lookup_objects: Vec::new(),
        lookup_whole: false,
    }
}

fn compared(old: &str, new: &str) -> crate::helm_preview::Compared {
    crate::helm_preview::compare(&preview_input(old, new))
}

const SUMMARY_OLD: &str = "---\nkind: Deployment\napiVersion: apps/v1\nmetadata:\n  name: web\n\
spec:\n  template:\n    spec:\n      containers:\n      - name: app\n        image: web:1\n      \
- name: side\n        image: side:1\n---\nkind: ConfigMap\napiVersion: v1\nmetadata:\n  \
name: app\ndata:\n  level: info\n  gone: x\n  DB_PASSWORD: old-pass\n---\nkind: Secret\n\
apiVersion: v1\nmetadata:\n  name: db\ndata:\n  pw: aHVudGVyMg==\n  same: c2FtZQ==\n---\n\
kind: Service\napiVersion: v1\nmetadata:\n  name: web\n---\nkind: Job\napiVersion: batch/v1\n\
metadata:\n  name: old-job\n---\nkind: PersistentVolumeClaim\napiVersion: v1\nmetadata:\n  \
name: data\n  annotations:\n    helm.sh/resource-policy: keep\n";

const SUMMARY_NEW: &str = "---\nkind: Deployment\napiVersion: apps/v1\nmetadata:\n  name: web\n\
spec:\n  template:\n    spec:\n      containers:\n      - name: side\n        image: side:1\n      \
- name: app\n        image: web:2\n---\nkind: ConfigMap\napiVersion: v1\nmetadata:\n  name: app\n\
data:\n  level: debug\n  fresh: y\n  DB_PASSWORD: new-pass\n  blob: QWJjZGVmZ2hpSktMTU5PUDEyMzQ1Njc4OTA\n\
---\nkind: Secret\napiVersion: v1\nmetadata:\n  name: db\ndata:\n  pw: aHVudGVyMw==\n  \
same: c2FtZQ==\n---\nkind: Service\napiVersion: v1\nmetadata:\n  name: web\n---\nkind: Pod\n\
apiVersion: v1\nmetadata:\n  name: fresh\nspec:\n  containers:\n  - name: a\n    image: a:1\n";

#[test]
fn a_summary_says_what_changes_and_nothing_secret() {
    let found = compared(SUMMARY_OLD, SUMMARY_NEW);
    let summary = &found.summary;
    assert_eq!(
        summary["counts"],
        json!({"added": 1, "changed": 3, "removed": 2, "unchanged": 1})
    );
    assert_eq!(summary["added"], json!([{"object": "Pod fresh"}]));
    assert_eq!(
        summary["removed"],
        json!([{"object": "Job old-job"},
               {"object": "PersistentVolumeClaim data", "kept": true}])
    );
    // By container name: `side` moved, only `app` is a change.
    assert_eq!(
        summary["images"],
        json!([
            {"object": "Pod fresh", "path": "spec.containers[a].image", "new": "a:1"},
            {"object": "Deployment web", "new": "web:2", "old": "web:1",
             "path": "spec.template.spec.containers[app].image"},
        ])
    );
    assert_eq!(
        summary["secretKeys"],
        json!([{"object": "Secret db", "key": "pw", "change": "changed"}])
    );
    assert_eq!(
        summary["configMapKeys"],
        json!([
            {"object": "ConfigMap app", "key": "DB_PASSWORD", "change": "changed",
             "old": "*** (8 bytes)", "new": "*** (8 bytes)"},
            {"object": "ConfigMap app", "key": "blob", "change": "added",
             "new": "*** (35 bytes)"},
            {"object": "ConfigMap app", "key": "fresh", "change": "added", "new": "y"},
            {"object": "ConfigMap app", "key": "gone", "change": "removed", "old": "x"},
            {"object": "ConfigMap app", "key": "level", "change": "changed",
             "old": "info", "new": "debug"},
        ])
    );
    assert!(summary.get("omitted").is_none(), "{summary}");
    let everything: String = format!("{summary} {found:?}");
    for value in [
        "old-pass",
        "new-pass",
        "aHVudGVyMg",
        "aHVudGVyMw",
        "QWJjZGVm",
    ] {
        assert!(!everything.contains(value), "{value}: {everything}");
    }
}

#[test]
fn the_kept_diff_shows_that_a_masked_value_changed() {
    let found = format!("{:?}", compared(SUMMARY_OLD, SUMMARY_NEW).entries);
    // The same size on both sides: without the mark nothing would show.
    assert!(found.contains("*** (7 bytes, changed)"), "{found}");
    assert!(found.contains("*** (8 bytes, changed)"), "{found}");
}

#[test]
fn a_summary_marks_what_lookup_may_misstate() {
    let marked = |list: &Value| {
        list.as_array()
            .unwrap()
            .iter()
            .filter(|row| row["lookupAffected"] == true)
            .count()
    };
    let some = crate::helm_preview::compare(&crate::helm_preview::PreviewInput {
        lookup_objects: vec!["Secret db".to_owned(), "Job old-job".to_owned()],
        ..preview_input(SUMMARY_OLD, SUMMARY_NEW)
    });
    assert_eq!(marked(&some.summary["changed"]), 1);
    assert_eq!(marked(&some.summary["removed"]), 1);
    assert_eq!(marked(&some.summary["secretKeys"]), 1);
    assert_eq!(marked(&some.summary["configMapKeys"]), 0);
    assert_eq!(marked(&some.summary["added"]), 0);

    let all = crate::helm_preview::compare(&crate::helm_preview::PreviewInput {
        lookup_whole: true,
        ..preview_input(SUMMARY_OLD, SUMMARY_NEW)
    });
    assert_eq!(marked(&all.summary["changed"]), 3);
    assert_eq!(marked(&all.summary["added"]), 1);
    assert_eq!(marked(&all.summary["images"]), 2);
}

#[test]
fn a_big_chart_stays_a_short_summary_that_counts_what_it_cut() {
    let name = "a-rather-long-object-name-of-a-chart-with-a-lot-of-objects";
    let new: String = (0..2000)
        .map(|n| format!("---\nkind: ConfigMap\nmetadata:\n  name: {name}-{n}\n"))
        .collect();
    let found = compared("", &new);
    let summary = &found.summary;
    assert_eq!(summary["counts"]["added"], 2000);
    let shown = summary["added"].as_array().unwrap().len();
    assert!(shown < 2000);
    assert_eq!(summary["omitted"]["added"], 2000 - shown, "{summary}");
    assert!(summary.to_string().len() <= crate::helm_preview::SUMMARY_BYTES + 200);
    assert_eq!(found.entries.len(), 2000);
    assert!(found.complete);
}

#[test]
fn documents_that_cannot_be_read_are_counted() {
    let found = compared(
        "",
        "---\nkind: [\n---\nkind: ConfigMap\nmetadata:\n  name: a\n",
    );
    assert_eq!(found.summary["unreadableDocuments"], 1);
    assert_eq!(found.summary["counts"]["added"], 1);
}

#[test]
fn changed_values_are_rows_of_masked_paths() {
    let old = json!({
        "image": {"tag": "1.0"},
        "auth": {"password": "old-pass"},
        "gone": true,
        "same": 1,
        "long": "x".repeat(500),
        "list": [1, 2],
    });
    let new = json!({
        "image": {"tag": "2.0"},
        "auth": {"password": "new-pass"},
        "fresh": "aGVsbG8tdGhpcy1pcy1hLXNlY3JldDEyMw",
        "same": 1,
        "long": "y".repeat(500),
        "list": [1, 3],
    });
    let rows = crate::helm_preview::changed_values(&old, &new);
    assert_eq!(
        rows,
        vec![
            json!({"path": "auth.password", "change": "changed",
                   "old": "*** (8 bytes)", "new": "*** (8 bytes)"}),
            json!({"path": "fresh", "change": "added", "new": "*** (34 bytes)"}),
            json!({"path": "gone", "change": "removed", "old": true}),
            json!({"path": "image.tag", "change": "changed", "old": "1.0", "new": "2.0"}),
            json!({"path": "list", "change": "changed", "old": [1, 2], "new": [1, 3]}),
            json!({"path": "long", "change": "changed"}),
        ]
    );
    assert!(crate::helm_preview::changed_values(&old, &old).is_empty());
}

#[test]
fn pages_add_up_to_the_text_and_stay_in_their_size() {
    use crate::helm_preview::{PAGE_BYTES, pages};
    let lines: String = (0..20_000).map(|n| format!("line {n} é\n")).collect();
    let one_line = "é".repeat(PAGE_BYTES);
    for text in [lines.as_str(), one_line.as_str(), "short\n"] {
        let found = pages(text);
        assert!(found.iter().all(|page| page.len() <= PAGE_BYTES));
        assert_eq!(found.concat(), text);
    }
    assert!(pages(&lines).iter().all(|page| page.ends_with('\n')));
    assert!(pages("").is_empty());
}

#[test]
fn a_session_keeps_only_its_last_previews() {
    use crate::helm_preview::{MAX_KEPT_PREVIEWS, PreviewStore};
    let mut store = PreviewStore::default();
    let ids: Vec<String> = (0..=MAX_KEPT_PREVIEWS)
        .map(|_| {
            let kept = store.keep("web", NS, Vec::new(), compared("", SUMMARY_NEW));
            kept["previewId"].as_str().unwrap().to_owned()
        })
        .collect();
    assert!(!store.has(&ids[0]));
    assert!(ids[1..].iter().all(|id| store.has(id)));
    store.clear();
    assert!(!store.has(&ids[1]));
}
