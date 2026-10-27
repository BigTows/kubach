//! Chart repositories of the user's Helm, read natively (ADR 0006).
//!
//! `helm repo add` writes the repository list to `repositories.yaml` and
//! `helm repo update` caches each repository's `index.yaml` as
//! `<cache>/<name>-index.yaml`. Listing repositories and searching charts
//! read those files directly, the way `helm repo list` and `helm search repo`
//! do, so they work without the binary and without the network. Adding,
//! updating and removing repositories stay with `helm` ([`super::cli`]).
//!
//! Paths follow Helm's own rules: `HELM_REPOSITORY_CONFIG` and
//! `HELM_REPOSITORY_CACHE`, else `HELM_CONFIG_HOME` / `HELM_CACHE_HOME`, else
//! `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` plus `helm`, else the platform
//! default (`~/Library/Preferences` and `~/Library/Caches` on macOS,
//! `%APPDATA%` and `%TEMP%` on Windows, `~/.config` and `~/.cache` elsewhere).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Deserializer};

use crate::names;
use crate::validation::{Field, ValidationError};

/// Where the user's Helm keeps its repository list and index cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelmPaths {
    /// `repositories.yaml`.
    pub repository_config: PathBuf,
    /// Directory with `<name>-index.yaml` files.
    pub repository_cache: PathBuf,
}

/// Platform whose Helm defaults apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    /// Linux and other Unixes (XDG defaults).
    Unix,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

impl HelmPaths {
    /// Paths for this process's environment, as `helm env` would print them.
    /// `None` when a needed base directory is unknown (no home directory).
    pub fn from_env() -> Option<Self> {
        Self::resolve(
            Platform::current(),
            crate::env::home_dir().as_deref(),
            crate::env::var_os,
        )
    }

    /// Helm's path rules over `var` (an environment lookup) and `home`.
    pub fn resolve(
        platform: Platform,
        home: Option<&Path>,
        var: impl Fn(&str) -> Option<OsString>,
    ) -> Option<Self> {
        // Empty variables count as unset, as in Helm.
        let var = |name: &str| {
            var(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        let base = |helm_var: &str, xdg_var: &str, default: Option<PathBuf>| {
            var(helm_var).or_else(|| Some(var(xdg_var).or(default)?.join("helm")))
        };
        let (config_default, cache_default) = match platform {
            Platform::MacOs => (
                home.map(|home| home.join("Library").join("Preferences")),
                home.map(|home| home.join("Library").join("Caches")),
            ),
            Platform::Windows => (var("APPDATA"), var("TEMP")),
            Platform::Unix => (
                home.map(|home| home.join(".config")),
                home.map(|home| home.join(".cache")),
            ),
        };
        let repository_config = match var("HELM_REPOSITORY_CONFIG") {
            Some(path) => path,
            None => base("HELM_CONFIG_HOME", "XDG_CONFIG_HOME", config_default)?
                .join("repositories.yaml"),
        };
        let repository_cache = match var("HELM_REPOSITORY_CACHE") {
            Some(path) => path,
            None => base("HELM_CACHE_HOME", "XDG_CACHE_HOME", cache_default)?.join("repository"),
        };
        Some(Self {
            repository_config,
            repository_cache,
        })
    }

    /// Cached index of the repository `name`.
    pub fn index_file(&self, name: &str) -> PathBuf {
        self.repository_cache.join(format!("{name}-index.yaml"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("can't read {}: {source}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is not valid YAML: {message}", .path.display())]
    Parse { path: PathBuf, message: String },
}

/// A repository from `repositories.yaml`. Credentials and TLS files stay in
/// the file: Kubach neither reads nor shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub name: String,
    pub url: String,
}

#[derive(Deserialize)]
struct RepositoriesFile {
    #[serde(default, deserialize_with = "null_default")]
    repositories: Vec<RepositoryEntry>,
}

#[derive(Deserialize)]
struct RepositoryEntry {
    #[serde(default, deserialize_with = "lenient_string")]
    name: String,
    #[serde(default, deserialize_with = "lenient_string")]
    url: String,
}

/// Parses `repositories.yaml`; entries without a name are skipped.
pub fn parse_repositories(text: &str) -> Result<Vec<Repository>, ValidationError> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let file: RepositoriesFile =
        serde_saphyr::from_str_with_options(text, options()).map_err(yaml_error)?;
    Ok(file
        .repositories
        .into_iter()
        .filter(|entry| !entry.name.is_empty())
        .map(|entry| Repository {
            name: entry.name,
            url: entry.url,
        })
        .collect())
}

/// Reads the repository list; a missing file means no repositories (Helm
/// creates it on the first `helm repo add`).
pub fn read_repositories(path: &Path) -> Result<Vec<Repository>, RepoError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(RepoError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    parse_repositories(&text).map_err(|error| RepoError::Parse {
        path: path.to_owned(),
        message: error.to_string(),
    })
}

/// One version of a chart in a repository index.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartVersion {
    #[serde(default, deserialize_with = "lenient_string")]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub version: String,
    #[serde(default, deserialize_with = "lenient_option")]
    pub app_version: Option<String>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub home: Option<String>,
    /// URL of the chart's icon (SVG or PNG), from `Chart.yaml`.
    #[serde(default, deserialize_with = "lenient_option")]
    pub icon: Option<String>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub created: Option<String>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub kube_version: Option<String>,
    #[serde(default, deserialize_with = "lenient_bool")]
    pub deprecated: bool,
    #[serde(default, deserialize_with = "lenient_list")]
    pub keywords: Vec<String>,
    #[serde(default, deserialize_with = "lenient_list")]
    pub sources: Vec<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub maintainers: Vec<Maintainer>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Maintainer {
    #[serde(default, deserialize_with = "lenient_string")]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_option")]
    pub email: Option<String>,
}

impl ChartVersion {
    /// A pre-release (`1.0.0-rc.1`); `helm search repo` hides these
    /// without `--devel`.
    pub fn is_prerelease(&self) -> bool {
        parse_version(&self.version).is_some_and(|version| !version.pre.is_empty())
    }
}

/// A parsed `index.yaml`: chart name → versions, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChartIndex {
    pub charts: BTreeMap<String, Vec<ChartVersion>>,
}

#[derive(Deserialize)]
struct IndexFile {
    #[serde(default, deserialize_with = "null_default")]
    entries: BTreeMap<String, Option<Vec<ChartVersion>>>,
}

/// Parses an `index.yaml` (or the JSON form newer Helm may write). Versions
/// are sorted newest first by SemVer; entries without versions are dropped.
pub fn parse_index(text: &str) -> Result<ChartIndex, ValidationError> {
    let file: IndexFile =
        serde_saphyr::from_str_with_options(text, index_options()).map_err(yaml_error)?;
    let charts = file
        .entries
        .into_iter()
        .filter_map(|(name, versions)| {
            let mut versions: Vec<ChartVersion> = versions?
                .into_iter()
                .filter(|version| !version.version.is_empty())
                .map(|mut version| {
                    if version.name.is_empty() {
                        version.name = name.clone();
                    }
                    version
                })
                .collect();
            versions.sort_by(|a, b| compare_versions(&b.version, &a.version));
            (!versions.is_empty()).then_some((name, versions))
        })
        .collect();
    Ok(ChartIndex { charts })
}

/// State of a repository's cached index.
#[derive(Clone, Debug)]
pub enum IndexState {
    Ready(Arc<ChartIndex>),
    /// Not cached yet: `helm repo update` downloads it.
    Missing,
    Invalid(String),
}

/// A repository with its cached index.
#[derive(Clone, Debug)]
pub struct LoadedRepository {
    pub repository: Repository,
    pub index: IndexState,
    /// Size and modification time of the index file it was parsed from;
    /// an unchanged file is not parsed again.
    stamp: Option<(u64, SystemTime)>,
}

impl LoadedRepository {
    /// A repository with an index already in memory (not read from disk).
    pub fn ready(repository: Repository, index: ChartIndex) -> Self {
        Self {
            repository,
            index: IndexState::Ready(Arc::new(index)),
            stamp: None,
        }
    }

    pub fn charts(&self) -> Option<&ChartIndex> {
        match &self.index {
            IndexState::Ready(index) => Some(index),
            _ => None,
        }
    }
}

/// Reads the repository list and every cached index. Indexes whose file did
/// not change since `previous` are reused instead of parsed again (a large
/// repository's index is tens of megabytes). Blocking: run off the UI thread.
pub fn load(
    paths: &HelmPaths,
    previous: &[LoadedRepository],
) -> Result<Vec<LoadedRepository>, RepoError> {
    let repositories = read_repositories(&paths.repository_config)?;
    Ok(repositories
        .into_iter()
        .map(|repository| {
            let path = paths.index_file(&repository.name);
            let stamp = std::fs::metadata(&path)
                .ok()
                .and_then(|meta| Some((meta.len(), meta.modified().ok()?)));
            let reused = previous.iter().find(|old| {
                old.repository == repository
                    && stamp.is_some()
                    && old.stamp == stamp
                    && matches!(old.index, IndexState::Ready(_))
            });
            if let Some(old) = reused {
                return old.clone();
            }
            let index = match std::fs::read_to_string(&path) {
                Ok(text) => match parse_index(&text) {
                    Ok(index) => IndexState::Ready(Arc::new(index)),
                    Err(error) => IndexState::Invalid(error.to_string()),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => IndexState::Missing,
                Err(error) => IndexState::Invalid(error.to_string()),
            };
            LoadedRepository {
                repository,
                index,
                stamp,
            }
        })
        .collect())
}

/// One row of a chart search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChartHit<'a> {
    pub repository: &'a str,
    pub version: &'a ChartVersion,
}

impl ChartHit<'_> {
    /// `repo/chart`, as `helm install` takes it.
    pub fn reference(&self) -> String {
        chart_reference(self.repository, &self.version.name)
    }
}

/// `repo/chart`.
pub fn chart_reference(repository: &str, chart: &str) -> String {
    format!("{repository}/{chart}")
}

/// Search options, as the flags of `helm search repo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions<'a> {
    /// Every version (`--versions`), not only the latest.
    pub all_versions: bool,
    /// Include pre-releases (`--devel`).
    pub devel: bool,
    /// Only charts of the repository with this name.
    pub repository: Option<&'a str>,
}

/// Charts of `repositories` matching `query` like `helm search repo`: a
/// case-insensitive substring of `repo/chart`, the description or a keyword
/// (an empty query matches every chart). Charts whose name matches come
/// first, then by name; with `all_versions` each chart lists its versions
/// newest first. A chart that only has pre-releases shows its newest one
/// even without `devel`, rather than disappearing. With `repository` only
/// that repository is searched.
pub fn search<'a>(
    repositories: &'a [LoadedRepository],
    query: &str,
    options: SearchOptions<'_>,
) -> Vec<ChartHit<'a>> {
    let query = query.trim().to_lowercase();
    let mut hits: Vec<(bool, String, Vec<&'a ChartVersion>, &'a str)> = Vec::new();
    for loaded in repositories {
        let Some(index) = loaded.charts() else {
            continue;
        };
        let repository = loaded.repository.name.as_str();
        if options
            .repository
            .is_some_and(|wanted| wanted != repository)
        {
            continue;
        }
        for (name, versions) in &index.charts {
            let reference = chart_reference(repository, name).to_lowercase();
            let shown: Vec<&ChartVersion> = versions
                .iter()
                .filter(|version| options.devel || !version.is_prerelease())
                .collect();
            let shown = if shown.is_empty() {
                versions.iter().take(1).collect()
            } else {
                shown
            };
            let Some(latest) = shown.first() else {
                continue;
            };
            let name_match = reference.contains(&query);
            let matches = query.is_empty()
                || name_match
                || latest
                    .description
                    .as_deref()
                    .is_some_and(|text| text.to_lowercase().contains(&query))
                || latest
                    .keywords
                    .iter()
                    .any(|keyword| keyword.to_lowercase().contains(&query));
            if !matches {
                continue;
            }
            let versions = if options.all_versions {
                shown
            } else {
                vec![*latest]
            };
            hits.push((!name_match, reference, versions, repository));
        }
    }
    hits.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    hits.into_iter()
        .flat_map(|(_, _, versions, repository)| {
            versions.into_iter().map(move |version| ChartHit {
                repository,
                version,
            })
        })
        .collect()
}

/// Versions of `chart` in `repository`, newest first.
pub fn find_chart<'a>(
    repositories: &'a [LoadedRepository],
    repository: &str,
    chart: &str,
) -> Option<&'a [ChartVersion]> {
    repositories
        .iter()
        .find(|loaded| loaded.repository.name == repository)?
        .charts()?
        .charts
        .get(chart)
        .map(Vec::as_slice)
}

/// A chart version as SemVer, leniently like Helm: a leading `v` and
/// missing minor or patch numbers are accepted (`v1.2` is `1.2.0`).
pub fn parse_version(version: &str) -> Option<semver::Version> {
    let version = version.trim();
    let version = version.strip_prefix(['v', 'V']).unwrap_or(version);
    if let Ok(parsed) = semver::Version::parse(version) {
        return Some(parsed);
    }
    let split = version.find(['-', '+']).unwrap_or(version.len());
    let (core, rest) = version.split_at(split);
    let padded = match core.matches('.').count() {
        0 => format!("{core}.0.0{rest}"),
        1 => format!("{core}.0{rest}"),
        _ => return None,
    };
    semver::Version::parse(&padded).ok()
}

/// Orders chart versions by SemVer; versions that are not SemVer sort below
/// those that are, and among themselves as text.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => a.cmp(b),
    }
}

/// Whether `name` can be a repository name for `helm repo add`.
pub fn check_repository_name(name: &str) -> Result<(), ValidationError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ValidationError::Empty(Field::RepositoryName));
    }
    if name.contains('/') || name.chars().any(char::is_whitespace) {
        return Err(ValidationError::InvalidChars {
            field: Field::RepositoryName,
            value: name.to_owned(),
        });
    }
    Ok(())
}

/// Host labels and path segments that say nothing about whose charts these
/// are, so a name is not taken from them.
const GENERIC_NAME_PARTS: &[&str] = &[
    "charts",
    "chart",
    "helm",
    "helm-charts",
    "helmcharts",
    "releases",
    "repo",
    "repos",
    "repository",
    "stable",
    "www",
    "github",
    "gitlab",
    "index.yaml",
];

/// A repository name for `url`, the way people usually name it: the last
/// path segment (`https://example.com/charts/argo` → `argo`), or the host
/// when the path is generic (`https://grafana.github.io/helm-charts` →
/// `grafana`, `https://charts.jetstack.io` → `jetstack`).
pub fn suggest_repository_name(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .split_once("://")
        .map_or(url.trim(), |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    let generic = |part: &str| GENERIC_NAME_PARTS.contains(&part.to_ascii_lowercase().as_str());
    let from_path = path.split('/').rev().find(|s| !s.is_empty() && !generic(s));
    let from_host = || {
        let labels: Vec<&str> = host.split('.').collect();
        let without_tld = &labels[..labels.len().saturating_sub(1).max(1)];
        without_tld.iter().rev().copied().find(|l| !generic(l))
    };
    let name: String = from_path
        .or_else(from_host)?
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches('-');
    (!name.is_empty() && check_repository_name(name).is_ok()).then(|| name.to_owned())
}

/// Whether `url` can be added with `helm repo add` (OCI registries are not
/// repositories: their charts install by `oci://` reference).
pub fn check_repository_url(url: &str) -> Result<(), ValidationError> {
    let url = url.trim();
    if url.starts_with("oci://") {
        return Err(ValidationError::OciRepository);
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or(ValidationError::UrlScheme)?;
    if rest.is_empty() || rest.chars().any(char::is_whitespace) {
        return Err(ValidationError::InvalidUrl);
    }
    Ok(())
}

/// Whether a username and password may be sent to `url`: only over
/// `https://`, a plain `http://` repository would get them in clear text
/// (#202).
pub fn check_repository_credentials(url: &str) -> Result<(), ValidationError> {
    let url = url.trim();
    let https = url
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"));
    if https {
        Ok(())
    } else {
        Err(ValidationError::InsecureCredentials)
    }
}

/// Whether `name` is a valid release name (DNS-1123 subdomain, at most 53
/// characters, as Helm requires).
pub fn check_release_name(name: &str) -> Result<(), ValidationError> {
    names::validate_dns_subdomain_max(name, super::cli::MAX_RELEASE_NAME).map_err(|error| {
        ValidationError::Name {
            field: Field::ReleaseName,
            error,
        }
    })
}

/// Whether `name` is a valid namespace name (DNS-1123 label).
pub fn check_namespace_name(name: &str) -> Result<(), ValidationError> {
    names::validate_dns_label(name).map_err(|error| ValidationError::Name {
        field: Field::Namespace,
        error,
    })
}

/// A YAML file Helm wrote that does not parse; the parser's text carries
/// the position.
fn yaml_error(error: impl std::fmt::Display) -> ValidationError {
    ValidationError::Yaml {
        field: None,
        line: None,
        message: error.to_string(),
    }
}

/// YAML 1.2 core schema, like Helm's YAML library.
fn options() -> serde_saphyr::Options {
    let mut options = serde_saphyr::Options::default();
    options.strict_booleans = true;
    options.with_snippet = false;
    options
}

/// Large repositories (Bitnami, Grafana) have indexes of tens of megabytes:
/// lift the event and node budgets well above them.
fn index_options() -> serde_saphyr::Options {
    let mut options = options();
    let mut budget = serde_saphyr::Budget::default();
    budget.max_events = 200_000_000;
    budget.max_nodes = 100_000_000;
    budget.max_total_scalar_bytes = 1 << 30;
    budget.max_depth = 256;
    options.budget = Some(budget);
    options
}

/// A scalar of any type as its text: index files carry `appVersion: 1.10`
/// or `version: 2` unquoted, and `1.10` must not turn into `1.1`. Mappings
/// and sequences where a scalar belongs are ignored.
struct ScalarText;

impl<'de> serde::de::Visitor<'de> for ScalarText {
    type Value = Option<String>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a scalar")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Some(value.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(Some(value))
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Some(value.to_string()))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Some(value.to_string()))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Some(value.to_string()))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(Some(value.to_string()))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_some<D: Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_string(self)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        while map
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Ok(None)
    }
}

/// [`ScalarText`] as a value.
struct Scalar(Option<String>);

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        de.deserialize_string(ScalarText).map(Scalar)
    }
}

fn lenient_option<'de, D: Deserializer<'de>>(de: D) -> Result<Option<String>, D::Error> {
    Ok(Scalar::deserialize(de)?.0.filter(|text| !text.is_empty()))
}

fn lenient_string<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    Ok(lenient_option(de)?.unwrap_or_default())
}

fn lenient_bool<'de, D: Deserializer<'de>>(de: D) -> Result<bool, D::Error> {
    Ok(Scalar::deserialize(de)?.0.as_deref() == Some("true"))
}

fn lenient_list<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<String>, D::Error> {
    Ok(Option::<Vec<Scalar>>::deserialize(de)?
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| item.0)
        .filter(|text| !text.is_empty())
        .collect())
}

fn null_default<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(de)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_names_come_from_urls() {
        let cases = [
            (
                "https://prometheus-community.github.io/helm-charts",
                Some("prometheus-community"),
            ),
            ("https://grafana.github.io/helm-charts/", Some("grafana")),
            ("https://charts.jetstack.io", Some("jetstack")),
            ("https://helm.releases.hashicorp.com", Some("hashicorp")),
            (
                "https://kubernetes.github.io/ingress-nginx",
                Some("ingress-nginx"),
            ),
            (
                "https://example.com:8443/charts/stable?x=1",
                Some("example"),
            ),
            (
                "https://user@nexus.corp.local/repository/Team_Charts",
                Some("team-charts"),
            ),
            ("charts.example.org/argo", Some("argo")),
            ("", None),
            ("https://", None),
        ];
        for (url, name) in cases {
            assert_eq!(suggest_repository_name(url).as_deref(), name, "{url}");
        }
    }

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let vars: Vec<(String, OsString)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn platform_defaults_match_helm() {
        let home = Path::new("/home/me");
        let mac = HelmPaths::resolve(Platform::MacOs, Some(home), env(&[])).unwrap();
        assert_eq!(
            mac.repository_config,
            Path::new("/home/me/Library/Preferences/helm/repositories.yaml")
        );
        assert_eq!(
            mac.repository_cache,
            Path::new("/home/me/Library/Caches/helm/repository")
        );
        let linux = HelmPaths::resolve(Platform::Unix, Some(home), env(&[])).unwrap();
        assert_eq!(
            linux.repository_config,
            Path::new("/home/me/.config/helm/repositories.yaml")
        );
        assert_eq!(
            linux.repository_cache,
            Path::new("/home/me/.cache/helm/repository")
        );
        let windows = HelmPaths::resolve(
            Platform::Windows,
            None,
            env(&[("APPDATA", "/appdata"), ("TEMP", "/temp")]),
        )
        .unwrap();
        assert_eq!(
            windows.repository_config,
            Path::new("/appdata/helm/repositories.yaml")
        );
        assert_eq!(windows.repository_cache, Path::new("/temp/helm/repository"));
        assert_eq!(HelmPaths::resolve(Platform::Unix, None, env(&[])), None);
    }

    #[test]
    fn environment_overrides_in_helm_order() {
        let home = Some(Path::new("/home/me"));
        // XDG applies on every platform and gets `helm` appended.
        let xdg = HelmPaths::resolve(
            Platform::MacOs,
            home,
            env(&[
                ("XDG_CONFIG_HOME", "/xdg/config"),
                ("XDG_CACHE_HOME", "/xdg/cache"),
            ]),
        )
        .unwrap();
        assert_eq!(
            xdg.repository_config,
            Path::new("/xdg/config/helm/repositories.yaml")
        );
        assert_eq!(
            xdg.repository_cache,
            Path::new("/xdg/cache/helm/repository")
        );
        // HELM_*_HOME is used as is and wins over XDG.
        let homes = HelmPaths::resolve(
            Platform::Unix,
            home,
            env(&[
                ("XDG_CONFIG_HOME", "/xdg/config"),
                ("HELM_CONFIG_HOME", "/helm/config"),
                ("HELM_CACHE_HOME", "/helm/cache"),
            ]),
        )
        .unwrap();
        assert_eq!(
            homes.repository_config,
            Path::new("/helm/config/repositories.yaml")
        );
        assert_eq!(homes.repository_cache, Path::new("/helm/cache/repository"));
        // The file variables win over everything; empty ones are unset.
        let files = HelmPaths::resolve(
            Platform::Unix,
            home,
            env(&[
                ("HELM_CONFIG_HOME", "/helm/config"),
                ("HELM_REPOSITORY_CONFIG", "/r/repos.yaml"),
                ("HELM_REPOSITORY_CACHE", "/r/cache"),
                ("HELM_CACHE_HOME", ""),
            ]),
        )
        .unwrap();
        assert_eq!(files.repository_config, Path::new("/r/repos.yaml"));
        assert_eq!(files.repository_cache, Path::new("/r/cache"));
        assert_eq!(
            files.index_file("bitnami"),
            Path::new("/r/cache/bitnami-index.yaml")
        );
    }

    #[test]
    fn parses_repositories_without_credentials() {
        let text = r#"
apiVersion: ""
generated: "2025-04-14T10:00:00Z"
repositories:
- caFile: ""
  certFile: ""
  insecure_skip_tls_verify: false
  keyFile: ""
  name: bitnami
  pass_credentials_all: false
  password: secret
  url: https://charts.bitnami.com/bitnami
  username: me
- name: external-dns
  url: https://kubernetes-sigs.github.io/external-dns/
- url: https://nameless.example
"#;
        let repos = parse_repositories(text).unwrap();
        assert_eq!(
            repos,
            [
                Repository {
                    name: "bitnami".into(),
                    url: "https://charts.bitnami.com/bitnami".into()
                },
                Repository {
                    name: "external-dns".into(),
                    url: "https://kubernetes-sigs.github.io/external-dns/".into()
                },
            ]
        );
        assert!(!format!("{repos:?}").contains("secret"));
        assert_eq!(parse_repositories("").unwrap(), []);
        assert_eq!(parse_repositories("repositories: null\n").unwrap(), []);
        assert!(parse_repositories("repositories: [").is_err());
    }

    #[test]
    fn missing_repository_file_means_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_repositories(&dir.path().join("repositories.yaml")).unwrap(),
            []
        );
    }

    const INDEX: &str = r#"
apiVersion: v1
entries:
  nginx:
  - name: nginx
    version: 1.9.0
    appVersion: 1.25.3
    description: NGINX web server
    keywords: [web, http]
    home: https://nginx.org
    icon: https://nginx.org/icon.svg
    sources:
    - https://github.com/nginx/nginx
    maintainers:
    - name: Jane
      email: jane@example.com
    urls: [nginx-1.9.0.tgz]
    created: "2025-01-02T03:04:05Z"
  - name: nginx
    version: 1.10.0
    appVersion: 1.10
    description: NGINX web server
  - name: nginx
    version: 2.0.0-rc.1
    appVersion: 1.28
    description: NGINX web server
  redis:
  - version: v7.2
    appVersion: 7.2
    description: In-memory store
    keywords: [cache, database]
    deprecated: true
  only-beta:
  - version: 0.1.0-beta.1
    description: Early chart
  empty: []
  broken:
  - description: no version
generated: "2025-01-02T03:04:05Z"
"#;

    #[test]
    fn parses_index_and_sorts_versions() {
        let index = parse_index(INDEX).unwrap();
        assert_eq!(
            index.charts.keys().collect::<Vec<_>>(),
            ["nginx", "only-beta", "redis"]
        );
        let nginx = &index.charts["nginx"];
        let versions: Vec<&str> = nginx.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(versions, ["2.0.0-rc.1", "1.10.0", "1.9.0"]);
        assert!(nginx[0].is_prerelease());
        // Unquoted numbers stay text as written: `1.10` is not `1.1`.
        assert_eq!(nginx[1].app_version.as_deref(), Some("1.10"));
        let old = &nginx[2];
        assert_eq!(old.keywords, ["web", "http"]);
        assert_eq!(old.sources, ["https://github.com/nginx/nginx"]);
        assert_eq!(old.maintainers[0].name, "Jane");
        assert_eq!(old.created.as_deref(), Some("2025-01-02T03:04:05Z"));
        assert_eq!(old.icon.as_deref(), Some("https://nginx.org/icon.svg"));
        assert_eq!(nginx[1].icon, None);
        let redis = &index.charts["redis"][0];
        assert_eq!(redis.name, "redis", "name comes from the entry key");
        assert!(redis.deprecated);
        assert_eq!(redis.app_version.as_deref(), Some("7.2"));
    }

    #[test]
    fn parses_json_index() {
        let json = r#"{"apiVersion":"v1","entries":{"web":[{"name":"web","version":"1.0.0","appVersion":"2"}]}}"#;
        let index = parse_index(json).unwrap();
        assert_eq!(index.charts["web"][0].app_version.as_deref(), Some("2"));
        assert!(parse_index("entries: [").is_err());
    }

    #[test]
    fn versions_compare_like_helm() {
        assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("v1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0.0-rc.1", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("latest", "0.0.1"), Ordering::Less);
        assert_eq!(parse_version("3").unwrap(), semver::Version::new(3, 0, 0));
        assert_eq!(parse_version("1.2-beta").unwrap().pre.as_str(), "beta");
    }

    fn loaded() -> Vec<LoadedRepository> {
        let index = Arc::new(parse_index(INDEX).unwrap());
        let repo = |name: &str, index: IndexState| LoadedRepository {
            repository: Repository {
                name: name.into(),
                url: format!("https://{name}.example"),
            },
            index,
            stamp: None,
        };
        vec![
            repo("stable", IndexState::Ready(index.clone())),
            repo("mirror", IndexState::Ready(index)),
            repo("new", IndexState::Missing),
        ]
    }

    fn names(hits: &[ChartHit]) -> Vec<String> {
        hits.iter()
            .map(|hit| format!("{}@{}", hit.reference(), hit.version.version))
            .collect()
    }

    #[test]
    fn search_matches_names_descriptions_and_keywords() {
        let repos = loaded();
        let latest = SearchOptions::default();
        assert_eq!(
            names(&search(&repos, "NGINX", latest)),
            ["mirror/nginx@1.10.0", "stable/nginx@1.10.0"]
        );
        // Keyword match; name matches would come first.
        assert_eq!(
            names(&search(&repos, "cache", latest)),
            ["mirror/redis@v7.2", "stable/redis@v7.2"]
        );
        // Repository prefix narrows like `helm search repo stable/`.
        assert_eq!(
            names(&search(&repos, "stable/", latest)),
            [
                "stable/nginx@1.10.0",
                "stable/only-beta@0.1.0-beta.1",
                "stable/redis@v7.2"
            ]
        );
        // Description match sorts after name matches.
        assert_eq!(
            names(&search(&repos, "store", latest)),
            ["mirror/redis@v7.2", "stable/redis@v7.2"]
        );
        assert_eq!(search(&repos, "", latest).len(), 6);
        assert!(search(&repos, "nothing-like-this", latest).is_empty());
    }

    #[test]
    fn search_narrows_to_one_repository() {
        let repos = loaded();
        let mirror = SearchOptions {
            repository: Some("mirror"),
            ..Default::default()
        };
        assert_eq!(
            names(&search(&repos, "", mirror)),
            [
                "mirror/nginx@1.10.0",
                "mirror/only-beta@0.1.0-beta.1",
                "mirror/redis@v7.2"
            ]
        );
        // Together with the query.
        assert_eq!(
            names(&search(&repos, "cache", mirror)),
            ["mirror/redis@v7.2"]
        );
        let unknown = SearchOptions {
            repository: Some("nowhere"),
            ..Default::default()
        };
        assert!(search(&repos, "", unknown).is_empty());
    }

    #[test]
    fn search_lists_versions_and_prereleases_on_request() {
        let repos = loaded();
        let all = SearchOptions {
            all_versions: true,
            ..Default::default()
        };
        assert_eq!(
            names(&search(&repos, "stable/nginx", all)),
            ["stable/nginx@1.10.0", "stable/nginx@1.9.0"]
        );
        let devel = SearchOptions {
            devel: true,
            ..Default::default()
        };
        assert_eq!(
            names(&search(&repos, "stable/nginx", devel)),
            ["stable/nginx@2.0.0-rc.1"]
        );
        assert_eq!(
            find_chart(&repos, "stable", "nginx").map(<[_]>::len),
            Some(3)
        );
        assert_eq!(find_chart(&repos, "new", "nginx"), None);
    }

    #[test]
    fn load_reads_indexes_and_reuses_unchanged_ones() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HelmPaths {
            repository_config: dir.path().join("repositories.yaml"),
            repository_cache: dir.path().join("cache"),
        };
        std::fs::create_dir(&paths.repository_cache).unwrap();
        std::fs::write(
            &paths.repository_config,
            "repositories:\n- name: stable\n  url: https://s\n- name: new\n  url: https://n\n\
             - name: bad\n  url: https://b\n",
        )
        .unwrap();
        std::fs::write(paths.index_file("stable"), INDEX).unwrap();
        std::fs::write(paths.index_file("bad"), "entries: [").unwrap();
        let repos = load(&paths, &[]).unwrap();
        assert_eq!(repos.len(), 3);
        assert_eq!(repos[0].charts().unwrap().charts.len(), 3);
        assert!(matches!(repos[1].index, IndexState::Missing));
        assert!(matches!(repos[2].index, IndexState::Invalid(_)));
        let again = load(&paths, &repos).unwrap();
        let (IndexState::Ready(old), IndexState::Ready(new)) = (&repos[0].index, &again[0].index)
        else {
            panic!("expected indexes");
        };
        assert!(Arc::ptr_eq(old, new), "unchanged index is reused");
        assert!(
            load(
                &HelmPaths {
                    repository_config: dir.path().join("none.yaml"),
                    repository_cache: dir.path().into(),
                },
                &[]
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn validates_names_and_urls() {
        assert!(check_repository_name("bitnami").is_ok());
        assert!(check_repository_name(" ").is_err());
        assert!(check_repository_name("a/b").is_err());
        assert!(check_repository_url("https://charts.example/x").is_ok());
        assert!(check_repository_url("oci://registry/x").is_err());
        assert!(check_repository_url("ftp://x").is_err());
        assert!(check_repository_url("https://").is_err());
        assert!(check_release_name("web-1.prod").is_ok());
        assert!(check_release_name("Web").is_err());
        assert!(check_release_name("-web").is_err());
        assert!(check_release_name(&"a".repeat(54)).is_err());
        assert!(check_namespace_name("kubach-test").is_ok());
        assert!(check_namespace_name("a.b").is_err());
        assert!(check_namespace_name("").is_err());
    }

    #[test]
    fn validation_variants_and_texts() {
        use crate::names::NameError;
        let text = |result: Result<(), ValidationError>| result.unwrap_err().to_string();
        assert_eq!(
            check_repository_name(" "),
            Err(ValidationError::Empty(Field::RepositoryName))
        );
        assert_eq!(
            text(check_repository_name("a b")),
            "The repository name can't contain '/' or spaces."
        );
        assert_eq!(
            check_repository_url("oci://registry/x"),
            Err(ValidationError::OciRepository)
        );
        assert_eq!(
            check_repository_url("ftp://x"),
            Err(ValidationError::UrlScheme)
        );
        assert_eq!(
            check_repository_url("https://a b"),
            Err(ValidationError::InvalidUrl)
        );
        assert_eq!(
            check_repository_credentials("http://x"),
            Err(ValidationError::InsecureCredentials)
        );
        assert_eq!(text(check_release_name("")), "Enter a release name.");
        assert_eq!(
            text(check_release_name(&"a".repeat(54))),
            "The release name is longer than 53 characters."
        );
        assert_eq!(
            check_release_name("Web"),
            Err(ValidationError::Name {
                field: Field::ReleaseName,
                error: NameError::InvalidChar('W'),
            })
        );
        assert_eq!(text(check_namespace_name("")), "Enter a namespace.");
        assert_eq!(
            check_namespace_name("a.b").unwrap_err().field(),
            Some(Field::Namespace)
        );
        assert!(matches!(
            parse_repositories("repositories: ["),
            Err(ValidationError::Yaml { field: None, .. })
        ));
    }

    #[test]
    fn credentials_go_only_over_https() {
        assert!(check_repository_credentials("https://charts.example").is_ok());
        assert!(check_repository_credentials(" HTTPS://charts.example ").is_ok());
        assert!(check_repository_credentials("http://charts.example").is_err());
        assert!(check_repository_credentials("HTTP://charts.example").is_err());
        assert!(check_repository_credentials("file:///charts").is_err());
        assert!(check_repository_credentials("").is_err());
    }
}
