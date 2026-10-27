//! PersistentVolumeClaim and PersistentVolume plugins: status, binding,
//! sizes, storage class, the volume source, the pods that mount a claim and
//! delete.

use k8s_openapi::api::core::v1::{PersistentVolume, PersistentVolumeClaim};
use kubach_i18n::{Message, msg};
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};
use serde_json::Value;

use super::{
    DELETE_ACTION, OVERVIEW, action, col, creation, decode, delete, fallback_cells, name_ns_cells,
    name_ns_columns, or_none, relations_with, section, text, unknown,
};
use crate::plugin::{
    Action, ActionId, ActionParams, BoxFuture, Brief, Cell, Column, ColumnKind, DetailSection,
    Field, FieldValue, GenericPlugin, Health, HealthStatus, ObjectRef, PluginError, RelatedFilter,
    RelatedObjects, RelatedOrder, Relation, RelationKind, ResourcePlugin, SectionBody,
};

pub struct PersistentVolumeClaimPlugin;
pub struct PersistentVolumePlugin;

/// Finalizer that keeps a claim while a pod uses it.
pub const PVC_PROTECTION: &str = "kubernetes.io/pvc-protection";
/// Finalizer that keeps a volume while a claim is bound to it.
pub const PV_PROTECTION: &str = "kubernetes.io/pv-protection";

/// Title of the section with the pods that mount a claim.
pub const CLAIM_USERS: &str = "Used by pods";

/// PersistentVolumeClaim conditions that report a failure when `True`.
pub(crate) const NEGATIVE_CONDITIONS: [&str; 3] = [
    "ControllerResizeError",
    "NodeResizeError",
    "ModifyVolumeError",
];
/// PersistentVolumeClaim conditions that report something to look at when
/// `True`: work in progress, or no pod using the claim.
pub(crate) const WARNING_CONDITIONS: [&str; 4] = [
    "Resizing",
    "FileSystemResizePending",
    "ModifyingVolume",
    "Unused",
];

/// `Terminating: used by pods a, b`: why a claim being deleted stays.
pub fn terminating_used_by(pods: &[String]) -> Message {
    msg!("state-claim-used-by", pods = pods.join(", "))
}

/// Note for a delete confirmation of claims that pods still mount.
pub fn delete_used_by_note(pods: &[String]) -> kubach_i18n::Message {
    kubach_i18n::msg!(
        "action-claim-used-by",
        pods = pods.join(", "),
        finalizer = PVC_PROTECTION
    )
}

/// Whether `pod` mounts the claim `claim` (of the pod's namespace).
pub fn pod_uses_claim(pod: &DynamicObject, claim: &str) -> bool {
    pod.data
        .pointer("/spec/volumes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|v| {
            v.pointer("/persistentVolumeClaim/claimName")
                .and_then(Value::as_str)
                == Some(claim)
        })
}

/// `RWO,RWX`, like kubectl.
fn access_modes(modes: Option<&Vec<String>>) -> String {
    modes
        .into_iter()
        .flatten()
        .map(|m| match m.as_str() {
            "ReadWriteOnce" => "RWO",
            "ReadOnlyMany" => "ROX",
            "ReadWriteMany" => "RWX",
            "ReadWriteOncePod" => "RWOP",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn storage(
    map: Option<
        &std::collections::BTreeMap<
            String,
            k8s_openapi::apimachinery::pkg::api::resource::Quantity,
        >,
    >,
) -> String {
    map.and_then(|m| m.get("storage"))
        .map(|q| q.0.clone())
        .unwrap_or_default()
}

fn has_finalizer(obj: &DynamicObject, finalizer: &str) -> bool {
    obj.metadata
        .finalizers
        .iter()
        .flatten()
        .any(|f| f == finalizer)
}

fn storage_class_link(name: &str) -> FieldValue {
    FieldValue::Link {
        phrase: None,
        label: name.to_owned(),
        target: ObjectRef {
            api_version: "storage.k8s.io/v1".into(),
            kind: "StorageClass".into(),
            name: name.into(),
            namespace: None,
        },
    }
}

fn reference(api_version: &str, kind: &str, name: &str, namespace: Option<&str>) -> Relation {
    Relation {
        kind: RelationKind::References,
        target: ObjectRef {
            api_version: api_version.into(),
            kind: kind.into(),
            name: name.into(),
            namespace: namespace.map(str::to_owned),
        },
    }
}

/// The pods of `namespace` that mount `claim`.
fn claim_users(namespace: Option<String>, claim: &str) -> DetailSection {
    section(
        CLAIM_USERS,
        SectionBody::Related(RelatedObjects {
            api_version: "v1".into(),
            kind: "Pod".into(),
            namespace,
            filter: RelatedFilter::ClaimUsers(claim.to_owned()),
            order: RelatedOrder::Name,
        }),
    )
}

fn delete_action<'a, K>(
    client: &'a Client,
    obj: &'a DynamicObject,
    action_id: &'a ActionId,
) -> BoxFuture<'a, Result<(), PluginError>>
where
    K: kube::Resource<DynamicType = ()> + 'a,
{
    Box::pin(async move {
        match *action_id {
            action::DELETE => delete::<K>(client, obj).await,
            _ => Err(unknown(action_id)),
        }
    })
}

// --------------------------------------------------------------------- claim

fn claim_phase(pvc: &PersistentVolumeClaim) -> &str {
    pvc.status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("Pending")
}

/// Health of a claim: `Bound`, `Pending`, `Lost`, or `Terminating` with
/// what keeps it (the cell shows the short form).
pub(crate) fn claim_health(obj: &DynamicObject, pvc: &PersistentVolumeClaim) -> Health {
    if obj.metadata.deletion_timestamp.is_some() {
        let message = if has_finalizer(obj, PVC_PROTECTION) {
            msg!("state-claim-protected", finalizer = PVC_PROTECTION)
        } else {
            return Health::new(HealthStatus::Progressing, "Terminating");
        };
        return Health::phrase(HealthStatus::Progressing, message);
    }
    let phase = claim_phase(pvc);
    let status = match phase {
        "Bound" => HealthStatus::Healthy,
        "Pending" => HealthStatus::Progressing,
        "Lost" => HealthStatus::Failed,
        _ => HealthStatus::Unknown,
    };
    Health::new(status, phase)
}

/// The short status: the phase, or `Terminating`.
fn status_label(obj: &DynamicObject, phase: &str) -> String {
    if obj.metadata.deletion_timestamp.is_some() {
        "Terminating".to_owned()
    } else {
        phase.to_owned()
    }
}

impl ResourcePlugin for PersistentVolumeClaimPlugin {
    fn name(&self) -> &'static str {
        "persistentvolumeclaim"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("", "v1", "PersistentVolumeClaim")]
    }

    fn columns(&self) -> Vec<Column> {
        let mut cols = name_ns_columns();
        cols.extend([
            col("status", "Status", ColumnKind::Status),
            col("volume", "Volume", ColumnKind::Text),
            col("capacity", "Capacity", ColumnKind::Text),
            col("request", "Request", ColumnKind::Text),
            col("storage_class", "Storage class", ColumnKind::Text),
            col("access_modes", "Access modes", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]);
        cols
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(pvc) = decode::<PersistentVolumeClaim>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let spec = pvc.spec.clone().unwrap_or_default();
        let status = pvc.status.clone().unwrap_or_default();
        let mut cells = name_ns_cells(obj);
        cells.extend([
            Cell::Status(Health::new(
                claim_health(obj, &pvc).status,
                status_label(obj, claim_phase(&pvc)),
            )),
            text(spec.volume_name.clone().unwrap_or_default()),
            text(storage(status.capacity.as_ref())),
            text(storage(
                spec.resources.as_ref().and_then(|r| r.requests.as_ref()),
            )),
            text(spec.storage_class_name.clone().unwrap_or_default()),
            text(access_modes(
                status.access_modes.as_ref().or(spec.access_modes.as_ref()),
            )),
            Cell::Age(creation(obj)),
        ]);
        cells
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(pvc) = decode::<PersistentVolumeClaim>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = pvc.spec.clone().unwrap_or_default();
        let status = pvc.status.clone().unwrap_or_default();
        let resources = spec.resources.clone().unwrap_or_default();
        let mut fields = vec![Field::text("Status", status_label(obj, claim_phase(&pvc)))];
        fields.push(match spec.volume_name.as_deref() {
            Some(volume) if !volume.is_empty() => {
                Field::link("Volume", "PersistentVolume", volume, None)
            }
            _ => Field::text("Volume", "<none>"),
        });
        fields.push(Field::text(
            "Capacity",
            or_none(storage(status.capacity.as_ref())),
        ));
        fields.push(Field::text(
            "Request",
            or_none(storage(resources.requests.as_ref())),
        ));
        let limit = storage(resources.limits.as_ref());
        if !limit.is_empty() {
            fields.push(Field::text("Limit", limit));
        }
        fields.push(match spec.storage_class_name.as_deref() {
            Some(class) if !class.is_empty() => {
                Field::new("Storage class", storage_class_link(class))
            }
            _ => Field::text("Storage class", "<none>"),
        });
        fields.push(Field::text(
            "Access modes",
            or_none(access_modes(
                status.access_modes.as_ref().or(spec.access_modes.as_ref()),
            )),
        ));
        fields.push(Field::text(
            "Volume mode",
            spec.volume_mode
                .clone()
                .unwrap_or_else(|| "Filesystem".into()),
        ));
        let source = spec
            .data_source_ref
            .as_ref()
            .map(|s| (&s.kind, &s.name))
            .or(spec.data_source.as_ref().map(|s| (&s.kind, &s.name)));
        if let Some((kind, name)) = source {
            fields.push(Field::text("Data source", format!("{kind} {name}")));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(fields))];
        if let Some(name) = obj.metadata.name.as_deref() {
            sections.push(claim_users(obj.metadata.namespace.clone(), name));
        }
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![DELETE_ACTION]
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        delete_action::<PersistentVolumeClaim>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let mut extra = Vec::new();
        if let Some(pvc) = decode::<PersistentVolumeClaim>(obj) {
            let spec = pvc.spec.unwrap_or_default();
            if let Some(volume) = spec.volume_name.filter(|v| !v.is_empty()) {
                extra.push(reference("v1", "PersistentVolume", &volume, None));
            }
            if let Some(class) = spec.storage_class_name.filter(|c| !c.is_empty()) {
                extra.push(reference("storage.k8s.io/v1", "StorageClass", &class, None));
            }
        }
        relations_with(obj, extra)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<PersistentVolumeClaim>(obj) {
            Some(pvc) => claim_health(obj, &pvc),
            None => GenericPlugin.health(obj),
        }
    }

    /// `Bound · 1Gi`.
    fn brief(&self, obj: &DynamicObject) -> Brief {
        let Some(pvc) = decode::<PersistentVolumeClaim>(obj) else {
            return GenericPlugin.brief(obj);
        };
        let health = claim_health(obj, &pvc);
        let size = storage(pvc.status.as_ref().and_then(|s| s.capacity.as_ref()));
        let mut note = claim_phase(&pvc).to_owned();
        if !size.is_empty() {
            note = format!("{note} · {size}");
        }
        Brief {
            health,
            note,
            phrase: None,
            containers: Vec::new(),
        }
    }
}

// -------------------------------------------------------------------- volume

/// Keys of a PersistentVolume spec that are not its volume source.
const NOT_SOURCE: [&str; 9] = [
    "accessModes",
    "capacity",
    "claimRef",
    "mountOptions",
    "nodeAffinity",
    "persistentVolumeReclaimPolicy",
    "storageClassName",
    "volumeMode",
    "volumeAttributesClassName",
];

/// Readable name of a volume source key.
fn source_title(key: &str) -> String {
    match key {
        "csi" => "CSI".into(),
        "hostPath" => "Host path".into(),
        "nfs" => "NFS".into(),
        "local" => "Local".into(),
        "iscsi" => "iSCSI".into(),
        "fc" => "Fibre Channel".into(),
        "awsElasticBlockStore" => "AWS EBS".into(),
        "gcePersistentDisk" => "GCE persistent disk".into(),
        "azureDisk" => "Azure disk".into(),
        "azureFile" => "Azure file".into(),
        "cephfs" => "CephFS".into(),
        "rbd" => "Ceph RBD".into(),
        other => other.into(),
    }
}

/// Fields of a volume source, nested maps flattened as `parent.key`.
fn flatten(prefix: &str, value: &Value, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, v, out);
            }
        }
        Value::String(s) => out.push((prefix.to_owned(), s.clone())),
        Value::Null => {}
        other => out.push((prefix.to_owned(), other.to_string())),
    }
}

/// The volume source of a PV: its type and fields (`driver`,
/// `volumeHandle`, `path`, `server`, …).
pub(crate) fn volume_source(obj: &DynamicObject) -> Option<(String, Vec<(String, String)>)> {
    let spec = obj.data.get("spec")?.as_object()?;
    let (key, value) = spec
        .iter()
        .find(|(k, v)| !NOT_SOURCE.contains(&k.as_str()) && v.is_object())?;
    let mut fields = Vec::new();
    flatten("", value, &mut fields);
    Some((source_title(key), fields))
}

fn volume_phase(pv: &PersistentVolume) -> &str {
    pv.status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("Pending")
}

/// Health of a volume: `Available`, `Bound`, `Released` (claim gone, data
/// kept), `Failed`, or `Terminating`.
pub(crate) fn volume_health(obj: &DynamicObject, pv: &PersistentVolume) -> Health {
    if obj.metadata.deletion_timestamp.is_some() {
        let message = if has_finalizer(obj, PV_PROTECTION) && volume_phase(pv) == "Bound" {
            msg!("state-volume-protected", finalizer = PV_PROTECTION)
        } else {
            return Health::new(HealthStatus::Progressing, "Terminating");
        };
        return Health::phrase(HealthStatus::Progressing, message);
    }
    let phase = volume_phase(pv);
    let status = match phase {
        "Available" | "Bound" => HealthStatus::Healthy,
        "Pending" => HealthStatus::Progressing,
        "Released" => HealthStatus::Degraded,
        "Failed" => HealthStatus::Failed,
        _ => HealthStatus::Unknown,
    };
    let message = pv
        .status
        .as_ref()
        .and_then(|s| s.message.clone().or_else(|| s.reason.clone()))
        .filter(|_| status != HealthStatus::Healthy)
        .map_or_else(|| phase.to_owned(), |m| format!("{phase}: {m}"));
    Health::new(status, message)
}

/// `namespace/name` of the bound claim.
fn claim_of(pv: &PersistentVolume) -> Option<(String, String)> {
    let claim = pv.spec.as_ref()?.claim_ref.as_ref()?;
    Some((claim.namespace.clone()?, claim.name.clone()?))
}

impl ResourcePlugin for PersistentVolumePlugin {
    fn name(&self) -> &'static str {
        "persistentvolume"
    }

    fn supported(&self) -> Vec<GroupVersionKind> {
        vec![GroupVersionKind::gvk("", "v1", "PersistentVolume")]
    }

    fn columns(&self) -> Vec<Column> {
        vec![
            col("name", "Name", ColumnKind::Text),
            col("status", "Status", ColumnKind::Status),
            col("claim", "Claim", ColumnKind::Text),
            col("capacity", "Capacity", ColumnKind::Text),
            col("storage_class", "Storage class", ColumnKind::Text),
            col("reclaim_policy", "Reclaim policy", ColumnKind::Text),
            col("access_modes", "Access modes", ColumnKind::Text),
            col("age", "Age", ColumnKind::Age),
        ]
    }

    fn cells(&self, obj: &DynamicObject) -> Vec<Cell> {
        let Some(pv) = decode::<PersistentVolume>(obj) else {
            return fallback_cells(obj, &self.columns());
        };
        let spec = pv.spec.clone().unwrap_or_default();
        let label = status_label(obj, volume_phase(&pv));
        vec![
            text(obj.metadata.name.clone().unwrap_or_default()),
            Cell::Status(Health::new(volume_health(obj, &pv).status, label)),
            text(
                claim_of(&pv)
                    .map(|(ns, name)| format!("{ns}/{name}"))
                    .unwrap_or_default(),
            ),
            text(storage(spec.capacity.as_ref())),
            text(spec.storage_class_name.clone().unwrap_or_default()),
            text(
                spec.persistent_volume_reclaim_policy
                    .clone()
                    .unwrap_or_default(),
            ),
            text(access_modes(spec.access_modes.as_ref())),
            Cell::Age(creation(obj)),
        ]
    }

    fn details(&self, obj: &DynamicObject) -> Vec<DetailSection> {
        let Some(pv) = decode::<PersistentVolume>(obj) else {
            return GenericPlugin.details(obj);
        };
        let spec = pv.spec.clone().unwrap_or_default();
        let claim = claim_of(&pv);
        let mut fields = vec![Field::text("Status", status_label(obj, volume_phase(&pv)))];
        fields.push(match &claim {
            Some((ns, name)) => Field::new(
                "Claim",
                FieldValue::Link {
                    phrase: None,
                    label: format!("{ns}/{name}"),
                    target: ObjectRef {
                        api_version: "v1".into(),
                        kind: "PersistentVolumeClaim".into(),
                        name: name.clone(),
                        namespace: Some(ns.clone()),
                    },
                },
            ),
            None => Field::text("Claim", "<none>"),
        });
        fields.push(Field::text(
            "Capacity",
            or_none(storage(spec.capacity.as_ref())),
        ));
        fields.push(match spec.storage_class_name.as_deref() {
            Some(class) if !class.is_empty() => {
                Field::new("Storage class", storage_class_link(class))
            }
            _ => Field::text("Storage class", "<none>"),
        });
        fields.push(Field::text(
            "Reclaim policy",
            or_none(
                spec.persistent_volume_reclaim_policy
                    .clone()
                    .unwrap_or_default(),
            ),
        ));
        fields.push(Field::text(
            "Access modes",
            or_none(access_modes(spec.access_modes.as_ref())),
        ));
        fields.push(Field::text(
            "Volume mode",
            spec.volume_mode
                .clone()
                .unwrap_or_else(|| "Filesystem".into()),
        ));
        let mount_options = spec.mount_options.clone().unwrap_or_default();
        if !mount_options.is_empty() {
            fields.push(Field::text("Mount options", mount_options.join(",")));
        }
        let mut sections = vec![section(OVERVIEW, SectionBody::Fields(fields))];
        if let Some((kind, mut pairs)) = volume_source(obj) {
            pairs.insert(0, ("Type".into(), kind));
            sections.push(section("Source", SectionBody::KeyValues(pairs)));
        }
        if let Some((ns, name)) = &claim {
            sections.push(claim_users(Some(ns.clone()), name));
        }
        sections.extend(GenericPlugin.details(obj));
        sections
    }

    fn actions(&self) -> Vec<Action> {
        vec![DELETE_ACTION]
    }

    fn run_action<'a>(
        &'a self,
        client: &'a Client,
        obj: &'a DynamicObject,
        action_id: &'a ActionId,
        _params: &'a ActionParams,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        delete_action::<PersistentVolume>(client, obj, action_id)
    }

    fn relations(&self, obj: &DynamicObject) -> Vec<Relation> {
        let mut extra = Vec::new();
        if let Some(pv) = decode::<PersistentVolume>(obj) {
            if let Some((ns, name)) = claim_of(&pv) {
                extra.push(reference("v1", "PersistentVolumeClaim", &name, Some(&ns)));
            }
            if let Some(class) = pv
                .spec
                .and_then(|s| s.storage_class_name)
                .filter(|c| !c.is_empty())
            {
                extra.push(reference("storage.k8s.io/v1", "StorageClass", &class, None));
            }
        }
        relations_with(obj, extra)
    }

    fn health(&self, obj: &DynamicObject) -> Health {
        match decode::<PersistentVolume>(obj) {
            Some(pv) => volume_health(obj, &pv),
            None => GenericPlugin.health(obj),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::plugin::builtin::pair;
    use crate::plugin::builtin::test_util::{obj, pairs, section as section_body};

    fn pvc(phase: &str, extra_meta: Value) -> DynamicObject {
        let mut meta = json!({
            "name": "data", "namespace": "ns1",
            "creationTimestamp": "2026-10-01T00:00:00Z",
            "finalizers": [PVC_PROTECTION]
        });
        if let (Value::Object(m), Value::Object(extra)) = (&mut meta, extra_meta) {
            m.extend(extra);
        }
        obj(json!({
            "apiVersion": "v1", "kind": "PersistentVolumeClaim",
            "metadata": meta,
            "spec": {
                "accessModes": ["ReadWriteOnce"],
                "resources": {"requests": {"storage": "1Gi"}},
                "storageClassName": "standard",
                "volumeName": "pvc-123",
                "volumeMode": "Filesystem"
            },
            "status": {
                "phase": phase,
                "accessModes": ["ReadWriteOnce"],
                "capacity": {"storage": "2Gi"}
            }
        }))
    }

    fn pv(phase: &str) -> DynamicObject {
        obj(json!({
            "apiVersion": "v1", "kind": "PersistentVolume",
            "metadata": {
                "name": "pvc-123",
                "creationTimestamp": "2026-10-01T00:00:00Z",
                "finalizers": [PV_PROTECTION]
            },
            "spec": {
                "accessModes": ["ReadWriteOnce", "ReadOnlyMany"],
                "capacity": {"storage": "2Gi"},
                "claimRef": {"kind": "PersistentVolumeClaim", "namespace": "ns1", "name": "data"},
                "persistentVolumeReclaimPolicy": "Delete",
                "storageClassName": "standard",
                "csi": {
                    "driver": "ebs.csi.aws.com",
                    "volumeHandle": "vol-0abc",
                    "fsType": "ext4",
                    "volumeAttributes": {"zone": "eu-1a"}
                }
            },
            "status": {"phase": phase}
        }))
    }

    fn field<'a>(sections: &'a [DetailSection], key: &str) -> &'a FieldValue {
        let SectionBody::Fields(fields) = section_body(sections, OVERVIEW) else {
            panic!("overview is not fields");
        };
        &fields
            .iter()
            .find(|f| f.key == key)
            .unwrap_or_else(|| panic!("field {key} missing"))
            .value
    }

    #[test]
    fn claim_cells() {
        let p = PersistentVolumeClaimPlugin;
        let cells = p.cells(&pvc("Bound", json!({})));
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[2..8],
            [
                Cell::Status(Health::new(HealthStatus::Healthy, "Bound")),
                text("pvc-123"),
                text("2Gi"),
                text("1Gi"),
                text("standard"),
                text("RWO"),
            ]
        );
        for (phase, status) in [
            ("Pending", HealthStatus::Progressing),
            ("Lost", HealthStatus::Failed),
        ] {
            assert_eq!(
                p.cells(&pvc(phase, json!({})))[2],
                Cell::Status(Health::new(status, phase))
            );
        }
    }

    #[test]
    fn terminating_claim_names_the_protection() {
        let p = PersistentVolumeClaimPlugin;
        let o = pvc(
            "Bound",
            json!({"deletionTimestamp": "2026-10-02T00:00:00Z"}),
        );
        assert_eq!(
            p.cells(&o)[2],
            Cell::Status(Health::new(HealthStatus::Progressing, "Terminating"))
        );
        let health = p.health(&o);
        assert_eq!(health.status, HealthStatus::Progressing);
        assert!(health.message.contains(PVC_PROTECTION), "{health:?}");
        assert_eq!(
            terminating_used_by(&["web-0".into(), "web-1".into()]).english(),
            "Terminating: used by pods web-0, web-1"
        );
        assert!(
            delete_used_by_note(&["web-0".into()])
                .english()
                .contains("web-0")
        );
    }

    #[test]
    fn claim_details_link_volume_class_and_pods() {
        let o = pvc("Bound", json!({}));
        let sections = PersistentVolumeClaimPlugin.details(&o);
        let FieldValue::Link { target, .. } = field(&sections, "Volume") else {
            panic!("volume is not a link");
        };
        assert_eq!(
            (target.kind.as_str(), target.namespace.as_deref()),
            ("PersistentVolume", None)
        );
        let FieldValue::Link { target, .. } = field(&sections, "Storage class") else {
            panic!("storage class is not a link");
        };
        assert_eq!(target.api_version, "storage.k8s.io/v1");
        assert_eq!(field(&sections, "Capacity").text(), "2Gi");
        assert_eq!(field(&sections, "Request").text(), "1Gi");
        assert_eq!(field(&sections, "Access modes").text(), "RWO");
        let SectionBody::Related(users) = section_body(&sections, CLAIM_USERS) else {
            panic!("users is not related");
        };
        assert_eq!(users.kind, "Pod");
        assert_eq!(users.namespace.as_deref(), Some("ns1"));
        assert_eq!(users.filter, RelatedFilter::ClaimUsers("data".into()));

        let pod = |claim: &str| {
            obj(json!({
                "apiVersion": "v1", "kind": "Pod",
                "metadata": {"name": "web", "namespace": "ns1"},
                "spec": {"containers": [], "volumes": [
                    {"name": "cfg", "configMap": {"name": "c"}},
                    {"name": "d", "persistentVolumeClaim": {"claimName": claim}}
                ]}
            }))
        };
        assert!(users.matches(&pod("data")));
        assert!(!users.matches(&pod("other")));

        let rels = PersistentVolumeClaimPlugin.relations(&o);
        let kinds: Vec<_> = rels.iter().map(|r| r.target.kind.as_str()).collect();
        assert_eq!(kinds, ["PersistentVolume", "StorageClass"]);
        let brief = PersistentVolumeClaimPlugin.brief(&o);
        assert_eq!(brief.note, "Bound · 2Gi");
    }

    #[test]
    fn volume_cells() {
        let p = PersistentVolumePlugin;
        let cells = p.cells(&pv("Bound"));
        assert_eq!(cells.len(), p.columns().len());
        assert_eq!(
            cells[1..7],
            [
                Cell::Status(Health::new(HealthStatus::Healthy, "Bound")),
                text("ns1/data"),
                text("2Gi"),
                text("standard"),
                text("Delete"),
                text("RWO,ROX"),
            ]
        );
        for (phase, status) in [
            ("Available", HealthStatus::Healthy),
            ("Released", HealthStatus::Degraded),
            ("Failed", HealthStatus::Failed),
        ] {
            assert_eq!(
                p.cells(&pv(phase))[1],
                Cell::Status(Health::new(status, phase))
            );
        }
    }

    #[test]
    fn volume_details_show_source_and_claim() {
        let o = pv("Bound");
        let sections = PersistentVolumePlugin.details(&o);
        let FieldValue::Link { label, target, .. } = field(&sections, "Claim") else {
            panic!("claim is not a link");
        };
        assert_eq!(label, "ns1/data");
        assert_eq!(target.namespace.as_deref(), Some("ns1"));
        assert_eq!(field(&sections, "Reclaim policy").text(), "Delete");
        assert_eq!(
            pairs(&sections, "Source"),
            [
                pair("Type", "CSI"),
                pair("driver", "ebs.csi.aws.com"),
                pair("volumeHandle", "vol-0abc"),
                pair("fsType", "ext4"),
                pair("volumeAttributes.zone", "eu-1a"),
            ]
        );
        let SectionBody::Related(users) = section_body(&sections, CLAIM_USERS) else {
            panic!("users is not related");
        };
        assert_eq!(users.filter, RelatedFilter::ClaimUsers("data".into()));

        let host = obj(json!({
            "apiVersion": "v1", "kind": "PersistentVolume",
            "metadata": {"name": "h"},
            "spec": {"capacity": {"storage": "1Gi"}, "hostPath": {"path": "/data", "type": ""}},
            "status": {"phase": "Available"}
        }));
        assert_eq!(
            volume_source(&host),
            Some((
                "Host path".into(),
                vec![pair("path", "/data"), pair("type", "")]
            ))
        );
        let rels = PersistentVolumePlugin.relations(&o);
        assert_eq!(rels[0].target.kind, "PersistentVolumeClaim");
        assert_eq!(rels[0].target.namespace.as_deref(), Some("ns1"));
    }

    #[test]
    fn released_volume_explains_itself() {
        let mut o = pv("Released");
        o.data["status"]["message"] = json!("claim deleted");
        assert_eq!(
            PersistentVolumePlugin.health(&o),
            Health::new(HealthStatus::Degraded, "Released: claim deleted")
        );
    }
}
