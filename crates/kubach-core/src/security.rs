//! Security checks for pods and pod templates.
//!
//! [`check_pod_spec`] takes a `PodSpec`, so Deployments, StatefulSets, Jobs
//! etc. can pass their `spec.template.spec`. Ephemeral (debug) containers are
//! not checked.

use k8s_openapi::api::core::v1::{Container, Pod, PodSecurityContext, PodSpec};
use kubach_i18n::{Message, msg};
use kube::api::DynamicObject;
use serde::Deserialize as _;

use crate::plugin::Note;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Low,
    Medium,
    High,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Low => "Low",
            Severity::Medium => "Medium",
            Severity::High => "High",
        }
    }

    /// [`label`](Self::label) as a message the UI translates.
    pub fn message(self) -> Message {
        match self {
            Severity::Low => msg!("security-severity-low"),
            Severity::Medium => msg!("security-severity-medium"),
            Severity::High => msg!("security-severity-high"),
        }
    }
}

/// Which check produced a finding, for filtering and stable identification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Check {
    Privileged,
    PrivilegeEscalation,
    RunAsRoot,
    HostPath,
    HostNamespace,
    DangerousCapability,
    MissingLimits,
    MissingRequests,
    MutableImageTag,
    WritableRootFilesystem,
}

/// Title and hint are [`Message`]s: the UI shows them in its language.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub check: Check,
    pub severity: Severity,
    /// `None` for pod-level findings.
    pub container: Option<String>,
    pub title: Message,
    pub hint: Message,
}

impl Finding {
    /// The finding as a note of a details section (`High: title` → hint).
    pub fn note(&self) -> Note {
        Note {
            severity: self.severity.message(),
            title: self.title.clone(),
            summary: None,
            message: String::new(),
            hint: Some(self.hint.clone()),
        }
    }
}

const DANGEROUS_CAPABILITIES: [&str; 5] =
    ["ALL", "SYS_ADMIN", "NET_ADMIN", "SYS_MODULE", "SYS_PTRACE"];

/// Checks a pod; a pod without a spec has no findings.
pub fn check_pod(pod: &Pod) -> Vec<Finding> {
    pod.spec.as_ref().map(check_pod_spec).unwrap_or_default()
}

/// Checks a pod held as a dynamic object (as the resource cache stores
/// it). A missing or malformed `spec` has no findings.
pub fn check_pod_object(object: &DynamicObject) -> Vec<Finding> {
    object
        .data
        .get("spec")
        .and_then(|spec| PodSpec::deserialize(spec).ok())
        .map(|spec| check_pod_spec(&spec))
        .unwrap_or_default()
}

/// Number of findings per severity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub high: usize,
    pub medium: usize,
    pub low: usize,
}

impl Counts {
    pub fn of(findings: &[Finding]) -> Self {
        findings
            .iter()
            .fold(Self::default(), |mut counts, finding| {
                match finding.severity {
                    Severity::High => counts.high += 1,
                    Severity::Medium => counts.medium += 1,
                    Severity::Low => counts.low += 1,
                }
                counts
            })
    }

    pub fn total(&self) -> usize {
        self.high + self.medium + self.low
    }

    /// The most severe level present.
    pub fn worst(&self) -> Option<Severity> {
        if self.high > 0 {
            Some(Severity::High)
        } else if self.medium > 0 {
            Some(Severity::Medium)
        } else if self.low > 0 {
            Some(Severity::Low)
        } else {
            None
        }
    }
}

/// Checks a pod spec, most severe findings first.
pub fn check_pod_spec(spec: &PodSpec) -> Vec<Finding> {
    let mut findings = Vec::new();
    check_pod_level(spec, &mut findings);
    let pod_security = spec.security_context.as_ref();
    let containers = spec
        .init_containers
        .iter()
        .flatten()
        .chain(&spec.containers);
    for container in containers {
        check_container(container, pod_security, &mut findings);
    }
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.severity));
    findings
}

fn check_pod_level(spec: &PodSpec, findings: &mut Vec<Finding>) {
    let namespaces = [
        (
            spec.host_network,
            "hostNetwork",
            msg!("security-host-network-hint"),
        ),
        (spec.host_pid, "hostPID", msg!("security-host-pid-hint")),
        (spec.host_ipc, "hostIPC", msg!("security-host-ipc-hint")),
    ];
    for (enabled, field, hint) in namespaces {
        if enabled == Some(true) {
            findings.push(Finding {
                check: Check::HostNamespace,
                severity: Severity::High,
                container: None,
                title: msg!("security-host-namespace", field = field),
                hint,
            });
        }
    }

    for volume in spec.volumes.iter().flatten() {
        if let Some(host_path) = &volume.host_path {
            findings.push(Finding {
                check: Check::HostPath,
                severity: Severity::High,
                container: None,
                title: msg!("security-host-path", volume = volume.name.as_str()),
                hint: msg!("security-host-path-hint", path = host_path.path.as_str()),
            });
        }
    }
}

fn check_container(
    container: &Container,
    pod_security: Option<&PodSecurityContext>,
    findings: &mut Vec<Finding>,
) {
    let mut push = |check, severity, title: Message, hint: Message| {
        findings.push(Finding {
            check,
            severity,
            container: Some(container.name.clone()),
            title,
            hint,
        });
    };
    let security = container.security_context.as_ref();

    if security.and_then(|s| s.privileged) == Some(true) {
        push(
            Check::Privileged,
            Severity::High,
            msg!("security-privileged"),
            msg!("security-privileged-hint"),
        );
    }

    if security.and_then(|s| s.allow_privilege_escalation) != Some(false) {
        push(
            Check::PrivilegeEscalation,
            Severity::Medium,
            msg!("security-privilege-escalation"),
            msg!("security-privilege-escalation-hint"),
        );
    }

    let run_as_user = security
        .and_then(|s| s.run_as_user)
        .or(pod_security.and_then(|s| s.run_as_user));
    let run_as_non_root = security
        .and_then(|s| s.run_as_non_root)
        .or(pod_security.and_then(|s| s.run_as_non_root));
    if run_as_user == Some(0) {
        push(
            Check::RunAsRoot,
            Severity::High,
            msg!("security-runs-as-root"),
            msg!("security-runs-as-root-hint"),
        );
    } else if run_as_user.is_none() && run_as_non_root != Some(true) {
        push(
            Check::RunAsRoot,
            Severity::Medium,
            msg!("security-may-run-as-root"),
            msg!("security-may-run-as-root-hint"),
        );
    }

    let added = security
        .and_then(|s| s.capabilities.as_ref())
        .and_then(|c| c.add.as_ref());
    for capability in added.into_iter().flatten() {
        let normalized = capability.to_ascii_uppercase();
        let normalized = normalized.strip_prefix("CAP_").unwrap_or(&normalized);
        if DANGEROUS_CAPABILITIES.contains(&normalized) {
            push(
                Check::DangerousCapability,
                Severity::High,
                msg!("security-capability", capability = normalized),
                msg!("security-capability-hint"),
            );
        }
    }

    let resources = container.resources.as_ref();
    let limits = resources
        .and_then(|r| r.limits.as_ref())
        .filter(|l| !l.is_empty());
    match limits {
        None => push(
            Check::MissingLimits,
            Severity::Medium,
            msg!("security-no-limits"),
            msg!("security-no-limits-hint"),
        ),
        Some(limits) if !limits.contains_key("memory") => push(
            Check::MissingLimits,
            Severity::Medium,
            msg!("security-no-memory-limit"),
            msg!("security-no-memory-limit-hint"),
        ),
        Some(_) => {}
    }
    if resources
        .and_then(|r| r.requests.as_ref())
        .is_none_or(|r| r.is_empty())
    {
        push(
            Check::MissingRequests,
            Severity::Low,
            msg!("security-no-requests"),
            msg!("security-no-requests-hint"),
        );
    }

    if let Some(image) = &container.image
        && let Some(problem) = mutable_tag(image)
    {
        push(
            Check::MutableImageTag,
            Severity::Medium,
            problem,
            msg!("security-mutable-tag-hint", image = image.as_str()),
        );
    }

    if security.and_then(|s| s.read_only_root_filesystem) != Some(true) {
        push(
            Check::WritableRootFilesystem,
            Severity::Low,
            msg!("security-writable-root"),
            msg!("security-writable-root-hint"),
        );
    }
}

/// Title of the problem when `image` is untagged or `:latest` without a digest.
fn mutable_tag(image: &str) -> Option<Message> {
    if image.contains('@') {
        return None;
    }
    let last_segment = image.rsplit('/').next().unwrap_or(image);
    match last_segment.split_once(':') {
        None => Some(msg!("security-untagged-image")),
        Some((_, "latest")) => Some(msg!("security-latest-image")),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A container that passes every check; tests break one thing at a time.
    fn hardened() -> Value {
        json!({
            "name": "app",
            "image": "nginx:1.27",
            "securityContext": {
                "allowPrivilegeEscalation": false,
                "runAsNonRoot": true,
                "readOnlyRootFilesystem": true,
            },
            "resources": {
                "limits": { "cpu": "500m", "memory": "256Mi" },
                "requests": { "cpu": "100m", "memory": "128Mi" },
            },
        })
    }

    fn spec_with(container: Value) -> PodSpec {
        serde_json::from_value(json!({ "containers": [container] })).unwrap()
    }

    fn patched(patch: Value) -> PodSpec {
        let mut container = hardened();
        merge(&mut container, patch);
        spec_with(container)
    }

    fn merge(target: &mut Value, patch: Value) {
        match (target, patch) {
            (Value::Object(target), Value::Object(patch)) => {
                for (key, value) in patch {
                    if value.is_null() {
                        target.remove(&key);
                    } else {
                        merge(target.entry(key).or_insert(Value::Null), value);
                    }
                }
            }
            (target, patch) => *target = patch,
        }
    }

    fn checks(findings: &[Finding]) -> Vec<(Check, Severity)> {
        findings.iter().map(|f| (f.check, f.severity)).collect()
    }

    #[test]
    fn hardened_container_is_clean() {
        assert_eq!(check_pod_spec(&spec_with(hardened())), []);
    }

    #[test]
    fn pod_without_spec() {
        assert_eq!(check_pod(&Pod::default()), []);
    }

    #[test]
    fn check_pod_uses_spec() {
        let pod: Pod = serde_json::from_value(json!({
            "metadata": { "name": "web" },
            "spec": { "containers": [hardened()], "hostPID": true },
        }))
        .unwrap();
        assert_eq!(
            checks(&check_pod(&pod)),
            [(Check::HostNamespace, Severity::High)]
        );
    }

    #[test]
    fn privileged() {
        let findings = check_pod_spec(&patched(
            json!({ "securityContext": { "privileged": true } }),
        ));
        assert_eq!(checks(&findings), [(Check::Privileged, Severity::High)]);
        assert_eq!(findings[0].container.as_deref(), Some("app"));
        assert_eq!(findings[0].title.english(), "Privileged container");
    }

    #[test]
    fn privilege_escalation_not_disabled() {
        for value in [Value::Null, json!(true)] {
            let spec = patched(json!({ "securityContext": { "allowPrivilegeEscalation": value } }));
            assert_eq!(
                checks(&check_pod_spec(&spec)),
                [(Check::PrivilegeEscalation, Severity::Medium)]
            );
        }
    }

    #[test]
    fn run_as_non_root_missing() {
        let spec = patched(json!({ "securityContext": { "runAsNonRoot": null } }));
        assert_eq!(
            checks(&check_pod_spec(&spec)),
            [(Check::RunAsRoot, Severity::Medium)]
        );
    }

    #[test]
    fn run_as_root_user() {
        let spec = patched(json!({ "securityContext": { "runAsNonRoot": null, "runAsUser": 0 } }));
        let findings = check_pod_spec(&spec);
        assert_eq!(checks(&findings), [(Check::RunAsRoot, Severity::High)]);
        assert_eq!(findings[0].title.english(), "Runs as root");
    }

    #[test]
    fn non_root_user_is_fine() {
        let spec =
            patched(json!({ "securityContext": { "runAsNonRoot": null, "runAsUser": 1000 } }));
        assert_eq!(check_pod_spec(&spec), []);
    }

    #[test]
    fn pod_security_context_is_inherited() {
        let mut container = hardened();
        merge(
            &mut container,
            json!({ "securityContext": { "runAsNonRoot": null } }),
        );
        let mut spec = spec_with(container.clone());
        spec.security_context = Some(PodSecurityContext {
            run_as_non_root: Some(true),
            ..Default::default()
        });
        assert_eq!(check_pod_spec(&spec), []);

        spec.security_context = Some(PodSecurityContext {
            run_as_user: Some(0),
            ..Default::default()
        });
        assert_eq!(
            checks(&check_pod_spec(&spec)),
            [(Check::RunAsRoot, Severity::High)]
        );
    }

    #[test]
    fn container_overrides_pod_security_context() {
        let mut container = hardened();
        merge(
            &mut container,
            json!({ "securityContext": { "runAsUser": 1000 } }),
        );
        let mut spec = spec_with(container);
        spec.security_context = Some(PodSecurityContext {
            run_as_user: Some(0),
            ..Default::default()
        });
        assert_eq!(check_pod_spec(&spec), []);
    }

    #[test]
    fn host_path_volume() {
        let spec: PodSpec = serde_json::from_value(json!({
            "containers": [hardened()],
            "volumes": [
                { "name": "docker", "hostPath": { "path": "/var/run/docker.sock" } },
                { "name": "cache", "emptyDir": {} },
            ],
        }))
        .unwrap();
        let findings = check_pod_spec(&spec);
        assert_eq!(checks(&findings), [(Check::HostPath, Severity::High)]);
        assert_eq!(findings[0].container, None);
        assert_eq!(findings[0].title.english(), "hostPath volume \"docker\"");
        assert!(findings[0].hint.english().contains("/var/run/docker.sock"));
    }

    #[test]
    fn host_namespaces() {
        let spec: PodSpec = serde_json::from_value(json!({
            "containers": [hardened()],
            "hostNetwork": true,
            "hostPID": true,
            "hostIPC": true,
        }))
        .unwrap();
        let findings = check_pod_spec(&spec);
        let titles: Vec<_> = findings.iter().map(|f| f.title.english()).collect();
        assert_eq!(
            titles,
            ["hostNetwork enabled", "hostPID enabled", "hostIPC enabled"]
        );
        assert!(findings.iter().all(|f| f.check == Check::HostNamespace));

        let spec: PodSpec = serde_json::from_value(json!({
            "containers": [hardened()],
            "hostNetwork": false,
        }))
        .unwrap();
        assert_eq!(check_pod_spec(&spec), []);
    }

    #[test]
    fn dangerous_capabilities() {
        let spec = patched(json!({ "securityContext": { "capabilities": {
            "add": ["SYS_ADMIN", "CAP_NET_ADMIN", "all", "NET_BIND_SERVICE"],
            "drop": ["ALL"],
        } } }));
        let findings = check_pod_spec(&spec);
        let titles: Vec<_> = findings.iter().map(|f| f.title.english()).collect();
        assert_eq!(
            titles,
            [
                "Adds capability SYS_ADMIN",
                "Adds capability NET_ADMIN",
                "Adds capability ALL",
            ]
        );
        assert!(findings.iter().all(|f| f.severity == Severity::High));
    }

    #[test]
    fn missing_resources() {
        let spec = patched(json!({ "resources": null }));
        assert_eq!(
            checks(&check_pod_spec(&spec)),
            [
                (Check::MissingLimits, Severity::Medium),
                (Check::MissingRequests, Severity::Low),
            ]
        );
    }

    #[test]
    fn empty_resource_maps_count_as_missing() {
        let mut container = hardened();
        container["resources"] = json!({ "limits": {}, "requests": {} });
        let spec = spec_with(container);
        assert_eq!(
            checks(&check_pod_spec(&spec)),
            [
                (Check::MissingLimits, Severity::Medium),
                (Check::MissingRequests, Severity::Low),
            ]
        );
    }

    #[test]
    fn missing_memory_limit() {
        let spec = patched(json!({ "resources": { "limits": { "memory": null } } }));
        let findings = check_pod_spec(&spec);
        assert_eq!(
            checks(&findings),
            [(Check::MissingLimits, Severity::Medium)]
        );
        assert_eq!(findings[0].title.english(), "No memory limit");
    }

    #[test]
    fn mutable_image_tags() {
        let tag = |image: &str| mutable_tag(image).map(|title| title.english());
        assert_eq!(tag("nginx").as_deref(), Some("Untagged image"));
        assert_eq!(tag("nginx:latest").as_deref(), Some("Image uses :latest"));
        assert_eq!(
            tag("registry:5000/team/nginx").as_deref(),
            Some("Untagged image")
        );
        assert_eq!(
            tag("registry:5000/team/nginx:latest").as_deref(),
            Some("Image uses :latest")
        );
        assert_eq!(mutable_tag("registry:5000/team/nginx:1.27"), None);
        assert_eq!(mutable_tag("nginx@sha256:abc"), None);
        assert_eq!(mutable_tag("nginx:latest@sha256:abc"), None);
    }

    #[test]
    fn latest_image_finding() {
        let spec = patched(json!({ "image": "ghcr.io/acme/api:latest" }));
        let findings = check_pod_spec(&spec);
        assert_eq!(
            checks(&findings),
            [(Check::MutableImageTag, Severity::Medium)]
        );
        assert!(
            findings[0]
                .hint
                .english()
                .contains("ghcr.io/acme/api:latest")
        );
    }

    #[test]
    fn writable_root_filesystem() {
        let spec = patched(json!({ "securityContext": { "readOnlyRootFilesystem": false } }));
        assert_eq!(
            checks(&check_pod_spec(&spec)),
            [(Check::WritableRootFilesystem, Severity::Low)]
        );
    }

    #[test]
    fn bare_container_reports_everything_sorted() {
        let spec = spec_with(json!({ "name": "bare", "image": "busybox" }));
        let findings = check_pod_spec(&spec);
        assert_eq!(
            checks(&findings),
            [
                (Check::PrivilegeEscalation, Severity::Medium),
                (Check::RunAsRoot, Severity::Medium),
                (Check::MissingLimits, Severity::Medium),
                (Check::MutableImageTag, Severity::Medium),
                (Check::MissingRequests, Severity::Low),
                (Check::WritableRootFilesystem, Severity::Low),
            ]
        );
        assert!(
            findings
                .iter()
                .all(|f| f.container.as_deref() == Some("bare"))
        );
    }

    #[test]
    fn dynamic_pod_object() {
        let object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": { "name": "web", "namespace": "shop" },
            "spec": {
                "containers": [hardened()],
                "volumes": [{ "name": "root", "hostPath": { "path": "/" } }],
            },
        }))
        .unwrap();
        assert_eq!(
            checks(&check_pod_object(&object)),
            [(Check::HostPath, Severity::High)]
        );

        let no_spec: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod", "metadata": { "name": "x" },
        }))
        .unwrap();
        assert_eq!(check_pod_object(&no_spec), []);
        let mut broken = no_spec;
        broken.data = json!({ "spec": { "containers": "nope" } });
        assert_eq!(check_pod_object(&broken), []);
    }

    #[test]
    fn counts_by_severity() {
        let spec = spec_with(json!({ "name": "bare", "image": "busybox",
            "securityContext": { "privileged": true } }));
        let counts = Counts::of(&check_pod_spec(&spec));
        assert_eq!(
            counts,
            Counts {
                high: 1,
                medium: 4,
                low: 2
            }
        );
        assert_eq!(counts.total(), 7);
        assert_eq!(counts.worst(), Some(Severity::High));
        assert_eq!(Counts::default().worst(), None);
        assert_eq!(
            Counts {
                low: 1,
                ..Default::default()
            }
            .worst(),
            Some(Severity::Low)
        );
        assert_eq!(Severity::Medium.label(), "Medium");
    }

    #[test]
    fn init_containers_are_checked() {
        let spec: PodSpec = serde_json::from_value(json!({
            "initContainers": [{
                "name": "setup",
                "image": "busybox:1.36",
                "securityContext": { "privileged": true, "allowPrivilegeEscalation": false,
                                     "runAsNonRoot": true, "readOnlyRootFilesystem": true },
                "resources": hardened()["resources"].clone(),
            }],
            "containers": [hardened()],
        }))
        .unwrap();
        let findings = check_pod_spec(&spec);
        assert_eq!(checks(&findings), [(Check::Privileged, Severity::High)]);
        assert_eq!(findings[0].container.as_deref(), Some("setup"));
    }
}
