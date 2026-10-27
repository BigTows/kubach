//! Explanations of `helm` failures the user can act on, read from the text
//! `helm` prints (it has no machine-readable errors).
//!
//! The common one (#261): the chart renders custom resources whose CRDs the
//! cluster does not have. `helm install --dry-run=server` (the preview)
//! sends every object to the API server, which answers `no matches for kind
//! "Redis" in version "redis.redis.opstreelabs.in/v1beta2"`, and helm adds
//! `ensure CRDs are installed first`.

/// A kind the API server does not serve: no CRD for it is installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingKind {
    pub kind: String,
    /// API group; empty for the core group.
    pub group: String,
    pub version: String,
}

impl MissingKind {
    /// `Redis (redis.redis.opstreelabs.in)`; a core kind shows its version.
    pub fn label(&self) -> String {
        if self.group.is_empty() {
            format!("{} ({})", self.kind, self.version)
        } else {
            format!("{} ({})", self.kind, self.group)
        }
    }
}

const NO_MATCHES: &str = "no matches for kind \"";
const IN_VERSION: &str = "\" in version \"";

/// Kinds `helm` failed on because the cluster does not know them, in the
/// order of the error text, each once.
pub fn missing_kinds(error: &str) -> Vec<MissingKind> {
    let mut kinds: Vec<MissingKind> = Vec::new();
    let mut rest = error;
    while let Some(start) = rest.find(NO_MATCHES) {
        rest = &rest[start + NO_MATCHES.len()..];
        let Some(parsed) = parse_kind(rest) else {
            continue;
        };
        if !kinds.contains(&parsed) {
            kinds.push(parsed);
        }
    }
    kinds
}

/// `Redis" in version "group/v1"…` after the opening quote.
fn parse_kind(text: &str) -> Option<MissingKind> {
    let (kind, rest) = text.split_once(IN_VERSION)?;
    let (api_version, _) = rest.split_once('"')?;
    if kind.is_empty() || kind.contains('"') || api_version.is_empty() {
        return None;
    }
    let (group, version) = api_version.rsplit_once('/').unwrap_or(("", api_version));
    Some(MissingKind {
        kind: kind.to_owned(),
        group: group.to_owned(),
        version: version.to_owned(),
    })
}

/// What a failure with missing CRDs means and what to do; `None` when the
/// error is not about missing kinds.
pub fn missing_crds_explanation(error: &str) -> Option<String> {
    let kinds = missing_kinds(error);
    if kinds.is_empty() {
        return None;
    }
    let labels: Vec<String> = kinds.iter().map(MissingKind::label).collect();
    let (list, them) = match labels.as_slice() {
        [one] => (one.clone(), "it"),
        _ => (labels.join(", "), "them"),
    };
    Some(format!(
        "The chart creates {list} resources, but the cluster has no CRD for {them}. \
         Usually the operator that defines {them} is installed first, as a separate chart. \
         If this chart ships the CRDs itself (its crds/ folder), only the preview fails: \
         a dry run does not create CRDs, so you can install without a preview."
    ))
}

#[cfg(test)]
mod tests {
    use super::{MissingKind, missing_crds_explanation, missing_kinds};

    /// What `helm install --dry-run=server` printed for `ot-helm/redis`.
    const REDIS: &str = "Error: INSTALLATION FAILED: unable to build kubernetes objects from \
        release manifest: resource mapping not found for name: \"redis\" namespace: \"default\" \
        from \"\": no matches for kind \"Redis\" in version \"redis.redis.opstreelabs.in/v1beta2\"\n\
        ensure CRDs are installed first";

    fn kind(kind: &str, group: &str, version: &str) -> MissingKind {
        MissingKind {
            kind: kind.into(),
            group: group.into(),
            version: version.into(),
        }
    }

    #[test]
    fn reads_the_missing_kind() {
        assert_eq!(
            missing_kinds(REDIS),
            [kind("Redis", "redis.redis.opstreelabs.in", "v1beta2")]
        );
        assert_eq!(
            missing_kinds(REDIS)[0].label(),
            "Redis (redis.redis.opstreelabs.in)"
        );
    }

    #[test]
    fn reads_several_kinds_once_each() {
        let error = "Error: INSTALLATION FAILED: unable to build kubernetes objects from release \
            manifest: [resource mapping not found for name: \"a\" namespace: \"\" from \"\": no \
            matches for kind \"Redis\" in version \"redis.redis.opstreelabs.in/v1beta2\"\n\
            ensure CRDs are installed first, resource mapping not found for name: \"b\" \
            namespace: \"\" from \"\": no matches for kind \"ServiceMonitor\" in version \
            \"monitoring.coreos.com/v1\"\nensure CRDs are installed first, resource mapping not \
            found for name: \"c\" namespace: \"\" from \"\": no matches for kind \"Redis\" in \
            version \"redis.redis.opstreelabs.in/v1beta2\"\nensure CRDs are installed first]";
        assert_eq!(
            missing_kinds(error),
            [
                kind("Redis", "redis.redis.opstreelabs.in", "v1beta2"),
                kind("ServiceMonitor", "monitoring.coreos.com", "v1"),
            ]
        );
        let text = missing_crds_explanation(error).unwrap();
        assert!(
            text.starts_with(
                "The chart creates Redis (redis.redis.opstreelabs.in), ServiceMonitor \
                 (monitoring.coreos.com) resources, but the cluster has no CRD for them."
            ),
            "{text}"
        );
    }

    #[test]
    fn core_group_kinds_show_their_version() {
        let error = "no matches for kind \"Widget\" in version \"v1\"";
        assert_eq!(missing_kinds(error), [kind("Widget", "", "v1")]);
        assert_eq!(missing_kinds(error)[0].label(), "Widget (v1)");
    }

    #[test]
    fn explains_missing_crds() {
        assert_eq!(
            missing_crds_explanation(REDIS).unwrap(),
            "The chart creates Redis (redis.redis.opstreelabs.in) resources, but the cluster \
             has no CRD for it. Usually the operator that defines it is installed first, as a \
             separate chart. If this chart ships the CRDs itself (its crds/ folder), only the \
             preview fails: a dry run does not create CRDs, so you can install without a \
             preview."
        );
    }

    #[test]
    fn other_errors_have_no_explanation() {
        assert_eq!(missing_crds_explanation("Error: chart not found"), None);
        assert!(missing_kinds("no matches for kind \"Broken").is_empty());
        assert!(missing_kinds("no matches for kind \"\" in version \"v1\"").is_empty());
        assert!(missing_kinds("").is_empty());
    }
}
