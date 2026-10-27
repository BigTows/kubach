//! Newer versions of a release's chart in the user's repositories (#273),
//! found natively in the cached `index.yaml` files ([`super::repo`]): no
//! network and no `helm`.
//!
//! A release keeps its chart's `Chart.yaml` but not the repository it came
//! from, so the repository is found by the chart name. When several
//! repositories have a chart of that name, the chart's `home` and `sources`
//! (and a repository URL among them) pick one; if that does not settle it,
//! the chart is ambiguous and no update is offered.

use super::ChartInfo;
use super::repo::{ChartVersion, LoadedRepository, parse_version};

/// A repository that has the release's chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepoChart<'a> {
    pub repository: &'a str,
    /// Versions of the chart, newest first.
    pub versions: &'a [ChartVersion],
}

/// Where a release's chart can be found among the repositories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChartSource<'a> {
    Found(RepoChart<'a>),
    /// Several repositories have the chart and its metadata does not tell
    /// which one it came from.
    Ambiguous(Vec<RepoChart<'a>>),
    /// No repository has it (an OCI, local or removed chart).
    NotFound,
}

impl<'a> ChartSource<'a> {
    /// Repositories the chart may come from: one when found, every
    /// candidate when ambiguous.
    pub fn candidates(&self) -> Vec<RepoChart<'a>> {
        match self {
            Self::Found(found) => vec![*found],
            Self::Ambiguous(candidates) => candidates.clone(),
            Self::NotFound => Vec::new(),
        }
    }
}

/// A newer version of a release's chart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartUpdate {
    pub repository: String,
    pub chart: String,
    pub version: String,
    pub app_version: Option<String>,
}

/// A URL compared loosely: no scheme, case, `www.`, trailing `/` or `.git`.
fn normalize(url: &str) -> String {
    let url = url.trim().to_lowercase();
    let url = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(&url);
    let url = url.strip_prefix("www.").unwrap_or(url);
    let url = url.trim_end_matches('/');
    url.strip_suffix(".git").unwrap_or(url).to_owned()
}

/// Finds the repository of `chart` (see the module docs).
pub fn locate_chart<'a>(
    repositories: &'a [LoadedRepository],
    chart: &ChartInfo,
) -> ChartSource<'a> {
    if chart.name.is_empty() {
        return ChartSource::NotFound;
    }
    let candidates: Vec<(RepoChart<'a>, &'a str)> = repositories
        .iter()
        .filter_map(|loaded| {
            let versions = loaded.charts()?.charts.get(&chart.name)?;
            Some((
                RepoChart {
                    repository: &loaded.repository.name,
                    versions,
                },
                loaded.repository.url.as_str(),
            ))
        })
        .collect();
    match candidates.as_slice() {
        [] => return ChartSource::NotFound,
        [(only, _)] => return ChartSource::Found(*only),
        _ => {}
    }
    let links: Vec<String> = chart
        .home
        .iter()
        .chain(&chart.sources)
        .map(|link| normalize(link))
        .filter(|link| !link.is_empty())
        .collect();
    let narrowed: Vec<RepoChart<'a>> = if links.is_empty() {
        Vec::new()
    } else {
        candidates
            .iter()
            .filter(|(candidate, url)| {
                let url = normalize(url);
                let repo_linked =
                    !url.is_empty() && links.iter().any(|link| link.starts_with(&url));
                repo_linked
                    || candidate.versions.iter().any(|version| {
                        version
                            .home
                            .iter()
                            .chain(&version.sources)
                            .any(|link| links.contains(&normalize(link)))
                    })
            })
            .map(|(candidate, _)| *candidate)
            .collect()
    };
    match narrowed.as_slice() {
        [only] => ChartSource::Found(*only),
        [] => ChartSource::Ambiguous(candidates.into_iter().map(|(c, _)| c).collect()),
        _ => ChartSource::Ambiguous(narrowed),
    }
}

/// The newest of `versions` that is newer than `installed` by SemVer.
/// Pre-releases count only when a pre-release is installed: a stable
/// release is never offered `2.0.0-rc.1`. `None` when `installed` is not
/// SemVer.
pub fn newer_version<'a>(
    installed: &str,
    versions: &'a [ChartVersion],
) -> Option<&'a ChartVersion> {
    let installed = parse_version(installed)?;
    let allow_pre = !installed.pre.is_empty();
    versions
        .iter()
        .filter_map(|version| Some((parse_version(&version.version)?, version)))
        .filter(|(parsed, _)| allow_pre || parsed.pre.is_empty())
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .filter(|(newest, _)| *newest > installed)
        .map(|(_, version)| version)
}

/// A newer version of `chart` in the one repository it comes from; `None`
/// when it is up to date, not found or ambiguous.
pub fn available_update(
    repositories: &[LoadedRepository],
    chart: &ChartInfo,
) -> Option<ChartUpdate> {
    let ChartSource::Found(found) = locate_chart(repositories, chart) else {
        return None;
    };
    let newer = newer_version(&chart.version, found.versions)?;
    Some(ChartUpdate {
        repository: found.repository.to_owned(),
        chart: chart.name.clone(),
        version: newer.version.clone(),
        app_version: newer.app_version.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helm::repo::{Repository, parse_index};

    fn repo(name: &str, url: &str, index: &str) -> LoadedRepository {
        LoadedRepository::ready(
            Repository {
                name: name.into(),
                url: url.into(),
            },
            parse_index(index).unwrap(),
        )
    }

    const WEB: &str = r#"
entries:
  web:
  - version: 1.2.0
    appVersion: "2.1"
    home: https://web.example
    sources: [https://github.com/acme/web]
  - version: 1.1.0
  - version: 2.0.0-rc.1
  - version: 1.0.0
  other:
  - version: 0.1.0
"#;

    const FORK: &str = r#"
entries:
  web:
  - version: 3.0.0
    home: https://fork.example
"#;

    fn chart(version: &str) -> ChartInfo {
        ChartInfo {
            name: "web".into(),
            version: version.into(),
            ..Default::default()
        }
    }

    #[test]
    fn newer_versions_follow_semver_and_skip_prereleases() {
        let index = parse_index(WEB).unwrap();
        let versions = &index.charts["web"];
        let newer = |installed| newer_version(installed, versions).map(|v| v.version.as_str());
        assert_eq!(newer("1.0.0"), Some("1.2.0"));
        assert_eq!(newer("1.1.0"), Some("1.2.0"));
        // A stable install is not offered the release candidate.
        assert_eq!(newer("1.2.0"), None);
        assert_eq!(newer("v1.2"), None);
        // A pre-release install is.
        assert_eq!(newer("1.2.0-beta.1"), Some("2.0.0-rc.1"));
        assert_eq!(newer("2.0.0-rc.1"), None);
        // Newer than anything in the repository, or not SemVer.
        assert_eq!(newer("9.0.0"), None);
        assert_eq!(newer("latest"), None);
    }

    #[test]
    fn single_repository_is_found_by_name() {
        let repos = [
            repo("acme", "https://charts.acme.io", WEB),
            repo("empty", "https://e", "entries: {}"),
        ];
        let ChartSource::Found(found) = locate_chart(&repos, &chart("1.0.0")) else {
            panic!("expected one repository");
        };
        assert_eq!(found.repository, "acme");
        assert_eq!(found.versions.len(), 4);
        assert_eq!(
            available_update(&repos, &chart("1.0.0")),
            Some(ChartUpdate {
                repository: "acme".into(),
                chart: "web".into(),
                version: "1.2.0".into(),
                app_version: Some("2.1".into()),
            })
        );
        assert_eq!(available_update(&repos, &chart("1.2.0")), None);
        let missing = ChartInfo {
            name: "nginx".into(),
            ..chart("1.0.0")
        };
        assert_eq!(locate_chart(&repos, &missing), ChartSource::NotFound);
        assert_eq!(
            locate_chart(&repos, &ChartInfo::default()),
            ChartSource::NotFound
        );
    }

    #[test]
    fn several_repositories_need_the_chart_metadata() {
        let repos = [
            repo("acme", "https://charts.acme.io", WEB),
            repo("fork", "https://fork.example/charts", FORK),
        ];
        // No home or sources: ambiguous, no update shown.
        let bare = chart("1.0.0");
        assert!(
            matches!(locate_chart(&repos, &bare), ChartSource::Ambiguous(ref c) if c.len() == 2)
        );
        assert_eq!(available_update(&repos, &bare), None);
        // Sources match one repository's index (loosely).
        let sourced = ChartInfo {
            sources: vec!["http://GitHub.com/acme/web.git/".into()],
            ..chart("1.0.0")
        };
        assert_eq!(
            available_update(&repos, &sourced).map(|u| (u.repository, u.version)),
            Some(("acme".into(), "1.2.0".into()))
        );
        // Home matches the other.
        let forked = ChartInfo {
            home: Some("https://fork.example".into()),
            ..chart("1.0.0")
        };
        assert_eq!(
            available_update(&repos, &forked).map(|u| u.repository),
            Some("fork".into())
        );
        // A link under a repository URL picks it.
        let linked = ChartInfo {
            home: Some("https://charts.acme.io/web".into()),
            ..chart("1.0.0")
        };
        assert_eq!(
            available_update(&repos, &linked).map(|u| u.repository),
            Some("acme".into())
        );
        // A mirror with the same metadata stays ambiguous.
        let mirrored = [
            repo("acme", "https://charts.acme.io", WEB),
            repo("mirror", "https://mirror.example", WEB),
        ];
        let source = locate_chart(&mirrored, &sourced);
        assert!(matches!(source, ChartSource::Ambiguous(ref c) if c.len() == 2));
        assert_eq!(source.candidates().len(), 2);
        assert_eq!(available_update(&mirrored, &sourced), None);
    }
}
