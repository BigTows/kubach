//! Encoding of the `release` key: `base64(gzip(json))` of Helm's
//! `release.Release`. Only the fields Kubach shows are decoded.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use k8s_openapi::jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use super::chart::StoredChart;
use super::{ChartInfo, HelmError, Hook, HookRun, Release, ReleaseStatus, Sensitive};

const GZIP_MAGIC: [u8; 3] = [0x1f, 0x8b, 0x08];

/// Decodes the `release` payload (the bytes after Kubernetes' own base64 of
/// the Secret data, or the ConfigMap string). Like Helm, accepts payloads that
/// are not gzipped.
pub fn decode_release(payload: &[u8]) -> Result<Release, HelmError> {
    release_from_json(&payload_json(payload)?)
}

/// Parses release JSON as `helm upgrade --output json` prints it.
pub fn release_from_json(json: &[u8]) -> Result<Release, HelmError> {
    let raw: RawRelease = serde_json::from_slice(json)?;
    Ok(raw.into())
}

/// The chart kept in a release payload, with its templates and files.
pub fn decode_chart(payload: &[u8]) -> Result<StoredChart, HelmError> {
    #[derive(Deserialize)]
    struct WithChart {
        #[serde(default)]
        chart: Option<Value>,
    }
    let release: WithChart = serde_json::from_slice(&payload_json(payload)?)?;
    StoredChart::from_json(release.chart.unwrap_or_default())
}

/// The JSON inside a payload: un-base64 and, when gzipped, gunzip.
fn payload_json(payload: &[u8]) -> Result<Vec<u8>, HelmError> {
    let compressed = STANDARD.decode(payload.trim_ascii())?;
    if !compressed.starts_with(&GZIP_MAGIC) {
        return Ok(compressed);
    }
    let mut json = Vec::new();
    GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut json)
        .map_err(HelmError::Gzip)?;
    Ok(json)
}

/// Encodes release JSON the way Helm stores it. Used by tests and fixtures.
pub fn encode_release(json: &Value) -> String {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let bytes = serde_json::to_vec(json).expect("JSON value serializes");
    encoder.write_all(&bytes).expect("writing to Vec");
    STANDARD.encode(encoder.finish().expect("writing to Vec"))
}

#[derive(Deserialize)]
struct RawRelease {
    #[serde(default)]
    name: String,
    #[serde(default)]
    info: Option<RawInfo>,
    #[serde(default)]
    chart: Option<RawChart>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    manifest: String,
    #[serde(default)]
    hooks: Option<Vec<RawHook>>,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    labels: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize, Default)]
struct RawInfo {
    #[serde(default)]
    first_deployed: String,
    #[serde(default)]
    last_deployed: String,
    #[serde(default)]
    deleted: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    notes: String,
}

#[derive(Deserialize, Default)]
struct RawChart {
    #[serde(default)]
    metadata: Option<RawMetadata>,
    #[serde(default)]
    values: Option<Value>,
}

#[derive(Deserialize, Default)]
struct RawMetadata {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default, rename = "appVersion")]
    app_version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    home: Option<String>,
    #[serde(default)]
    sources: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RawHook {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    manifest: String,
    #[serde(default)]
    events: Option<Vec<String>>,
    #[serde(default)]
    last_run: Option<RawHookRun>,
    #[serde(default)]
    weight: i64,
    #[serde(default)]
    delete_policies: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RawHookRun {
    #[serde(default)]
    started_at: String,
    #[serde(default)]
    completed_at: String,
    #[serde(default)]
    phase: String,
}

/// Go's zero time, `0001-01-01T00:00:00Z`, in Unix seconds.
const GO_ZERO_TIME: i64 = -62_135_596_800;

/// Helm writes zero times as `""` (older versions as Go's zero time);
/// anything unparsable is treated as absent.
fn timestamp(value: &str) -> Option<Timestamp> {
    value
        .parse::<Timestamp>()
        .ok()
        .filter(|t| t.as_second() > GO_ZERO_TIME)
}

/// `null` values (as Helm writes empty maps) become `{}`.
fn object_or_empty(value: Option<Value>) -> Value {
    match value {
        Some(value @ Value::Object(_)) => value,
        _ => Value::Object(Default::default()),
    }
}

impl From<RawRelease> for Release {
    fn from(raw: RawRelease) -> Self {
        let info = raw.info.unwrap_or_default();
        let chart = raw.chart.unwrap_or_default();
        let metadata = chart.metadata.unwrap_or_default();
        let hooks = raw
            .hooks
            .unwrap_or_default()
            .into_iter()
            .map(|hook| Hook {
                name: hook.name,
                kind: hook.kind,
                path: hook.path,
                events: hook.events.unwrap_or_default(),
                weight: hook.weight,
                delete_policies: hook.delete_policies.unwrap_or_default(),
                last_run: hook
                    .last_run
                    .filter(|run| !run.phase.is_empty())
                    .map(|run| HookRun {
                        started_at: timestamp(&run.started_at),
                        completed_at: timestamp(&run.completed_at),
                        phase: run.phase,
                    }),
                manifest: Sensitive::new(hook.manifest),
            })
            .collect();
        Release {
            name: raw.name,
            namespace: raw.namespace,
            revision: raw.version,
            status: ReleaseStatus::parse(&info.status),
            chart: ChartInfo {
                name: metadata.name,
                version: metadata.version,
                app_version: metadata.app_version.filter(|v| !v.is_empty()),
                description: metadata.description.filter(|v| !v.is_empty()),
                icon: metadata.icon.filter(|v| !v.trim().is_empty()),
                home: metadata.home.filter(|v| !v.trim().is_empty()),
                sources: metadata
                    .sources
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|v| !v.trim().is_empty())
                    .collect(),
            },
            first_deployed: timestamp(&info.first_deployed),
            last_deployed: timestamp(&info.last_deployed),
            deleted: timestamp(&info.deleted),
            description: info.description,
            notes: info.notes,
            user_values: Sensitive::new(object_or_empty(raw.config)),
            chart_values: Sensitive::new(object_or_empty(chart.values)),
            manifest: Sensitive::new(raw.manifest),
            hooks,
            labels: raw.labels.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;

    /// A release as Helm serializes it (trimmed to realistic fields).
    pub(crate) fn release_json(revision: u32, status: &str, replicas: u32) -> Value {
        json!({
            "name": "web",
            "namespace": "apps",
            "version": revision,
            "info": {
                "first_deployed": "2026-10-01T10:00:00.123456789+03:00",
                "last_deployed": format!("2026-10-0{revision}T12:00:00Z"),
                "deleted": "",
                "description": if revision == 1 { "Install complete" } else { "Upgrade complete" },
                "status": status,
                "notes": "Visit http://web.local\n"
            },
            "chart": {
                "metadata": {
                    "name": "nginx",
                    "version": format!("1.{revision}.0"),
                    "appVersion": "1.27.0",
                    "description": "A web server",
                    "icon": "https://example.com/nginx.svg",
                    "home": "https://nginx.org",
                    "sources": ["https://github.com/nginx/nginx", ""],
                    "apiVersion": "v2"
                },
                "lock": null,
                "templates": [{"name": "templates/deployment.yaml", "data": "a2luZDogRGVwbG95bWVudA=="}],
                "values": {
                    "replicaCount": 1,
                    "image": {"repository": "nginx", "tag": "stable"},
                    "service": {"type": "ClusterIP", "port": 80}
                },
                "schema": null,
                "files": []
            },
            "config": {"replicaCount": replicas, "auth": {"password": "hunter2"}},
            "manifest": format!("---\n# Source: nginx/templates/deployment.yaml\nkind: Deployment\nspec:\n  replicas: {replicas}\n"),
            "hooks": [{
                "name": "web-test",
                "kind": "Pod",
                "path": "nginx/templates/tests/test.yaml",
                "manifest": "kind: Pod\n",
                "events": ["test"],
                "last_run": {"started_at": "", "completed_at": "", "phase": ""},
                "weight": -5,
                "delete_policies": ["before-hook-creation"]
            }]
        })
    }

    #[test]
    fn decodes_gzipped_release() {
        let payload = encode_release(&release_json(2, "deployed", 3));
        let release = decode_release(payload.as_bytes()).unwrap();

        assert_eq!(release.name, "web");
        assert_eq!(release.namespace, "apps");
        assert_eq!(release.revision, 2);
        assert_eq!(release.status, ReleaseStatus::Deployed);
        assert_eq!(release.chart.reference(), "nginx-1.2.0");
        assert_eq!(release.chart.app_version.as_deref(), Some("1.27.0"));
        assert_eq!(release.chart.home.as_deref(), Some("https://nginx.org"));
        assert_eq!(release.chart.sources, ["https://github.com/nginx/nginx"]);
        assert_eq!(
            release.chart.icon.as_deref(),
            Some("https://example.com/nginx.svg")
        );
        assert_eq!(release.description, "Upgrade complete");
        assert_eq!(release.notes, "Visit http://web.local\n");
        assert_eq!(
            release.last_deployed.unwrap().to_string(),
            "2026-10-02T12:00:00Z"
        );
        assert!(release.first_deployed.is_some());
        assert_eq!(release.deleted, None);
        assert_eq!(release.user_values.expose()["replicaCount"], 3);
        assert_eq!(release.chart_values.expose()["image"]["tag"], "stable");
        assert!(release.manifest.expose().contains("replicas: 3"));

        let hook = &release.hooks[0];
        assert_eq!(hook.events, ["test"]);
        assert_eq!(hook.weight, -5);
        assert_eq!(hook.last_run, None);
    }

    #[test]
    fn decodes_the_stored_chart() {
        let payload = encode_release(&release_json(2, "deployed", 3));
        let chart = decode_chart(payload.as_bytes()).unwrap();
        assert_eq!(chart.metadata()["name"], "nginx");
        assert_eq!(chart.templates().len(), 1);
        assert_eq!(chart.templates()[0].data, b"kind: Deployment");
        assert!(!chart.has_dependencies());
    }

    #[test]
    fn parses_helm_output_json() {
        let json = serde_json::to_vec(&release_json(4, "pending-upgrade", 2)).unwrap();
        let release = release_from_json(&json).unwrap();
        assert_eq!(release.revision, 4);
        assert_eq!(release.status, ReleaseStatus::PendingUpgrade);
        assert!(release.manifest.expose().contains("replicas: 2"));
    }

    #[test]
    fn decodes_uncompressed_release() {
        let json = serde_json::to_vec(&release_json(1, "superseded", 1)).unwrap();
        let release = decode_release(STANDARD.encode(json).as_bytes()).unwrap();
        assert_eq!(release.status, ReleaseStatus::Superseded);
    }

    #[test]
    fn null_config_becomes_empty_object() {
        let mut json = release_json(1, "deployed", 1);
        json["config"] = Value::Null;
        json["hooks"] = Value::Null;
        let release = decode_release(encode_release(&json).as_bytes()).unwrap();
        assert_eq!(release.user_values.expose(), &json!({}));
        assert!(release.hooks.is_empty());
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            decode_release(b"not base64!"),
            Err(HelmError::Base64(_))
        ));
        let truncated = STANDARD.encode([0x1f, 0x8b, 0x08, 0x00]);
        assert!(matches!(
            decode_release(truncated.as_bytes()),
            Err(HelmError::Gzip(_))
        ));
        assert!(matches!(
            decode_release(STANDARD.encode("{").as_bytes()),
            Err(HelmError::Json(_))
        ));
    }

    #[test]
    fn sensitive_debug_is_redacted() {
        let release =
            decode_release(encode_release(&release_json(1, "deployed", 1)).as_bytes()).unwrap();
        let debug = format!("{release:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("Sensitive(<redacted>)"));
    }

    #[test]
    fn unknown_status_is_kept() {
        assert_eq!(
            ReleaseStatus::parse("pending-upgrade"),
            ReleaseStatus::PendingUpgrade
        );
        assert_eq!(ReleaseStatus::parse("weird").as_str(), "weird");
        assert!(ReleaseStatus::PendingRollback.is_pending());
    }
}
