//! Server-side identity of a session (ADR 0005, second line of defense).
//!
//! - **Impersonation**: the user's credentials plus `Impersonate-User`/
//!   `-Group` of an administrator-prepared identity and
//!   `Impersonate-Extra-kubach.io/session`. Nothing is created.
//! - **Temporary ServiceAccount**: `kubach-mcp-<session>` with a Role that
//!   holds exactly the rights the user granted so far (empty at the start,
//!   patched on every grant and revocation, #227) and a RoleBinding (owned by the SA, so deleting the SA removes
//!   everything), and a short-lived token from `TokenRequest`, refreshed in
//!   memory.
//! - **Client only**: the user's credentials.
//!
//! Every client carries `User-Agent: kubach-mcp/<version> session/<id>` for
//! correlation with the API server's audit log. Tokens never leave memory.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use http::{HeaderName, HeaderValue};
use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use k8s_openapi::api::core::v1::ServiceAccount;
use k8s_openapi::api::rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use k8s_openapi::jiff::Timestamp;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::config::AuthInfo;
use kube::{Client, Config};
use secrecy::SecretString;
use tokio::task::JoinHandle;

use crate::access::Access;
use crate::grants::Grants;
use crate::rules::grant_rules;
use crate::scope::{SessionMode, SessionScope};
use kubach_core::sync;

pub const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";
pub const MANAGED_BY_VALUE: &str = "kubach";
pub const SESSION_LABEL: &str = "kubach.io/session";
pub const EXPIRES_AT_ANNOTATION: &str = "kubach.io/expires-at";
pub const CREATED_BY_ANNOTATION: &str = "kubach.io/created-by";
/// Extra key sent with impersonation; percent-encoded as the API requires.
pub const SESSION_EXTRA_HEADER: &str = "impersonate-extra-kubach.io%2fsession";

/// Shortest token lifetime the API server accepts.
const MIN_TOKEN_SECONDS: i64 = 600;
const MAX_TOKEN_SECONDS: i64 = 3600;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("API request failed: {0}")]
    Api(#[source] Box<kube::Error>),
    #[error("invalid header value: {0}")]
    Header(String),
    #[error("the API server returned no token")]
    NoToken,
}

impl From<kube::Error> for IdentityError {
    fn from(error: kube::Error) -> Self {
        Self::Api(Box::new(error))
    }
}

/// `kubach-mcp/<version> session/<id>`.
pub fn user_agent(session_id: &str) -> String {
    format!("kubach-mcp/{} session/{session_id}", kubach_core::VERSION)
}

fn header(name: &'static str, value: &str) -> Result<(HeaderName, HeaderValue), IdentityError> {
    let value = HeaderValue::from_str(value).map_err(|e| IdentityError::Header(e.to_string()))?;
    Ok((HeaderName::from_static(name), value))
}

/// `base` with the session's User-Agent.
pub fn session_config(base: &Config, session_id: &str) -> Result<Config, IdentityError> {
    let mut config = base.clone();
    config
        .headers
        .retain(|(name, _)| name != http::header::USER_AGENT);
    config
        .headers
        .push(header("user-agent", &user_agent(session_id))?);
    Ok(config)
}

/// `base` impersonating `user`/`groups`; the session extra only when the
/// user may impersonate it (otherwise the API server rejects every call).
pub fn impersonation_config(
    base: &Config,
    session_id: &str,
    user: &str,
    groups: &[String],
    with_session_extra: bool,
) -> Result<Config, IdentityError> {
    let mut config = session_config(base, session_id)?;
    config.auth_info.impersonate = Some(user.to_owned());
    config.auth_info.impersonate_groups = (!groups.is_empty()).then(|| groups.to_vec());
    if with_session_extra {
        config
            .headers
            .push(header(SESSION_EXTRA_HEADER, session_id)?);
    }
    Ok(config)
}

/// `base`'s cluster (URL, CA, proxy) with only a bearer token as
/// credentials: client certificates, exec plugins and impersonation of the
/// user are dropped.
pub fn token_config(base: &Config, session_id: &str, token: &str) -> Result<Config, IdentityError> {
    let mut config = session_config(base, session_id)?;
    config.auth_info = AuthInfo {
        token: Some(SecretString::from(token.to_owned())),
        ..AuthInfo::default()
    };
    Ok(config)
}

fn client(config: Config) -> Result<Client, IdentityError> {
    kubach_core::redact::client(config).map_err(IdentityError::from)
}

/// The identity a session's API calls run under.
pub enum Identity {
    ClientOnly {
        client: Client,
    },
    Impersonation {
        client: Client,
        user: String,
        groups: Vec<String>,
    },
    TemporaryServiceAccount(TemporaryServiceAccount),
}

impl Identity {
    pub fn mode(&self) -> SessionMode {
        match self {
            Self::ClientOnly { .. } => SessionMode::ClientOnly,
            Self::Impersonation { .. } => SessionMode::Impersonation,
            Self::TemporaryServiceAccount(_) => SessionMode::TemporaryServiceAccount,
        }
    }

    /// Client for the next call (a refreshed token replaces it).
    pub fn client(&self) -> Client {
        match self {
            Self::ClientOnly { client } | Self::Impersonation { client, .. } => client.clone(),
            Self::TemporaryServiceAccount(sa) => sa.client(),
        }
    }

    /// For the audit log: `user`, `impersonated:<name>`,
    /// `serviceaccount:<ns>/<name>`.
    pub fn describe(&self) -> String {
        match self {
            Self::ClientOnly { .. } => "user".into(),
            Self::Impersonation { user, .. } => format!("impersonated:{user}"),
            Self::TemporaryServiceAccount(sa) => {
                format!("serviceaccount:{}/{}", sa.namespace, sa.name)
            }
        }
    }

    /// Mirrors the session's grants on the server (#227): the Role of a
    /// temporary ServiceAccount gets exactly the granted rights within the
    /// session's `access` (#15). Other identities have nothing to change:
    /// impersonation stays within the administrator's RBAC, client-only
    /// within Kubach.
    pub async fn sync_grants(&self, grants: &Grants, access: Access) -> Result<(), IdentityError> {
        match self {
            Self::TemporaryServiceAccount(sa) => {
                sa.set_rules(grant_rules(grants, &sa.namespace, access))
                    .await
            }
            _ => Ok(()),
        }
    }

    /// Revokes server-side rights: deletes a temporary ServiceAccount.
    pub async fn end(&self) -> Result<(), IdentityError> {
        match self {
            Self::TemporaryServiceAccount(sa) => sa.delete().await,
            _ => Ok(()),
        }
    }
}

/// Labels of every object a session creates.
fn session_labels(session_id: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (MANAGED_BY_LABEL.to_owned(), MANAGED_BY_VALUE.to_owned()),
        (SESSION_LABEL.to_owned(), session_id.to_owned()),
    ])
}

/// Token lifetime for the remaining session time, within the API's bounds.
pub fn token_seconds(remaining: Duration) -> i64 {
    let wanted = i64::try_from(remaining.as_secs())
        .unwrap_or(i64::MAX)
        .saturating_add(60);
    wanted.clamp(MIN_TOKEN_SECONDS, MAX_TOKEN_SECONDS)
}

/// When to refresh a token valid for `lifetime`: at 80%, at least 30 s early.
pub fn refresh_after(lifetime: Duration) -> Duration {
    let at = lifetime.mul_f64(0.8);
    at.min(lifetime.saturating_sub(Duration::from_secs(30)))
}

/// A per-session ServiceAccount with a Role and a RoleBinding.
pub struct TemporaryServiceAccount {
    pub namespace: String,
    pub name: String,
    user_client: Client,
    client: Arc<RwLock<Client>>,
    refresher: Mutex<Option<JoinHandle<()>>>,
}

impl TemporaryServiceAccount {
    /// Creates SA, Role and RoleBinding as the user and fetches a token.
    /// Partially created objects are deleted on failure.
    pub async fn create(
        user_client: &Client,
        base: &Config,
        scope: &SessionScope,
        namespace: &str,
    ) -> Result<Self, IdentityError> {
        let name = format!("kubach-mcp-{}", scope.id);
        let annotations = BTreeMap::from([
            (
                EXPIRES_AT_ANNOTATION.to_owned(),
                scope.expires_at.to_string(),
            ),
            (CREATED_BY_ANNOTATION.to_owned(), user_agent(&scope.id)),
            ("kubach.io/profile".to_owned(), scope.profile.to_string()),
        ]);
        let meta = |owner: Option<OwnerReference>| ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(namespace.to_owned()),
            labels: Some(session_labels(&scope.id)),
            annotations: Some(annotations.clone()),
            owner_references: owner.map(|o| vec![o]),
            ..Default::default()
        };
        let accounts: Api<ServiceAccount> = Api::namespaced(user_client.clone(), namespace);
        let account = accounts
            .create(
                &PostParams::default(),
                &ServiceAccount {
                    metadata: meta(None),
                    automount_service_account_token: Some(false),
                    ..Default::default()
                },
            )
            .await?;
        let sa = Self {
            namespace: namespace.to_owned(),
            name: name.clone(),
            user_client: user_client.clone(),
            client: Arc::new(RwLock::new(user_client.clone())),
            refresher: Mutex::new(None),
        };
        match sa.finish_setup(base, scope, &account, meta).await {
            Ok(()) => Ok(sa),
            Err(error) => {
                let _ = sa.delete().await;
                Err(error)
            }
        }
    }

    async fn finish_setup(
        &self,
        base: &Config,
        scope: &SessionScope,
        account: &ServiceAccount,
        meta: impl Fn(Option<OwnerReference>) -> ObjectMeta,
    ) -> Result<(), IdentityError> {
        let owner = OwnerReference {
            api_version: "v1".into(),
            kind: "ServiceAccount".into(),
            name: self.name.clone(),
            uid: account.metadata.uid.clone().unwrap_or_default(),
            ..Default::default()
        };
        let roles: Api<Role> = Api::namespaced(self.user_client.clone(), &self.namespace);
        roles
            .create(
                &PostParams::default(),
                &Role {
                    metadata: meta(Some(owner.clone())),
                    // Empty until the user grants a right (#227).
                    rules: Some(grant_rules(&Grants::new(), &self.namespace, Access::NONE)),
                },
            )
            .await?;
        let bindings: Api<RoleBinding> = Api::namespaced(self.user_client.clone(), &self.namespace);
        bindings
            .create(
                &PostParams::default(),
                &RoleBinding {
                    metadata: meta(Some(owner)),
                    role_ref: RoleRef {
                        api_group: Some("rbac.authorization.k8s.io".into()),
                        kind: "Role".into(),
                        name: self.name.clone(),
                    },
                    subjects: Some(vec![Subject {
                        kind: "ServiceAccount".into(),
                        name: self.name.clone(),
                        namespace: Some(self.namespace.clone()),
                        api_group: None,
                    }]),
                },
            )
            .await?;

        let remaining = scope
            .deadline()
            .saturating_duration_since(std::time::Instant::now());
        let (token, lifetime) =
            request_token(&self.user_client, &self.namespace, &self.name, remaining).await?;
        *sync::write(&self.client) = client(token_config(base, &scope.id, &token)?)?;

        let refresher = tokio::spawn(refresh_loop(
            self.user_client.clone(),
            base.clone(),
            scope.id.clone(),
            self.namespace.clone(),
            self.name.clone(),
            scope.deadline(),
            Arc::clone(&self.client),
            lifetime,
        ));
        *sync::lock(&self.refresher) = Some(refresher);
        Ok(())
    }

    pub fn client(&self) -> Client {
        sync::read(&self.client).clone()
    }

    /// A fresh short-lived token of the ServiceAccount, for a `helm`
    /// process Kubach runs in this namespace (#418). It goes to helm through
    /// its environment only, never to the agent, a file or the arguments.
    pub async fn helm_token(&self) -> Result<String, IdentityError> {
        let (token, _) = request_token(
            &self.user_client,
            &self.namespace,
            &self.name,
            HELM_TOKEN_LIFETIME,
        )
        .await?;
        Ok(token)
    }

    /// Replaces the rules of the session's Role, as the user.
    pub async fn set_rules(&self, rules: Vec<PolicyRule>) -> Result<(), IdentityError> {
        let roles: Api<Role> = Api::namespaced(self.user_client.clone(), &self.namespace);
        // A merge patch replaces the whole list.
        let patch = serde_json::json!({ "rules": rules });
        roles
            .patch(&self.name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        Ok(())
    }

    /// Stops refreshing and deletes the SA; GC removes the Role and
    /// RoleBinding, and the SA's tokens stop working at once.
    pub async fn delete(&self) -> Result<(), IdentityError> {
        if let Some(task) = sync::lock(&self.refresher).take() {
            task.abort();
        }
        let accounts: Api<ServiceAccount> =
            Api::namespaced(self.user_client.clone(), &self.namespace);
        match accounts
            .delete(&self.name, &DeleteParams::background())
            .await
        {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(status)) if status.is_not_found() => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for TemporaryServiceAccount {
    fn drop(&mut self) {
        if let Some(task) = sync::lock(&self.refresher).take() {
            task.abort();
        }
    }
}

/// Lifetime asked for a token handed to `helm`: the shortest the API
/// server issues.
const HELM_TOKEN_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// A kubeconfig with `base`'s cluster only (server, CA, TLS name) and a
/// user without credentials: `helm` gets the token from `HELM_KUBETOKEN`
/// (#418), so neither the user's credentials nor the token touch the disk.
pub fn cluster_kubeconfig(base: &Config) -> String {
    use base64::Engine as _;
    let engine = base64::engine::general_purpose::STANDARD;
    let mut cluster = serde_json::Map::new();
    cluster.insert(
        "server".into(),
        serde_json::Value::String(
            base.cluster_url
                .to_string()
                .trim_end_matches('/')
                .to_owned(),
        ),
    );
    if let Some(certs) = &base.root_cert {
        let mut pem = String::new();
        for der in certs {
            pem.push_str("-----BEGIN CERTIFICATE-----\n");
            let encoded = engine.encode(der);
            for line in encoded.as_bytes().chunks(64) {
                pem.push_str(&String::from_utf8_lossy(line));
                pem.push('\n');
            }
            pem.push_str("-----END CERTIFICATE-----\n");
        }
        cluster.insert(
            "certificate-authority-data".into(),
            serde_json::Value::String(engine.encode(pem)),
        );
    }
    if base.accept_invalid_certs {
        cluster.insert("insecure-skip-tls-verify".into(), true.into());
    }
    if let Some(name) = &base.tls_server_name {
        cluster.insert("tls-server-name".into(), name.clone().into());
    }
    if let Some(proxy) = &base.proxy_url {
        cluster.insert("proxy-url".into(), proxy.to_string().into());
    }
    let config = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Config",
        "clusters": [{"name": "kubach", "cluster": cluster}],
        "users": [{"name": "kubach", "user": {}}],
        "contexts": [{"name": "kubach", "context": {"cluster": "kubach", "user": "kubach"}}],
        "current-context": "kubach",
    });
    // JSON is YAML; client-go reads either.
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

/// Requests a token; returns it with its actual lifetime.
async fn request_token(
    user_client: &Client,
    namespace: &str,
    name: &str,
    remaining: Duration,
) -> Result<(String, Duration), IdentityError> {
    let accounts: Api<ServiceAccount> = Api::namespaced(user_client.clone(), namespace);
    let seconds = token_seconds(remaining);
    let request = TokenRequest {
        spec: Some(TokenRequestSpec {
            audiences: None,
            expiration_seconds: Some(seconds),
            bound_object_ref: None,
        }),
        ..Default::default()
    };
    let response = accounts
        .create_token_request(name, &PostParams::default(), &request)
        .await?;
    let status = response.status.ok_or(IdentityError::NoToken)?;
    let token = status
        .token
        .filter(|t| !t.is_empty())
        .ok_or(IdentityError::NoToken)?;
    let now = Timestamp::now();
    let lifetime = status
        .expiration_timestamp
        .and_then(|expires| u64::try_from(expires.0.as_second() - now.as_second()).ok())
        .unwrap_or(seconds as u64);
    Ok((token, Duration::from_secs(lifetime)))
}

#[allow(clippy::too_many_arguments)]
async fn refresh_loop(
    user_client: Client,
    base: Config,
    session_id: String,
    namespace: String,
    name: String,
    deadline: std::time::Instant,
    target: Arc<RwLock<Client>>,
    mut lifetime: Duration,
) {
    loop {
        tokio::time::sleep(refresh_after(lifetime)).await;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        let fresh = async {
            let (token, new_lifetime) =
                request_token(&user_client, &namespace, &name, remaining).await?;
            let client = client(token_config(&base, &session_id, &token)?)?;
            Ok::<_, IdentityError>((client, new_lifetime))
        };
        match fresh.await {
            Ok((client, new_lifetime)) => {
                *sync::write(&target) = client;
                lifetime = new_lifetime;
            }
            // Retry soon; the old token is still valid for a while.
            Err(_) => lifetime = Duration::from_secs(60),
        }
    }
}

/// Whether a session object is past its `kubach.io/expires-at`. Objects
/// without a readable expiry count as expired: they are ours and orphaned.
pub fn is_expired(meta: &ObjectMeta, now: Timestamp) -> bool {
    meta.annotations
        .as_ref()
        .and_then(|a| a.get(EXPIRES_AT_ANNOTATION))
        .and_then(|value| value.parse::<Timestamp>().ok())
        .is_none_or(|expires| expires <= now)
}

/// Deletes expired session ServiceAccounts in `namespace` (Role and
/// RoleBinding follow via GC). Returns the deleted names.
pub async fn sweep_expired(
    user_client: &Client,
    namespace: &str,
) -> Result<Vec<String>, IdentityError> {
    let accounts: Api<ServiceAccount> = Api::namespaced(user_client.clone(), namespace);
    let selector = format!("{MANAGED_BY_LABEL}={MANAGED_BY_VALUE},{SESSION_LABEL}");
    let list = accounts
        .list(&ListParams::default().labels(&selector))
        .await?;
    let now = Timestamp::now();
    let mut deleted = Vec::new();
    for account in list.items {
        if !is_expired(&account.metadata, now) {
            continue;
        }
        let Some(name) = account.metadata.name else {
            continue;
        };
        match accounts.delete(&name, &DeleteParams::background()).await {
            Ok(_) => deleted.push(name),
            Err(kube::Error::Api(status)) if status.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config::new("https://127.0.0.1:6443".parse().unwrap())
    }

    #[test]
    fn user_agent_names_the_session() {
        let config = session_config(&base(), "abc").unwrap();
        let ua: Vec<_> = config
            .headers
            .iter()
            .filter(|(name, _)| name == http::header::USER_AGENT)
            .collect();
        assert_eq!(ua.len(), 1);
        assert!(ua[0].1.to_str().unwrap().ends_with("session/abc"));
    }

    #[test]
    fn impersonation_sets_user_groups_and_extra() {
        let groups = vec!["kubach:mcp".to_string()];
        let config =
            impersonation_config(&base(), "abc", "kubach:mcp:web:debug", &groups, true).unwrap();
        assert_eq!(
            config.auth_info.impersonate.as_deref(),
            Some("kubach:mcp:web:debug")
        );
        assert_eq!(config.auth_info.impersonate_groups, Some(groups));
        assert!(
            config
                .headers
                .iter()
                .any(|(name, value)| name == SESSION_EXTRA_HEADER && value == "abc")
        );
        let without = impersonation_config(&base(), "abc", "u", &[], false).unwrap();
        assert!(without.auth_info.impersonate_groups.is_none());
        assert!(
            !without
                .headers
                .iter()
                .any(|(name, _)| name == SESSION_EXTRA_HEADER)
        );
    }

    #[test]
    fn token_config_drops_user_credentials() {
        let mut user = base();
        user.auth_info.client_certificate_data = Some("cert".into());
        user.auth_info.impersonate = Some("admin".into());
        user.auth_info.exec = None;
        let config = token_config(&user, "abc", "t0k3n").unwrap();
        assert!(config.auth_info.client_certificate_data.is_none());
        assert!(config.auth_info.impersonate.is_none());
        assert!(config.auth_info.token.is_some());
        assert_eq!(config.cluster_url, user.cluster_url);
    }

    #[test]
    fn token_lifetimes_are_bounded() {
        assert_eq!(token_seconds(Duration::from_secs(30)), MIN_TOKEN_SECONDS);
        assert_eq!(token_seconds(Duration::from_secs(1200)), 1260);
        assert_eq!(
            token_seconds(Duration::from_secs(8 * 3600)),
            MAX_TOKEN_SECONDS
        );
        assert_eq!(
            refresh_after(Duration::from_secs(600)),
            Duration::from_secs(480)
        );
        assert_eq!(
            refresh_after(Duration::from_secs(100)),
            Duration::from_secs(70)
        );
    }

    #[test]
    fn expiry_annotation() {
        let now: Timestamp = "2026-10-22T12:00:00Z".parse().unwrap();
        let meta = |expires: Option<&str>| ObjectMeta {
            annotations: expires
                .map(|e| BTreeMap::from([(EXPIRES_AT_ANNOTATION.to_owned(), e.to_owned())])),
            ..Default::default()
        };
        assert!(is_expired(&meta(Some("2026-10-22T11:59:59Z")), now));
        assert!(!is_expired(&meta(Some("2026-10-22T12:30:00Z")), now));
        assert!(is_expired(&meta(Some("garbage")), now));
        assert!(is_expired(&meta(None), now));
    }

    #[test]
    fn session_objects_are_labeled() {
        let labels = session_labels("abc");
        assert_eq!(labels[MANAGED_BY_LABEL], "kubach");
        assert_eq!(labels[SESSION_LABEL], "abc");
    }
}
