//! Writing Secrets through the user (#230, ADR 0005 "Запись Secrets через
//! пользователя").
//!
//! The agent asks Kubach to create, update, copy or delete a Secret, but it
//! never handles a value: for a create or an update Kubach shows its own
//! form in the app's window ([`RequestKind::Secret`]), the user types,
//! generates or picks the values from files, and the answer
//! ([`ConfirmationDecision::Provided`]) carries them to Kubach only. Copying
//! happens inside Kubach. The agent gets the keys, the sizes, the operation
//! and the workloads that use the Secret; the audit log gets the keys and
//! the operation. This is not MCP elicitation, which would route the input
//! through the agent's client and into the model's context.
//!
//! Values live in [`SecretBytes`] (hidden from `Debug`, wiped on drop), and
//! the buffers Kubach builds for a write are wiped once it is sent.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kubach_core::secrets::checks::{self, KeyMatch};
use kubach_core::secrets::typed;
use kubach_core::secrets::value::{SecretBytes, wipe};
use kube::Client;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams, Preconditions};
use serde::ser::{SerializeMap as _, Serializer};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};

use crate::confirm::{ConfirmationDecision, ConfirmationRequest, RequestKind, ask};
use crate::policy::{Allowed, Denial, DenialCode};
use crate::secret_tools::{secret_error, users_of};
use crate::tools::{Ctx, FIELD_MANAGER, ToolError, ToolOutput, parse};

/// Most bytes of values in one Secret (the API server's limit for a whole
/// object is 1 MiB).
pub const MAX_SECRET_BYTES: usize = 1_000_000;
/// Most keys the agent may name in one request.
pub const MAX_KEYS: usize = 32;
/// Longest explanation the agent may give the user, in characters.
pub const MAX_EXPLANATION_CHARS: usize = 1000;
/// Set on a copy: where it came from.
pub const COPIED_FROM_ANNOTATION: &str = "kubach.io/copied-from";
/// Types a Secret may be created with: the forms Kubach has.
pub const CREATE_TYPES: [&str; 3] = [
    typed::TYPE_OPAQUE,
    typed::TYPE_TLS,
    typed::TYPE_DOCKER_CONFIG_JSON,
];
/// Never copied: the token controller would bind them to a ServiceAccount
/// of the target namespace.
const SERVICE_ACCOUNT_TOKEN: &str = "kubernetes.io/service-account-token";

/// What the agent wants done with a Secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretOp {
    /// A new Secret; the user enters every value.
    Create,
    /// Replace, add or remove keys of an existing Secret; the user enters
    /// the new values.
    Update,
    /// Copy a Secret into another namespace or name, inside Kubach.
    Copy,
    /// Delete a Secret.
    Delete,
}

impl SecretOp {
    /// The answer carries values ([`ConfirmationDecision::Provided`]).
    pub fn takes_values(self) -> bool {
        matches!(self, Self::Create | Self::Update)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Copy => "copy",
            Self::Delete => "delete",
        }
    }
}

/// A key and the size of its value; never the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySize {
    pub key: String,
    pub size: usize,
}

/// A workload or pod that uses the Secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretUser {
    pub kind: String,
    pub name: String,
    /// How: `env DB_PASSWORD (key password)`, `volume certs`,
    /// `imagePullSecrets`.
    pub via: Vec<String>,
}

/// The Secret a copy starts from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretSource {
    pub namespace: String,
    pub name: String,
}

/// What the user is asked about a Secret, shown in Kubach's own dialog.
/// `name`, `keys`, `remove` and `explanation` come from the agent: the UI
/// marks them as unverified, because a prompt-injected agent may ask for
/// a production token here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRequest {
    pub op: SecretOp,
    /// The Secret written or deleted (the target of a copy).
    pub name: String,
    /// `Opaque`, `kubernetes.io/tls`, `kubernetes.io/dockerconfigjson` or,
    /// for an update, whatever the Secret has.
    pub secret_type: String,
    /// Keys the agent asks values for (create) or wants set (update); the
    /// form starts with them, the user decides.
    #[serde(default)]
    pub keys: Vec<String>,
    /// Keys the agent suggests removing (update); not removed unless the
    /// user does it.
    #[serde(default)]
    pub remove: Vec<String>,
    /// The Secret's keys now (update, delete; the source of a copy).
    #[serde(default)]
    pub existing: Vec<KeySize>,
    #[serde(default)]
    pub source: Option<SecretSource>,
    /// Pods and workloads that reference the Secret.
    #[serde(default)]
    pub used_by: Vec<SecretUser>,
    /// Why, in the agent's words.
    #[serde(default)]
    pub explanation: String,
}

/// The user's answer to a create or update form: values for keys to set
/// and existing keys to remove.
///
/// `Debug` shows keys and sizes only. It serializes (base64) only to cross
/// the bridge from the app to a `kubach mcp` process, which writes the
/// Secret; nothing else serializes it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretInput {
    /// All keys on create; added or replaced keys on update.
    pub set: BTreeMap<String, SecretBytes>,
    /// Existing keys to remove (update only).
    pub remove: BTreeSet<String>,
}

impl SecretInput {
    pub fn total_bytes(&self) -> usize {
        self.set.values().map(SecretBytes::len).sum()
    }
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set: BTreeMap<&str, String> = self
            .set
            .iter()
            .map(|(key, value)| (key.as_str(), format!("<hidden, {} bytes>", value.len())))
            .collect();
        f.debug_struct("SecretInput")
            .field("set", &set)
            .field("remove", &self.remove)
            .finish()
    }
}

impl Serialize for SecretInput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Encoded<'a>(&'a BTreeMap<String, SecretBytes>);
        impl Serialize for Encoded<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(self.0.len()))?;
                for (key, value) in self.0 {
                    let mut text = STANDARD.encode(value.expose());
                    let result = map.serialize_entry(key, &text);
                    // Wipe the encoded copy.
                    let mut bytes = std::mem::take(&mut text).into_bytes();
                    wipe(&mut bytes);
                    result?;
                }
                map.end()
            }
        }
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("set", &Encoded(&self.set))?;
        map.serialize_entry("remove", &self.remove)?;
        map.end()
    }
}

/// One base64 value on the wire.
struct Encoded(SecretBytes);

impl<'de> Deserialize<'de> for Encoded {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut bytes = String::deserialize(deserializer)?.into_bytes();
        let decoded = STANDARD.decode(&bytes);
        wipe(&mut bytes);
        decoded
            .map(|value| Self(SecretBytes::new(value)))
            .map_err(|_| serde::de::Error::custom("a Secret value is not valid base64"))
    }
}

impl<'de> Deserialize<'de> for SecretInput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            set: BTreeMap<String, Encoded>,
            #[serde(default)]
            remove: BTreeSet<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            set: wire
                .set
                .into_iter()
                .map(|(key, value)| (key, value.0))
                .collect(),
            remove: wire.remove,
        })
    }
}

/// Whether `input` is a valid answer to `request`. Messages name keys,
/// never values. The app runs the same check before it answers.
pub fn check_input(request: &SecretRequest, input: &SecretInput) -> Result<(), String> {
    let existing: BTreeSet<&str> = request.existing.iter().map(|k| k.key.as_str()).collect();
    for key in input.set.keys() {
        kubach_core::secrets::validate_key(key).map_err(|e| e.to_string())?;
    }
    if input.total_bytes() > MAX_SECRET_BYTES {
        return Err(format!(
            "the values exceed {MAX_SECRET_BYTES} bytes, the limit of a Secret"
        ));
    }
    match request.op {
        SecretOp::Create => {
            if !input.remove.is_empty() {
                return Err("a new Secret has no keys to remove".into());
            }
            if input.set.is_empty() {
                return Err("enter at least one value".into());
            }
        }
        SecretOp::Update => {
            if input.set.is_empty() && input.remove.is_empty() {
                return Err("nothing to change".into());
            }
            if let Some(key) = input.remove.iter().find(|k| !existing.contains(k.as_str())) {
                return Err(format!("the Secret has no key {key} to remove"));
            }
            if let Some(key) = input.set.keys().find(|k| input.remove.contains(*k)) {
                return Err(format!("key {key} is both set and removed"));
            }
        }
        SecretOp::Copy | SecretOp::Delete => return Err("this request takes no values".into()),
    }
    // Keys the Secret ends up with.
    let mut keys: BTreeSet<&str> = if request.op == SecretOp::Update {
        existing.clone()
    } else {
        BTreeSet::new()
    };
    keys.extend(input.set.keys().map(String::as_str));
    for key in &input.remove {
        keys.remove(key.as_str());
    }
    match request.secret_type.as_str() {
        typed::TYPE_TLS => {
            for key in [typed::KEY_TLS_CERT, typed::KEY_TLS_KEY] {
                if !keys.contains(key) {
                    return Err(format!("a TLS Secret needs {key}"));
                }
            }
            let cert = input.set.get(typed::KEY_TLS_CERT);
            let key = input.set.get(typed::KEY_TLS_KEY);
            match (cert, key) {
                (Some(cert), Some(key)) => {
                    typed::certificates(cert.expose()).map_err(|e| e.to_string())?;
                    if !typed::has_private_key(key.expose()) {
                        return Err("tls.key holds no PEM private key".into());
                    }
                    match checks::key_matches_certificate(cert.expose(), key.expose()) {
                        KeyMatch::Matches => {}
                        KeyMatch::Differs => {
                            return Err(
                                "tls.key does not belong to the certificate in tls.crt".into()
                            );
                        }
                        KeyMatch::Unknown => {
                            return Err(
                                "Kubach could not tell whether tls.key belongs to tls.crt".into()
                            );
                        }
                    }
                }
                (None, None) => {}
                _ => return Err("replace tls.crt and tls.key together".into()),
            }
        }
        typed::TYPE_DOCKER_CONFIG_JSON => {
            if !keys.contains(typed::KEY_DOCKER_CONFIG_JSON) {
                return Err(format!(
                    "a registry Secret needs {}",
                    typed::KEY_DOCKER_CONFIG_JSON
                ));
            }
            if let Some(config) = input.set.get(typed::KEY_DOCKER_CONFIG_JSON) {
                let registries =
                    checks::registries(config.expose(), false).map_err(|e| e.to_string())?;
                if registries.is_empty() {
                    return Err("the registry config names no registry".into());
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// ------------------------------------------------------------- arguments

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InputArgs {
    pub namespace: Option<String>,
    pub name: String,
    pub mode: SecretOp,
    #[serde(rename = "type")]
    pub secret_type: Option<String>,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub remove_keys: Vec<String>,
    #[serde(default)]
    pub explanation: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CopyArgs {
    pub namespace: Option<String>,
    pub name: String,
    pub to_namespace: Option<String>,
    pub to_name: Option<String>,
    #[serde(default)]
    pub explanation: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeleteArgs {
    pub namespace: Option<String>,
    pub name: String,
    #[serde(default)]
    pub explanation: String,
}

/// The agent's explanation: bounded, and free of characters that would
/// hide text from the user.
pub(crate) fn check_explanation(text: &str) -> Result<(), Denial> {
    if text.chars().count() > MAX_EXPLANATION_CHARS {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!("explanation is longer than {MAX_EXPLANATION_CHARS} characters"),
        ));
    }
    if let Some(c) = text
        .chars()
        .find(|&c| c != '\n' && crate::policy::hidden_in_command(c))
    {
        return Err(Denial::new(
            DenialCode::InvalidInput,
            format!(
                "explanation contains U+{:04X}: control, invisible and bidi characters are not \
                 allowed",
                u32::from(c)
            ),
        ));
    }
    Ok(())
}

// ------------------------------------------------------------- reading

/// What a Secret is now, without its values.
struct Current {
    secret_type: String,
    immutable: bool,
    uid: Option<String>,
    resource_version: Option<String>,
    keys: Vec<KeySize>,
}

/// Wipes the values of a Secret Kubach built or the API returned.
fn wipe_secret(secret: &mut Secret) {
    for value in secret.data.iter_mut().flat_map(|data| data.values_mut()) {
        wipe(&mut value.0);
    }
    for value in secret
        .string_data
        .iter_mut()
        .flat_map(|data| data.values_mut())
    {
        let mut bytes = std::mem::take(value).into_bytes();
        wipe(&mut bytes);
    }
}

/// The Secret's values as [`SecretBytes`]; the rest of it is dropped.
fn take_values(secret: &mut Secret) -> BTreeMap<String, SecretBytes> {
    secret
        .data
        .take()
        .unwrap_or_default()
        .into_iter()
        .map(|(key, bytes)| (key, SecretBytes::new(bytes.0)))
        .collect()
}

fn current_of(mut secret: Secret) -> Current {
    let values = take_values(&mut secret);
    wipe_secret(&mut secret);
    Current {
        secret_type: secret
            .type_
            .unwrap_or_else(|| typed::TYPE_OPAQUE.to_owned()),
        immutable: secret.immutable.unwrap_or(false),
        uid: secret.metadata.uid,
        resource_version: secret.metadata.resource_version,
        keys: values
            .iter()
            .map(|(key, value)| KeySize {
                key: key.clone(),
                size: value.len(),
            })
            .collect(),
    }
}

async fn fetch_opt(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<Option<Secret>, ToolError> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = api.get_opt(name).await.map_err(secret_error)?;
    match secret {
        Some(mut secret) if secret.metadata.namespace.as_deref() != Some(namespace) => {
            wipe_secret(&mut secret);
            Err(ToolError::Failed(format!(
                "the API returned an object outside {namespace}; refusing to use it"
            )))
        }
        other => Ok(other),
    }
}

async fn current(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<Option<Current>, ToolError> {
    Ok(fetch_opt(client, namespace, name).await?.map(current_of))
}

// ------------------------------------------------------------- writing

fn post_params() -> PostParams {
    PostParams {
        field_manager: Some(FIELD_MANAGER.into()),
        ..PostParams::default()
    }
}

/// Creates the Secret with `values` (POST: fails if it exists).
async fn create(
    client: &Client,
    namespace: &str,
    name: &str,
    secret_type: &str,
    values: &BTreeMap<String, SecretBytes>,
    annotations: Option<BTreeMap<String, String>>,
) -> Result<(), ToolError> {
    let mut secret = Secret {
        metadata: ObjectMeta {
            name: Some(name.to_owned()),
            namespace: Some(namespace.to_owned()),
            annotations,
            ..ObjectMeta::default()
        },
        type_: Some(secret_type.to_owned()),
        data: Some(
            values
                .iter()
                .map(|(key, value)| (key.clone(), ByteString(value.expose().to_vec())))
                .collect(),
        ),
        ..Secret::default()
    };
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let result = api.create(&post_params(), &secret).await;
    wipe_secret(&mut secret);
    let mut created = result.map_err(secret_error)?;
    wipe_secret(&mut created);
    Ok(())
}

/// Sets and removes keys with a merge patch naming only those keys, as the
/// UI does (ADR 0010): keys left alone are not sent. `resource_version`
/// makes the server refuse the patch if the Secret changed while the user
/// was typing.
async fn patch(
    client: &Client,
    namespace: &str,
    name: &str,
    input: &SecretInput,
    resource_version: Option<&str>,
) -> Result<(), ToolError> {
    let mut data = Map::new();
    for (key, value) in &input.set {
        data.insert(key.clone(), Value::String(STANDARD.encode(value.expose())));
    }
    for key in &input.remove {
        data.insert(key.clone(), Value::Null);
    }
    let mut body = json!({ "data": data });
    if let Some(version) = resource_version {
        body["metadata"] = json!({ "resourceVersion": version });
    }
    let params = PatchParams {
        field_manager: Some(FIELD_MANAGER.into()),
        ..PatchParams::default()
    };
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let result = api.patch(name, &params, &Patch::Merge(&body)).await;
    if let Some(Value::Object(data)) = body.get_mut("data") {
        for value in data.values_mut() {
            if let Value::String(text) = value {
                let mut bytes = std::mem::take(text).into_bytes();
                wipe(&mut bytes);
            }
        }
    }
    let mut patched = result.map_err(|error| match &error {
        kube::Error::Api(status) if status.code == 409 => ToolError::Api {
            code: Some(409),
            message: "the Secret changed in the cluster while the user was entering values; \
                      nothing was written"
                .into(),
        },
        _ => secret_error(error),
    })?;
    wipe_secret(&mut patched);
    Ok(())
}

// ------------------------------------------------------------- the user

/// Asks the user about `secret` and returns their answer.
async fn ask_user(
    ctx: &Ctx<'_>,
    namespace: &str,
    summary: String,
    secret: SecretRequest,
) -> Result<ConfirmationDecision, ToolError> {
    let request = ConfirmationRequest {
        kind: RequestKind::Secret {
            secret: Box::new(secret),
        },
        session_id: ctx.session.id().to_owned(),
        context: ctx.session.scope().context.clone(),
        client: ctx.client_name.to_owned(),
        tool: ctx.tool.to_owned(),
        namespace: namespace.to_owned(),
        summary,
        details: String::new(),
        dangerous: Vec::new(),
        mode: ctx.session.mode_in(namespace),
        // Entering values is the confirmation of one write (#230).
        allow_remember: false,
    };
    let decision = ask(ctx.confirmer, &request).await;
    if !ctx.session.is_active() {
        return Err(Denial::new(
            DenialCode::SessionEnded,
            "the session ended while waiting for the user",
        )
        .into());
    }
    Ok(decision)
}

/// A "no" in words the agent can act on.
fn refused(decision: ConfirmationDecision, what: &str) -> ToolError {
    match decision {
        ConfirmationDecision::Rejected { reason } => ToolError::Rejected(Some(format!(
            "the user declined to {what} in the Kubach window{}; nothing was written. Do not \
             ask again unless the user wants it",
            reason
                .filter(|r| !r.is_empty())
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        ))),
        ConfirmationDecision::Denied { reason } => ToolError::Rejected(Some(format!(
            "no answer from the user to {what}{}; nothing was written",
            reason.map(|r| format!(" ({r})")).unwrap_or_default()
        ))),
        ConfirmationDecision::Approved
        | ConfirmationDecision::ApprovedAlways
        | ConfirmationDecision::ApprovedForSession
        | ConfirmationDecision::Provided { .. } => {
            ToolError::Failed(format!("unexpected answer to {what}; nothing was written"))
        }
    }
}

/// Lets a temporary ServiceAccount patch and delete this one Secret
/// (`resourceNames`), once the user said yes to a write.
async fn allow_on_server(ctx: &Ctx<'_>, namespace: &str, name: &str) -> Result<(), ToolError> {
    ctx.session
        .allow_secret_write(namespace, name)
        .await
        .map_err(|error| {
            ToolError::Failed(format!(
                "the user approved the change, but Kubach could not allow it on the server: \
                 {error}; nothing was written"
            ))
        })
}

fn name_of(allowed: &Allowed) -> &str {
    allowed.name.as_deref().unwrap_or_default()
}

fn keys_json(keys: &[KeySize]) -> Value {
    json!(
        keys.iter()
            .map(|k| json!({"key": k.key, "size": k.size}))
            .collect::<Vec<_>>()
    )
}

const VALUES_NOTE: &str = "The values were entered or generated by the user in Kubach and \
     written by Kubach. They are not available to you and are not part of this answer.";

const RESTART_NOTE: &str = "Pods read Secret values when they start (env) or with a delay \
     (volumes): rollout_restart the workloads in usedBy if they must pick up the change now.";

// ------------------------------------------------------------- tools

/// `request_secret_input`: the user enters the values of a new Secret or
/// the changes to an existing one in Kubach's window.
pub(crate) async fn input(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: InputArgs = parse(args)?;
    let namespace = allowed.namespace.as_str();
    let name = name_of(allowed);
    let client = ctx.session.client_in(namespace)?;
    let now = current(&client, namespace, name).await?;
    let (secret_type, existing, resource_version) = match (a.mode, now) {
        (SecretOp::Create, Some(_)) => {
            return Err(ToolError::Failed(format!(
                "Secret {namespace}/{name} already exists; use mode \"update\" to change it"
            )));
        }
        (SecretOp::Create, None) => (
            a.secret_type
                .clone()
                .unwrap_or_else(|| typed::TYPE_OPAQUE.to_owned()),
            Vec::new(),
            None,
        ),
        (SecretOp::Update, None) => {
            return Err(ToolError::Failed(format!(
                "Secret {namespace}/{name} does not exist; use mode \"create\""
            )));
        }
        (SecretOp::Update, Some(now)) if now.immutable => {
            return Err(ToolError::Failed(format!(
                "Secret {namespace}/{name} is immutable: its data cannot change. Delete it and \
                 create it again"
            )));
        }
        (SecretOp::Update, Some(now)) => (now.secret_type, now.keys, now.resource_version),
        (op, _) => {
            return Err(ToolError::Failed(format!(
                "mode {:?} is not a mode of request_secret_input",
                op.as_str()
            )));
        }
    };
    let (used_by, _) = users_of(&client, namespace, name).await;
    let request = SecretRequest {
        op: a.mode,
        name: name.to_owned(),
        secret_type: secret_type.clone(),
        keys: a.keys.clone(),
        remove: a.remove_keys.clone(),
        existing: existing.clone(),
        source: None,
        used_by: used_by.clone(),
        explanation: a.explanation.clone(),
    };
    let what = match a.mode {
        SecretOp::Create => format!("enter values for the new Secret {namespace}/{name}"),
        _ => format!("change Secret {namespace}/{name}"),
    };
    let summary = format!("{} asks you to {what}", ctx.client_name);
    let decision = ask_user(ctx, namespace, summary, request.clone()).await?;
    let ConfirmationDecision::Provided { input } = decision else {
        return Err(refused(decision, &what));
    };
    check_input(&request, &input).map_err(|message| {
        ToolError::Failed(format!(
            "the values from the form were not accepted: {message}"
        ))
    })?;
    let sizes: BTreeMap<&str, usize> = existing.iter().map(|k| (k.key.as_str(), k.size)).collect();
    if a.mode == SecretOp::Update {
        allow_on_server(ctx, namespace, name).await?;
        patch(
            &client,
            namespace,
            name,
            &input,
            resource_version.as_deref(),
        )
        .await?;
    } else {
        create(&client, namespace, name, &secret_type, &input.set, None).await?;
    }
    let keys: Vec<Value> = input
        .set
        .iter()
        .map(|(key, value)| {
            json!({
                "key": key,
                "size": value.len(),
                "change": if sizes.contains_key(key.as_str()) { "replaced" } else { "added" },
            })
        })
        .collect();
    let removed: Vec<&String> = input.remove.iter().collect();
    let mut out = json!({
        "namespace": namespace,
        "name": name,
        "operation": if a.mode == SecretOp::Create { "created" } else { "updated" },
        "type": secret_type,
        "keys": keys,
        "removed": removed,
        "usedBy": used_by,
        "values": VALUES_NOTE,
    });
    if a.mode == SecretOp::Update && !used_by.is_empty() {
        out["next"] = json!(RESTART_NOTE);
    }
    let audit = json!({
        "operation": a.mode.as_str(),
        "namespace": namespace,
        "name": name,
        "type": secret_type,
        "keys": input.set.keys().collect::<Vec<_>>(),
        "removed": removed,
    });
    Ok(ToolOutput {
        text: serde_json::to_string_pretty(&out).unwrap_or_default(),
        confirmed: true,
        audit: Some(audit),
    })
}

/// `copy_secret`: Kubach copies the data and type of a Secret into another
/// namespace or name; the agent sees keys and sizes only.
pub(crate) async fn copy(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: CopyArgs = parse(args)?;
    let from_ns = allowed.namespace.as_str();
    let from = name_of(allowed);
    let Some(to) = allowed.to.as_ref() else {
        return Err(ToolError::Failed("no target for the copy".into()));
    };
    // Read under the source's identity, written under the target's (#401).
    let client = ctx.session.client_in(from_ns)?;
    let target = ctx.session.client_in(&to.namespace)?;
    let Some(mut source) = fetch_opt(&client, from_ns, from).await? else {
        return Err(ToolError::Failed(format!(
            "Secret {from_ns}/{from} does not exist"
        )));
    };
    let values = take_values(&mut source);
    wipe_secret(&mut source);
    let secret_type = source
        .type_
        .clone()
        .unwrap_or_else(|| typed::TYPE_OPAQUE.to_owned());
    if secret_type == SERVICE_ACCOUNT_TOKEN {
        return Err(ToolError::Failed(
            "ServiceAccount token Secrets are not copied: they belong to a ServiceAccount".into(),
        ));
    }
    if current(&target, &to.namespace, &to.name).await?.is_some() {
        return Err(ToolError::Failed(format!(
            "Secret {}/{} already exists; Kubach does not overwrite it with a copy",
            to.namespace, to.name
        )));
    }
    let keys: Vec<KeySize> = values
        .iter()
        .map(|(key, value)| KeySize {
            key: key.clone(),
            size: value.len(),
        })
        .collect();
    let request = SecretRequest {
        op: SecretOp::Copy,
        name: to.name.clone(),
        secret_type: secret_type.clone(),
        keys: Vec::new(),
        remove: Vec::new(),
        existing: keys.clone(),
        source: Some(SecretSource {
            namespace: from_ns.to_owned(),
            name: from.to_owned(),
        }),
        used_by: Vec::new(),
        explanation: a.explanation.clone(),
    };
    let what = format!(
        "copy Secret {from_ns}/{from} to {}/{}",
        to.namespace, to.name
    );
    let summary = format!("{} asks to {what}", ctx.client_name);
    let decision = ask_user(ctx, &to.namespace, summary, request).await?;
    if !decision.is_approved() {
        return Err(refused(decision, &what));
    }
    let annotations = BTreeMap::from([(
        COPIED_FROM_ANNOTATION.to_owned(),
        format!("{from_ns}/{from}"),
    )]);
    create(
        &target,
        &to.namespace,
        &to.name,
        &secret_type,
        &values,
        Some(annotations),
    )
    .await?;
    let out = json!({
        "from": format!("{from_ns}/{from}"),
        "namespace": to.namespace,
        "name": to.name,
        "operation": "copied",
        "type": secret_type,
        "keys": keys_json(&keys),
        "values": "Copied inside Kubach; not part of this answer.",
        "dropped": "ownerReferences, labels, annotations (including last-applied-configuration)",
    });
    let audit = json!({
        "operation": "copy",
        "from": format!("{from_ns}/{from}"),
        "namespace": to.namespace,
        "name": to.name,
        "type": secret_type,
        "keys": keys.iter().map(|k| &k.key).collect::<Vec<_>>(),
    });
    Ok(ToolOutput {
        text: serde_json::to_string_pretty(&out).unwrap_or_default(),
        confirmed: true,
        audit: Some(audit),
    })
}

/// `delete_secret`: confirmed every time; the user sees which pods and
/// workloads reference the Secret.
pub(crate) async fn delete(
    ctx: &Ctx<'_>,
    allowed: &Allowed,
    args: &Map<String, Value>,
) -> Result<ToolOutput, ToolError> {
    let a: DeleteArgs = parse(args)?;
    let namespace = allowed.namespace.as_str();
    let name = name_of(allowed);
    let client = ctx.session.client_in(namespace)?;
    let Some(now) = current(&client, namespace, name).await? else {
        return Err(ToolError::Failed(format!(
            "Secret {namespace}/{name} does not exist"
        )));
    };
    let (used_by, _) = users_of(&client, namespace, name).await;
    let request = SecretRequest {
        op: SecretOp::Delete,
        name: name.to_owned(),
        secret_type: now.secret_type.clone(),
        keys: Vec::new(),
        remove: Vec::new(),
        existing: now.keys.clone(),
        source: None,
        used_by: used_by.clone(),
        explanation: a.explanation.clone(),
    };
    let what = format!("delete Secret {namespace}/{name}");
    let summary = format!("{} asks to {what}", ctx.client_name);
    let decision = ask_user(ctx, namespace, summary, request).await?;
    if !decision.is_approved() {
        return Err(refused(decision, &what));
    }
    allow_on_server(ctx, namespace, name).await?;
    let params = DeleteParams {
        // Exactly the Secret the user saw.
        preconditions: Some(Preconditions {
            uid: now.uid.clone(),
            resource_version: None,
        }),
        ..DeleteParams::background()
    };
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let deleted = api.delete(name, &params).await.map_err(secret_error)?;
    if let Some(mut secret) = deleted.left() {
        wipe_secret(&mut secret);
    }
    let mut out = json!({
        "namespace": namespace,
        "name": name,
        "operation": "deleted",
        "type": now.secret_type,
        "keys": keys_json(&now.keys),
        "usedBy": used_by,
    });
    if !used_by.is_empty() {
        out["note"] = json!(
            "The workloads in usedBy still reference this Secret; new pods will fail to start \
             until it exists again or the references are removed."
        );
    }
    let audit = json!({
        "operation": "delete",
        "namespace": namespace,
        "name": name,
        "type": now.secret_type,
        "keys": now.keys.iter().map(|k| &k.key).collect::<Vec<_>>(),
    });
    Ok(ToolOutput {
        text: serde_json::to_string_pretty(&out).unwrap_or_default(),
        confirmed: true,
        audit: Some(audit),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOT_A_CERT: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";

    fn input(pairs: &[(&str, &[u8])], remove: &[&str]) -> SecretInput {
        SecretInput {
            set: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), SecretBytes::new(v.to_vec())))
                .collect(),
            remove: remove.iter().map(|k| k.to_string()).collect(),
        }
    }

    fn request(op: SecretOp, secret_type: &str, existing: &[&str]) -> SecretRequest {
        SecretRequest {
            op,
            name: "db".into(),
            secret_type: secret_type.into(),
            keys: Vec::new(),
            remove: Vec::new(),
            existing: existing
                .iter()
                .map(|k| KeySize {
                    key: k.to_string(),
                    size: 3,
                })
                .collect(),
            source: None,
            used_by: Vec::new(),
            explanation: String::new(),
        }
    }

    #[test]
    fn debug_and_errors_hold_no_values() {
        let answer = input(&[("password", b"hunter2-xyz")], &["old"]);
        let printed = format!("{answer:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("11 bytes") && printed.contains("old"));
        let decision = ConfirmationDecision::Provided { input: answer };
        assert!(!format!("{decision:?}").contains("hunter2"));
        assert!(!decision.is_approved());
        assert_eq!(decision.reason(), None);
    }

    #[test]
    fn input_crosses_the_bridge_and_back() {
        let answer = input(&[("password", b"hunter2"), ("bin", &[0xff, 0])], &["old"]);
        let decision = ConfirmationDecision::Provided {
            input: answer.clone(),
        };
        let wire = serde_json::to_string(&decision).unwrap();
        // Encoded for the wire; never as plain text.
        assert!(!wire.contains("hunter2"), "{wire}");
        let back: ConfirmationDecision = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, decision);
        let bad = r#"{"decision":"provided","input":{"set":{"k":"not base64!"}}}"#;
        let error = serde_json::from_str::<ConfirmationDecision>(bad)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("not base64!"), "{error}");
    }

    #[test]
    fn create_and_update_answers_are_checked() {
        let create = request(SecretOp::Create, "Opaque", &[]);
        assert!(check_input(&create, &input(&[("a", b"1")], &[])).is_ok());
        assert!(check_input(&create, &input(&[], &[])).is_err());
        assert!(check_input(&create, &input(&[("a", b"1")], &["b"])).is_err());
        assert!(check_input(&create, &input(&[("bad key", b"1")], &[])).is_err());
        let big = vec![b'a'; MAX_SECRET_BYTES + 1];
        assert!(check_input(&create, &input(&[("a", &big)], &[])).is_err());

        let update = request(SecretOp::Update, "Opaque", &["a", "b"]);
        assert!(check_input(&update, &input(&[("c", b"1")], &["a"])).is_ok());
        assert!(check_input(&update, &input(&[], &[])).is_err());
        assert!(check_input(&update, &input(&[], &["zzz"])).is_err());
        assert!(check_input(&update, &input(&[("a", b"1")], &["a"])).is_err());
        let copy = request(SecretOp::Copy, "Opaque", &["a"]);
        assert!(check_input(&copy, &input(&[("a", b"1")], &[])).is_err());
    }

    #[test]
    fn typed_forms_are_checked() {
        let tls = request(SecretOp::Create, typed::TYPE_TLS, &[]);
        let only_cert = input(&[("tls.crt", NOT_A_CERT.as_bytes())], &[]);
        let error = check_input(&tls, &only_cert).unwrap_err();
        assert!(error.contains("tls.key"), "{error}");
        let broken = input(
            &[("tls.crt", NOT_A_CERT.as_bytes()), ("tls.key", b"nope")],
            &[],
        );
        assert!(check_input(&tls, &broken).is_err());
        let update = request(SecretOp::Update, typed::TYPE_TLS, &["tls.crt", "tls.key"]);
        assert!(check_input(&update, &input(&[], &["tls.key"])).is_err());

        let docker = request(SecretOp::Create, typed::TYPE_DOCKER_CONFIG_JSON, &[]);
        assert!(check_input(&docker, &input(&[(".dockerconfigjson", b"{}")], &[])).is_err());
        let config = br#"{"auths":{"registry.example.com":{"auth":"dTpw"}}}"#;
        assert!(check_input(&docker, &input(&[(".dockerconfigjson", config)], &[])).is_ok());
    }

    #[test]
    fn explanations_hide_nothing() {
        assert!(check_explanation("Create the DB password.\nThanks").is_ok());
        assert!(check_explanation("a\u{202E}b").is_err());
        assert!(check_explanation("a\u{1b}[2Kb").is_err());
        assert!(check_explanation(&"x".repeat(MAX_EXPLANATION_CHARS + 1)).is_err());
    }
}
