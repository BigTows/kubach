//! Readable summaries of well-known Secret types: registry logins of a
//! docker config, certificates of a TLS Secret, the user of basic auth.
//! Passwords, auth tokens and private keys are never part of a summary.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use super::value::SecretBytes;
use super::x509::{self, CertificateInfo};
use crate::validation::{Field, ValidationError};

pub const TYPE_OPAQUE: &str = "Opaque";
pub const TYPE_DOCKER_CONFIG_JSON: &str = "kubernetes.io/dockerconfigjson";
pub const TYPE_DOCKER_CFG: &str = "kubernetes.io/dockercfg";
pub const TYPE_TLS: &str = "kubernetes.io/tls";
pub const TYPE_BASIC_AUTH: &str = "kubernetes.io/basic-auth";

pub const KEY_DOCKER_CONFIG_JSON: &str = ".dockerconfigjson";
pub const KEY_DOCKER_CFG: &str = ".dockercfg";
pub const KEY_TLS_CERT: &str = "tls.crt";
pub const KEY_TLS_KEY: &str = "tls.key";
pub const KEY_USERNAME: &str = "username";
pub const KEY_PASSWORD: &str = "password";

/// Secret types with a summary view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedKind {
    DockerConfigJson,
    DockerCfg,
    Tls,
    BasicAuth,
}

impl TypedKind {
    pub fn of(secret_type: &str) -> Option<Self> {
        match secret_type {
            TYPE_DOCKER_CONFIG_JSON => Some(Self::DockerConfigJson),
            TYPE_DOCKER_CFG => Some(Self::DockerCfg),
            TYPE_TLS => Some(Self::Tls),
            TYPE_BASIC_AUTH => Some(Self::BasicAuth),
            _ => None,
        }
    }

    /// The keys a summary is built from. Private keys and passwords are
    /// not among them, so they are never fetched for a summary.
    pub fn source_keys(self) -> &'static [&'static str] {
        match self {
            Self::DockerConfigJson => &[KEY_DOCKER_CONFIG_JSON],
            Self::DockerCfg => &[KEY_DOCKER_CFG],
            Self::Tls => &[KEY_TLS_CERT],
            Self::BasicAuth => &[KEY_USERNAME],
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::DockerConfigJson | Self::DockerCfg => "Registry credentials",
            Self::Tls => "TLS certificate",
            Self::BasicAuth => "Basic authentication",
        }
    }
}

/// One registry of a docker config, without its password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryLogin {
    pub registry: String,
    pub username: Option<String>,
    pub email: Option<String>,
    /// A password or auth token is set (it is never shown).
    pub has_password: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Summary {
    Registries(Vec<RegistryLogin>),
    /// The chain in `tls.crt`, leaf first.
    Certificates(Vec<CertificateInfo>),
    BasicAuth {
        username: Option<String>,
    },
}

/// Builds the summary of `kind` from the values of its
/// [`source_keys`](TypedKind::source_keys).
pub fn summarize(
    kind: TypedKind,
    values: &BTreeMap<String, SecretBytes>,
) -> Result<Summary, ValidationError> {
    let value = |key: &str| values.get(key);
    match kind {
        TypedKind::DockerConfigJson | TypedKind::DockerCfg => {
            let key = kind.source_keys()[0];
            let value = value(key).ok_or_else(|| ValidationError::MissingKey(key.to_owned()))?;
            docker_logins(value.expose(), kind == TypedKind::DockerCfg).map(Summary::Registries)
        }
        TypedKind::Tls => {
            let value = value(KEY_TLS_CERT)
                .ok_or_else(|| ValidationError::MissingKey(KEY_TLS_CERT.to_owned()))?;
            certificates(value.expose()).map(Summary::Certificates)
        }
        TypedKind::BasicAuth => Ok(Summary::BasicAuth {
            username: value(KEY_USERNAME).map(|v| String::from_utf8_lossy(v.expose()).into()),
        }),
    }
}

/// Registries of a `.dockerconfigjson` (`{"auths": {…}}`) or a legacy
/// `.dockercfg` (the registry map itself).
pub fn docker_logins(bytes: &[u8], legacy: bool) -> Result<Vec<RegistryLogin>, ValidationError> {
    let config: Value =
        serde_json::from_slice(bytes).map_err(|_| ValidationError::Json(Field::DockerConfig))?;
    let auths = if legacy {
        config.as_object()
    } else {
        config.get("auths").and_then(Value::as_object)
    }
    .ok_or(ValidationError::Nothing(Field::DockerConfig))?;
    Ok(auths
        .iter()
        .map(|(registry, entry)| {
            let text = |key: &str| {
                entry
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            };
            // `auth` is base64 of `user:password`; only the user is kept.
            let auth_user = text("auth")
                .and_then(|auth| STANDARD.decode(auth).ok())
                .and_then(|decoded| {
                    let user = decoded.split(|&b| b == b':').next()?.to_vec();
                    let mut decoded = decoded;
                    super::value::wipe(&mut decoded);
                    String::from_utf8(user).ok()
                });
            RegistryLogin {
                registry: registry.clone(),
                username: text("username").map(str::to_owned).or(auth_user),
                email: text("email").map(str::to_owned),
                has_password: text("password").is_some()
                    || text("auth").is_some()
                    || text("identitytoken").is_some(),
            }
        })
        .collect())
}

/// Certificates in a PEM bundle, in order.
pub fn certificates(pem: &[u8]) -> Result<Vec<CertificateInfo>, ValidationError> {
    let text = std::str::from_utf8(pem).map_err(|_| ValidationError::Pem(Field::TlsCert))?;
    let blocks: Vec<_> = x509::pem_blocks(text)
        .into_iter()
        .filter(|block| block.label == "CERTIFICATE")
        .collect();
    if blocks.is_empty() {
        return Err(ValidationError::Nothing(Field::TlsCert));
    }
    blocks
        .iter()
        .map(|block| x509::parse_certificate(&block.der).map_err(ValidationError::from))
        .collect()
}

/// Whether `pem` holds a private key block (the key itself is not parsed).
pub fn has_private_key(pem: &[u8]) -> bool {
    std::str::from_utf8(pem).is_ok_and(|text| {
        x509::pem_blocks(text)
            .iter()
            .any(|block| block.label.ends_with("PRIVATE KEY"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values(pairs: &[(&str, &[u8])]) -> BTreeMap<String, SecretBytes> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), SecretBytes::new(v.to_vec())))
            .collect()
    }

    #[test]
    fn docker_config_without_passwords() {
        let config = json!({"auths": {
            "ghcr.io": {"username": "bot", "password": "s3cret", "email": "bot@example.com"},
            "registry.example.com": {"auth": STANDARD.encode("alice:pa55")},
            "public.example.com": {}
        }});
        let bytes = serde_json::to_vec(&config).unwrap();
        let summary = summarize(
            TypedKind::DockerConfigJson,
            &values(&[(".dockerconfigjson", &bytes)]),
        )
        .unwrap();
        let Summary::Registries(logins) = summary else {
            panic!("{summary:?}");
        };
        assert_eq!(logins.len(), 3);
        assert_eq!(logins[0].registry, "ghcr.io");
        assert_eq!(logins[0].username.as_deref(), Some("bot"));
        assert_eq!(logins[0].email.as_deref(), Some("bot@example.com"));
        assert!(logins[0].has_password);
        // Registries keep their order in the config.
        assert_eq!(logins[1].username.as_deref(), Some("alice"));
        assert!(logins[1].has_password);
        assert_eq!(logins[2].username, None);
        assert!(!logins[2].has_password);
        let printed = format!("{logins:?}");
        assert!(!printed.contains("s3cret") && !printed.contains("pa55"));
    }

    #[test]
    fn legacy_dockercfg() {
        let bytes = br#"{"quay.io": {"username": "u", "password": "p"}}"#;
        let logins = docker_logins(bytes, true).unwrap();
        assert_eq!(logins[0].registry, "quay.io");
        assert!(docker_logins(b"nope", false).is_err());
        assert!(docker_logins(b"{}", false).is_err());
    }

    #[test]
    fn tls_summary_reads_the_certificate_only() {
        assert_eq!(TypedKind::Tls.source_keys(), ["tls.crt"]);
        let summary = summarize(
            TypedKind::Tls,
            &values(&[("tls.crt", x509::TEST_CERT.as_bytes())]),
        )
        .unwrap();
        let Summary::Certificates(certs) = summary else {
            panic!();
        };
        assert_eq!(certs[0].subject, "C=DE, O=Kubach Test, CN=example.test");
        assert!(summarize(TypedKind::Tls, &values(&[("tls.crt", b"junk")])).is_err());
        assert!(summarize(TypedKind::Tls, &values(&[])).is_err());
    }

    #[test]
    fn basic_auth_shows_the_user() {
        assert_eq!(TypedKind::BasicAuth.source_keys(), ["username"]);
        let summary = summarize(TypedKind::BasicAuth, &values(&[("username", b"admin")]));
        assert_eq!(
            summary,
            Ok(Summary::BasicAuth {
                username: Some("admin".into())
            })
        );
    }

    #[test]
    fn kinds() {
        assert_eq!(TypedKind::of(TYPE_TLS), Some(TypedKind::Tls));
        assert_eq!(TypedKind::of(TYPE_OPAQUE), None);
        assert!(has_private_key(
            b"-----BEGIN EC PRIVATE KEY-----\nAAEC\n-----END EC PRIVATE KEY-----"
        ));
        assert!(!has_private_key(x509::TEST_CERT.as_bytes()));
    }
}
