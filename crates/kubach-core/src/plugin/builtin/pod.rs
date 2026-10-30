//! Pod plugin: kubectl-style table, containers, diagnostics and security.

use k8s_openapi::api::core::v1::{
    Container, ContainerPort, ContainerState, ContainerStatus, Event, Pod, PodSpec, Probe,
    ResourceRequirements, Volume,
};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use k8s_openapi::jiff::Timestamp;
use kubach_i18n::msg;
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};

use super::{
    DELETE_ACTION, OVERVIEW, PORT_FORWARD_ACTION, action, creation, decode, delete, fallback_cells,
    name_ns_cells, name_ns_columns, or_none, push_notes, relations_with, section, text, unknown,
};
use crate::diagnostics::{self, FindingKind, Severity};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Brief, Cell, Column, ColumnKind, ContainerBrief,
    ContainerInfo, ContainerPhase, ContainerRole, DetailSection, Field, FieldValue, GenericPlugin,
    Health, HealthStatus, Note, ObjectRef, PluginError, Relation, RelationKind, ResourcePlugin,
    SectionBody, StreamKind,
};
use crate::security;

pub struct PodPlugin;

/// Evicts the pod; one already gone counts as done.
async fn evict(client: &Client, obj: &DynamicObject) -> Result<(), PluginError> {
    let name = obj
        .metadata
        .name
        .as_deref()
        .ok_or_else(|| PluginError::InvalidObject("object has no name".into()))?;
    let namespace = obj
        .metadata
        .namespace
        .as_deref()
        .ok_or_else(|| PluginError::InvalidObject("pod has no namespace".into()))?;
    crate::eviction::evict_pod(client, namespace, name).await?;
    Ok(())
}

/// Summary computed like `kubectl get pods`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PodSummary {
    pub status: String,
    pub ready: usize,
    pub total: usize,
    pub restarts: i32,
    pub last_restart: Option<Timestamp>,
}

fn is_sidecar(c: Option<&Container>) -> bool {
    c.and_then(|c| c.restart_policy.as_deref()) == Some("Always")
}

fn last_finished(cs: &ContainerStatus) -> Option<Timestamp> {
    cs.last_state
        .as_ref()?
        .terminated
        .as_ref()?
        .finished_at
        .as_ref()
        .map(|t| t.0)
}

fn later(a: Option<Timestamp>, b: Option<Timestamp>) -> Option<Timestamp> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn exit_reason(prefix: &str, signal: Option<i32>, code: i32) -> String {
    match signal {
        Some(s) if s != 0 => format!("{prefix}Signal:{s}"),
        _ => format!("{prefix}ExitCode:{code}"),
    }
}

/// Pod conditions that report something to look at when `True`:
/// `DisruptionTarget` means the pod is about to be evicted or preempted.
pub(crate) const WARNING_CONDITIONS: [&str; 1] = ["DisruptionTarget"];

/// Port of kubectl's `printPod` status logic.
pub(crate) fn summarize(pod: &Pod) -> PodSummary {
    let spec = pod.spec.as_ref();
    let status = pod.status.as_ref();
    let init_specs = spec
        .and_then(|s| s.init_containers.as_deref())
        .unwrap_or_default();
    let conditions = status
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or_default();
    let phase = status.and_then(|s| s.phase.clone()).unwrap_or_default();

    let mut reason = phase.clone();
    if let Some(r) = status
        .and_then(|s| s.reason.as_deref())
        .filter(|r| !r.is_empty())
    {
        reason = r.to_owned();
    }
    if conditions
        .iter()
        .any(|c| c.type_ == "PodScheduled" && c.reason.as_deref() == Some("SchedulingGated"))
    {
        reason = "SchedulingGated".into();
    }

    let init_spec = |name: &str| init_specs.iter().find(|c| c.name == name);
    let mut total = spec.map_or(0, |s| s.containers.len())
        + init_specs.iter().filter(|c| is_sidecar(Some(c))).count();
    let mut ready = 0;
    let mut restarts = 0;
    let mut last_restart = None;
    let mut sidecar_restarts = 0;
    let mut sidecar_last_restart = None;

    let mut initializing = false;
    let init_statuses = status
        .and_then(|s| s.init_container_statuses.as_deref())
        .unwrap_or_default();
    for (i, cs) in init_statuses.iter().enumerate() {
        restarts += cs.restart_count;
        last_restart = later(last_restart, last_finished(cs));
        let sidecar = is_sidecar(init_spec(&cs.name));
        if sidecar {
            sidecar_restarts += cs.restart_count;
            sidecar_last_restart = later(sidecar_last_restart, last_finished(cs));
        }
        let state = cs.state.as_ref();
        let terminated = state.and_then(|s| s.terminated.as_ref());
        let waiting = state.and_then(|s| s.waiting.as_ref());
        if terminated.is_some_and(|t| t.exit_code == 0) {
            continue;
        }
        if sidecar && cs.started == Some(true) {
            if cs.ready {
                ready += 1;
            }
            continue;
        }
        if let Some(t) = terminated {
            reason = match t.reason.as_deref().filter(|r| !r.is_empty()) {
                Some(r) => format!("Init:{r}"),
                None => exit_reason("Init:", t.signal, t.exit_code),
            };
        } else if let Some(r) = waiting
            .and_then(|w| w.reason.as_deref())
            .filter(|r| !r.is_empty() && *r != "PodInitializing")
        {
            reason = format!("Init:{r}");
        } else {
            reason = format!("Init:{i}/{}", init_specs.len());
        }
        initializing = true;
        break;
    }

    let initialized = conditions
        .iter()
        .any(|c| c.type_ == "Initialized" && c.status == "True");
    if !initializing || initialized {
        restarts = sidecar_restarts;
        last_restart = sidecar_last_restart;
        let mut has_running = false;
        let statuses = status
            .and_then(|s| s.container_statuses.as_deref())
            .unwrap_or_default();
        for cs in statuses.iter().rev() {
            restarts += cs.restart_count;
            last_restart = later(last_restart, last_finished(cs));
            let state = cs.state.as_ref();
            let waiting = state.and_then(|s| s.waiting.as_ref());
            let terminated = state.and_then(|s| s.terminated.as_ref());
            if let Some(r) = waiting
                .and_then(|w| w.reason.as_deref())
                .filter(|r| !r.is_empty())
            {
                reason = r.to_owned();
            } else if let Some(t) = terminated {
                reason = match t.reason.as_deref().filter(|r| !r.is_empty()) {
                    Some(r) => r.to_owned(),
                    None => exit_reason("", t.signal, t.exit_code),
                };
            } else if cs.ready && state.and_then(|s| s.running.as_ref()).is_some() {
                has_running = true;
                ready += 1;
            }
        }
        if reason == "Completed" && has_running {
            let pod_ready = conditions
                .iter()
                .any(|c| c.type_ == "Ready" && c.status == "True");
            reason = if pod_ready { "Running" } else { "NotReady" }.into();
        }
    }

    if pod.metadata.deletion_timestamp.is_some() {
        if status.and_then(|s| s.reason.as_deref()) == Some("NodeLost") {
            reason = "Unknown".into();
        } else if phase != "Succeeded" && phase != "Failed" {
            reason = "Terminating".into();
        }
    }
    total = total.max(ready);
    PodSummary {
        status: reason,
        ready,
        total,
        restarts,
        last_restart,
    }
}

/// Restarts with the time of the last one (`2 (5m ago)` in views).
fn restarts_cell(s: &PodSummary) -> Cell {
    match s.last_restart {
        Some(t) if s.restarts > 0 => Cell::CountAge(s.restarts.into(), t),
        _ => Cell::Number(s.restarts.into()),
    }
}

/// Health derived from [`diagnostics::diagnose`]; falls back to the phase.
pub(crate) fn pod_health(pod: &Pod, now: Timestamp) -> Health {
    pod_health_with_events(pod, &[], now)
}

/// Like the Pod plugin's health, but event-based findings (`FailedMount`,
/// `FailedCreatePodSandBox`) from the pod's `events` also count.
pub fn pod_health_with_events(pod: &Pod, events: &[Event], now: Timestamp) -> Health {
    let findings = diagnostics::diagnose_at(pod, events, now);
    if let Some(f) = findings.first() {
        match f.severity {
            Severity::Error => return Health::phrase(HealthStatus::Failed, f.title.clone()),
            Severity::Warning => return Health::phrase(HealthStatus::Degraded, f.title.clone()),
            Severity::Info
                if matches!(
                    f.kind,
                    FindingKind::Terminating | FindingKind::SchedulingGated
                ) =>
            {
                return Health::phrase(HealthStatus::Progressing, f.title.clone());
            }
            Severity::Info => {}
        }
    }
    let summary = summarize(pod);
    let phase = pod.status.as_ref().and_then(|s| s.phase.as_deref());
    match phase {
        Some("Running") if summary.ready >= summary.total => {
            Health::new(HealthStatus::Healthy, "Running")
        }
        Some("Running") => Health::phrase(
            HealthStatus::Progressing,
            msg!(
                "state-pod-containers-ready",
                ready = summary.ready,
                total = summary.total
            ),
        ),
        Some("Succeeded") => Health::new(HealthStatus::Healthy, "Completed"),
        Some("Pending") => Health::new(HealthStatus::Progressing, summary.status),
        Some("Failed") => Health::new(HealthStatus::Failed, summary.status),
        _ => Health::unknown(),
    }
}

fn resources_text(r: Option<&ResourceRequirements>, limits: bool) -> String {
    let map = r.and_then(|r| if limits { &r.limits } else { &r.requests }.as_ref());
    map.into_iter()
        .flatten()
        .map(|(k, v)| format!("{k}={}", v.0))
        .collect::<Vec<_>>()
        .join(", ")
}

fn ports(ports: Option<&Vec<ContainerPort>>) -> Vec<String> {
    ports
        .into_iter()
        .flatten()
        .map(|p| {
            let proto = p.protocol.as_deref().unwrap_or("TCP");
            match &p.name {
                Some(n) => format!("{n} {}/{proto}", p.container_port),
                None => format!("{}/{proto}", p.container_port),
            }
        })
        .collect()
}

fn phase(state: Option<&ContainerState>) -> ContainerPhase {
    let Some(state) = state else {
        return ContainerPhase::Unknown;
    };
    if let Some(r) = &state.running {
        return ContainerPhase::Running {
            since: r.started_at.as_ref().map(|t| t.0),
        };
    }
    if let Some(w) = &state.waiting {
        return ContainerPhase::Waiting {
            reason: w.reason.clone().unwrap_or_else(|| "Waiting".into()),
            message: w.message.clone().filter(|m| !m.is_empty()),
        };
    }
    if let Some(t) = &state.terminated {
        return ContainerPhase::Terminated {
            reason: t.reason.clone().unwrap_or_else(|| "Terminated".into()),
            exit_code: t.exit_code,
        };
    }
    ContainerPhase::Unknown
}

fn int_or_string(v: &IntOrString) -> String {
    match v {
        IntOrString::Int(n) => n.to_string(),
        IntOrString::String(s) => s.clone(),
    }
}

/// `http-get :80/healthz · delay 0s · timeout 1s · every 10s · failure 3 ·
/// success 1`: the defaults Kubernetes applies are spelled out, as a short
/// timeout or a missing delay is what usually makes a probe kill a pod.
fn probe_text(p: &Probe) -> String {
    let action = if let Some(h) = &p.http_get {
        let scheme = h
            .scheme
            .as_deref()
            .filter(|s| !s.eq_ignore_ascii_case("HTTP"))
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        format!(
            "http-get {}:{}{}{scheme}",
            h.host.as_deref().unwrap_or_default(),
            int_or_string(&h.port),
            h.path.as_deref().unwrap_or("/")
        )
    } else if let Some(t) = &p.tcp_socket {
        format!(
            "tcp {}:{}",
            t.host.as_deref().unwrap_or_default(),
            int_or_string(&t.port)
        )
    } else if let Some(g) = &p.grpc {
        match g.service.as_deref().filter(|s| !s.is_empty()) {
            Some(service) => format!("grpc :{} service {service}", g.port),
            None => format!("grpc :{}", g.port),
        }
    } else if let Some(e) = &p.exec {
        format!("exec {}", e.command.clone().unwrap_or_default().join(" "))
    } else {
        "unknown".into()
    };
    let mut parts = vec![
        action,
        format!("delay {}s", p.initial_delay_seconds.unwrap_or(0)),
        format!("timeout {}s", p.timeout_seconds.unwrap_or(1)),
        format!("every {}s", p.period_seconds.unwrap_or(10)),
        format!("failure {}", p.failure_threshold.unwrap_or(3)),
        format!("success {}", p.success_threshold.unwrap_or(1)),
    ];
    if let Some(grace) = p.termination_grace_period_seconds {
        parts.push(format!("grace {grace}s"));
    }
    parts.join(" · ")
}

/// Longest termination message kept in [`last_state_text`].
const MESSAGE_LIMIT: usize = 200;

/// The previous run of a container: `Terminated: OOMKilled (exit code 137,
/// signal 9) · started … · finished …`, with the first line of its
/// termination message.
fn last_state_text(state: &ContainerState) -> Option<String> {
    let t = state.terminated.as_ref()?;
    let mut code = format!("exit code {}", t.exit_code);
    if let Some(signal) = t.signal.filter(|s| *s != 0) {
        code.push_str(&format!(", signal {signal}"));
    }
    let mut parts = vec![format!(
        "Terminated: {} ({code})",
        t.reason.as_deref().unwrap_or("Terminated")
    )];
    if let Some(started) = &t.started_at {
        parts.push(format!("started {}", started.0));
    }
    if let Some(finished) = &t.finished_at {
        parts.push(format!("finished {}", finished.0));
    }
    if let Some(line) = t
        .message
        .as_deref()
        .and_then(|m| m.lines().map(str::trim).find(|l| !l.is_empty()))
    {
        let mut line: String = line.chars().take(MESSAGE_LIMIT).collect();
        if line.len() < t.message.as_deref().unwrap_or_default().trim().len() {
            line.push('…');
        }
        parts.push(format!("message: {line}"));
    }
    Some(parts.join(" · "))
}

pub(crate) fn probes(c: &Container) -> Vec<(String, String)> {
    [
        ("Startup", c.startup_probe.as_ref()),
        ("Liveness", c.liveness_probe.as_ref()),
        ("Readiness", c.readiness_probe.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, probe)| Some((name.to_owned(), probe_text(probe?))))
    .collect()
}

/// Environment variables: values, or where they come from.
fn env(c: &Container) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = c
        .env_from
        .iter()
        .flatten()
        .filter_map(|e| {
            let prefix = e.prefix.as_deref().unwrap_or_default();
            let source = if let Some(s) = &e.config_map_ref {
                format!("all keys of ConfigMap {}", s.name)
            } else {
                format!("all keys of Secret {}", e.secret_ref.as_ref()?.name)
            };
            Some((format!("{prefix}*"), source))
        })
        .collect();
    out.extend(c.env.iter().flatten().map(|e| {
        let value = match (&e.value, &e.value_from) {
            (Some(v), _) => v.clone(),
            (None, Some(from)) => {
                if let Some(s) = &from.config_map_key_ref {
                    format!("ConfigMap {} · {}", s.name, s.key)
                } else if let Some(s) = &from.secret_key_ref {
                    format!("Secret {} · {}", s.name, s.key)
                } else if let Some(f) = &from.field_ref {
                    format!("field {}", f.field_path)
                } else if let Some(r) = &from.resource_field_ref {
                    format!("resource {}", r.resource)
                } else {
                    "<from source>".into()
                }
            }
            (None, None) => String::new(),
        };
        (e.name.clone(), value)
    }));
    out
}

fn mounts(c: &Container) -> Vec<(String, String)> {
    c.volume_mounts
        .iter()
        .flatten()
        .map(|m| {
            let ro = if m.read_only == Some(true) {
                " (ro)"
            } else {
                ""
            };
            (m.name.clone(), format!("{}{ro}", m.mount_path))
        })
        .collect()
}

/// The kubelet's readiness, except for plain init containers: they are
/// never ready, so a running one is just running.
fn readiness(cs: Option<&ContainerStatus>, role: ContainerRole) -> Option<bool> {
    cs.filter(|_| role != ContainerRole::Init)
        .map(|cs| cs.ready)
}

fn container_info(
    c: &Container,
    cs: Option<&ContainerStatus>,
    role: ContainerRole,
) -> ContainerInfo {
    let last_state = cs
        .and_then(|cs| cs.last_state.as_ref())
        .and_then(last_state_text);
    ContainerInfo {
        name: c.name.clone(),
        role,
        image: c.image.clone().unwrap_or_default(),
        phase: phase(cs.and_then(|cs| cs.state.as_ref())),
        last_state,
        ready: readiness(cs, role),
        restarts: cs.map_or(0, |cs| cs.restart_count),
        ports: ports(c.ports.as_ref()),
        probes: probes(c),
        requests: or_none(resources_text(c.resources.as_ref(), false)),
        limits: or_none(resources_text(c.resources.as_ref(), true)),
        env: env(c),
        mounts: mounts(c),
    }
}

/// Init (and sidecar) containers, then app containers, with their status.
pub(crate) fn containers(pod: &Pod) -> (Vec<ContainerInfo>, Vec<ContainerInfo>) {
    let Some(spec) = &pod.spec else {
        return Default::default();
    };
    let status = pod.status.as_ref();
    fn find<'a>(list: Option<&'a Vec<ContainerStatus>>, name: &str) -> Option<&'a ContainerStatus> {
        list.into_iter().flatten().find(|cs| cs.name == name)
    }
    let inits = spec
        .init_containers
        .iter()
        .flatten()
        .map(|c| {
            let role = if is_sidecar(Some(c)) {
                ContainerRole::Sidecar
            } else {
                ContainerRole::Init
            };
            let cs = find(
                status.and_then(|s| s.init_container_statuses.as_ref()),
                &c.name,
            );
            container_info(c, cs, role)
        })
        .collect();
    let apps = spec
        .containers
        .iter()
        .map(|c| {
            let cs = find(status.and_then(|s| s.container_statuses.as_ref()), &c.name);
            container_info(c, cs, ContainerRole::App)
        })
        .collect();
    (inits, apps)
}

/// Status cubes of every container from the spec, init (and sidecar)
/// containers first; those without a status yet are [`ContainerHealth::Unknown`].
/// Cheaper than [`containers`], which a table cell does not need.
///
/// [`ContainerHealth::Unknown`]: crate::plugin::ContainerHealth::Unknown
pub(crate) fn container_briefs(pod: &Pod) -> Vec<ContainerBrief> {
    let Some(spec) = &pod.spec else {
        return Vec::new();
    };
    let status = pod.status.as_ref();
    let brief = |c: &Container, statuses: Option<&Vec<ContainerStatus>>, role| {
        let cs = statuses.into_iter().flatten().find(|cs| cs.name == c.name);
        ContainerBrief::new(
            c.name.clone(),
            role,
            &phase(cs.and_then(|cs| cs.state.as_ref())),
            readiness(cs, role),
            cs.map_or(0, |cs| cs.restart_count),
        )
    };
    let init_statuses = status.and_then(|s| s.init_container_statuses.as_ref());
    let statuses = status.and_then(|s| s.container_statuses.as_ref());
    let inits = spec.init_containers.iter().flatten().map(|c| {
        let role = if is_sidecar(Some(c)) {
            ContainerRole::Sidecar
        } else {
            ContainerRole::Init
        };
        brief(c, init_statuses, role)
    });
    let apps = spec
        .containers
        .iter()
        .map(|c| brief(c, statuses, ContainerRole::App));
    inits.chain(apps).collect()
}

/// A volume, linked to its ConfigMap, Secret or claim.
fn volume_field(v: &Volume, namespace: Option<&str>) -> Field {
    let link = |kind: &str, name: &str| {
        Field::new(
            v.name.clone(),
            FieldValue::Link {
                phrase: None,
                label: format!("{kind} {name}"),
                target: ObjectRef {
                    api_version: "v1".into(),
                    kind: kind.into(),
                    name: name.into(),
                    namespace: namespace.map(str::to_owned),
                },
            },
        )
    };
    if let Some(s) = &v.config_map {
        return link("ConfigMap", &s.name);
    }
    if let Some(name) = v.secret.as_ref().and_then(|s| s.secret_name.as_deref()) {
        return link("Secret", name);
    }
    if let Some(s) = &v.persistent_volume_claim {
        return link("PersistentVolumeClaim", &s.claim_name);
    }
    Field::text(v.name.clone(), volume_text(v))
}

/// Node selector, tolerations and affinity of a pod spec.
pub(crate) fn scheduling(spec: &PodSpec) -> Vec<Field> {
    let mut out = Vec::new();
    if let Some(selector) = spec.node_selector.as_ref().filter(|s| !s.is_empty()) {
        out.push(Field::new(
            "Node selector",
            FieldValue::Labels(
                selector
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        ));
    }
    let tolerations: Vec<String> = spec
        .tolerations
        .iter()
        .flatten()
        .map(|t| {
            let key = t.key.as_deref().unwrap_or("*");
            let mut text = match (t.operator.as_deref(), t.value.as_deref()) {
                (Some("Exists"), _) => format!("{key} exists"),
                (_, Some(value)) => format!("{key}={value}"),
                _ => key.to_owned(),
            };
            if let Some(effect) = &t.effect {
                text.push_str(&format!(" :{effect}"));
            }
            if let Some(seconds) = t.toleration_seconds {
                text.push_str(&format!(" for {seconds}s"));
            }
            text
        })
        .collect();
    if !tolerations.is_empty() {
        out.push(Field::text("Tolerations", tolerations.join("\n")));
    }
    if let Some(affinity) = &spec.affinity
        && let Ok(text) = serde_json::to_string(affinity)
        && text != "{}"
    {
        out.push(Field::text("Affinity", text));
    }
    out
}

fn volume_text(v: &Volume) -> String {
    if let Some(s) = &v.config_map {
        return format!("ConfigMap {}", s.name);
    }
    if let Some(s) = &v.secret {
        return format!("Secret {}", s.secret_name.clone().unwrap_or_default());
    }
    if let Some(s) = &v.persistent_volume_claim {
        return format!("PersistentVolumeClaim {}", s.claim_name);
    }
    if v.empty_dir.is_some() {
        return "EmptyDir".into();
    }
    if let Some(s) = &v.host_path {
        return format!("HostPath {}", s.path);
    }
    if v.projected.is_some() {
        return "Projected".into();
    }
    if v.downward_api.is_some() {
        return "DownwardAPI".into();
    }
    if v.csi.is_some() {
        return "CSI".into();
    }
    if v.ephemeral.is_some() {
        return "Ephemeral".into();
    }
    "Other".into()
}

fn core_ref(kind: &str, name: &str, namespace: Option<&String>) -> Relation {
    Relation {
        kind: RelationKind::References,
        target: ObjectRef {
            api_version: "v1".into(),
            kind: kind.into(),
            name: name.into(),
            namespace: namespace.cloned(),
        },
    }
}

/// Node, service account, ConfigMaps, Secrets and PVCs referenced by a pod.
fn references(pod: &Pod) -> Vec<Relation> {
    let ns = pod.metadata.namespace.as_ref();
    let mut out = Vec::new();
    let Some(spec) = &pod.spec else {
        return out;
    };
    if let Some(node) = &spec.node_name {
        out.push(core_ref("Node", node, None));
    }
    if let Some(sa) = &spec.service_account_name {
        out.push(core_ref("ServiceAccount", sa, ns));
    }
    let mut add = |kind: &str, name: &str| {
        let r = core_ref(kind, name, ns);
        if !name.is_empty() && !out.contains(&r) {
            out.push(r);
        }
    };
    for v in spec.volumes.iter().flatten() {
        if let Some(s) = &v.config_map {
            add("ConfigMap", &s.name);
        }
        if let Some(s) = &v.secret {
            add("Secret", s.secret_name.as_deref().unwrap_or_default());
        }
        if let Some(s) = &v.persistent_volume_claim {
            add("PersistentVolumeClaim", &s.claim_name);
        }
        for src in v.projected.iter().flat_map(|p| p.sources.iter().flatten()) {
            if let Some(s) = &src.config_map {
                add("ConfigMap", &s.name);
            }
            if let Some(s) = &src.secret {
                add("Secret", &s.name);
            }
        }
    }
    let containers = spec
        .init_containers
        .iter()
        .flatten()
        .chain(&spec.containers);
    for c in containers {
        for e in c.env_from.iter().flatten() {
            if let Some(s) = &e.config_map_ref {
                add("ConfigMap", &s.name);
            }
            if let Some(s) = &e.secret_ref {
                add("Secret", &s.name);
            }
        }
        for e in c.env.iter().flatten() {
            let Some(from) = &e.value_from else { continue };
            if let Some(s) = &from.config_map_key_ref {
                add("ConfigMap", &s.name);
            }
            if let Some(s) = &from.secret_key_ref {
                add("Secret", &s.name);
            }
        }
    }
    for s in spec.image_pull_secrets.iter().flatten() {
        add("Secret", &s.name);
    }
    out
}

fn diagnostics_notes(pod: &Pod, now: Timestamp) -> Vec<Note> {
    diagnostics::diagnose_at(pod, &[], now)
        .iter()
        .map(diagnostics::Finding::note)
        .collect()
}

/// Security findings as notes (`High: title` → hint).
pub(crate) fn security_notes(findings: Vec<security::Finding>) -> Vec<Note> {
    findings.iter().map(security::Finding::note).collect()
}

impl PodPlugin {
    fn cells_at(&self, obj: &DynamicObject, now: Timestamp) -> Vec<Cell> {
        let Some(pod) = decode::<Pod>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let s = summarize(&pod);
        let health = pod_health(&pod, now);
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Containers(container_briefs(&pod)),
            Cell::Status(Health::new(health.status, s.status.clone())),
            restarts_cell(&s),
            Cell::Age(creation(obj)),
            text(or_none(
                pod.spec
                    .as_ref()
                    .and_then(|s| s.node_name.clone())
                    .unwrap_or_default(),
            )),
            text(or_none(
                pod.status
                    .as_ref()
                    .and_then(|s| s.pod_ip.clone())
                    .unwrap_or_default(),
            )),
        ]);
        cells
    }

    fn details_at(&self, obj: &DynamicObject, now: Timestamp) -> Vec<DetailSection> {
        let Some(pod) = decode::<Pod>(obj) else {
            return GenericPlugin.details(obj);
        };
        let summary = summarize(&pod);
        let spec = pod.spec.as_ref();
        let status = pod.status.as_ref();
        let ns = pod.metadata.namespace.as_deref();
        let opt = |v: Option<&String>| or_none(v.cloned().unwrap_or_default());
        let mut overview = vec![
            Field::text("Status", summary.status.clone()),
            Field::text("Phase", opt(status.and_then(|s| s.phase.as_ref()))),
        ];
        if let Some(reason) = status
            .and_then(|s| s.message.clone().or_else(|| s.reason.clone()))
            .filter(|r| !r.is_empty())
        {
            overview.push(Field::text("Reason", reason));
        }
        overview.push(match spec.and_then(|s| s.node_name.as_deref()) {
            Some(node) => Field::link("Node", "Node", node, None),
            None => Field::text("Node", "<none>"),
        });
        let ips: Vec<String> = status
            .and_then(|s| s.pod_ips.as_ref())
            .into_iter()
            .flatten()
            .map(|ip| ip.ip.clone())
            .collect();
        overview.push(if ips.len() > 1 {
            Field::text("Pod IPs", ips.join(", "))
        } else {
            Field::text("Pod IP", opt(status.and_then(|s| s.pod_ip.as_ref())))
        });
        overview.push(Field::text(
            "Host IP",
            opt(status.and_then(|s| s.host_ip.as_ref())),
        ));
        overview.push(Field::text(
            "QoS class",
            opt(status.and_then(|s| s.qos_class.as_ref())),
        ));
        if let Some(sa) = spec.and_then(|s| s.service_account_name.as_deref()) {
            overview.push(Field::link("Service account", "ServiceAccount", sa, ns));
        }
        let priority = match (
            spec.and_then(|s| s.priority_class_name.as_deref()),
            spec.and_then(|s| s.priority),
        ) {
            (Some(class), Some(value)) => format!("{class} ({value})"),
            (Some(class), None) => class.to_owned(),
            (None, Some(value)) => value.to_string(),
            (None, None) => "<none>".into(),
        };
        overview.push(Field::text("Priority", priority));
        overview.push(Field::text(
            "Restart policy",
            opt(spec.and_then(|s| s.restart_policy.as_ref())),
        ));
        if let Some(started) = status.and_then(|s| s.start_time.as_ref()) {
            overview.push(Field::new("Started", FieldValue::Time(started.0)));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(overview))];

        let (inits, apps) = containers(&pod);
        if !inits.is_empty() {
            sections.push(section("Init containers", SectionBody::Containers(inits)));
        }
        if !apps.is_empty() {
            sections.push(section("Containers", SectionBody::Containers(apps)));
        }
        push_notes(&mut sections, "Diagnostics", diagnostics_notes(&pod, now));
        push_notes(
            &mut sections,
            "Security",
            security_notes(security::check_pod(&pod)),
        );
        let volumes: Vec<Field> = spec
            .and_then(|s| s.volumes.as_ref())
            .into_iter()
            .flatten()
            .map(|v| volume_field(v, ns))
            .collect();
        if !volumes.is_empty() {
            sections.push(section("Volumes", SectionBody::Fields(volumes)));
        }
        if let Some(spec) = spec {
            let scheduling = scheduling(spec);
            if !scheduling.is_empty() {
                sections.push(section("Scheduling", SectionBody::Fields(scheduling)).collapsed());
            }
        }
        // Labels, annotations, owners and conditions from the generic plugin.
        sections.extend(GenericPlugin.details(obj));
        sections
    }
}

impl ResourcePlugin for PodPlugin {
    fn name(&self) -> &'static str {
        "pod"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("", "v1", "Pod")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            super::col("containers", "Containers", ColumnKind::Containers),
            super::col("status", "Status", ColumnKind::Status),
            super::col("restarts", "Restarts", ColumnKind::Text),
            super::col("age", "Age", ColumnKind::Age),
            super::col("node", "Node", ColumnKind::Text),
            super::col("ip", "IP", ColumnKind::Text),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        self.cells_at(obj, Timestamp::now())
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        self.details_at(obj, Timestamp::now())
    }

    fn actions(&self) -> Vec<Action> {
        vec![
            Action::stream(action::LOGS, "Logs", StreamKind::Logs),
            Action::stream(action::EXEC, "Exec", StreamKind::Exec),
            Action::stream(action::DEBUG, "Debug", StreamKind::Debug),
            PORT_FORWARD_ACTION,
            Action::api(action::EVICT, "Evict", true),
            DELETE_ACTION,
        ]
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(async move {
            match *action_id {
                action::DELETE => delete::<Pod>(client, obj).await,
                action::EVICT => evict(client, obj).await,
                // Logs, exec, debug and port-forward are streams the
                // caller opens: `unknown` refuses them.
                _ => Err(unknown(action_id)),
            }
        })
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        match decode::<Pod>(obj) {
            Some(pod) => relations_with(obj, references(&pod)),
            None => GenericPlugin.relations(obj),
        }
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<Pod>(obj) {
            Some(pod) => pod_health(&pod, Timestamp::now()),
            None => GenericPlugin.health(obj),
        }
    }

    fn brief(&self, obj: &DynamicObject) -> Brief {
        let Some(pod) = decode::<Pod>(obj) else {
            return Brief {
                health: GenericPlugin.health(obj),
                note: String::new(),
                phrase: None,
                containers: Vec::new(),
            };
        };
        let summary = summarize(&pod);
        let health = pod_health(&pod, Timestamp::now());
        let note = match pod.spec.as_ref().and_then(|s| s.node_name.as_deref()) {
            Some(node) => msg!(
                "brief-pod-on-node",
                ready = summary.ready,
                total = summary.total,
                restarts = summary.restarts,
                node = node
            ),
            None => msg!(
                "brief-pod",
                ready = summary.ready,
                total = summary.total,
                restarts = summary.restarts
            ),
        };
        let containers = container_briefs(&pod)
            .into_iter()
            .filter(|c| c.role != ContainerRole::Init)
            .collect();
        Brief::phrased(Health::new(health.status, summary.status), note, containers)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::plugin::builtin::test_util::{obj, section, ts};
    use crate::plugin::{ContainerHealth, Placement};

    const NOW: &str = "2026-10-22T12:00:00Z";

    fn pod(status: Value) -> DynamicObject {
        obj(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {
                "name": "web-1", "namespace": "default",
                "creationTimestamp": "2026-10-22T10:00:00Z",
                "labels": {"app": "web"},
                "ownerReferences": [{"apiVersion": "apps/v1", "kind": "ReplicaSet", "name": "web-abc", "uid": "u"}]
            },
            "spec": {
                "nodeName": "node-1",
                "serviceAccountName": "web",
                "containers": [{
                    "name": "app", "image": "nginx:1.27",
                    "ports": [{"name": "http", "containerPort": 80}],
                    "resources": {"requests": {"cpu": "100m"}, "limits": {"memory": "128Mi"}},
                    "envFrom": [{"configMapRef": {"name": "web-config"}}],
                    "env": [{"name": "PW", "valueFrom": {"secretKeyRef": {"name": "db", "key": "pw"}}}]
                }],
                "volumes": [
                    {"name": "data", "persistentVolumeClaim": {"claimName": "web-data"}},
                    {"name": "certs", "secret": {"secretName": "tls"}}
                ]
            },
            "status": status
        }))
    }

    fn running() -> Value {
        json!({
            "phase": "Running", "podIP": "10.0.0.5",
            "conditions": [
                {"type": "Initialized", "status": "True"},
                {"type": "Ready", "status": "True"}
            ],
            "containerStatuses": [{
                "name": "app", "image": "nginx:1.27", "imageID": "", "ready": true,
                "restartCount": 2,
                "state": {"running": {"startedAt": "2026-10-22T11:50:00Z"}},
                "lastState": {"terminated": {"exitCode": 1, "reason": "Error", "finishedAt": "2026-10-22T11:55:00Z"}}
            }]
        })
    }

    fn crash_loop() -> Value {
        json!({
            "phase": "Running",
            "conditions": [{"type": "Ready", "status": "False"}],
            "containerStatuses": [{
                "name": "app", "image": "nginx:1.27", "imageID": "", "ready": false,
                "restartCount": 7,
                "state": {"waiting": {"reason": "CrashLoopBackOff"}},
                "lastState": {"terminated": {"exitCode": 1, "reason": "Error", "finishedAt": "2026-10-22T11:58:00Z"}}
            }]
        })
    }

    #[test]
    fn running_pod_cells() {
        let p = PodPlugin;
        let cells = p.cells_at(&pod(running()), ts(NOW));
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[0], text("web-1"));
        assert_eq!(
            cells[2],
            Cell::Containers(vec![ContainerBrief {
                name: "app".into(),
                role: ContainerRole::App,
                health: ContainerHealth::Ready,
                state: "Running".into(),
                phase: ContainerPhase::Running {
                    since: Some(ts("2026-10-22T11:50:00Z")),
                },
                restarts: 2,
            }])
        );
        assert_eq!(
            cells[3],
            Cell::Status(Health::new(HealthStatus::Healthy, "Running"))
        );
        assert_eq!(cells[4], Cell::CountAge(2, ts("2026-10-22T11:55:00Z")));
        assert_eq!(cells[6], text("node-1"));
        assert_eq!(cells[7], text("10.0.0.5"));
    }

    #[test]
    fn crash_loop_status_and_health() {
        let p = PodPlugin;
        let cells = p.cells_at(&pod(crash_loop()), ts(NOW));
        let Cell::Containers(cubes) = &cells[2] else {
            panic!("{:?}", cells[2])
        };
        assert_eq!(cubes[0].health, ContainerHealth::Failing);
        assert_eq!(cubes[0].state, "Waiting: CrashLoopBackOff");
        assert_eq!(cubes[0].restarts, 7);
        assert_eq!(
            cells[3],
            Cell::Status(Health::new(HealthStatus::Failed, "CrashLoopBackOff"))
        );
        assert_eq!(cells[4], Cell::CountAge(7, ts("2026-10-22T11:58:00Z")));
        assert_eq!(cells[7], text("<none>"));
        let h = pod_health(&decode(&pod(crash_loop())).unwrap(), ts(NOW));
        assert_eq!(h.status, HealthStatus::Failed);
        assert!(h.message.contains("crash looping"), "{}", h.message);
    }

    #[test]
    fn completed_and_terminating() {
        let completed = json!({
            "phase": "Succeeded",
            "containerStatuses": [{
                "name": "app", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
                "state": {"terminated": {"exitCode": 0, "reason": "Completed"}}
            }]
        });
        let p: Pod = decode(&pod(completed)).unwrap();
        assert_eq!(summarize(&p).status, "Completed");
        assert_eq!(pod_health(&p, ts(NOW)).status, HealthStatus::Healthy);

        let mut o = pod(running());
        o.metadata.deletion_timestamp = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
            ts("2026-10-22T12:00:30Z"),
        ));
        let p: Pod = decode(&o).unwrap();
        assert_eq!(summarize(&p).status, "Terminating");
        assert_eq!(pod_health(&p, ts(NOW)).status, HealthStatus::Progressing);
    }

    #[test]
    fn init_container_status() {
        let mut o = pod(json!({
            "phase": "Pending",
            "initContainerStatuses": [{
                "name": "init", "image": "busybox", "imageID": "", "ready": false, "restartCount": 0,
                "state": {"running": {}}
            }],
            "containerStatuses": [{
                "name": "app", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
                "state": {"waiting": {"reason": "PodInitializing"}}
            }]
        }));
        o.data["spec"]["initContainers"] = json!([{"name": "init", "image": "busybox"}]);
        let p: Pod = decode(&o).unwrap();
        assert_eq!(summarize(&p).status, "Init:0/1");
        // A running init container is never ready, and that is fine.
        let cubes = container_briefs(&p);
        assert_eq!(cubes[0].health, ContainerHealth::Ready);
        assert_eq!(cubes[0].state, "Running");
        assert_eq!(cubes[1].health, ContainerHealth::Starting);
    }

    #[test]
    fn container_cubes_cover_spec_init_first() {
        let mut o = pod(json!({
            "phase": "Pending",
            "initContainerStatuses": [
                {
                    "name": "migrate", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
                    "state": {"terminated": {"exitCode": 0, "reason": "Completed"}}
                },
                {
                    "name": "proxy", "image": "x", "imageID": "", "ready": false, "restartCount": 1,
                    "state": {"running": {}}
                }
            ],
            "containerStatuses": [{
                "name": "app", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
                "state": {"terminated": {"exitCode": 2, "reason": "Error"}}
            }]
        }));
        o.data["spec"]["initContainers"] = json!([
            {"name": "migrate", "image": "x"},
            {"name": "proxy", "image": "x", "restartPolicy": "Always"}
        ]);
        // Declared but without a status yet.
        o.data["spec"]["containers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "worker", "image": "x"}));
        let p: Pod = decode(&o).unwrap();
        let cubes = container_briefs(&p);
        let summary: Vec<_> = cubes
            .iter()
            .map(|c| (c.name.as_str(), c.role, c.health))
            .collect();
        assert_eq!(
            summary,
            [
                ("migrate", ContainerRole::Init, ContainerHealth::Completed),
                ("proxy", ContainerRole::Sidecar, ContainerHealth::NotReady),
                ("app", ContainerRole::App, ContainerHealth::Failed),
                ("worker", ContainerRole::App, ContainerHealth::Unknown),
            ]
        );
        assert_eq!(cubes[1].restarts, 1);
        assert_eq!(cubes[2].state, "Terminated: Error (exit code 2)");
        assert_eq!(crate::plugin::not_ready_count(&cubes), 3);
        // Related lists skip plain init containers.
        let brief = PodPlugin.brief(&o);
        let names: Vec<_> = brief.containers.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["proxy", "app", "worker"]);
    }

    #[test]
    fn exit_code_without_reason() {
        let p: Pod = decode(&pod(json!({
            "phase": "Running",
            "containerStatuses": [{
                "name": "app", "image": "x", "imageID": "", "ready": false, "restartCount": 0,
                "state": {"terminated": {"exitCode": 3}}
            }]
        })))
        .unwrap();
        assert_eq!(summarize(&p).status, "ExitCode:3");
    }

    #[test]
    fn details_have_containers_diagnostics_security() {
        let mut o = pod(crash_loop());
        o.data["spec"]["tolerations"] =
            json!([{"key": "gpu", "operator": "Exists", "effect": "NoSchedule"}]);
        o.data["spec"]["containers"][0]["readinessProbe"] =
            json!({"httpGet": {"path": "/ready", "port": 80}, "periodSeconds": 5});
        let sections = PodPlugin.details_at(&o, ts(NOW));
        assert_eq!(sections[0].placement, Placement::Overview);
        let SectionBody::Fields(overview) = &sections[0].body else {
            panic!()
        };
        assert!(overview.contains(&Field::link("Node", "Node", "node-1", None)));
        assert!(overview.contains(&Field::link(
            "Service account",
            "ServiceAccount",
            "web",
            Some("default")
        )));
        let SectionBody::Containers(c) = section(&sections, "Containers") else {
            panic!()
        };
        assert_eq!(c[0].image, "nginx:1.27");
        assert_eq!(c[0].ports, ["http 80/TCP"]);
        assert_eq!(c[0].requests, "cpu=100m");
        assert_eq!(c[0].limits, "memory=128Mi");
        assert_eq!(c[0].state_text(), "Waiting: CrashLoopBackOff");
        assert_eq!(c[0].health().status, HealthStatus::Failed);
        assert_eq!(c[0].restarts, 7);
        assert_eq!(
            c[0].probes,
            [(
                "Readiness".to_owned(),
                "http-get :80/ready · delay 0s · timeout 1s · every 5s · failure 3 · success 1"
                    .to_owned()
            )]
        );
        assert!(
            c[0].env
                .contains(&("*".into(), "all keys of ConfigMap web-config".into()))
        );
        assert!(c[0].env.contains(&("PW".into(), "Secret db · pw".into())));
        let SectionBody::Notes(d) = section(&sections, "Diagnostics") else {
            panic!()
        };
        assert!(d[0].key_in(kubach_i18n::Locale::En).starts_with("Error:"));
        let SectionBody::Notes(s) = section(&sections, "Security") else {
            panic!()
        };
        assert!(!s.is_empty());
        let SectionBody::Fields(volumes) = section(&sections, "Volumes") else {
            panic!()
        };
        assert_eq!(volumes[0].value.text(), "PersistentVolumeClaim web-data");
        let scheduling = sections.iter().find(|s| s.title == "Scheduling").unwrap();
        assert!(scheduling.collapsed);
        assert_eq!(
            scheduling.text_lines(),
            ["Tolerations: gpu exists :NoSchedule"]
        );
        let labels = sections.iter().find(|s| s.title == "Labels").unwrap();
        assert_eq!(labels.placement, Placement::Metadata);
    }

    #[test]
    fn probe_text_spells_out_defaults() {
        let probe = |v: Value| probe_text(&serde_json::from_value(v).unwrap());
        assert_eq!(
            probe(json!({"httpGet": {"path": "/healthz", "port": 8080}})),
            "http-get :8080/healthz · delay 0s · timeout 1s · every 10s · failure 3 · success 1"
        );
        assert_eq!(
            probe(json!({
                "httpGet": {"host": "10.0.0.1", "port": 8443, "scheme": "HTTPS"},
                "initialDelaySeconds": 15, "timeoutSeconds": 5, "periodSeconds": 20,
                "failureThreshold": 6, "successThreshold": 2,
                "terminationGracePeriodSeconds": 30
            })),
            "http-get 10.0.0.1:8443/ (HTTPS) · delay 15s · timeout 5s · every 20s · failure 6 · \
             success 2 · grace 30s"
        );
        assert_eq!(
            probe(json!({"tcpSocket": {"port": "db"}})),
            "tcp :db · delay 0s · timeout 1s · every 10s · failure 3 · success 1"
        );
        assert_eq!(
            probe(json!({"grpc": {"port": 9090, "service": "health"}})),
            "grpc :9090 service health · delay 0s · timeout 1s · every 10s · failure 3 · success 1"
        );
        assert_eq!(
            probe(json!({"exec": {"command": ["sh", "-c", "pg_isready"]}})),
            "exec sh -c pg_isready · delay 0s · timeout 1s · every 10s · failure 3 · success 1"
        );
    }

    /// A pod whose liveness probe (1s timeout, no delay) keeps killing its
    /// app container, behind a completed init container and a sidecar
    /// that was OOM-killed.
    fn probe_loop() -> DynamicObject {
        obj(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "api-0", "namespace": "default"},
            "spec": {
                "initContainers": [
                    {"name": "migrate", "image": "migrate:1"},
                    {"name": "proxy", "image": "envoy:1", "restartPolicy": "Always",
                     "readinessProbe": {"tcpSocket": {"port": 15000}}}
                ],
                "containers": [{
                    "name": "api", "image": "api:2",
                    "livenessProbe": {"httpGet": {"path": "/health", "port": 8080}, "timeoutSeconds": 1},
                    "startupProbe": {"grpc": {"port": 9090}, "failureThreshold": 30, "periodSeconds": 2}
                }]
            },
            "status": {
                "phase": "Running",
                "initContainerStatuses": [
                    {"name": "migrate", "image": "migrate:1", "imageID": "", "ready": true,
                     "restartCount": 0,
                     "state": {"terminated": {"exitCode": 0, "reason": "Completed",
                               "startedAt": "2026-10-22T11:00:00Z", "finishedAt": "2026-10-22T11:00:05Z"}}},
                    {"name": "proxy", "image": "envoy:1", "imageID": "", "ready": true,
                     "restartCount": 1,
                     "state": {"running": {"startedAt": "2026-10-22T11:30:00Z"}},
                     "lastState": {"terminated": {"exitCode": 137, "reason": "OOMKilled",
                                   "startedAt": "2026-10-22T11:00:06Z", "finishedAt": "2026-10-22T11:29:59Z"}}}
                ],
                "containerStatuses": [{
                    "name": "api", "image": "api:2", "imageID": "", "ready": false,
                    "restartCount": 12,
                    "state": {"waiting": {"reason": "CrashLoopBackOff", "message": "back-off 5m0s"}},
                    "lastState": {"terminated": {
                        "exitCode": 143, "signal": 15, "reason": "Error",
                        "message": "shutting down\nbye",
                        "startedAt": "2026-10-22T11:58:00Z", "finishedAt": "2026-10-22T11:58:31Z"
                    }}
                }]
            }
        }))
    }

    #[test]
    fn containers_show_probes_and_last_state() {
        let sections = PodPlugin.details_at(&probe_loop(), ts(NOW));
        let SectionBody::Containers(inits) = section(&sections, "Init containers") else {
            panic!()
        };
        assert_eq!(inits[0].last_state, None);
        assert!(inits[0].probes.is_empty());
        assert_eq!(
            inits[1].last_state.as_deref(),
            Some(
                "Terminated: OOMKilled (exit code 137) · started 2026-10-22T11:00:06Z · \
                 finished 2026-10-22T11:29:59Z"
            )
        );
        let SectionBody::Containers(apps) = section(&sections, "Containers") else {
            panic!()
        };
        assert_eq!(
            apps[0].last_state.as_deref(),
            Some(
                "Terminated: Error (exit code 143, signal 15) · started 2026-10-22T11:58:00Z · \
                 finished 2026-10-22T11:58:31Z · message: shutting down…"
            )
        );
        let names: Vec<_> = apps[0].probes.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Startup", "Liveness"]);
    }

    #[test]
    fn container_text_lines_for_agents() {
        let sections = PodPlugin.details_at(&probe_loop(), ts(NOW));
        let inits = sections
            .iter()
            .find(|s| s.title == "Init containers")
            .unwrap();
        assert_eq!(
            inits.text_lines(),
            [
                "migrate (Init): migrate:1",
                "  State: Terminated: Completed (exit code 0)",
                "  Restarts: 0",
                "proxy (Sidecar): envoy:1",
                "  State: Running since 2026-10-22T11:30:00Z",
                "  Ready: true",
                "  Restarts: 1",
                "  Last state: Terminated: OOMKilled (exit code 137) · started 2026-10-22T11:00:06Z · \
                 finished 2026-10-22T11:29:59Z",
                "  Readiness probe: tcp :15000 · delay 0s · timeout 1s · every 10s · failure 3 · \
                 success 1",
            ]
        );
        let apps = sections.iter().find(|s| s.title == "Containers").unwrap();
        assert_eq!(
            apps.text_lines(),
            [
                "api (App): api:2",
                "  State: Waiting: CrashLoopBackOff (back-off 5m0s)",
                "  Ready: false",
                "  Restarts: 12",
                "  Last state: Terminated: Error (exit code 143, signal 15) · \
                 started 2026-10-22T11:58:00Z · finished 2026-10-22T11:58:31Z · \
                 message: shutting down…",
                "  Startup probe: grpc :9090 · delay 0s · timeout 1s · every 2s · failure 30 · \
                 success 1",
                "  Liveness probe: http-get :8080/health · delay 0s · timeout 1s · every 10s · \
                 failure 3 · success 1",
            ]
        );
        let plain = PodPlugin.details_at(&pod(running()), ts(NOW));
        let apps = plain.iter().find(|s| s.title == "Containers").unwrap();
        assert!(apps.text_lines().contains(&"  Probes: none".to_owned()));
    }

    #[test]
    fn brief_has_container_statuses() {
        let brief = PodPlugin.brief(&pod(running()));
        assert_eq!(brief.health.status, HealthStatus::Healthy);
        assert_eq!(brief.note, "1/1 ready · 2 restarts · node-1");
        assert_eq!(brief.containers.len(), 1);
        assert_eq!(brief.containers[0].health, ContainerHealth::Ready);
    }

    #[test]
    fn relations_cover_owner_node_sa_and_refs() {
        let rels = PodPlugin.relations(&pod(running()));
        let names: Vec<_> = rels
            .iter()
            .map(|r| format!("{}/{}", r.target.kind, r.target.name))
            .collect();
        assert_eq!(
            names,
            [
                "ReplicaSet/web-abc",
                "Node/node-1",
                "ServiceAccount/web",
                "PersistentVolumeClaim/web-data",
                "Secret/tls",
                "ConfigMap/web-config",
                "Secret/db",
            ]
        );
        assert_eq!(rels[0].kind, RelationKind::OwnedBy);
        assert_eq!(rels[1].target.namespace, None);
    }

    #[test]
    fn undecodable_pod_falls_back() {
        let o = obj(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "bad", "namespace": "ns"},
            "spec": {"containers": "not-a-list"}
        }));
        let p = PodPlugin;
        let cells = p.cells(&o);
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(cells[0], text("bad"));
        assert_eq!(cells[2], text(""));
        assert_eq!(p.health(&o), Health::unknown());
    }

    #[test]
    fn declares_streaming_actions() {
        let actions = PodPlugin.actions();
        let ids: Vec<_> = actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            ["logs", "exec", "debug", "port-forward", "evict", "delete"]
        );
        let streams: Vec<_> = actions.iter().filter_map(Action::stream_kind).collect();
        assert_eq!(
            streams,
            [
                StreamKind::Logs,
                StreamKind::Exec,
                StreamKind::Debug,
                StreamKind::PortForward
            ]
        );
        for api in &actions[4..] {
            assert!(api.dangerous);
            assert_eq!(api.kind, crate::plugin::ActionKind::Api);
        }
    }
}
