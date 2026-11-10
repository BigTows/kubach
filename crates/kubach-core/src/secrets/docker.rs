//! A docker config (`.dockerconfigjson`, legacy `.dockercfg`) as a list of
//! registries that can be edited: the registry's address, user, email and
//! password. Everything else in the JSON — other top-level fields
//! (`credHelpers`, `credsStore`…), other fields of a registry
//! (`identitytoken`…) — is kept as it is. `auth` (base64 of
//! `user:password`) is written by Kubach whenever the user or the password
//! of a registry changes.
//!
//! A config holds passwords: its `Debug` names registries only, and the
//! bytes it serialises to are [`SecretBytes`].

use std::collections::BTreeSet;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};

use super::typed::TypedKind;
use super::value::{SecretBytes, wipe};
use crate::validation::{Field, ValidationError};

const AUTHS: &str = "auths";
const USERNAME: &str = "username";
const PASSWORD: &str = "password";
const EMAIL: &str = "email";
const AUTH: &str = "auth";

/// One registry as the user edits it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    /// The registry's address: the key in `auths`.
    pub server: String,
    pub username: String,
    pub email: String,
    /// From `password`, or the part of `auth` after the user.
    pub password: SecretBytes,
}

/// A registry of the edited list: `original` is the address it had in the
/// config it comes from, `None` for a registry the user added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryEdit {
    pub original: Option<String>,
    pub registry: Registry,
}

/// A secret field of one registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegistrySecret {
    /// `password`, or the password inside `auth`.
    Password,
    /// `auth` as it is stored (base64 of `user:password`).
    Auth,
}

/// A parsed docker config.
#[derive(Clone, PartialEq, Eq)]
pub struct DockerConfig {
    /// `.dockercfg`: the root is the registry map itself.
    legacy: bool,
    root: Map<String, Value>,
}

impl fmt::Debug for DockerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let servers: Vec<&String> = self
            .auths()
            .map(|auths| auths.keys().collect())
            .unwrap_or_default();
        f.debug_struct("DockerConfig")
            .field("legacy", &self.legacy)
            .field("registries", &servers)
            .finish_non_exhaustive()
    }
}

impl DockerConfig {
    /// An empty config, for a value written from scratch.
    pub fn empty(legacy: bool) -> Self {
        Self {
            legacy,
            root: Map::new(),
        }
    }

    /// The config in the value of a Secret of `kind`; `None` for kinds that
    /// are not docker configs.
    pub fn of_kind(kind: TypedKind, bytes: &[u8]) -> Option<Result<Self, ValidationError>> {
        match kind {
            TypedKind::DockerConfigJson => Some(Self::parse(bytes, false)),
            TypedKind::DockerCfg => Some(Self::parse(bytes, true)),
            TypedKind::Tls | TypedKind::BasicAuth => None,
        }
    }

    /// Parses a `.dockerconfigjson` (`{"auths": {…}}`) or, with `legacy`, a
    /// `.dockercfg` (the registry map itself). A config without `auths` has
    /// no registries; anything that is not a map of registry objects is an
    /// error, so that nothing unexpected is rewritten.
    pub fn parse(bytes: &[u8], legacy: bool) -> Result<Self, ValidationError> {
        let invalid = || ValidationError::Json(Field::DockerConfig);
        let root: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        let Value::Object(root) = root else {
            return Err(invalid());
        };
        let config = Self { legacy, root };
        let auths = match config.root.get(AUTHS) {
            _ if legacy => &config.root,
            None => return Ok(config),
            Some(Value::Object(auths)) => auths,
            Some(_) => return Err(invalid()),
        };
        if auths.values().any(|entry| !entry.is_object()) {
            return Err(invalid());
        }
        Ok(config)
    }

    pub fn legacy(&self) -> bool {
        self.legacy
    }

    fn auths(&self) -> Option<&Map<String, Value>> {
        if self.legacy {
            Some(&self.root)
        } else {
            self.root.get(AUTHS).and_then(Value::as_object)
        }
    }

    fn entry(&self, server: &str) -> Option<&Map<String, Value>> {
        self.auths()?.get(server)?.as_object()
    }

    /// The registries, in the config's order.
    pub fn registries(&self) -> Vec<Registry> {
        self.auths()
            .into_iter()
            .flatten()
            .filter_map(|(server, entry)| Some(registry(server, entry.as_object()?)))
            .collect()
    }

    /// Whether `server` has a value for `field`.
    pub fn has_secret(&self, server: &str, field: RegistrySecret) -> bool {
        self.secret(server, field).is_some_and(|v| !v.is_empty())
    }

    /// The value of a secret field of `server`; `None` if the registry is
    /// not in the config or has no such field.
    pub fn secret(&self, server: &str, field: RegistrySecret) -> Option<SecretBytes> {
        let entry = self.entry(server)?;
        match field {
            RegistrySecret::Auth => text(entry, AUTH).map(SecretBytes::from),
            RegistrySecret::Password => {
                let password = registry(server, entry).password;
                (!password.is_empty() || entry.contains_key(PASSWORD)).then_some(password)
            }
        }
    }

    /// The config with its registries replaced by `edits`, in their order.
    /// A registry that keeps its user, email and password keeps its entry
    /// untouched (also under a new address); a changed user or password
    /// rewrites `auth`; registries not in `edits` are removed. Other fields
    /// stay.
    pub fn with_registries(&self, edits: &[RegistryEdit]) -> Result<Self, ValidationError> {
        let mut seen = BTreeSet::new();
        let mut auths = Map::new();
        for edit in edits {
            let wanted = &edit.registry;
            let server = wanted.server.trim();
            if server.is_empty() {
                return Err(ValidationError::Empty(Field::RegistryServer));
            }
            if !seen.insert(server) {
                return Err(ValidationError::Duplicate {
                    field: Field::RegistryServer,
                    value: server.to_owned(),
                });
            }
            let username = wanted.username.trim();
            // `auth` is `user:password`: a colon would end the user early.
            if username.contains(':') {
                return Err(ValidationError::InvalidChars {
                    field: Field::Username,
                    value: username.to_owned(),
                });
            }
            if username.is_empty() && !wanted.password.is_empty() {
                return Err(ValidationError::Empty(Field::Username));
            }
            let original = edit.original.as_deref().and_then(|name| self.entry(name));
            let mut entry = original.cloned().unwrap_or_default();
            let current = original
                .map(|entry| registry(server, entry))
                .unwrap_or_default();
            if current.username != username || current.password != wanted.password {
                set_credentials(&mut entry, username, &wanted.password);
            }
            let email = wanted.email.trim();
            if current.email != email {
                if email.is_empty() {
                    entry.remove(EMAIL);
                } else {
                    entry.insert(EMAIL.to_owned(), Value::String(email.to_owned()));
                }
            }
            auths.insert(server.to_owned(), Value::Object(entry));
        }
        let root = if self.legacy {
            auths
        } else {
            let mut root = self.root.clone();
            root.insert(AUTHS.to_owned(), Value::Object(auths));
            root
        };
        Ok(Self {
            legacy: self.legacy,
            root,
        })
    }

    /// The config as the Secret's value. It carries passwords.
    pub fn to_bytes(&self) -> SecretBytes {
        SecretBytes::new(serde_json::to_vec(&self.root).unwrap_or_default())
    }
}

fn text<'a>(entry: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

/// What an entry says about its registry. `username` and `password` win
/// over what `auth` holds, as docker reads them.
fn registry(server: &str, entry: &Map<String, Value>) -> Registry {
    let (auth_user, auth_password) = text(entry, AUTH).and_then(decode_auth).unzip();
    Registry {
        server: server.to_owned(),
        username: text(entry, USERNAME)
            .filter(|user| !user.is_empty())
            .map(str::to_owned)
            .or(auth_user)
            .unwrap_or_default(),
        email: text(entry, EMAIL).unwrap_or_default().to_owned(),
        password: text(entry, PASSWORD)
            .filter(|password| !password.is_empty())
            .map(SecretBytes::from)
            .or(auth_password)
            .unwrap_or_default(),
    }
}

/// User and password of an `auth` value (`base64(user:password)`).
fn decode_auth(auth: &str) -> Option<(String, SecretBytes)> {
    let mut decoded = STANDARD.decode(auth.trim()).ok()?;
    let split = decoded.iter().position(|&b| b == b':');
    let result = split.and_then(|at| {
        let user = String::from_utf8(decoded[..at].to_vec()).ok()?;
        Some((user, SecretBytes::new(decoded[at + 1..].to_vec())))
    });
    wipe(&mut decoded);
    result
}

/// `auth` for `username` and `password`.
pub fn encode_auth(username: &str, password: &SecretBytes) -> String {
    let mut plain = Vec::with_capacity(username.len() + 1 + password.len());
    plain.extend_from_slice(username.as_bytes());
    plain.push(b':');
    plain.extend_from_slice(password.expose());
    let auth = STANDARD.encode(&plain);
    wipe(&mut plain);
    auth
}

/// Writes a new user and password into `entry`. `auth` is always rewritten;
/// `username` and `password` are written too, as `kubectl create secret
/// docker-registry` does, unless the entry had only `auth`. Without a user
/// and a password all three go.
fn set_credentials(entry: &mut Map<String, Value>, username: &str, password: &SecretBytes) {
    if username.is_empty() && password.is_empty() {
        for key in [USERNAME, PASSWORD, AUTH] {
            entry.remove(key);
        }
        return;
    }
    let auth_only =
        entry.contains_key(AUTH) && !entry.contains_key(USERNAME) && !entry.contains_key(PASSWORD);
    if !auth_only {
        entry.insert(USERNAME.to_owned(), Value::String(username.to_owned()));
        entry.insert(
            PASSWORD.to_owned(),
            Value::String(String::from_utf8_lossy(password.expose()).into_owned()),
        );
    }
    entry.insert(
        AUTH.to_owned(),
        Value::String(encode_auth(username, password)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: &Value) -> DockerConfig {
        DockerConfig::parse(&serde_json::to_vec(value).unwrap(), false).unwrap()
    }

    fn json_of(config: &DockerConfig) -> Value {
        serde_json::from_slice(config.to_bytes().expose()).unwrap()
    }

    fn edits(config: &DockerConfig) -> Vec<RegistryEdit> {
        config
            .registries()
            .into_iter()
            .map(|registry| RegistryEdit {
                original: Some(registry.server.clone()),
                registry,
            })
            .collect()
    }

    fn sample() -> Value {
        json!({
            "auths": {
                "ghcr.io": {
                    "username": "bot", "password": "s3cret", "email": "bot@example.com",
                    "auth": STANDARD.encode("bot:s3cret")
                },
                "registry.example.com": {"auth": STANDARD.encode("alice:pa:55")},
                "token.example.com": {"identitytoken": "idt-9", "extra": {"a": [1, 2]}},
                "public.example.com": {}
            },
            "credHelpers": {"aws.example.com": "ecr-login"},
            "credsStore": "osxkeychain",
            "HttpHeaders": {"User-Agent": "test"}
        })
    }

    #[test]
    fn reads_registries_in_order() {
        let config = parse(&sample());
        let registries = config.registries();
        assert_eq!(
            registries
                .iter()
                .map(|r| r.server.as_str())
                .collect::<Vec<_>>(),
            [
                "ghcr.io",
                "registry.example.com",
                "token.example.com",
                "public.example.com"
            ]
        );
        assert_eq!(registries[0].username, "bot");
        assert_eq!(registries[0].email, "bot@example.com");
        assert_eq!(registries[0].password.expose(), b"s3cret");
        // Only `auth`: the user is up to the first colon.
        assert_eq!(registries[1].username, "alice");
        assert_eq!(registries[1].password.expose(), b"pa:55");
        assert_eq!(
            registries[2],
            Registry {
                server: "token.example.com".into(),
                ..Registry::default()
            }
        );
    }

    #[test]
    fn debug_shows_no_values() {
        let config = parse(&sample());
        let printed = format!("{config:?} {:?}", config.registries());
        assert!(printed.contains("ghcr.io"));
        for hidden in ["s3cret", "pa:55", "idt-9", &STANDARD.encode("bot:s3cret")] {
            assert!(!printed.contains(hidden), "{printed}");
        }
    }

    #[test]
    fn untouched_round_trip_keeps_everything() {
        let config = parse(&sample());
        let same = config.with_registries(&edits(&config)).unwrap();
        assert_eq!(same, config);
        assert_eq!(json_of(&same), sample());
        // Key order is kept too.
        assert_eq!(
            same.to_bytes().expose(),
            serde_json::to_vec(&sample()).unwrap()
        );
    }

    #[test]
    fn changed_password_rewrites_auth_and_keeps_unknown_fields() {
        let mut source = sample();
        source["auths"]["ghcr.io"]["identitytoken"] = json!("keep-me");
        let config = parse(&source);
        let mut list = edits(&config);
        list[0].registry.password = SecretBytes::from("n3w");
        list[1].registry.username = "bob".into();
        let changed = config.with_registries(&list).unwrap();
        let mut expected = source.clone();
        expected["auths"]["ghcr.io"]["password"] = json!("n3w");
        expected["auths"]["ghcr.io"]["auth"] = json!(STANDARD.encode("bot:n3w"));
        // An entry with only `auth` stays that way.
        expected["auths"]["registry.example.com"] = json!({"auth": STANDARD.encode("bob:pa:55")});
        assert_eq!(json_of(&changed), expected);
        assert_ne!(changed, config);
    }

    #[test]
    fn email_alone_leaves_credentials() {
        // An `auth` Kubach can't decode is not touched by an email edit.
        let source = json!({"auths": {"r.example.com": {"auth": "not base64!", "email": "a@b.c"}}});
        let config = parse(&source);
        let mut list = edits(&config);
        assert_eq!(list[0].registry.username, "");
        list[0].registry.email = " new@b.c ".into();
        let changed = config.with_registries(&list).unwrap();
        assert_eq!(
            json_of(&changed),
            json!({"auths": {"r.example.com": {"auth": "not base64!", "email": "new@b.c"}}})
        );
        list[0].registry.email.clear();
        let cleared = config.with_registries(&list).unwrap();
        assert_eq!(
            json_of(&cleared),
            json!({"auths": {"r.example.com": {"auth": "not base64!"}}})
        );
    }

    #[test]
    fn add_rename_remove() {
        let config = parse(&sample());
        let mut list = edits(&config);
        // Remove ghcr.io, rename the token registry, add a new one.
        list.remove(0);
        list[1].registry.server = " renamed.example.com ".into();
        list.push(RegistryEdit {
            original: None,
            registry: Registry {
                server: "new.example.com".into(),
                username: "carol".into(),
                email: String::new(),
                password: SecretBytes::from("pw"),
            },
        });
        let changed = json_of(&config.with_registries(&list).unwrap());
        let source = sample();
        assert_eq!(
            changed["auths"],
            json!({
                "registry.example.com": source["auths"]["registry.example.com"],
                "renamed.example.com": source["auths"]["token.example.com"],
                "public.example.com": {},
                "new.example.com": {
                    "username": "carol", "password": "pw",
                    "auth": STANDARD.encode("carol:pw")
                }
            })
        );
        for key in ["credHelpers", "credsStore", "HttpHeaders"] {
            assert_eq!(changed[key], source[key]);
        }
    }

    #[test]
    fn credentials_for_an_entry_without_them() {
        let config = parse(&sample());
        let mut list = edits(&config);
        list[2].registry.username = "dave".into();
        list[2].registry.password = SecretBytes::from("pw");
        let changed = json_of(&config.with_registries(&list).unwrap());
        assert_eq!(
            changed["auths"]["token.example.com"],
            json!({
                "identitytoken": "idt-9", "extra": {"a": [1, 2]},
                "username": "dave", "password": "pw", "auth": STANDARD.encode("dave:pw")
            })
        );
        // Clearing user and password removes all three fields.
        let mut list = edits(&config);
        list[0].registry.username.clear();
        list[0].registry.password = SecretBytes::default();
        let cleared = json_of(&config.with_registries(&list).unwrap());
        assert_eq!(
            cleared["auths"]["ghcr.io"],
            json!({"email": "bot@example.com"})
        );
    }

    #[test]
    fn checks_the_list() {
        let config = DockerConfig::empty(false);
        let edit = |server: &str, user: &str, password: &str| RegistryEdit {
            original: None,
            registry: Registry {
                server: server.into(),
                username: user.into(),
                email: String::new(),
                password: SecretBytes::from(password),
            },
        };
        assert_eq!(
            config.with_registries(&[edit(" ", "u", "p")]),
            Err(ValidationError::Empty(Field::RegistryServer))
        );
        assert_eq!(
            config.with_registries(&[edit("a.io", "u", "p"), edit("a.io ", "u", "p")]),
            Err(ValidationError::Duplicate {
                field: Field::RegistryServer,
                value: "a.io".into()
            })
        );
        assert_eq!(
            config.with_registries(&[edit("a.io", "", "p")]),
            Err(ValidationError::Empty(Field::Username))
        );
        assert!(matches!(
            config.with_registries(&[edit("a.io", "u:x", "p")]),
            Err(ValidationError::InvalidChars {
                field: Field::Username,
                ..
            })
        ));
        // From scratch: the layout kubectl writes.
        let built = config.with_registries(&[edit("a.io", "u", "p")]).unwrap();
        assert_eq!(
            json_of(&built),
            json!({"auths": {"a.io": {
                "username": "u", "password": "p", "auth": STANDARD.encode("u:p")
            }}})
        );
        // No registries at all is a valid (empty) config.
        assert_eq!(
            json_of(&built.with_registries(&[]).unwrap()),
            json!({"auths": {}})
        );
    }

    #[test]
    fn legacy_dockercfg_layout() {
        let source = json!({
            "quay.io": {"username": "u", "password": "p", "email": "u@example.com",
                        "auth": STANDARD.encode("u:p")},
            "old.example.com": {"auth": STANDARD.encode("x:y"), "note": 1}
        });
        let bytes = serde_json::to_vec(&source).unwrap();
        let config = DockerConfig::of_kind(TypedKind::DockerCfg, &bytes)
            .unwrap()
            .unwrap();
        assert!(config.legacy());
        let mut list = edits(&config);
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].registry.username, "x");
        assert_eq!(json_of(&config.with_registries(&list).unwrap()), source);
        list[0].registry.password = SecretBytes::from("p2");
        list.remove(1);
        // The registry map stays the root: no `auths` wrapper.
        assert_eq!(
            json_of(&config.with_registries(&list).unwrap()),
            json!({"quay.io": {
                "username": "u", "password": "p2", "email": "u@example.com",
                "auth": STANDARD.encode("u:p2")
            }})
        );
        assert!(DockerConfig::of_kind(TypedKind::Tls, &bytes).is_none());
    }

    #[test]
    fn broken_json_is_an_error() {
        let invalid = Err(ValidationError::Json(Field::DockerConfig));
        for (bytes, legacy) in [
            (&b"not json"[..], false),
            (b"[]", false),
            (b"\"text\"", true),
            (br#"{"auths": []}"#, false),
            (br#"{"auths": {"a.io": "token"}}"#, false),
            (br#"{"a.io": 1}"#, true),
            (b"{\"auths\": {", false),
        ] {
            assert_eq!(DockerConfig::parse(bytes, legacy), invalid, "{bytes:?}");
        }
        // No `auths`: a config with credential helpers only.
        let helpers = DockerConfig::parse(br#"{"credsStore": "desktop"}"#, false).unwrap();
        assert!(helpers.registries().is_empty());
    }

    #[test]
    fn secret_fields() {
        let config = parse(&sample());
        let secret = |server: &str, field| config.secret(server, field);
        assert_eq!(
            secret("ghcr.io", RegistrySecret::Password)
                .unwrap()
                .expose(),
            b"s3cret"
        );
        assert_eq!(
            secret("ghcr.io", RegistrySecret::Auth).unwrap().expose(),
            STANDARD.encode("bot:s3cret").as_bytes()
        );
        assert_eq!(
            secret("registry.example.com", RegistrySecret::Password)
                .unwrap()
                .expose(),
            b"pa:55"
        );
        assert!(config.has_secret("registry.example.com", RegistrySecret::Auth));
        assert!(!config.has_secret("token.example.com", RegistrySecret::Password));
        assert!(!config.has_secret("public.example.com", RegistrySecret::Auth));
        assert!(secret("missing.example.com", RegistrySecret::Auth).is_none());
        assert_eq!(encode_auth("u", &SecretBytes::from("p")), "dTpw");
    }
}
