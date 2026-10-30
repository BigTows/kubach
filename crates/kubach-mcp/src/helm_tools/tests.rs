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
    });
    let mut masked = mask_values_listed(&mut values);
    masked.sort();
    assert_eq!(
        masked,
        [
            "apiKeys[0]",
            "apiKeys[1]",
            "auth.password",
            "secretPort",
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
