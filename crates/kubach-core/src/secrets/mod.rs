//! Secret values for the user, on demand.
//!
//! The cache keeps Secrets without values (see [`crate::cache`]). This
//! module fetches values only when the user asks — a separate `GET` at that
//! moment — and hands back just the keys asked for, wrapped in
//! [`SecretBytes`] so they never show up in `Debug` output, logs or error
//! messages. Changes go out as a merge patch naming only changed keys.
//!
//! The fetch and edit functions are for a person in the UI only. The MCP
//! agent (#227) gets metadata, [`checks`] and [`refs`] of values Kubach
//! reads itself; a value reaches it only through its own reveal tool.

pub mod checks;
pub mod clipboard;
pub mod edit;
pub mod generate;
pub mod mask;
pub mod refs;
pub mod typed;
pub mod value;
pub mod x509;

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Secret;
use kube::Client;
use kube::api::{Api, Patch, PatchParams, PostParams};

use crate::rbac::{self, ResourceAccess};
pub use clipboard::{CLIPBOARD_TTL, ClearTrigger, ClipboardGuard};
pub use edit::{KeyChange, NewSecret, ReviewLine, SecretPatch, validate_key, validate_name};
pub use typed::{RegistryLogin, Summary, TypedKind};
pub use value::{Decoded, SecretBytes, describe_binary, describe_size};
pub use x509::{CertificateInfo, Validity};

/// Field manager of Kubach's Secret edits.
const FIELD_MANAGER: &str = "kubach";

/// Why a Secret operation failed. Messages never contain values.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("No permission (RBAC): {0}")]
    Forbidden(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error(
        "The Secret changed in the cluster since it was loaded; reload it and make the edits \
         again"
    )]
    Conflict,
    #[error("The Secret already exists")]
    AlreadyExists,
    #[error("The Secret is immutable: its data can't be changed, only deleted and recreated")]
    Immutable,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Api(String),
}

impl From<kube::Error> for SecretError {
    fn from(error: kube::Error) -> Self {
        match &error {
            kube::Error::Api(status) => {
                let message = if status.message.is_empty() {
                    status.reason.clone()
                } else {
                    status.message.clone()
                };
                match status.code {
                    403 => Self::Forbidden(message),
                    404 => Self::NotFound(message),
                    409 if status.reason == "AlreadyExists" => Self::AlreadyExists,
                    409 => Self::Conflict,
                    422 => Self::Invalid(message),
                    _ => Self::Api(message),
                }
            }
            // A decoding error may quote the response (a value), so only
            // where it failed is kept.
            kube::Error::SerdeError(serde) => Self::Api(format!(
                "Can't read the cluster's response (line {}, column {})",
                serde.line(),
                serde.column()
            )),
            // Transport errors carry no response body.
            _ => Self::Api(error.to_string()),
        }
    }
}

/// What a Secret is, without its values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretInfo {
    pub secret_type: String,
    pub immutable: bool,
    pub resource_version: Option<String>,
    /// Keys with the size of their values, sorted.
    pub keys: Vec<(String, usize)>,
}

impl SecretInfo {
    pub fn typed_kind(&self) -> Option<TypedKind> {
        TypedKind::of(&self.secret_type)
    }
}

/// Values fetched for some keys, with the Secret they came from.
#[derive(Debug)]
pub struct Revealed {
    pub info: SecretInfo,
    pub values: BTreeMap<String, SecretBytes>,
}

fn api(client: &Client, namespace: &str) -> Api<Secret> {
    Api::namespaced(client.clone(), namespace)
}

/// Splits a fetched Secret into what may be kept (`info`) and the values of
/// `keys`; every other value is wiped right away.
fn split(mut secret: Secret, keys: Option<&[&str]>) -> Revealed {
    let mut data = secret.data.take().unwrap_or_default();
    let mut values = BTreeMap::new();
    let mut sizes = Vec::with_capacity(data.len());
    for (key, bytes) in data.iter_mut() {
        sizes.push((key.clone(), bytes.0.len()));
        let wanted = keys.is_none_or(|keys| keys.contains(&key.as_str()));
        let bytes = std::mem::take(&mut bytes.0);
        let bytes = SecretBytes::new(bytes);
        if wanted {
            values.insert(key.clone(), bytes);
        }
    }
    // `stringData` is write-only and never returned, but be thorough.
    if let Some(mut string_data) = secret.string_data.take() {
        for value in string_data.values_mut() {
            let mut bytes = std::mem::take(value).into_bytes();
            value::wipe(&mut bytes);
        }
    }
    Revealed {
        info: SecretInfo {
            secret_type: secret
                .type_
                .unwrap_or_else(|| typed::TYPE_OPAQUE.to_owned()),
            immutable: secret.immutable.unwrap_or(false),
            resource_version: secret.metadata.resource_version,
            keys: sizes,
        },
        values,
    }
}

/// Fetches the Secret now (never from the cache) and returns the values of
/// `keys` only; missing keys are absent from the result.
pub async fn reveal(
    client: &Client,
    namespace: &str,
    name: &str,
    keys: &[&str],
) -> Result<Revealed, SecretError> {
    let secret = api(client, namespace).get(name).await?;
    Ok(split(secret, Some(keys)))
}

/// Fetches the Secret's keys, sizes, type and version; values are dropped
/// (and wiped) as soon as they are measured.
pub async fn inspect(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<SecretInfo, SecretError> {
    let secret = api(client, namespace).get(name).await?;
    Ok(split(secret, Some(&[])).info)
}

/// Sends `patch` (changed keys only). Refused locally for immutable Secrets
/// and empty patches.
pub async fn apply_patch(
    client: &Client,
    namespace: &str,
    name: &str,
    patch: &SecretPatch,
) -> Result<(), SecretError> {
    if patch.is_empty() {
        return Err(SecretError::Invalid("Nothing to apply".into()));
    }
    if let Some(key) = patch.invalid_keys().first() {
        return Err(SecretError::Invalid(
            validate_key(key)
                .err()
                .map(String::from)
                .unwrap_or_default(),
        ));
    }
    let params = PatchParams {
        field_manager: Some(FIELD_MANAGER.into()),
        ..PatchParams::default()
    };
    let body = patch.merge_patch();
    api(client, namespace)
        .patch(name, &params, &Patch::Merge(&body))
        .await?;
    Ok(())
}

/// Creates `secret` as `namespace/name`.
pub async fn create(
    client: &Client,
    namespace: &str,
    name: &str,
    secret: &NewSecret,
) -> Result<(), SecretError> {
    let object = secret
        .build(namespace, name)
        .map_err(|error| SecretError::Invalid(error.to_string()))?;
    let params = PostParams {
        field_manager: Some(FIELD_MANAGER.into()),
        ..PostParams::default()
    };
    api(client, namespace).create(&params, &object).await?;
    Ok(())
}

/// Whether the user may do something with Secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    Allowed,
    Denied(String),
    /// The check itself failed; the action may still work.
    Unknown(String),
}

impl Access {
    pub fn allowed(&self) -> bool {
        !matches!(self, Self::Denied(_))
    }
}

/// Asks the API server (`SelfSubjectAccessReview`) whether the user may
/// `verb` the Secret `name` (or Secrets in general) in `namespace`.
pub async fn can_i(client: &Client, verb: &str, namespace: &str, name: Option<&str>) -> Access {
    let mut access = ResourceAccess::new(verb, "", "secrets").in_namespace(namespace);
    if let Some(name) = name {
        access = access.named(name);
    }
    match rbac::can_i(client, &access).await {
        Ok(decision) if decision.allowed => Access::Allowed,
        Ok(decision) => Access::Denied(denied_message(verb, namespace, decision.reason)),
        Err(error) => Access::Unknown(error.to_string()),
    }
}

fn denied_message(verb: &str, namespace: &str, reason: Option<String>) -> String {
    let base = format!("Your credentials don't allow you to {verb} Secrets in {namespace}");
    match reason {
        Some(reason) => format!("{base} ({reason})."),
        None => format!("{base}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::ByteString;
    use kube::core::Status;

    fn secret() -> Secret {
        Secret {
            type_: Some("kubernetes.io/basic-auth".into()),
            immutable: Some(true),
            data: Some(BTreeMap::from([
                ("username".to_owned(), ByteString(b"admin".to_vec())),
                ("password".to_owned(), ByteString(b"hunter2".to_vec())),
            ])),
            metadata: kube::api::ObjectMeta {
                resource_version: Some("7".into()),
                ..Default::default()
            },
            ..Secret::default()
        }
    }

    #[test]
    fn split_keeps_only_requested_values() {
        let revealed = split(secret(), Some(&["username"]));
        assert_eq!(revealed.values.len(), 1);
        assert_eq!(revealed.values["username"].expose(), b"admin");
        assert_eq!(
            revealed.info,
            SecretInfo {
                secret_type: "kubernetes.io/basic-auth".into(),
                immutable: true,
                resource_version: Some("7".into()),
                keys: vec![("password".into(), 7), ("username".into(), 5)],
            }
        );
        assert_eq!(revealed.info.typed_kind(), Some(TypedKind::BasicAuth));
        assert!(!format!("{revealed:?}").contains("admin"));
        assert!(split(secret(), Some(&[])).values.is_empty());
    }

    #[test]
    fn default_type_is_opaque() {
        let revealed = split(Secret::default(), None);
        assert_eq!(revealed.info.secret_type, "Opaque");
        assert!(revealed.info.keys.is_empty());
    }

    fn api_error(code: u16, reason: &str) -> kube::Error {
        let status: Status = serde_json::from_value(serde_json::json!({
            "status": "Failure", "message": "msg", "reason": reason, "code": code
        }))
        .unwrap();
        kube::Error::Api(Box::new(status))
    }

    #[test]
    fn errors() {
        assert_eq!(
            SecretError::from(api_error(403, "Forbidden")),
            SecretError::Forbidden("msg".into())
        );
        assert_eq!(
            SecretError::from(api_error(409, "Conflict")),
            SecretError::Conflict
        );
        assert_eq!(
            SecretError::from(api_error(409, "AlreadyExists")),
            SecretError::AlreadyExists
        );
        assert_eq!(
            SecretError::from(api_error(422, "Invalid")),
            SecretError::Invalid("msg".into())
        );
    }

    #[test]
    fn decoding_errors_do_not_quote_values() {
        let serde = serde_json::from_str::<u8>("\"hunter2\"").unwrap_err();
        assert!(serde.to_string().contains("hunter2"));
        let message = SecretError::from(kube::Error::SerdeError(serde)).to_string();
        assert!(!message.contains("hunter2"), "{message}");
        assert!(message.contains("line 1"), "{message}");
    }

    #[test]
    fn denied_messages() {
        assert_eq!(
            denied_message("get", "web", None),
            "Your credentials don't allow you to get Secrets in web."
        );
    }
}
