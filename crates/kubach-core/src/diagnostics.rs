//! Explains why a pod is unhealthy: pure functions over `Pod` (and its
//! events) that produce data-only findings for the UI and MCP.

use k8s_openapi::api::core::v1::{
    Container, ContainerStateTerminated, ContainerStatus, Event, Pod, PodCondition,
};
use k8s_openapi::jiff::Timestamp;
use kubach_i18n::{Locale, Message, msg};

use crate::plugin::Note;

/// How bad a finding is. Ordered from least to most severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// What kind of problem a finding describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindingKind {
    CrashLoopBackOff,
    /// `ImagePullBackOff`, `ErrImagePull`, `InvalidImageName` and friends.
    ImagePull,
    OomKilled,
    /// `PodScheduled=False`; causes are parsed from the scheduler message and
    /// may be empty when it is not recognized.
    Unschedulable(Vec<SchedulingCause>),
    /// Scheduling is held back by `spec.schedulingGates`.
    SchedulingGated,
    /// `CreateContainerConfigError`, usually a missing ConfigMap or Secret.
    ContainerConfig(Option<MissingRef>),
    InitContainerFailed,
    /// A container exited with a non-zero code and is not restarted.
    ContainerFailed,
    /// A container is waiting for a reason not covered by other kinds.
    ContainerWaiting,
    /// A running container restarted before, without crash looping now.
    Restarted,
    /// `Ready=False` while the pod is running.
    NotReady,
    Terminating,
    StuckTerminating,
    Evicted,
    PodFailed,
    /// A volume cannot be attached or mounted (from events).
    VolumeMountFailed,
    /// The pod sandbox (network, runtime) cannot be created (from events).
    SandboxFailed,
}

/// Why the scheduler could not place a pod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchedulingCause {
    InsufficientCpu,
    InsufficientMemory,
    /// Any other resource, e.g. `nvidia.com/gpu` or `ephemeral-storage`.
    InsufficientResource(String),
    UntoleratedTaint,
    /// `nodeSelector` or node affinity matches no node.
    NodeSelectorMismatch,
    PodAffinityMismatch,
    UnboundPvc,
    VolumeNodeAffinityConflict,
    NoFreePorts,
}

/// A ConfigMap or Secret referenced by a container but absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MissingRef {
    ConfigMap(String),
    Secret(String),
}

/// One explained problem with a pod. Texts of ours are [`Message`]s the UI
/// translates (`english()` for MCP); the cluster's own words stay as they
/// are in [`message`](Self::message).
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub severity: Severity,
    pub kind: FindingKind,
    /// Container the finding is about; `None` for pod-level findings.
    pub container: Option<String>,
    pub title: Message,
    /// What happened, in our words (`Last run ended with exit code 1.`).
    pub summary: Option<Message>,
    /// The message of the kubelet, the scheduler or an event; may be empty.
    pub message: String,
    pub hint: Option<Message>,
}

impl Finding {
    /// The summary and the cluster's message, one per line, in English.
    pub fn detail(&self) -> String {
        self.detail_in(Locale::En)
    }

    /// [`detail`](Self::detail) in `locale`.
    pub fn detail_in(&self, locale: Locale) -> String {
        let summary = self.summary.as_ref().map(|s| s.translate(locale));
        summary
            .into_iter()
            .chain(Some(self.message.clone()).filter(|m| !m.is_empty()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The finding as a note of a details section.
    pub fn note(&self) -> Note {
        Note {
            severity: self.severity.message(),
            title: self.title.clone(),
            summary: self.summary.clone(),
            message: self.message.clone(),
            hint: self.hint.clone(),
        }
    }
}

impl Severity {
    /// `Error`, `Warning`, `Info`.
    pub fn message(self) -> Message {
        match self {
            Severity::Error => msg!("diagnostic-severity-error"),
            Severity::Warning => msg!("diagnostic-severity-warning"),
            Severity::Info => msg!("diagnostic-severity-info"),
        }
    }
}

/// Diagnoses `pod` as of now. `events` are the pod's events (may be empty);
/// events of other objects are ignored.
pub fn diagnose(pod: &Pod, events: &[Event]) -> Vec<Finding> {
    diagnose_at(pod, events, Timestamp::now())
}

/// Like [`diagnose`], with an explicit current time. Findings are sorted by
/// severity, most severe first. A healthy pod yields no findings.
pub fn diagnose_at(pod: &Pod, events: &[Event], now: Timestamp) -> Vec<Finding> {
    let mut findings = Vec::new();
    let status = pod.status.as_ref();
    let phase = status.and_then(|s| s.phase.as_deref());
    let conditions = status
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or_default();
    let terminating = pod.metadata.deletion_timestamp.is_some();

    if let Some(finding) = terminating_finding(pod, now) {
        findings.push(finding);
    }
    if let Some(finding) = pod_failed_finding(pod) {
        findings.push(finding);
    }
    if let Some(finding) = scheduling_finding(conditions) {
        findings.push(finding);
    }

    let init_statuses = status
        .and_then(|s| s.init_container_statuses.as_deref())
        .unwrap_or_default();
    for cs in init_statuses {
        findings.extend(container_finding(pod, cs, true));
    }
    let statuses = status
        .and_then(|s| s.container_statuses.as_deref())
        .unwrap_or_default();
    for cs in statuses {
        findings.extend(container_finding(pod, cs, false));
    }

    if phase == Some("Pending") {
        findings.extend(event_findings(pod, events));
    }

    if phase == Some("Running") && !terminating {
        findings.extend(readiness_findings(conditions, statuses, &findings));
    }

    findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
    findings
}

fn terminating_finding(pod: &Pod, now: Timestamp) -> Option<Finding> {
    let deadline = pod.metadata.deletion_timestamp.as_ref()?.0;
    let finalizers = pod.metadata.finalizers.as_deref().unwrap_or_default();
    let overdue = now.as_second() - deadline.as_second();
    if overdue <= 0 {
        return Some(Finding {
            severity: Severity::Info,
            kind: FindingKind::Terminating,
            container: None,
            title: msg!("diagnostic-terminating"),
            summary: Some(msg!(
                "diagnostic-terminating-detail",
                duration = format_duration(-overdue)
            )),
            message: String::new(),
            hint: None,
        });
    }
    let duration = format_duration(overdue);
    let (summary, hint) = if finalizers.is_empty() {
        (
            msg!("diagnostic-stuck-detail", duration = duration.as_str()),
            msg!("diagnostic-stuck-hint"),
        )
    } else {
        (
            msg!(
                "diagnostic-stuck-finalizers-detail",
                duration = duration.as_str(),
                finalizers = finalizers.join(", ")
            ),
            msg!("diagnostic-stuck-finalizers-hint"),
        )
    };
    Some(Finding {
        severity: Severity::Warning,
        kind: FindingKind::StuckTerminating,
        container: None,
        title: msg!("diagnostic-stuck", duration = duration),
        summary: Some(summary),
        message: String::new(),
        hint: Some(hint),
    })
}

fn pod_failed_finding(pod: &Pod) -> Option<Finding> {
    let status = pod.status.as_ref()?;
    if status.phase.as_deref() != Some("Failed") {
        return None;
    }
    let reason = status.reason.as_deref();
    let message = status.message.clone().unwrap_or_default();
    if reason == Some("Evicted") {
        return Some(Finding {
            severity: Severity::Error,
            kind: FindingKind::Evicted,
            container: None,
            title: msg!("diagnostic-evicted"),
            summary: None,
            message,
            hint: Some(msg!("diagnostic-evicted-hint")),
        });
    }
    // Container findings explain failures better; only report reasons the
    // container statuses would not, like `DeadlineExceeded`.
    let reason = reason?;
    Some(Finding {
        severity: Severity::Error,
        kind: FindingKind::PodFailed,
        container: None,
        title: msg!("diagnostic-pod-failed", reason = reason),
        summary: None,
        message,
        hint: None,
    })
}

fn scheduling_finding(conditions: &[PodCondition]) -> Option<Finding> {
    let cond = conditions
        .iter()
        .find(|c| c.type_ == "PodScheduled" && c.status == "False")?;
    let message = cond.message.clone().unwrap_or_default();
    if cond.reason.as_deref() == Some("SchedulingGated") {
        return Some(Finding {
            severity: Severity::Info,
            kind: FindingKind::SchedulingGated,
            container: None,
            title: msg!("diagnostic-gated"),
            summary: None,
            message,
            hint: Some(msg!("diagnostic-gated-hint")),
        });
    }
    let causes = scheduling_causes(&message);
    let title = if causes.is_empty() {
        msg!("diagnostic-unschedulable")
    } else {
        let labels: Vec<Message> = causes.iter().map(cause_label).collect();
        msg!("diagnostic-unschedulable-causes", causes = labels)
    };
    let hint = causes
        .first()
        .map(cause_hint)
        .unwrap_or_else(|| msg!("diagnostic-unschedulable-hint"));
    Some(Finding {
        severity: Severity::Error,
        kind: FindingKind::Unschedulable(causes),
        container: None,
        title,
        summary: None,
        message,
        hint: Some(hint),
    })
}

/// Parses a scheduler message such as `0/3 nodes are available: 1 Insufficient
/// cpu, 2 node(s) had untolerated taint {...}.`
fn scheduling_causes(message: &str) -> Vec<SchedulingCause> {
    let lower = message.to_ascii_lowercase();
    let mut causes = Vec::new();
    let mut push = |cause: SchedulingCause| {
        if !causes.contains(&cause) {
            causes.push(cause);
        }
    };
    for (index, _) in lower.match_indices("insufficient ") {
        let rest = &message[index + "insufficient ".len()..];
        let resource: String = rest
            .chars()
            .take_while(|c| !matches!(c, ',' | ' ' | ')'))
            .collect();
        let resource = resource.trim_end_matches('.');
        push(match resource {
            "cpu" => SchedulingCause::InsufficientCpu,
            "memory" => SchedulingCause::InsufficientMemory,
            other => SchedulingCause::InsufficientResource(other.to_string()),
        });
    }
    if lower.contains("untolerated taint") || lower.contains("didn't tolerate") {
        push(SchedulingCause::UntoleratedTaint);
    }
    if lower.contains("node affinity/selector")
        || lower.contains("didn't match node selector")
        || lower.contains("didn't match pod's node affinity")
    {
        push(SchedulingCause::NodeSelectorMismatch);
    }
    if lower.contains("pod affinity") || lower.contains("pod anti-affinity") {
        push(SchedulingCause::PodAffinityMismatch);
    }
    if lower.contains("unbound immediate persistentvolumeclaims")
        || lower.contains("unbound persistentvolumeclaim")
        || (lower.contains("persistentvolumeclaim") && lower.contains("not found"))
    {
        push(SchedulingCause::UnboundPvc);
    }
    if lower.contains("volume node affinity conflict") {
        push(SchedulingCause::VolumeNodeAffinityConflict);
    }
    if lower.contains("free ports") {
        push(SchedulingCause::NoFreePorts);
    }
    causes
}

fn cause_label(cause: &SchedulingCause) -> Message {
    match cause {
        SchedulingCause::InsufficientCpu => msg!("diagnostic-cause-cpu"),
        SchedulingCause::InsufficientMemory => msg!("diagnostic-cause-memory"),
        SchedulingCause::InsufficientResource(name) => {
            msg!("diagnostic-cause-resource", resource = name)
        }
        SchedulingCause::UntoleratedTaint => msg!("diagnostic-cause-taints"),
        SchedulingCause::NodeSelectorMismatch => msg!("diagnostic-cause-selector"),
        SchedulingCause::PodAffinityMismatch => msg!("diagnostic-cause-pod-affinity"),
        SchedulingCause::UnboundPvc => msg!("diagnostic-cause-pvc"),
        SchedulingCause::VolumeNodeAffinityConflict => msg!("diagnostic-cause-volume-affinity"),
        SchedulingCause::NoFreePorts => msg!("diagnostic-cause-ports"),
    }
}

fn cause_hint(cause: &SchedulingCause) -> Message {
    match cause {
        SchedulingCause::InsufficientCpu
        | SchedulingCause::InsufficientMemory
        | SchedulingCause::InsufficientResource(_) => msg!("diagnostic-resources-hint"),
        SchedulingCause::UntoleratedTaint => msg!("diagnostic-taints-hint"),
        SchedulingCause::NodeSelectorMismatch => msg!("diagnostic-selector-hint"),
        SchedulingCause::PodAffinityMismatch => msg!("diagnostic-pod-affinity-hint"),
        SchedulingCause::UnboundPvc => msg!("diagnostic-pvc-hint"),
        SchedulingCause::VolumeNodeAffinityConflict => msg!("diagnostic-volume-affinity-hint"),
        SchedulingCause::NoFreePorts => msg!("diagnostic-ports-hint"),
    }
}

/// `Container `api`` or `Init container `migrate``.
fn container_label(name: &str, init: bool) -> Message {
    if init {
        msg!("diagnostic-init-container", name = name)
    } else {
        msg!("diagnostic-container", name = name)
    }
}

fn container_finding(pod: &Pod, cs: &ContainerStatus, init: bool) -> Option<Finding> {
    let name = &cs.name;
    let label = container_label(name, init);
    let state = cs.state.as_ref();
    let last_terminated = cs.last_state.as_ref().and_then(|s| s.terminated.as_ref());
    let spec = container_spec(pod, name, init);

    if let Some(waiting) = state.and_then(|s| s.waiting.as_ref()) {
        let reason = waiting.reason.as_deref().unwrap_or_default();
        let message = waiting.message.clone().unwrap_or_default();
        match reason {
            "CrashLoopBackOff" => {
                if last_terminated.is_some_and(is_oom) {
                    return Some(oom_finding(cs, spec, init, true));
                }
                let kind = if init {
                    FindingKind::InitContainerFailed
                } else {
                    FindingKind::CrashLoopBackOff
                };
                let title = match last_terminated {
                    Some(last) => msg!(
                        "diagnostic-crash-looping-exit",
                        container = label,
                        exit = exit_summary(last),
                        restarts = restarts(cs.restart_count)
                    ),
                    None => msg!(
                        "diagnostic-crash-looping",
                        container = label,
                        restarts = restarts(cs.restart_count)
                    ),
                };
                let (summary, message) = match last_terminated {
                    Some(last) => termination_detail(last),
                    None => (None, message),
                };
                return Some(Finding {
                    severity: Severity::Error,
                    kind,
                    container: Some(name.clone()),
                    title,
                    summary,
                    message,
                    hint: Some(msg!("diagnostic-previous-logs-hint")),
                });
            }
            "ImagePullBackOff"
            | "ErrImagePull"
            | "InvalidImageName"
            | "ErrImageNeverPull"
            | "RegistryUnavailable"
            | "ImageInspectError" => {
                let image = if cs.image.is_empty() {
                    spec.and_then(|c| c.image.clone()).unwrap_or_default()
                } else {
                    cs.image.clone()
                };
                return Some(Finding {
                    severity: Severity::Error,
                    kind: FindingKind::ImagePull,
                    container: Some(name.clone()),
                    title: msg!(
                        "diagnostic-image-pull",
                        container = label,
                        image = image,
                        reason = reason
                    ),
                    summary: None,
                    hint: Some(image_hint(reason, &message)),
                    message,
                });
            }
            "CreateContainerConfigError" => {
                let missing = missing_ref(&message);
                let title = match &missing {
                    Some(MissingRef::ConfigMap(n)) => {
                        msg!("diagnostic-missing-configmap", container = label, name = n)
                    }
                    Some(MissingRef::Secret(n)) => {
                        msg!("diagnostic-missing-secret", container = label, name = n)
                    }
                    None => msg!("diagnostic-invalid-config", container = label),
                };
                return Some(Finding {
                    severity: Severity::Error,
                    kind: FindingKind::ContainerConfig(missing),
                    container: Some(name.clone()),
                    title,
                    summary: None,
                    message,
                    hint: Some(msg!("diagnostic-config-hint")),
                });
            }
            "" | "ContainerCreating" | "PodInitializing" => {}
            _ => {
                return Some(Finding {
                    severity: Severity::Warning,
                    kind: FindingKind::ContainerWaiting,
                    container: Some(name.clone()),
                    title: msg!("diagnostic-waiting", container = label, reason = reason),
                    summary: None,
                    message,
                    hint: None,
                });
            }
        }
    }

    if let Some(terminated) = state.and_then(|s| s.terminated.as_ref()) {
        if is_oom(terminated) {
            return Some(oom_finding(cs, spec, init, false));
        }
        if terminated.exit_code != 0 {
            let kind = if init {
                FindingKind::InitContainerFailed
            } else {
                FindingKind::ContainerFailed
            };
            let (summary, message) = termination_detail(terminated);
            return Some(Finding {
                severity: Severity::Error,
                kind,
                container: Some(name.clone()),
                title: msg!(
                    "diagnostic-failed",
                    container = label,
                    exit = exit_summary(terminated)
                ),
                summary,
                message,
                hint: Some(msg!("diagnostic-logs-hint")),
            });
        }
    }

    if state.and_then(|s| s.running.as_ref()).is_some()
        && let Some(last) = last_terminated
        && cs.restart_count > 0
    {
        if is_oom(last) {
            let mut finding = oom_finding(cs, spec, init, false);
            finding.severity = Severity::Warning;
            return Some(finding);
        }
        let (summary, message) = termination_detail(last);
        return Some(Finding {
            severity: Severity::Info,
            kind: FindingKind::Restarted,
            container: Some(name.clone()),
            title: msg!(
                "diagnostic-restarted",
                container = label,
                exit = exit_summary(last),
                restarts = restarts(cs.restart_count)
            ),
            summary,
            message,
            hint: Some(msg!("diagnostic-previous-logs-hint")),
        });
    }
    None
}

fn oom_finding(
    cs: &ContainerStatus,
    spec: Option<&Container>,
    init: bool,
    crash_looping: bool,
) -> Finding {
    let name = &cs.name;
    let label = container_label(name, init);
    let limit = spec
        .and_then(|c| c.resources.as_ref())
        .and_then(|r| r.limits.as_ref())
        .and_then(|l| l.get("memory"))
        .map(|q| q.0.clone());
    let restarts = restarts(cs.restart_count);
    let title = match (&limit, crash_looping) {
        (Some(limit), true) => msg!(
            "diagnostic-oom-limit-looping",
            container = label,
            limit = limit,
            restarts = restarts
        ),
        (Some(limit), false) => msg!(
            "diagnostic-oom-limit",
            container = label,
            limit = limit,
            restarts = restarts
        ),
        (None, true) => msg!(
            "diagnostic-oom-looping",
            container = label,
            restarts = restarts
        ),
        (None, false) => msg!("diagnostic-oom", container = label, restarts = restarts),
    };
    let (summary, hint) = match &limit {
        Some(limit) => (
            msg!("diagnostic-oom-limit-detail", limit = limit),
            msg!("diagnostic-oom-limit-hint", limit = limit),
        ),
        None => (msg!("diagnostic-oom-detail"), msg!("diagnostic-oom-hint")),
    };
    Finding {
        severity: Severity::Error,
        kind: FindingKind::OomKilled,
        container: Some(name.clone()),
        title,
        summary: Some(summary),
        message: String::new(),
        hint: Some(hint),
    }
}

fn readiness_findings(
    conditions: &[PodCondition],
    statuses: &[ContainerStatus],
    existing: &[Finding],
) -> Vec<Finding> {
    let Some(cond) = conditions
        .iter()
        .find(|c| c.type_ == "Ready" && c.status == "False")
    else {
        return Vec::new();
    };
    let message = cond.message.clone().unwrap_or_default();
    let unready: Vec<Finding> = statuses
        .iter()
        .filter(|cs| !cs.ready && cs.state.as_ref().and_then(|s| s.running.as_ref()).is_some())
        .map(|cs| {
            let name = &cs.name;
            let label = container_label(name, false);
            let (title, hint) = if cs.started == Some(false) {
                (
                    msg!("diagnostic-startup-probe", container = label),
                    msg!("diagnostic-startup-probe-hint"),
                )
            } else {
                (
                    msg!("diagnostic-not-ready", container = label),
                    msg!("diagnostic-not-ready-hint"),
                )
            };
            Finding {
                severity: Severity::Warning,
                kind: FindingKind::NotReady,
                container: Some(name.clone()),
                title,
                summary: None,
                message: message.clone(),
                hint: Some(hint),
            }
        })
        .collect();
    if !unready.is_empty() || existing.iter().any(|f| f.container.is_some()) {
        return unready;
    }
    // All containers are ready, so readiness gates or similar hold the pod back.
    vec![Finding {
        severity: Severity::Warning,
        kind: FindingKind::NotReady,
        container: None,
        title: msg!("diagnostic-pod-not-ready"),
        summary: None,
        message,
        hint: Some(msg!("diagnostic-pod-not-ready-hint")),
    }]
}

fn event_findings(pod: &Pod, events: &[Event]) -> Vec<Finding> {
    let mut latest: Vec<&Event> = Vec::new();
    for event in events.iter().filter(|e| is_pod_warning(pod, e)) {
        let kind = event_kind(event);
        if kind.is_none() {
            continue;
        }
        match latest.iter_mut().find(|e| event_kind(e) == kind) {
            Some(slot) if event_time(event) >= event_time(slot) => *slot = event,
            Some(_) => {}
            None => latest.push(event),
        }
    }
    latest
        .into_iter()
        .filter_map(|event| {
            let kind = event_kind(event)?;
            let message = event.message.clone().unwrap_or_default();
            let (title, hint) = match kind {
                FindingKind::VolumeMountFailed => (
                    msg!("diagnostic-volume-mount"),
                    match missing_ref(&message) {
                        Some(MissingRef::ConfigMap(n)) => {
                            msg!("diagnostic-volume-configmap-hint", name = n)
                        }
                        Some(MissingRef::Secret(n)) => {
                            msg!("diagnostic-volume-secret-hint", name = n)
                        }
                        None => msg!("diagnostic-volume-hint"),
                    },
                ),
                _ => (msg!("diagnostic-sandbox"), msg!("diagnostic-sandbox-hint")),
            };
            Some(Finding {
                severity: Severity::Error,
                kind,
                container: None,
                title,
                summary: None,
                message,
                hint: Some(hint),
            })
        })
        .collect()
}

fn is_pod_warning(pod: &Pod, event: &Event) -> bool {
    let obj = &event.involved_object;
    let same_pod = match (obj.uid.as_deref(), pod.metadata.uid.as_deref()) {
        (Some(a), Some(b)) => a == b,
        _ => {
            obj.kind.as_deref() == Some("Pod")
                && obj.name.is_some()
                && obj.name == pod.metadata.name
                && obj.namespace == pod.metadata.namespace
        }
    };
    same_pod && event.type_.as_deref() == Some("Warning")
}

fn event_kind(event: &Event) -> Option<FindingKind> {
    match event.reason.as_deref()? {
        "FailedMount" | "FailedAttachVolume" => Some(FindingKind::VolumeMountFailed),
        "FailedCreatePodSandBox" => Some(FindingKind::SandboxFailed),
        _ => None,
    }
}

fn event_time(event: &Event) -> Option<Timestamp> {
    event
        .last_timestamp
        .as_ref()
        .map(|t| t.0)
        .or_else(|| event.event_time.as_ref().map(|t| t.0))
}

fn container_spec<'a>(pod: &'a Pod, name: &str, init: bool) -> Option<&'a Container> {
    let spec = pod.spec.as_ref()?;
    let containers = if init {
        spec.init_containers.as_deref().unwrap_or_default()
    } else {
        &spec.containers
    };
    containers.iter().find(|c| c.name == name)
}

fn is_oom(terminated: &ContainerStateTerminated) -> bool {
    terminated.reason.as_deref() == Some("OOMKilled")
}

/// `exit code 1` or `OOMKilled, exit code 137`.
fn exit_summary(terminated: &ContainerStateTerminated) -> Message {
    let code = terminated.exit_code;
    match terminated.reason.as_deref() {
        Some(reason) if reason != "Error" && reason != "Completed" => {
            msg!("diagnostic-exit-reason", reason = reason, code = code)
        }
        _ => msg!("diagnostic-exit-code", code = code),
    }
}

/// How the last run ended, in our words, and the container's own message.
fn termination_detail(terminated: &ContainerStateTerminated) -> (Option<Message>, String) {
    let code = terminated.exit_code;
    let summary = match (signal_name(code), terminated.reason.as_deref()) {
        (Some(signal), Some(reason)) => msg!(
            "diagnostic-last-run-signal-reason",
            code = code,
            signal = signal,
            reason = reason
        ),
        (Some(signal), None) => msg!("diagnostic-last-run-signal", code = code, signal = signal),
        (None, Some(reason)) => msg!("diagnostic-last-run-reason", code = code, reason = reason),
        (None, None) => msg!("diagnostic-last-run", code = code),
    };
    let message = terminated
        .message
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    (Some(summary), message)
}

fn signal_name(exit_code: i32) -> Option<&'static str> {
    match exit_code {
        130 => Some("SIGINT"),
        134 => Some("SIGABRT"),
        137 => Some("SIGKILL"),
        139 => Some("SIGSEGV"),
        143 => Some("SIGTERM"),
        _ => None,
    }
}

fn restarts(count: i32) -> Message {
    msg!("diagnostic-restarts", count = count)
}

fn image_hint(reason: &str, message: &str) -> Message {
    let lower = message.to_ascii_lowercase();
    if reason == "InvalidImageName" {
        msg!("diagnostic-image-name-hint")
    } else if reason == "ErrImageNeverPull" {
        msg!("diagnostic-never-pull-hint")
    } else if ["unauthorized", "authentication", "denied", "forbidden"]
        .iter()
        .any(|s| lower.contains(s))
    {
        msg!("diagnostic-credentials-hint")
    } else if ["not found", "manifest unknown", "does not exist"]
        .iter()
        .any(|s| lower.contains(s))
    {
        msg!("diagnostic-image-missing-hint")
    } else {
        msg!("diagnostic-image-hint")
    }
}

/// Extracts the ConfigMap/Secret name from kubelet messages like
/// `configmap "app" not found` or `couldn't find key X in Secret ns/app`.
fn missing_ref(message: &str) -> Option<MissingRef> {
    let lower = message.to_ascii_lowercase();
    for (prefix, secret) in [("configmap \"", false), ("secret \"", true)] {
        if let Some(start) = lower.find(prefix) {
            let rest = &message[start + prefix.len()..];
            if let Some(end) = rest.find('"') {
                let name = rest[..end].to_string();
                return Some(if secret {
                    MissingRef::Secret(name)
                } else {
                    MissingRef::ConfigMap(name)
                });
            }
        }
    }
    for (marker, secret) in [(" in configmap ", false), (" in secret ", true)] {
        if let Some(start) = lower.find(marker) {
            let rest = &message[start + marker.len()..];
            let full = rest.split_whitespace().next()?;
            let name = full.rsplit('/').next()?.to_string();
            return Some(if secret {
                MissingRef::Secret(name)
            } else {
                MissingRef::ConfigMap(name)
            });
        }
    }
    None
}

fn format_duration(seconds: i64) -> String {
    match seconds {
        ..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

#[cfg(test)]
mod tests;
