//! Changes to Secrets: a targeted patch of changed keys only, and new
//! Secrets (Opaque, docker-registry, TLS). Reviews name keys, never values.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::Secret;
use kube::api::ObjectMeta;
use serde_json::{Map, Value, json};

use crate::names;
use crate::validation::{Field, ValidationError};

use super::typed::{
    self, KEY_DOCKER_CONFIG_JSON, KEY_TLS_CERT, KEY_TLS_KEY, TYPE_DOCKER_CONFIG_JSON, TYPE_OPAQUE,
    TYPE_TLS,
};
use super::value::SecretBytes;

/// What happens to one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyChange {
    Added,
    Changed,
    Removed,
}

impl KeyChange {
    /// What a review shows in place of the value.
    pub fn placeholder(self) -> &'static str {
        match self {
            Self::Added => "<hidden, added>",
            Self::Changed => "<hidden, changed>",
            Self::Removed => "<removed>",
        }
    }
}

/// One line of a review: a key and what happens to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewLine {
    pub key: String,
    pub change: KeyChange,
}

impl ReviewLine {
    /// `password: <hidden, changed>`.
    pub fn describe(&self) -> String {
        format!("{}: {}", self.key, self.change.placeholder())
    }
}

/// Changes to an existing Secret's `data`, sent as a JSON merge patch that
/// names only the changed keys: keys left alone are not sent at all, so
/// values Kubach never loaded can't be overwritten.
#[derive(Clone, Debug, Default)]
pub struct SecretPatch {
    existing: BTreeSet<String>,
    resource_version: Option<String>,
    set: BTreeMap<String, SecretBytes>,
    remove: BTreeSet<String>,
}

impl SecretPatch {
    /// `existing` are the keys the Secret had at `resource_version`; with a
    /// version the server rejects the patch if the Secret changed since.
    pub fn new(
        existing: impl IntoIterator<Item = String>,
        resource_version: Option<String>,
    ) -> Self {
        Self {
            existing: existing.into_iter().collect(),
            resource_version,
            ..Self::default()
        }
    }

    /// Sets `key` to `value` (adds it if new).
    pub fn set(&mut self, key: impl Into<String>, value: SecretBytes) {
        let key = key.into();
        self.remove.remove(&key);
        self.set.insert(key, value);
    }

    /// Removes an existing `key`; a key added in this patch is just dropped.
    pub fn remove(&mut self, key: &str) {
        self.set.remove(key);
        if self.existing.contains(key) {
            self.remove.insert(key.to_owned());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.remove.is_empty()
    }

    /// Changed keys by name, without values.
    pub fn review(&self) -> Vec<ReviewLine> {
        let mut lines: Vec<ReviewLine> = self
            .set
            .keys()
            .map(|key| ReviewLine {
                key: key.clone(),
                change: if self.existing.contains(key) {
                    KeyChange::Changed
                } else {
                    KeyChange::Added
                },
            })
            .chain(self.remove.iter().map(|key| ReviewLine {
                key: key.clone(),
                change: KeyChange::Removed,
            }))
            .collect();
        lines.sort_by(|a, b| a.key.cmp(&b.key));
        lines
    }

    /// Keys whose names are invalid for a Secret.
    pub fn invalid_keys(&self) -> Vec<String> {
        self.set
            .keys()
            .filter(|key| validate_key(key).is_err())
            .cloned()
            .collect()
    }

    /// The merge patch body. It carries values (base64): never log it.
    pub(crate) fn merge_patch(&self) -> Value {
        let mut data = Map::new();
        for (key, value) in &self.set {
            data.insert(key.clone(), Value::String(STANDARD.encode(value.expose())));
        }
        for key in &self.remove {
            data.insert(key.clone(), Value::Null);
        }
        let mut patch = json!({ "data": data });
        if let Some(version) = &self.resource_version {
            patch["metadata"] = json!({ "resourceVersion": version });
        }
        patch
    }
}

/// A Secret to create.
#[derive(Clone, Debug)]
pub enum NewSecret {
    Opaque {
        entries: Vec<(String, SecretBytes)>,
    },
    DockerRegistry {
        server: String,
        username: String,
        password: SecretBytes,
        email: Option<String>,
    },
    Tls {
        cert: SecretBytes,
        key: SecretBytes,
    },
}

/// `kubectl create secret docker-registry` uses Docker Hub by default.
pub const DEFAULT_REGISTRY: &str = "https://index.docker.io/v1/";

impl NewSecret {
    pub fn secret_type(&self) -> &'static str {
        match self {
            Self::Opaque { .. } => TYPE_OPAQUE,
            Self::DockerRegistry { .. } => TYPE_DOCKER_CONFIG_JSON,
            Self::Tls { .. } => TYPE_TLS,
        }
    }

    /// Checks what can be checked locally; messages name fields, not values.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Opaque { entries } => {
                let mut seen = BTreeSet::new();
                for (key, _) in entries {
                    validate_key(key)?;
                    if !seen.insert(key) {
                        return Err(ValidationError::Duplicate {
                            field: Field::SecretKey,
                            value: key.clone(),
                        });
                    }
                }
                Ok(())
            }
            Self::DockerRegistry {
                server,
                username,
                password,
                ..
            } => {
                if server.trim().is_empty() {
                    return Err(ValidationError::Empty(Field::RegistryServer));
                }
                if username.trim().is_empty() {
                    return Err(ValidationError::Empty(Field::Username));
                }
                if password.is_empty() {
                    return Err(ValidationError::Empty(Field::Password));
                }
                Ok(())
            }
            Self::Tls { cert, key } => {
                typed::certificates(cert.expose())?;
                if !typed::has_private_key(key.expose()) {
                    return Err(ValidationError::Nothing(Field::TlsKey));
                }
                Ok(())
            }
        }
    }

    /// Keys of the new Secret, for the review.
    pub fn review(&self) -> Vec<ReviewLine> {
        let keys: Vec<String> = match self {
            Self::Opaque { entries } => entries.iter().map(|(k, _)| k.clone()).collect(),
            Self::DockerRegistry { .. } => vec![KEY_DOCKER_CONFIG_JSON.into()],
            Self::Tls { .. } => vec![KEY_TLS_CERT.into(), KEY_TLS_KEY.into()],
        };
        keys.into_iter()
            .map(|key| ReviewLine {
                key,
                change: KeyChange::Added,
            })
            .collect()
    }

    /// The Secret object to create. It carries values: never log it.
    pub fn build(&self, namespace: &str, name: &str) -> Result<Secret, ValidationError> {
        validate_name(name)?;
        self.validate()?;
        let data: BTreeMap<String, ByteString> = match self {
            Self::Opaque { entries } => entries
                .iter()
                .map(|(key, value)| (key.clone(), ByteString(value.expose().to_vec())))
                .collect(),
            Self::DockerRegistry {
                server,
                username,
                password,
                email,
            } => {
                let config = docker_config(server, username, password, email.as_deref());
                BTreeMap::from([(
                    KEY_DOCKER_CONFIG_JSON.to_owned(),
                    ByteString(config.expose().to_vec()),
                )])
            }
            Self::Tls { cert, key } => BTreeMap::from([
                (KEY_TLS_CERT.to_owned(), ByteString(cert.expose().to_vec())),
                (KEY_TLS_KEY.to_owned(), ByteString(key.expose().to_vec())),
            ]),
        };
        Ok(Secret {
            metadata: ObjectMeta {
                name: Some(name.to_owned()),
                namespace: Some(namespace.to_owned()),
                ..ObjectMeta::default()
            },
            type_: Some(self.secret_type().to_owned()),
            data: Some(data),
            ..Secret::default()
        })
    }
}

/// `.dockerconfigjson` for one registry, as `kubectl create secret
/// docker-registry` writes it.
fn docker_config(
    server: &str,
    username: &str,
    password: &SecretBytes,
    email: Option<&str>,
) -> SecretBytes {
    let password = String::from_utf8_lossy(password.expose());
    let mut auth = format!("{username}:{password}").into_bytes();
    let mut entry = json!({
        "username": username,
        "password": password,
        "auth": STANDARD.encode(&auth),
    });
    super::value::wipe(&mut auth);
    if let Some(email) = email.filter(|e| !e.is_empty()) {
        entry["email"] = json!(email);
    }
    let config = json!({ "auths": { server.trim(): entry } });
    SecretBytes::new(serde_json::to_vec(&config).unwrap_or_default())
}

/// Data keys: `[-._a-zA-Z0-9]+`, at most 253 characters.
pub fn validate_key(key: &str) -> Result<(), ValidationError> {
    let field = Field::SecretKey;
    if key.is_empty() {
        return Err(ValidationError::Empty(field));
    }
    if key.len() > MAX_KEY_LEN {
        return Err(ValidationError::TooLong {
            field,
            value: key.to_owned(),
            max: MAX_KEY_LEN,
        });
    }
    if key == "." || key == ".." {
        return Err(ValidationError::Reserved {
            field,
            value: key.to_owned(),
        });
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(ValidationError::InvalidChars {
            field,
            value: key.to_owned(),
        });
    }
    Ok(())
}

/// Longest Secret data key.
const MAX_KEY_LEN: usize = 253;

/// Object names: a DNS subdomain (lowercase letters, digits, '-' and '.',
/// starting and ending with a letter or digit, at most 253 characters).
pub fn validate_name(name: &str) -> Result<(), ValidationError> {
    names::validate_dns_subdomain(name).map_err(ValidationError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::x509::TEST_CERT;

    fn existing() -> SecretPatch {
        SecretPatch::new(
            ["user".to_owned(), "pass".to_owned(), "old".to_owned()],
            Some("42".into()),
        )
    }

    #[test]
    fn patch_names_only_changed_keys() {
        let mut patch = existing();
        assert!(patch.is_empty());
        patch.set("pass", SecretBytes::from("n3w"));
        patch.set("token", SecretBytes::from("t"));
        patch.remove("old");
        assert_eq!(
            patch.merge_patch(),
            json!({
                "metadata": {"resourceVersion": "42"},
                "data": {"pass": "bjN3", "token": "dA==", "old": null}
            })
        );
        // `user` is not mentioned at all.
        assert!(patch.merge_patch()["data"].get("user").is_none());
    }

    #[test]
    fn review_shows_no_values() {
        let mut patch = existing();
        patch.set("pass", SecretBytes::from("n3w-value"));
        patch.set("token", SecretBytes::from("t0ken"));
        patch.remove("old");
        let lines: Vec<String> = patch.review().iter().map(ReviewLine::describe).collect();
        assert_eq!(
            lines,
            [
                "old: <removed>",
                "pass: <hidden, changed>",
                "token: <hidden, added>"
            ]
        );
        let printed = format!("{patch:?}");
        assert!(!printed.contains("n3w-value") && !printed.contains("t0ken"));
    }

    #[test]
    fn removing_an_added_key_drops_it() {
        let mut patch = existing();
        patch.set("new", SecretBytes::from("x"));
        patch.remove("new");
        assert!(patch.is_empty());
        // Setting a removed key again changes it instead.
        patch.remove("pass");
        patch.set("pass", SecretBytes::from("y"));
        assert_eq!(patch.review()[0].change, KeyChange::Changed);
    }

    #[test]
    fn keys_and_names() {
        assert!(validate_key("tls.crt").is_ok());
        assert!(validate_key(".dockerconfigjson").is_ok());
        assert!(matches!(
            validate_key("a/b"),
            Err(ValidationError::InvalidChars { .. })
        ));
        assert_eq!(
            validate_key(""),
            Err(ValidationError::Empty(Field::SecretKey))
        );
        assert!(matches!(
            validate_key(".."),
            Err(ValidationError::Reserved { .. })
        ));
        assert!(matches!(
            validate_key(&"k".repeat(254)),
            Err(ValidationError::TooLong { max: 253, .. })
        ));
        assert!(validate_name("my-secret.v1").is_ok());
        assert_eq!(
            validate_name("My"),
            Err(ValidationError::from(names::NameError::InvalidChar('M')))
        );
        assert!(validate_name("-a").is_err());
        assert!(validate_name("a.-b").is_err());
        assert!(validate_name("a..b").is_err());
        let mut patch = existing();
        patch.set("bad key", SecretBytes::from("x"));
        assert_eq!(patch.invalid_keys(), ["bad key"]);
    }

    #[test]
    fn opaque_secret() {
        let new = NewSecret::Opaque {
            entries: vec![
                ("user".into(), SecretBytes::from("admin")),
                ("pass".into(), SecretBytes::from("pw")),
            ],
        };
        let secret = new.build("ns", "creds").unwrap();
        assert_eq!(secret.type_.as_deref(), Some("Opaque"));
        assert_eq!(secret.metadata.namespace.as_deref(), Some("ns"));
        assert_eq!(secret.data.unwrap()["user"].0, b"admin");
        let lines: Vec<String> = new.review().iter().map(ReviewLine::describe).collect();
        assert_eq!(lines, ["user: <hidden, added>", "pass: <hidden, added>"]);
        let twice = NewSecret::Opaque {
            entries: vec![
                ("a".into(), SecretBytes::default()),
                ("a".into(), SecretBytes::default()),
            ],
        };
        assert!(twice.validate().is_err());
        assert!(new.build("ns", "Bad_Name").is_err());
    }

    #[test]
    fn docker_registry_secret() {
        let new = NewSecret::DockerRegistry {
            server: "ghcr.io".into(),
            username: "bot".into(),
            password: SecretBytes::from("pw"),
            email: Some("bot@example.com".into()),
        };
        let secret = new.build("ns", "pull").unwrap();
        assert_eq!(
            secret.type_.as_deref(),
            Some("kubernetes.io/dockerconfigjson")
        );
        let data = secret.data.unwrap();
        let config: Value = serde_json::from_slice(&data[".dockerconfigjson"].0).unwrap();
        assert_eq!(
            config,
            json!({"auths": {"ghcr.io": {
                "username": "bot", "password": "pw", "email": "bot@example.com",
                "auth": STANDARD.encode("bot:pw")
            }}})
        );
        // The summary reads it back without the password.
        let logins = typed::docker_logins(&data[".dockerconfigjson"].0, false).unwrap();
        assert_eq!(logins[0].username.as_deref(), Some("bot"));
        let missing = NewSecret::DockerRegistry {
            server: DEFAULT_REGISTRY.into(),
            username: "bot".into(),
            password: SecretBytes::default(),
            email: None,
        };
        assert!(missing.validate().is_err());
    }

    #[test]
    fn tls_secret_checks_its_files() {
        let key =
            SecretBytes::from("-----BEGIN PRIVATE KEY-----\nAAEC\n-----END PRIVATE KEY-----\n");
        let new = NewSecret::Tls {
            cert: SecretBytes::from(TEST_CERT),
            key: key.clone(),
        };
        let secret = new.build("ns", "tls").unwrap();
        assert_eq!(secret.type_.as_deref(), Some("kubernetes.io/tls"));
        assert_eq!(
            secret.data.unwrap().keys().collect::<Vec<_>>(),
            ["tls.crt", "tls.key"]
        );
        let swapped = NewSecret::Tls {
            cert: key.clone(),
            key: SecretBytes::from(TEST_CERT),
        };
        assert!(swapped.validate().is_err());
        let no_key = NewSecret::Tls {
            cert: SecretBytes::from(TEST_CERT),
            key: SecretBytes::from(TEST_CERT),
        };
        assert_eq!(
            no_key.validate(),
            Err(ValidationError::Nothing(Field::TlsKey))
        );
    }
}
