//! Secret tools for the agent (#227): metadata, checks inside Kubach,
//! comparison of two values in the cluster and, with the user's
//! confirmation of that very Secret, its values.
//!
//! Values are read here (the API returns them with every `get` and `list`),
//! used for the answer and dropped. Only [`reveal`] puts a value into its
//! output; nothing here returns a hash, a fingerprint or a prefix of one.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, StatefulSet};
use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::api::core::v1::{Pod, PodSpec, Secret};
use k8s_openapi::jiff::Timestamp;
use kubach_core::plugin::format_age;
use kubach_core::secrets::checks::{self, KeyMatch};
use kubach_core::secrets::refs::{SecretRef, Via, secret_refs};
use kubach_core::secrets::typed;
use kubach_core::secrets::value::wipe;
use kube::Client;
use kube::api::{Api, ListParams};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::policy::Allowed;
use crate::secret_write::SecretUser;
use crate::tools::{Ctx, ToolError};

/// Most Secrets listed at once.
const MAX_SECRETS: u32 = 500;
/// Most bytes of values one reveal hands out.
const MAX_REVEAL_BYTES: usize = 64 * 1024;

/// A fetched Secret's values; wiped when dropped.
struct Values {
    secret_type: String,
    data: BTreeMap<String, Vec<u8>>,
}

impl Values {
    fn of(mut secret: Secret) -> Self {
        let data = secret
            .data
            .take()
            .unwrap_or_default()
            .into_iter()
            .map(|(key, bytes)| (key, bytes.0))
            .collect();
        Self {
            secret_type: secret
                .type_
                .unwrap_or_else(|| typed::TYPE_OPAQUE.to_owned()),
            data,
        }
    }
}

impl Drop for Values {
    fn drop(&mut self) {
        for bytes in self.data.values_mut() {
            wipe(bytes);
        }
    }
}

fn json_text(value: &impl Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// One Secret of the namespace, values dropped at once.
async fn fetch(client: &Client, namespace: &str, name: &str) -> Result<Values, ToolError> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = api.get(name).await.map_err(secret_error)?;
    if secret.metadata.namespace.as_deref() != Some(namespace) {
        return Err(ToolError::Failed(format!(
            "the API returned an object outside {namespace}; refusing to use it"
        )));
    }
    Ok(Values::of(secret))
}

/// API errors without the response body, which might quote a value.
pub(crate) fn secret_error(error: kube::Error) -> ToolError {
    match error {
        kube::Error::Api(status) => ToolError::Api {
            code: Some(status.code),
            message: status.message.clone(),
        },
        kube::Error::SerdeError(_) => ToolError::Failed("cannot read the API's answer".into()),
        other => ToolError::Api {
            code: None,
            message: other.to_string(),
        },
    }
}

fn name_of(allowed: &Allowed) -> &str {
    allowed.name.as_deref().unwrap_or_default()
}

// ------------------------------------------------------------- metadata

/// A workload's pod spec, for references.
struct Workload {
    kind: &'static str,
    name: String,
    spec: PodSpec,
}

async fn list_of<K>(client: &Client, namespace: &str) -> Result<Vec<K>, kube::Error>
where
    K: kube::Resource<Scope = k8s_openapi::NamespaceResourceScope>
        + Clone
        + DeserializeOwned
        + std::fmt::Debug,
    <K as kube::Resource>::DynamicType: Default,
{
    let api: Api<K> = Api::namespaced(client.clone(), namespace);
    Ok(api.list(&ListParams::default()).await?.items)
}

fn owned(meta: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta) -> bool {
    meta.owner_references
        .as_ref()
        .is_some_and(|refs| !refs.is_empty())
}

fn in_namespace(
    meta: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
    namespace: &str,
) -> bool {
    meta.namespace.as_deref() == Some(namespace)
}

/// Pod specs of the namespace's workloads; pods and jobs that a controller
/// owns are left out (their controller is listed). Kinds the identity may
/// not list are noted.
async fn workloads(client: &Client, namespace: &str, notes: &mut Vec<String>) -> Vec<Workload> {
    let mut found = Vec::new();
    macro_rules! collect {
        ($kind:ty, $label:literal, $spec:expr, $skip_owned:expr) => {
            match list_of::<$kind>(client, namespace).await {
                Ok(items) => {
                    for item in items {
                        if !in_namespace(&item.metadata, namespace)
                            || ($skip_owned && owned(&item.metadata))
                        {
                            continue;
                        }
                        let spec: Option<PodSpec> = $spec(&item);
                        if let Some(spec) = spec {
                            found.push(Workload {
                                kind: $label,
                                name: item.metadata.name.clone().unwrap_or_default(),
                                spec,
                            });
                        }
                    }
                }
                Err(error) => notes.push(format!(
                    "{} not checked: {}",
                    $label,
                    match error {
                        kube::Error::Api(status) => status.message.clone(),
                        other => other.to_string(),
                    }
                )),
            }
        };
    }
    collect!(Pod, "Pod", |p: &Pod| p.spec.clone(), true);
    collect!(
        Deployment,
        "Deployment",
        |d: &Deployment| d.spec.as_ref().and_then(|s| s.template.spec.clone()),
        false
    );
    collect!(
        StatefulSet,
        "StatefulSet",
        |s: &StatefulSet| s.spec.as_ref().and_then(|s| s.template.spec.clone()),
        false
    );
    collect!(
        DaemonSet,
        "DaemonSet",
        |d: &DaemonSet| d.spec.as_ref().and_then(|s| s.template.spec.clone()),
        false
    );
    collect!(
        Job,
        "Job",
        |j: &Job| j.spec.as_ref().and_then(|s| s.template.spec.clone()),
        true
    );
    collect!(
        CronJob,
        "CronJob",
        |c: &CronJob| c
            .spec
            .job_template
            .spec
            .as_ref()
            .and_then(|s| s.template.spec.clone()),
        false
    );
    found
}

/// Pods and workloads of `namespace` that reference Secret `name`, for the
/// write dialogs and answers (#230), and notes on kinds the identity may
/// not list.
pub(crate) async fn users_of(
    client: &Client,
    namespace: &str,
    name: &str,
) -> (Vec<SecretUser>, Vec<String>) {
    let mut notes = Vec::new();
    let mut users = Vec::new();
    for workload in workloads(client, namespace, &mut notes).await {
        let via: Vec<String> = secret_refs(&workload.spec)
            .into_iter()
            .filter(|reference| reference.secret == name)
            .map(describe_ref)
            .collect();
        if !via.is_empty() {
            users.push(SecretUser {
                kind: workload.kind.to_owned(),
                name: workload.name,
                via,
            });
        }
    }
    (users, notes)
}

/// `env in app, key password`.
fn describe_ref(reference: SecretRef) -> String {
    let how = match reference.via {
        Via::Env => "env",
        Via::EnvFrom => "envFrom",
        Via::Volume => "volume",
        Via::Projected => "projected volume",
        Via::ImagePullSecret => "imagePullSecrets",
    };
    let mut text = format!("{how} in {}", reference.at);
    if let Some(key) = reference.key {
        text.push_str(&format!(", key {key}"));
    }
    if reference.optional {
        text.push_str(" (optional)");
    }
    text
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Use {
    kind: &'static str,
    name: String,
    #[serde(flatten)]
    reference: SecretRef,
    /// The Secret lacks this key (or does not exist).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    missing: bool,
}

/// What a list of Secrets and their users says, without values.
fn describe(
    namespace: &str,
    secrets: Vec<Secret>,
    workloads: &[Workload],
    only: Option<&str>,
    notes: Vec<String>,
) -> Value {
    let now = Timestamp::now();
    let mut keys_of: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut listed = Vec::new();
    for secret in secrets {
        let name = secret.metadata.name.clone().unwrap_or_default();
        let age = secret
            .metadata
            .creation_timestamp
            .as_ref()
            .map(|t| format_age(now.as_second() - t.0.as_second()));
        let immutable = secret.immutable.unwrap_or(false);
        let values = Values::of(secret);
        let keys: Vec<Value> = values
            .data
            .iter()
            .map(|(key, bytes)| json!({"key": key, "size": bytes.len()}))
            .collect();
        keys_of.insert(name.clone(), values.data.keys().cloned().collect());
        listed.push((name, values.secret_type.clone(), immutable, age, keys));
    }
    let mut uses: BTreeMap<String, Vec<Use>> = BTreeMap::new();
    for workload in workloads {
        for reference in secret_refs(&workload.spec) {
            if only.is_some_and(|only| only != reference.secret) {
                continue;
            }
            let missing = match (keys_of.get(&reference.secret), &reference.key) {
                (None, _) => true,
                (Some(keys), Some(key)) => !keys.contains(key),
                (Some(_), None) => false,
            };
            uses.entry(reference.secret.clone()).or_default().push(Use {
                kind: workload.kind,
                name: workload.name.clone(),
                reference,
                missing,
            });
        }
    }
    let secrets: Vec<Value> = listed
        .into_iter()
        .map(|(name, secret_type, immutable, age, keys)| {
            let used_by = uses.remove(&name).unwrap_or_default();
            json!({
                "name": name,
                "type": secret_type,
                "immutable": immutable,
                "age": age,
                "keys": keys,
                "usedBy": used_by,
            })
        })
        .collect();
    // References to Secrets that are not there at all.
    let missing: Vec<Value> = uses
        .into_iter()
        .map(|(name, used_by)| json!({"name": name, "usedBy": used_by}))
        .collect();
    json!({
        "namespace": namespace,
        "secrets": secrets,
        "missingSecrets": missing,
        "notes": notes,
        "values": "not included; use check_secret for checks done inside Kubach",
    })
}

/// `list_secrets`: names, types, keys, sizes, age and references.
pub(crate) async fn list(ctx: &Ctx<'_>, allowed: &Allowed) -> Result<String, ToolError> {
    let namespace = &allowed.namespace;
    let client = ctx.session.client_in(namespace)?;
    let secrets = match &allowed.name {
        Some(name) => {
            let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
            match api.get_opt(name).await.map_err(secret_error)? {
                Some(secret) => vec![secret],
                None => Vec::new(),
            }
        }
        None => {
            let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
            api.list(&ListParams::default().limit(MAX_SECRETS))
                .await
                .map_err(secret_error)?
                .items
        }
    };
    let secrets: Vec<Secret> = secrets
        .into_iter()
        .filter(|s| in_namespace(&s.metadata, namespace))
        .collect();
    let mut notes = Vec::new();
    let workloads = workloads(&client, namespace, &mut notes).await;
    Ok(json_text(&describe(
        namespace,
        secrets,
        &workloads,
        allowed.name.as_deref(),
        notes,
    )))
}

// ------------------------------------------------------------- checks

/// Checks of `values`; `only` limits them to one key.
fn check_values(values: &Values, only: Option<&str>, now: i64) -> Result<Value, ToolError> {
    if let Some(key) = only
        && !values.data.contains_key(key)
    {
        return Err(ToolError::Failed(format!("the Secret has no key {key:?}")));
    }
    let keys: BTreeMap<&str, checks::ValueReport> = values
        .data
        .iter()
        .filter(|(key, _)| only.is_none_or(|only| only == key.as_str()))
        .map(|(key, bytes)| (key.as_str(), checks::check_value(bytes, now)))
        .collect();
    let mut out = json!({"type": values.secret_type, "keys": keys});
    let cert = values.data.get(typed::KEY_TLS_CERT);
    let key = values.data.get(typed::KEY_TLS_KEY);
    if let (Some(cert), Some(key)) = (cert, key) {
        let matched: KeyMatch = checks::key_matches_certificate(cert, key);
        out["tlsKeyMatchesCertificate"] = json!(matched);
    }
    for (name, legacy) in [
        (typed::KEY_DOCKER_CONFIG_JSON, false),
        (typed::KEY_DOCKER_CFG, true),
    ] {
        if let Some(config) = values.data.get(name)
            && only.is_none_or(|only| only == name)
        {
            out["registries"] = match checks::registries(config, legacy) {
                Ok(list) => json!(list),
                Err(error) => json!({"error": error.to_string()}),
            };
        }
    }
    Ok(out)
}

/// `check_secret`: conclusions about the values, never the values.
pub(crate) async fn check(ctx: &Ctx<'_>, allowed: &Allowed) -> Result<String, ToolError> {
    let values = fetch(
        &ctx.session.client_in(&allowed.namespace)?,
        &allowed.namespace,
        name_of(allowed),
    )
    .await?;
    let mut out = check_values(
        &values,
        allowed.key.as_deref(),
        Timestamp::now().as_second(),
    )?;
    out["namespace"] = json!(allowed.namespace);
    out["name"] = json!(name_of(allowed));
    Ok(json_text(&out))
}

/// `compare_secret_values`: whether two values in the cluster are equal.
pub(crate) async fn compare(ctx: &Ctx<'_>, allowed: &Allowed) -> Result<String, ToolError> {
    let Some(other) = &allowed.other else {
        return Err(ToolError::Failed("nothing to compare with".into()));
    };
    let key = allowed.key.as_deref().unwrap_or_default();
    // Each namespace under its own identity (#401).
    let a = fetch(
        &ctx.session.client_in(&allowed.namespace)?,
        &allowed.namespace,
        name_of(allowed),
    )
    .await?;
    let b = fetch(
        &ctx.session.client_in(&other.namespace)?,
        &other.namespace,
        &other.name,
    )
    .await?;
    let (Some(left), Some(right)) = (a.data.get(key), b.data.get(&other.key)) else {
        return Err(ToolError::Failed(
            "one of the Secrets has no such key; list_secrets shows the keys".into(),
        ));
    };
    let equal = left == right;
    Ok(json_text(&json!({
        "first": format!("{}/{}[{key}]", allowed.namespace, name_of(allowed)),
        "second": format!("{}/{}[{}]", other.namespace, other.name, other.key),
        "equal": equal,
    })))
}

// ------------------------------------------------------------- reveal

/// The values of `values` (one key or all) for the agent: text as is,
/// anything else as base64.
fn reveal_values(values: &Values, only: Option<&str>) -> Result<Value, ToolError> {
    if let Some(key) = only
        && !values.data.contains_key(key)
    {
        return Err(ToolError::Failed(format!("the Secret has no key {key:?}")));
    }
    let mut total = 0;
    let mut data = serde_json::Map::new();
    for (key, bytes) in &values.data {
        if only.is_some_and(|only| only != key) {
            continue;
        }
        total += bytes.len();
        if total > MAX_REVEAL_BYTES {
            return Err(ToolError::Failed(format!(
                "the values exceed {MAX_REVEAL_BYTES} bytes; ask for one key"
            )));
        }
        let shown = match std::str::from_utf8(bytes) {
            Ok(text) => json!({"text": text}),
            Err(_) => json!({"base64": STANDARD.encode(bytes)}),
        };
        data.insert(key.clone(), shown);
    }
    Ok(json!({"type": values.secret_type, "data": data}))
}

/// `get_secret_value`: the values, after the user confirmed this Secret.
pub(crate) async fn reveal(ctx: &Ctx<'_>, allowed: &Allowed) -> Result<String, ToolError> {
    let values = fetch(
        &ctx.session.client_in(&allowed.namespace)?,
        &allowed.namespace,
        name_of(allowed),
    )
    .await?;
    let mut out = reveal_values(&values, allowed.key.as_deref())?;
    out["namespace"] = json!(allowed.namespace);
    out["name"] = json!(name_of(allowed));
    out["note"] = json!(
        "Revealed with the user's confirmation for this session. Do not repeat these values \
         unless needed; the user was advised to rotate the Secret afterwards."
    );
    Ok(json_text(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &[u8])]) -> Values {
        Values {
            secret_type: "Opaque".into(),
            data: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_vec()))
                .collect(),
        }
    }

    #[test]
    fn checks_carry_no_values_or_hashes() {
        let secret = values(&[
            ("password", b" hunter2\n"),
            ("url", b"postgres://app:topsecret@db:5432/shop"),
            ("config", br#"{"token": "abc123xyz"}"#),
        ]);
        let text = check_values(&secret, None, 0).unwrap().to_string();
        for leaked in ["hunter2", "topsecret", "abc123xyz", "shop", "app:"] {
            assert!(!text.contains(leaked), "{leaked} in {text}");
        }
        // Nothing that looks like a digest of a value.
        assert!(!text.contains("sha") && !text.contains("hash") && !text.contains("fingerprint"));
        assert!(text.contains("\"trailingNewline\":true"), "{text}");
        assert!(text.contains("\"host\":\"db\""), "{text}");
        assert!(check_values(&secret, Some("nope"), 0).is_err());
    }

    #[test]
    fn metadata_lists_keys_sizes_and_users() {
        let secret: Secret = serde_json::from_value(json!({
            "metadata": {"name": "db", "namespace": "web"},
            "type": "Opaque",
            "data": {"password": "aHVudGVyMg=="}
        }))
        .unwrap();
        let spec: PodSpec = serde_json::from_value(json!({
            "containers": [{"name": "app", "env": [
                {"name": "P", "valueFrom": {"secretKeyRef": {"name": "db", "key": "password"}}},
                {"name": "U", "valueFrom": {"secretKeyRef": {"name": "db", "key": "user"}}},
                {"name": "T", "valueFrom": {"secretKeyRef": {"name": "api", "key": "token"}}}
            ]}]
        }))
        .unwrap();
        let workloads = [Workload {
            kind: "Deployment",
            name: "shop".into(),
            spec,
        }];
        let out = describe("web", vec![secret], &workloads, None, Vec::new());
        let text = out.to_string();
        assert!(
            !text.contains("hunter2") && !text.contains("aHVudGVyMg"),
            "{text}"
        );
        assert_eq!(out["secrets"][0]["keys"][0]["size"], 7);
        let used = out["secrets"][0]["usedBy"].as_array().unwrap();
        assert_eq!(used.len(), 2);
        assert!(
            used.iter()
                .any(|u| u["key"] == "user" && u["missing"] == true)
        );
        assert_eq!(out["missingSecrets"][0]["name"], "api");
    }

    #[test]
    fn reveal_is_bounded_and_keeps_binary_safe() {
        let secret = values(&[("text", b"hunter2"), ("bin", &[0xff, 0x00])]);
        let out = reveal_values(&secret, None).unwrap();
        assert_eq!(out["data"]["text"]["text"], "hunter2");
        assert_eq!(out["data"]["bin"]["base64"], "/wA=");
        let one = reveal_values(&secret, Some("text")).unwrap();
        assert!(one["data"].get("bin").is_none());
        let big = values(&[("blob", &vec![b'a'; MAX_REVEAL_BYTES + 1])]);
        assert!(reveal_values(&big, None).is_err());
    }
}
