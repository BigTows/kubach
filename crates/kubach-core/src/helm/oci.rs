//! Versions of a chart in an OCI registry (`oci://host/path/chart`, #301).
//!
//! Helm pushes chart versions as tags of the repository (with `+` written
//! as `_`, which tags can't hold), so the version list is the tag list of
//! the OCI Distribution API: `GET /v2/<repository>/tags/list`, page by page
//! along `Link: <…>; rel="next"`. Registries answer an anonymous request
//! with `401` and a `WWW-Authenticate` challenge; a `Bearer` one names the
//! token service (Docker Hub: `auth.docker.io`) that hands out a pull token,
//! anonymously for public charts. Only when that is refused are the
//! credentials of `helm registry login` (or `docker login`) used; they are
//! sent to the registry and its token service only, never logged.
//!
//! Credential helpers (`credsStore`, `credHelpers`) are not run: their
//! logins stay with helm, which still installs the chart.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use futures::StreamExt as _;
use http_body_util::BodyExt as _;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use super::repo::{HelmPaths, Platform, compare_versions, parse_version};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// A page of tags or a token; tag lists of big charts are ~100 KiB.
const BODY_LIMIT: u64 = 4 * 1024 * 1024;
/// Pages followed at most (1000 tags each).
const MAX_PAGES: usize = 20;

/// `oci://<registry>/<repository>[:tag|@digest]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OciReference {
    /// Host with an optional port: `registry-1.docker.io`, `localhost:5000`.
    pub registry: String,
    /// Path of the chart in the registry: `bitnamicharts/postgresql`.
    pub repository: String,
    /// A tag (`:1.2.3`) or digest (`@sha256:…`) given in the reference.
    pub tag: Option<String>,
}

impl OciReference {
    /// Last segment of the repository: the chart name.
    pub fn chart_name(&self) -> &str {
        self.repository
            .rsplit('/')
            .next()
            .unwrap_or(&self.repository)
    }
}

/// Parses an `oci://` chart reference; `None` for anything else or a
/// reference without a repository path.
pub fn parse_reference(input: &str) -> Option<OciReference> {
    let input = input.trim();
    let rest = input
        .get(..6)
        .filter(|scheme| scheme.eq_ignore_ascii_case("oci://"))
        .map(|_| &input[6..])?;
    if rest
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '?' | '#' | '\\'))
    {
        return None;
    }
    let (registry, path) = rest.split_once('/')?;
    let path = path.trim_end_matches('/');
    if registry.is_empty() || registry.contains('@') || path.is_empty() {
        return None;
    }
    let (path, digest) = match path.split_once('@') {
        Some((path, digest)) => (path, Some(digest)),
        None => (path, None),
    };
    // A tag is a `:` in the last segment (a registry port is before `/`).
    let last_start = path.rfind('/').map_or(0, |i| i + 1);
    let (repository, tag) = match path[last_start..].split_once(':') {
        Some((name, tag)) => (&path[..last_start + name.len()], Some(tag)),
        None => (path, None),
    };
    if repository.is_empty()
        || repository.ends_with('/')
        || repository.split('/').any(str::is_empty)
    {
        return None;
    }
    let tag = digest.or(tag).filter(|t| !t.is_empty()).map(str::to_owned);
    Some(OciReference {
        registry: registry.to_owned(),
        repository: repository.to_owned(),
        tag,
    })
}

/// Release name for a chart typed in: the chart name from the last segment
/// of an `oci://` reference, `repo/chart`, a directory, or a packaged chart
/// (`name-1.2.3.tgz`, path or URL, without version and extension). Made a
/// valid release name (lowercase, `_` as `-`); `None` when that fails.
pub fn release_name(chart: &str) -> Option<String> {
    let chart = chart.trim();
    let name = if let Some(reference) = parse_reference(chart) {
        reference.chart_name().to_owned()
    } else {
        let lower = chart.to_ascii_lowercase();
        let chart = if lower.starts_with("http://") || lower.starts_with("https://") {
            chart.split(['?', '#']).next().unwrap_or(chart)
        } else {
            chart
        };
        let last = chart
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default();
        let lower = last.to_ascii_lowercase();
        let stem = [".tgz", ".tar.gz"]
            .iter()
            .find(|ext| lower.ends_with(*ext))
            .map(|ext| &last[..last.len() - ext.len()]);
        match stem {
            Some(stem) => without_version(stem).to_owned(),
            None => last.to_owned(),
        }
    };
    let name: String = name
        .chars()
        .map(|c| match c {
            '_' => '-',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    let name = name.trim_matches(['-', '.']);
    let name = name
        .get(..super::cli::MAX_RELEASE_NAME)
        .unwrap_or(name)
        .trim_end_matches(['-', '.']);
    super::repo::check_release_name(name)
        .is_ok()
        .then(|| name.to_owned())
}

/// `name-1.2.3` without `-1.2.3`: the first `-` followed by a version.
fn without_version(stem: &str) -> &str {
    stem.match_indices('-')
        .find(|(i, _)| {
            let version = &stem[i + 1..];
            version.starts_with(|c: char| c.is_ascii_digit() || c == 'v')
                && version.contains('.')
                && parse_version(version).is_some()
        })
        .map_or(stem, |(i, _)| &stem[..i])
}

/// Chart versions among the tags of an OCI repository, newest first: tags
/// that are versions (signatures, `latest` and the like are not), with `_`
/// back to `+`. Pre-releases only with `devel`, unless there is nothing
/// else (then they are listed rather than nothing).
pub fn chart_versions(tags: &[String], devel: bool) -> Vec<String> {
    let mut versions: Vec<String> = tags
        .iter()
        .map(|tag| tag.replace('_', "+"))
        .filter(|tag| parse_version(tag).is_some())
        .collect();
    versions.sort_by(|a, b| compare_versions(b, a));
    versions.dedup();
    if !devel && versions.iter().any(|v| !is_prerelease(v)) {
        versions.retain(|v| !is_prerelease(v));
    }
    versions
}

/// A version with a pre-release part (`1.0.0-rc.1`), skipped by helm
/// without `--devel`.
pub fn is_prerelease(version: &str) -> bool {
    parse_version(version).is_some_and(|v| !v.pre.is_empty())
}

/// The newest version that is not a pre-release, else the newest one.
pub fn newest_stable(versions: &[String]) -> Option<&str> {
    versions
        .iter()
        .find(|v| !is_prerelease(v))
        .or(versions.first())
        .map(String::as_str)
}

/// Auth scheme of a `WWW-Authenticate` challenge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Bearer,
    Basic,
}

/// A parsed `WWW-Authenticate` challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    pub scheme: Scheme,
    pub realm: Option<String>,
    pub service: Option<String>,
    pub scope: Option<String>,
}

/// Parses `Bearer realm="…",service="…",scope="…"` (or `Basic realm=…`);
/// `None` for other schemes.
pub fn parse_challenge(header: &str) -> Option<Challenge> {
    let header = header.trim();
    let (scheme, params) = header.split_once(' ').unwrap_or((header, ""));
    let scheme = if scheme.eq_ignore_ascii_case("bearer") {
        Scheme::Bearer
    } else if scheme.eq_ignore_ascii_case("basic") {
        Scheme::Basic
    } else {
        return None;
    };
    let mut challenge = Challenge {
        scheme,
        realm: None,
        service: None,
        scope: None,
    };
    let mut rest = params.trim();
    while !rest.is_empty() {
        let Some((key, after)) = rest.split_once('=') else {
            break;
        };
        let key = key.trim().trim_start_matches(',').trim();
        let after = after.trim_start();
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            // Quoted values may hold commas (`repository:a:pull,push`).
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, escaped)) = chars.next() {
                            value.push(escaped);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => value.push(c),
                }
            }
            (value, &quoted[end..])
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (after[..end].trim().to_owned(), &after[end..])
        };
        match key.to_ascii_lowercase().as_str() {
            "realm" => challenge.realm = Some(value),
            "service" => challenge.service = Some(value),
            "scope" => challenge.scope = Some(value),
            _ => {}
        }
        rest = next.trim_start().trim_start_matches(',').trim_start();
    }
    Some(challenge)
}

/// URL of the token request for a bearer `challenge`: the realm with its
/// service and the scope (a pull of `repository` when the challenge names
/// none). `None` without an `https://` realm.
pub fn token_url(challenge: &Challenge, repository: &str) -> Option<String> {
    let realm = challenge.realm.as_deref()?;
    if !realm
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
    {
        return None;
    }
    let scope = challenge
        .scope
        .clone()
        .unwrap_or_else(|| format!("repository:{repository}:pull"));
    let mut url = realm.to_owned();
    let mut separator = if realm.contains('?') { '&' } else { '?' };
    if let Some(service) = &challenge.service {
        url.push(separator);
        url.push_str("service=");
        url.push_str(&percent_encode(service));
        separator = '&';
    }
    url.push(separator);
    url.push_str("scope=");
    url.push_str(&percent_encode(&scope));
    Some(url)
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The `rel="next"` target of a `Link` header as an absolute URL on
/// `registry`; `None` without one, or when it points to another host (the
/// token must not leave the registry).
pub fn next_link(header: &str, registry: &str) -> Option<String> {
    header.split(',').find_map(|link| {
        let (target, params) = link.split_once(';')?;
        let target = target.trim().strip_prefix('<')?.strip_suffix('>')?;
        let next = params.split(';').any(|param| {
            param
                .trim()
                .split_once('=')
                .is_some_and(|(k, v)| k.trim() == "rel" && v.trim().trim_matches('"') == "next")
        });
        if !next {
            return None;
        }
        if target.starts_with('/') {
            return Some(format!("https://{registry}{target}"));
        }
        let rest = target.strip_prefix("https://")?;
        let host = rest.split('/').next()?;
        host.eq_ignore_ascii_case(registry)
            .then(|| target.to_owned())
    })
}

#[derive(Deserialize)]
struct TagList {
    #[serde(default)]
    tags: Option<Vec<String>>,
}

/// Tags of a `tags/list` answer (`"tags": null` is an empty list).
pub fn parse_tags(body: &[u8]) -> Result<Vec<String>, serde_json::Error> {
    Ok(serde_json::from_slice::<TagList>(body)?
        .tags
        .unwrap_or_default())
}

#[derive(Deserialize)]
struct TokenReply {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

/// The token of a token service answer (`token`, or OAuth's
/// `access_token`).
fn parse_token(body: &[u8]) -> Option<SecretString> {
    let reply: TokenReply = serde_json::from_slice(body).ok()?;
    reply
        .token
        .or(reply.access_token)
        .filter(|t| !t.is_empty())
        .map(SecretString::from)
}

/// A registry login. Debug output hides the password.
pub struct Credentials {
    pub username: String,
    password: SecretString,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

impl Credentials {
    pub fn new(username: String, password: SecretString) -> Self {
        Self { username, password }
    }

    fn basic(&self) -> SecretString {
        let pair = format!("{}:{}", self.username, self.password.expose_secret());
        SecretString::from(format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(pair)
        ))
    }
}

#[derive(Deserialize)]
struct AuthConfig {
    #[serde(default)]
    auths: std::collections::BTreeMap<String, AuthEntry>,
}

#[derive(Deserialize)]
struct AuthEntry {
    #[serde(default)]
    auth: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

/// Keys a login to `registry` may be stored under: the host, with a
/// scheme, and Docker Hub's aliases.
fn auth_keys(registry: &str) -> Vec<String> {
    let mut hosts = vec![registry.to_ascii_lowercase()];
    if matches!(
        hosts[0].as_str(),
        "registry-1.docker.io" | "docker.io" | "index.docker.io"
    ) {
        hosts = ["registry-1.docker.io", "index.docker.io", "docker.io"]
            .map(str::to_owned)
            .to_vec();
    }
    hosts
}

/// The login to `registry` in a Docker-style `config.json` (`auths`, as
/// written by `helm registry login` and `docker login`).
pub fn credentials_from_config(text: &str, registry: &str) -> Option<Credentials> {
    let config: AuthConfig = serde_json::from_str(text).ok()?;
    let keys = auth_keys(registry);
    let entry = config.auths.iter().find_map(|(key, entry)| {
        let host = key
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        keys.contains(&host).then_some(entry)
    })?;
    if let Some(auth) = entry.auth.as_deref().filter(|a| !a.is_empty()) {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(auth.trim())
            .ok()?;
        let decoded = String::from_utf8(decoded).ok()?;
        let (user, password) = decoded.split_once(':')?;
        return Some(Credentials::new(
            user.to_owned(),
            SecretString::from(password.to_owned()),
        ));
    }
    match (&entry.username, &entry.password) {
        (Some(user), Some(password)) if !user.is_empty() => Some(Credentials::new(
            user.clone(),
            SecretString::from(password.clone()),
        )),
        _ => None,
    }
}

/// Where `helm registry login` and `docker login` keep logins, in that
/// order: `HELM_REGISTRY_CONFIG` or `<helm config>/registry/config.json`,
/// then `DOCKER_CONFIG/config.json` or `~/.docker/config.json`.
pub fn credential_files(
    platform: Platform,
    home: Option<&Path>,
    var: impl Fn(&str) -> Option<OsString>,
) -> Vec<PathBuf> {
    let var = |name: &str| var(name).filter(|value| !value.is_empty());
    let mut files = Vec::new();
    match var("HELM_REGISTRY_CONFIG") {
        Some(path) => files.push(PathBuf::from(path)),
        None => {
            // Helm's config home: where `repositories.yaml` lives by default.
            let paths = HelmPaths::resolve(platform, home, |name| {
                (name != "HELM_REPOSITORY_CONFIG")
                    .then(|| var(name))
                    .flatten()
            });
            if let Some(dir) = paths.as_ref().and_then(|p| p.repository_config.parent()) {
                files.push(dir.join("registry").join("config.json"));
            }
        }
    }
    match var("DOCKER_CONFIG") {
        Some(dir) => files.push(PathBuf::from(dir).join("config.json")),
        None => {
            if let Some(home) = home {
                files.push(home.join(".docker").join("config.json"));
            }
        }
    }
    files
}

/// The user's login to `registry`, if any.
pub fn find_credentials(registry: &str) -> Option<Credentials> {
    credential_files(
        Platform::current(),
        crate::env::home_dir().as_deref(),
        crate::env::var_os,
    )
    .iter()
    .find_map(|file| {
        let text = std::fs::read_to_string(file).ok()?;
        credentials_from_config(&text, registry)
    })
}

#[derive(Debug, thiserror::Error)]
pub enum OciError {
    #[error("not an oci:// chart reference")]
    NotOci,
    #[error("can't reach {host}: {message}")]
    Network { host: String, message: String },
    #[error(
        "{registry} refused to list the versions of {repository}: \
         check the reference, or log in with helm registry login if the chart is private"
    )]
    Denied {
        registry: String,
        repository: String,
    },
    #[error("{registry} has no chart {repository}")]
    NotFound {
        registry: String,
        repository: String,
    },
    #[error("{host} answered HTTP {status}")]
    Http { host: String, status: u16 },
    #[error("{host} sent an invalid answer: {message}")]
    Invalid { host: String, message: String },
}

/// Chart versions of the `oci://` reference `chart`, newest first
/// ([`chart_versions`]).
pub async fn list_versions(chart: &str, devel: bool) -> Result<Vec<String>, OciError> {
    let reference = parse_reference(chart).ok_or(OciError::NotOci)?;
    let tags = list_tags(&reference).await?;
    Ok(chart_versions(&tags, devel))
}

/// All tags of the repository: anonymously, then with the user's login when
/// the registry refuses.
pub async fn list_tags(reference: &OciReference) -> Result<Vec<String>, OciError> {
    let anonymous = fetch_tags(reference, None).await;
    if matches!(anonymous, Err(OciError::Denied { .. }))
        && let Some(login) = find_credentials(&reference.registry)
    {
        return fetch_tags(reference, Some(&login)).await;
    }
    anonymous
}

async fn fetch_tags(
    reference: &OciReference,
    login: Option<&Credentials>,
) -> Result<Vec<String>, OciError> {
    let registry = &reference.registry;
    let denied = || OciError::Denied {
        registry: registry.clone(),
        repository: reference.repository.clone(),
    };
    let mut url = format!(
        "https://{registry}/v2/{}/tags/list?n=1000",
        reference.repository
    );
    let mut auth: Option<SecretString> = None;
    let mut tags = Vec::new();
    for _ in 0..MAX_PAGES {
        let mut reply = get(&url, auth.as_ref()).await?;
        if reply.status == http::StatusCode::UNAUTHORIZED && auth.is_none() {
            let challenge = reply
                .headers
                .get(http::header::WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_challenge)
                .ok_or_else(denied)?;
            auth = Some(match challenge.scheme {
                Scheme::Bearer => fetch_token(&challenge, reference, login).await?,
                Scheme::Basic => login.ok_or_else(denied)?.basic(),
            });
            reply = get(&url, auth.as_ref()).await?;
        }
        match reply.status.as_u16() {
            200..=299 => {}
            401 | 403 => {
                return Err(denied());
            }
            404 => {
                return Err(OciError::NotFound {
                    registry: registry.clone(),
                    repository: reference.repository.clone(),
                });
            }
            status => {
                return Err(OciError::Http {
                    host: registry.clone(),
                    status,
                });
            }
        }
        tags.extend(parse_tags(&reply.body).map_err(|e| OciError::Invalid {
            host: registry.clone(),
            message: e.to_string(),
        })?);
        let next = reply
            .headers
            .get(http::header::LINK)
            .and_then(|v| v.to_str().ok())
            .and_then(|link| next_link(link, registry));
        match next {
            Some(next) if next != url => url = next,
            _ => break,
        }
    }
    Ok(tags)
}

/// A pull token from the token service of `challenge`, with the login as
/// Basic auth when given.
async fn fetch_token(
    challenge: &Challenge,
    reference: &OciReference,
    login: Option<&Credentials>,
) -> Result<SecretString, OciError> {
    let url = token_url(challenge, &reference.repository).ok_or_else(|| OciError::Invalid {
        host: reference.registry.clone(),
        message: "the auth challenge names no https:// token service".into(),
    })?;
    let basic = login.map(Credentials::basic);
    let reply = get(&url, basic.as_ref()).await?;
    let host = host_of(&url);
    match reply.status.as_u16() {
        200..=299 => parse_token(&reply.body)
            .map(|token| SecretString::from(format!("Bearer {}", token.expose_secret())))
            .ok_or_else(|| OciError::Invalid {
                host,
                message: "no token in the answer".into(),
            }),
        401 | 403 => Err(OciError::Denied {
            registry: reference.registry.clone(),
            repository: reference.repository.clone(),
        }),
        status => Err(OciError::Http { host, status }),
    }
}

fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(url)
        .to_owned()
}

struct Reply {
    status: http::StatusCode,
    headers: http::HeaderMap,
    body: Vec<u8>,
}

/// GETs `url` (https) with an optional `Authorization`, through the proxy
/// from the environment. kube's client is hyper + rustls with the system
/// roots, already in the tree; without kubeconfig auth it sends nothing
/// else.
async fn get(url: &str, authorization: Option<&SecretString>) -> Result<Reply, OciError> {
    let host = host_of(url);
    let invalid = || OciError::Invalid {
        host: host.clone(),
        message: format!("bad URL {url}"),
    };
    let uri: http::Uri = url.parse().map_err(|_| invalid())?;
    let (Some(scheme), Some(authority)) = (uri.scheme(), uri.authority()) else {
        return Err(invalid());
    };
    let network = |message: String| OciError::Network {
        host: host.clone(),
        message,
    };
    let base: http::Uri = format!("{scheme}://{authority}")
        .parse()
        .map_err(|_| invalid())?;
    let mut config = kube::Config::new(base);
    config.connect_timeout = Some(CONNECT_TIMEOUT);
    config.read_timeout = Some(READ_TIMEOUT);
    config.proxy_url = crate::tools::proxy::from_env(&uri);
    let client = kube::Client::try_from(config).map_err(|e| network(e.to_string()))?;
    let path = uri.path_and_query().map_or("/", |p| p.as_str());
    let mut request = http::Request::get(path)
        .header(http::header::ACCEPT, "application/json")
        .header(
            http::header::USER_AGENT,
            format!("Kubach/{}", crate::VERSION),
        );
    if let Some(value) = authorization {
        let mut value =
            http::HeaderValue::from_str(value.expose_secret()).map_err(|_| OciError::Invalid {
                host: host.clone(),
                message: "the token is not a valid header".into(),
            })?;
        value.set_sensitive(true);
        request = request.header(http::header::AUTHORIZATION, value);
    }
    let request = request
        .body(kube::client::Body::empty())
        .map_err(|_| invalid())?;
    let response = client
        .send(request)
        .await
        .map_err(|e| network(e.to_string()))?;
    let status = response.status();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    let mut stream = response.into_body().into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| network(e.to_string()))?;
        if (body.len() + chunk.len()) as u64 > BODY_LIMIT {
            return Err(OciError::Invalid {
                host,
                message: "the answer is too large".into(),
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Reply {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| (*t).to_owned()).collect()
    }

    #[test]
    fn parses_oci_references() {
        let r = parse_reference("oci://registry-1.docker.io/bitnamicharts/postgresql").unwrap();
        assert_eq!(r.registry, "registry-1.docker.io");
        assert_eq!(r.repository, "bitnamicharts/postgresql");
        assert_eq!(r.tag, None);
        assert_eq!(r.chart_name(), "postgresql");
        let r = parse_reference(" OCI://localhost:5000/charts/app:1.2.3 ").unwrap();
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.repository, "charts/app");
        assert_eq!(r.tag.as_deref(), Some("1.2.3"));
        let r = parse_reference("oci://ghcr.io/org/app@sha256:abc").unwrap();
        assert_eq!(r.repository, "org/app");
        assert_eq!(r.tag.as_deref(), Some("sha256:abc"));
        assert_eq!(
            parse_reference("oci://ghcr.io/app/").unwrap().repository,
            "app"
        );
        for bad in [
            "oci://",
            "oci://ghcr.io",
            "oci://ghcr.io/",
            "oci://ghcr.io/a b",
            "oci://ghcr.io//app",
            "https://ghcr.io/app",
            "bitnami/nginx",
        ] {
            assert_eq!(parse_reference(bad), None, "{bad}");
        }
    }

    #[test]
    fn release_names_from_charts() {
        let cases = [
            (
                "oci://registry-1.docker.io/bitnamicharts/postgresql",
                "postgresql",
            ),
            ("oci://ghcr.io/org/My_Chart:1.0.0", "my-chart"),
            ("bitnami/nginx", "nginx"),
            ("./charts/app/", "app"),
            ("/tmp/nginx-15.4.2.tgz", "nginx"),
            ("C:\\charts\\cert-manager-v1.14.0.tgz", "cert-manager"),
            (
                "https://example.com/charts/ingress-nginx-4.10.0-rc.1.tgz?x=1",
                "ingress-nginx",
            ),
            (
                "kube-prometheus-stack-58.0.0.tar.gz",
                "kube-prometheus-stack",
            ),
            ("app-v2.tgz", "app-v2"),
        ];
        for (chart, name) in cases {
            assert_eq!(release_name(chart).as_deref(), Some(name), "{chart}");
        }
        assert_eq!(release_name(""), None);
        assert_eq!(release_name("oci://ghcr.io/org/***"), None);
    }

    #[test]
    fn versions_from_tags_newest_first() {
        let list = tags(&[
            "12.1.2",
            "12.10.0",
            "latest",
            "13.0.0-rc.1",
            "12.1.10",
            "1.0.0_build.5",
            "sha256-ab12.sig",
            "12.10.0",
        ]);
        assert_eq!(
            chart_versions(&list, false),
            ["12.10.0", "12.1.10", "12.1.2", "1.0.0+build.5"]
        );
        let devel = chart_versions(&list, true);
        assert_eq!(devel[0], "13.0.0-rc.1");
        assert_eq!(newest_stable(&devel), Some("12.10.0"));
        // Only pre-releases: listed rather than nothing.
        let pre = tags(&["2.0.0-beta.1", "2.0.0-alpha.1"]);
        assert_eq!(
            chart_versions(&pre, false),
            ["2.0.0-beta.1", "2.0.0-alpha.1"]
        );
        assert_eq!(
            newest_stable(&chart_versions(&pre, false)),
            Some("2.0.0-beta.1")
        );
        assert_eq!(newest_stable(&[]), None);
    }

    #[test]
    fn parses_auth_challenges() {
        let c = parse_challenge(
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:bitnamicharts/postgresql:pull""#,
        )
        .unwrap();
        assert_eq!(c.scheme, Scheme::Bearer);
        assert_eq!(c.realm.as_deref(), Some("https://auth.docker.io/token"));
        assert_eq!(c.service.as_deref(), Some("registry.docker.io"));
        assert_eq!(
            token_url(&c, "ignored").unwrap(),
            "https://auth.docker.io/token?service=registry.docker.io\
             &scope=repository%3Abitnamicharts%2Fpostgresql%3Apull"
        );
        // Commas inside quotes, no service, scope from the repository.
        let c = parse_challenge(
            r#"bearer realm="https://ghcr.io/token", scope="repository:a:pull,push""#,
        )
        .unwrap();
        assert_eq!(c.scope.as_deref(), Some("repository:a:pull,push"));
        let c = parse_challenge(r#"Bearer realm="https://r.example/t?x=1""#).unwrap();
        assert_eq!(
            token_url(&c, "org/app").unwrap(),
            "https://r.example/t?x=1&scope=repository%3Aorg%2Fapp%3Apull"
        );
        let c = parse_challenge("Basic realm=Registry").unwrap();
        assert_eq!(c.scheme, Scheme::Basic);
        assert_eq!(c.realm.as_deref(), Some("Registry"));
        assert_eq!(parse_challenge("Negotiate"), None);
        // No plain-http token service.
        let c = parse_challenge(r#"Bearer realm="http://r.example/t""#).unwrap();
        assert_eq!(token_url(&c, "a"), None);
    }

    #[test]
    fn follows_next_links_on_the_registry_only() {
        let reg = "registry-1.docker.io";
        assert_eq!(
            next_link(
                r#"</v2/bitnamicharts/postgresql/tags/list?last=12.4.2&n=50>; rel="next""#,
                reg
            )
            .as_deref(),
            Some(
                "https://registry-1.docker.io/v2/bitnamicharts/postgresql/tags/list?last=12.4.2&n=50"
            )
        );
        assert_eq!(
            next_link(
                "<https://registry-1.docker.io/v2/a/tags/list?last=x>; rel=next",
                reg
            )
            .as_deref(),
            Some("https://registry-1.docker.io/v2/a/tags/list?last=x")
        );
        assert_eq!(
            next_link(r#"<https://evil.example/v2/a>; rel="next""#, reg),
            None
        );
        assert_eq!(next_link(r#"</v2/a?last=1>; rel="prev""#, reg), None);
    }

    #[test]
    fn parses_tag_lists_and_tokens() {
        assert_eq!(
            parse_tags(br#"{"name":"a","tags":["1.0.0","2.0.0"]}"#).unwrap(),
            ["1.0.0", "2.0.0"]
        );
        assert!(
            parse_tags(br#"{"name":"a","tags":null}"#)
                .unwrap()
                .is_empty()
        );
        assert!(parse_tags(b"<html>").is_err());
        assert_eq!(
            parse_token(br#"{"token":"t1","access_token":"t2"}"#)
                .unwrap()
                .expose_secret(),
            "t1"
        );
        assert_eq!(
            parse_token(br#"{"access_token":"t2"}"#)
                .unwrap()
                .expose_secret(),
            "t2"
        );
        assert!(parse_token(br#"{"token":""}"#).is_none());
    }

    #[test]
    fn reads_logins_from_config_files() {
        let auth = base64::engine::general_purpose::STANDARD.encode("me:p:ss");
        let text = format!(
            r#"{{"auths":{{"https://index.docker.io/v1/":{{"auth":"{auth}"}},
                "ghcr.io":{{"username":"u","password":"pw"}}}},"credsStore":"desktop"}}"#
        );
        let hub = credentials_from_config(&text, "registry-1.docker.io").unwrap();
        assert_eq!(hub.username, "me");
        assert_eq!(hub.password.expose_secret(), "p:ss");
        assert!(!format!("{hub:?}").contains("p:ss"));
        let ghcr = credentials_from_config(&text, "GHCR.io").unwrap();
        assert_eq!(ghcr.username, "u");
        assert!(credentials_from_config(&text, "quay.io").is_none());
        assert!(credentials_from_config("not json", "ghcr.io").is_none());
    }

    #[test]
    fn credential_files_follow_helm_and_docker() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        let home = Some(Path::new("/home/me"));
        assert_eq!(
            credential_files(Platform::MacOs, home, env(&[])),
            [
                PathBuf::from("/home/me/Library/Preferences/helm/registry/config.json"),
                PathBuf::from("/home/me/.docker/config.json"),
            ]
        );
        assert_eq!(
            credential_files(
                Platform::Unix,
                home,
                env(&[
                    ("HELM_REPOSITORY_CONFIG", "/r/repos.yaml"),
                    ("HELM_CONFIG_HOME", "/helm"),
                    ("DOCKER_CONFIG", "/d"),
                ])
            ),
            [
                PathBuf::from("/helm/registry/config.json"),
                PathBuf::from("/d/config.json"),
            ]
        );
        assert_eq!(
            credential_files(
                Platform::Unix,
                None,
                env(&[("HELM_REGISTRY_CONFIG", "/reg.json")])
            ),
            [PathBuf::from("/reg.json")]
        );
    }
}
