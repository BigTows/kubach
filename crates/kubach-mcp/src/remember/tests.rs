use crate::access::Access;
use crate::confirm::{ConfirmationRequest, RequestKind};
use crate::grants::Grant;
use crate::helm_tools::{HelmAction, HelmChange, HelmCredentials};
use crate::policy::Finding;
use crate::remember::{
    ExactCommand, RememberNote, Remembered, RememberedAction, RememberedSet, rememberable,
};
use crate::scope::SessionMode;
use crate::secret_write::{SecretOp, SecretRequest};

const NS: &str = "cert-manager";

fn call(tool: &str) -> ConfirmationRequest {
    ConfirmationRequest {
        kind: RequestKind::Call,
        session_id: "s".into(),
        context: "minikube".into(),
        client: "test".into(),
        tool: tool.into(),
        namespace: NS.into(),
        summary: "summary".into(),
        details: String::new(),
        dangerous: Vec::new(),
        mode: SessionMode::TemporaryServiceAccount,
        allow_remember: false,
        diff_cut: None,
    }
}

fn helm_change(action: HelmAction) -> HelmChange {
    HelmChange {
        action,
        release: "web".into(),
        chart: "bitnami/nginx".into(),
        version: None,
        revision: None,
        values: String::new(),
        objects: vec!["Deployment web".into()],
        cluster_objects: Vec::new(),
        create_namespace: false,
        identity: "serviceaccount:cert-manager/kubach-mcp-s".into(),
        credentials: HelmCredentials::Session,
        user_objects: Vec::new(),
        namespaces: Vec::new(),
        values_diff: String::new(),
        diff_cut: None,
    }
}

fn helm(tool: &str, change: HelmChange) -> ConfirmationRequest {
    ConfirmationRequest {
        kind: RequestKind::Helm {
            helm: Box::new(change),
        },
        ..call(tool)
    }
}

fn secret(op: SecretOp, tool: &str) -> ConfirmationRequest {
    ConfirmationRequest {
        kind: RequestKind::Secret {
            secret: Box::new(SecretRequest {
                op,
                name: "db".into(),
                secret_type: "Opaque".into(),
                keys: Vec::new(),
                remove: Vec::new(),
                existing: Vec::new(),
                source: None,
                used_by: Vec::new(),
                explanation: "because".into(),
            }),
        },
        ..call(tool)
    }
}

fn remembered(action: RememberedAction) -> Option<Remembered> {
    Some(Remembered {
        action,
        namespace: NS.into(),
        exact: None,
    })
}

fn finding() -> Finding {
    Finding {
        document: Some(0),
        path: ".spec.template.spec.volumes[0].hostPath".into(),
        message: "mounts a host path".into(),
    }
}

#[test]
fn listed_changes_may_be_remembered() {
    assert_eq!(
        rememberable(&call("scale")),
        remembered(RememberedAction::Scale)
    );
    assert_eq!(
        rememberable(&call("rollout_restart")),
        remembered(RememberedAction::RolloutRestart)
    );
    assert_eq!(
        rememberable(&call("apply_manifest")),
        remembered(RememberedAction::ApplyManifest)
    );
    let exec = ConfirmationRequest {
        kind: RequestKind::Exec {
            command: vec!["id".into()],
        },
        ..call("exec")
    };
    assert_eq!(rememberable(&exec), remembered(RememberedAction::Exec));
    let forward = ConfirmationRequest {
        kind: RequestKind::PortForward {
            target: "service/web".into(),
            port: "80".into(),
            local_port: None,
        },
        ..call("port_forward_start")
    };
    assert_eq!(
        rememberable(&forward),
        remembered(RememberedAction::PortForwardStart)
    );
    for (tool, action, remembered_as) in [
        (
            "helm_install",
            HelmAction::Install,
            RememberedAction::HelmInstall,
        ),
        (
            "helm_upgrade",
            HelmAction::Upgrade,
            RememberedAction::HelmUpgrade,
        ),
        (
            "helm_rollback",
            HelmAction::Rollback,
            RememberedAction::HelmRollback,
        ),
    ] {
        assert_eq!(
            rememberable(&helm(tool, helm_change(action))),
            remembered(remembered_as),
            "{tool}"
        );
    }
    // Each kind on its own: an upgrade is not an install.
    assert_ne!(
        rememberable(&helm("helm_upgrade", helm_change(HelmAction::Upgrade))),
        rememberable(&helm("helm_install", helm_change(HelmAction::Install)))
    );
    // The impersonated identity holds the boundary too.
    let impersonated = ConfirmationRequest {
        mode: SessionMode::Impersonation,
        ..call("scale")
    };
    assert!(rememberable(&impersonated).is_some());
}

#[test]
fn deletions_are_always_asked() {
    assert_eq!(rememberable(&call("delete_resource")), None);
    assert_eq!(
        rememberable(&helm("helm_uninstall", helm_change(HelmAction::Uninstall))),
        None
    );
    // A request that names one tool and does another is not remembered.
    assert_eq!(
        rememberable(&helm("helm_install", helm_change(HelmAction::Uninstall))),
        None
    );
    assert_eq!(
        rememberable(&helm("helm_uninstall", helm_change(HelmAction::Install))),
        None
    );
}

#[test]
fn secrets_are_always_asked() {
    for (op, tool) in [
        (SecretOp::Create, "request_secret_input"),
        (SecretOp::Update, "request_secret_input"),
        (SecretOp::Copy, "copy_secret"),
        (SecretOp::Delete, "delete_secret"),
    ] {
        assert_eq!(rememberable(&secret(op, tool)), None, "{tool}");
    }
    let reveal = ConfirmationRequest {
        kind: RequestKind::Grant {
            grant: Grant::secret(NS, "db"),
        },
        ..call("get_secret_value")
    };
    assert_eq!(rememberable(&reveal), None);
    let blocked = ConfirmationRequest {
        kind: RequestKind::RevealBlocked { name: "db".into() },
        ..call("get_secret_value")
    };
    assert_eq!(rememberable(&blocked), None);
    // Even a plain call that names a Secret tool.
    assert_eq!(rememberable(&call("get_secret_value")), None);
    assert_eq!(rememberable(&call("delete_secret")), None);
}

#[test]
fn helm_with_the_users_credentials_is_always_asked() {
    for credentials in [HelmCredentials::AskUser, HelmCredentials::User] {
        for (tool, action) in [
            ("helm_install", HelmAction::Install),
            ("helm_upgrade", HelmAction::Upgrade),
            ("helm_rollback", HelmAction::Rollback),
        ] {
            let mut change = helm_change(action);
            change.credentials = credentials;
            change.identity = crate::USER_IDENTITY.into();
            assert_eq!(rememberable(&helm(tool, change)), None, "{tool}");
        }
    }
    // Objects only the user's credentials could create, even if marked
    // otherwise.
    let mut change = helm_change(HelmAction::Upgrade);
    change.user_objects = vec!["ClusterRole web".into()];
    assert_eq!(rememberable(&helm("helm_upgrade", change)), None);
}

#[test]
fn helm_beyond_its_namespace_is_always_asked() {
    let mut cluster = helm_change(HelmAction::Install);
    cluster.cluster_objects = vec!["ClusterRole web".into()];
    assert_eq!(rememberable(&helm("helm_install", cluster)), None);
    let mut elsewhere = helm_change(HelmAction::Upgrade);
    elsewhere.namespaces = vec!["monitoring".into()];
    assert_eq!(rememberable(&helm("helm_upgrade", elsewhere)), None);
    let mut create = helm_change(HelmAction::Install);
    create.create_namespace = true;
    assert_eq!(rememberable(&helm("helm_install", create)), None);
}

#[test]
fn dangerous_findings_are_always_asked() {
    for tool in ["scale", "rollout_restart", "apply_manifest"] {
        let request = ConfirmationRequest {
            dangerous: vec![finding()],
            ..call(tool)
        };
        assert_eq!(rememberable(&request), None, "{tool}");
    }
    let rollback = ConfirmationRequest {
        dangerous: vec![finding()],
        ..helm("helm_rollback", helm_change(HelmAction::Rollback))
    };
    assert_eq!(rememberable(&rollback), None);
}

#[test]
fn other_questions_are_always_asked() {
    let questions = [
        RequestKind::Grant {
            grant: Grant::modify(NS),
        },
        RequestKind::Grant {
            grant: Grant::read(NS),
        },
        RequestKind::Access {
            raise: Access::ALL,
            current: Access::NONE,
            server_url: "https://127.0.0.1".into(),
        },
        RequestKind::ChartSource {
            source: "oci://example.com/charts".into(),
        },
        // Whatever its tool: a batch is never remembered (#108).
        RequestKind::Batch {
            batch: Box::new(crate::confirm::BatchChange {
                action: crate::confirm::BatchAction::Scale,
                objects: Vec::new(),
                left_out: Vec::new(),
            }),
        },
        RequestKind::UseContext {
            server_url: "https://127.0.0.1".into(),
            source: "kubeconfig".into(),
        },
    ];
    for kind in questions {
        let request = ConfirmationRequest {
            kind,
            ..call("scale")
        };
        assert_eq!(rememberable(&request), None);
    }
    // Logs of a pod the agent shaped (#190).
    assert_eq!(rememberable(&call("get_logs")), None);
    // A kind that does not fit the tool.
    let exec_as_call = call("exec");
    assert_eq!(rememberable(&exec_as_call), None);
    let no_namespace = ConfirmationRequest {
        namespace: String::new(),
        ..call("scale")
    };
    assert_eq!(rememberable(&no_namespace), None);
}

#[test]
fn client_only_namespaces_are_always_asked() {
    let request = ConfirmationRequest {
        mode: SessionMode::ClientOnly,
        ..call("scale")
    };
    assert_eq!(rememberable(&request), None);
    let helm = ConfirmationRequest {
        mode: SessionMode::ClientOnly,
        ..helm("helm_upgrade", helm_change(HelmAction::Upgrade))
    };
    assert_eq!(rememberable(&helm), None);
}

#[test]
fn forgetting_wins_over_a_late_tick() {
    let scale = Remembered {
        action: RememberedAction::Scale,
        namespace: NS.into(),
        exact: None,
    };
    let exec = Remembered {
        action: RememberedAction::Exec,
        namespace: "web".into(),
        exact: None,
    };
    let mut set = RememberedSet::default();
    let epoch = set.epoch();
    assert!(set.insert(scale.clone(), epoch));
    assert!(set.insert(exec.clone(), epoch));
    // Asked before a revocation, answered after it: not kept.
    let asked = set.epoch();
    set.forget_in(NS);
    assert!(!set.contains(&scale) && set.contains(&exec));
    assert!(!set.insert(scale.clone(), asked));
    assert!(!set.contains(&scale));
    set.clear();
    assert!(set.list().is_empty());
}

#[test]
fn the_agent_is_told() {
    let r = Remembered {
        action: RememberedAction::HelmUpgrade,
        namespace: NS.into(),
        exact: None,
    };
    let used = RememberNote::Used(r.clone());
    assert_eq!(used.audit(), "used");
    assert!(
        used.agent_text()
            .contains("helm_upgrade in namespace cert-manager")
    );
    assert_eq!(RememberNote::Saved(r).audit(), "saved");
    assert_eq!(RememberedAction::ALL.len(), 9);
}

fn node_command(node: &str, command: &[&str]) -> ConfirmationRequest {
    ConfirmationRequest {
        kind: RequestKind::NodeCommand {
            node: node.into(),
            command: command.iter().map(|arg| (*arg).to_owned()).collect(),
        },
        // No namespace and no session identity: the user's credentials.
        namespace: String::new(),
        mode: SessionMode::ClientOnly,
        ..call("node_host_read")
    }
}

#[test]
fn a_host_command_is_remembered_only_as_that_exact_command_on_that_node() {
    let cat = node_command("n1", &["cat", "/etc/nginx/nginx.conf"]);
    let offer = rememberable(&cat).expect("offered");
    assert_eq!(offer.action, RememberedAction::NodeHostRead);
    assert_eq!(
        offer.exact,
        Some(ExactCommand {
            node: "n1".into(),
            command: vec!["cat".into(), "/etc/nginx/nginx.conf".into()],
        })
    );
    assert_eq!(
        offer.to_string(),
        "exactly `cat /etc/nginx/nginx.conf` on node n1"
    );

    let mut set = RememberedSet::default();
    let epoch = set.epoch();
    assert!(set.insert(offer.clone(), epoch));
    // The same command again: no window.
    assert!(set.contains(&rememberable(&cat).unwrap()));
    // Another file, another argument, another program, another node: asked.
    for other in [
        node_command("n1", &["cat", "/etc/nginx/conf.d/default.conf"]),
        node_command("n1", &["cat", "/etc/nginx/nginx.conf", "/etc/hosts"]),
        node_command("n1", &["head", "/etc/nginx/nginx.conf"]),
        node_command("n2", &["cat", "/etc/nginx/nginx.conf"]),
    ] {
        assert!(!set.contains(&rememberable(&other).unwrap()), "{other:?}");
    }
    // Revoking a namespace's rights does not touch it; "ask again" does.
    set.forget_in(NS);
    assert!(set.contains(&offer));
    set.clear();
    assert!(!set.contains(&offer));
}

#[test]
fn a_host_command_reading_a_sensitive_file_is_never_remembered() {
    let key = ConfirmationRequest {
        dangerous: vec![finding()],
        ..node_command("n1", &["cat", "/etc/kubernetes/pki/ca.key"])
    };
    assert_eq!(rememberable(&key), None);
    // The kind belongs to its tool alone.
    let other_tool = ConfirmationRequest {
        tool: "exec".into(),
        ..node_command("n1", &["dmesg"])
    };
    assert_eq!(rememberable(&other_tool), None);
}

#[test]
fn an_exact_command_survives_the_bridge() {
    let offer = rememberable(&node_command("n1", &["journalctl", "-k", "-n", "50"])).unwrap();
    let json = serde_json::to_string(&offer).unwrap();
    assert_eq!(serde_json::from_str::<Remembered>(&json).unwrap(), offer);
    // An older peer's item has no `exact`.
    let old: Remembered = serde_json::from_str(r#"{"action":"scale","namespace":"web"}"#).unwrap();
    assert_eq!(
        old,
        Remembered::in_namespace(RememberedAction::Scale, "web")
    );
    assert!(!serde_json::to_string(&old).unwrap().contains("exact"));
}
